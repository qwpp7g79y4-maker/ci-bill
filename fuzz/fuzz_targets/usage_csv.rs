//! Usage CSV: arbitrary bytes must never panic, and a successful parse must
//! never let a per-row amount overflow `i64` micros silently (`rank` turns
//! that into a clean error instead).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(rows) = ci_bill::usage::parse_bytes(data) else {
        return;
    };
    // A successful parse must rank cleanly: either a `Ranking`, whose grand
    // total fits in `i64` micros by construction, or a clean `UsageError`
    // (e.g. `AmountOverflow`), never a panic.
    let _ = ci_bill::rank::rank(&rows, None);
});
