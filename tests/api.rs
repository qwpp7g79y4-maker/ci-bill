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

fn section<'a>(text: &'a str, heading: &str) -> &'a str {
    let marker = format!("\n{heading}\n");
    let start = text
        .find(&marker)
        .unwrap_or_else(|| panic!("missing {heading} in {text}"))
        + marker.len();
    let rest = &text[start..];
    let end = ["\nRuns\n", "\nJobs\n", "\nIn progress\n"]
        .into_iter()
        .filter_map(|item| rest.find(item))
        .min()
        .unwrap_or(rest.len());
    &rest[..end]
}

fn scratch(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("api-cli")
        .join(name);
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn write_json(dir: &Path, name: &str, value: &serde_json::Value) {
    std::fs::write(dir.join(name), serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

#[test]
fn fixture_report_reads_pages_and_marks_in_progress() {
    let output = report(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--runs",
        "tests/fixtures/api",
    ]);
    assert_exit(&output, 0);
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("USD (from net_amount)\n"), "{text}");
    assert!(!text.contains("Sample"), "{text}");
    assert!(
        !text.contains("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        "{text}"
    );
    assert!(!text.contains("api.github.example"), "{text}");
    assert!(!text.contains("previous_attempt_url"), "{text}");

    let runs = section(&text, "Runs");
    assert!(
        runs.contains(
            "1001  .github/workflows/ci.yml  attempt 1  number 40  push  success  2026-03-01T10:00:00Z\n"
        ),
        "{runs}"
    );
    assert!(
        runs.contains(
            "1002  .github/workflows/lint.yml  attempt 2  number 41  pull_request  in progress  2026-03-02T11:00:00Z\n"
        ),
        "{runs}"
    );
    assert!(
        runs.contains(
            "1003  .github/workflows/release.yml  attempt 1  number 7  release  failure  2026-03-03T12:00:00Z\n"
        ),
        "{runs}"
    );

    let jobs = section(&text, "Jobs");
    assert!(
        jobs.contains(
            "501  build  run 1001  .github/workflows/ci.yml  attempt 1  labels ubuntu-latest  runner GitHub Actions 88  started 2026-03-01T10:00:05Z  completed 2026-03-01T10:02:05Z  success\n"
        ),
        "{jobs}"
    );
    assert!(
        jobs.contains(
            "502  test  run 1001  .github/workflows/ci.yml  attempt 1  labels ubuntu-latest, linux  runner GitHub Actions 89  started 2026-03-01T10:02:06Z  completed 2026-03-01T10:04:00Z  success\n"
        ),
        "{jobs}"
    );
    assert!(
        jobs.contains(
            "602  fmt  run 1002  .github/workflows/lint.yml  attempt 2  labels ubuntu-latest  runner GitHub Actions 91  started 2026-03-02T11:00:12Z  completed 2026-03-02T11:01:12Z  success\n"
        ),
        "{jobs}"
    );
    assert!(
        jobs.contains(
            "701  publish  run 1003  .github/workflows/release.yml  attempt 1  labels windows-latest  runner -  started 2026-03-03T12:00:01Z  completed 2026-03-03T12:08:01Z  failure\n"
        ),
        "{jobs}"
    );
    assert!(!jobs.lines().any(|line| line.starts_with("601 ")), "{jobs}");
    assert!(
        !jobs.lines().any(|line| line.contains("  lint  ")),
        "{jobs}"
    );

    let running = section(&text, "In progress");
    assert_eq!(
        running,
        "601  lint  run 1002  .github/workflows/lint.yml  attempt 2  labels macos-latest  runner GitHub Actions 90  started 2026-03-02T11:00:10Z  in progress\n"
    );
    assert_eq!(running.matches("in progress").count(), 1);
}

#[test]
fn fixture_json_reads_pages_and_marks_in_progress() {
    let output = report(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--runs",
        "tests/fixtures/api",
        "--json",
    ]);
    assert_exit(&output, 0);
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(!text.contains("USD (from net_amount)"), "{text}");
    assert!(!text.contains("Sample"), "{text}");
    let value: serde_json::Value = serde_json::from_str(text.trim_end()).expect("json");
    assert_eq!(value["schema_version"], 2);
    assert_eq!(value["currency"], "USD");
    assert_eq!(value["money_unit"], "USD");
    assert!(value["total_net"].is_string());
    assert!(!text.contains("0000000001"), "{text}");
    assert!(!text.contains("9999999"), "{text}");
    assert!(value["workflows"].is_array());

    let runs = value["workflow_runs"].as_array().unwrap();
    assert_eq!(runs.len(), 3);
    assert_eq!(runs[0]["id"], 1001);
    assert_eq!(runs[0]["path"], ".github/workflows/ci.yml");
    assert_eq!(runs[0]["run_attempt"], 1);
    assert_eq!(runs[0]["run_number"], 40);
    assert_eq!(runs[0]["event"], "push");
    assert_eq!(runs[0]["conclusion"], "success");
    assert_eq!(runs[0]["run_started_at"], "2026-03-01T10:00:00Z");
    let ci_jobs = runs[0]["jobs"].as_array().unwrap();
    assert_eq!(ci_jobs.len(), 2);
    assert_eq!(ci_jobs[0]["name"], "build");
    assert_eq!(ci_jobs[0]["id"], 501);
    assert_eq!(ci_jobs[0]["counts"], true);
    assert_eq!(ci_jobs[1]["name"], "test");
    assert_eq!(ci_jobs[1]["id"], 502);
    assert_eq!(
        ci_jobs[1]["labels"],
        serde_json::json!(["ubuntu-latest", "linux"])
    );

    assert_eq!(runs[1]["id"], 1002);
    assert!(runs[1]["conclusion"].is_null());
    let lint_jobs = runs[1]["jobs"].as_array().unwrap();
    assert_eq!(lint_jobs.len(), 1);
    assert_eq!(lint_jobs[0]["name"], "fmt");
    assert_eq!(lint_jobs[0]["id"], 602);
    assert_eq!(lint_jobs[0]["counts"], true);

    assert_eq!(runs[2]["id"], 1003);
    assert_eq!(runs[2]["event"], "release");
    assert_eq!(runs[2]["jobs"][0]["name"], "publish");
    assert_eq!(runs[2]["jobs"][0]["runner_name"], serde_json::Value::Null);
    assert_eq!(runs[2]["jobs"][0]["counts"], true);

    let running = value["in_progress"].as_array().unwrap();
    assert_eq!(running.len(), 1);
    assert_eq!(running[0]["id"], 601);
    assert_eq!(running[0]["name"], "lint");
    assert_eq!(running[0]["run_id"], 1002);
    assert_eq!(running[0]["run_attempt"], 2);
    assert_eq!(running[0]["counts"], false);
    assert_eq!(running[0]["message"], "in progress");
    assert!(running[0]["completed_at"].is_null());
    assert!(running[0]["conclusion"].is_null());
    assert_eq!(running[0]["workflow_path"], ".github/workflows/lint.yml");
    assert_eq!(running[0]["labels"], serde_json::json!(["macos-latest"]));

    let counted: Vec<u64> = runs
        .iter()
        .flat_map(|run| run["jobs"].as_array().unwrap())
        .map(|job| job["id"].as_u64().unwrap())
        .collect();
    assert_eq!(counted, vec![501, 502, 602, 701]);
    assert!(!counted.contains(&601));
}

#[test]
fn absent_completed_at_is_in_progress_on_the_cli() {
    let dir = scratch("absent-completed");
    write_json(
        &dir,
        "runs-1.json",
        &serde_json::json!({
            "workflow_runs": [{
                "id": 8,
                "path": ".github/workflows/ci.yml",
                "run_attempt": 1,
                "run_number": 2,
                "event": "push",
                "conclusion": "success",
                "run_started_at": "2026-04-01T00:00:00Z"
            }]
        }),
    );
    write_json(
        &dir,
        "jobs-8.json",
        &serde_json::json!({
            "jobs": [
                {
                    "id": 80,
                    "run_id": 8,
                    "run_attempt": 1,
                    "name": "build",
                    "labels": ["ubuntu-latest"],
                    "started_at": "2026-04-01T00:00:00Z",
                    "completed_at": "2026-04-01T00:02:00Z",
                    "conclusion": "success",
                    "runner_name": "runner-a"
                },
                {
                    "id": 81,
                    "run_id": 8,
                    "run_attempt": 1,
                    "name": "lint",
                    "labels": ["macos-latest"],
                    "started_at": "2026-04-01T00:00:05Z",
                    "conclusion": null,
                    "runner_name": "runner-b"
                }
            ]
        }),
    );

    let output = report(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--runs",
        dir.to_str().unwrap(),
    ]);
    assert_exit(&output, 0);
    let text = stdout(&output);
    let jobs = section(&text, "Jobs");
    assert!(jobs.contains("80  build"), "{jobs}");
    assert!(jobs.contains("completed 2026-04-01T00:02:00Z"), "{jobs}");
    assert!(!jobs.contains("81"), "{jobs}");
    assert!(!jobs.contains("lint"), "{jobs}");
    let running = section(&text, "In progress");
    assert!(
        running.contains(
            "81  lint  run 8  .github/workflows/ci.yml  attempt 1  labels macos-latest  runner runner-b  started 2026-04-01T00:00:05Z  in progress\n"
        ),
        "{running}"
    );
    assert!(!running.contains("completed"), "{running}");

    let json_out = report(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--runs",
        dir.to_str().unwrap(),
        "--json",
    ]);
    assert_exit(&json_out, 0);
    let value: serde_json::Value = serde_json::from_str(stdout(&json_out).trim_end()).unwrap();
    assert_eq!(
        value["workflow_runs"][0]["jobs"].as_array().unwrap().len(),
        1
    );
    assert_eq!(value["workflow_runs"][0]["jobs"][0]["id"], 80);
    assert_eq!(value["workflow_runs"][0]["jobs"][0]["counts"], true);
    assert_eq!(value["in_progress"][0]["id"], 81);
    assert_eq!(value["in_progress"][0]["counts"], false);
    assert_eq!(value["in_progress"][0]["message"], "in progress");
    assert!(value["in_progress"][0]["completed_at"].is_null());
}

