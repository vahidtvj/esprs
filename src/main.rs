use anyhow::Result;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::uart::*;
use power::{PowerControl, SimcomPower};
use sms::{AtPort, Sms, SmsService};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod pinout;
use pinout::*;

struct UartAtPort<'d> {
    driver: UartDriver<'d>,
    urc_cb: Option<Box<dyn FnMut(&str) + Send + 'static>>,
}

impl<'d> UartAtPort<'d> {
    fn new(driver: UartDriver<'d>) -> Self {
        Self {
            driver,
            urc_cb: None,
        }
    }
}

impl<'d> AtPort for UartAtPort<'d> {
    fn send_command(&mut self, cmd: &str, timeout: Duration) -> Result<Vec<String>> {
        let full_cmd = format!("{}\r\n", cmd);
        log::debug!(target: "uart", ">> {}", cmd);
        self.driver.write(full_cmd.as_bytes())?;

        let mut lines = Vec::new();
        let mut buf = [0u8; 256];
        let mut line_buf = String::new();
        let timeout_ticks = (timeout.as_millis() / 10) as usize;
        let mut got_response = false;

        for _ in 0..timeout_ticks {
            if let Ok(len) = self.driver.read(&mut buf, 10) {
                if len > 0 {
                    let s = String::from_utf8_lossy(&buf[..len]);
                    for c in s.chars() {
                        if c == '\n' {
                            let line = line_buf.trim().to_string();
                            line_buf.clear();
                            if !line.is_empty() {
                                log::debug!(target: "uart", "<< {}", line);
                                if line.starts_with("+CMTI:") {
                                    if let Some(ref mut cb) = self.urc_cb {
                                        cb(&line);
                                    }
                                } else if line == "OK"
                                    || line == "ERROR"
                                    || line.starts_with("+CME ERROR")
                                {
                                    lines.push(line.clone());
                                    got_response = true;
                                    break;
                                } else {
                                    lines.push(line.clone());
                                }
                            }
                        } else if c != '\r' {
                            line_buf.push(c);
                        }
                    }
                }
            }
            if got_response {
                break;
            }
        }
        Ok(lines)
    }

    fn on_urc(&mut self, callback: Box<dyn FnMut(&str) + Send + 'static>) {
        self.urc_cb = Some(callback);
    }

    fn poll(&mut self, timeout: Duration) -> Result<Option<u8>> {
        let mut buf = [0u8; 256];
        let mut line_buf = String::new();
        let timeout_ticks = (timeout.as_millis() / 10) as usize;

        for _ in 0..timeout_ticks {
            if let Ok(len) = self.driver.read(&mut buf, 10) {
                if len > 0 {
                    let s = String::from_utf8_lossy(&buf[..len]);
                    for c in s.chars() {
                        if c == '\n' {
                            let line = line_buf.trim().to_string();
                            line_buf.clear();
                            if !line.is_empty() {
                                log::debug!(target: "uart", "<< (poll) {}", line);
                                if line.starts_with("+CMTI:") {
                                    if let Some(ref mut cb) = self.urc_cb {
                                        cb(&line);
                                    }
                                    if let Some(comma_pos) = line.find(',') {
                                        if let Ok(index) =
                                            line[comma_pos + 1..].trim().parse::<u8>()
                                        {
                                            return Ok(Some(index));
                                        }
                                    }
                                }
                            }
                        } else if c != '\r' {
                            line_buf.push(c);
                        }
                    }
                }
            }
        }
        Ok(None)
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
    pwr.power_on()?;

    let config = config::Config::new().baudrate(esp_idf_svc::hal::units::Hertz(MODEM_BAUDRATE));
    let uart = UartDriver::new(
        peripherals.uart1,
        peripherals.pins.gpio26,
        peripherals.pins.gpio27,
        Option::<esp_idf_svc::hal::gpio::AnyIOPin>::None,
        Option::<esp_idf_svc::hal::gpio::AnyIOPin>::None,
        &config,
    )?;

    let (tx, rx): (Sender<Sms>, Receiver<Sms>) = channel();
    let tx = Arc::new(Mutex::new(tx));

    let at_port = UartAtPort::new(uart);
    let mut sms_service = SmsService::new(at_port);
    sms_service.init()?;

    let tx_clone = tx.clone();
    sms_service.on_sms(move |sms| {
        log::debug!(target: "esprs", "on_sms callback triggered for: {}", sms.sender);
        if let Ok(tx) = tx_clone.lock() {
            let _ = tx.send(sms);
        }
    });

    log::info!("SMS service initialized, listening for incoming SMS...");

    loop {
        sms_service.poll(Duration::from_millis(100))?;
        if let Ok(sms) = rx.try_recv() {
            let encoded = base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                sms.message.as_bytes(),
            );
            log::info!("Received SMS from {}: [base64] {}", sms.sender, encoded);
        }
    }
}
