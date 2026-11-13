use crate::codec::decode_gsm7;

pub fn decode_sender(sender_type: u8, sender_len: usize, sender_raw: &[u8]) -> String {
    // Extract TON (Type of Number) from sender_type (bits 6:4)
    let ton = (sender_type >> 4) & 0x07;

    if ton == 5 {
        // Alphanumeric (TON=5)
        let septet_count = (sender_len * 4) / 7;
        decode_gsm7(sender_raw, 0, septet_count)
    } else {
        let mut s = String::new();
        // Prefix '+' only for TON 1 (international)
        if ton == 1 {
            s.push('+');
        }
        for &b in sender_raw {
            let low = b & 0x0F;
            let high = b >> 4;
            // Low nibble first (semi-octet order)
            if low != 0x0F {
                s.push(decode_nibble(low));
            }
            if high != 0x0F {
                s.push(decode_nibble(high));
            }
        }
        // Truncate to sender_len digits (excluding '+' prefix)
        let digit_part = if ton == 1 { &s[1..] } else { &s[..] };
        if digit_part.len() > sender_len {
            let prefix = if ton == 1 { "+" } else { "" };
            s = format!("{}{}", prefix, &digit_part[..sender_len]);
        }
        s
    }
}

fn decode_nibble(n: u8) -> char {
    match n {
        0x0..=0x9 => (b'0' + n) as char,
        0xA => '*',
        0xB => '#',
        0xC => 'a',
        0xD => 'b',
        0xE => 'c',
        _ => '_', // filler (0xF) should not reach here
    }
}
