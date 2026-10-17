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
        .join("remote-workflow")
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
2026-09-01,actions,actions_linux,10,minutes,0.006000,0.060000,0,0.060000,ci,example-org,example-org/app,.github/workflows/caller.yml
2026-09-02,actions,actions_linux,10,minutes,0.010000,0.100000,0,0.100000,ci,example-org,example-org/app,.github/workflows/other.yml
";

const CALLER: &str = "\
name: Caller
on: push
jobs:
  call-lint:
    uses: acme/tools/.github/workflows/lint.yml@v1
    with:
      extra: \"1\"
    secrets: inherit
";

const CALLEE: &str = "\
name: Lint
on: workflow_call
jobs:
  lint:
    runs-on: macos-latest
    timeout-minutes: 10
    steps:
      - run: cargo fmt --check
";

fn layout(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = scratch(name);
    let workflows = root.join("workflows");
    let runs = root.join("runs");
    std::fs::create_dir_all(&workflows).unwrap();
    std::fs::create_dir_all(&runs).unwrap();
    write(&root.join("usage.csv"), USAGE);
    write(&workflows.join("caller.yml"), CALLER);
    (root.join("usage.csv"), workflows, runs)
}

#[test]
fn cli_remote_without_cache_shows_the_uses_line() {
    let (usage, workflows, runs) = layout("without-cache");
    let output = report(&[
        "report",
        "--usage",
        usage.to_str().unwrap(),
        "--workflows",
        workflows.to_str().unwrap(),
        "--runs",
        runs.to_str().unwrap(),
    ]);
    assert_exit(&output, 0);
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
    let text = stdout(&output);
    assert_no_limitation(&text);
    assert!(
        text.contains("example-org/app .github/workflows/caller.yml"),
        "{text}"
    );
    assert!(text.contains("0.06"), "{text}");
    assert!(text.contains("0.10"), "{text}");
    assert!(
        text.contains("F10 ") && text.contains("caller.yml:5:"),
        "{text}"
    );
    assert!(
        text.contains("job call-lint uses acme/tools/.github/workflows/lint.yml@v1"),
        "{text}"
    );
    assert!(!text.contains("Workflows without usage"), "{text}");
    let caller = text
        .find("example-org/app .github/workflows/caller.yml")
        .unwrap();
    let other = text
        .find("example-org/app .github/workflows/other.yml")
        .unwrap();
    assert!(other < caller, "{text}");
    assert!(!text[other..caller].contains("F10 "), "{text}");
    assert!(text[caller..].contains("F10 "), "{text}");
}

#[test]
fn cli_remote_cache_file_shows_callee_findings() {
    let (usage, workflows, runs) = layout("with-cache");
    write(
        &runs.join("acme/tools/.github/workflows/lint.yml@v1"),
        CALLEE,
    );
    let output = report(&[
        "report",
        "--usage",
        usage.to_str().unwrap(),
        "--workflows",
        workflows.to_str().unwrap(),
        "--runs",
        runs.to_str().unwrap(),
        "--json",
    ]);
    assert_exit(&output, 0);
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
    let text = stdout(&output);
    assert_no_limitation(&text);
    let json: serde_json::Value = serde_json::from_str(text.trim_end()).expect("json");
    assert_eq!(json["schema_version"], 2);
    let caller = json["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["path"] == ".github/workflows/caller.yml")
        .unwrap();
    assert_eq!(caller["net"], "0.060000");
    let findings = caller["findings"].as_array().unwrap();
    assert!(
        findings.iter().any(|item| {
            item["id"] == "F1"
                && item["line"] == 5
                && item["message"] == "lint on macOS"
                && item["file"]
                    .as_str()
                    .unwrap()
                    .ends_with("acme/tools/.github/workflows/lint.yml@v1")
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
    assert_eq!(other["net"], "0.100000");
    assert!(other["findings"].as_array().unwrap().is_empty());
}

#[test]
fn cli_invalid_uses_is_a_finding() {
    let (usage, workflows, _) = layout("invalid");
    write(
        &workflows.join("caller.yml"),
        "name: Caller\non: push\njobs:\n  call-lint:\n    uses: \"\"\n  remote:\n    uses: acme/tools/.github/workflows/lint.yml@\n",
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
    assert!(text.contains("F11 "), "{text}");
    assert!(
        text.contains("invalid reusable workflow uses: job call-lint uses \"\""),
        "{text}"
    );
    assert!(
        text.contains("invalid reusable workflow uses: job remote uses \"acme/tools/.github/workflows/lint.yml@\""),
        "{text}"
    );
    assert!(text.contains("caller.yml:5:"), "{text}");
    assert!(text.contains("caller.yml:7:"), "{text}");
}

#[test]
fn cli_bad_cache_yaml_exits_2_without_the_body() {
    let (usage, workflows, runs) = layout("bad-yaml");
    write(
        &runs.join("acme/tools/.github/workflows/lint.yml@v1"),
        "jobs: [\nUNIQUE_SECRET_TOKEN\n",
    );
    let output = report(&[
        "report",
        "--usage",
        usage.to_str().unwrap(),
        "--workflows",
        workflows.to_str().unwrap(),
        "--runs",
        runs.to_str().unwrap(),
    ]);
    assert_exit(&output, 2);
    let err = stderr(&output);
    assert!(err.contains("invalid YAML"), "{err}");
    assert!(err.contains("line "), "{err}");
    assert!(!err.contains("UNIQUE_SECRET_TOKEN"), "{err}");
    assert!(!stdout(&output).contains("UNIQUE_SECRET_TOKEN"));
    assert_no_limitation(&err);
}

#[test]
fn cli_cache_coexists_with_saved_runs() {
    let (usage, workflows, runs) = layout("with-api");
    write(
        &runs.join("acme/tools/.github/workflows/lint.yml@feature/widget"),
        CALLEE,
    );
    write(
        &workflows.join("caller.yml"),
        "name: Caller\r\non: push\r\njobs:\r\n  call-lint:\r\n    secrets: inherit\r\n    uses: acme/tools/.github/workflows/lint.yml@feature/widget\r\n",
    );
    write(
        &runs.join("runs-1.json"),
        r#"{
  "workflow_runs": [{
    "id": 1001,
    "path": ".github/workflows/caller.yml",
    "run_attempt": 1,
    "run_number": 4,
    "event": "push",
    "conclusion": "success",
    "run_started_at": "2026-03-01T10:00:00Z"
  }]
}"#,
    );
    write(
        &runs.join("jobs-1001.json"),
        r#"{
  "jobs": [{
    "id": 501,
    "run_id": 1001,
    "run_attempt": 1,
    "name": "build",
    "labels": ["ubuntu-latest"],
    "started_at": "2026-03-01T10:00:05Z",
    "completed_at": "2026-03-01T10:02:05Z",
    "conclusion": "success",
    "runner_name": "runner"
  }]
}"#,
    );
    let output = report(&[
        "report",
        "--usage",
        usage.to_str().unwrap(),
        "--workflows",
        workflows.to_str().unwrap(),
        "--runs",
        runs.to_str().unwrap(),
    ]);
    assert_exit(&output, 0);
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
    let text = stdout(&output);
    assert_no_limitation(&text);
    assert!(text.contains("lint on macOS"), "{text}");
    assert!(text.contains("lint.yml@feature/widget"), "{text}");
    assert!(text.contains("build"), "{text}");
    assert!(text.contains("0.06"), "{text}");
}
