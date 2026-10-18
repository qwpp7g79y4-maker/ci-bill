//! Saved GitHub Actions API JSON (`--runs` directory pages): arbitrary bytes
//! fed to both the `runs-*.json` and `jobs-*.json` parsers must never panic.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let _ = ci_bill::api::parse_runs(text);
    let _ = ci_bill::api::parse_jobs(text);
});
