use std::process::Command;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ci-bill"))
        .args(args)
        .output()
        .expect("run ci-bill")
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn help_shows_options() {
    let top = run(&["--help"]);
    assert!(
        top.status.success(),
        "ci-bill --help failed\n{}",
        stderr(&top)
    );
    assert!(
        stdout(&top).contains("report"),
        "top-level help should name the report command:\n{}",
        stdout(&top)
    );

    let output = run(&["report", "--help"]);
    assert!(
        output.status.success(),
        "ci-bill report --help failed\n{}",
        stderr(&output)
    );
    let text = stdout(&output);
    assert!(text.contains("--usage"), "{text}");
    assert!(text.contains("--workflows"), "{text}");
    assert!(text.contains("--top"), "{text}");
    assert!(text.contains("--json"), "{text}");
    assert!(text.contains("--budget"), "{text}");
}

#[test]
fn unknown_option_exits_3() {
    let output = run(&["--no-such-option"]);
    assert_eq!(
        output.status.code(),
        Some(3),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );

    let nested = run(&["report", "--usage", "usage.csv", "--no-such-option"]);
    assert_eq!(
        nested.status.code(),
        Some(3),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&nested),
        stderr(&nested)
    );
}

#[test]
fn unreadable_usage_exits_2() {
    let missing = run(&["report", "--usage", "/no/such/ci-bill-usage.csv"]);
    assert_eq!(
        missing.status.code(),
        Some(2),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&missing),
        stderr(&missing)
    );
    let missing_err = stderr(&missing);
    assert!(missing_err.contains("line 0"), "{missing_err}");
    assert!(missing_err.contains("cannot read file"), "{missing_err}");

    let path = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("bad-number.csv");
    std::fs::write(
        &path,
        "sku,quantity,unit_type,net_amount,gross_amount,workflow_path\nactions_linux,NOT_A_NUMBER,minutes,1,1,a.yml\n",
    )
    .unwrap();
    let output = run(&["report", "--usage", path.to_str().unwrap()]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    let err = stderr(&output);
    assert!(err.contains("line 2"), "{err}");
    assert!(err.contains("quantity is not a number"), "{err}");
    assert!(!err.contains("NOT_A_NUMBER"), "{err}");
}

#[test]
fn top_flag_exits_0() {
    let output = run(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--top",
        "2",
    ]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
}

#[test]
fn missing_workflows_dir_exits_2() {
    let output = run(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--workflows",
        "samples/no-such-workflows",
    ]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    let err = stderr(&output);
    assert!(err.contains("cannot read directory"), "{err}");
    assert!(err.contains("samples/no-such-workflows"), "{err}");
}

#[test]
fn sample_usage_exits_0() {
    let output = run(&["report", "--usage", "samples/usage-sample.csv"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
}

#[test]
fn invalid_workflow_yaml_exits_2_without_content() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("invalid-workflow-yaml");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let secret = "SECRET_TOKEN_XYZ";
    std::fs::write(
        dir.join("broken.yml"),
        format!("name: Demo\njobs: [\n  {secret}\n"),
    )
    .unwrap();

    let output = run(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--workflows",
        dir.to_str().unwrap(),
    ]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    let err = stderr(&output);
    assert!(err.contains("broken.yml"), "{err}");
    assert!(err.contains("line 4"), "{err}");
    assert!(err.contains("invalid YAML"), "{err}");
    assert!(!err.contains(secret), "{err}");
    assert!(!stdout(&output).contains(secret), "{}", stdout(&output));
}

#[test]
fn missing_usage_exits_3() {
    let output = run(&["report"]);
    assert_eq!(
        output.status.code(),
        Some(3),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );

    let with_optional = run(&["report", "--workflows", "workflows", "--top", "5"]);
    assert_eq!(
        with_optional.status.code(),
        Some(3),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&with_optional),
        stderr(&with_optional)
    );
}
