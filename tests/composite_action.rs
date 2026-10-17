use std::path::{Path, PathBuf};
use std::process::Command;

fn report(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ci-bill"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(args)
        .output()
        .expect("run ci-bill")
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("utf-8 stdout")
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("utf-8 stderr")
}

fn assert_exit(output: &std::process::Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout:\n{}\nstderr:\n{}",
        stdout(output),
        stderr(output)
    );
}

fn assert_no_limitation(text: &str) {
    let lower = text.to_ascii_lowercase();
    assert!(!lower.contains("not supported"), "{text}");
    assert!(!lower.contains("unsupported"), "{text}");
    assert!(!lower.contains("skipped"), "{text}");
    assert!(!lower.contains("not implemented"), "{text}");
}

fn scratch(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("composite-action")
        .join(name);
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, text).unwrap();
}

const USAGE: &str = "\
date,product,sku,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,net_amount,username,organization,repository,workflow_path
2026-09-01,actions,actions_macos,10,minutes,0.062000,0.620000,0,0.620000,ci,example-org,example-org/app,.github/workflows/caller.yml
2026-09-02,actions,actions_linux,10,minutes,0.006000,0.060000,0,0.060000,ci,example-org,example-org/app,.github/workflows/other.yml
";

const CALLER: &str = "\
name: Build
on: push
jobs:
  build:
    runs-on: macos-latest
    timeout-minutes: 10
    steps:
      - uses: ./.github/actions/lint
";

const COMPOSITE: &str = "\
name: Lint
description: Format the tree
runs:
  using: composite
  steps:
    - shell: bash
      run: cargo fmt --check
";

fn layout(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = scratch(name);
    let workflows = root.join(".github").join("workflows");
    std::fs::create_dir_all(&workflows).unwrap();
    write(&root.join("usage.csv"), USAGE);
    write(&workflows.join("caller.yml"), CALLER);
    write(&workflows.join("other.yml"), "name: Other\non: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    timeout-minutes: 5\n    steps:\n      - run: cargo test\n");
    let usage = root.join("usage.csv");
    (root, usage, workflows)
}