#[test]
fn null_completed_at_is_in_progress_on_the_cli() {
    let dir = scratch("null-completed");
    write_json(
        &dir,
        "runs-a.json",
        &serde_json::json!({
            "workflow_runs": [{
                "id": 4,
                "path": ".github/workflows/lint.yml",
                "run_attempt": 3,
                "run_number": 9,
                "event": "pull_request",
                "conclusion": null,
                "run_started_at": "2026-05-01T00:00:00Z"
            }]
        }),
    );
    write_json(
        &dir,
        "jobs-4.json",
        &serde_json::json!({
            "jobs": [{
                "id": 40,
                "run_id": 4,
                "run_attempt": 3,
                "name": "eslint",
                "labels": ["windows-latest"],
                "started_at": "2026-05-01T00:00:01Z",
                "completed_at": null,
                "conclusion": null,
                "runner_name": null
            }]
        }),
    );
    write_json(&dir, "jobs-4-2.json", &serde_json::json!({"jobs": []}));

    let output = report(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--runs",
        dir.to_str().unwrap(),
        "--top",
        "1",
    ]);
    assert_exit(&output, 0);
    let text = stdout(&output);
    assert!(section(&text, "Jobs").contains("(none)\n"), "{text}");
    let running = section(&text, "In progress");
    assert!(
        running.contains("40  eslint") && running.contains("in progress"),
        "{running}"
    );
    assert!(running.contains("runner -"), "{running}");
    assert!(!section(&text, "Jobs").contains("eslint"), "{text}");
}

