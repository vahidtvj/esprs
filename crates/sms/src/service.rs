use anyhow::{Context, Result};
use std::time::{Duration, Instant};

use crate::at_port::{poll_urcs, send_command, AtPortRaw, AtState, Clock, SystemClock};
use crate::reassembler::{CompletedMessage, Reassembler, ReassemblerConfig};
use crate::{ParseResult, Sms};

/// Storage location for SMS messages
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmsStorage {
    /// SIM card storage
    SM,
    /// Mobile equipment (internal) storage
    ME,
}

impl SmsStorage {
    fn as_str(&self) -> &'static str {
        match self {
            SmsStorage::SM => "SM",
            SmsStorage::ME => "ME",
        }
    }
}

impl Default for SmsStorage {
    fn default() -> Self {
        SmsStorage::SM
    }
}

pub struct SmsService<P: AtPortRaw, C: Clock> {
    port: P,
    callback: Option<Box<dyn FnMut(Sms) + Send + 'static>>,
    reassembler: Reassembler,
    clock: C,
    /// AT state for line framing and URC handling
    at_state: AtState,
    /// SMS storage location (ME or SM)
    storage: SmsStorage,
    /// Interval between stored-message sweeps.
    sweep_interval: Duration,
    /// Time of the last stored-message sweep.
    last_sweep: Option<Instant>,
}

impl<P: AtPortRaw> SmsService<P, SystemClock> {
    pub fn new(port: P) -> Self {
        Self::with_clock(port, SystemClock)
    }

    pub fn with_storage(port: P, storage: SmsStorage) -> Self {
        Self::with_clock_and_storage(port, SystemClock, storage)
    }
}

impl<P: AtPortRaw, C: Clock> SmsService<P, C> {
    pub fn with_clock(port: P, clock: C) -> Self {
        Self::with_clock_and_storage(port, clock, SmsStorage::default())
    }

    pub fn with_clock_and_storage(port: P, clock: C, storage: SmsStorage) -> Self {
        Self {
            port,
            callback: None,
            reassembler: Reassembler::new(),
            clock,
            at_state: AtState::new(),
            storage,
            sweep_interval: Duration::from_secs(60),
            last_sweep: None,
        }
    }

    pub fn with_config(port: P, clock: C, config: ReassemblerConfig) -> Self {
        Self {
            port,
            callback: None,
            reassembler: Reassembler::with_config(config),
            clock,
            at_state: AtState::new(),
            storage: SmsStorage::default(),
            sweep_interval: Duration::from_secs(60),
            last_sweep: None,
        }
    }

    /// Construct a service with a custom stored-message sweep interval.
    pub fn with_sweep_interval(port: P, clock: C, sweep_interval: Duration) -> Self {
        Self::with_clock_and_sweep_interval(port, clock, sweep_interval)
    }

    /// Construct a service with a custom clock and stored-message sweep interval.
    pub fn with_clock_and_sweep_interval(port: P, clock: C, sweep_interval: Duration) -> Self {
        let mut service = Self::with_clock(port, clock);
        service.sweep_interval = sweep_interval;
        service
    }

    /// Set the stored-message sweep interval.
    pub fn set_sweep_interval(&mut self, sweep_interval: Duration) {
        self.sweep_interval = sweep_interval;
    }

    /// Return the stored-message sweep interval.
    pub fn sweep_interval(&self) -> Duration {
        self.sweep_interval
    }

