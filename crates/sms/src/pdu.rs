use anyhow::Result;

use crate::codec::{decode_gsm7, decode_ucs2};
use crate::sender::decode_sender;
use crate::Sms;

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

    let first_octet = bytes[pos];
    // Only accept SMS-DELIVER (MTI bits 1:0 == 00)
    if (first_octet & 0x03) != 0x00 {
        anyhow::bail!("Not an SMS-DELIVER PDU");
    }
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

    let sender = decode_sender(sender_type, sender_len, sender_raw);

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

    // Validate user data length
    if is_ucs2 {
        // UCS2: udl is number of octets (bytes), must be even (2 bytes per char)
        if udl % 2 != 0 {
            anyhow::bail!("UCS2 UDL must be even");
        }
        if user_data.len() < udl {
            anyhow::bail!("UCS2 user data truncated");
        }
    } else {
        // GSM7: udl is number of septets
        let required_bytes = (udl * 7 + 7) / 8;
        if user_data.len() < required_bytes {
            anyhow::bail!("GSM7 user data truncated");
        }
    }

    // Check UDHI (User Data Header Indicator) - bit 6 of first_octet
    let has_udh = (first_octet & 0x40) != 0;
    let message = if has_udh {
        if user_data.is_empty() {
            anyhow::bail!("UDH indicated but no user data");
        }
        let udhl = user_data[0] as usize;
        if user_data.len() < 1 + udhl {
            anyhow::bail!("UDH length exceeds user data");
        }
        // Skip UDH (1 byte UDHL + udhl bytes UDH), decode the rest
        let message_data = &user_data[1 + udhl..];
        if is_ucs2 {
            decode_ucs2(message_data)
        } else {
            // For GSM7 with UDH, the UDL includes UDH septets.
            // We need to recalculate septet count for message only.
            // This is complex; for now just decode with remaining data
            decode_gsm7(message_data, 0, udl)
        }
    } else {
        if is_ucs2 {
            decode_ucs2(user_data)
        } else {
            decode_gsm7(user_data, 0, udl)
        }
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

pub fn hex_to_bytes(hex: &str) -> Result<Vec<u8>> {
    let hex = hex.trim();
    if hex.is_empty() {
        anyhow::bail!("Hex string empty");
    }
    if hex.len() % 2 != 0 {
        anyhow::bail!("Hex string odd length");
    }
    // Reject non-ASCII
    if !hex.is_ascii() {
        anyhow::bail!("Hex string contains non-ASCII");
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for i in (0..hex.len()).step_by(2) {
        let byte_str = &hex[i..i + 2];
        // Reject any non-hex character (including '+')
        if !byte_str.chars().all(|c| c.is_ascii_hexdigit()) {
            anyhow::bail!("Hex string contains invalid character");
        }
        let byte = u8::from_str_radix(byte_str, 16).map_err(|e| anyhow::anyhow!(e))?;
        bytes.push(byte);
    }
    Ok(bytes)
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
