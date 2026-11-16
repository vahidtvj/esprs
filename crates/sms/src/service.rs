use anyhow::Result;
use std::time::{Duration, Instant};

use crate::reassembler::{CompletedMessage, Reassembler, ReassemblerConfig};
use crate::{ParseResult, Sms};

/// Trait for injectable clock - allows testing with mocked time
pub trait Clock {
    fn now(&self) -> Instant;
}

/// Real clock using system time
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

pub trait AtPort {
    fn send_command(&mut self, cmd: &str, timeout: Duration) -> Result<Vec<String>>;
    fn on_urc(&mut self, callback: Box<dyn FnMut(&str) + Send + 'static>);
    fn poll(&mut self, timeout: Duration) -> Result<Option<u8>>;
}

pub struct SmsService<P: AtPort, C: Clock> {
    port: P,
    callback: Option<Box<dyn FnMut(Sms) + Send + 'static>>,
    reassembler: Reassembler,
    clock: C,
}

impl<P: AtPort> SmsService<P, SystemClock> {
    pub fn new(port: P) -> Self {
        Self::with_clock(port, SystemClock)
    }
}

impl<P: AtPort, C: Clock> SmsService<P, C> {
    pub fn with_clock(port: P, clock: C) -> Self {
        Self {
            port,
            callback: None,
            reassembler: Reassembler::new(),
            clock,
        }
    }

