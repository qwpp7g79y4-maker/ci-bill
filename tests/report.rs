use std::process::Command;

fn money_micros(text: &str) -> i64 {
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

fn assert_money(value: &serde_json::Value) {
    let text = value
        .as_str()
        .unwrap_or_else(|| panic!("money string: {value}"));
    let digits = text.trim_start_matches('-');
    let (whole, frac) = digits.split_once('.').unwrap_or_else(|| panic!("{text}"));
    assert!(
        !whole.is_empty() && whole.chars().all(|c| c.is_ascii_digit()),
        "{text}"
    );
    assert_eq!(frac.len(), 6, "{text}");
    assert!(frac.chars().all(|c| c.is_ascii_digit()), "{text}");
}

fn report(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ci-bill"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(args)
        .output()
        .expect("run ci-bill")
}

#[test]
fn sample_report_matches_snapshot() {
    let output = report(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--workflows",
        "samples/.github/workflows",
    ]);
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stderr:\n{stderr}\nstdout:\n{stdout}"
    );
    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.contains("2026-09-01 .. 2026-09-30"), "{stdout}");
    assert!(stdout.contains("example-org/sample-app"), "{stdout}");
    assert!(stdout.contains("Other products: 0.00\n"), "{stdout}");
    let expected = include_str!("snapshots/report.txt");
    assert_eq!(stdout, expected);
    assert!(!stdout.contains("Sample"), "{stdout}");
    assert!(!stdout.starts_with('{'), "{stdout}");
}

