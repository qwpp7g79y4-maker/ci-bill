//! `--quiet` and `--verbose`. A pipe prints no `what to do` line.
//! `--verbose` adds that line, and on success one context line. Exit codes stay.

use std::process::{Command, Stdio};

fn run(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_ci-bill"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn help_lists_quiet_and_verbose() {
    let (code, stdout, stderr) = run(&["--help"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("--quiet"), "{stdout}");
    assert!(stdout.contains("--verbose"), "{stdout}");
    assert!(!stdout.contains("--threshold"));
    let (report_code, report_help, report_err) = run(&["report", "--help"]);
    assert_eq!(report_code, 0, "{report_err}");
    assert!(report_help.contains("--quiet"), "{report_help}");
    assert!(report_help.contains("--verbose"), "{report_help}");
}

#[test]
fn pipe_and_quiet_omit_the_hint_verbose_prints_it() {
    let missing = ["report", "--usage", "examples/does-not-exist.csv"];
    let (code, stdout, stderr) = run(&missing);
    assert_eq!(code, 2, "{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert_eq!(stderr.lines().count(), 1, "{stderr:?}");
    assert!(stderr.contains("cannot read file"), "{stderr}");
    assert!(!stderr.contains("what to do:"), "{stderr}");

    let (qcode, qout, qerr) = run(&[
        "--quiet",
        "report",
        "--usage",
        "examples/does-not-exist.csv",
    ]);
    assert_eq!(qcode, 2, "{qerr}");
    assert!(qout.is_empty());
    assert_eq!(qerr, stderr, "quiet matches a pipe");

    let (vcode, vout, verr) = run(&[
        "--verbose",
        "report",
        "--usage",
        "examples/does-not-exist.csv",
    ]);
    assert_eq!(vcode, 2, "{verr}");
    assert!(vout.is_empty());
    assert!(verr.contains("cannot read file"), "{verr}");
    assert!(
        verr.contains("what to do: check that the path exists"),
        "{verr}"
    );
}

#[test]
fn quiet_with_verbose_stays_exit_3() {
    let (code, stdout, stderr) = run(&[
        "--quiet",
        "--verbose",
        "report",
        "--usage",
        "samples/usage-sample.csv",
    ]);
    assert_eq!(code, 3, "{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("error:"), "{stderr}");
    assert_ne!(code, 1, "no threshold exit");

    let (nested, nested_out, nested_err) = run(&[
        "report",
        "--quiet",
        "--verbose",
        "--usage",
        "samples/usage-sample.csv",
    ]);
    assert_eq!(nested, 3, "{nested_err}");
    assert!(nested_out.is_empty(), "{nested_out}");
}

#[test]
fn verbose_context_stays_off_stdout() {
    let args = ["report", "--usage", "samples/usage-sample.csv"];
    let (code, stdout, stderr) = run(&args);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");

    let (vcode, vout, verr) = run(&["--verbose", "report", "--usage", "samples/usage-sample.csv"]);
    assert_eq!(vcode, 0, "{verr}");
    assert_eq!(vout, stdout, "verbose does not change the report");
    assert!(
        verr.starts_with("context: workflows ") && verr.contains(", findings "),
        "{verr:?}"
    );
    assert_eq!(verr.lines().count(), 1, "{verr:?}");

    let (qcode, qout, qerr) = run(&["--quiet", "report", "--usage", "samples/usage-sample.csv"]);
    assert_eq!(qcode, 0, "{qerr}");
    assert_eq!(qout, stdout);
    assert!(qerr.is_empty(), "{qerr}");
}

#[test]
fn prices_verbose_adds_context_and_keeps_stdout() {
    let (code, stdout, stderr) = run(&["prices"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    let (vcode, vout, verr) = run(&["--verbose", "prices"]);
    assert_eq!(vcode, 0, "{verr}");
    assert_eq!(vout, stdout);
    assert!(verr.starts_with("context: lines "), "{verr:?}");
    let last = verr.trim_end().chars().next_back().unwrap_or('x');
    assert!(last.is_ascii_digit(), "{verr}");
    let (qcode, qout, qerr) = run(&["--quiet", "prices"]);
    assert_eq!(qcode, 0, "{qerr}");
    assert_eq!(qout, stdout);
    assert!(qerr.is_empty(), "{qerr}");
}

#[test]
fn unknown_label_stays_exit_3_without_a_hint_line() {
    for args in [
        &["prices", "--label", "no-such-runner"][..],
        &["--quiet", "prices", "--label", "no-such-runner"][..],
        &["--verbose", "prices", "--label", "no-such-runner"][..],
    ] {
        let (code, stdout, stderr) = run(args);
        assert_eq!(code, 3, "{args:?} {stderr}");
        assert!(stdout.is_empty(), "{args:?}");
        assert!(stderr.contains("unknown runner label"), "{args:?} {stderr}");
        assert!(!stderr.contains("what to do:"), "{args:?} {stderr}");
        assert!(!stderr.contains("context:"), "{args:?} {stderr}");
        assert_ne!(code, 1);
    }
}

#[test]
fn budget_exit_stays_1() {
    for args in [
        &[
            "report",
            "--usage",
            "samples/usage-sample.csv",
            "--budget",
            "0",
        ][..],
        &[
            "--quiet",
            "report",
            "--usage",
            "samples/usage-sample.csv",
            "--budget",
            "0",
        ][..],
        &[
            "--verbose",
            "report",
            "--usage",
            "samples/usage-sample.csv",
            "--budget",
            "0",
        ][..],
    ] {
        let (code, stdout, stderr) = run(args);
        assert_eq!(code, 1, "{args:?} stdout={stdout} stderr={stderr}");
        assert!(stdout.contains("Budget exceeded"), "{args:?} {stdout}");
        assert!(!stderr.contains("context:"), "{args:?} {stderr}");
    }
}

#[test]
fn fetch_date_error_stays_exit_3() {
    for args in [
        &[
            "fetch",
            "--repo",
            "a/b",
            "--out",
            "out",
            "--since",
            "2026-02-01",
            "--until",
            "2026-01-01",
        ][..],
        &[
            "--quiet",
            "fetch",
            "--repo",
            "a/b",
            "--out",
            "out",
            "--since",
            "2026-02-01",
            "--until",
            "2026-01-01",
        ][..],
        &[
            "--verbose",
            "fetch",
            "--repo",
            "a/b",
            "--out",
            "out",
            "--since",
            "2026-02-01",
            "--until",
            "2026-01-01",
        ][..],
    ] {
        let (code, stdout, stderr) = run(args);
        assert_eq!(code, 3, "{args:?} {stderr}");
        assert!(stdout.is_empty(), "{args:?}");
        assert!(stderr.contains("swap the two dates"), "{args:?} {stderr}");
        assert!(!stderr.contains("context:"), "{args:?} {stderr}");
        assert_ne!(code, 1);
    }
}
