//! Job minutes and list price on the report command.

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
        .join("job-cost")
        .join(name);
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
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

fn job_ids(section: &str) -> Vec<u64> {
    section
        .lines()
        .filter_map(|line| {
            let (id, _) = line.split_once(' ')?;
            id.parse().ok()
        })
        .collect()
}

fn line_of(section: &str, id: u64) -> &str {
    section
        .lines()
        .find(|line| line.starts_with(&format!("{id} ")))
        .unwrap_or_else(|| panic!("missing job {id} in {section}"))
}

const USAGE: &str = "\
date,product,sku,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,net_amount,username,organization,repository,workflow_path
2026-04-01,actions,actions_linux,1,minutes,0.006,0.006,0,0.006,dev,acme,acme/widgets,.github/workflows/ci.yml
";

fn write_fixture(dir: &Path) {
    let runs = "{\r\n\"workflow_runs\": [\r\n{\r\n\"event\": \"push\",\r\n\"conclusion\": \"success\",\r\n\"run_number\": 3,\r\n\"run_started_at\": \"2026-04-01T00:00:00Z\",\r\n\"path\": \"acme/widgets/.github/workflows/ci.yml@main\",\r\n\"run_attempt\": 1,\r\n\"id\": 1\r\n},\r\n{\r\n\"path\": \".github/workflows/lint.yml@refs/heads/main\",\r\n\"id\": 2,\r\n\"run_attempt\": 1,\r\n\"run_number\": 4,\r\n\"event\": \"pull_request\",\r\n\"conclusion\": \"success\",\r\n\"run_started_at\": \"2026-04-01T00:00:00Z\"\r\n}\r\n]\r\n}\r\n";
    std::fs::write(dir.join("runs-1.json"), runs).unwrap();
    // Keys are not in a fixed order. Blank, missing, and null fields are included.
    let jobs = "{\r\n\"jobs\": [\r\n{\r\n\"runner_name\": \"runner-a\",\r\n\"conclusion\": \"success\",\r\n\"completed_at\": \"2026-04-01T00:01:01Z\",\r\n\"started_at\": \"2026-04-01T00:00:00Z\",\r\n\"labels\": [\"ubuntu-latest\"],\r\n\"name\": \"build\",\r\n\"run_attempt\": 1,\r\n\"run_id\": 1,\r\n\"id\": 1\r\n},\r\n{\r\n\"id\": 4,\r\n\"name\": \"pack\",\r\n\"labels\": [\"Windows-Latest\"],\r\n\"run_id\": 1,\r\n\"run_attempt\": 1,\r\n\"started_at\": \"2026-04-01T00:00:00Z\",\r\n\"completed_at\": \"2026-04-01T00:01:30Z\",\r\n\"conclusion\": \"Cancelled\",\r\n\"runner_name\": null\r\n},\r\n{\r\n\"conclusion\": \"success\",\r\n\"completed_at\": \"2026-04-01T00:01:00Z\",\r\n\"started_at\": \"2026-04-01T00:00:00Z\",\r\n\"labels\": [\"ubuntu-latest\"],\r\n\"name\": \"test\",\r\n\"run_attempt\": 1,\r\n\"run_id\": 1,\r\n\"id\": 2,\r\n\"runner_name\": \"runner-a\"\r\n},\r\n{\r\n\"name\": \"zero\",\r\n\"id\": 3,\r\n\"run_id\": 1,\r\n\"run_attempt\": 1,\r\n\"labels\": [\"ubuntu-latest\"],\r\n\"started_at\": \"2026-04-01T00:00:00Z\",\r\n\"completed_at\": \"2026-04-01T00:00:00Z\",\r\n\"conclusion\": \"Failure\",\r\n\"runner_name\": \"runner-a\"\r\n},\r\n{\r\n\"conclusion\": \"SKIPPED\",\r\n\"completed_at\": \"2026-04-01T00:10:00Z\",\r\n\"started_at\": \"2026-04-01T00:00:00Z\",\r\n\"labels\": [\"macos-latest\"],\r\n\"name\": \"fmt\",\r\n\"run_id\": 1,\r\n\"run_attempt\": 1,\r\n\"id\": 5,\r\n\"runner_name\": \"runner-a\"\r\n},\r\n{\r\n\"id\": 6,\r\n\"run_id\": 1,\r\n\"run_attempt\": 1,\r\n\"name\": \"queue\",\r\n\"labels\": [\"macos-latest\"],\r\n\"started_at\": \"2026-04-01T00:00:05Z\",\r\n\"completed_at\": null,\r\n\"conclusion\": null,\r\n\"runner_name\": \"runner-b\"\r\n},\r\n{\r\n\"id\": 8,\r\n\"run_id\": 1,\r\n\"run_attempt\": 1,\r\n\"name\": \"blank\",\r\n\"labels\": [\"ubuntu-latest\"],\r\n\"started_at\": \"2026-04-01T00:00:00Z\",\r\n\"completed_at\": \"\",\r\n\"conclusion\": \"success\",\r\n\"runner_name\": \"\"\r\n},\r\n{\r\n\"conclusion\": \"success\",\r\n\"completed_at\": \"2026-04-01T00:05:00Z\",\r\n\"labels\": [\"ubuntu-latest\"],\r\n\"name\": \"nostart\",\r\n\"run_attempt\": 1,\r\n\"run_id\": 1,\r\n\"id\": 9,\r\n\"runner_name\": null\r\n},\r\n{\r\n\"id\": 10,\r\n\"run_id\": 1,\r\n\"run_attempt\": 1,\r\n\"name\": \"custom\",\r\n\"started_at\": \"2026-04-01T00:00:00Z\",\r\n\"completed_at\": \"2026-04-01T00:01:01Z\",\r\n\"conclusion\": \"success\",\r\n\"runner_name\": null\r\n},\r\n{\r\n\"id\": 11,\r\n\"run_id\": 1,\r\n\"run_attempt\": 1,\r\n\"name\": \"bare\",\r\n\"labels\": null,\r\n\"started_at\": \"2026-04-01T00:00:00Z\",\r\n\"completed_at\": \"2026-04-01T00:01:00Z\",\r\n\"conclusion\": \"success\",\r\n\"runner_name\": null\r\n},\r\n{\r\n\"id\": 12,\r\n\"run_id\": 1,\r\n\"run_attempt\": 1,\r\n\"name\": \"gone\",\r\n\"labels\": [],\r\n\"started_at\": \"2026-04-01T00:00:00Z\",\r\n\"conclusion\": null,\r\n\"runner_name\": null\r\n}\r\n]\r\n}\r\n";
    std::fs::write(dir.join("jobs-1.json"), jobs).unwrap();
    let lint = "{\r\n\"jobs\": [\r\n{\r\n\"runner_name\": \"runner-c\",\r\n\"conclusion\": \"success\",\r\n\"completed_at\": \"2026-04-01T00:01:01Z\",\r\n\"started_at\": \"2026-04-01T00:00:00Z\",\r\n\"labels\": [\"UBUNTU-LATEST\"],\r\n\"name\": \"lint\",\r\n\"run_attempt\": 1,\r\n\"run_id\": 2,\r\n\"id\": 7\r\n}\r\n]\r\n}\r\n";
    std::fs::write(dir.join("jobs-2.json"), lint).unwrap();
    std::fs::write(dir.join("usage.csv"), USAGE).unwrap();
}

