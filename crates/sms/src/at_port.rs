use anyhow::{bail, Result};
use std::time::{Duration, Instant};

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

/// Trait for AT command port - raw byte I/O only
/// All line framing and command termination logic is in this crate
pub trait AtPortRaw {
    fn write_raw(&mut self, data: &[u8]) -> Result<()>;
    fn read_raw(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize>;
}

/// Errors from AT command processing
#[derive(Debug, thiserror::Error)]
pub enum AtError {
    #[error("Command timeout")]
    Timeout,
    #[error("Modem error: {0}")]
    ModemError(String),
    #[error("CME error: {0}")]
    CmeError(String),
    #[error("CMS error: {0}")]
    CmsError(String),
}

/// State for AT command/response processing
pub struct AtState {
    /// Buffer for incomplete line (persists across reads)
    line_buf: String,
    /// Queue for URCs (+CMTI notifications)
    urc_queue: Vec<u16>,
    /// Whether we're currently in a command (waiting for OK/ERROR)
    in_command: bool,
    /// Lines collected during current command
    cmd_lines: Vec<String>,
    /// Whether the command echo line has been seen
    saw_echo: bool,
}

impl AtState {
    pub fn new() -> Self {
        Self {
            line_buf: String::new(),
            urc_queue: Vec::new(),
            in_command: false,
            cmd_lines: Vec::new(),
            saw_echo: false,
        }
    }

    /// Process incoming bytes, returning completed lines
    /// Returns (command_response_lines, urc_indices_found_during_command)
    /// command_response_lines is Some when a command completes (OK/ERROR/CME/CMS)
    /// URCs found while not in_command are queued internally for poll_urcs
    /// Processes entire chunk - bytes after terminator are not discarded
    pub fn process_bytes(&mut self, data: &[u8]) -> (Option<Result<Vec<String>>>, Vec<u16>) {
        let urcs_during_command = Vec::new();
        let mut command_result = None;
        let s = String::from_utf8_lossy(data);

        for c in s.chars() {
            if c == '\n' {
                let line = self.line_buf.trim().to_string();
                self.line_buf.clear();

                if line.is_empty() {
                    continue;
                }

                // Check for URC: +CMTI: "<mem>",<index>
                if line.starts_with("+CMTI:") {
                    if let Some(index) = parse_cmti(&line) {
                        if self.in_command {
                            // URC during command - queue it for later poll
                            self.urc_queue.push(index);
                        } else {
                            // URC outside command - queue it for poll
                            self.urc_queue.push(index);
                        }
                    }
                    // Ignore malformed/non-numeric CMTI
                    continue;
                }

                // Ignore other URCs: RING, +CLIP, etc.
                if line == "RING" || line.starts_with("+CLIP:") {
                    continue;
                }

                if self.in_command {
                    // Check for command echo (e.g., "AT+CMGF=0" or "\r\r\n")
                    if !self.saw_echo && (line.starts_with("AT") || line.is_empty()) {
                        self.saw_echo = true;
                        continue;
                    }

                    // Skip empty lines (matching original behavior)
                    if line.is_empty() {
                        continue;
                    }

                    // Collect response lines
                    self.cmd_lines.push(line.clone());

                    // Check for command terminators
                    if line == "OK"
                        || line == "ERROR"
                        || line.starts_with("+CME ERROR:")
                        || line.starts_with("+CMS ERROR:")
                    {
                        let result = if line == "OK" {
                            Ok(std::mem::take(&mut self.cmd_lines))
                        } else if line == "ERROR" {
                            Err(AtError::ModemError(line).into())
                        } else if line.starts_with("+CME ERROR:") {
                            Err(
                                AtError::CmeError(line["+CME ERROR:".len()..].trim().to_string())
                                    .into(),
                            )
                        } else {
                            Err(
                                AtError::CmsError(line["+CMS ERROR:".len()..].trim().to_string())
                                    .into(),
                            )
                        };
                        command_result = Some(result);
                        self.in_command = false;
                        self.saw_echo = false;
                        // Continue processing remaining bytes in chunk
                    }
                    // Otherwise continue collecting lines
                }
            } else if c != '\r' {
                self.line_buf.push(c);
            }
        }

        (command_result, urcs_during_command)
    }

    /// Start a new command
    pub fn start_command(&mut self) {
        self.in_command = true;
        self.cmd_lines.clear();
        self.saw_echo = false;
    }

