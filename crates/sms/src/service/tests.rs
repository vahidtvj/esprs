use super::*;
use crate::at_port::{AtPortRaw, MockClock};
use crate::test_vectors::concat_cases::*;
use crate::test_vectors::ok_cases::GSM7_HI;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Mock raw port for testing SmsService
struct MockRawPort {
    /// Queue of (index, PDU response lines) for CMGR reads
    cmgr_responses: Mutex<VecDeque<(u16, Vec<String>)>>,
    /// Queue of CMGL response lines
    cmgl_responses: Mutex<VecDeque<Vec<String>>>,
    /// Indices whose CMGD command should return ERROR
    cmgd_errors: Mutex<Vec<u16>>,
    /// Queue of raw byte chunks to return on read
    read_queue: Mutex<VecDeque<Vec<u8>>>,
    /// Record of CMGD commands sent
    deleted_indices: Mutex<Vec<u16>>,
    /// Record of commands written (for verification)
    written_commands: Mutex<Vec<String>>,
    /// Whether to simulate CPIN not ready
    cpin_not_ready: Mutex<bool>,
    /// Whether to simulate CMGF error
    cmgf_error: Mutex<bool>,
    /// AT sync failure count (for testing retries)
    at_sync_failures: Mutex<usize>,
}

impl MockRawPort {
    fn new() -> Self {
        Self {
            cmgr_responses: Mutex::new(VecDeque::new()),
            cmgl_responses: Mutex::new(VecDeque::new()),
            cmgd_errors: Mutex::new(Vec::new()),
            read_queue: Mutex::new(VecDeque::new()),
            deleted_indices: Mutex::new(Vec::new()),
            written_commands: Mutex::new(Vec::new()),
            cpin_not_ready: Mutex::new(false),
            cmgf_error: Mutex::new(false),
            at_sync_failures: Mutex::new(0),
        }
    }

    /// Add a CMGR response for a specific index (as raw bytes)
    fn add_cmgr_response(&self, index: u16, lines: Vec<String>) {
        self.cmgr_responses
            .lock()
            .unwrap()
            .push_back((index, lines));
    }

    fn add_cmgl_response(&self, lines: Vec<String>) {
        self.cmgl_responses.lock().unwrap().push_back(lines);
    }

    fn set_cmgd_error(&self, index: u16) {
        self.cmgd_errors.lock().unwrap().push(index);
    }

    /// Add raw bytes to the read queue
    fn push_read(&self, data: Vec<u8>) {
        self.read_queue.lock().unwrap().push_back(data);
    }

    /// Get deleted indices
    fn get_deleted(&self) -> Vec<u16> {
        self.deleted_indices.lock().unwrap().clone()
    }

    /// Get written commands for verification
    fn get_written_commands(&self) -> Vec<String> {
        self.written_commands.lock().unwrap().clone()
    }

    /// Set CPIN not ready for testing
    fn set_cpin_not_ready(&self, val: bool) {
        *self.cpin_not_ready.lock().unwrap() = val;
    }

    /// Set CMGF error for testing
    fn set_cmgf_error(&self, val: bool) {
        *self.cmgf_error.lock().unwrap() = val;
    }

    /// Set AT sync failures for testing retries
    fn set_at_sync_failures(&self, count: usize) {
        *self.at_sync_failures.lock().unwrap() = count;
    }
}