    pub fn with_config(port: P, clock: C, config: ReassemblerConfig) -> Self {
        Self {
            port,
            callback: None,
            reassembler: Reassembler::with_config(config),
            clock,
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

    /// Read SMS and return full ParseResult including concat info
    pub fn read_with_concat(&mut self, index: u8) -> Result<ParseResult> {
        let lines = self
            .port
            .send_command(&format!("AT+CMGR={index}"), Duration::from_secs(5))?;
        crate::pdu::parse_pdu_with_udh_response(&lines)
    }

    /// Read SMS (backward compatible - returns only Sms)
    pub fn read(&mut self, index: u8) -> Result<Sms> {
        self.read_with_concat(index).map(|r| r.sms)
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
        let now = self.clock.now();

        // Check for new incoming SMS
        if let Some(index) = self.port.poll(timeout)? {
            log::info!("Incoming SMS notification at index {}, reading...", index);
            match self.read_with_concat(index) {
                Ok(parse_result) => {
                    log::info!(
                        "Successfully read SMS from {}, feeding to reassembler...",
                        parse_result.sms.sender
                    );
                    let completed = self.reassembler.feed(
                        index as u16,
                        &parse_result.sms.sender,
                        &parse_result.sms.date,
                        &parse_result.sms.message,
                        parse_result.concat.as_ref(),
                        now,
                    );
                    self.handle_completed(completed)?;
                }
                Err(e) => {
                    log::error!("Failed to read SMS at index {}: {}", index, e);
                }
            }
        }

        // Expire old incomplete groups
        let expired = self.reassembler.expire(now);
        if !expired.is_empty() {
            log::info!("Expired {} incomplete SMS group(s)", expired.len());
        }
        self.handle_completed(expired)?;

        Ok(())
    }

    fn handle_completed(&mut self, completed: Vec<CompletedMessage>) -> Result<()> {
        for msg in completed {
            // Call callback with joined message
            if let Some(ref mut cb) = self.callback {
                cb(msg.sms);
            }
            // Delete all indices for this group
            for index in msg.indices {
                let _ = self.delete(index as u8);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_vectors::concat_cases::*;
    use crate::test_vectors::ok_cases::GSM7_HI;
    use crate::Sms;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// Mock clock that allows controlling time
    #[derive(Clone)]
    struct MockClock {
        now: Arc<Mutex<Instant>>,
    }

    impl MockClock {
        fn new(start: Instant) -> Self {
            Self {
                now: Arc::new(Mutex::new(start)),
            }
        }

        fn advance(&self, dur: Duration) {
            *self.now.lock().unwrap() += dur;
        }
    }

    impl Clock for MockClock {
        fn now(&self) -> Instant {
            *self.now.lock().unwrap()
        }
    }

    /// Mock AT port for testing
    struct MockAtPort {
        /// Queue of (index, PDU response lines) for CMGR reads
        cmgr_responses: Mutex<VecDeque<(u8, Vec<String>)>>,
        /// Queue of CMTI indices to return from poll
        cmti_queue: Mutex<VecDeque<u8>>,
        /// Record of CMGD commands sent
        deleted_indices: Mutex<Vec<u8>>,
    }

    impl MockAtPort {
        fn new() -> Self {
            Self {
                cmgr_responses: Mutex::new(VecDeque::new()),
                cmti_queue: Mutex::new(VecDeque::new()),
                deleted_indices: Mutex::new(Vec::new()),
            }
        }

        /// Add a CMGR response for a specific index
        fn add_cmgr_response(&self, index: u8, lines: Vec<String>) {
            self.cmgr_responses
                .lock()
                .unwrap()
                .push_back((index, lines));
        }

        /// Add a CMTI notification index
        fn add_cmti(&self, index: u8) {
            self.cmti_queue.lock().unwrap().push_back(index);
        }

        /// Get deleted indices
        fn get_deleted(&self) -> Vec<u8> {
            self.deleted_indices.lock().unwrap().clone()
        }
    }

    impl AtPort for MockAtPort {
        fn send_command(&mut self, cmd: &str, _timeout: Duration) -> Result<Vec<String>> {

            if cmd.starts_with("AT+CMGR=") {
                let mut responses = self.cmgr_responses.lock().unwrap();
                if let Some((idx, lines)) = responses.pop_front() {
                    // Verify index matches
                    let expected_idx = cmd["AT+CMGR=".len()..].parse::<u8>().unwrap();
                    assert_eq!(idx, expected_idx, "CMGR index mismatch");
                    return Ok(lines);
                }
                anyhow::bail!("No CMGR response queued for index");
            } else if cmd.starts_with("AT+CMGD=") {
                let idx = cmd["AT+CMGD=".len()..].parse::<u8>().unwrap();
                self.deleted_indices.lock().unwrap().push(idx);
                return Ok(vec!["OK".to_string()]);
            }
            // For init commands, return OK
            Ok(vec!["OK".to_string()])
        }

        fn on_urc(&mut self, _callback: Box<dyn FnMut(&str) + Send + 'static>) {
            // Not used in tests
        }

        fn poll(&mut self, _timeout: Duration) -> Result<Option<u8>> {
            let mut queue = self.cmti_queue.lock().unwrap();
            Ok(queue.pop_front())
        }
    }

    /// Build CMGR response lines for a PDU
    fn cmgr_lines(pdu: &str) -> Vec<String> {
        vec![
            format!("AT+CMGR=1"),
            format!("\"+CMGR: 0,\\\"\\\",{}\"", pdu.len() / 2),
            pdu.to_string(),
            "OK".to_string(),
        ]
    }

    #[test]
    fn test_multipart_three_parts_completes_and_deletes() {
        // Use GSM7_10_P1, P2, P3 at indices 4, 5, 6
        let p1_pdu = GSM7_10_P1.0;
        let p2_pdu = GSM7_10_P2.0;
        let p3_pdu = GSM7_10_P3.0;

        let port = MockAtPort::new();
        port.add_cmti(4);
        port.add_cmti(5);
        port.add_cmti(6);
        port.add_cmgr_response(4, cmgr_lines(p1_pdu));
        port.add_cmgr_response(5, cmgr_lines(p2_pdu));
        port.add_cmgr_response(6, cmgr_lines(p3_pdu));

        let clock = MockClock::new(Instant::now());
        let mut service = SmsService::with_clock(port, clock);

        let received = Arc::new(Mutex::new(Vec::new()));
        let received_clone = received.clone();
        service.on_sms(move |sms| {
            received_clone.lock().unwrap().push(sms.message);
        });

        // Poll for all three parts
        service.poll(Duration::from_millis(100)).unwrap();
        service.poll(Duration::from_millis(100)).unwrap();
        service.poll(Duration::from_millis(100)).unwrap();

        // Check callback was called once with joined message
        let msgs = received.lock().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0], "one two three");

        // Check all three indices were deleted
        let deleted = service.port.get_deleted();
        assert_eq!(deleted.len(), 3);
        let mut deleted_sorted = deleted.clone();
        deleted_sorted.sort();
        assert_eq!(deleted_sorted, vec![4, 5, 6]);
    }

    #[test]
    fn test_multipart_single_part_expires() {
        // Only P1 arrives at index 4
        let p1_pdu = GSM7_10_P1.0;

        let port = MockAtPort::new();
        port.add_cmti(4);
        port.add_cmgr_response(4, cmgr_lines(p1_pdu));

        let start = Instant::now();
        let clock = MockClock::new(start);
        let mut service = SmsService::with_clock(port, clock.clone());

        let received = Arc::new(Mutex::new(Vec::new()));
        let received_clone = received.clone();
        service.on_sms(move |sms| {
            received_clone.lock().unwrap().push(sms.message);
        });

        // Poll for the first part
        service.poll(Duration::from_millis(100)).unwrap();

        // No callback yet (group incomplete)
        assert!(received.lock().unwrap().is_empty());

        // Advance time beyond timeout (5 minutes + 1 second)
        clock.advance(Duration::from_secs(5 * 60 + 1));

        // Poll again - should expire and deliver partial message
        service.poll(Duration::from_millis(100)).unwrap();

        // Check callback was called with partial message
        let msgs = received.lock().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0], "one ");

        // Check index 4 was deleted
        let deleted = service.port.get_deleted();
        assert_eq!(deleted, vec![4]);
    }

    #[test]
    fn test_non_multipart_delivers_and_deletes() {
        // Plain GSM7_HI message
        let pdu = GSM7_HI.0;

        let port = MockAtPort::new();
        port.add_cmti(7);
        port.add_cmgr_response(7, cmgr_lines(pdu));

        let clock = MockClock::new(Instant::now());
        let mut service = SmsService::with_clock(port, clock);

        let received = Arc::new(Mutex::new(Vec::new()));
        let received_clone = received.clone();
        service.on_sms(move |sms| {
            received_clone.lock().unwrap().push(sms.message);
        });

        // Poll for the message
        service.poll(Duration::from_millis(100)).unwrap();

        // Check callback was called with message
        let msgs = received.lock().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0], "Hi");

        // Check index 7 was deleted
        let deleted = service.port.get_deleted();
        assert_eq!(deleted, vec![7]);
    }
}
