use std::process::Command;

/// Net 10, gross 20, minutes 12. Gross is higher so a budget check on gross would disagree.
const USAGE: &str = "\
sku,quantity,unit_type,net_amount,gross_amount,workflow_path
actions_linux,10,minutes,4,8,.github/workflows/ci.yml
actions_macos,2,minutes,6,12,.github/workflows/lint.yml
";

/// Net 0, gross 5. Budget 0 matches the net total.
const ZERO_USAGE: &str = "\
sku,quantity,unit_type,net_amount,gross_amount,workflow_path
actions_storage,1,gigabyte-hours,0,5,
";

fn run(args: &[&str]) -> std::process::Output {
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

fn write_usage(name: &str, body: &str) -> String {
    let path = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::write(&path, body).unwrap();
    path.into_os_string().into_string().unwrap()
}

fn parse_json(output: &std::process::Output) -> serde_json::Value {
    let text = stdout(output);
    serde_json::from_str(text.trim_end()).unwrap_or_else(|err| panic!("{err}\n{text}"))
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

#[test]
fn budget_under_total_exits_0() {
    let path = write_usage("budget-under.csv", USAGE);
    let plain = run(&["report", "--usage", &path]);
    assert_exit(&plain, 0);
    let plain_out = stdout(&plain);

    for amount in ["11", "10.125", "12", "100", "10.01"] {
        let text = run(&["report", "--usage", &path, "--budget", amount]);
        assert_exit(&text, 0);
        assert_eq!(stdout(&text), plain_out, "amount {amount}");
        assert!(stderr(&text).is_empty(), "{}", stderr(&text));
        assert!(!stdout(&text).contains("Budget exceeded"), "{amount}");

        let json = run(&["report", "--usage", &path, "--json", "--budget", amount]);
        assert_exit(&json, 0);
        assert!(stderr(&json).is_empty(), "{}", stderr(&json));
        let value = parse_json(&json);
        assert_eq!(value["budget_exceeded"], false, "{amount} {value}");
        assert!(value["budget"].is_number(), "{value}");
        assert_eq!(value["total_net"].as_f64(), Some(10.0), "{value}");
        assert!(!stdout(&json).contains("Budget exceeded"), "{amount}");
        assert!(!stdout(&json).contains("USD (from net_amount)"));
    }

    let wide = run(&["report", "--usage", &path, "--json", "--budget", "10.125"]);
    let value = parse_json(&wide);
    assert_eq!(
        value["budget"].as_f64().unwrap().to_bits(),
        10.125_f64.to_bits(),
        "budget must stay an unrounded JSON number"
    );
    assert_eq!(value["currency"], "USD");
    assert_eq!(value["schema_version"], 1);

    // 12 is above the net total (10) and below the gross total (20).
    let text = run(&["report", "--usage", &path, "--budget", "12"]);
    assert_exit(&text, 0);
    assert!(!stdout(&text).contains("Budget exceeded"));
}

#[test]
fn budget_equal_to_total_exits_0() {
    let path = write_usage("budget-equal.csv", USAGE);
    let plain = run(&["report", "--usage", &path]);
    assert_exit(&plain, 0);
    let plain_out = stdout(&plain);

    for amount in ["10", "10.0", "10.00", "+10", "1e1", "10e0"] {
        let text = run(&["report", "--usage", &path, "--budget", amount]);
        assert_exit(&text, 0);
        assert_eq!(stdout(&text), plain_out, "amount {amount}");
        assert!(stderr(&text).is_empty(), "{amount} {}", stderr(&text));
        assert!(!stdout(&text).contains("Budget exceeded"), "{amount}");

        let json = run(&["report", "--usage", &path, "--json", "--budget", amount]);
        assert_exit(&json, 0);
        assert!(stderr(&json).is_empty(), "{amount} {}", stderr(&json));
        let value = parse_json(&json);
        assert_eq!(value["budget_exceeded"], false, "{amount} {value}");
        assert_eq!(
            value["budget"].as_f64().unwrap().to_bits(),
            10.0_f64.to_bits(),
            "{amount}"
        );
        assert_eq!(
            value["total_net"].as_f64().unwrap().to_bits(),
            10.0_f64.to_bits()
        );
    }

    let zero_path = write_usage("budget-zero.csv", ZERO_USAGE);
    for amount in ["0", "0.0", "0.00", "+0", "0e0"] {
        let text = run(&["report", "--usage", &zero_path, "--budget", amount]);
        assert_exit(&text, 0);
        assert!(stderr(&text).is_empty(), "{amount} {}", stderr(&text));
        assert!(!stdout(&text).contains("Budget exceeded"), "{amount}");
        assert!(stdout(&text).contains("Total net: 0.00\n"), "{amount}");

        let json = run(&[
            "report", "--usage", &zero_path, "--json", "--budget", amount,
        ]);
        assert_exit(&json, 0);
        let value = parse_json(&json);
        assert_eq!(value["budget_exceeded"], false, "{amount} {value}");
        assert_eq!(value["total_net"].as_f64(), Some(0.0), "{value}");
        assert_eq!(value["budget"].as_f64(), Some(0.0), "{amount} {value}");
        // Gross is 5. The budget matches net, so this stays inside the budget.
        assert!(value["workflows"][0]["gross"].as_f64().unwrap() > 0.0);
    }
}

#[test]
fn budget_over_total_exits_1() {
    let path = write_usage("budget-over.csv", USAGE);
    let plain = run(&["report", "--usage", &path]);
    assert_exit(&plain, 0);
    let plain_out = stdout(&plain);

    let cases = [
        ("9.5", "Budget exceeded: 10.00 > 9.50"),
        ("9", "Budget exceeded: 10.00 > 9.00"),
        ("0", "Budget exceeded: 10.00 > 0.00"),
        ("9.25", "Budget exceeded: 10.00 > 9.25"),
        ("1", "Budget exceeded: 10.00 > 1.00"),
    ];
    for (amount, line) in cases {
        let text = run(&["report", "--usage", &path, "--budget", amount]);
        assert_exit(&text, 1);
        assert_eq!(
            stdout(&text),
            format!("{plain_out}{line}\n"),
            "amount {amount}"
        );
        assert!(stderr(&text).is_empty(), "{amount} {}", stderr(&text));
        assert!(stdout(&text).contains("Total net: 10.00\n"), "{amount}");
        assert!(stdout(&text).contains("USD (from net_amount)"), "{amount}");

        let json = run(&["report", "--usage", &path, "--json", "--budget", amount]);
        assert_exit(&json, 1);
        assert_eq!(stderr(&json), format!("{line}\n"), "{amount}");
        assert!(!stdout(&json).contains("Budget exceeded"), "{amount}");
        let value = parse_json(&json);
        assert_eq!(value["budget_exceeded"], true, "{amount} {value}");
        assert!(value["budget"].is_number(), "{value}");
        assert!(value["budget_exceeded"].is_boolean(), "{value}");
        assert_eq!(value["total_net"].as_f64(), Some(10.0), "{value}");
        assert_eq!(value["currency"], "USD");
        assert_eq!(value["schema_version"], 1);
        let parsed_budget = amount.parse::<f64>().unwrap();
        assert_eq!(
            value["budget"].as_f64().unwrap().to_bits(),
            parsed_budget.to_bits(),
            "{amount}"
        );
    }

    let top_plain = run(&["report", "--usage", &path, "--top", "1"]);
    let top = run(&["report", "--usage", &path, "--top", "1", "--budget", "7"]);
    assert_exit(&top_plain, 0);
    assert_exit(&top, 1);
    assert_eq!(
        stdout(&top),
        format!("{}Budget exceeded: 10.00 > 7.00\n", stdout(&top_plain))
    );
    assert!(!stdout(&top).contains("Budget exceeded: 6.00"));
    assert!(stdout(&top).contains("Total net: 10.00\n"));

    let top_json = run(&[
        "report", "--usage", &path, "--json", "--top", "1", "--budget", "7",
    ]);
    assert_exit(&top_json, 1);
    assert_eq!(stderr(&top_json), "Budget exceeded: 10.00 > 7.00\n");
    let value = parse_json(&top_json);
    assert_eq!(value["budget_exceeded"], true);
    assert_eq!(value["budget"].as_f64(), Some(7.0));
    assert_eq!(value["total_net"].as_f64(), Some(10.0));
    assert_eq!(value["workflows"].as_array().unwrap().len(), 1);
    assert_eq!(value["workflows"][0]["net"].as_f64(), Some(6.0));

    let sample_plain = run(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--workflows",
        "samples/.github/workflows",
    ]);
    assert_exit(&sample_plain, 0);
    let sample_out = stdout(&sample_plain);
    let shown = sample_out
        .lines()
        .find(|line| line.starts_with("Total net: "))
        .expect("total")
        .trim_start_matches("Total net: ");
    let sample_over = run(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--workflows",
        "samples/.github/workflows",
        "--budget",
        "1",
    ]);
    assert_exit(&sample_over, 1);
    assert_eq!(
        stdout(&sample_over),
        format!("{sample_out}Budget exceeded: {shown} > 1.00\n")
    );
    assert!(stdout(&sample_over).contains("F1"));
    assert!(stderr(&sample_over).is_empty());

    let sample_json = run(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--workflows",
        "samples/.github/workflows",
        "--json",
        "--budget",
        "1",
    ]);
    assert_exit(&sample_json, 1);
    assert_eq!(
        stderr(&sample_json),
        format!("Budget exceeded: {shown} > 1.00\n")
    );
    let value = parse_json(&sample_json);
    assert_eq!(value["budget_exceeded"], true);
    assert_eq!(value["budget"].as_f64(), Some(1.0));
    assert!(value["total_net"].as_f64().unwrap() > 1.0);
    assert!(!value["workflows"].as_array().unwrap().is_empty());
}

