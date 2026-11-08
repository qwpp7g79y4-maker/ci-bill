//! Each input under `examples/` produces the output stored next to it.

use std::fs;
use std::process::Command;

fn run(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_ci-bill"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(args)
        .output()
        .expect("run ci-bill");
    assert!(
        output.status.success(),
        "args {args:?}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    String::from_utf8(output.stdout).expect("utf-8 stdout")
}

fn expected(name: &str) -> String {
    let path = format!("{}/examples/{name}", env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("{path}: {err}"))
}

#[test]
fn webshop_example_matches_golden_output() {
    let got = run(&["report", "--usage", "examples/webshop-usage.csv"]);
    assert_eq!(got, expected("webshop-usage.expected.txt"));
}

#[test]
fn api_gateway_example_matches_golden_output() {
    let got = run(&[
        "report",
        "--usage",
        "examples/api-gateway-usage.csv",
        "--workflows",
        "examples/api-gateway-workflows",
    ]);
    assert_eq!(got, expected("api-gateway-usage.expected.txt"));
}

#[test]
fn minimal_usage_example_is_accepted() {
    let got = run(&["report", "--usage", "examples/usage.csv"]);
    assert!(!got.is_empty());
}
