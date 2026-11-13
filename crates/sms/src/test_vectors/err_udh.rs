#[cfg(test)]
/// PDUs with UDH errors that must return Err
pub const CASES: &[&str] = &[
    // UDHL overflow
    "0791893905004100440C918929103254760000621101719185400CFF00032A0202AE6F399B0C",
];
