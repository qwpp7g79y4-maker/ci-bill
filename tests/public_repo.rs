//! Public repositories invoice standard runners at zero and keep the list price.

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
        .join("public-repo")
        .join(name);
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn write(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).unwrap();
}

fn money(text: &str) -> i64 {
    let (whole, frac) = text.split_once('.').expect("decimal");
    assert_eq!(frac.len(), 6, "{text}");
    whole.parse::<i64>().unwrap() * 1_000_000 + frac.parse::<i64>().unwrap()
}

const HEADER: &str =
    "date,product,sku,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,net_amount,username,organization,repository,workflow_path";

fn other_usage() -> String {
    format!(
        "{HEADER}\n2026-04-01,actions,actions_linux,1,minutes,0.006,0.006,0,0.006,dev,acme,acme/other,.github/workflows/other.yml\n"
    )
}

fn runs_json(path: &str) -> String {
    format!(
        r#"{{"workflow_runs":[{{"id":1,"path":"{path}","run_attempt":1,"run_number":1,"event":"push","conclusion":"success","run_started_at":"2026-04-01T00:00:00Z"}}]}}"#
    )
}

fn job(id: u64, name: &str, label: &str) -> String {
    format!(
        r#"{{"id":{id},"run_id":1,"run_attempt":1,"name":"{name}","labels":["{label}"],"started_at":"2026-04-01T00:00:00Z","completed_at":"2026-04-01T00:01:00Z","conclusion":"success","runner_name":"runner-a"}}"#
    )
}

fn standard_jobs() -> String {
    format!(
        "{{\"jobs\":[{},{},{},{},{}]}}",
        job(1, "lint", "macos-latest"),
        job(2, "wide", "ubuntu-22.04-4core"),
        job(3, "build", "ubuntu-latest"),
        job(4, "pack", "windows-latest"),
        job(5, "big", "macos-14-large"),
    )
}

fn write_public_fixture(dir: &Path, repo: &str, runs: &str, jobs: &str, usage: &str) {
    write(dir, "repo.json", repo);
    write(dir, "runs-1.json", runs);
    write(dir, "jobs-1.json", jobs);
    write(dir, "usage.csv", usage);
}

fn run_json(dir: &Path) -> (String, serde_json::Value) {
    let usage = dir.join("usage.csv");
    let output = report(&[
        "report",
        "--usage",
        usage.to_str().unwrap(),
        "--runs",
        dir.to_str().unwrap(),
        "--json",
    ]);
    assert_exit(&output, 0);
    let body = stdout(&output);
    assert_clean(&body);
    assert_clean(&stderr(&output));
    let value = serde_json::from_str(body.trim_end()).unwrap();
    (body, value)
}

fn run_text(dir: &Path) -> String {
    let usage = dir.join("usage.csv");
    let output = report(&[
        "report",
        "--usage",
        usage.to_str().unwrap(),
        "--runs",
        dir.to_str().unwrap(),
    ]);
    assert_exit(&output, 0);
    let text = stdout(&output);
    assert_clean(&text);
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
    text
}

fn assert_clean(text: &str) {
    let lower = text.to_ascii_lowercase();
    assert!(!lower.contains("not supported"), "{text}");
    assert!(!lower.contains("skipped"), "{text}");
    assert!(!lower.contains("limitation"), "{text}");
}

fn finished_job<'a>(value: &'a serde_json::Value, sku: &str) -> &'a serde_json::Value {
    value["workflow_runs"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|run| run["jobs"].as_array().unwrap())
        .find(|job| job["sku"] == sku)
        .unwrap_or_else(|| panic!("missing {sku} in {value}"))
}

fn line_with<'a>(text: &'a str, sku: &str) -> &'a str {
    text.lines()
        .find(|line| line.split_whitespace().any(|word| word == sku))
        .unwrap_or_else(|| panic!("missing {sku} in {text}"))
}

fn assert_public_job_prices(text: &str, value: &serde_json::Value) {
    let macos = finished_job(value, "actions_macos");
    assert_eq!(macos["invoice"], "0.000000");
    assert_eq!(macos["list_price"], "0.062000");
    assert_eq!(macos["method"], "list_price");
    assert_eq!(macos["minutes"], 1);
    let macos_line = line_with(text, "actions_macos");
    assert!(macos_line.contains("list 0.06"), "{macos_line}");
    assert!(macos_line.contains("invoice 0.00"), "{macos_line}");

    let wide = finished_job(value, "actions_linux_4_core");
    assert_eq!(wide["invoice"], "0.012000");
    assert_eq!(wide["list_price"], "0.012000");
    assert!(money(wide["invoice"].as_str().unwrap()) > 0);
    let wide_line = line_with(text, "actions_linux_4_core");
    assert!(wide_line.contains("list 0.01"), "{wide_line}");
    assert!(wide_line.contains("invoice 0.01"), "{wide_line}");

    let linux = finished_job(value, "actions_linux");
    assert_eq!(linux["invoice"], "0.000000");
    assert_eq!(linux["list_price"], "0.006000");
    let windows = finished_job(value, "actions_windows");
    assert_eq!(windows["invoice"], "0.000000");
    assert_eq!(windows["list_price"], "0.010000");
    let large = finished_job(value, "actions_macos_l");
    assert_eq!(large["invoice"], "0.077000");
    assert_eq!(large["list_price"], "0.077000");
    assert!(money(large["invoice"].as_str().unwrap()) > 0);
}

