pub mod concat_cases;
pub mod err_hex_mti;
pub mod err_length;
pub mod err_udh;
#[cfg(test)]
pub mod ok_cases;

/// Smoke test to verify the module compiles
#[test]
fn test_vectors_compile() {
    // Just verify we can access the constants
    let _ = ok_cases::GSM7_HI;
    let _ = ok_cases::UCS2_SALAM;
    let _ = err_hex_mti::CASES;
    let _ = err_length::CASES;
    let _ = err_udh::CASES;
    let _ = concat_cases::GSM7_2A_P1;
}