#[test]
fn broken_json_exits_2_with_the_file_name() {
    let secret = "LEAKED_SECRET_TOKEN";
    let cases = [
        ("runs-1.json", format!("{{ {secret}"), true),
        ("runs-2.json", secret.to_string(), true),
        (
            "runs-3.json",
            format!(r#"{{"workflow_runs":"{secret}"}}"#),
            true,
        ),
        ("runs-4.json", "[]".to_string(), true),
        ("runs-5.json", "null".to_string(), true),
        ("runs-6.json", String::new(), true),
        (
            "runs-7.json",
            format!(r#"{{"workflow_runs":[{{"id":"{secret}"}}]}}"#),
            true,
        ),
        (
            "jobs-9.json",
            format!(r#"{{"jobs":[{{"name":"{secret}"}}]}}"#),
            false,
        ),
        ("jobs-9-2.json", "{".to_string(), false),
        ("jobs-8.json", String::new(), false),
        (
            "jobs-12.json",
            format!(
                r#"{{"jobs":[{{"id":1,"run_id":12,"run_attempt":1,"name":"build","completed_at":{{"leak":"{secret}"}}}}]}}"#
            ),
            false,
        ),
    ];

    for (index, (name, body, is_runs)) in cases.iter().enumerate() {
        let dir = scratch(&format!("broken-{index}"));
        if !is_runs {
            write_json(
                &dir,
                "runs-ok.json",
                &serde_json::json!({
                    "workflow_runs": [{
                        "id": 1,
                        "path": ".github/workflows/ci.yml",
                        "run_attempt": 1,
                        "run_number": 1,
                        "event": "push",
                        "conclusion": "success",
                        "run_started_at": "2026-01-01T00:00:00Z"
                    }]
                }),
            );
        }
        std::fs::write(dir.join(name), body).unwrap();
        let output = report(&[
            "report",
            "--usage",
            "samples/usage-sample.csv",
            "--runs",
            dir.to_str().unwrap(),
        ]);
        assert_exit(&output, 2);
        let err = stderr(&output);
        assert!(err.contains(name), "{name}\n{err}");
        assert!(err.contains("invalid JSON"), "{name}\n{err}");
        assert!(!err.contains(secret), "{name}\n{err}");
        assert!(
            !stdout(&output).contains(secret),
            "{name}\n{}",
            stdout(&output)
        );
        assert!(stdout(&output).is_empty(), "{name}\n{}", stdout(&output));
    }
}

#[test]
fn missing_runs_dir_and_file_path_exit_2() {
    let missing = report(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--runs",
        "tests/fixtures/api/no-such-dir",
    ]);
    assert_exit(&missing, 2);
    let err = stderr(&missing);
    assert!(err.contains("cannot read directory"), "{err}");
    assert!(err.contains("tests/fixtures/api/no-such-dir"), "{err}");
    assert!(stdout(&missing).is_empty(), "{}", stdout(&missing));

    let output = report(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--runs",
        "samples/usage-sample.csv",
    ]);
    assert_exit(&output, 2);
    let err = stderr(&output);
    assert!(err.contains("cannot read directory"), "{err}");
    assert!(err.contains("usage-sample.csv"), "{err}");
}

#[test]
fn runs_flag_still_requires_usage() {
    let missing_usage = report(&["report", "--runs", "tests/fixtures/api"]);
    assert_exit(&missing_usage, 3);

    let missing_dir = report(&["report", "--usage", "samples/usage-sample.csv", "--runs"]);
    assert_exit(&missing_dir, 3);
}

#[test]
fn top_and_budget_keep_the_in_progress_line() {
    let over = report(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--runs",
        "tests/fixtures/api",
        "--top",
        "1",
        "--budget",
        "0",
    ]);
    assert_exit(&over, 1);
    let text = stdout(&over);
    assert!(text.contains("Budget exceeded:"), "{text}");
    assert!(
        section(&text, "In progress").contains("601  lint"),
        "{text}"
    );
    assert!(section(&text, "Jobs").contains("701  publish"), "{text}");
    assert!(!section(&text, "Jobs").contains("601"), "{text}");

    let json = report(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--runs",
        "tests/fixtures/api",
        "--json",
        "--budget",
        "0",
    ]);
    assert_exit(&json, 1);
    let err = stderr(&json);
    assert!(err.contains("Budget exceeded:"), "{err}");
    let value: serde_json::Value = serde_json::from_str(stdout(&json).trim_end()).unwrap();
    assert_eq!(value["budget_exceeded"], true);
    assert_eq!(value["in_progress"][0]["message"], "in progress");
    assert_eq!(value["in_progress"][0]["counts"], false);
    assert_eq!(value["in_progress"][0]["id"], 601);
}