    /// Initialize the modem for SMS operation
    pub fn init(&mut self) -> Result<()> {
        // 1. AT sync with retries and delay (modem may not be ready after power-on)
        self.at_sync()?;

        // 2. ATE0 - disable command echo
        send_command(
            &mut self.port,
            &self.clock,
            "ATE0",
            Duration::from_secs(3),
            &mut self.at_state,
        )
        .context("failed to disable echo (ATE0)")?;

        // 3. AT+CPIN? must report READY, else Err
        let lines = send_command(
            &mut self.port,
            &self.clock,
            "AT+CPIN?",
            Duration::from_secs(5),
            &mut self.at_state,
        )
        .context("failed to query SIM PIN status (AT+CPIN?)")?;

        if !lines.iter().any(|line| line.contains("+CPIN: READY")) {
            anyhow::bail!("SIM not ready: {}", lines.join(", "));
        }

        // 4. Network registration via AT+CREG?/AT+CEREG?, bounded wait; log and continue on timeout
        self.wait_for_network_registration()?;

        // 5. AT+CMGF=0 - PDU mode
        send_command(
            &mut self.port,
            &self.clock,
            "AT+CMGF=0",
            Duration::from_secs(3),
            &mut self.at_state,
        )
        .context("failed to set PDU mode (AT+CMGF=0)")?;

        // 6. AT+CPMS set explicitly to one storage for read, delete and receive
        let storage_str = self.storage.as_str();
        let cpms_cmd = format!(
            "AT+CPMS=\"{}\",\"{}\",\"{}\"",
            storage_str, storage_str, storage_str
        );
        send_command(
            &mut self.port,
            &self.clock,
            &cpms_cmd,
            Duration::from_secs(5),
            &mut self.at_state,
        )
        .context(format!(
            "failed to set SMS storage to {} (AT+CPMS)",
            storage_str
        ))?;

        // 7. AT+CNMI=2,1,0,0,0 - new SMS indications
        send_command(
            &mut self.port,
            &self.clock,
            "AT+CNMI=2,1,0,0,0",
            Duration::from_secs(3),
            &mut self.at_state,
        )
        .context("failed to configure new SMS indications (AT+CNMI)")?;

        // Sweep messages already in storage once initialization is complete.
        self.sweep_stored_messages();
        Ok(())
    }

    /// Send AT commands until we get OK response (modem sync)
    fn at_sync(&mut self) -> Result<()> {
        const MAX_RETRIES: usize = 10;
        const RETRY_DELAY: Duration = Duration::from_millis(500);

        for attempt in 1..=MAX_RETRIES {
            log::debug!("AT sync attempt {}/{}", attempt, MAX_RETRIES);
            match send_command(
                &mut self.port,
                &self.clock,
                "AT",
                Duration::from_secs(2),
                &mut self.at_state,
            ) {
                Ok(_) => {
                    log::info!("Modem responded to AT sync on attempt {}", attempt);
                    return Ok(());
                }
                Err(e) => {
                    log::debug!("AT sync attempt {} failed: {}", attempt, e);
                    if attempt < MAX_RETRIES {
                        self.clock.sleep(RETRY_DELAY);
                    }
                }
            }
        }
        anyhow::bail!("modem did not respond to AT after {} attempts", MAX_RETRIES);
    }

    /// Wait for network registration (CREG and CEREG) with bounded wait
    fn wait_for_network_registration(&mut self) -> Result<()> {
        const MAX_WAIT: Duration = Duration::from_secs(30);
        const POLL_INTERVAL: Duration = Duration::from_secs(2);
        let deadline = self.clock.now() + MAX_WAIT;

        log::info!("Waiting for network registration...");

        while self.clock.now() < deadline {
            // Check CREG (2G/3G)
            if let Ok(lines) = send_command(
                &mut self.port,
                &self.clock,
                "AT+CREG?",
                Duration::from_secs(3),
                &mut self.at_state,
            ) {
                if let Some(status) = parse_creg_response(&lines) {
                    if status == 1 || status == 5 {
                        log::info!("Network registered (CREG: {})", status);
                        return Ok(());
                    }
                }
            }

            // Check CEREG (LTE)
            if let Ok(lines) = send_command(
                &mut self.port,
                &self.clock,
                "AT+CEREG?",
                Duration::from_secs(3),
                &mut self.at_state,
            ) {
                if let Some(status) = parse_cereg_response(&lines) {
                    if status == 1 || status == 5 {
                        log::info!("Network registered (CEREG: {})", status);
                        return Ok(());
                    }
                }
            }

            log::debug!("Network not yet registered, waiting...");
            self.clock.sleep(POLL_INTERVAL);
        }

        log::warn!(
            "Network registration timeout after {}s, continuing anyway",
            MAX_WAIT.as_secs()
        );
        Ok(())
    }

    /// Read SMS and return full ParseResult including concat info
    pub fn read_with_concat(&mut self, index: u16) -> Result<ParseResult> {
        let lines = self.read_response(index)?;
        crate::pdu::parse_pdu_with_udh_response(&lines)
    }