#[test]
fn cli_composite_lint_on_macos_uses_the_action_line() {
    let (root, usage, workflows) = layout("macos-yml");
    write(&root.join(".github/actions/lint/action.yml"), COMPOSITE);
    let output = report(&[
        "report",
        "--usage",
        usage.to_str().unwrap(),
        "--workflows",
        workflows.to_str().unwrap(),
        "--json",
    ]);
    assert_exit(&output, 0);
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
    let text = stdout(&output);
    assert_no_limitation(&text);
    let json: serde_json::Value = serde_json::from_str(text.trim_end()).expect("json");
    let caller = json["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["path"] == ".github/workflows/caller.yml")
        .unwrap();
    assert_eq!(caller["net"], "0.620000");
    assert_eq!(caller["steps"][0]["name"], "./.github/actions/lint");
    assert_eq!(caller["steps"][0]["line"], 8);
    let findings = caller["findings"].as_array().unwrap();
    assert!(
        findings.iter().any(|item| {
            item["id"] == "F1"
                && item["line"] == 7
                && item["message"] == "lint on macOS"
                && item["file"].as_str().unwrap().ends_with("action.yml")
        }),
        "{findings:?}"
    );
    assert!(json["unattached_findings"].as_array().unwrap().is_empty());
    let other = json["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["path"] == ".github/workflows/other.yml")
        .unwrap();
    assert!(other["findings"].as_array().unwrap().is_empty());

    let text_output = report(&[
        "report",
        "--usage",
        usage.to_str().unwrap(),
        "--workflows",
        workflows.to_str().unwrap(),
    ]);
    assert_exit(&text_output, 0);
    let rendered = stdout(&text_output);
    assert_no_limitation(&rendered);
    assert!(
        rendered.contains("action.yml:7: lint on macOS"),
        "{rendered}"
    );
    assert!(rendered.contains("0.62"), "{rendered}");
    assert!(!rendered.contains("Workflows without usage"), "{rendered}");
    let caller_at = rendered
        .find("example-org/app .github/workflows/caller.yml")
        .unwrap();
    let other_at = rendered
        .find("example-org/app .github/workflows/other.yml")
        .unwrap();
    assert!(caller_at < other_at, "{rendered}");
    assert!(rendered[caller_at..other_at].contains("F1 "), "{rendered}");
    assert!(!rendered[other_at..].contains("F1 "), "{rendered}");
}

#[test]
fn cli_action_yaml_variant_uses_that_file() {
    let (root, usage, workflows) = layout("macos-yaml");
    write(&root.join(".github/actions/lint/action.yaml"), COMPOSITE);
    let output = report(&[
        "report",
        "--usage",
        usage.to_str().unwrap(),
        "--workflows",
        workflows.to_str().unwrap(),
    ]);
    assert_exit(&output, 0);
    let text = stdout(&output);
    assert_no_limitation(&text);
    assert!(text.contains("action.yaml:7: lint on macOS"), "{text}");
    assert!(!text.contains("Workflows without usage"), "{text}");
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
}

#[test]
fn cli_missing_action_directory_is_a_finding() {
    let (_root, usage, workflows) = layout("missing");
    let output = report(&[
        "report",
        "--usage",
        usage.to_str().unwrap(),
        "--workflows",
        workflows.to_str().unwrap(),
    ]);
    assert_exit(&output, 0);
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
    let text = stdout(&output);
    assert_no_limitation(&text);
    assert!(text.contains("F12 "), "{text}");
    assert!(
        text.contains("caller.yml:8: action not found: job build uses ./.github/actions/lint"),
        "{text}"
    );
    assert!(!text.contains("Workflows without usage"), "{text}");
    assert!(text.contains("0.62"), "{text}");
}

#[test]
fn cli_docker_and_node_actions_stay_on_the_step() {
    let (root, usage, workflows) = layout("docker");
    write(
        &root.join(".github/actions/lint/action.yml"),
        "name: Image\nruns:\n  using: docker\n  image: Dockerfile\n  steps:\n    - run: cargo fmt --check\n",
    );
    write(
        &root.join(".github/actions/lint/Dockerfile"),
        "FROM alpine\nRUN cargo fmt --check\n",
    );
    let output = report(&[
        "report",
        "--usage",
        usage.to_str().unwrap(),
        "--workflows",
        workflows.to_str().unwrap(),
        "--json",
    ]);
    assert_exit(&output, 0);
    let text = stdout(&output);
    assert_no_limitation(&text);
    let json: serde_json::Value = serde_json::from_str(text.trim_end()).unwrap();
    let caller = json["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["path"] == ".github/workflows/caller.yml")
        .unwrap();
    assert!(
        caller["findings"].as_array().unwrap().is_empty(),
        "{caller}"
    );
    assert_eq!(caller["steps"][0]["name"], "./.github/actions/lint");
    assert_eq!(caller["steps"][0]["line"], 8);

    write(
        &root.join(".github/actions/lint/action.yml"),
        "name: Script\nruns:\n  using: node20\n  main: index.js\n  steps:\n    - run: npx eslint .\n",
    );
    let output = report(&[
        "report",
        "--usage",
        usage.to_str().unwrap(),
        "--workflows",
        workflows.to_str().unwrap(),
    ]);
    assert_exit(&output, 0);
    let text = stdout(&output);
    assert_no_limitation(&text);
    assert!(!text.contains("F1 "), "{text}");
    assert!(!text.contains("F12 "), "{text}");
}

#[test]
fn cli_invalid_action_yaml_exits_2_without_the_body() {
    let (root, usage, workflows) = layout("bad-yaml");
    let token = "TOKEN_NOT_IN_ERRORS";
    write(
        &root.join(".github/actions/lint/action.yml"),
        &format!("runs: [\n{token}\n"),
    );
    let output = report(&[
        "report",
        "--usage",
        usage.to_str().unwrap(),
        "--workflows",
        workflows.to_str().unwrap(),
    ]);
    assert_exit(&output, 2);
    let err = stderr(&output);
    assert!(err.contains("invalid YAML"), "{err}");
    assert!(err.contains("action.yml"), "{err}");
    assert!(!err.contains(token), "{err}");
    assert!(!stdout(&output).contains(token), "{}", stdout(&output));
}
