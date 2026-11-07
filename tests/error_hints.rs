//! Error messages say what went wrong and what the user can do about it.

use std::process::{Command, Output};

fn ci_bill(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ci-bill"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(args)
        .output()
        .expect("run ci-bill")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn unreadable_usage_file_says_how_to_fix_it() {
    let out = ci_bill(&["report", "--usage", "examples/does-not-exist.csv"]);
    assert_eq!(out.status.code(), Some(2));
    let err = stderr(&out);
    assert!(err.contains("cannot read file"), "{err}");
    assert!(err.contains("what to do:"), "{err}");
}

#[test]
fn missing_workflows_directory_says_how_to_fix_it() {
    let out = ci_bill(&[
        "report",
        "--usage",
        "examples/usage.csv",
        "--workflows",
        "examples/no-such-dir",
    ]);
    assert_eq!(out.status.code(), Some(2));
    let err = stderr(&out);
    assert!(err.contains("cannot read directory"), "{err}");
    assert!(err.contains("what to do: check that --workflows"), "{err}");
}

#[test]
fn missing_runs_directory_says_how_to_fix_it() {
    let out = ci_bill(&[
        "report",
        "--usage",
        "examples/usage.csv",
        "--runs",
        "examples/no-such-dir",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("what to do: check that --runs"));
}

#[test]
fn unknown_runner_label_suggests_sku_or_runner_map() {
    let out = ci_bill(&["prices", "--label", "no-such-runner"]);
    assert_eq!(out.status.code(), Some(3));
    let err = stderr(&out);
    assert!(
        err.contains("unknown runner label: no-such-runner"),
        "{err}"
    );
    assert!(err.contains("--runner-map"), "{err}");
}

#[test]
fn reversed_fetch_dates_say_to_swap_them() {
    let out = ci_bill(&[
        "fetch",
        "--repo",
        "a/b",
        "--out",
        "/nonexistent-out",
        "--since",
        "2026-02-01",
        "--until",
        "2026-01-01",
    ]);
    assert_eq!(out.status.code(), Some(3));
    assert!(stderr(&out).contains("swap the two dates"));
}
