//! Re-run waste on the report command.
//!
//! Inside one run, the first execution of a job name is the original run.
//! Each later execution with a different job id is waste unless it is an
//! inherited copy. The finding names the cost and the file line.

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

fn assert_plain(text: &str) {
    let lower = text.to_ascii_lowercase();
    assert!(!lower.contains("not supported"), "{text}");
    assert!(!lower.contains("unsupported"), "{text}");
    assert!(!lower.contains("skipped"), "{text}");
    assert!(!lower.contains("limitation"), "{text}");
    assert!(!lower.contains("not implemented"), "{text}");
}

fn scratch(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("rerun")
        .join(name);
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

const YAML: &str = "\
name: CI
on: push
jobs:
  lint:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - run: echo lint
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - run: echo test
";

struct Spec {
    id: u64,
    attempt: u64,
    name: String,
    labels: Option<Vec<String>>,
    started: Option<String>,
    completed: Option<String>,
    conclusion: Option<String>,
    runner: Option<String>,
}

fn finished(id: u64, attempt: u64, name: &str, day: &str, labels: &str) -> Spec {
    Spec {
        id,
        attempt,
        name: name.to_string(),
        labels: Some(vec![labels.to_string()]),
        started: Some(format!("2026-04-{day}T00:00:00Z")),
        completed: Some(format!("2026-04-{day}T00:10:00Z")),
        conclusion: Some("success".to_string()),
        runner: Some("runner-a".to_string()),
    }
}

fn field(key: &str, value: &str) -> String {
    format!("\"{key}\": {value}")
}

fn string_field(key: &str, value: &str) -> String {
    field(key, &format!("\"{value}\""))
}

fn spec_json(spec: &Spec, reversed: bool) -> String {
    let mut fields = vec![
        field("id", &spec.id.to_string()),
        field("run_id", "9"),
        field("run_attempt", &spec.attempt.to_string()),
        string_field("name", &spec.name),
    ];
    if let Some(labels) = &spec.labels {
        let items = labels
            .iter()
            .map(|label| format!("\"{label}\""))
            .collect::<Vec<_>>()
            .join(", ");
        fields.push(field("labels", &format!("[{items}]")));
    }
    if let Some(started) = &spec.started {
        fields.push(string_field("started_at", started));
    }
    match &spec.completed {
        Some(completed) if !completed.is_empty() => {
            fields.push(string_field("completed_at", completed));
        }
        Some(_) => fields.push(field("completed_at", "null")),
        None => {}
    }
    match &spec.conclusion {
        Some(conclusion) if !conclusion.is_empty() => {
            fields.push(string_field("conclusion", conclusion));
        }
        Some(_) => fields.push(field("conclusion", "null")),
        None => {}
    }
    match &spec.runner {
        Some(runner) if !runner.is_empty() => fields.push(string_field("runner_name", runner)),
        Some(_) => fields.push(field("runner_name", "null")),
        None => {}
    }
    if reversed {
        fields.reverse();
    }
    format!("{{{}}}", fields.join(", "))
}

struct Fixture {
    jobs: Vec<Spec>,
    minutes: u64,
    net: &'static str,
    gross: &'static str,
    run_path: &'static str,
    repo: &'static str,
    workflow_path: &'static str,
    reversed: bool,
    crlf: bool,
    yaml: String,
}

fn write_fixture(dir: &Path, fixture: &Fixture) {
    let workflows = dir.join("workflows");
    std::fs::create_dir_all(&workflows).unwrap();
    let yaml = if fixture.crlf {
        fixture.yaml.replace('\n', "\r\n")
    } else {
        fixture.yaml.clone()
    };
    std::fs::write(workflows.join("ci.yml"), yaml).unwrap();

    let mut runs = format!(
        "{{\"workflow_runs\": [{{\"id\": 9, \"path\": \"{}\", \"run_attempt\": 2, \"run_number\": 9, \"event\": \"push\", \"conclusion\": \"success\", \"run_started_at\": \"2026-04-02T00:00:00Z\"}}]}}",
        fixture.run_path
    );
    let mut jobs = format!(
        "{{\"jobs\": [{}]}}",
        fixture
            .jobs
            .iter()
            .map(|spec| spec_json(spec, fixture.reversed))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut usage = format!(
        "date,product,sku,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,net_amount,username,organization,repository,workflow_path\n2026-04-01,actions,actions_linux,{},minutes,0.006,{},0,{},dev,acme,{},{}\n",
        fixture.minutes, fixture.gross, fixture.net, fixture.repo, fixture.workflow_path
    );
    if fixture.reversed {
        usage = format!(
            "workflow_path,repository,organization,username,net_amount,discount_amount,gross_amount,applied_cost_per_quantity,unit_type,quantity,sku,product,date\n{},{},acme,dev,{},0,{},0.006,minutes,{},actions_linux,actions,2026-04-01\n",
            fixture.workflow_path, fixture.repo, fixture.net, fixture.gross, fixture.minutes
        );
        runs = format!(
            "{{\"workflow_runs\": [{{\"run_started_at\": \"2026-04-02T00:00:00Z\", \"conclusion\": \"success\", \"event\": \"push\", \"run_number\": 9, \"run_attempt\": 2, \"path\": \"{}\", \"id\": 9}}]}}",
            fixture.run_path
        );
    }
    if fixture.crlf {
        runs = runs.replace('\n', "\r\n");
        jobs = jobs.replace('\n', "\r\n");
        usage = usage.replace('\n', "\r\n");
        if !runs.contains('\r') {
            runs = runs.replace(',', ",\r\n");
            jobs = jobs.replace("}, {", "},\r\n{");
        }
    }
    std::fs::write(dir.join("runs-9.json"), runs).unwrap();
    std::fs::write(dir.join("jobs-9.json"), jobs).unwrap();
    std::fs::write(dir.join("usage.csv"), usage).unwrap();
}

fn run_fixture(name: &str, fixture: &Fixture) -> String {
    let dir = scratch(name);
    write_fixture(&dir, fixture);
    let usage = dir.join("usage.csv");
    let workflows = dir.join("workflows");
    let output = report(&[
        "report",
        "--usage",
        usage.to_str().unwrap(),
        "--workflows",
        workflows.to_str().unwrap(),
        "--runs",
        dir.to_str().unwrap(),
    ]);
    assert_exit(&output, 0);
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
    let text = stdout(&output);
    assert_plain(&text);
    text
}

fn f5_lines(text: &str) -> Vec<&str> {
    text.lines().filter(|line| line.contains("F5 ")).collect()
}

fn base(jobs: Vec<Spec>, minutes: u64, net: &'static str, gross: &'static str) -> Fixture {
    Fixture {
        jobs,
        minutes,
        net,
        gross,
        run_path: "acme/widgets/.github/workflows/ci.yml@main",
        repo: "acme/widgets",
        workflow_path: ".github/workflows/ci.yml",
        reversed: false,
        crlf: false,
        yaml: YAML.to_string(),
    }
}

#[test]
fn partial_rerun_counts_only_the_repeated_job() {
    let text = run_fixture(
        "partial",
        &base(
            vec![
                finished(1, 1, "lint", "01", "ubuntu-latest"),
                finished(2, 1, "test", "01", "ubuntu-latest"),
                finished(3, 2, "test", "02", "ubuntu-latest"),
            ],
            30,
            "0.180",
            "0.180",
        ),
    );
    let lines = f5_lines(&text);
    assert_eq!(lines.len(), 2, "{text}");
    assert!(
        text.contains("re-runs cost 0.06 USD (1 extra executions)"),
        "{text}"
    );
    assert!(
        text.contains("re-run of test (job 3, attempt 2) costs 0.06 USD"),
        "{text}"
    );
    assert!(text.contains("ci.yml:9:"), "{text}");
    assert!(!text.contains("re-run of lint"), "{text}");
    assert!(!text.contains("Workflows without usage"), "{text}");

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("rerun")
        .join("partial");
    let usage = dir.join("usage.csv");
    let output = report(&[
        "report",
        "--usage",
        usage.to_str().unwrap(),
        "--workflows",
        dir.join("workflows").to_str().unwrap(),
        "--runs",
        dir.to_str().unwrap(),
        "--json",
    ]);
    assert_exit(&output, 0);
    let body = stdout(&output);
    assert_plain(&body);
    let value: serde_json::Value = serde_json::from_str(body.trim_end()).unwrap();
    let findings = value["workflows"][0]["findings"].as_array().unwrap();
    let f5: Vec<_> = findings.iter().filter(|item| item["id"] == "F5").collect();
    assert_eq!(f5.len(), 2, "{findings:?}");
    assert!(f5
        .iter()
        .any(|item| item["message"] == "re-runs cost 0.06 USD (1 extra executions)"));
    let job = f5.iter().find(|item| item["line"] == 9).unwrap();
    assert!(job["file"].as_str().unwrap().ends_with("ci.yml"));
    assert_eq!(
        job["message"],
        "re-run of test (job 3, attempt 2) costs 0.06 USD"
    );
}

#[test]
fn partial_rerun_variants_keep_the_same_extra() {
    let mut later = finished(3, 2, "TEST", "02", "UBUNTU-LATEST");
    later.conclusion = Some(String::new());
    later.runner = None;
    let mut fixture = base(
        vec![
            later,
            finished(2, 1, "Test", "01", "UBUNTU-LATEST"),
            finished(1, 1, "LINT", "01", "UBUNTU-LATEST"),
        ],
        30,
        "0.180",
        "0.180",
    );
    fixture.run_path = "ACME/WIDGETS/.GitHub/Workflows/CI.yml@MAIN";
    fixture.reversed = true;
    fixture.crlf = true;
    let text = run_fixture("partial-variant", &fixture);
    let lines = f5_lines(&text);
    assert_eq!(lines.len(), 2, "{text}");
    assert!(
        text.contains("re-runs cost 0.06 USD (1 extra executions)"),
        "{text}"
    );
    assert!(
        text.contains("re-run of TEST (job 3, attempt 2) costs 0.06 USD"),
        "{text}"
    );
    assert!(text.contains("ci.yml:9:"), "{text}");
    assert!(!text.contains("re-run of LINT"), "{text}");
    assert!(
        text.contains("acme/widgets .github/workflows/ci.yml"),
        "{text}"
    );
    assert!(!text.contains("Workflows without usage"), "{text}");
}

#[test]
fn full_rerun_counts_every_job_once() {
    let text = run_fixture(
        "full",
        &base(
            vec![
                finished(1, 1, "lint", "01", "ubuntu-latest"),
                finished(2, 1, "test", "01", "ubuntu-latest"),
                finished(4, 2, "lint", "02", "ubuntu-latest"),
                finished(5, 2, "test", "02", "ubuntu-latest"),
            ],
            40,
            "0.240",
            "0.240",
        ),
    );
    let lines = f5_lines(&text);
    assert_eq!(lines.len(), 3, "{text}");
    assert!(
        text.contains("re-runs cost 0.12 USD (2 extra executions)"),
        "{text}"
    );
    assert!(text.contains("ci.yml:4:"), "{text}");
    assert!(text.contains("ci.yml:9:"), "{text}");
    assert!(text.contains("re-run of lint (job 4, attempt 2) costs 0.06 USD"));
    assert!(text.contains("re-run of test (job 5, attempt 2) costs 0.06 USD"));
}

#[test]
fn full_rerun_variants_keep_one_extra_per_job() {
    let mut lint = finished(4, 2, "LINT", "02", "UBUNTU-LATEST");
    lint.runner = Some(String::new());
    lint.conclusion = None;
    let mut test = finished(5, 2, "TEST", "02", "Ubuntu-Latest");
    test.runner = None;
    let mut fixture = base(
        vec![
            test,
            lint,
            finished(2, 1, "test", "01", "ubuntu-latest"),
            finished(1, 1, "lint", "01", "ubuntu-latest"),
        ],
        40,
        "0.240",
        "0.240",
    );
    fixture.reversed = true;
    fixture.crlf = true;
    fixture.run_path = "acme/widgets/.github/workflows/ci.yml@refs/heads/main";
    let text = run_fixture("full-variant", &fixture);
    let lines = f5_lines(&text);
    assert_eq!(lines.len(), 3, "{text}");
    assert!(
        text.contains("re-runs cost 0.12 USD (2 extra executions)"),
        "{text}"
    );
    assert!(text.contains("re-run of LINT (job 4, attempt 2) costs 0.06 USD"));
    assert!(text.contains("re-run of TEST (job 5, attempt 2) costs 0.06 USD"));
    assert!(text.contains("ci.yml:4:"), "{text}");
    assert!(text.contains("ci.yml:9:"), "{text}");
}

#[test]
fn an_inherited_copy_is_not_an_extra_execution() {
    let mut copied = finished(4, 2, "lint", "01", "ubuntu-latest");
    copied.started = Some("2026-04-01T00:00:00Z".to_string());
    copied.completed = Some("2026-04-01T00:10:00Z".to_string());
    let text = run_fixture(
        "copy",
        &base(
            vec![
                finished(1, 1, "lint", "01", "ubuntu-latest"),
                copied,
                finished(2, 1, "test", "01", "ubuntu-latest"),
                finished(5, 2, "test", "02", "ubuntu-latest"),
            ],
            30,
            "0.180",
            "0.180",
        ),
    );
    let lines = f5_lines(&text);
    assert_eq!(lines.len(), 2, "{text}");
    assert!(!text.contains("re-run of lint"), "{text}");
    assert!(text.contains("re-run of test (job 5, attempt 2) costs 0.06 USD"));
    assert!(text.contains("inherited copy"), "{text}");

    let mut only = finished(4, 2, "lint", "01", "ubuntu-latest");
    only.started = Some("2026-04-01T11:00:00Z".to_string());
    only.completed = Some("2026-04-01T11:10:00Z".to_string());
    let quiet = run_fixture(
        "copy-only",
        &base(
            vec![finished(1, 1, "lint", "01", "ubuntu-latest"), only],
            10,
            "0.060",
            "0.060",
        ),
    );
    assert!(f5_lines(&quiet).is_empty(), "{quiet}");
    assert!(!quiet.contains("re-runs cost"), "{quiet}");
}

#[test]
fn inherited_copy_variants_still_add_nothing() {
    let mut copied = finished(4, 2, "LINT", "01", "UBUNTU-LATEST");
    copied.started = Some("2026-04-01T00:00:00Z".to_string());
    copied.completed = Some("2026-04-01T00:10:00Z".to_string());
    copied.conclusion = Some("SUCCESS".to_string());
    copied.runner = None;
    let mut fixture = base(
        vec![copied, finished(1, 1, "lint", "01", "ubuntu-latest")],
        10,
        "0.060",
        "0.060",
    );
    fixture.reversed = true;
    fixture.crlf = true;
    let text = run_fixture("copy-variant", &fixture);
    assert!(f5_lines(&text).is_empty(), "{text}");
    assert!(text.contains("inherited copy"), "{text}");
}

#[test]
fn an_unlinkable_name_stays_visible_on_the_workflow_file() {
    let text = run_fixture(
        "unlinked",
        &base(
            vec![
                finished(7, 1, "custom-check", "01", "ubuntu-latest"),
                finished(8, 2, "custom-check", "02", "ubuntu-latest"),
            ],
            20,
            "0.120",
            "0.120",
        ),
    );
    let lines = f5_lines(&text);
    assert_eq!(lines.len(), 2, "{text}");
    assert!(
        text.contains("re-runs cost 0.06 USD (1 extra executions)"),
        "{text}"
    );
    assert!(text.contains("re-run of custom-check (job 8, attempt 2) costs 0.06 USD"));
    assert!(text.contains("ci.yml:1:"), "{text}");
    assert!(!text.contains("ci.yml:4: re-run of custom-check"), "{text}");
    assert!(!text.contains("ci.yml:9: re-run of custom-check"), "{text}");
}

#[test]
fn unlinkable_name_variants_stay_visible() {
    let mut extra = finished(8, 2, "Custom-Check", "02", "UBUNTU-LATEST");
    extra.conclusion = None;
    extra.runner = Some(String::new());
    extra.labels = Some(vec![]);
    let mut original = finished(7, 1, "custom-check", "01", "ubuntu-latest");
    original.runner = None;
    let mut fixture = base(vec![extra, original], 10, "0.060", "0.060");
    fixture.reversed = true;
    fixture.crlf = true;
    fixture.run_path = "acme/widgets/.github/workflows/ci.yml@refs/heads/topic";
    let text = run_fixture("unlinked-variant", &fixture);
    assert!(
        text.contains("re-run of Custom-Check (job 8, attempt 2) costs 0.00 USD"),
        "{text}"
    );
    assert!(
        text.contains("re-runs cost 0.00 USD (1 extra executions)"),
        "{text}"
    );
    assert!(text.contains("ci.yml:1:"), "{text}");
    assert!(!text.contains("Workflows without usage\nF5"), "{text}");
}