    fn read_response(&mut self, index: u16) -> Result<Vec<String>> {
        send_command(
            &mut self.port,
            &self.clock,
            &format!("AT+CMGR={}", index),
            Duration::from_secs(5),
            &mut self.at_state,
        )
    }

    /// Read SMS (backward compatible - returns only Sms)
    pub fn read(&mut self, index: u16) -> Result<Sms> {
        self.read_with_concat(index).map(|r| r.sms)
    }

    pub fn delete(&mut self, index: u16) -> Result<()> {
        send_command(
            &mut self.port,
            &self.clock,
            &format!("AT+CMGD={}", index),
            Duration::from_secs(3),
            &mut self.at_state,
        )?;
        Ok(())
    }

    fn delete_logged(&mut self, index: u16) {
        if let Err(error) = self.delete(index) {
            log::error!("Failed to delete SMS at index {}: {}", index, error);
        }
    }

    pub fn on_sms<F>(&mut self, cb: F)
    where
        F: FnMut(Sms) + Send + 'static,
    {
        self.callback = Some(Box::new(cb));
    }

    pub fn poll(&mut self, timeout: Duration) -> Result<()> {
        let now = self.clock.now();

        // Check for new incoming SMS
        if let Some(index) = poll_urcs(&mut self.port, &self.clock, &mut self.at_state, timeout)? {
            log::info!("Incoming SMS notification at index {}, reading...", index);
            self.process_message(index, now);
        }

        // Expire old incomplete groups
        let expired = self.reassembler.expire(now);
        if !expired.is_empty() {
            log::info!("Expired {} incomplete SMS group(s)", expired.len());
        }
        self.handle_completed(expired)?;

        // Periodically recover messages for which a +CMTI indication was missed.
        if self
            .last_sweep
            .map(|last| now.duration_since(last) >= self.sweep_interval)
            .unwrap_or(false)
        {
            self.sweep_stored_messages();
        }

        Ok(())
    }

    fn handle_completed(&mut self, completed: Vec<CompletedMessage>) -> Result<()> {
        for msg in completed {
            // Call callback with joined message
            if let Some(ref mut cb) = self.callback {
                cb(msg.sms);
            }
            // Delete all indices for this group. A failed delete must not stop
            // processing of the remaining messages.
            for index in msg.indices {
                self.delete_logged(index);
            }
        }
        Ok(())
    }

    fn process_message(&mut self, index: u16, now: Instant) {
        let lines = match self.read_response(index) {
            Ok(lines) => lines,
            Err(error) => {
                log::error!("Failed to read SMS at index {}: {}", index, error);
                self.delete_logged(index);
                return;
            }
        };

        let raw_pdu = extract_pdu(&lines).unwrap_or_else(|| "<missing>".to_string());
        if is_non_deliver_pdu(&raw_pdu) {
            log::info!("Deleting non-DELIVER SMS at index {}", index);
            self.delete_logged(index);
            return;
        }
        let parsed = crate::pdu::parse_pdu_with_udh(&raw_pdu);
        let parse_result = match parsed {
            Ok(result) => result,
            Err(error) => {
                log::error!(
                    "Failed to parse SMS at index {} (raw PDU: {}): {}",
                    index,
                    raw_pdu,
                    error
                );
                self.delete_logged(index);
                return;
            }
        };

        log::info!(
            "Successfully read SMS from {}, feeding to reassembler...",
            parse_result.sms.sender
        );
        let completed = self.reassembler.feed(
            index,
            &parse_result.sms.sender,
            &parse_result.sms.date,
            &parse_result.sms.message,
            parse_result.concat.as_ref(),
            now,
        );
        let _ = self.handle_completed(completed);
    }

    /// List and process all messages currently in the selected SMS store.
    fn sweep_stored_messages(&mut self) {
        let now = self.clock.now();
        self.last_sweep = Some(now);

        let lines = match send_command(
            &mut self.port,
            &self.clock,
            "AT+CMGL=4",
            Duration::from_secs(5),
            &mut self.at_state,
        ) {
            Ok(lines) => lines,
            Err(error) => {
                log::error!("Failed to sweep stored SMS messages (AT+CMGL=4): {}", error);
                return;
            }
        };

        for (index, raw_pdu) in parse_cmg_l_messages(&lines) {
            log::info!("Sweeping stored SMS at index {}, processing...", index);
            match raw_pdu {
                Some(raw_pdu) => self.process_swept_message(index, &raw_pdu, now),
                None => {
                    log::error!("Missing PDU for swept SMS at index {}", index);
                    self.delete_logged(index);
                }
            }
        }
    }