#[test]
fn invalid_budget_exits_3() {
    let path = write_usage("budget-invalid.csv", USAGE);
    let invalid = [
        ("-1", "negative"),
        ("-0.01", "negative"),
        ("-10", "negative"),
        ("-0", "negative"),
        ("-0.0", "negative"),
        ("NaN", "nan"),
        ("nan", "nan"),
        ("NAN", "nan"),
        ("+NaN", "nan"),
        ("+nan", "nan"),
        ("inf", "inf"),
        ("INF", "inf"),
        ("Infinity", "inf"),
        ("infinity", "inf"),
        ("+inf", "inf"),
        ("+Infinity", "inf"),
        ("-inf", "inf"),
        ("-Infinity", "inf"),
        ("1e309", "inf"),
        ("abc", "text"),
        ("ten", "text"),
        ("10usd", "text"),
        ("1,000", "text"),
        (" ", "text"),
        ("--", "text"),
        ("", "empty"),
    ];

    for (amount, kind) in invalid {
        for json in [false, true] {
            let mut args = vec!["report", "--usage", path.as_str(), "--budget", amount];
            if json {
                args.insert(1, "--json");
            }
            let output = run(&args);
            assert_eq!(
                output.status.code(),
                Some(3),
                "{kind} {amount:?} json={json}\nstdout:\n{}\nstderr:\n{}",
                stdout(&output),
                stderr(&output)
            );
            assert_ne!(output.status.code(), Some(1), "{kind} {amount:?}");
            assert!(
                stdout(&output).is_empty(),
                "{kind} {amount:?} json={json}\n{}",
                stdout(&output)
            );
            let err = stderr(&output);
            assert!(
                err.contains("finite USD amount"),
                "{kind} {amount:?} json={json}\n{err}"
            );
            assert!(!err.contains("Budget exceeded"), "{kind} {amount:?}\n{err}");
        }
    }

    for args in [
        vec!["report", "--usage", path.as_str(), "--budget"],
        vec!["report", "--usage", path.as_str(), "--json", "--budget"],
        vec!["report", "--usage", path.as_str(), "--budget="],
    ] {
        let output = run(&args);
        assert_eq!(
            output.status.code(),
            Some(3),
            "{args:?}\nstdout:\n{}\nstderr:\n{}",
            stdout(&output),
            stderr(&output)
        );
        assert!(stdout(&output).is_empty(), "{args:?}\n{}", stdout(&output));
        assert!(
            !stderr(&output).contains("Budget exceeded"),
            "{args:?}\n{}",
            stderr(&output)
        );
    }

    let missing_file = run(&[
        "report",
        "--usage",
        "/no/such/ci-bill-budget.csv",
        "--budget",
        "NaN",
    ]);
    assert_exit(&missing_file, 3);
    assert!(stdout(&missing_file).is_empty());
    assert!(stderr(&missing_file).contains("finite USD amount"));
}