#[test]
fn sample_json_matches_snapshot_and_fields() {
    let output = report(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--workflows",
        "samples/.github/workflows",
        "--json",
    ]);
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stderr:\n{stderr}\nstdout:\n{stdout}"
    );
    assert!(stderr.is_empty(), "{stderr}");
    assert!(!stdout.contains("USD (from net_amount)"), "{stdout}");
    assert!(!stdout.contains("Sample"), "{stdout}");
    let expected = include_str!("snapshots/report.json");
    assert_eq!(stdout, expected);

    assert!(!stdout.contains("0000000001"), "{stdout}");
    assert!(!stdout.contains("9999999"), "{stdout}");
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim_end()).expect("one json object");
    assert_eq!(value["schema_version"], 2);
    assert_eq!(value["currency"], "USD");
    assert_eq!(value["money_unit"], "USD");
    assert_eq!(value["total_net"], "284.208000");
    assert_eq!(value["other_products_net"], "0.000000");
    assert!(value["total_minutes"].is_number(), "{value}");
    assert_eq!(value["total_minutes"].as_i64(), Some(12208));
    assert_eq!(value["period"]["from"], "2026-09-01");
    assert_eq!(value["period"]["to"], "2026-09-30");
    assert_eq!(
        value["repositories"],
        serde_json::json!(["example-org/sample-app"])
    );

    let workflows = value["workflows"].as_array().expect("workflows");
    assert_eq!(workflows.len(), 5);
    let paths: Vec<_> = workflows
        .iter()
        .map(|item| item["path"].as_str().expect("path"))
        .collect();
    assert_eq!(
        paths,
        vec![
            ".github/workflows/lint.yml",
            ".github/workflows/ci.yml",
            ".github/workflows/release.yml",
            ".github/workflows/docs.yml",
            "(no workflow)",
        ]
    );

    let total_net = money_micros(value["total_net"].as_str().unwrap());
    let mut net_sum = 0;
    for workflow in workflows {
        assert_money(workflow.get("net").unwrap());
        assert_money(workflow.get("gross").unwrap());
        assert_money(workflow.get("discount").unwrap());
        assert!(workflow["minutes"].is_number());
        assert_money(workflow.get("share").unwrap());
        assert_eq!(workflow["repository"], "example-org/sample-app");
        net_sum += money_micros(workflow["net"].as_str().unwrap());
        for sku in workflow["skus"].as_array().expect("skus") {
            assert!(sku["sku"].is_string());
            assert_money(sku.get("net").unwrap());
            assert!(sku["minutes"].is_number());
            assert!(sku.get("gross").is_none());
        }
        for finding in workflow["findings"].as_array().expect("findings") {
            assert!(finding["id"].is_string());
            assert!(finding["file"].is_string());
            assert!(finding["line"].is_number());
            assert!(finding["message"].is_string());
            assert_money(finding.get("savings").expect("savings"));
            assert!(finding["method"].is_string());
        }
    }
    assert_eq!(net_sum, total_net);

    let lint = &workflows[0];
    assert_eq!(lint["net"], "230.144000");
    assert_eq!(lint["minutes"].as_i64(), Some(3712));
    assert_eq!(lint["skus"][0]["sku"], "actions_macos");
    assert_eq!(lint["skus"].as_array().unwrap().len(), 1);
    let lint_findings = lint["findings"].as_array().unwrap();
    assert_eq!(lint_findings.len(), 3);
    assert_eq!(lint_findings[0]["id"], "F1");
    assert_eq!(lint_findings[0]["line"], 8);
    assert_eq!(lint_findings[0]["message"], "lint on macOS");
    assert_eq!(
        lint_findings[0]["file"],
        "samples/.github/workflows/lint.yml"
    );
    assert_eq!(lint_findings[0]["savings"], "207.872000");
    assert_eq!(lint_findings[0]["method"], "linux_rate");
    assert_eq!(lint_findings[1]["id"], "F2");
    assert_eq!(lint_findings[1]["line"], 7);
    assert_eq!(lint_findings[1]["savings"], "22.320000");
    assert_eq!(lint_findings[1]["method"], "stuck_run");
    assert_eq!(lint_findings[2]["id"], "F3");
    assert_eq!(lint_findings[2]["line"], 2);
    assert_eq!(
        lint_findings[2]["message"],
        "pull_request without concurrency cancel-in-progress"
    );
    assert_eq!(lint_findings[2]["savings"], "0.000000");
    assert_eq!(lint_findings[2]["method"], "overlap");
    assert_eq!(
        lint["steps"],
        serde_json::json!([
            {"line": 10, "name": "actions/checkout@v4"},
            {"line": 11, "name": "cargo fmt --check"},
            {"line": 12, "name": "cargo clippy -- -D warnings"}
        ])
    );
    for step in lint["steps"].as_array().unwrap() {
        assert_eq!(step.as_object().unwrap().len(), 2, "{step}");
        assert!(step["line"].is_number());
        assert!(step["name"].is_string());
    }

    assert!(workflows[1]["findings"].as_array().unwrap().is_empty());
    assert_eq!(
        workflows[1]["steps"],
        serde_json::json!([
            {"line": 12, "name": "actions/checkout@v4"},
            {"line": 13, "name": "cargo test"}
        ])
    );
    assert_eq!(workflows[1]["skus"][0]["sku"], "actions_linux");
    assert_eq!(workflows[1]["minutes"].as_i64(), Some(7372));
    assert_eq!(workflows[1]["net"], "44.232000");

    assert!(workflows[2]["findings"].as_array().unwrap().is_empty());
    assert_eq!(
        workflows[2]["steps"],
        serde_json::json!([
            {"line": 10, "name": "actions/checkout@v4"},
            {"line": 11, "name": "cargo build --release"}
        ])
    );
    assert_eq!(workflows[2]["skus"][0]["sku"], "actions_windows");
    assert_eq!(workflows[2]["minutes"].as_i64(), Some(772));
    assert_eq!(workflows[2]["net"], "7.720000");

    let docs = &workflows[3];
    assert_eq!(docs["skus"][0]["sku"], "actions_linux");
    assert_eq!(docs["minutes"].as_i64(), Some(352));
    assert_eq!(docs["net"], "2.112000");
    let docs_findings = docs["findings"].as_array().unwrap();
    assert_eq!(docs_findings.len(), 1);
    assert_eq!(docs_findings[0]["id"], "F2");
    assert_eq!(docs_findings[0]["line"], 4);
    assert_eq!(
        docs_findings[0]["file"],
        "samples/.github/workflows/docs.yml"
    );
    assert_eq!(docs_findings[0]["savings"], "2.160000");
    assert_eq!(docs_findings[0]["method"], "stuck_run");
    assert_eq!(
        docs_findings[0]["message"],
        "default timeout is 360 minutes"
    );
    assert_eq!(
        docs["steps"],
        serde_json::json!([
            {"line": 7, "name": "actions/checkout@v4"},
            {"line": 8, "name": "cargo doc --no-deps"}
        ])
    );

    let storage = &workflows[4];
    assert_eq!(storage["path"], "(no workflow)");
    assert_eq!(storage["net"], "0.000000");
    assert_eq!(storage["minutes"].as_i64(), Some(0));
    assert_eq!(storage["gross"], "0.005040");
    assert_eq!(storage["skus"][0]["sku"], "actions_storage");
    assert!(storage["findings"].as_array().unwrap().is_empty());
    assert_eq!(storage["steps"], serde_json::json!([]));

    assert!(value["unattached_findings"].as_array().unwrap().is_empty());
    assert!(value.get("budget").is_none(), "{value}");
    assert!(value.get("budget_exceeded").is_none(), "{value}");
}
