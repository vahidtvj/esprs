use anyhow::Result;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::delay::{FreeRtos, TickType};
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::uart::*;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use mqtt::{MqttClient, MqttConfig};
use power::{PowerControl, SimcomPower};
use sms::{AtPortRaw, SmsService};
use std::time::Duration;
use wifi::{Wifi, WifiConfig};

mod pinout;
use pinout::*;

struct UartAtPort<'d> {
    driver: UartDriver<'d>,
}

impl<'d> UartAtPort<'d> {
    fn new(driver: UartDriver<'d>) -> Self {
        Self { driver }
    }
}

impl<'d> AtPortRaw for UartAtPort<'d> {
    fn write_raw(&mut self, data: &[u8]) -> Result<()> {
        log::debug!(target: "uart", ">> {}", String::from_utf8_lossy(data).trim());
        let mut written = 0;
        while written < data.len() {
            let count = self.driver.write(&data[written..])?;
            if count == 0 {
                anyhow::bail!("UART write returned zero bytes");
            }
            written += count;
        }
        Ok(())
    }

    fn read_raw(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize> {
        // Convert Duration to FreeRTOS ticks
        let ticks = TickType::new_millis(timeout.as_millis() as u64).ticks();
        let len = match self.driver.read(buf, ticks) {
            Ok(len) => len,
            Err(error) if error.code() == esp_idf_svc::sys::ESP_ERR_TIMEOUT => return Ok(0),
            Err(error) => return Err(error.into()),
        };
        if len > 0 {
            log::debug!(target: "uart", "<< (raw) {}", String::from_utf8_lossy(&buf[..len]).trim());
        }
        Ok(len)
    }
}

fn main() {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    log::set_max_level(log::LevelFilter::Debug);

    // Also set esp-idf log level
    unsafe {
        esp_idf_svc::sys::esp_log_level_set(
            c"esprs".as_ptr(),
            esp_idf_svc::sys::esp_log_level_t_ESP_LOG_DEBUG,
        );
        esp_idf_svc::sys::esp_log_level_set(
            c"power".as_ptr(),
            esp_idf_svc::sys::esp_log_level_t_ESP_LOG_DEBUG,
        );
        esp_idf_svc::sys::esp_log_level_set(
            c"sms".as_ptr(),
            esp_idf_svc::sys::esp_log_level_t_ESP_LOG_DEBUG,
        );
        esp_idf_svc::sys::esp_log_level_set(
            c"uart".as_ptr(),
            esp_idf_svc::sys::esp_log_level_t_ESP_LOG_DEBUG,
        );
    }

    if let Err(error) = run() {
        log::error!("Fatal error: {:#}; restarting in 1 second", error);
        FreeRtos::delay_ms(1000);
        unsafe { esp_idf_svc::sys::esp_restart() };
    }
}

fn run() -> Result<()> {
    log::info!("Starting ESP32 + SIMCOM A7670 modem workspace demo");

    let wifi_ssid = option_env!("WIFI_SSID").unwrap_or("");
    let mqtt_broker_url = option_env!("MQTT_BROKER_URL").unwrap_or("");
    anyhow::ensure!(!wifi_ssid.is_empty(), "WIFI_SSID must be set at build time");
    anyhow::ensure!(
        !mqtt_broker_url.is_empty(),
        "MQTT_BROKER_URL must be set at build time"
    );

    let peripherals = Peripherals::take()?;
    let sysloop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;
    let mut wifi = Wifi::new(
        peripherals.modem,
        sysloop,
        Some(nvs),
        WifiConfig {
            ssid: wifi_ssid,
            password: option_env!("WIFI_PASSWORD").unwrap_or(""),
        },
    )?;
    let mut mqtt = MqttClient::new(MqttConfig {
        broker_url: mqtt_broker_url,
        client_id: "esprs",
        username: option_env!("MQTT_USERNAME"),
        password: option_env!("MQTT_PASSWORD"),
    })?;

    let mut pwr = SimcomPower::lilygo(
        peripherals.pins.gpio12.into(),
        peripherals.pins.gpio5.into(),
        peripherals.pins.gpio4.into(),
    )?;

    let config = config::Config::new().baudrate(esp_idf_svc::hal::units::Hertz(MODEM_BAUDRATE));
    let uart = UartDriver::new(
        peripherals.uart1,
        peripherals.pins.gpio26,
        peripherals.pins.gpio27,
        Option::<esp_idf_svc::hal::gpio::AnyIOPin>::None,
        Option::<esp_idf_svc::hal::gpio::AnyIOPin>::None,
        &config,
    )?;

    let at_port = UartAtPort::new(uart);

    log::info!("Power-cycling modem for a clean boot");
    pwr.power_off()?;
    FreeRtos::delay_ms(1000);
    pwr.power_on()?;

    let mut sms_service = SmsService::new(at_port);

    sms_service.on_sms(move |sms| {
        log::debug!(target: "esprs", "on_sms callback triggered for: {}", sms.sender);
        let payload = format!("FROM: {}\n{}\n\n{}", sms.sender, sms.date, sms.message);
        if let Err(error) = mqtt.publish("/sms/gsmv", payload.as_bytes()) {
            log::error!(target: "esprs", "Failed to queue SMS for MQTT delivery: {:#}", error);
        } else {
            log::info!(target: "esprs", "Queued SMS from {} for MQTT delivery", sms.sender);
        }
    });

    sms_service.init()?;

    log::info!("SMS service initialized, listening for incoming SMS...");

    loop {
        if let Err(error) = wifi.ensure_connected() {
            log::warn!(target: "esprs", "WiFi connection failed; retrying: {:#}", error);
            FreeRtos::delay_ms(1000);
        }
        if let Err(error) = sms_service.poll(Duration::from_millis(100)) {
            log::warn!(target: "esprs", "Transient SMS poll error: {:#}", error);
        }
    }
}