    fn process_swept_message(&mut self, index: u16, raw_pdu: &str, now: Instant) {
        if is_non_deliver_pdu(raw_pdu) {
            log::info!("Deleting non-DELIVER swept SMS at index {}", index);
            self.delete_logged(index);
            return;
        }
        let parsed = crate::pdu::parse_pdu_with_udh(raw_pdu);
        let parse_result = match parsed {
            Ok(result) => result,
            Err(error) => {
                log::error!(
                    "Failed to parse swept SMS at index {} (raw PDU: {}): {}",
                    index,
                    raw_pdu,
                    error
                );
                self.delete_logged(index);
                return;
            }
        };

        let completed = self.reassembler.feed(
            index,
            &parse_result.sms.sender,
            &parse_result.sms.date,
            &parse_result.sms.message,
            parse_result.concat.as_ref(),
            now,
        );
        let _ = self.handle_completed(completed);
    }
}

fn extract_pdu(lines: &[String]) -> Option<String> {
    let position = lines
        .iter()
        .position(|line| line.contains("+CMGR:") || line.contains("+CMGL:"))
        .or_else(|| lines.iter().position(|line| !line.trim().is_empty()))?;
    lines.get(position + 1).map(|pdu| pdu.trim().to_string())
}

fn is_non_deliver_pdu(raw_pdu: &str) -> bool {
    let Ok(bytes) = crate::pdu::hex_to_bytes(raw_pdu) else {
        return false;
    };
    if bytes.is_empty() {
        return false;
    }
    let smsc_len = bytes[0] as usize;
    let tp_index = 1 + smsc_len;
    bytes
        .get(tp_index)
        .map(|first_octet| first_octet & 0x03 != 0)
        .unwrap_or(false)
}

/// Parse the `+CMGL: <index>,<stat>,<alpha>,<length>` records and their PDU lines.
/// The A7670 emits both `,,` and `,"",` for an empty alpha field.
fn parse_cmg_l_messages(lines: &[String]) -> Vec<(u16, Option<String>)> {
    let mut messages = Vec::new();
    let mut position = 0;
    while position < lines.len() {
        let line = lines[position].trim();
        if !line.starts_with("+CMGL:") {
            position += 1;
            continue;
        }

        let header = line.trim_start_matches("+CMGL:").trim();
        let mut fields = header.splitn(4, ',');
        let index = fields
            .next()
            .and_then(|value| value.trim().parse::<u16>().ok());
        let _stat = fields.next();
        let _alpha = fields.next().unwrap_or("");
        let _length = fields.next().unwrap_or("");

        if index.is_none() {
            log::warn!("Ignoring malformed +CMGL header: {}", line);
            position += 1;
            continue;
        }

        if let Some(raw_pdu) = lines.get(position + 1) {
            messages.push((index.unwrap(), Some(raw_pdu.trim().to_string())));
            position += 2;
        } else {
            log::warn!("Missing PDU after +CMGL header: {}", line);
            messages.push((index.unwrap(), None));
            position += 1;
        }
    }
    messages
}

fn parse_creg_response(lines: &[String]) -> Option<u8> {
    for line in lines {
        if line.starts_with("+CREG:") {
            // Format: +CREG: <n>,<stat>[,<lac>,<ci>[,<AcT>]]
            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() >= 2 {
                if let Ok(stat) = parts[1].trim().parse::<u8>() {
                    return Some(stat);
                }
            }
        }
    }
    None
}

fn parse_cereg_response(lines: &[String]) -> Option<u8> {
    for line in lines {
        if line.starts_with("+CEREG:") {
            // Format: +CEREG: <n>,<stat>[,<tac>,<ci>[,<AcT>]]
            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() >= 2 {
                if let Ok(stat) = parts[1].trim().parse::<u8>() {
                    return Some(stat);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests;