impl AtPortRaw for MockRawPort {
    fn write_raw(&mut self, data: &[u8]) -> Result<()> {
        let cmd = String::from_utf8_lossy(data).trim().to_string();
        self.written_commands.lock().unwrap().push(cmd.clone());

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
            if self.cmgd_errors.lock().unwrap().contains(&idx) {
                self.push_read(b"ERROR\r\n".to_vec());
            } else {
                self.push_read(b"OK\r\n".to_vec());
            }
            return Ok(());
        } else if cmd == "AT+CMGL=4" {
            let response = self
                .cmgl_responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| vec!["OK".to_string()])
                .join("\r\n")
                + "\r\n";
            self.push_read(response.into_bytes());
            return Ok(());
        }

        // For init commands, provide appropriate responses
        let response = if cmd == "AT" {
            // Handle AT sync retries
            let mut failures = self.at_sync_failures.lock().unwrap();
            if *failures > 0 {
                *failures -= 1;
                // Return timeout/error to trigger retry
                b"".to_vec() // No response -> timeout
            } else {
                b"OK\r\n".to_vec()
            }
        } else if cmd == "ATE0" {
            b"OK\r\n".to_vec()
        } else if cmd == "AT+CPIN?" {
            if *self.cpin_not_ready.lock().unwrap() {
                b"\r\n+CPIN: SIM PIN\r\n\r\nOK\r\n".to_vec()
            } else {
                b"\r\n+CPIN: READY\r\n\r\nOK\r\n".to_vec()
            }
        } else if cmd == "AT+CREG?" {
            b"\r\n+CREG: 0,1\r\n\r\nOK\r\n".to_vec()
        } else if cmd == "AT+CEREG?" {
            b"\r\n+CEREG: 0,1\r\n\r\nOK\r\n".to_vec()
        } else if cmd == "AT+CMGF=0" {
            if *self.cmgf_error.lock().unwrap() {
                b"ERROR\r\n".to_vec()
            } else {
                b"OK\r\n".to_vec()
            }
        } else if cmd.starts_with("AT+CPMS=") {
            b"\r\n+CPMS: 10,50,10,50,10,50\r\n\r\nOK\r\n".to_vec()
        } else if cmd == "AT+CNMI=2,1,0,0,0" {
            b"OK\r\n".to_vec()
        } else {
            b"OK\r\n".to_vec()
        };
        self.push_read(response);
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
fn test_init_command_order() {
    let port = MockRawPort::new();
    let clock = MockClock::new(Instant::now());
    let mut service = SmsService::with_clock_and_storage(port, clock, SmsStorage::SM);

    service.init().unwrap();

    let commands = service.port.get_written_commands();
    // Verify exact command order
    assert_eq!(commands[0], "AT"); // AT sync
    assert_eq!(commands[1], "ATE0"); // Echo off
    assert_eq!(commands[2], "AT+CPIN?"); // PIN status
                                         // CREG/CEREG checks may appear multiple times during wait
                                         // Find CMGF=0
    let cmgf_idx = commands
        .iter()
        .position(|c| c == "AT+CMGF=0")
        .expect("CMGF=0 not found");
    // CPMS should be after CMGF
    let cpms_idx = commands
        .iter()
        .position(|c| c.starts_with("AT+CPMS="))
        .expect("CPMS not found");
    assert!(cmgf_idx < cpms_idx, "CPMS should come after CMGF=0");
    // CNMI should be after CPMS
    let cnmi_idx = commands
        .iter()
        .position(|c| c == "AT+CNMI=2,1,0,0,0")
        .expect("CNMI not found");
    assert!(cpms_idx < cnmi_idx, "CNMI should come after CPMS");

    // Verify CPMS uses SM storage
    let cpms_cmd = &commands[cpms_idx];
    assert!(
        cpms_cmd.contains("\"SM\""),
        "CPMS should use SM storage: {}",
        cpms_cmd
    );
    assert!(
        !commands.iter().any(|c| c == "AT+CSCS=\"UCS2\""),
        "CSCS=UCS2 should not be sent"
    );
}

#[test]
fn test_init_storage_me() {
    let port = MockRawPort::new();
    let clock = MockClock::new(Instant::now());
    let mut service = SmsService::with_clock_and_storage(port, clock, SmsStorage::ME);

    service.init().unwrap();

    let commands = service.port.get_written_commands();
    let cpms_idx = commands
        .iter()
        .position(|c| c.starts_with("AT+CPMS="))
        .expect("CPMS not found");
    let cpms_cmd = &commands[cpms_idx];
    assert!(
        cpms_cmd.contains("\"ME\""),
        "CPMS should use ME storage: {}",
        cpms_cmd
    );
}

#[test]
fn test_init_cpin_not_ready() {
    let port = MockRawPort::new();
    port.set_cpin_not_ready(true);
    let clock = MockClock::new(Instant::now());
    let mut service = SmsService::with_clock_and_storage(port, clock, SmsStorage::SM);

    let result = service.init();
    assert!(result.is_err(), "init should fail when CPIN not ready");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("SIM not ready"),
        "Error should mention SIM not ready: {}",
        err
    );
}