#[test]
fn report_lists_each_job_under_its_workflow_with_minutes_and_list_price() {
    let dir = scratch("minutes");
    write_fixture(&dir);
    let usage = dir.join("usage.csv");
    let output = report(&[
        "report",
        "--usage",
        usage.to_str().unwrap(),
        "--runs",
        dir.to_str().unwrap(),
    ]);
    assert_exit(&output, 0);
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
    let text = stdout(&output);
    let lower = text.to_ascii_lowercase();
    assert!(!lower.contains("not supported"), "{text}");
    assert!(!lower.contains("limitation"), "{text}");
    assert!(!text.contains('@'), "{text}");

    let jobs = section(&text, "Jobs");
    assert!(jobs.starts_with(".github/workflows/ci.yml\n"), "{jobs}");
    let lint_at = jobs
        .find(".github/workflows/lint.yml\n")
        .expect("lint group");
    assert!(jobs.find(".github/workflows/ci.yml\n").unwrap() < lint_at);
    assert_eq!(job_ids(jobs), vec![4, 1, 9, 2, 3, 10, 11, 5, 7], "{jobs}");
    assert!(
        line_of(jobs, 1).contains("2 min  actions_linux  list 0.01"),
        "{jobs}"
    );
    assert!(
        line_of(jobs, 2).contains("1 min  actions_linux  list 0.01"),
        "{jobs}"
    );
    assert!(
        line_of(jobs, 3).contains("1 min  actions_linux  list 0.01"),
        "{jobs}"
    );
    assert!(
        line_of(jobs, 4).contains("2 min  actions_windows  list 0.02"),
        "{jobs}"
    );
    assert!(line_of(jobs, 4).contains("Cancelled"), "{jobs}");
    assert!(
        line_of(jobs, 5).contains("0 min  actions_macos  list 0.00"),
        "{jobs}"
    );
    assert!(line_of(jobs, 5).contains("SKIPPED"), "{jobs}");
    assert!(
        line_of(jobs, 7).contains("2 min  actions_linux  list 0.01"),
        "{jobs}"
    );
    assert!(
        line_of(jobs, 9).contains("1 min  actions_linux  list 0.01"),
        "{jobs}"
    );
    assert!(line_of(jobs, 10).contains("2 min  -  list 0.00"), "{jobs}");
    assert!(line_of(jobs, 11).contains("1 min  -  list 0.00"), "{jobs}");
    assert!(!jobs.contains("\n6 "), "{jobs}");
    assert!(!jobs.contains("\n8 "), "{jobs}");
    assert!(!jobs.contains("\n12 "), "{jobs}");

    let running = section(&text, "In progress");
    assert_eq!(job_ids(running), vec![8, 12, 6], "{running}");
    for id in [8_u64, 12, 6] {
        let line = line_of(running, id);
        assert!(line.contains("in progress"), "{line}");
        assert!(line.contains("0 min"), "{line}");
        assert!(line.contains("list 0.00"), "{line}");
        assert!(!line.contains("completed"), "{line}");
    }
    assert_eq!(running.matches("in progress").count(), 3, "{running}");

    let json_out = report(&[
        "report",
        "--usage",
        usage.to_str().unwrap(),
        "--runs",
        dir.to_str().unwrap(),
        "--json",
    ]);
    assert_exit(&json_out, 0);
    let body = stdout(&json_out);
    assert!(
        !body.to_ascii_lowercase().contains("not supported"),
        "{body}"
    );
    let value: serde_json::Value = serde_json::from_str(body.trim_end()).unwrap();
    let runs = value["workflow_runs"].as_array().unwrap();
    assert_eq!(runs[0]["path"], ".github/workflows/ci.yml");
    assert_eq!(runs[1]["path"], ".github/workflows/lint.yml");
    let ci = runs[0]["jobs"].as_array().unwrap();
    assert_eq!(
        ci.iter()
            .map(|job| job["id"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![4, 1, 9, 2, 3, 10, 11, 5]
    );
    let by_id = |id: u64| {
        ci.iter()
            .find(|job| job["id"].as_u64() == Some(id))
            .unwrap_or_else(|| panic!("missing {id}"))
    };
    assert_eq!(by_id(1)["minutes"], 2);
    assert_eq!(by_id(1)["sku"], "actions_linux");
    assert_eq!(by_id(1)["usd_per_minute"], "0.006000");
    assert_eq!(by_id(1)["list_price"], "0.012000");
    assert_eq!(by_id(2)["minutes"], 1);
    assert_eq!(by_id(2)["list_price"], "0.006000");
    assert_eq!(by_id(3)["minutes"], 1);
    assert_eq!(by_id(3)["list_price"], "0.006000");
    assert_eq!(by_id(4)["minutes"], 2);
    assert_eq!(by_id(4)["sku"], "actions_windows");
    assert_eq!(by_id(4)["usd_per_minute"], "0.010000");
    assert_eq!(by_id(4)["list_price"], "0.020000");
    assert_eq!(by_id(5)["minutes"], 0);
    assert_eq!(by_id(5)["sku"], "actions_macos");
    assert_eq!(by_id(5)["usd_per_minute"], "0.062000");
    assert_eq!(by_id(5)["list_price"], "0.000000");
    assert_eq!(by_id(9)["minutes"], 1);
    assert_eq!(by_id(9)["list_price"], "0.006000");
    assert_eq!(by_id(10)["minutes"], 2);
    assert!(by_id(10)["sku"].is_null());
    assert!(by_id(10)["usd_per_minute"].is_null());
    assert_eq!(by_id(10)["list_price"], "0.000000");
    assert_eq!(by_id(11)["minutes"], 1);
    assert!(by_id(11)["sku"].is_null());
    assert_eq!(by_id(11)["list_price"], "0.000000");

    let lint = runs[1]["jobs"].as_array().unwrap();
    assert_eq!(lint.len(), 1);
    assert_eq!(lint[0]["id"], 7);
    assert_eq!(lint[0]["minutes"], 2);
    assert_eq!(lint[0]["sku"], "actions_linux");
    assert_eq!(lint[0]["list_price"], "0.012000");
    assert_eq!(lint[0]["usd_per_minute"], "0.006000");

    let progress = value["in_progress"].as_array().unwrap();
    assert_eq!(progress.len(), 3);
    for job in progress {
        assert_eq!(job["minutes"], 0);
        assert_eq!(job["list_price"], "0.000000");
        assert_eq!(job["message"], "in progress");
        assert_eq!(job["counts"], false);
        let completed = &job["completed_at"];
        assert!(
            completed.is_null() || completed.as_str() == Some(""),
            "{completed}"
        );
    }
    let progress_ids: Vec<u64> = progress
        .iter()
        .map(|job| job["id"].as_u64().unwrap())
        .collect();
    assert_eq!(progress_ids, vec![8, 12, 6]);
    assert_eq!(progress[0]["sku"], "actions_linux");
    assert!(progress[1]["sku"].is_null());
    assert_eq!(progress[2]["sku"], "actions_macos");
}

#[test]
fn json_prices_match_the_table_for_each_runner_family() {
    let dir = scratch("families");
    let runs = r#"{"workflow_runs":[{"id":9,"path":".github/workflows/ci.yml","run_attempt":1,"run_number":1,"event":"push","conclusion":"success","run_started_at":"2026-07-01T00:00:00Z"}]}"#;
    std::fs::write(dir.join("runs-9.json"), runs).unwrap();
    let jobs = r#"{
  "jobs": [
    {"conclusion":"success","labels":["ubuntu-slim"],"name":"slim","completed_at":"2026-07-01T00:02:00Z","started_at":"2026-07-01T00:00:00Z","run_attempt":1,"run_id":9,"id":1,"runner_name":null},
    {"labels":["ubuntu-22.04-arm"],"conclusion":"success","name":"arm","completed_at":"2026-07-01T00:02:00Z","started_at":"2026-07-01T00:00:00Z","run_id":9,"run_attempt":1,"id":2,"runner_name":null},
    {"name":"large","labels":["macos-14-large"],"conclusion":"success","completed_at":"2026-07-01T00:02:00Z","started_at":"2026-07-01T00:00:00Z","id":3,"run_id":9,"run_attempt":1,"runner_name":null},
    {"name":"xlarge","labels":["xcode-27-xlarge"],"conclusion":"success","completed_at":"2026-07-01T00:02:00Z","started_at":"2026-07-01T00:00:00Z","id":4,"run_id":9,"run_attempt":1,"runner_name":null},
    {"name":"cores","labels":["ubuntu-24.04-16core"],"conclusion":"success","completed_at":"2026-07-01T00:02:00Z","started_at":"2026-07-01T00:00:00Z","id":5,"run_id":9,"run_attempt":1,"runner_name":null},
    {"name":"hosted","labels":["self-hosted","macos-latest"],"conclusion":"success","completed_at":"2026-07-01T00:02:00Z","started_at":"2026-07-01T00:00:00Z","id":6,"run_id":9,"run_attempt":1,"runner_name":null},
    {"name":"winarm","labels":["windows-11-arm"],"conclusion":"success","completed_at":"2026-07-01T00:02:00Z","started_at":"2026-07-01T00:00:00Z","id":7,"run_id":9,"run_attempt":1,"runner_name":null}
  ]
}"#;
    std::fs::write(dir.join("jobs-9.json"), jobs).unwrap();
    std::fs::write(dir.join("usage.csv"), USAGE).unwrap();
    let output = report(&[
        "report",
        "--usage",
        dir.join("usage.csv").to_str().unwrap(),
        "--runs",
        dir.to_str().unwrap(),
        "--json",
    ]);
    assert_exit(&output, 0);
    let value: serde_json::Value = serde_json::from_str(stdout(&output).trim_end()).unwrap();
    let listed = value["workflow_runs"][0]["jobs"].as_array().unwrap();
    let price = |id: u64| {
        listed
            .iter()
            .find(|job| job["id"].as_u64() == Some(id))
            .unwrap()
            .clone()
    };
    let expect = [
        (1, "actions_linux_slim", "0.002000", "0.004000"),
        (2, "actions_linux_arm", "0.005000", "0.010000"),
        (3, "actions_macos_l", "0.077000", "0.154000"),
        (4, "actions_macos_xl", "0.102000", "0.204000"),
        (5, "actions_linux_16_core", "0.042000", "0.084000"),
        (6, "self_hosted", "0.000000", "0.000000"),
        (7, "actions_windows_arm", "0.010000", "0.020000"),
    ];
    for (id, sku, rate, list) in expect {
        let job = price(id);
        assert_eq!(job["minutes"], 2, "{id}");
        assert_eq!(job["sku"], sku, "{id}");
        assert_eq!(job["usd_per_minute"], rate, "{id}");
        assert_eq!(job["list_price"], list, "{id}");
    }
    // Highest list price first: xlarge, large, 16-core, windows arm, arm, slim, self-hosted.
    assert_eq!(
        listed
            .iter()
            .map(|job| job["id"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![4, 3, 5, 7, 2, 1, 6]
    );
}
