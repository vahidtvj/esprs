use anyhow::Result;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sms {
    pub sender: String,
    pub date: String,
    pub message: String,
}

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

pub fn parse_pdu_response(lines: &[String]) -> Result<Sms> {
    let cmgr_pos = lines
        .iter()
        .position(|l| l.contains("+CMGR:"))
        .ok_or_else(|| anyhow::anyhow!("No +CMGR header found"))?;

    let pdu_hex = lines
        .get(cmgr_pos + 1)
        .ok_or_else(|| anyhow::anyhow!("No PDU line found"))?
        .trim();

    parse_pdu(pdu_hex)
}

pub fn parse_pdu(pdu_hex: &str) -> Result<Sms> {
    let bytes = hex_to_bytes(pdu_hex)?;
    if bytes.len() < 3 {
        anyhow::bail!("PDU too short");
    }

    let smsc_len = bytes[0] as usize;
    let mut pos = 1 + smsc_len;

    if pos >= bytes.len() {
        anyhow::bail!("Invalid SMSC length");
    }

    let _first_octet = bytes[pos];
    pos += 1;

    if pos >= bytes.len() {
        anyhow::bail!("Invalid sender length position");
    }

    let sender_len = bytes[pos] as usize;
    pos += 1;

    if pos >= bytes.len() {
        anyhow::bail!("Invalid sender type position");
    }

    let sender_type = bytes[pos];
    pos += 1;

    let sender_nibbles_len = (sender_len + 1) / 2;
    if pos + sender_nibbles_len > bytes.len() {
        anyhow::bail!("Sender address out of bounds");
    }

    let sender_raw = &bytes[pos..pos + sender_nibbles_len];
    pos += sender_nibbles_len;

    let sender = if (sender_type >> 4) == 0b1101 {
        let septet_count = (sender_len * 4) / 7;
        decode_gsm7(sender_raw, 0, septet_count)
    } else {
        let mut s = String::from("+");
        for &b in sender_raw {
            let low = b & 0x0F;
            let high = b >> 4;
            if low != 0x0F {
                s.push((b'0' + low) as char);
            }
            if high != 0x0F {
                s.push((b'0' + high) as char);
            }
        }
        if s.len() > 1 + sender_len {
            s.truncate(1 + sender_len);
        }
        s
    };

    if pos + 2 > bytes.len() {
        anyhow::bail!("Invalid PID/DCS position");
    }

    let _pid = bytes[pos];
    let dcs = bytes[pos + 1];
    pos += 2;

    if pos + 7 > bytes.len() {
        anyhow::bail!("Invalid timestamp position");
    }

    let ts = &bytes[pos..pos + 7];
    pos += 7;

    let date = parse_date(ts);

    if pos >= bytes.len() {
        anyhow::bail!("Invalid user data length position");
    }

    let udl = bytes[pos] as usize;
    pos += 1;

    let user_data = &bytes[pos..];

    let is_ucs2 = (dcs & 0x0C) == 0x08;

    let message = if is_ucs2 {
        decode_ucs2(user_data)
    } else {
        decode_gsm7(user_data, 0, udl)
    };

    Ok(Sms {
        sender,
        date,
        message,
    })
}

fn parse_date(ts: &[u8]) -> String {
    let swap = |b: u8| -> (u8, u8) { (b & 0x0F, b >> 4) };
    let (y1, y2) = swap(ts[0]);
    let (m1, m2) = swap(ts[1]);
    let (d1, d2) = swap(ts[2]);
    let (h1, h2) = swap(ts[3]);
    let (min1, min2) = swap(ts[4]);
    let (s1, s2) = swap(ts[5]);

    format!(
        "20{}{}/{}{}/{}{} {}{}:{}{}:{}{}",
        y1, y2, m1, m2, d1, d2, h1, h2, min1, min2, s1, s2
    )
}

fn hex_to_bytes(hex: &str) -> Result<Vec<u8>> {
    let hex = hex.trim();
    if hex.len() % 2 != 0 {
        anyhow::bail!("Hex string odd length");
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|e| anyhow::anyhow!(e)))
        .collect()
}