#[test]
fn empty_runs_directory_exits_0() {
    let dir = scratch("empty");
    let output = report(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--runs",
        dir.to_str().unwrap(),
    ]);
    assert_exit(&output, 0);
    let text = stdout(&output);
    assert!(text.contains("\nRuns\n(none)\n"), "{text}");
    assert!(text.contains("\nJobs\n(none)\n"), "{text}");
    assert!(text.contains("\nIn progress\n(none)\n"), "{text}");
    assert!(!text.contains("in progress\n"), "{text}");
}

const LINK_USAGE: &str = "\
date,product,sku,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,net_amount,username,organization,repository,workflow_path
2026-04-01,actions,actions_linux,1,minutes,0.006,0.006,0,0.006,dev,acme,acme/widgets,.github/workflows/ci.yml
";

fn assert_plain(text: &str) {
    let lower = text.to_ascii_lowercase();
    assert!(!lower.contains("not supported"), "{text}");
    assert!(!lower.contains("skipped"), "{text}");
    assert!(!lower.contains("limitation"), "{text}");
}

fn report_runs(dir: &Path, json: bool) -> std::process::Output {
    std::fs::write(dir.join("usage.csv"), LINK_USAGE).unwrap();
    let usage = dir.join("usage.csv");
    let usage = usage.to_str().unwrap();
    let runs = dir.to_str().unwrap();
    if json {
        report(&["report", "--usage", usage, "--runs", runs, "--json"])
    } else {
        report(&["report", "--usage", usage, "--runs", runs])
    }
}

