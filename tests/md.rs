use std::process::Command;

fn report(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ci-bill"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(args)
        .output()
        .expect("run ci-bill")
}

#[test]
fn sample_md_matches_snapshot() {
    let output = report(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--workflows",
        "samples/.github/workflows",
        "--md",
    ]);
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stderr:\n{stderr}\nstdout:\n{stdout}"
    );
    assert!(stderr.is_empty(), "{stderr}");
    let expected = include_str!("snapshots/report.md");
    assert_eq!(stdout, expected);
    assert!(stdout.contains("lint.yml:8"), "{stdout}");
    assert!(stdout.contains("207.87"), "{stdout}");
    assert!(!stdout.contains("Sample"), "{stdout}");
}

#[test]
fn md_is_not_affected_by_top() {
    let with_top = report(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--workflows",
        "samples/.github/workflows",
        "--md",
        "--top",
        "1",
    ]);
    let stdout = String::from_utf8(with_top.stdout).expect("utf-8 stdout");
    assert!(with_top.status.success());
    let expected = include_str!("snapshots/report.md");
    assert_eq!(
        stdout, expected,
        "the teardown lists every job and finding, not just the top workflow"
    );
}

#[test]
fn md_without_workflows_omits_job_and_finding_sections() {
    let output = report(&["report", "--usage", "samples/usage-sample.csv", "--md"]);
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    assert!(output.status.success());
    assert!(stdout.starts_with("# ci-bill teardown\n"), "{stdout}");
    assert!(stdout.contains("Total net: 284.21 USD\n"), "{stdout}");
    assert!(!stdout.contains("## Top jobs"), "{stdout}");
    assert!(!stdout.contains("## Findings"), "{stdout}");
    assert!(!stdout.contains("## Suggested fix"), "{stdout}");
}

#[test]
fn md_runs_the_usual_budget_check() {
    let under = report(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--workflows",
        "samples/.github/workflows",
        "--md",
        "--budget",
        "1000",
    ]);
    assert!(under.status.success());
    assert!(String::from_utf8_lossy(&under.stdout).contains("# ci-bill teardown"));

    let over = report(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--workflows",
        "samples/.github/workflows",
        "--md",
        "--budget",
        "10",
    ]);
    assert_eq!(over.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&over.stdout);
    assert!(stdout.contains("# ci-bill teardown"), "{stdout}");
    assert!(
        stdout.contains("Budget exceeded: 284.21 > 10.00\n"),
        "{stdout}"
    );
}

#[test]
fn md_help_mentions_the_flag() {
    let output = report(&["report", "--help"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--md"), "{stdout}");
}
