use std::process::Command;

fn report(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ci-bill"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(args)
        .output()
        .expect("run ci-bill")
}

#[test]
fn sample_report_prices_f1_and_orders_findings_by_savings() {
    let output = report(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--workflows",
        "samples/.github/workflows",
        "--json",
    ]);
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stderr:\n{stderr}\nstdout:\n{stdout}"
    );
    assert!(stderr.is_empty(), "{stderr}");
    let value: serde_json::Value = serde_json::from_str(stdout.trim_end()).unwrap();
    let lint = &value["workflows"][0];
    let findings = lint["findings"].as_array().unwrap();
    assert_eq!(findings[0]["id"], "F1");
    assert_eq!(findings[0]["savings"], "207.872000");
    assert_eq!(findings[0]["method"], "linux_rate");
    assert_eq!(findings[1]["id"], "F2");
    assert_eq!(findings[1]["savings"], "22.320000");
    assert_eq!(findings[2]["id"], "F3");
    assert_eq!(findings[2]["savings"], "0.000000");
    let docs = &value["workflows"][3];
    assert_eq!(docs["findings"][0]["id"], "F2");
    assert_eq!(docs["findings"][0]["savings"], "2.160000");
    assert_eq!(docs["findings"][0]["method"], "stuck_run");
    for workflow in value["workflows"].as_array().unwrap() {
        for finding in workflow["findings"].as_array().unwrap() {
            assert!(finding["savings"].is_string(), "{finding}");
            assert!(finding["method"].is_string(), "{finding}");
        }
    }

    let text = report(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--workflows",
        "samples/.github/workflows",
    ]);
    let rendered = String::from_utf8(text.stdout).unwrap();
    assert!(text.status.success(), "{rendered}");
    let f1 = rendered.find("F1 ").unwrap();
    let f2 = rendered.find("F2 ").unwrap();
    let f3 = rendered.find("F3 ").unwrap();
    assert!(f1 < f2 && f2 < f3, "{rendered}");
    assert!(
        rendered.contains("savings 207.87 USD (linux_rate)"),
        "{rendered}"
    );
    let lower = rendered.to_ascii_lowercase();
    assert!(!lower.contains("not supported"), "{rendered}");
    assert!(!lower.contains("limitation"), "{rendered}");
}
