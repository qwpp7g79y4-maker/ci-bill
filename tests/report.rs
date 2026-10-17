use std::process::Command;

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

    let value: serde_json::Value =
        serde_json::from_str(stdout.trim_end()).expect("one json object");
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["currency"], "USD");
    assert!(value["total_net"].is_number(), "{value}");
    assert!(value["total_minutes"].is_number(), "{value}");
    assert_eq!(value["total_minutes"].as_f64(), Some(12208.0));

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

    let total_net = value["total_net"].as_f64().expect("total_net");
    assert_eq!(format!("{total_net:.3}"), "284.208");
    let mut net_sum = 0.0;
    for workflow in workflows {
        assert!(workflow["net"].is_number());
        assert!(workflow["gross"].is_number());
        assert!(workflow["minutes"].is_number());
        assert!(workflow["share"].is_number());
        let net = workflow["net"].as_f64().expect("net");
        net_sum += net;
        let share = workflow["share"].as_f64().expect("share");
        let expected_share = if total_net == 0.0 {
            0.0
        } else {
            net / total_net * 100.0
        };
        assert_eq!(share.to_bits(), expected_share.to_bits(), "{workflow}");
        for sku in workflow["skus"].as_array().expect("skus") {
            assert!(sku["sku"].is_string());
            assert!(sku["net"].is_number());
            assert!(sku["minutes"].is_number());
            assert!(sku.get("gross").is_none());
        }
        for finding in workflow["findings"].as_array().expect("findings") {
            assert!(finding["id"].is_string());
            assert!(finding["file"].is_string());
            assert!(finding["line"].is_number());
            assert!(finding["message"].is_string());
        }
    }
    assert_eq!(net_sum.to_bits(), total_net.to_bits());

    let lint = &workflows[0];
    assert_eq!(format!("{:.3}", lint["net"].as_f64().unwrap()), "230.144");
    assert_eq!(lint["minutes"].as_f64(), Some(3712.0));
    assert_eq!(lint["skus"][0]["sku"], "actions_macos");
    assert_eq!(lint["skus"].as_array().unwrap().len(), 1);
    let lint_findings = lint["findings"].as_array().unwrap();
    assert_eq!(lint_findings.len(), 3);
    assert_eq!(lint_findings[0]["id"], "F3");
    assert_eq!(lint_findings[0]["line"], 2);
    assert_eq!(
        lint_findings[0]["file"],
        "samples/.github/workflows/lint.yml"
    );
    assert_eq!(
        lint_findings[0]["message"],
        "pull_request without concurrency cancel-in-progress"
    );
    assert_eq!(lint_findings[1]["id"], "F2");
    assert_eq!(lint_findings[1]["line"], 7);
    assert_eq!(lint_findings[2]["id"], "F1");
    assert_eq!(lint_findings[2]["line"], 8);
    assert_eq!(lint_findings[2]["message"], "lint on macOS");
    assert_eq!(
        lint_findings[2]["file"],
        "samples/.github/workflows/lint.yml"
    );

    assert!(workflows[1]["findings"].as_array().unwrap().is_empty());
    assert_eq!(workflows[1]["skus"][0]["sku"], "actions_linux");
    assert_eq!(workflows[1]["minutes"].as_f64(), Some(7372.0));
    assert_eq!(
        format!("{:.3}", workflows[1]["net"].as_f64().unwrap()),
        "44.232"
    );

    assert!(workflows[2]["findings"].as_array().unwrap().is_empty());
    assert_eq!(workflows[2]["skus"][0]["sku"], "actions_windows");
    assert_eq!(workflows[2]["minutes"].as_f64(), Some(772.0));
    assert_eq!(
        format!("{:.3}", workflows[2]["net"].as_f64().unwrap()),
        "7.720"
    );

    let docs = &workflows[3];
    assert_eq!(docs["skus"][0]["sku"], "actions_linux");
    assert_eq!(docs["minutes"].as_f64(), Some(352.0));
    let docs_net = docs["net"].as_f64().unwrap();
    assert_eq!(format!("{docs_net:.3}"), "2.112");
    let docs_findings = docs["findings"].as_array().unwrap();
    assert_eq!(docs_findings.len(), 1);
    assert_eq!(docs_findings[0]["id"], "F2");
    assert_eq!(docs_findings[0]["line"], 4);
    assert_eq!(
        docs_findings[0]["file"],
        "samples/.github/workflows/docs.yml"
    );
    assert_eq!(
        docs_findings[0]["message"],
        "default timeout is 360 minutes"
    );

    let storage = &workflows[4];
    assert_eq!(storage["path"], "(no workflow)");
    assert_eq!(storage["net"].as_f64(), Some(0.0));
    assert_eq!(storage["minutes"].as_f64(), Some(0.0));
    assert!(storage["gross"].as_f64().unwrap() > 0.0);
    assert_eq!(storage["skus"][0]["sku"], "actions_storage");
    assert!(storage["findings"].as_array().unwrap().is_empty());

    assert!(value["unattached_findings"].as_array().unwrap().is_empty());
    assert!(value.get("budget").is_none(), "{value}");
    assert!(value.get("budget_exceeded").is_none(), "{value}");
}
