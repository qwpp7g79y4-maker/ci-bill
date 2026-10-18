//! Overlap between two runs of one workflow on the same branch or pull request.
//!
//! A later run that starts before an earlier run finishes, when that earlier
//! run does not end as cancelled, is the cancel-in-progress saving. The amount
//! is the billed minutes the earlier jobs would have dropped, priced in USD,
//! on the `concurrency:` line or the `on:` line.

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
        .join("overlap")
        .join(name);
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

const ON_YAML: &str = "\
name: CI
on: push
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - run: echo test
";

const CONCURRENCY_YAML: &str = "\
name: CI
on: push
concurrency:
  group: ci
  cancel-in-progress: false
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - run: echo test
";

const EXACT: &str = "cancel-in-progress would save 0.012 USD (2 minutes)";

struct RunSpec {
    id: u64,
    path: String,
    event: String,
    conclusion: Option<String>,
    started: Option<String>,
    branch: Option<String>,
    /// `None` omits the field. `Some` writes the array, which may be empty.
    pulls: Option<Vec<u64>>,
    attempt: u64,
    number: u64,
}

struct JobSpec {
    id: u64,
    run_id: u64,
    name: String,
    labels: Vec<String>,
    started: String,
    completed: String,
    conclusion: String,
    attempt: u64,
}

struct Fixture {
    yaml: String,
    runs: Vec<RunSpec>,
    jobs: Vec<JobSpec>,
    crlf: bool,
    reversed: bool,
}

fn older_job() -> JobSpec {
    JobSpec {
        id: 10,
        run_id: 1,
        name: "test".to_string(),
        labels: vec!["ubuntu-latest".to_string()],
        started: "2026-04-01T00:00:00Z".to_string(),
        completed: "2026-04-01T00:03:00Z".to_string(),
        conclusion: "success".to_string(),
        attempt: 1,
    }
}

fn newer_job() -> JobSpec {
    JobSpec {
        id: 11,
        run_id: 2,
        name: "test".to_string(),
        labels: vec!["ubuntu-latest".to_string()],
        started: "2026-04-01T00:01:00Z".to_string(),
        completed: "2026-04-01T00:02:00Z".to_string(),
        conclusion: "success".to_string(),
        attempt: 1,
    }
}

fn run_spec(
    id: u64,
    conclusion: Option<&str>,
    started: &str,
    branch: Option<&str>,
    pulls: Option<Vec<u64>>,
) -> RunSpec {
    RunSpec {
        id,
        path: "acme/widgets/.github/workflows/ci.yml@main".to_string(),
        event: "pull_request".to_string(),
        conclusion: conclusion.map(str::to_string),
        started: Some(started.to_string()),
        branch: branch.map(str::to_string),
        pulls,
        attempt: 1,
        number: id,
    }
}

fn overlap_fixture(older_conclusion: &str, yaml: &str) -> Fixture {
    Fixture {
        yaml: yaml.to_string(),
        runs: vec![
            run_spec(
                1,
                Some(older_conclusion),
                "2026-04-01T00:00:00Z",
                Some("feature"),
                Some(vec![7]),
            ),
            run_spec(
                2,
                Some("success"),
                "2026-04-01T00:01:00Z",
                Some("feature"),
                Some(vec![7]),
            ),
        ],
        jobs: vec![older_job(), newer_job()],
        crlf: false,
        reversed: false,
    }
}

fn quote(text: &str) -> String {
    format!("\"{text}\"")
}

fn field(key: &str, value: &str) -> String {
    format!("\"{key}\": {value}")
}

fn run_json(spec: &RunSpec, reversed: bool) -> String {
    let mut fields = vec![
        field("id", &spec.id.to_string()),
        field("path", &quote(&spec.path)),
        field("run_attempt", &spec.attempt.to_string()),
        field("run_number", &spec.number.to_string()),
        field("event", &quote(&spec.event)),
    ];
    match &spec.conclusion {
        Some(conclusion) => fields.push(field("conclusion", &quote(conclusion))),
        None => fields.push(field("conclusion", "null")),
    }
    if let Some(started) = &spec.started {
        fields.push(field("run_started_at", &quote(started)));
    }
    if let Some(branch) = &spec.branch {
        fields.push(field("head_branch", &quote(branch)));
    }
    if let Some(pulls) = &spec.pulls {
        let items = pulls
            .iter()
            .map(|number| format!("{{\"number\": {number}}}"))
            .collect::<Vec<_>>()
            .join(", ");
        fields.push(field("pull_requests", &format!("[{items}]")));
    }
    if reversed {
        fields.reverse();
    }
    format!("{{{}}}", fields.join(", "))
}

