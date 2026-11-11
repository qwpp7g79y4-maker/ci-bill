//! The webshop report is the same when the process has no way off the machine.
//!
//! W-45. `unshare --user --map-root-user --net` starts a network namespace
//! whose route table is empty, so nothing can connect out. The report reads
//! the existing webshop usage file. No provider and no price were added.
//! The embedded price table is unchanged.

#![cfg(target_os = "linux")]

use std::fs;
use std::process::Command;

/// A runner that may not create a user or network namespace.
fn netns_denied(stderr: &str) -> bool {
    let low = stderr.to_ascii_lowercase();
    low.contains("operation not permitted")
        || low.contains("eperm")
        || low.contains("uid_map")
        || low.contains("permission denied")
}

/// The webshop report, with outgoing connections blocked.
///
/// The proof reads `/proc/net/route` in that namespace. `report` then reads
/// `examples/webshop-usage.csv` and must print
/// `examples/webshop-usage.expected.txt`. No price file is passed.
/// When `unshare` is missing, or the namespace is not permitted, the test
/// prints `overgeslagen` and returns. Any other `unshare` failure still fails.
#[test]
fn webshop_report_matches_expected_output_with_outgoing_connections_blocked() {
    let ns = ["--user", "--map-root-user", "--net", "--"];
    let proof = match Command::new("unshare")
        .args(ns)
        .args(["cat", "/proc/net/route"])
        .output()
    {
        Ok(out) => out,
        Err(err) => {
            eprintln!("overgeslagen: geen unshare ({err})");
            return;
        }
    };
    if !proof.status.success() {
        let err = String::from_utf8_lossy(&proof.stderr);
        if netns_denied(&err) {
            eprintln!("overgeslagen: geen netns");
            return;
        }
        panic!("unshare failed: {err}");
    }
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