fn decode_ucs2(bytes: &[u8]) -> String {
    let u16s: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&u16s)
}

fn decode_gsm7(bytes: &[u8], padding_bits: u8, septet_count: usize) -> String {
    let mut message = String::new();
    let mut temp = 0u8;
    let mut j = (7 - padding_bits) % 7;
    let mut count = 0;
    let mut escape = false;

    for (i, &o) in bytes.iter().enumerate() {
        let s = ((o << j) & 127) | temp;
        temp = o >> (7 - j);

        if padding_bits == 0 || i != 0 {
            if escape {
                message.push_str(parse_escape(s));
                escape = false;
            } else if s == 0x1B {
                escape = true;
            } else {
                message.push_str(gsm7_lookup(s));
            }
            count += 1;
            if count == septet_count {
                break;
            }
        }

        j += 1;
        if j == 7 {
            if escape {
                message.push_str(parse_escape(temp));
                escape = false;
            } else if temp == 0x1B {
                escape = true;
            } else {
                message.push_str(gsm7_lookup(temp));
            }
            temp = 0;
            j = 0;
            count += 1;
            if count == septet_count {
                break;
            }
        }
    }

    message
}

fn parse_escape(c: u8) -> &'static str {
    match c {
        0x0A => "",
        0x14 => "^",
        0x28 => "{",
        0x29 => "}",
        0x2F => "\\",
        0x3C => "[",
        0x3D => "~",
        0x3E => "]",
        0x40 => "|",
        0x65 => "€",
        _ => "",
    }
}

fn gsm7_lookup(c: u8) -> &'static str {
    const GSM7_TABLE: &[&str] = &[
        "@", "£", "$", "¥", "è", "é", "ù", "ì", "ò", "Ç", "\n", "Ø", "ø", "\r", "Å", "å", "Δ", "_",
        "Φ", "Γ", "Λ", "Ω", "Π", "Ψ", "Σ", "Θ", "Ξ", "", "Æ", "æ", "ß", "É", " ", "!", "\"", "#",
        "¤", "%", "&", "'", "(", ")", "*", "+", ",", "-", ".", "/", "0", "1", "2", "3", "4", "5",
        "6", "7", "8", "9", ":", ";", "<", "=", ">", "?", "¡", "A", "B", "C", "D", "E", "F", "G",
        "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R", "S", "T", "U", "V", "W", "X", "Y",
        "Z", "Ä", "Ö", "Ñ", "Ü", "§", "¿", "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k",
        "l", "m", "n", "o", "p", "q", "r", "s", "t", "u", "v", "w", "x", "y", "z", "ä", "ö", "ñ",
        "ü", "à",
    ];
    GSM7_TABLE.get(c as usize).copied().unwrap_or("")
}

#[cfg(test)]
mod test_vectors;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_real_pdu_gsm7() {
        // PDU from modem: "Hi"
        let lines = vec![
            "AT+CMGR=9".to_string(),
            "+CMGR: 0,\"\",21".to_string(),
            "0791893905004100040C9189291032547600006211017191854102C834".to_string(),
            "OK".to_string(),
        ];
        let sms = parse_pdu_response(&lines).unwrap();
        assert_eq!(sms.sender, "+989201234567");
        assert_eq!(sms.message, "Hi");
    }

    #[test]
    fn test_parse_real_pdu_ucs2() {
        // PDU from modem: "سلام" (UCS2)
        let lines = vec![
            "AT+CMGR=9".to_string(),
            "+CMGR: 0,\"\",27".to_string(),
            "0791893905004100040C91892910325476000862110171125241080633064406270645".to_string(),
            "OK".to_string(),
        ];
        let sms = parse_pdu_response(&lines).unwrap();
        assert_eq!(sms.sender, "+989201234567");
        assert_eq!(sms.message, "\u{0633}\u{0644}\u{0627}\u{0645}");
    }
}