#[test]
fn test_init_cmgf_error() {
    let port = MockRawPort::new();
    port.set_cmgf_error(true);
    let clock = MockClock::new(Instant::now());
    let mut service = SmsService::with_clock_and_storage(port, clock, SmsStorage::SM);

    let result = service.init();
    assert!(result.is_err(), "init should fail when CMGF returns ERROR");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("PDU mode"),
        "Error should mention PDU mode: {}",
        err
    );
}

#[test]
fn test_at_sync_retries() {
    let port = MockRawPort::new();
    // Fail first 3 AT attempts, then succeed
    port.set_at_sync_failures(3);
    let clock = MockClock::new(Instant::now());
    let mut service = SmsService::with_clock_and_storage(port, clock, SmsStorage::SM);

    service.init().unwrap();

    let commands = service.port.get_written_commands();
    // Should have 4 AT commands (3 failures + 1 success)
    let at_count = commands.iter().filter(|c| *c == "AT").count();
    assert_eq!(
        at_count, 4,
        "AT sync should be attempted 4 times (3 retries + success)"
    );
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
    let mut service = SmsService::with_clock_and_storage(port, clock, SmsStorage::SM);

    let received = Arc::new(Mutex::new(Vec::new()));
    let received_clone = received.clone();
    service.on_sms(move |sms| {
        received_clone.lock().unwrap().push(sms.message);
    });

    service.init().unwrap();

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
    let mut service = SmsService::with_clock_and_storage(port, clock.clone(), SmsStorage::SM);

    let received = Arc::new(Mutex::new(Vec::new()));
    let received_clone = received.clone();
    service.on_sms(move |sms| {
        received_clone.lock().unwrap().push(sms.message);
    });

    service.init().unwrap();

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

fn service_with_callback(
    port: MockRawPort,
    clock: MockClock,
) -> (SmsService<MockRawPort, MockClock>, Arc<Mutex<Vec<String>>>) {
    let received = Arc::new(Mutex::new(Vec::new()));
    let received_clone = received.clone();
    let mut service = SmsService::with_sweep_interval(port, clock, Duration::from_secs(60));
    service.on_sms(move |sms| {
        received_clone.lock().unwrap().push(sms.message);
    });
    (service, received)
}

#[test]
fn test_cmgl_sweep_parses_both_empty_alpha_forms_and_deletes() {
    let port = MockRawPort::new();
    port.add_cmgl_response(vec![
        "+CMGL: 1,1,,21".to_string(),
        "0791893905004100040C9189291032547600006211017191854002C834".to_string(),
        "+CMGL: 3,1,\"\",29".to_string(),
        "0791893905004100040C918929103254760000621101719185400BC8329BFD065DDF723619".to_string(),
        "OK".to_string(),
    ]);
    let clock = MockClock::new(Instant::now());
    let (mut service, received) = service_with_callback(port, clock.clone());

    service.init().unwrap();
    clock.advance(Duration::from_secs(60));
    service.poll(Duration::from_millis(1)).unwrap();

    assert_eq!(*received.lock().unwrap(), vec!["Hi", "Hello World"]);
    assert_eq!(service.port.get_deleted(), vec![1, 3]);
}

#[test]
fn test_periodic_sweep_waits_for_interval() {
    let port = MockRawPort::new();
    let clock = MockClock::new(Instant::now());
    let (mut service, received) = service_with_callback(port, clock.clone());

    service.init().unwrap();
    let cmgl_count_after_init = service
        .port
        .get_written_commands()
        .iter()
        .filter(|command| *command == "AT+CMGL=4")
        .count();

    service.poll(Duration::from_millis(1)).unwrap();
    let cmgl_count_before_interval = service
        .port
        .get_written_commands()
        .iter()
        .filter(|command| *command == "AT+CMGL=4")
        .count();
    assert_eq!(cmgl_count_before_interval, cmgl_count_after_init);
    assert!(received.lock().unwrap().is_empty());
}

#[test]
fn test_cmgr_invalid_pdu_is_deleted_without_callback() {
    let port = MockRawPort::new();
    port.add_cmgr_response(
        7,
        vec![
            "+CMGR: 0,\"\",2".to_string(),
            "ZZ".to_string(),
            "OK".to_string(),
        ],
    );
    port.push_read(b"\r\n+CMTI: \"SM\",7\r\n".to_vec());
    let clock = MockClock::new(Instant::now());
    let (mut service, received) = service_with_callback(port, clock);

    service.init().unwrap();
    service.poll(Duration::from_millis(1)).unwrap();

    assert!(received.lock().unwrap().is_empty());
    assert_eq!(service.port.get_deleted(), vec![7]);
}

#[test]
fn test_sms_submit_is_deleted_without_callback() {
    let port = MockRawPort::new();
    port.add_cmgl_response(vec![
        "+CMGL: 2,3,,29".to_string(),
        "0791893905004100010C9189291032547600006211017191854002C834".to_string(),
        "OK".to_string(),
    ]);
    let clock = MockClock::new(Instant::now());
    let (mut service, received) = service_with_callback(port, clock.clone());

    service.init().unwrap();
    clock.advance(Duration::from_secs(60));
    service.poll(Duration::from_millis(1)).unwrap();

    assert!(received.lock().unwrap().is_empty());
    assert_eq!(service.port.get_deleted(), vec![2]);
}

#[test]
fn test_cmgd_failure_does_not_abort_later_messages() {
    let port = MockRawPort::new();
    port.set_cmgd_error(1);
    port.add_cmgl_response(vec![
        "+CMGL: 1,1,,21".to_string(),
        "0791893905004100040C9189291032547600006211017191854002C834".to_string(),
        "+CMGL: 3,1,\"\",29".to_string(),
        "0791893905004100040C918929103254760000621101719185400BC8329BFD065DDF723619".to_string(),
        "OK".to_string(),
    ]);
    let clock = MockClock::new(Instant::now());
    let (mut service, received) = service_with_callback(port, clock.clone());

    service.init().unwrap();
    clock.advance(Duration::from_secs(60));
    service.poll(Duration::from_millis(1)).unwrap();

    assert_eq!(*received.lock().unwrap(), vec!["Hi", "Hello World"]);
    assert_eq!(service.port.get_deleted(), vec![1, 3]);
}

#[test]
fn test_cmgl_sweep_reassembles_multipart_message() {
    let port = MockRawPort::new();
    port.add_cmgl_response(vec![
        format!("+CMGL: 4,1,,{}", GSM7_10_P1.0.len() / 2),
        GSM7_10_P1.0.to_string(),
        format!("+CMGL: 5,1,,{}", GSM7_10_P2.0.len() / 2),
        GSM7_10_P2.0.to_string(),
        format!("+CMGL: 6,1,,{}", GSM7_10_P3.0.len() / 2),
        GSM7_10_P3.0.to_string(),
        "OK".to_string(),
    ]);
    let clock = MockClock::new(Instant::now());
    let (mut service, received) = service_with_callback(port, clock.clone());

    service.init().unwrap();
    clock.advance(Duration::from_secs(60));
    service.poll(Duration::from_millis(1)).unwrap();

    assert_eq!(*received.lock().unwrap(), vec!["one two three"]);
    assert_eq!(service.port.get_deleted(), vec![4, 5, 6]);
}

#[test]
fn test_non_multipart_delivers_and_deletes() {
    // Plain GSM7_HI message
    let pdu = GSM7_HI.0;

    let port = MockRawPort::new();
    port.add_cmgr_response(7, cmgr_lines(pdu));
    port.push_read(b"\r\n+CMTI: \"SM\",7\r\n".to_vec());

    let clock = MockClock::new(Instant::now());
    let mut service = SmsService::with_clock_and_storage(port, clock, SmsStorage::SM);

    let received = Arc::new(Mutex::new(Vec::new()));
    let received_clone = received.clone();
    service.on_sms(move |sms| {
        received_clone.lock().unwrap().push(sms.message);
    });

    service.init().unwrap();

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
