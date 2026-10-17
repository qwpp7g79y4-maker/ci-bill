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
