use anyhow::Result;
use esp_idf_svc::hal::delay::TickType;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::uart::*;
use power::{PowerControl, SimcomPower};
use sms::{send_command, AtError, AtPortRaw, AtState, SmsService, SystemClock};
use std::time::Duration;

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
        self.driver.write(data)?;
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

fn main() -> Result<()> {
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

    log::info!("Starting ESP32 + SIMCOM A7670 modem workspace demo");

    let peripherals = Peripherals::take()?;

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

    let mut at_port = UartAtPort::new(uart);
    let mut at_state = AtState::new();
    let clock = SystemClock;
    match send_command(
        &mut at_port,
        &clock,
        "AT",
        Duration::from_secs(2),
        &mut at_state,
    ) {
        Ok(_) => log::info!("Modem is already powered and responded to AT"),
        Err(error) if matches!(error.downcast_ref::<AtError>(), Some(AtError::Timeout)) => {
            log::info!("Modem did not respond to AT; powering it on");
            pwr.power_on()?;
        }
        Err(error) => return Err(error.context("failed to query modem with AT")),
    }

    let mut sms_service = SmsService::new(at_port);

    sms_service.on_sms(|sms| {
        log::debug!(target: "esprs", "on_sms callback triggered for: {}", sms.sender);
        let encoded = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            sms.message.as_bytes(),
        );
        log::info!("Received SMS from {}: [base64] {}", sms.sender, encoded);
    });

    sms_service.init()?;

    log::info!("SMS service initialized, listening for incoming SMS...");

    loop {
        if let Err(error) = sms_service.poll(Duration::from_millis(100)) {
            log::warn!(target: "esprs", "Transient SMS poll error: {:#}", error);
        }
    }
}