fn id_lines<'a>(section: &'a str, id: &str) -> Vec<&'a str> {
    let prefix = format!("{id} ");
    section
        .lines()
        .filter(|line| line.starts_with(&prefix))
        .collect()
}

#[test]
fn cli_links_ref_owner_prefix_case_crlf_and_reordered_fields() {
    let dir = scratch("cli-link");
    let crlf = "{\r\n\"workflow_runs\": [\r\n{\r\n\"event\": \"push\",\r\n\"run_number\": 1,\r\n\"conclusion\": null,\r\n\"run_attempt\": 1,\r\n\"id\": 1,\r\n\"path\": \".github/workflows/ci.yml@main\",\r\n\"run_started_at\": \"2026-04-01T12:00:00Z\"\r\n},\r\n{\r\n\"path\": \".github/workflows/ci.yml@refs/heads/main\",\r\n\"id\": 2,\r\n\"run_attempt\": 1,\r\n\"run_number\": 2,\r\n\"event\": \"pull_request\",\r\n\"conclusion\": \"success\",\r\n\"run_started_at\": \"2026-04-02T12:00:00Z\"\r\n}\r\n]\r\n}\r\n";
    std::fs::write(dir.join("runs-1.json"), crlf).unwrap();
    write_json(
        &dir,
        "runs-2.json",
        &serde_json::json!({
            "workflow_runs": [
                {
                    "run_started_at": "2026-04-03T12:00:00Z",
                    "event": "push",
                    "conclusion": "success",
                    "run_number": 3,
                    "run_attempt": 1,
                    "path": "octocat/octo-repo/.github/workflows/ci.yml@main",
                    "id": 3
                },
                {
                    "id": 4,
                    "path": "OWNER/REPO/.GitHub/Workflows/CI.yml@REFS/HEADS/Feature/Foo",
                    "run_attempt": 1,
                    "run_number": 4,
                    "event": "workflow_dispatch",
                    "conclusion": "",
                    "run_started_at": ""
                }
            ]
        }),
    );
    write_json(
        &dir,
        "jobs-1.json",
        &serde_json::json!({
            "jobs": [{
                "runner_name": "runner-a",
                "conclusion": "success",
                "completed_at": "2026-04-01T12:02:00Z",
                "started_at": "2026-04-01T12:00:05Z",
                "labels": ["ubuntu-latest"],
                "name": "build",
                "run_attempt": 1,
                "run_id": 1,
                "id": 11
            }]
        }),
    );

    let output = report_runs(&dir, false);
    assert_exit(&output, 0);
    let text = stdout(&output);
    assert_plain(&text);
    assert!(text.contains(".github/workflows/ci.yml"), "{text}");
    assert!(!text.contains('@'), "{text}");
    assert!(!text.contains("octocat"), "{text}");
    assert!(!text.contains("OWNER/REPO"), "{text}");
    assert!(!text.contains("refs/heads"), "{text}");
    let runs = section(&text, "Runs");
    assert!(
        runs.contains(
            "1  .github/workflows/ci.yml  attempt 1  number 1  push  in progress  2026-04-01T12:00:00Z\n"
        ),
        "{runs}"
    );
    assert!(
        runs.contains(
            "2  .github/workflows/ci.yml  attempt 1  number 2  pull_request  success  2026-04-02T12:00:00Z\n"
        ),
        "{runs}"
    );
    assert!(
        runs.contains(
            "3  .github/workflows/ci.yml  attempt 1  number 3  push  success  2026-04-03T12:00:00Z\n"
        ),
        "{runs}"
    );
    assert!(
        runs.contains("4  .GitHub/Workflows/CI.yml  attempt 1  number 4  workflow_dispatch"),
        "{runs}"
    );
    let jobs = section(&text, "Jobs");
    assert!(
        jobs.contains("11  build  run 1  .github/workflows/ci.yml"),
        "{jobs}"
    );
    assert_eq!(id_lines(jobs, "11").len(), 1);

    let json_out = report_runs(&dir, true);
    assert_exit(&json_out, 0);
    let body = stdout(&json_out);
    assert_plain(&body);
    let value: serde_json::Value = serde_json::from_str(body.trim_end()).unwrap();
    let paths: Vec<&str> = value["workflow_runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|run| run["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&".github/workflows/ci.yml"), "{paths:?}");
    assert!(paths.contains(&".GitHub/Workflows/CI.yml"), "{paths:?}");
    assert!(paths.iter().all(|path| !path.contains('@')), "{paths:?}");
    assert!(
        paths
            .iter()
            .all(|path| !path.contains("OWNER") && !path.contains("octocat")),
        "{paths:?}"
    );
}