fn assert_list_price_invoice(value: &serde_json::Value) {
    let macos = finished_job(value, "actions_macos");
    assert_eq!(macos["invoice"], macos["list_price"]);
    assert_eq!(macos["list_price"], "0.062000");
    let wide = finished_job(value, "actions_linux_4_core");
    assert_eq!(wide["invoice"], "0.012000");
    assert_eq!(wide["list_price"], "0.012000");
}

#[test]
fn public_repo_zeros_standard_sku_invoice_and_keeps_list_price() {
    let dir = scratch("public");
    write_public_fixture(
        &dir,
        "{\"private\":false,\"default_branch\":\"main\"}\n",
        &runs_json("acme/app/.github/workflows/ci.yml@main"),
        &standard_jobs(),
        &other_usage(),
    );
    let text = run_text(&dir);
    let (_, value) = run_json(&dir);
    assert_public_job_prices(&text, &value);
    let other = value["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|workflow| workflow["path"] == ".github/workflows/other.yml")
        .unwrap();
    assert_eq!(other["invoice"]["amount"], "0.006000");
    assert_eq!(other["net"], "0.006000");
}

#[test]
fn public_repo_crlf_reordered_keys_empty_fields_and_capitals() {
    let dir = scratch("variants");
    let mut repo = vec![0xEF, 0xBB, 0xBF];
    repo.extend_from_slice(
        b"{\r\n\"description\": \"\",\r\n\"default_branch\": \"\",\r\n\"Private\": \"FALSE\"\r\n}\r\n",
    );
    std::fs::write(dir.join("repo.json"), repo).unwrap();
    write(
        &dir,
        "runs-1.json",
        "{\r\n\"total_count\": 1,\r\n\"workflow_runs\": [\r\n{\r\n\"event\": \"push\",\r\n\"conclusion\": null,\r\n\"run_number\": 9,\r\n\"run_started_at\": \"2026-04-01T00:00:00Z\",\r\n\"path\": \"ACME/APP/.github/workflows/CI.yml@MAIN\",\r\n\"run_attempt\": 1,\r\n\"id\": 1\r\n}\r\n]\r\n}\r\n",
    );
    // Keys are not in a fixed order. Blank, missing, and null fields are included.
    let jobs = "{\r\n\"jobs\": [\r\n{\r\n\"conclusion\": null,\r\n\"completed_at\": \"2026-04-01T00:01:00Z\",\r\n\"started_at\": \"2026-04-01T00:00:00Z\",\r\n\"labels\": [\"MacOS-Latest\"],\r\n\"name\": \"lint\",\r\n\"run_attempt\": 1,\r\n\"run_id\": 1,\r\n\"id\": 1\r\n},\r\n{\r\n\"runner_name\": \"\",\r\n\"id\": 2,\r\n\"name\": \"wide\",\r\n\"labels\": [\"UBUNTU-22.04-4CORE\"],\r\n\"run_id\": 1,\r\n\"run_attempt\": 1,\r\n\"started_at\": \"2026-04-01T00:00:00Z\",\r\n\"completed_at\": \"2026-04-01T00:01:00Z\",\r\n\"conclusion\": \"SUCCESS\"\r\n},\r\n{\r\n\"id\": 3,\r\n\"run_id\": 1,\r\n\"run_attempt\": 1,\r\n\"name\": \"build\",\r\n\"labels\": [\"Ubuntu-Latest\"],\r\n\"started_at\": \"2026-04-01T00:00:00Z\",\r\n\"completed_at\": \"2026-04-01T00:01:00Z\",\r\n\"conclusion\": \"success\",\r\n\"runner_name\": null\r\n},\r\n{\r\n\"labels\": [\"Windows-Latest\"],\r\n\"name\": \"pack\",\r\n\"id\": 4,\r\n\"run_id\": 1,\r\n\"run_attempt\": 1,\r\n\"started_at\": \"2026-04-01T00:00:00Z\",\r\n\"completed_at\": \"2026-04-01T00:01:00Z\",\r\n\"conclusion\": \"Failure\"\r\n},\r\n{\r\n\"id\": 5,\r\n\"name\": \"big\",\r\n\"labels\": [\"Macos-14-LARGE\"],\r\n\"run_id\": 1,\r\n\"run_attempt\": 1,\r\n\"started_at\": \"2026-04-01T00:00:00Z\",\r\n\"completed_at\": \"2026-04-01T00:01:00Z\",\r\n\"conclusion\": \"success\",\r\n\"runner_name\": null\r\n},\r\n{\r\n\"id\": 6,\r\n\"run_id\": 1,\r\n\"run_attempt\": 1,\r\n\"name\": \"wait\",\r\n\"labels\": [\"macos-latest\"],\r\n\"started_at\": \"2026-04-01T00:00:05Z\",\r\n\"completed_at\": null,\r\n\"conclusion\": null,\r\n\"runner_name\": \"\"\r\n}\r\n]\r\n}\r\n";
    write(&dir, "jobs-1.json", jobs);
    let header = "workflow_path,repository,organization,username,net_amount,discount_amount,gross_amount,applied_cost_per_quantity,unit_type,quantity,sku,product,date";
    let row = ".github/workflows/other.yml,ACME/OTHER,,,0.006,0,0.006,0.006,Minutes,1,Actions_Linux,Actions,2026-04-01";
    write(&dir, "usage.csv", &format!("{header}\r\n{row}\r\n"));

    let text = run_text(&dir);
    let (_, value) = run_json(&dir);
    assert_public_job_prices(&text, &value);
    let running = value["in_progress"].as_array().unwrap();
    assert_eq!(running.len(), 1);
    assert_eq!(running[0]["sku"], "actions_macos");
    assert_eq!(running[0]["invoice"], "0.000000");
    assert_eq!(running[0]["list_price"], "0.000000");
    assert_eq!(running[0]["minutes"], 0);
    let progress = text
        .split("In progress\n")
        .nth(1)
        .unwrap_or_else(|| panic!("missing in progress in {text}"));
    assert!(progress.contains("invoice 0.00"), "{progress}");
    assert!(progress.contains("list 0.00"), "{progress}");
}

#[test]
fn csv_net_wins_on_a_public_repo() {
    let dir = scratch("csv-wins");
    write(
        &dir,
        "repo.json",
        "{\r\n\"visibility\": \"\",\r\n\"private\": false\r\n}\r\n",
    );
    write(
        &dir,
        "runs-1.json",
        &runs_json("ACME/APP/.github/workflows/ci.yml@refs/heads/main"),
    );
    write(
        &dir,
        "jobs-1.json",
        &format!(
            "{{\"jobs\":[{},{}]}}",
            job(1, "lint", "macos-latest"),
            job(2, "wide", "ubuntu-22.04-4core"),
        ),
    );
    let header = "net_amount,sku,repository,workflow_path,date,product,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,username,organization";
    let macos = "2.000000,Actions_MacOS,ACME/APP,.github/workflows/ci.yml,2026-04-01,Actions,1,MINUTES,2.000000,2.000000,0,,";
    let wide = "3.000000,ACTIONS_LINUX_4_CORE,ACME/APP,.github/workflows/ci.yml,2026-04-01,actions,1,minutes,3,3.000000,0,dev,acme";
    write(
        &dir,
        "usage.csv",
        &format!("{header}\r\n{macos}\r\n{wide}\r\n"),
    );

    let text = run_text(&dir);
    let (_, value) = run_json(&dir);
    let macos_job = finished_job(&value, "actions_macos");
    assert_eq!(macos_job["invoice"], "2.000000");
    assert_eq!(macos_job["list_price"], "0.062000");
    assert_eq!(macos_job["method"], "billed_minutes");
    let macos_line = line_with(&text, "actions_macos");
    assert!(macos_line.contains("list 0.06"), "{macos_line}");
    assert!(macos_line.contains("invoice 2.00"), "{macos_line}");

    let wide_job = finished_job(&value, "actions_linux_4_core");
    assert_eq!(wide_job["invoice"], "3.000000");
    assert_eq!(wide_job["list_price"], "0.012000");
    assert!(money(wide_job["invoice"].as_str().unwrap()) > 0);
    assert_eq!(wide_job["method"], "billed_minutes");
    let workflow = &value["workflows"][0];
    assert_eq!(workflow["repository"], "ACME/APP");
    assert_eq!(workflow["invoice"]["amount"], "5.000000");
    assert_eq!(workflow["net"], "5.000000");
}

#[test]
fn csv_net_wins_for_yaml_jobs_on_a_public_repo() {
    let dir = scratch("yaml-wins");
    let workflows = dir.join("workflows");
    std::fs::create_dir(&workflows).unwrap();
    write(
        &workflows,
        "ci.yml",
        "name: CI\r\non: push\r\njobs:\r\n  lint:\r\n    runs-on: macos-latest\r\n    steps:\r\n      - run: echo lint\r\n",
    );
    write(&dir, "repo.json", "{\"PRIVATE\":false,\"name\":\"\"}\n");
    let header = HEADER;
    let row = "2026-04-01,actions,actions_macos,1,minutes,1.620000,1.620000,0,1.620000,,,acme/app,.github/workflows/ci.yml";
    write(&dir, "usage.csv", &format!("{header}\n{row}\n"));
    let usage = dir.join("usage.csv");
    let output = report(&[
        "report",
        "--usage",
        usage.to_str().unwrap(),
        "--runs",
        dir.to_str().unwrap(),
        "--workflows",
        workflows.to_str().unwrap(),
        "--json",
    ]);
    assert_exit(&output, 0);
    let body = stdout(&output);
    assert_clean(&body);
    let value: serde_json::Value = serde_json::from_str(body.trim_end()).unwrap();
    let job = &value["workflows"][0]["job_costs"][0];
    assert_eq!(job["amount"], "1.620000");
    assert_eq!(job["method"], "yaml_weight");
    assert_eq!(job["sku"], "actions_macos");
    assert_eq!(value["workflows"][0]["invoice"]["amount"], "1.620000");
    assert_ne!(job["amount"], "0.000000");
}

#[test]
fn private_repo_keeps_the_list_price_invoice() {
    let dir = scratch("private");
    write_public_fixture(
        &dir,
        "{\"private\":true}\n",
        &runs_json("acme/app/.github/workflows/ci.yml@main"),
        &standard_jobs(),
        &other_usage(),
    );
    let text = run_text(&dir);
    let (_, value) = run_json(&dir);
    assert_list_price_invoice(&value);
    let macos_line = line_with(&text, "actions_macos");
    assert!(macos_line.contains("list 0.06"), "{macos_line}");
    assert!(!macos_line.contains("invoice"), "{macos_line}");
}

#[test]
fn missing_repo_json_keeps_the_list_price_invoice() {
    let dir = scratch("missing-repo");
    write(
        &dir,
        "runs-1.json",
        &runs_json("acme/app/.github/workflows/ci.yml@main"),
    );
    write(&dir, "jobs-1.json", &standard_jobs());
    write(&dir, "usage.csv", &other_usage());
    let (_, value) = run_json(&dir);
    assert_list_price_invoice(&value);
}

#[test]
fn missing_null_and_blank_private_keep_the_list_price_invoice() {
    let bodies = [
        "{}\n",
        "{\"private\":null,\"description\":\"\"}\n",
        "{\"Private\":\"   \",\"default_branch\":\"\"}\n",
        "{\r\n\"name\": \"\",\r\n\"id\": 1\r\n}\r\n",
    ];
    for (index, body) in bodies.iter().enumerate() {
        let dir = scratch(&format!("blank-private-{index}"));
        write_public_fixture(
            &dir,
            body,
            &runs_json("acme/app/.github/workflows/ci.yml@main"),
            &standard_jobs(),
            &other_usage(),
        );
        let (_, value) = run_json(&dir);
        assert_list_price_invoice(&value);
    }
}

#[test]
fn broken_repo_json_exits_2_without_the_body() {
    let cases = [
        "{private: TOKEN-ZZZ-BODY\n",
        "{\"private\":0,\"note\":\"TOKEN-NUM-BODY\"}\n",
        "{\"private\":\"yes\",\"note\":\"TOKEN-WORD-BODY\"}\n",
    ];
    for (index, body) in cases.iter().enumerate() {
        let dir = scratch(&format!("broken-{index}"));
        write(
            &dir,
            "runs-1.json",
            &runs_json(".github/workflows/ci.yml@main"),
        );
        write(&dir, "usage.csv", &other_usage());
        write(&dir, "repo.json", body);
        let usage = dir.join("usage.csv");
        let output = report(&[
            "report",
            "--usage",
            usage.to_str().unwrap(),
            "--runs",
            dir.to_str().unwrap(),
        ]);
        assert_exit(&output, 2);
        let err = stderr(&output);
        assert!(err.contains("repo.json"), "{err}");
        assert!(err.contains("invalid JSON"), "{err}");
        assert!(!err.contains("TOKEN-"), "{err}");
        assert!(!stdout(&output).contains("TOKEN-"), "{}", stdout(&output));
    }

    let dir = scratch("repo-directory");
    write(&dir, "usage.csv", &other_usage());
    std::fs::create_dir(dir.join("repo.json")).unwrap();
    let usage = dir.join("usage.csv");
    let output = report(&[
        "report",
        "--usage",
        usage.to_str().unwrap(),
        "--runs",
        dir.to_str().unwrap(),
    ]);
    assert_exit(&output, 2);
    let err = stderr(&output);
    assert!(err.contains("repo.json"), "{err}");
    assert!(err.contains("cannot read file"), "{err}");
}
