pub fn decode_ucs2(bytes: &[u8]) -> String {
    let u16s: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&u16s)
}

pub fn decode_gsm7(bytes: &[u8], padding_bits: u8, septet_count: usize) -> String {
    // Handle UDL = 0 (empty message)
    if septet_count == 0 {
        return String::new();
    }

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