#[test]
fn cli_duplicate_job_id_counts_once_across_order_and_crlf() {
    let dir = scratch("cli-dedupe");
    write_json(
        &dir,
        "runs-1.json",
        &serde_json::json!({
            "workflow_runs": [{
                "id": 7,
                "path": ".github/workflows/ci.yml@main",
                "run_attempt": 1,
                "run_number": 7,
                "event": "push",
                "conclusion": "success",
                "run_started_at": "2026-04-01T12:00:00Z"
            }]
        }),
    );
    let first = "{\r\n\"jobs\": [\r\n{\r\n\"id\": 70,\r\n\"run_id\": 7,\r\n\"run_attempt\": 1,\r\n\"name\": \"kept\",\r\n\"labels\": [\"ubuntu-latest\"],\r\n\"started_at\": \"2026-04-01T12:00:05Z\",\r\n\"completed_at\": \"2026-04-01T12:01:00Z\",\r\n\"conclusion\": \"success\",\r\n\"runner_name\": \"runner-a\"\r\n}\r\n]\r\n}\r\n";
    std::fs::write(dir.join("jobs-7-a.json"), first).unwrap();
    std::fs::write(
        dir.join("jobs-7-b.json"),
        r#"{"jobs":[{"runner_name":"other","conclusion":"failure","completed_at":"2026-04-01T13:00:00Z","started_at":"2026-04-01T12:30:00Z","labels":["windows-latest"],"name":"dropped","run_attempt":1,"run_id":7,"id":70}]}"#,
    )
    .unwrap();
    std::fs::write(dir.join("jobs-7-attempts-1.json"), b"{").unwrap();
    let nested = dir.join("attempts").join("1");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(
        nested.join("jobs-7.json"),
        r#"{"jobs":[{"id":71,"run_id":7,"run_attempt":1,"name":"attempt-only","labels":[],"started_at":"2026-04-01T12:00:00Z","completed_at":"2026-04-01T12:01:00Z","conclusion":"success","runner_name":null}]}"#,
    )
    .unwrap();

    let output = report_runs(&dir, false);
    assert_exit(&output, 0);
    let text = stdout(&output);
    assert_plain(&text);
    let jobs = section(&text, "Jobs");
    assert_eq!(id_lines(jobs, "70").len(), 1, "{jobs}");
    assert!(jobs.contains("kept"), "{jobs}");
    assert!(!jobs.contains("dropped"), "{jobs}");
    assert!(!jobs.contains("attempt-only"), "{jobs}");
    assert!(!text.contains("71 "), "{text}");

    let json_out = report_runs(&dir, true);
    assert_exit(&json_out, 0);
    let value: serde_json::Value = serde_json::from_str(stdout(&json_out).trim_end()).unwrap();
    let listed = value["workflow_runs"][0]["jobs"].as_array().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], 70);
    assert_eq!(listed[0]["name"], "kept");
    assert_eq!(listed[0]["billable"], true);
    assert_eq!(listed[0]["inherited_copy"], false);
}

