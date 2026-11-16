use anyhow::Result;
use std::time::Duration;

use crate::at_port::{poll_urcs, send_command, AtPortRaw, AtState, Clock, SystemClock};
use crate::reassembler::{CompletedMessage, Reassembler, ReassemblerConfig};
use crate::{ParseResult, Sms};

pub struct SmsService<P: AtPortRaw, C: Clock> {
    port: P,
    callback: Option<Box<dyn FnMut(Sms) + Send + 'static>>,
    reassembler: Reassembler,
    clock: C,
    /// AT state for line framing and URC handling
    at_state: AtState,
}

impl<P: AtPortRaw> SmsService<P, SystemClock> {
    pub fn new(port: P) -> Self {
        Self::with_clock(port, SystemClock)
    }
}

impl<P: AtPortRaw, C: Clock> SmsService<P, C> {
    pub fn with_clock(port: P, clock: C) -> Self {
        Self {
            port,
            callback: None,
            reassembler: Reassembler::new(),
            clock,
            at_state: AtState::new(),
        }
    }

    pub fn with_config(port: P, clock: C, config: ReassemblerConfig) -> Self {
        Self {
            port,
            callback: None,
            reassembler: Reassembler::with_config(config),
            clock,
            at_state: AtState::new(),
        }
    }

    pub fn init(&mut self) -> Result<()> {
        send_command(
            &mut self.port,
            &self.clock,
            "AT+CMGF=0",
            Duration::from_secs(3),
            &mut self.at_state,
        )?;
        send_command(
            &mut self.port,
            &self.clock,
            "AT+CSCS=\"UCS2\"",
            Duration::from_secs(3),
            &mut self.at_state,
        )?;
        send_command(
            &mut self.port,
            &self.clock,
            "AT+CNMI=2,1,0,0,0",
            Duration::from_secs(3),
            &mut self.at_state,
        )?;
        Ok(())
    }

    /// Read SMS and return full ParseResult including concat info
    pub fn read_with_concat(&mut self, index: u16) -> Result<ParseResult> {
        let lines = send_command(
            &mut self.port,
            &self.clock,
            &format!("AT+CMGR={index}"),
            Duration::from_secs(5),
            &mut self.at_state,
        )?;
        crate::pdu::parse_pdu_with_udh_response(&lines)
    }

    /// Read SMS (backward compatible - returns only Sms)
    pub fn read(&mut self, index: u16) -> Result<Sms> {
        self.read_with_concat(index).map(|r| r.sms)
    }

    pub fn delete(&mut self, index: u16) -> Result<()> {
        send_command(
            &mut self.port,
            &self.clock,
            &format!("AT+CMGD={index}"),
            Duration::from_secs(3),
            &mut self.at_state,
        )?;
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
        if let Some(index) = poll_urcs(&mut self.port, &self.clock, &mut self.at_state, timeout)? {
            log::info!("Incoming SMS notification at index {}, reading...", index);
            match self.read_with_concat(index) {
                Ok(parse_result) => {
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
                let _ = self.delete(index);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::at_port::{AtPortRaw, MockClock};
    use crate::test_vectors::concat_cases::*;
    use crate::test_vectors::ok_cases::GSM7_HI;
    use crate::Sms;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// Mock raw port for testing SmsService
    struct MockRawPort {
        /// Queue of (index, PDU response lines) for CMGR reads
        cmgr_responses: Mutex<VecDeque<(u16, Vec<String>)>>,
        /// Queue of raw byte chunks to return on read
        read_queue: Mutex<VecDeque<Vec<u8>>>,
        /// Record of CMGD commands sent
        deleted_indices: Mutex<Vec<u16>>,
    }

    impl MockRawPort {
        fn new() -> Self {
            Self {
                cmgr_responses: Mutex::new(VecDeque::new()),
                read_queue: Mutex::new(VecDeque::new()),
                deleted_indices: Mutex::new(Vec::new()),
            }
        }

        /// Add a CMGR response for a specific index (as raw bytes)
        fn add_cmgr_response(&self, index: u16, lines: Vec<String>) {
            self.cmgr_responses
                .lock()
                .unwrap()
                .push_back((index, lines));
        }

        /// Add raw bytes to the read queue
        fn push_read(&self, data: Vec<u8>) {
            self.read_queue.lock().unwrap().push_back(data);
        }

        /// Get deleted indices
        fn get_deleted(&self) -> Vec<u16> {
            self.deleted_indices.lock().unwrap().clone()
        }
    }

    impl AtPortRaw for MockRawPort {
        fn write_raw(&mut self, data: &[u8]) -> Result<()> {
            // Parse the command to determine what response to queue
            let cmd = String::from_utf8_lossy(data).trim().to_string();

            if cmd.starts_with("AT+CMGR=") {
                let mut responses = self.cmgr_responses.lock().unwrap();
                if let Some((idx, lines)) = responses.pop_front() {
                    let expected_idx = cmd["AT+CMGR=".len()..].parse::<u16>().unwrap();
                    assert_eq!(idx, expected_idx, "CMGR index mismatch");
                    // Queue the response as raw bytes
                    let response = lines.join("\r\n") + "\r\n";
                    self.push_read(response.into_bytes());
                    return Ok(());
                }
                anyhow::bail!("No CMGR response queued for index");
            } else if cmd.starts_with("AT+CMGD=") {
                let idx = cmd["AT+CMGD=".len()..].parse::<u16>().unwrap();
                self.deleted_indices.lock().unwrap().push(idx);
                self.push_read(b"OK\r\n".to_vec());
                return Ok(());
            }
            // For init commands, return OK
            self.push_read(b"OK\r\n".to_vec());
            Ok(())
        }

        fn read_raw(&mut self, buf: &mut [u8], _timeout: Duration) -> Result<usize> {
            let mut queue = self.read_queue.lock().unwrap();
            if queue.is_empty() {
                return Ok(0);
            }
            let data = queue.pop_front().unwrap();
            let len = data.len().min(buf.len());
            buf[..len].copy_from_slice(&data[..len]);
            if data.len() > len {
                queue.push_front(data[len..].to_vec());
            }
            Ok(len)
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

        let port = MockRawPort::new();
        // Queue CMTI notifications as raw bytes - but we need to interleave them with CMGR responses
        // The issue is that send_command reads from the queue, so we need to queue CMGR responses
        // at the right time. Let's use the write_raw hook to queue CMGR responses.
        port.add_cmgr_response(4, cmgr_lines(p1_pdu));
        port.add_cmgr_response(5, cmgr_lines(p2_pdu));
        port.add_cmgr_response(6, cmgr_lines(p3_pdu));
        // Pre-queue CMTI notifications - they will be read by poll_urcs
        port.push_read(b"\r\n+CMTI: \"SM\",4\r\n".to_vec());
        port.push_read(b"\r\n+CMTI: \"SM\",5\r\n".to_vec());
        port.push_read(b"\r\n+CMTI: \"SM\",6\r\n".to_vec());

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

        let port = MockRawPort::new();
        port.add_cmgr_response(4, cmgr_lines(p1_pdu));
        port.push_read(b"\r\n+CMTI: \"SM\",4\r\n".to_vec());

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

        let port = MockRawPort::new();
        port.add_cmgr_response(7, cmgr_lines(pdu));
        port.push_read(b"\r\n+CMTI: \"SM\",7\r\n".to_vec());

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
