//! Invoice reconciliation: job costs sum to the CSV row.

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

fn scratch(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("reconcile")
        .join(name);
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn money(text: &str) -> i64 {
    let negative = text.starts_with('-');
    let text = text.trim_start_matches('-');
    let (whole, frac) = text.split_once('.').expect("decimal");
    assert_eq!(frac.len(), 6, "{text}");
    let value = whole.parse::<i64>().unwrap() * 1_000_000 + frac.parse::<i64>().unwrap();
    if negative {
        -value
    } else {
        value
    }
}

fn write(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).unwrap();
}

const HEADER: &str =
    "date,product,sku,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,net_amount,username,organization,repository,workflow_path";

fn job(id: u64, name: &str, labels: &str, start: &str, end: &str, run_id: u64) -> String {
    format!(
        r#"{{"id":{id},"run_id":{run_id},"run_attempt":1,"name":"{name}","labels":[{labels}],"started_at":"{start}","completed_at":"{end}","conclusion":"success","runner_name":"runner-a"}}"#
    )
}

fn runs_body(path: &str, run_id: u64) -> String {
    format!(
        r#"{{"workflow_runs":[{{"id":{run_id},"path":"{path}","run_attempt":1,"run_number":1,"event":"push","conclusion":"success","run_started_at":"2026-04-01T00:00:00Z"}}]}}"#
    )
}

fn run_report(dir: &Path, extra: &[&str]) -> serde_json::Value {
    let usage = dir.join("usage.csv");
    let mut args = vec![
        "report",
        "--usage",
        usage.to_str().unwrap(),
        "--runs",
        dir.to_str().unwrap(),
        "--json",
    ];
    args.extend_from_slice(extra);
    let output = report(&args);
    assert_exit(&output, 0);
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        !text.to_ascii_lowercase().contains("not supported"),
        "{text}"
    );
    assert!(!text.to_ascii_lowercase().contains("skipped"), "{text}");
    serde_json::from_str(text.trim_end()).expect("json")
}