    /// Check if currently in a command
    pub fn in_command(&self) -> bool {
        self.in_command
    }

    /// Get queued URC indices
    pub fn take_urcs(&mut self) -> Vec<u16> {
        std::mem::take(&mut self.urc_queue)
    }

    /// Get any pending URC from queue
    pub fn pop_urc(&mut self) -> Option<u16> {
        if self.urc_queue.is_empty() {
            None
        } else {
            Some(self.urc_queue.remove(0))
        }
    }
}

/// Parse +CMTI: "<mem>",<index> line, returns index if valid
fn parse_cmti(line: &str) -> Option<u16> {
    // Format: +CMTI: "SM",3 or +CMTI: "ME",300
    let prefix = "+CMTI:";
    if !line.starts_with(prefix) {
        return None;
    }
    let rest = &line[prefix.len()..].trim();
    // Find comma
    let comma_pos = rest.find(',')?;
    // Parse index part (after comma)
    let index_str = rest[comma_pos + 1..].trim();
    index_str.parse::<u16>().ok()
}

/// Blocking command execution with timeout
/// Uses shared AtState so URCs during command are queued for later poll
pub fn send_command<P, C>(
    port: &mut P,
    clock: &C,
    cmd: &str,
    timeout: Duration,
    state: &mut AtState,
) -> Result<Vec<String>>
where
    P: AtPortRaw,
    C: Clock,
{
    let deadline = clock.now() + timeout;

    // Send command
    port.write_raw(format!("{}\r\n", cmd).as_bytes())?;
    state.start_command();

    let mut buf = [0u8; 256];

    loop {
        // Check timeout
        if clock.now() >= deadline {
            bail!(AtError::Timeout);
        }

        // Read available data
        let remaining = deadline.saturating_duration_since(clock.now());
        let read_timeout = remaining.min(Duration::from_millis(10));
        let len = port.read_raw(&mut buf, read_timeout)?;
        if len > 0 {
            let (cmd_result, _urcs) = state.process_bytes(&buf[..len]);
            // URCs during command are queued in state.urc_queue
            if let Some(result) = cmd_result {
                return result;
            }
        }
    }
}

/// Non-blocking poll for URCs
pub fn poll_urcs<P, C>(
    port: &mut P,
    clock: &C,
    state: &mut AtState,
    timeout: Duration,
) -> Result<Option<u16>>
where
    P: AtPortRaw,
    C: Clock,
{
    let deadline = clock.now() + timeout;
    let mut buf = [0u8; 256];

    loop {
        // Check timeout
        if clock.now() >= deadline {
            break;
        }

        // First check if we have queued URCs
        if let Some(urc) = state.pop_urc() {
            return Ok(Some(urc));
        }

        let remaining = deadline.saturating_duration_since(clock.now());
        let read_timeout = remaining.min(Duration::from_millis(10));
        let len = port.read_raw(&mut buf, read_timeout)?;
        if len > 0 {
            let (_, _new_urcs) = state.process_bytes(&buf[..len]);
            // Check queue again after processing
            if let Some(urc) = state.pop_urc() {
                return Ok(Some(urc));
            }
        }
    }

    // Check queue one more time before returning
    Ok(state.pop_urc())
}

/// Mock clock for testing (available for integration tests)
/// Auto-advances by 1ms on each now() call to simulate time passing in loops
#[derive(Clone)]
pub struct MockClock {
    now: std::sync::Arc<std::sync::Mutex<Instant>>,
    auto_advance: std::sync::Arc<std::sync::Mutex<Duration>>,
}

impl MockClock {
    pub fn new(start: Instant) -> Self {
        Self {
            now: std::sync::Arc::new(std::sync::Mutex::new(start)),
            auto_advance: std::sync::Arc::new(std::sync::Mutex::new(Duration::from_millis(1))),
        }
    }

    pub fn advance(&self, dur: Duration) {
        *self.now.lock().unwrap() += dur;
    }

    /// Disable auto-advance (for tests that need fixed time)
    pub fn disable_auto_advance(&self) {
        *self.auto_advance.lock().unwrap() = Duration::ZERO;
    }

