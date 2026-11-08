//! The webshop report is the same when the process has no way off the machine.
//!
//! W-45. `unshare --user --map-root-user --net` starts a network namespace
//! whose route table is empty, so nothing can connect out. The report reads
//! the existing webshop usage file. No provider and no price were added.
//! The embedded price table is unchanged.

#![cfg(target_os = "linux")]

use std::fs;
use std::process::Command;

/// The webshop report, with outgoing connections blocked.
///
/// The proof reads `/proc/net/route` in that namespace. `report` then reads
/// `examples/webshop-usage.csv` and must print
/// `examples/webshop-usage.expected.txt`. No price file is passed.
#[test]
fn webshop_report_matches_expected_output_with_outgoing_connections_blocked() {
    let ns = ["--user", "--map-root-user", "--net", "--"];
    let proof = Command::new("unshare")
        .args(ns)
        .args(["cat", "/proc/net/route"])
        .output()
        .expect("unshare route table");
    assert!(
        proof.status.success(),
        "unshare failed: {}",
        String::from_utf8_lossy(&proof.stderr)
    );
    let routes = String::from_utf8(proof.stdout).expect("utf-8 route table");
    assert!(
        !routes.split_whitespace().any(|field| field == "00000000"),
        "network namespace still has a route: {routes:?}"
    );

    let root = env!("CARGO_MANIFEST_DIR");
    let out = Command::new("unshare")
        .args(ns)
        .arg(env!("CARGO_BIN_EXE_ci-bill"))
        .current_dir(root)
        .args(["report", "--usage", "examples/webshop-usage.csv"])
        .output()
        .expect("run ci-bill with outgoing connections blocked");
    let want = fs::read_to_string(format!("{root}/examples/webshop-usage.expected.txt"))
        .expect("expected file");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8(out.stdout).expect("utf-8"), want);
}
