use anyhow::Result;
use std::time::Duration;

use crate::pdu::parse_pdu_response;
use crate::Sms;

pub trait AtPort {
    fn send_command(&mut self, cmd: &str, timeout: Duration) -> Result<Vec<String>>;
    fn on_urc(&mut self, callback: Box<dyn FnMut(&str) + Send + 'static>);
    fn poll(&mut self, timeout: Duration) -> Result<Option<u8>>;
}

pub struct SmsService<P: AtPort> {
    port: P,
    callback: Option<Box<dyn FnMut(Sms) + Send + 'static>>,
}

impl<P: AtPort> SmsService<P> {
    pub fn new(port: P) -> Self {
        Self {
            port,
            callback: None,
        }
    }

    pub fn init(&mut self) -> Result<()> {
        self.port
            .send_command("AT+CMGF=0", Duration::from_secs(3))?;
        self.port
            .send_command("AT+CSCS=\"UCS2\"", Duration::from_secs(3))?;
        self.port
            .send_command("AT+CNMI=2,1,0,0,0", Duration::from_secs(3))?;
        Ok(())
    }

    pub fn read(&mut self, index: u8) -> Result<Sms> {
        let lines = self
            .port
            .send_command(&format!("AT+CMGR={index}"), Duration::from_secs(5))?;
        parse_pdu_response(&lines)
    }

    pub fn delete(&mut self, index: u8) -> Result<()> {
        self.port
            .send_command(&format!("AT+CMGD={index}"), Duration::from_secs(3))?;
        Ok(())
    }

    pub fn on_sms<F>(&mut self, cb: F)
    where
        F: FnMut(Sms) + Send + 'static,
    {
        self.callback = Some(Box::new(cb));
    }

    pub fn poll(&mut self, timeout: Duration) -> Result<()> {
        if let Some(index) = self.port.poll(timeout)? {
            log::info!("Incoming SMS notification at index {}, reading...", index);
            match self.read(index) {
                Ok(sms) => {
                    log::info!("Successfully read SMS from {}, deleting...", sms.sender);
                    if let Some(ref mut cb) = self.callback {
                        cb(sms);
                    }
                    let _ = self.delete(index);
                }
                Err(e) => {
                    log::error!("Failed to read SMS at index {}: {}", index, e);
                }
            }
        }
        Ok(())
    }
}
