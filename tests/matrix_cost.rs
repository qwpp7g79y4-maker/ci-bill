//! Matrix rounding loss on the report command.

use std::path::PathBuf;
use std::process::Command;

const LINUX_6: &str = "matrix runs 6 jobs; 12 billed minutes vs 7 raw minutes; rounding loss 5 minutes at 0.006 USD/min (0.03 USD)";
const MACOS_6: &str = "matrix runs 6 jobs; 12 billed minutes vs 7 raw minutes; rounding loss 5 minutes at 0.062 USD/min (0.31 USD)";

const YAML: &str = "\
name: CI
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    strategy:
      matrix:
        os: [ubuntu-latest, windows-latest]
        node: [14, 16, 18]
    steps:
      - run: echo build
";

const USAGE: &str = "\
date,product,sku,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,net_amount,username,organization,repository,workflow_path
2026-04-01,actions,actions_linux,12,minutes,0.006,0.072,0,0.072,dev,acme,acme/widgets,.github/workflows/ci.yml
";

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

fn scratch(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("matrix-cost")
        .join(name);
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(path.join("workflows")).unwrap();
    std::fs::create_dir_all(path.join("runs")).unwrap();
    path
}

fn assert_public(text: &str) {
    let lower = text.to_ascii_lowercase();
    assert!(!lower.contains("not supported"), "{text}");
    assert!(!lower.contains("limitation"), "{text}");
    assert!(!lower.contains("skipped"), "{text}");
}

fn cell(index: usize, prefix: bool) -> String {
    if prefix {
        let os = if index < 3 {
            "ubuntu-latest"
        } else {
            "windows-latest"
        };
        let node = match index % 3 {
            0 => "14",
            1 => "16",
            _ => "18",
        };
        format!("build ({os}, {node})")
    } else {
        format!("cell-{index}")
    }
}

fn jobs_body(names: &[String], label: &str, extra_noise: bool) -> String {
    let mut items = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let id = index + 1;
        // Key order changes with the job so a fixed field order is not required.
        if index % 2 == 0 {
            items.push(format!(
                "{{\"id\":{id},\"run_id\":1,\"run_attempt\":1,\"name\":{name:?},\"labels\":[{label:?}],\"started_at\":\"2026-04-01T00:00:00Z\",\"completed_at\":\"2026-04-01T00:01:10Z\",\"conclusion\":\"success\",\"runner_name\":null}}"
            ));
        } else {
            items.push(format!(
                "{{\"runner_name\":null,\"conclusion\":null,\"completed_at\":\"2026-04-01T00:01:10Z\",\"started_at\":\"2026-04-01T00:00:00Z\",\"labels\":[{label:?}],\"name\":{name:?},\"run_attempt\":1,\"run_id\":1,\"id\":{id}}}"
            ));
        }
    }
    if extra_noise {
        items.push(
            "{\"id\":40,\"run_id\":1,\"run_attempt\":1,\"name\":\"build (ubuntu-latest, 20)\",\"labels\":[\"macos-latest\"],\"started_at\":\"2026-04-01T00:00:00Z\",\"completed_at\":null,\"conclusion\":null}"
                .to_string(),
        );
        items.push(
            "{\"id\":42,\"run_id\":1,\"run_attempt\":1,\"name\":\"   \",\"labels\":null,\"started_at\":\"2026-04-01T00:00:00Z\",\"completed_at\":\"\",\"conclusion\":\"success\"}"
                .to_string(),
        );
    }
    format!("{{\"jobs\":[{}]}}", items.join(","))
}

fn runs_body() -> String {
    "{\"workflow_runs\":[{\"event\":\"push\",\"conclusion\":\"success\",\"run_number\":4,\"run_started_at\":\"2026-04-01T00:00:00Z\",\"path\":\"acme/widgets/.github/workflows/ci.yml@refs/heads/main\",\"run_attempt\":1,\"id\":1}]}".to_string()
}

fn write_case(name: &str, yaml: &str, jobs: &str, runs: &str) -> (PathBuf, PathBuf, PathBuf) {
    let dir = scratch(name);
    std::fs::write(dir.join("workflows/ci.yml"), yaml).unwrap();
    std::fs::write(dir.join("runs/jobs-1.json"), jobs).unwrap();
    std::fs::write(dir.join("runs/runs-1.json"), runs).unwrap();
    std::fs::write(dir.join("usage.csv"), USAGE).unwrap();
    (
        dir.join("usage.csv"),
        dir.join("workflows"),
        dir.join("runs"),
    )
}

