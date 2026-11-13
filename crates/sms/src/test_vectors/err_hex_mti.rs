#[cfg(test)]
/// Invalid hex strings that must return Err, never panic
pub const CASES: &[&str] = &[
    "",                                                           // empty
    "0",                                                          // odd length
    "ZZ",                                                         // invalid hex
    "é1",                                                         // non-ascii
    "+1",                                                         // invalid chars
    "07",                                                         // too short
    "0791893905004100040C9189291032547600006211017191854002C83",  // odd length
    "0791893905004100010C9189291032547600006211017191854002C834", // SMS-SUBMIT (MTI=01)
    "0791893905004100060C9189291032547600006211017191854002C834", // Status Report (MTI=06)
];
