//! Price table JSON (`--sku`/`--label` table, or a future `--prices` file)
//! and runner-map CSV (`--runner-map`): arbitrary bytes must never panic.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = ci_bill::prices::parse_table_json(text);
    }
    if let Ok(table) = ci_bill::prices::embedded_table() {
        let _ = ci_bill::prices::parse_runner_map(data, table);
    }
});
