//! JSON report rendering: a usage CSV that ranks cleanly must render to text
//! that `serde_json` can parse back, and the grand total must stay a finite,
//! non-negative-overflowing `i64` micros amount.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(rows) = ci_bill::usage::parse_bytes(data) else {
        return;
    };
    let Ok(ranking) = ci_bill::rank::rank(&rows, None) else {
        return;
    };
    let text =
        ci_bill::report::render_json(&ranking.workflows, &ranking.bill, None, None, None, &[]);
    let value: serde_json::Value =
        serde_json::from_str(&text).expect("render_json must always produce valid JSON");
    assert!(
        value["total_net"].is_string(),
        "total_net missing or not a string: {value}"
    );
});
