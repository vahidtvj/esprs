pub mod codec;
pub mod pdu;
pub mod sender;
pub mod service;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sms {
    pub sender: String,
    pub date: String,
    pub message: String,
}

pub use pdu::{parse_pdu, parse_pdu_response};
pub use service::{AtPort, SmsService};

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

    #[test]
    fn test_err_hex_mti() {
        use crate::test_vectors::err_hex_mti::CASES;
        for case in CASES {
            let result = parse_pdu(case);
            assert!(result.is_err(), "Expected Err for case: {}", case);
        }
    }

    #[test]
    fn test_err_length() {
        use crate::test_vectors::err_length::CASES;
        for case in CASES {
            let result = parse_pdu(case);
            assert!(result.is_err(), "Expected Err for case: {}", case);
        }
    }

    #[test]
    fn test_err_udh() {
        use crate::test_vectors::err_udh::CASES;
        for case in CASES {
            let result = parse_pdu(case);
            assert!(result.is_err(), "Expected Err for case: {}", case);
        }
    }

    #[test]
    fn test_ok_cases() {
        use crate::test_vectors::ok_cases::*;
        let cases = [
            (GSM7_HI, "GSM7_HI"),
            (GSM7_HELLO_WORLD, "GSM7_HELLO_WORLD"),
            (GSM7_ABCDEFG, "GSM7_ABCDEFG"),
            (GSM7_ABCDEFGH, "GSM7_ABCDEFGH"),
            (GSM7_SPECIAL, "GSM7_SPECIAL"),
            (GSM7_ESCAPE, "GSM7_ESCAPE"),
            (GSM7_160, "GSM7_160"),
            (GSM7_NEWLINE, "GSM7_NEWLINE"),
            (NATIONAL_A1, "NATIONAL_A1"),
            (UNKNOWN_81, "UNKNOWN_81"),
            (ODD_SENDER, "ODD_SENDER"),
            (SHORTCODE, "SHORTCODE"),
            (ALNUM_GOOGLE, "ALNUM_GOOGLE"),
            (ALNUM_MYBANK, "ALNUM_MYBANK"),
            (UCS2_SALAM, "UCS2_SALAM"),
            (UCS2_MIXED, "UCS2_MIXED"),
            (UCS2_EMOJI, "UCS2_EMOJI"),
            (ALNUM_SHOP, "ALNUM_SHOP"),
            (TZ_NEGATIVE, "TZ_NEGATIVE"),
        ];
        for (case, name) in cases {
            let (pdu, expected_sender, expected_message, expected_date) = case;
            let result = parse_pdu(pdu);
            let sms = result.unwrap_or_else(|e| panic!("Failed to parse {}: {}", name, e));
            assert_eq!(sms.sender, expected_sender, "Sender mismatch for {}", name);
            assert_eq!(
                sms.message, expected_message,
                "Message mismatch for {}",
                name
            );
            assert_eq!(sms.date, expected_date, "Date mismatch for {}", name);
        }
    }

    #[test]
    fn test_gsm7_empty() {
        use crate::test_vectors::ok_cases::GSM7_EMPTY;
        let (pdu, expected_sender, expected_message, expected_date) = GSM7_EMPTY;
        let result = parse_pdu(pdu);
        let sms = result.unwrap_or_else(|e| panic!("Failed to parse GSM7_EMPTY: {}", e));
        assert_eq!(sms.sender, expected_sender);
        assert_eq!(sms.message, expected_message);
        assert_eq!(sms.date, expected_date);
    }

    #[test]
    fn test_dcs_compressed() {
        // DCS 0x20 = compressed GSM-7 (general group, bit 5 set, alphabet 00)
        // Build a minimal PDU with DCS=0x20
        // SMSC: 07 91 89 39 05 00 41 00
        // First octet: 04 (SMS-DELIVER)
        // Sender len: 0C, type: 91, address: 89 29 10 32 54 76
        // PID: 00, DCS: 20 (compressed)
        // Timestamp: 62 11 01 71 91 85 40
        // UDL: 02, User data: C8 34 (dummy)
        let pdu = "0791893905004100040C9189291032547600206211017191854002C834";
        let result = parse_pdu(pdu);
        assert!(result.is_err(), "Compressed DCS should return Err");
    }

    #[test]
    fn test_dcs_8bit() {
        // DCS 0x04 = 8-bit data (general group, alphabet 01)
        // Build a PDU with DCS=0x04 and 8-bit user data
        // SMSC: 07 91 89 39 05 00 41 00
        // First octet: 04 (SMS-DELIVER)
        // Sender len: 0C, type: 91, address: 89 29 10 32 54 76
        // PID: 00, DCS: 04 (8-bit data)
        // Timestamp: 62 11 01 71 91 85 40
        // UDL: 05 (5 bytes), User data: 48 65 6C 6C 6F ("Hello" in ASCII/UTF-8)
        let pdu = "0791893905004100040C918929103254760004621101719185400548656C6C6F";
        let result = parse_pdu(pdu);
        let sms = result.unwrap_or_else(|e| panic!("Failed to parse 8-bit DCS: {}", e));
        assert_eq!(sms.sender, "+989201234567");
        assert_eq!(sms.message, "Hello");
        assert_eq!(sms.date, "2026/11/10 17:19:58 +01:00");
    }

    #[test]
    fn test_hex_to_bytes_errors() {
        // Empty string
        assert!(pdu::hex_to_bytes("").is_err());
        // Odd length
        assert!(pdu::hex_to_bytes("0").is_err());
        assert!(pdu::hex_to_bytes("abc").is_err());
        // Non-ASCII
        assert!(pdu::hex_to_bytes("é1").is_err());
        // Invalid hex chars including +
        assert!(pdu::hex_to_bytes("ZZ").is_err());
        assert!(pdu::hex_to_bytes("+1").is_err());
        assert!(pdu::hex_to_bytes("0G").is_err());
        // Valid cases
        assert!(pdu::hex_to_bytes("00").is_ok());
        assert!(pdu::hex_to_bytes("FF").is_ok());
        assert!(pdu::hex_to_bytes("0123456789ABCDEF").is_ok());
    }
}
