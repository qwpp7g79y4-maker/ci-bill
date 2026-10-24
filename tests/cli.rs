use std::io::Write;
use std::process::{Command, Stdio};

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ci-bill"))
        .args(args)
        .output()
        .expect("run ci-bill")
}

fn run_with_stdin(args: &[&str], input: &[u8]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ci-bill"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ci-bill");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(input)
        .expect("write ci-bill stdin");
    child.wait_with_output().expect("wait for ci-bill")
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
    assert!(text.contains("--runs"), "{text}");
    assert!(text.contains("--top"), "{text}");
    assert!(text.contains("--json"), "{text}");
    assert!(text.contains("--budget"), "{text}");
    assert!(text.contains("--fix-diff"), "{text}");
    assert!(text.contains("--md"), "{text}");
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
        "date,product,sku,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,net_amount,username,organization,repository,workflow_path\n2026-09-01,actions,actions_linux,NOT_A_NUMBER,minutes,0.006,1,0,1,ada,example-org,example-org/app,a.yml\n",
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
fn amount_overflow_exits_2_for_text_json_and_md() {
    let path = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("overflow.csv");
    std::fs::write(
        &path,
        "date,product,sku,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,net_amount,username,organization,repository,workflow_path\n\
2026-09-01,actions,actions_linux,1,minutes,0,0,0,9000000000000,ada,example-org,example-org/app,a.yml\n\
2026-09-02,actions,actions_linux,1,minutes,0,0,0,9000000000000,ada,example-org,example-org/app,b.yml\n",
    )
    .unwrap();

    for extra_args in [[].as_slice(), ["--json"].as_slice(), ["--md"].as_slice()] {
        let mut args = vec!["report", "--usage", path.to_str().unwrap()];
        args.extend_from_slice(extra_args);
        let output = run(&args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "args {args:?}\nstdout:\n{}\nstderr:\n{}",
            stdout(&output),
            stderr(&output)
        );
        let err = stderr(&output);
        assert!(!err.contains("line 0"), "{args:?}: {err}");
        assert!(
            err.contains("net_amount total is out of range (sum of all rows)"),
            "{args:?}: {err}"
        );
        assert!(stdout(&output).is_empty(), "{args:?}: {}", stdout(&output));
    }
}

#[test]
fn omitted_workflows_net_overflow_does_not_panic() {
    let path = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("omitted-overflow.csv");
    std::fs::write(
        &path,
        "date,product,sku,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,net_amount,username,organization,repository,workflow_path\n\
2026-09-01,actions,actions_linux,1,count,9000000000000,9000000000000,0,9000000000000,ada,example-org,example-org/app,a.yml\n\
2026-09-02,actions,actions_linux,1,count,8000000000000,0,8000000000000,-8000000000000,ada,example-org,example-org/app,b.yml\n\
2026-09-03,actions,actions_linux,1,count,8000000000000,0,8000000000000,-8000000000000,ada,example-org,example-org/app,c.yml\n",
    )
    .unwrap();

    for extra_args in [[].as_slice(), ["--json"].as_slice(), ["--md"].as_slice()] {
        let mut args = vec!["report", "--usage", path.to_str().unwrap(), "--top", "1"];
        args.extend_from_slice(extra_args);
        let output = run(&args);
        assert_eq!(
            output.status.code(),
            Some(0),
            "args {args:?}\nstdout:\n{}\nstderr:\n{}",
            stdout(&output),
            stderr(&output)
        );
        assert!(stderr(&output).is_empty(), "{args:?}: {}", stderr(&output));
        if extra_args == ["--json"] {
            let value: serde_json::Value =
                serde_json::from_str(&stdout(&output)).expect("report json");
            assert!(value["outside_top"]["net"].is_string());
        }
    }
}

#[test]
fn usage_dash_reads_stdin() {
    let csv = std::fs::read("samples/usage-sample.csv").unwrap();
    let from_file = run(&["report", "--usage", "samples/usage-sample.csv"]);
    assert_eq!(from_file.status.code(), Some(0), "{}", stderr(&from_file));

    let from_stdin = run_with_stdin(&["report", "--usage", "-"], &csv);
    assert_eq!(
        from_stdin.status.code(),
        Some(0),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&from_stdin),
        stderr(&from_stdin)
    );
    assert_eq!(stdout(&from_stdin), stdout(&from_file));
    assert!(stderr(&from_stdin).is_empty(), "{}", stderr(&from_stdin));
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