    /// Enable auto-advance with custom step
    pub fn enable_auto_advance(&self, step: Duration) {
        *self.auto_advance.lock().unwrap() = step;
    }
}

impl Clock for MockClock {
    fn now(&self) -> Instant {
        let mut now = self.now.lock().unwrap();
        let step = *self.auto_advance.lock().unwrap();
        *now += step;
        *now
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// Mock raw port for testing
    struct MockRawPort {
        /// Bytes to return on each read call
        read_queue: Mutex<Vec<Vec<u8>>>,
        /// Bytes written
        written: Mutex<Vec<u8>>,
    }

    impl MockRawPort {
        fn new() -> Self {
            Self {
                read_queue: Mutex::new(Vec::new()),
                written: Mutex::new(Vec::new()),
            }
        }

        fn push_read(&self, data: Vec<u8>) {
            self.read_queue.lock().unwrap().push(data);
        }

        fn get_written(&self) -> Vec<u8> {
            self.written.lock().unwrap().clone()
        }
    }

    impl AtPortRaw for MockRawPort {
        fn write_raw(&mut self, data: &[u8]) -> Result<()> {
            self.written.lock().unwrap().extend_from_slice(data);
            Ok(())
        }

        fn read_raw(&mut self, buf: &mut [u8], _timeout: Duration) -> Result<usize> {
            let mut queue = self.read_queue.lock().unwrap();
            if queue.is_empty() {
                return Ok(0);
            }
            let data = queue.remove(0);
            let len = data.len().min(buf.len());
            buf[..len].copy_from_slice(&data[..len]);
            // If there's leftover, push it back
            if data.len() > len {
                queue.insert(0, data[len..].to_vec());
            }
            Ok(len)
        }
    }

    #[test]
    fn test_urc_split_across_chunks() {
        // ["\r\n+CMTI: \"SM\"", ",3\r\n"] -> URC index 3 queued for poll
        let mut state = AtState::new();

        // Process first chunk - incomplete line
        let (_, urcs) = state.process_bytes(b"\r\n+CMTI: \"SM\"");
        assert!(urcs.is_empty());

        // Process second chunk - completes line
        let (_, urcs) = state.process_bytes(b",3\r\n");
        assert!(urcs.is_empty()); // Not in command, goes to queue

        // Should be in queue for poll
        assert_eq!(state.take_urcs(), vec![3]);
    }

    #[test]
    fn test_command_ok_with_urc_in_same_chunk() {
        // command AT gets "\r\nOK\r\n+CMTI: \"SM\",4\r\n" in one chunk -> OK, index 4 queued for next poll
        let mut state = AtState::new();
        state.start_command();

        let (result, urcs) = state.process_bytes(b"\r\nOK\r\n+CMTI: \"SM\",4\r\n");
        assert!(result.is_some());
        assert_eq!(result.unwrap().unwrap(), vec!["OK"]);
        // URC during command is queued in state, not returned in urcs
        assert!(urcs.is_empty());
        assert_eq!(state.take_urcs(), vec![4]);
    }

    #[test]
    fn test_cmgr_with_urc_interleaved() {
        // AT+CMGR=3 gets "+CMGR: 0,\"\",21\r\n0791893905004100040C9189291032547600006211017191854002C834\r\n\r\n+CMTI: \"SM\",5\r\n\r\nOK\r\n"
        // -> lines exclude the CMTI and empty lines, index 5 queued
        let mut state = AtState::new();
        state.start_command();

        let chunk = b"+CMGR: 0,\"\",21\r\n0791893905004100040C9189291032547600006211017191854002C834\r\n\r\n+CMTI: \"SM\",5\r\n\r\nOK\r\n";
        let (result, urcs) = state.process_bytes(chunk);
        assert!(result.is_some());
        let lines = result.unwrap().unwrap();
        assert_eq!(lines.len(), 3); // +CMGR line, PDU line, OK (empty lines skipped)
        assert_eq!(lines[0], "+CMGR: 0,\"\",21");
        assert_eq!(
            lines[1],
            "0791893905004100040C9189291032547600006211017191854002C834"
        );
        assert_eq!(lines[2], "OK");
        // URC during command is queued
        assert!(urcs.is_empty());
        assert_eq!(state.take_urcs(), vec![5]);
    }

    #[test]
    fn test_cmgr_byte_by_byte() {
        // Repeat with one byte per read, same result
        let mut state = AtState::new();
        state.start_command();

        let chunk = b"+CMGR: 0,\"\",21\r\n0791893905004100040C9189291032547600006211017191854002C834\r\n\r\n+CMTI: \"SM\",5\r\n\r\nOK\r\n";
        let mut result = None;
        let mut all_urcs = Vec::new();

        for &byte in chunk {
            let (r, urcs) = state.process_bytes(&[byte]);
            all_urcs.extend(urcs);
            if r.is_some() {
                result = r;
            }
        }

        assert!(result.is_some());
        let lines = result.unwrap().unwrap();
        assert_eq!(lines.len(), 3); // +CMGR line, PDU line, OK (empty lines skipped)
        assert_eq!(lines[2], "OK");
        // URC during command is queued in state
        assert!(all_urcs.is_empty());
        assert_eq!(state.take_urcs(), vec![5]);
    }

    #[test]
    fn test_cms_error() {
        // "+CMS ERROR: 321\r\n" -> immediate Err with the text
        let mut state = AtState::new();
        state.start_command();

        let (result, _) = state.process_bytes(b"+CMS ERROR: 321\r\n");
        assert!(result.is_some());
        assert!(result.as_ref().unwrap().is_err());
        let err = result.unwrap().unwrap_err();
        assert!(err.to_string().contains("321"));
    }

    #[test]
    fn test_cme_error() {
        // "+CME ERROR: SIM not inserted\r\n" -> Err
        let mut state = AtState::new();
        state.start_command();

        let (result, _) = state.process_bytes(b"+CME ERROR: SIM not inserted\r\n");
        assert!(result.is_some());
        assert!(result.as_ref().unwrap().is_err());
        let err = result.unwrap().unwrap_err();
        assert!(err.to_string().contains("SIM not inserted"));
    }

    #[test]
    fn test_error() {
        // "ERROR\r\n" -> Err
        let mut state = AtState::new();
        state.start_command();

        let (result, _) = state.process_bytes(b"ERROR\r\n");
        assert!(result.is_some());
        assert!(result.as_ref().unwrap().is_err());
    }

    #[test]
    fn test_echo_handling() {
        // "AT+CMGF=0\r\r\nOK\r\n" -> Ok
        let mut state = AtState::new();
        state.start_command();

        let (result, _) = state.process_bytes(b"AT+CMGF=0\r\r\nOK\r\n");
        assert!(result.is_some());
        let lines = result.unwrap().unwrap();
        assert_eq!(lines, vec!["OK"]);
    }

    #[test]
    fn test_cmti_me_300() {
        // "\r\n+CMTI: \"ME\",300\r\n" -> index 300 queued for poll
        let mut state = AtState::new();
        let (_, urcs) = state.process_bytes(b"\r\n+CMTI: \"ME\",300\r\n");
        assert!(urcs.is_empty()); // Not in command, goes to queue
        assert_eq!(state.take_urcs(), vec![300]);
    }

    #[test]
    fn test_invalid_cmti_ignored() {
        // "+CMTI: \"SM\",abc\r\n", "RING\r\n", "+CLIP: \"123\",129\r\n" -> ignored
        let mut state = AtState::new();
        let (_, urcs) = state.process_bytes(b"+CMTI: \"SM\",abc\r\n");
        assert!(urcs.is_empty());
        assert!(state.take_urcs().is_empty());

        let (_, urcs) = state.process_bytes(b"RING\r\n");
        assert!(urcs.is_empty());

        let (_, urcs) = state.process_bytes(b"+CLIP: \"123\",129\r\n");
        assert!(urcs.is_empty());
    }

    #[test]
    fn test_poll_mid_line() {
        // poll ends mid-line with "+CMTI: \"SM\"," and a later poll gives "7\r\n" -> index 7
        let mut state = AtState::new();

        // First poll - incomplete line
        let (_, urcs) = state.process_bytes(b"+CMTI: \"SM\",");
        assert!(urcs.is_empty());
        assert!(state.take_urcs().is_empty());

        // Second poll - completes line
        let (_, urcs) = state.process_bytes(b"7\r\n");
        assert!(urcs.is_empty());
        assert_eq!(state.take_urcs(), vec![7]);
    }

    #[test]
    fn test_command_timeout() {
        // no terminator -> Err after the timeout
        // Use SystemClock so time actually advances
        let port = MockRawPort::new();
        let clock = SystemClock;
        let mut port = port;
        let mut state = AtState::new();

        let result = send_command(
            &mut port,
            &clock,
            "AT",
            Duration::from_millis(50),
            &mut state,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timeout"));
    }
}
