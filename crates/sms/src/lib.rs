pub mod at_port;
pub mod codec;
pub mod pdu;
pub mod reassembler;
pub mod sender;
pub mod service;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sms {
    pub sender: String,
    pub date: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Concat {
    pub ref_num: u16,
    pub total: u8,
    pub seq: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseResult {
    pub sms: Sms,
    pub concat: Option<Concat>,
}

pub use at_port::{
    poll_urcs, send_command, AtError, AtPortRaw, AtState, Clock, MockClock, SystemClock,
};
pub use pdu::{parse_pdu, parse_pdu_response, parse_pdu_with_udh, parse_pdu_with_udh_response};
pub use reassembler::{CompletedMessage, Reassembler, ReassemblerConfig};
pub use service::SmsService;

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

    #[test]
    fn test_concat_cases() {
        use crate::test_vectors::concat_cases::*;
        let cases = [
            (GSM7_2A_P1, "GSM7_2A_P1"),
            (GSM7_2A_P2, "GSM7_2A_P2"),
            (UCS2_07_P1, "UCS2_07_P1"),
            (UCS2_07_P2, "UCS2_07_P2"),
            (UCS2_1234_P1, "UCS2_1234_P1"),
            (UCS2_1234_P2, "UCS2_1234_P2"),
            (GSM7_10_P1, "GSM7_10_P1"),
            (GSM7_10_P2, "GSM7_10_P2"),
            (GSM7_10_P3, "GSM7_10_P3"),
            (OTHER_2A_P1, "OTHER_2A_P1"),
        ];
        for (case, name) in cases {
            let (
                pdu,
                expected_sender,
                expected_message,
                expected_ref,
                expected_total,
                expected_seq,
            ) = case;
            let result = parse_pdu_with_udh(pdu)
                .unwrap_or_else(|e| panic!("Failed to parse {}: {}", name, e));
            assert_eq!(
                result.sms.sender, expected_sender,
                "Sender mismatch for {}",
                name
            );
            assert_eq!(
                result.sms.message, expected_message,
                "Message mismatch for {}",
                name
            );
            let concat = result
                .concat
                .unwrap_or_else(|| panic!("Expected concat info for {}", name));
            assert_eq!(concat.ref_num, expected_ref, "Ref mismatch for {}", name);
            assert_eq!(concat.total, expected_total, "Total mismatch for {}", name);
            assert_eq!(concat.seq, expected_seq, "Seq mismatch for {}", name);
        }
    }

    #[test]
    fn test_err_udh_cases() {
        use crate::test_vectors::err_udh::CASES;
        for case in CASES {
            let result = parse_pdu_with_udh(case);
            assert!(result.is_err(), "Expected Err for case: {}", case);
        }
    }

    #[test]
    fn test_concat_invalid_total_zero() {
        // Build a PDU based on GSM7_2A_P1 but with total=0
        // Original: GSM7_2A_P1 has concat IE 0x00 0x03 0x2A 0x02 0x01 (ref=0x2A, total=2, seq=1)
        // Modify to total=0: 0x00 0x03 0x2A 0x00 0x01
        let pdu = "0791893905004100440C91892910325476000062110171918540170500032A000182C16030180C0683416499CD7E8300";
        let result = parse_pdu_with_udh(pdu);
        assert!(result.is_err(), "Expected Err for total=0");
    }

    #[test]
    fn test_concat_invalid_seq_zero() {
        // Build a PDU based on GSM7_2A_P1 but with seq=0
        // Original: GSM7_2A_P1 has concat IE 0x00 0x03 0x2A 0x02 0x01 (ref=0x2A, total=2, seq=1)
        // Modify to seq=0: 0x00 0x03 0x2A 0x02 0x00
        let pdu = "0791893905004100440C91892910325476000062110171918540170500032A020082C16030180C0683416499CD7E8300";
        let result = parse_pdu_with_udh(pdu);
        assert!(result.is_err(), "Expected Err for seq=0");
    }

    #[test]
    fn test_concat_invalid_seq_gt_total() {
        // Build a PDU based on GSM7_2A_P1 but with seq=3 (total=2)
        // Original: GSM7_2A_P1 has concat IE 0x00 0x03 0x2A 0x02 0x01 (ref=0x2A, total=2, seq=1)
        // Modify to seq=3: 0x00 0x03 0x2A 0x02 0x03
        let pdu = "0791893905004100440C91892910325476000062110171918540170500032A020382C16030180C0683416499CD7E8300";
        let result = parse_pdu_with_udh(pdu);
        assert!(result.is_err(), "Expected Err for seq > total");
    }

    #[test]
    fn test_concat_unknown_ie_before_concat() {
        // Build a PDU with an unknown IE (0x01) before the concat IE (0x00)
        // Using UCS2 to avoid GSM-7 padding complexity
        // Based on UCS2_07_P1 but with an unknown IE before the concat IE
        // Original UCS2_07_P1: 0791893905004100440C918929103254760008621101719185401005000307020106330644062706450020
        // Breakdown:
        // UDL=0x10 (16 bytes)
        // UDH: 05 00 03 07 02 01 (UDHL=5, concat IE 0x00 ref=7 total=2 seq=1)
        // Message: 06330644062706450020 (10 bytes = "سلام ")
        //
        // With unknown IE 0x01 len=2 before concat:
        // UDH: 09 01 02 AA BB 00 03 07 02 01 (UDHL=9, unknown IE + concat IE)
        // Extra 4 bytes UDH = UDL increases by 4
        // New UDL = 16 + 4 = 20 (0x14)
        // UDH at start of user_data: 09 01 02 AA BB 00 03 07 02 01
        // Message same: 06330644062706450020

        let pdu = "0791893905004100440C9189291032547600086211017191854014090102AABB000307020106330644062706450020";
        let result = parse_pdu_with_udh(pdu).unwrap_or_else(|e| panic!("Failed to parse: {}", e));
        // Just verify concat info is extracted correctly; message may vary
        let concat = result.concat.unwrap();
        assert_eq!(concat.ref_num, 0x07);
        assert_eq!(concat.total, 2);
        assert_eq!(concat.seq, 1);
    }

    #[test]
    fn test_parse_pdu_backward_compat() {
        // Ensure existing parse_pdu still works and returns Sms
        let pdu = "0791893905004100040C9189291032547600006211017191854102C834";
        let sms = parse_pdu(pdu).unwrap();
        assert_eq!(sms.sender, "+989201234567");
        assert_eq!(sms.message, "Hi");
    }

    #[test]
    fn test_parse_pdu_with_udh_no_udhi() {
        // Test parse_pdu_with_udh on non-UDH PDU
        let pdu = "0791893905004100040C9189291032547600006211017191854102C834";
        let result = parse_pdu_with_udh(pdu).unwrap();
        assert_eq!(result.sms.sender, "+989201234567");
        assert_eq!(result.sms.message, "Hi");
        assert!(result.concat.is_none());
    }

    #[test]
    fn test_ucs2_decoding_is_bounded_by_udl() {
        use crate::test_vectors::ok_cases::UCS2_SALAM;
        let pdu = format!("{}0041", UCS2_SALAM.0);
        let sms = parse_pdu(&pdu).unwrap();
        assert_eq!(sms.message, UCS2_SALAM.2);
    }

    #[test]
    fn test_udh_larger_than_udl_is_rejected() {
        let ucs2 = "0791893905004100440C918929103254760008621101719185400505000307020106330644062706450020";
        assert!(parse_pdu_with_udh(ucs2).is_err());

        let gsm7 = "0791893905004100440C9189291032547600006211017191854006050003100301DEEE3208";
        assert!(parse_pdu_with_udh(gsm7).is_err());
    }

    #[test]
    fn test_concat_16bit_ref() {
        // Test UCS2_1234_P1 and UCS2_1234_P2 which use 16-bit ref (IE 0x08)
        use crate::test_vectors::concat_cases::*;
        let cases = [
            (UCS2_1234_P1, "UCS2_1234_P1"),
            (UCS2_1234_P2, "UCS2_1234_P2"),
        ];
        for (case, name) in cases {
            let (
                pdu,
                expected_sender,
                expected_message,
                expected_ref,
                expected_total,
                expected_seq,
            ) = case;
            let result = parse_pdu_with_udh(pdu)
                .unwrap_or_else(|e| panic!("Failed to parse {}: {}", name, e));
            assert_eq!(
                result.sms.sender, expected_sender,
                "Sender mismatch for {}",
                name
            );
            assert_eq!(
                result.sms.message, expected_message,
                "Message mismatch for {}",
                name
            );
            let concat = result
                .concat
                .unwrap_or_else(|| panic!("Expected concat info for {}", name));
            assert_eq!(concat.ref_num, expected_ref, "Ref mismatch for {}", name);
            assert_eq!(concat.total, expected_total, "Total mismatch for {}", name);
            assert_eq!(concat.seq, expected_seq, "Seq mismatch for {}", name);
        }
    }
}
