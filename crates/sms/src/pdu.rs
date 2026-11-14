use anyhow::Result;

use crate::codec::{decode_gsm7, decode_ucs2};
use crate::sender::decode_sender;
use crate::{Concat, ParseResult, Sms};

pub fn parse_pdu(pdu_hex: &str) -> Result<Sms> {
    let result = parse_pdu_with_udh(pdu_hex)?;
    Ok(result.sms)
}

pub fn parse_pdu_with_udh(pdu_hex: &str) -> Result<ParseResult> {
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

    // Determine encoding from DCS
    let (is_ucs2, is_8bit, is_compressed) = decode_dcs(dcs);
    if is_compressed {
        anyhow::bail!("Compressed DCS not supported");
    }

    // Validate user data length (initial check - UDH will be validated separately if present)
    if is_ucs2 {
        // UCS2: udl is number of octets (bytes)
        // If UDH is present, UDL includes UDH bytes, so we only check minimum length here
        if user_data.len() < udl {
            anyhow::bail!("UCS2 user data truncated");
        }
    } else if is_8bit {
        // 8-bit data: udl is number of bytes
        if user_data.len() < udl {
            anyhow::bail!("8-bit user data truncated");
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
    let (message, concat) = if has_udh {
        parse_udh(user_data, udl, is_ucs2, is_8bit)?
    } else {
        // For non-UDH, validate UCS2 UDL is even
        if is_ucs2 && udl % 2 != 0 {
            anyhow::bail!("UCS2 UDL must be even");
        }
        let message = if is_ucs2 {
            decode_ucs2(user_data)
        } else if is_8bit {
            // 8-bit data: decode as lossy UTF-8, bounded by UDL bytes
            let data = &user_data[..udl.min(user_data.len())];
            String::from_utf8_lossy(data).into_owned()
        } else {
            decode_gsm7(user_data, 0, udl)
        };
        (message, None)
    };

    Ok(ParseResult {
        sms: Sms {
            sender,
            date,
            message,
        },
        concat,
    })
}

fn parse_udh(
    user_data: &[u8],
    udl: usize,
    is_ucs2: bool,
    is_8bit: bool,
) -> Result<(String, Option<Concat>)> {
    if user_data.is_empty() {
        anyhow::bail!("UDH indicated but no user data");
    }
    let udhl = user_data[0] as usize;
    if user_data.len() < 1 + udhl {
        anyhow::bail!("UDH length exceeds user data");
    }

    let udh = &user_data[1..1 + udhl];
    let concat = parse_concat_ie(udh)?;

    // Calculate message data after UDH
    let message_data = &user_data[1 + udhl..];

    let message = if is_ucs2 {
        // UCS2: UDL is bytes, subtract UDHL+1 bytes
        let expected_msg_bytes = udl.saturating_sub(udhl + 1);
        if message_data.len() < expected_msg_bytes {
            anyhow::bail!("UCS2 message data truncated after UDH");
        }
        // UCS2 message part must be even number of bytes
        if expected_msg_bytes % 2 != 0 {
            anyhow::bail!("UCS2 message part after UDH must be even");
        }
        let msg_bytes = &message_data[..expected_msg_bytes.min(message_data.len())];
        decode_ucs2(msg_bytes)
    } else if is_8bit {
        // 8-bit: UDL is bytes, subtract UDHL+1 bytes
        let expected_msg_bytes = udl.saturating_sub(udhl + 1);
        let msg_bytes = &message_data[..expected_msg_bytes.min(message_data.len())];
        String::from_utf8_lossy(msg_bytes).into_owned()
    } else {
        // GSM-7: UDL is septets
        // padding bits = (7 - (8*(UDHL+1)) % 7) % 7
        let udh_bytes = udhl + 1; // UDHL + 1 byte for UDHL itself
        let udh_bits = udh_bytes * 8;
        let padding_bits = (7 - (udh_bits % 7)) % 7;
        let udh_septets = (udh_bits + 6) / 7; // ceil(udh_bits / 7)
        let msg_septets = udl.saturating_sub(udh_septets);
        decode_gsm7(message_data, padding_bits as u8, msg_septets)
    };

    Ok((message, concat))
}

fn parse_concat_ie(udh: &[u8]) -> Result<Option<Concat>> {
    let mut i = 0;
    while i < udh.len() {
        if i + 1 >= udh.len() {
            anyhow::bail!("UDH truncated: missing IE length");
        }
        let ie_id = udh[i];
        let ie_len = udh[i + 1] as usize;
        if i + 2 + ie_len > udh.len() {
            anyhow::bail!("UDH truncated: IE data exceeds UDH length");
        }
        let ie_data = &udh[i + 2..i + 2 + ie_len];

        match ie_id {
            0x00 => {
                // 8-bit reference number: ref(1), total(1), seq(1)
                if ie_len < 3 {
                    anyhow::bail!("Concat IE 0x00 too short");
                }
                let ref_num = ie_data[0] as u16;
                let total = ie_data[1];
                let seq = ie_data[2];
                if total == 0 {
                    anyhow::bail!("Concat total must be >= 1");
                }
                if seq == 0 || seq > total {
                    anyhow::bail!("Concat seq must be 1..total");
                }
                return Ok(Some(Concat {
                    ref_num,
                    total,
                    seq,
                }));
            }
            0x08 => {
                // 16-bit reference number: ref(2), total(1), seq(1)
                if ie_len < 4 {
                    anyhow::bail!("Concat IE 0x08 too short");
                }
                let ref_num = u16::from_be_bytes([ie_data[0], ie_data[1]]);
                let total = ie_data[2];
                let seq = ie_data[3];
                if total == 0 {
                    anyhow::bail!("Concat total must be >= 1");
                }
                if seq == 0 || seq > total {
                    anyhow::bail!("Concat seq must be 1..total");
                }
                return Ok(Some(Concat {
                    ref_num,
                    total,
                    seq,
                }));
            }
            _ => {
                // Ignore other IEs
            }
        }

        i += 2 + ie_len;
    }
    Ok(None)
}

fn parse_date(ts: &[u8]) -> String {
    let swap = |b: u8| -> (u8, u8) { (b & 0x0F, b >> 4) };
    let (y1, y2) = swap(ts[0]);
    let (m1, m2) = swap(ts[1]);
    let (d1, d2) = swap(ts[2]);
    let (h1, h2) = swap(ts[3]);
    let (min1, min2) = swap(ts[4]);
    let (s1, s2) = swap(ts[5]);

    // Parse timezone byte (ts[6])
    // Low nibble: bit 3 = sign (0=+, 1=-), bits 0-2 = tens of quarter-hours
    // High nibble: bits 4-7 = units of quarter-hours
    let tz_byte = ts[6];
    let low_nibble = tz_byte & 0x0F;
    let high_nibble = tz_byte >> 4;
    let sign = if (low_nibble & 0x08) != 0 { '-' } else { '+' };
    let tens = (low_nibble & 0x07) as i8;
    let units = high_nibble as i8;
    let quarter_hours = tens * 10 + units;
    let hours = quarter_hours / 4;
    let minutes = (quarter_hours % 4) * 15;

    format!(
        "20{}{}/{}{}/{}{} {}{}:{}{}:{}{} {}{:02}:{:02}",
        y1,
        y2,
        m1,
        m2,
        d1,
        d2,
        h1,
        h2,
        min1,
        min2,
        s1,
        s2,
        sign,
        hours.abs(),
        minutes
    )
}

// Decode DCS (Data Coding Scheme) byte according to 3GPP TS 23.038
// Returns (is_ucs2, is_8bit, is_compressed)
fn decode_dcs(dcs: u8) -> (bool, bool, bool) {
    // Group 0xF0-0xFF: bit 2 selects alphabet (0=GSM-7, 1=8-bit)
    if (dcs & 0xF0) == 0xF0 {
        let is_8bit = (dcs & 0x04) != 0;
        return (false, is_8bit, false);
    }

    // General group (bit 7 = 0): bits 3:2 = alphabet
    // Bit 5 = compressed
    let is_compressed = (dcs & 0x20) != 0;
    let alphabet = (dcs >> 2) & 0x03;

    match alphabet {
        0b00 => (false, false, is_compressed), // GSM-7
        0b01 => (false, true, is_compressed),  // 8-bit data
        0b10 => (true, false, is_compressed),  // UCS2
        0b11 => (false, false, is_compressed), // Reserved, treat as GSM-7
        _ => unreachable!(),
    }
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
