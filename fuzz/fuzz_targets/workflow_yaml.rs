//! Workflow YAML: arbitrary bytes, including anchors and aliases, must never
//! panic or allocate without bound (CB-1: alias expansion is budgeted).

#![no_main]

use std::path::Path;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let path = Path::new("ci.yml");
    let Ok(workflow) = ci_bill::workflow::scan_yaml(path, text) else {
        return;
    };
    let _ = ci_bill::findings::findings(std::slice::from_ref(&workflow));
});