fn job_amounts(value: &serde_json::Value) -> Vec<(u64, i64, String)> {
    value["workflows"][0]["job_costs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|job| {
            (
                job["job_id"].as_u64().unwrap(),
                money(job["amount"].as_str().unwrap()),
                job["method"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn assert_three_jobs_sum(value: &serde_json::Value) {
    let amounts = job_amounts(value);
    assert_eq!(amounts.len(), 3, "{value}");
    assert!(
        amounts
            .iter()
            .all(|(_, _, method)| method == "billed_minutes"),
        "{amounts:?}"
    );
    let mut by_id = amounts.clone();
    by_id.sort_by_key(|(id, _, _)| *id);
    // 1 min, 1 min, 3 min against 100001 micros. Remainder 1 goes to the 3 minute job.
    assert_eq!(by_id[0].1, 20_000, "{by_id:?}");
    assert_eq!(by_id[1].1, 20_000, "{by_id:?}");
    assert_eq!(by_id[2].1, 60_001, "{by_id:?}");
    let sum: i64 = by_id.iter().map(|(_, amount, _)| *amount).sum();
    assert_eq!(sum, 100_001);
    assert_eq!(
        money(value["workflows"][0]["invoice"]["amount"].as_str().unwrap()),
        100_001
    );
    assert_eq!(value["workflows"][0]["invoice"]["method"], "billed_minutes");
    assert_eq!(value["workflows"][0]["measured"]["method"], "list_price");
    // 5 billed minutes * 0.006 = 0.030000
    assert_eq!(value["workflows"][0]["measured"]["amount"], "0.030000");
    assert_eq!(value["workflows"][0]["difference"]["amount"], "0.070001");
    assert_eq!(
        value["workflows"][0]["difference"]["method"],
        "billed_minutes"
    );
    let runs = value["workflow_runs"][0]["jobs"].as_array().unwrap();
    for job in runs {
        assert_eq!(job["method"], "billed_minutes");
        assert_eq!(job["list_price_method"], "list_price");
        assert!(job["list_price"].is_string());
        assert!(job["invoice"].is_string());
    }
}

#[test]
fn three_jobs_sum_exactly_to_the_csv_row() {
    let dir = scratch("three");
    let row = "2026-04-01,actions,actions_linux,5,minutes,0.006,0.030,0,0.100001,dev,acme,acme/app,.github/workflows/ci.yml";
    write(&dir, "usage.csv", &format!("{HEADER}\n{row}\n"));
    write(
        &dir,
        "runs-1.json",
        &runs_body("acme/app/.github/workflows/ci.yml@main", 7),
    );
    let jobs = format!(
        "{{\"jobs\":[{},{},{}]}}",
        job(
            1,
            "small",
            "\"ubuntu-latest\"",
            "2026-04-01T00:00:00Z",
            "2026-04-01T00:01:00Z",
            7
        ),
        job(
            2,
            "mid",
            "\"ubuntu-latest\"",
            "2026-04-01T00:00:00Z",
            "2026-04-01T00:01:00Z",
            7
        ),
        job(
            3,
            "large",
            "\"ubuntu-latest\"",
            "2026-04-01T00:00:00Z",
            "2026-04-01T00:03:00Z",
            7
        ),
    );
    write(&dir, "jobs-7.json", &jobs);
    let value = run_report(&dir, &[]);
    assert_three_jobs_sum(&value);
}

#[test]
fn three_jobs_keep_the_sum_for_crlf_order_case_and_empty_fields() {
    let dir = scratch("three-variants");
    let header = "workflow_path,repository,organization,username,net_amount,discount_amount,gross_amount,applied_cost_per_quantity,unit_type,quantity,sku,product,date";
    let row = ".github/workflows/ci.yml,Acme/App,,,0.100001,0,0.030,0.006,Minutes,5,Actions_Linux,Actions,2026-04-01";
    write(&dir, "usage.csv", &format!("{header}\r\n{row}\r\n"));
    write(
        &dir,
        "runs-a.json",
        "{\r\n\"workflow_runs\": [\r\n{\r\n\"run_started_at\": \"2026-04-01T00:00:00Z\",\r\n\"conclusion\": \"success\",\r\n\"event\": \"push\",\r\n\"run_number\": 4,\r\n\"run_attempt\": 1,\r\n\"path\": \"Acme/App/.github/workflows/ci.yml@refs/heads/main\",\r\n\"id\": 7\r\n}\r\n]\r\n}\r\n",
    );
    // Largest job is first. Optional fields are null or absent. Pages are split.
    let large = "{\r\n\"completed_at\": \"2026-04-01T00:03:00Z\",\r\n\"started_at\": \"2026-04-01T00:00:00Z\",\r\n\"labels\": [\"UBUNTU-LATEST\"],\r\n\"name\": \"large\",\r\n\"run_id\": 7,\r\n\"run_attempt\": 1,\r\n\"id\": 3,\r\n\"conclusion\": null\r\n}";
    let small = "{\r\n\"id\": 1,\r\n\"name\": \"small\",\r\n\"labels\": [\"ubuntu-latest\"],\r\n\"run_id\": 7,\r\n\"run_attempt\": 1,\r\n\"started_at\": \"2026-04-01T00:00:00Z\",\r\n\"completed_at\": \"2026-04-01T00:01:00Z\",\r\n\"conclusion\": \"success\"\r\n}";
    let mid = "{\r\n\"runner_name\": \"\",\r\n\"conclusion\": \"SUCCESS\",\r\n\"completed_at\": \"2026-04-01T00:01:00Z\",\r\n\"started_at\": \"2026-04-01T00:00:00Z\",\r\n\"labels\": [\"ubuntu-22.04\"],\r\n\"name\": \"mid\",\r\n\"run_attempt\": 1,\r\n\"run_id\": 7,\r\n\"id\": 2\r\n}";
    write(&dir, "jobs-7.json", &format!("{{\"jobs\":[{large}]}}\r\n"));
    write(
        &dir,
        "jobs-7-page.json",
        &format!("{{\"jobs\":[{small},{mid}]}}\n"),
    );
    let value = run_report(&dir, &[]);
    assert_three_jobs_sum(&value);
    let text = {
        let usage = dir.join("usage.csv");
        let output = report(&[
            "report",
            "--usage",
            usage.to_str().unwrap(),
            "--runs",
            dir.to_str().unwrap(),
        ]);
        assert_exit(&output, 0);
        stdout(&output)
    };
    assert!(
        text.contains("measured 0.03  invoice 0.10  difference 0.07"),
        "{text}"
    );
    assert!(text.contains("large  job 3 = 0.06"), "{text}");
    assert!(text.contains("small  job 1 = 0.02"), "{text}");
    assert!(text.contains("mid  job 2 = 0.02"), "{text}");
}

#[test]
fn jobs_without_a_csv_row_keep_list_price() {
    let dir = scratch("list-price");
    let row = "2026-04-01,actions,actions_linux,1,minutes,0.006,0.006,0,0.006,dev,acme,acme/app,.github/workflows/ci.yml";
    write(&dir, "usage.csv", &format!("{HEADER}\n{row}\n"));
    write(
        &dir,
        "runs-1.json",
        &format!(
            "{{\"workflow_runs\":[{},{}]}}",
            r#"{"id":1,"path":".github/workflows/ci.yml","run_attempt":1,"run_number":1,"event":"push","conclusion":"success","run_started_at":"2026-04-01T00:00:00Z"}"#,
            r#"{"id":2,"path":".github/workflows/lint.yml@main","run_attempt":1,"run_number":2,"event":"push","conclusion":"success","run_started_at":"2026-04-01T00:00:00Z"}"#
        ),
    );
    let ci = job(
        1,
        "build",
        "\"ubuntu-latest\"",
        "2026-04-01T00:00:00Z",
        "2026-04-01T00:01:00Z",
        1,
    );
    let windows = job(
        2,
        "pack",
        "\"windows-latest\"",
        "2026-04-01T00:00:00Z",
        "2026-04-01T00:02:00Z",
        1,
    );
    let lint = job(
        3,
        "lint",
        "\"macos-latest\"",
        "2026-04-01T00:00:00Z",
        "2026-04-01T00:02:00Z",
        2,
    );
    write(
        &dir,
        "jobs-1.json",
        &format!("{{\"jobs\":[{ci},{windows}]}}"),
    );
    write(&dir, "jobs-2.json", &format!("{{\"jobs\":[{lint}]}}"));
    let value = run_report(&dir, &[]);
    let ci_workflow = value["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["path"] == ".github/workflows/ci.yml")
        .unwrap();
    let costs = ci_workflow["job_costs"].as_array().unwrap();
    let build = costs.iter().find(|job| job["name"] == "build").unwrap();
    let pack = costs.iter().find(|job| job["name"] == "pack").unwrap();
    assert_eq!(build["method"], "billed_minutes");
    assert_eq!(build["amount"], "0.006000");
    assert_eq!(pack["method"], "list_price");
    assert_eq!(pack["amount"], "0.020000");
    assert_eq!(pack["list_price"].as_str(), None);
    let runs = value["workflow_runs"].as_array().unwrap();
    let lint_job = runs
        .iter()
        .find(|run| run["path"] == ".github/workflows/lint.yml")
        .unwrap()["jobs"][0]
        .clone();
    assert_eq!(lint_job["method"], "list_price");
    assert_eq!(lint_job["invoice"], "0.124000");
    assert_eq!(lint_job["list_price"], "0.124000");
    assert_eq!(lint_job["list_price_method"], "list_price");
}

#[test]
fn two_repositories_scale_separately() {
    let dir = scratch("two-repos");
    let header = HEADER;
    let left = "2026-04-01,actions,actions_linux,1,minutes,0.006,0.006,0,0.010000,,,acme/left,.github/workflows/ci.yml";
    let right = "2026-04-01,actions,actions_linux,1,minutes,0.006,0.006,0,0.020000,dev,acme,acme/right,.github/workflows/ci.yml";
    write(&dir, "usage.csv", &format!("{header}\n{left}\n{right}\n"));
    write(
        &dir,
        "runs-1.json",
        r#"{"workflow_runs":[
            {"id":1,"path":"acme/left/.github/workflows/ci.yml@main","run_attempt":1,"run_number":1,"event":"push","conclusion":"success","run_started_at":"2026-04-01T00:00:00Z"},
            {"id":2,"path":"acme/right/.github/workflows/CI.yml@refs/heads/main","run_attempt":1,"run_number":1,"event":"push","conclusion":"success","run_started_at":"2026-04-01T00:00:00Z"}
        ]}"#,
    );
    let left_job = job(
        1,
        "build",
        "\"ubuntu-latest\"",
        "2026-04-01T00:00:00Z",
        "2026-04-01T00:01:00Z",
        1,
    );
    let right_job = job(
        2,
        "build",
        "\"ubuntu-latest\"",
        "2026-04-01T00:00:00Z",
        "2026-04-01T00:01:00Z",
        2,
    );
    write(&dir, "jobs-1.json", &format!("{{\"jobs\":[{left_job}]}}"));
    write(&dir, "jobs-2.json", &format!("{{\"jobs\":[{right_job}]}}"));
    let value = run_report(&dir, &[]);
    let workflows = value["workflows"].as_array().unwrap();
    assert_eq!(workflows.len(), 2);
    for workflow in workflows {
        let amount = workflow["job_costs"][0]["amount"].as_str().unwrap();
        let invoice = workflow["invoice"]["amount"].as_str().unwrap();
        assert_eq!(amount, invoice, "{workflow}");
        assert_eq!(workflow["job_costs"][0]["method"], "billed_minutes");
    }
    let left_amount = workflows
        .iter()
        .find(|item| item["repository"] == "acme/left")
        .unwrap()["job_costs"][0]["amount"]
        .as_str()
        .unwrap();
    let right_amount = workflows
        .iter()
        .find(|item| item["repository"] == "acme/right")
        .unwrap()["job_costs"][0]["amount"]
        .as_str()
        .unwrap();
    assert_eq!(left_amount, "0.010000");
    assert_eq!(right_amount, "0.020000");
}

const LINT_AND_TEST: &str = "\
name: CI
on: push
jobs:
  lint:
    runs-on: macos-latest
    steps:
      - run: cargo clippy -- -D warnings
  test:
    runs-on: ubuntu-latest
    steps:
      - run: cargo test
";

fn yaml_report(dir: &Path, workflow: &str, csv: &str) -> (String, serde_json::Value) {
    let workflows = dir.join("workflows");
    std::fs::create_dir_all(&workflows).unwrap();
    write(&workflows, "ci.yml", workflow);
    write(dir, "usage.csv", csv);
    let usage = dir.join("usage.csv");
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
    let json = report(&[
        "report",
        "--usage",
        usage.to_str().unwrap(),
        "--workflows",
        workflows.to_str().unwrap(),
        "--json",
    ]);
    assert_exit(&json, 0);
    let body = stdout(&json);
    let value: serde_json::Value = serde_json::from_str(body.trim_end()).unwrap();
    (text, value)
}

fn assert_lint_and_test(text: &str, value: &serde_json::Value) {
    let jobs = value["workflows"][0]["job_costs"].as_array().unwrap();
    assert_eq!(jobs.len(), 2, "{value}");
    let lint = jobs.iter().find(|job| job["name"] == "lint").unwrap();
    let test = jobs.iter().find(|job| job["name"] == "test").unwrap();
    assert_eq!(lint["method"], "yaml_weight");
    assert_eq!(test["method"], "yaml_weight");
    assert_eq!(lint["sku"], "actions_macos");
    assert_eq!(test["sku"], "actions_linux");
    assert_eq!(lint["amount"], "0.620001");
    assert_eq!(test["amount"], "1.000000");
    assert!(lint["file"].as_str().unwrap().ends_with("ci.yml"), "{lint}");
    assert!(lint["line"].as_u64().unwrap() > 0);
    assert!(test["line"].as_u64().unwrap() >= lint["line"].as_u64().unwrap());
    let sum = money(lint["amount"].as_str().unwrap()) + money(test["amount"].as_str().unwrap());
    assert_eq!(sum, money(value["workflows"][0]["net"].as_str().unwrap()));
    assert_eq!(value["workflows"][0]["invoice"]["method"], "yaml_weight");
    assert_eq!(value["workflows"][0]["invoice"]["amount"], "1.620001");
    assert_eq!(value["workflows"][0]["measured"]["method"], "list_price");
    assert_eq!(value["workflows"][0]["measured"]["amount"], "0.000000");
    assert_eq!(value["workflows"][0]["difference"]["method"], "yaml_weight");
    assert_eq!(value["workflows"][0]["difference"]["amount"], "1.620001");
    assert!(text.contains("lint"), "{text}");
    assert!(text.contains("= 0.62"), "{text}");
    assert!(text.contains("= 1.00"), "{text}");
    assert!(
        text.contains("measured 0.00  invoice 1.62  difference 1.62"),
        "{text}"
    );
}

#[test]
fn yaml_weight_splits_one_macos_lint_and_one_linux_test() {
    let dir = scratch("yaml");
    let macos = "2026-05-01,actions,actions_macos,10,minutes,0.062,0.620,0,0.620001,dev,acme,acme/app,.github/workflows/ci.yml";
    let linux = "2026-05-01,actions,actions_linux,1,minutes,0.006,0.006,0,1.000000,dev,acme,acme/app,.github/workflows/ci.yml";
    let csv = format!("{HEADER}\n{macos}\n{linux}\n");
    let (text, value) = yaml_report(&dir, LINT_AND_TEST, &csv);
    assert_lint_and_test(&text, &value);
    let lint = value["workflows"][0]["job_costs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["name"] == "lint")
        .unwrap();
    assert_eq!(lint["line"], 5);
    let test = value["workflows"][0]["job_costs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["name"] == "test")
        .unwrap();
    assert_eq!(test["line"], 9);
}

#[test]
fn yaml_weight_accepts_crlf_flow_style_uppercase_and_reordered_columns() {
    let dir = scratch("yaml-variants");
    let workflow = "name: CI\r\non: [push]\r\njobs: {lint: {runs-on: MACOS-LATEST, steps: [{run: cargo clippy -- -D warnings}]}, test: {runs-on: UBUNTU-LATEST, steps: [{run: cargo test}]}}\r\n";
    let header = "repository,workflow_path,username,organization,net_amount,discount_amount,gross_amount,applied_cost_per_quantity,unit_type,quantity,sku,product,date";
    let macos = "ACME/APP,.GitHub/Workflows/CI.yml,,,0.620001,0,0.620,0.062,MINUTES,10,ACTIONS_MACOS,ACTIONS,2026-05-02";
    let linux = "ACME/APP,.GitHub/Workflows/CI.yml,,,1.000000,0,0.006,0.006,minutes,1,actions_linux,actions,2026-05-01";
    let csv = format!("{header}\r\n{linux}\r\n{macos}\r\n");
    let (text, value) = yaml_report(&dir, workflow, &csv);
    assert_lint_and_test(&text, &value);
    assert_eq!(value["workflows"].as_array().unwrap().len(), 1, "{value}");
}

#[test]
fn sample_shows_the_lint_job_at_lint_yml_line_8() {
    let output = report(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--workflows",
        "samples/.github/workflows",
    ]);
    assert_exit(&output, 0);
    let text = stdout(&output);
    assert!(
        text.contains("samples/.github/workflows/lint.yml:8 = 230.14"),
        "{text}"
    );
    assert!(
        text.contains("measured 0.00  invoice 230.14  difference 230.14"),
        "{text}"
    );
    let json = report(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--workflows",
        "samples/.github/workflows",
        "--json",
    ]);
    assert_exit(&json, 0);
    let body = stdout(&json);
    assert!(!body.contains("0000000001"), "{body}");
    assert!(!body.contains("9999999"), "{body}");
    let value: serde_json::Value = serde_json::from_str(body.trim_end()).unwrap();
    let lint = value["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["path"] == ".github/workflows/lint.yml")
        .unwrap();
    assert_eq!(lint["job_costs"].as_array().unwrap().len(), 1);
    let job = &lint["job_costs"][0];
    assert_eq!(job["name"], "lint");
    assert_eq!(job["line"], 8);
    assert_eq!(job["method"], "yaml_weight");
    assert_eq!(job["sku"], "actions_macos");
    assert_eq!(job["amount"], "230.144000");
    assert!(job["file"].as_str().unwrap().ends_with("lint.yml"));
    assert_eq!(lint["invoice"]["amount"], "230.144000");
    assert_eq!(lint["invoice"]["method"], "yaml_weight");
    assert_eq!(lint["measured"]["amount"], "0.000000");
    assert_eq!(lint["measured"]["method"], "list_price");
    assert_eq!(lint["difference"]["amount"], "230.144000");
    assert_eq!(lint["difference"]["method"], "yaml_weight");
    let ci = value["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["path"] == ".github/workflows/ci.yml")
        .unwrap();
    assert_eq!(ci["job_costs"][0]["name"], "test");
    assert_eq!(ci["job_costs"][0]["line"], 9);
    assert_eq!(ci["job_costs"][0]["amount"], "44.232000");
    assert_eq!(ci["job_costs"][0]["method"], "yaml_weight");
}