fn run_report(usage: &str, workflows: &str, runs: &str, json: bool) -> String {
    let mut args = vec![
        "report",
        "--usage",
        usage,
        "--workflows",
        workflows,
        "--runs",
        runs,
    ];
    if json {
        args.push("--json");
    }
    let output = report(&args);
    assert_exit(&output, 0);
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
    let text = stdout(&output);
    assert_public(&text);
    text
}

fn f4_line(text: &str) -> String {
    text.lines()
        .find(|line| line.contains("F4 ") && line.contains("matrix runs"))
        .unwrap_or_else(|| panic!("missing F4 in {text}"))
        .to_string()
}

#[test]
fn six_linux_jobs_of_seventy_seconds_replace_the_estimate() {
    let names: Vec<String> = (0..6).map(|index| cell(index, true)).collect();
    let (usage, workflows, runs) = write_case(
        "linux",
        YAML,
        &jobs_body(&names, "ubuntu-latest", false),
        &runs_body(),
    );
    let text = run_report(
        usage.to_str().unwrap(),
        workflows.to_str().unwrap(),
        runs.to_str().unwrap(),
        false,
    );
    let line = f4_line(&text);
    assert!(line.contains(":8: "), "{line}");
    assert!(line.contains(LINUX_6), "{line}");
    assert!(
        !text.contains("each job is billed in whole minutes"),
        "{text}"
    );
    assert!(!text.contains("at least"), "{text}");
    assert!(!text.contains("Workflows without usage"), "{text}");

    let json = run_report(
        usage.to_str().unwrap(),
        workflows.to_str().unwrap(),
        runs.to_str().unwrap(),
        true,
    );
    let value: serde_json::Value = serde_json::from_str(json.trim_end()).unwrap();
    let findings = value["workflows"][0]["findings"].as_array().unwrap();
    let f4 = findings.iter().find(|item| item["id"] == "F4").unwrap();
    assert_eq!(f4["message"], LINUX_6);
    assert_eq!(f4["line"], 8);
}

#[test]
fn a_macos_matrix_shows_the_macos_rate() {
    let names: Vec<String> = (0..6).map(|index| cell(index, true)).collect();
    let (usage, workflows, runs) = write_case(
        "macos",
        YAML,
        &jobs_body(&names, "macos-latest", false),
        &runs_body(),
    );
    let text = run_report(
        usage.to_str().unwrap(),
        workflows.to_str().unwrap(),
        runs.to_str().unwrap(),
        false,
    );
    let line = f4_line(&text);
    assert!(line.contains(MACOS_6), "{line}");
    assert!(line.contains("0.062 USD/min"), "{line}");
    assert!(line.contains("(0.31 USD)"), "{line}");
    assert!(line.contains(":8: "), "{line}");
}

#[test]
fn jobs_without_a_prefix_land_on_the_matrix_line() {
    let names: Vec<String> = (0..6).map(|index| cell(index, false)).collect();
    let (usage, workflows, runs) = write_case(
        "plain",
        YAML,
        &jobs_body(&names, "ubuntu-latest", false),
        &runs_body(),
    );
    let text = run_report(
        usage.to_str().unwrap(),
        workflows.to_str().unwrap(),
        runs.to_str().unwrap(),
        false,
    );
    let line = f4_line(&text);
    assert!(line.contains(":8: "), "{line}");
    assert!(!line.contains(":4: "), "{line}");
    assert!(line.contains(LINUX_6), "{line}");
}

#[test]
fn crlf_capitals_reordered_keys_and_blank_fields_keep_the_loss() {
    let mut names: Vec<String> = (0..6)
        .map(|index| cell(index, true).to_ascii_uppercase())
        .collect();
    names.reverse();
    let yaml = YAML.replace('\n', "\r\n");
    let jobs = jobs_body(&names, "UBUNTU-LATEST", true).replace('\n', "\r\n");
    let runs = runs_body().replace('\n', "\r\n");
    let (usage, workflows, runs_dir) = write_case("crlf", &yaml, &jobs, &runs);
    let text = run_report(
        usage.to_str().unwrap(),
        workflows.to_str().unwrap(),
        runs_dir.to_str().unwrap(),
        false,
    );
    let line = f4_line(&text);
    assert!(line.contains(":8: "), "{line}");
    assert!(line.contains(LINUX_6), "{line}");
    assert!(
        !text.contains("each job is billed in whole minutes"),
        "{text}"
    );
}