fn job_json(spec: &JobSpec, reversed: bool) -> String {
    let labels = spec
        .labels
        .iter()
        .map(|label| quote(label))
        .collect::<Vec<_>>()
        .join(", ");
    let mut fields = vec![
        field("id", &spec.id.to_string()),
        field("run_id", &spec.run_id.to_string()),
        field("run_attempt", &spec.attempt.to_string()),
        field("name", &quote(&spec.name)),
        field("labels", &format!("[{labels}]")),
        field("started_at", &quote(&spec.started)),
        field("completed_at", &quote(&spec.completed)),
        field("conclusion", &quote(&spec.conclusion)),
        field("runner_name", &quote("runner-a")),
    ];
    if reversed {
        fields.reverse();
    }
    format!("{{{}}}", fields.join(", "))
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

    let runs_body = fixture
        .runs
        .iter()
        .map(|spec| run_json(spec, fixture.reversed))
        .collect::<Vec<_>>()
        .join(", ");
    let mut runs = format!("{{\"workflow_runs\": [{runs_body}]}}");
    let mut jobs_by_run: Vec<(u64, String)> = Vec::new();
    for spec in &fixture.jobs {
        if let Some(entry) = jobs_by_run.iter_mut().find(|item| item.0 == spec.run_id) {
            entry.1.push_str(", ");
            entry.1.push_str(&job_json(spec, fixture.reversed));
        } else {
            jobs_by_run.push((spec.run_id, job_json(spec, fixture.reversed)));
        }
    }
    let usage = "date,product,sku,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,net_amount,username,organization,repository,workflow_path\n2026-04-01,actions,actions_linux,100,minutes,0.006,0.600000,0,0.600000,dev,acme,acme/widgets,.github/workflows/ci.yml\n";
    let mut usage = usage.to_string();
    if fixture.crlf {
        runs = runs.replace(',', ",\r\n");
        usage = usage.replace('\n', "\r\n");
    }
    std::fs::write(dir.join("runs-1.json"), runs).unwrap();
    for (run_id, body) in jobs_by_run {
        let mut text = format!("{{\"jobs\": [{body}]}}");
        if fixture.crlf {
            text = text.replace(',', ",\r\n");
        }
        std::fs::write(dir.join(format!("jobs-{run_id}.json")), text).unwrap();
    }
    std::fs::write(dir.join("usage.csv"), usage).unwrap();
}

fn run_case(name: &str, fixture: &Fixture) -> String {
    let dir = scratch(name);
    write_fixture(&dir, fixture);
    let output = report(&[
        "report",
        "--usage",
        dir.join("usage.csv").to_str().unwrap(),
        "--workflows",
        dir.join("workflows").to_str().unwrap(),
        "--runs",
        dir.to_str().unwrap(),
    ]);
    assert_exit(&output, 0);
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
    let text = stdout(&output);
    assert_plain(&text);
    text
}

fn json_case(name: &str, fixture: &Fixture) -> serde_json::Value {
    let dir = scratch(name);
    write_fixture(&dir, fixture);
    let output = report(&[
        "report",
        "--usage",
        dir.join("usage.csv").to_str().unwrap(),
        "--workflows",
        dir.join("workflows").to_str().unwrap(),
        "--runs",
        dir.to_str().unwrap(),
        "--json",
    ]);
    assert_exit(&output, 0);
    let text = stdout(&output);
    assert_plain(&text);
    serde_json::from_str(text.trim_end()).expect("json")
}

fn savings(value: &serde_json::Value) -> Vec<&serde_json::Value> {
    let mut found = Vec::new();
    if let Some(workflows) = value["workflows"].as_array() {
        for workflow in workflows {
            if let Some(items) = workflow["findings"].as_array() {
                found.extend(items.iter().filter(|item| item["id"] == "F3$"));
            }
        }
    }
    if let Some(items) = value["unattached_findings"].as_array() {
        found.extend(items.iter().filter(|item| item["id"] == "F3$"));
    }
    found
}