#[test]
fn cli_inherited_copy_costs_zero_and_blank_times_do_not() {
    let dir = scratch("cli-copy");
    write_json(
        &dir,
        "runs-1.json",
        &serde_json::json!({
            "workflow_runs": [{
                "id": 9,
                "path": "octocat/octo-repo/.github/workflows/ci.yml@refs/heads/main",
                "run_attempt": 2,
                "run_number": 9,
                "event": "push",
                "conclusion": "success",
                "run_started_at": "2026-04-01T12:00:00Z"
            }]
        }),
    );
    write_json(
        &dir,
        "jobs-9.json",
        &serde_json::json!({
            "jobs": [
                {
                    "id": 1,
                    "run_id": 9,
                    "run_attempt": 1,
                    "name": "test",
                    "labels": ["ubuntu-latest"],
                    "started_at": "2026-04-01T10:00:00Z",
                    "completed_at": "2026-04-01T10:10:00Z",
                    "conclusion": "success",
                    "runner_name": "runner-a"
                },
                {
                    "id": 2,
                    "run_id": 9,
                    "run_attempt": 2,
                    "name": "test",
                    "labels": ["ubuntu-latest"],
                    "started_at": "2026-04-01T10:00:00Z",
                    "completed_at": "2026-04-01T10:10:00Z",
                    "conclusion": "success",
                    "runner_name": "runner-a"
                },
                {
                    "id": 3,
                    "run_id": 9,
                    "run_attempt": 2,
                    "name": "lint",
                    "labels": ["ubuntu-latest"],
                    "started_at": "2026-04-01T12:00:30Z",
                    "completed_at": "2026-04-01T12:02:00Z",
                    "conclusion": "success",
                    "runner_name": "runner-a"
                },
                {
                    "id": 4,
                    "run_id": 9,
                    "run_attempt": 2,
                    "name": "pack",
                    "labels": [],
                    "started_at": "",
                    "completed_at": "2026-04-01T12:03:00Z",
                    "conclusion": "success",
                    "runner_name": null
                },
                {
                    "id": 5,
                    "run_id": 9,
                    "run_attempt": 2,
                    "name": "queue",
                    "labels": ["ubuntu-latest"],
                    "started_at": "2026-04-01T11:00:00Z",
                    "completed_at": null,
                    "conclusion": null,
                    "runner_name": "runner-b"
                }
            ]
        }),
    );

    let output = report_runs(&dir, false);
    assert_exit(&output, 0);
    let text = stdout(&output);
    assert_plain(&text);
    assert!(!text.contains("octocat"), "{text}");
    assert!(!text.contains('@'), "{text}");
    let jobs = section(&text, "Jobs");
    assert_eq!(id_lines(jobs, "1").len(), 1, "{jobs}");
    assert_eq!(id_lines(jobs, "2").len(), 1, "{jobs}");
    assert!(id_lines(jobs, "1")[0].contains("attempt 1"), "{jobs}");
    assert!(!id_lines(jobs, "1")[0].contains("inherited copy"), "{jobs}");
    assert!(
        id_lines(jobs, "2")[0].contains("inherited copy  cost 0.00"),
        "{jobs}"
    );
    assert!(!id_lines(jobs, "3")[0].contains("inherited copy"), "{jobs}");
    assert!(!id_lines(jobs, "4")[0].contains("inherited copy"), "{jobs}");
    assert!(!jobs.contains("queue"), "{jobs}");
    let running = section(&text, "In progress");
    assert_eq!(id_lines(running, "5").len(), 1, "{running}");
    assert!(running.contains("inherited copy  cost 0.00"), "{running}");
    assert!(running.contains("in progress"), "{running}");

    let json_out = report_runs(&dir, true);
    assert_exit(&json_out, 0);
    let body = stdout(&json_out);
    assert_plain(&body);
    let value: serde_json::Value = serde_json::from_str(body.trim_end()).unwrap();
    assert_eq!(
        value["workflow_runs"][0]["path"],
        ".github/workflows/ci.yml"
    );
    let listed = value["workflow_runs"][0]["jobs"].as_array().unwrap();
    let by_id = |id: u64| {
        listed
            .iter()
            .find(|job| job["id"].as_u64() == Some(id))
            .unwrap_or_else(|| panic!("missing {id} in {listed:?}"))
    };
    assert_eq!(by_id(1)["billable"], true);
    assert_eq!(by_id(1)["inherited_copy"], false);
    assert!(by_id(1).get("net_amount").is_none());
    assert_eq!(by_id(2)["billable"], false);
    assert_eq!(by_id(2)["inherited_copy"], true);
    assert_eq!(by_id(2)["counts"], true);
    assert_eq!(by_id(2)["net_amount"], "0.000000");
    assert_eq!(by_id(3)["billable"], true);
    assert_eq!(by_id(4)["inherited_copy"], false);
    assert_eq!(by_id(4)["billable"], true);
    assert!(by_id(4).get("net_amount").is_none());
    let progress = &value["in_progress"][0];
    assert_eq!(progress["id"], 5);
    assert_eq!(progress["inherited_copy"], true);
    assert_eq!(progress["billable"], false);
    assert_eq!(progress["net_amount"], "0.000000");
    assert_eq!(progress["message"], "in progress");
    assert_eq!(progress["workflow_path"], ".github/workflows/ci.yml");
}