#[test]
fn without_budget_never_exits_1() {
    let path = write_usage("budget-absent.csv", USAGE);
    let text = run(&["report", "--usage", &path]);
    assert_exit(&text, 0);
    assert!(stderr(&text).is_empty());
    assert!(!stdout(&text).contains("Budget exceeded"));
    assert!(stdout(&text).contains("Total net: 10.00\n"));

    let json = run(&["report", "--usage", &path, "--json"]);
    assert_exit(&json, 0);
    assert!(stderr(&json).is_empty());
    let value = parse_json(&json);
    assert!(value.get("budget").is_none(), "{value}");
    assert!(value.get("budget_exceeded").is_none(), "{value}");
    assert_eq!(value["total_net"].as_f64(), Some(10.0));

    let sample = run(&["report", "--usage", "samples/usage-sample.csv"]);
    assert_exit(&sample, 0);
    assert!(stderr(&sample).is_empty());
    assert!(!stdout(&sample).contains("Budget exceeded"));

    let sample_json = run(&["report", "--usage", "samples/usage-sample.csv", "--json"]);
    assert_exit(&sample_json, 0);
    let sample_value = parse_json(&sample_json);
    assert!(sample_value.get("budget").is_none(), "{sample_value}");
    assert!(
        sample_value.get("budget_exceeded").is_none(),
        "{sample_value}"
    );
    assert!(sample_value["total_net"].as_f64().unwrap() > 1.0);

    let with_workflows = run(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--workflows",
        "samples/.github/workflows",
        "--top",
        "2",
    ]);
    assert_exit(&with_workflows, 0);
    assert_ne!(with_workflows.status.code(), Some(1));
    assert!(!stdout(&with_workflows).contains("Budget exceeded"));
}