#[test]
fn an_overlapping_run_reports_the_exact_saving_on_the_on_line() {
    let text = run_case("overlap", &overlap_fixture("success", ON_YAML));
    assert!(text.contains(&format!("ci.yml:2: {EXACT}")), "{text}");
    assert!(!text.contains("Workflows without usage"), "{text}");
    let value = json_case("overlap-json", &overlap_fixture("success", ON_YAML));
    let found = savings(&value);
    assert_eq!(found.len(), 1, "{value}");
    assert_eq!(found[0]["line"], 2);
    assert_eq!(found[0]["message"], EXACT);
    assert_eq!(found[0]["id"], "F3$");
}

#[test]
fn a_cancelled_older_run_reports_no_amount() {
    for (name, conclusion) in [
        ("cancelled", "cancelled"),
        ("cancelled-caps", "CANCELLED"),
        ("cancelled-mixed", "Cancelled"),
    ] {
        let text = run_case(name, &overlap_fixture(conclusion, ON_YAML));
        assert!(!text.contains("F3$"), "{name}: {text}");
        assert!(!text.contains("would save"), "{name}: {text}");
        let value = json_case(
            &format!("{name}-json"),
            &overlap_fixture(conclusion, ON_YAML),
        );
        assert!(savings(&value).is_empty(), "{name}: {value}");
    }
}

#[test]
fn crlf_reversed_fields_and_capitals_keep_the_exact_amount() {
    let mut fixture = overlap_fixture("SUCCESS", ON_YAML);
    fixture.crlf = true;
    fixture.reversed = true;
    fixture.runs[0].path = "ACME/WIDGETS/.GitHub/Workflows/CI.yml@MAIN".to_string();
    fixture.runs[1].path = "acme/widgets/.github/workflows/ci.yml@refs/heads/main".to_string();
    fixture.runs[0].event = "PULL_REQUEST".to_string();
    fixture.runs[1].event = "Pull_Request".to_string();
    fixture.runs[0].branch = Some("FEATURE".to_string());
    fixture.runs[1].branch = Some("feature".to_string());
    fixture.jobs[0].labels = vec!["UBUNTU-LATEST".to_string()];
    fixture.jobs[0].conclusion = "SUCCESS".to_string();
    fixture.jobs.reverse();
    fixture.runs.reverse();
    let text = run_case("variants", &fixture);
    assert!(text.contains(&format!("ci.yml:2: {EXACT}")), "{text}");
}

#[test]
fn missing_identity_fields_have_no_amount_and_a_pull_request_still_matches() {
    let mut missing = overlap_fixture("success", ON_YAML);
    missing.runs[0].branch = None;
    missing.runs[1].branch = None;
    missing.runs[0].pulls = None;
    missing.runs[1].pulls = None;
    let text = run_case("missing-identity", &missing);
    assert!(!text.contains("F3$"), "{text}");

    let mut empty_branch = overlap_fixture("success", ON_YAML);
    empty_branch.runs[0].branch = Some(String::new());
    empty_branch.runs[1].branch = Some("   ".to_string());
    empty_branch.runs[0].pulls = Some(vec![]);
    empty_branch.runs[1].pulls = Some(vec![]);
    empty_branch.runs[0].conclusion = None;
    let text = run_case("empty-identity", &empty_branch);
    assert!(!text.contains("F3$"), "{text}");

    let mut by_pr = overlap_fixture("success", ON_YAML);
    by_pr.runs[0].branch = Some(String::new());
    by_pr.runs[1].branch = None;
    by_pr.runs[0].pulls = Some(vec![12]);
    by_pr.runs[1].pulls = Some(vec![12]);
    let text = run_case("pull-request", &by_pr);
    assert!(text.contains(EXACT), "{text}");

    let mut other_branch = overlap_fixture("success", ON_YAML);
    other_branch.runs[1].branch = Some("other".to_string());
    other_branch.runs[1].pulls = Some(vec![99]);
    let text = run_case("other-branch", &other_branch);
    assert!(!text.contains("F3$"), "{text}");
}

#[test]
fn the_amount_sits_on_the_concurrency_line_when_that_key_exists() {
    let text = run_case("concurrency", &overlap_fixture("success", CONCURRENCY_YAML));
    assert!(text.contains(&format!("ci.yml:3: {EXACT}")), "{text}");
    assert!(!text.contains("ci.yml:2:"), "{text}");
}
