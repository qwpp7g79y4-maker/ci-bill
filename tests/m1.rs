use std::process::Command;

const HEADER: &str =
    "date,product,sku,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,net_amount,username,organization,repository,workflow_path";

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

fn money_micros(text: &str) -> i64 {
    let negative = text.starts_with('-');
    let text = text.trim_start_matches('-');
    let (whole, frac) = text.split_once('.').unwrap_or_else(|| panic!("{text}"));
    assert_eq!(frac.len(), 6, "{text}");
    let value = whole.parse::<i64>().unwrap() * 1_000_000 + frac.parse::<i64>().unwrap();
    if negative {
        -value
    } else {
        value
    }
}

/// Two repositories, one shared workflow path, mixed case, CRLF, and a shuffled header.
fn two_repo_csv(line_end: &str) -> String {
    let shuffled = [
        "Repository",
        "WORKFLOW_PATH",
        "Net_Amount",
        "Gross_Amount",
        "Discount_Amount",
        "Applied_Cost_Per_Quantity",
        "Quantity",
        "UNIT_TYPE",
        "Sku",
        "Product",
        "DATE",
        "Username",
        "Organization",
    ]
    .join(",");
    let mut body = String::new();
    body.push_str(&shuffled);
    body.push_str(line_end);
    // repository, path, net, gross, discount, price, quantity, unit, sku, product, date, user, org
    for (repo, product, date, net) in [
        ("example-org/zeta", "actions", "2026-09-30", "4"),
        ("example-org/alpha", "ACTIONS", "2026-09-01", "4"),
    ] {
        body.push_str(&format!(
            "{repo},.github/workflows/ci.yml,{net},{net},0,0.4,10,minutes,actions_linux,{product},{date},ada,example-org{line_end}"
        ));
    }
    body
}

#[test]
fn sample_text_shows_period_and_repository() {
    let output = run(&["report", "--usage", "samples/usage-sample.csv"]);
    assert_exit(&output, 0);
    let text = stdout(&output);
    assert!(
        text.contains("Period: 2026-09-01 .. 2026-09-30\n"),
        "{text}"
    );
    assert!(
        text.contains("Repositories: example-org/sample-app\n"),
        "{text}"
    );
    assert!(text.contains("Total net: 284.21\n"), "{text}");
    assert!(text.contains("Other products: 0.00\n"), "{text}");
    assert!(!text.contains("0000000001"), "{text}");
    assert!(!text.contains("9999999"), "{text}");
}

#[test]
fn sample_json_money_is_exact_decimal_text() {
    let output = run(&["report", "--usage", "samples/usage-sample.csv", "--json"]);
    assert_exit(&output, 0);
    let text = stdout(&output);
    assert!(!text.contains("0000000001"), "{text}");
    assert!(!text.contains("9999999"), "{text}");
    let value = parse_json(&output);
    assert_eq!(value["schema_version"], 2);
    assert_eq!(value["money_unit"], "USD");
    assert_eq!(value["total_net"], "284.208000");
    assert_eq!(value["other_products_net"], "0.000000");
    assert_eq!(value["period"]["from"], "2026-09-01");
    assert_eq!(value["period"]["to"], "2026-09-30");
    let ci = value["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["path"] == ".github/workflows/ci.yml")
        .unwrap();
    assert_eq!(ci["net"], "44.232000");
    assert_eq!(ci["repository"], "example-org/sample-app");
    assert!(ci["net"].is_string());
    assert!(value["total_minutes"].is_number());
}

#[test]
fn two_repositories_with_the_same_workflow_stay_two_rows() {
    for (name, line_end) in [("two-repos-lf.csv", "\n"), ("two-repos-crlf.csv", "\r\n")] {
        let path = write_usage(name, &two_repo_csv(line_end));
        let text_out = run(&["report", "--usage", &path]);
        assert_exit(&text_out, 0);
        let text = stdout(&text_out);
        assert_eq!(
            text.matches(".github/workflows/ci.yml").count(),
            2,
            "{text}"
        );
        assert!(
            text.contains("example-org/alpha .github/workflows/ci.yml"),
            "{text}"
        );
        assert!(
            text.contains("example-org/zeta .github/workflows/ci.yml"),
            "{text}"
        );
        assert!(
            text.contains("Period: 2026-09-01 .. 2026-09-30\n"),
            "{text}"
        );
        assert!(
            text.contains("Repositories: example-org/alpha, example-org/zeta\n"),
            "{text}"
        );
        let alpha = text.find("example-org/alpha").unwrap();
        let zeta = text.find("example-org/zeta").unwrap();
        assert!(alpha < zeta, "equal net sorts by repository\n{text}");

        let json_out = run(&["report", "--usage", &path, "--json"]);
        assert_exit(&json_out, 0);
        let raw = stdout(&json_out);
        assert!(!raw.contains("0000000001"), "{raw}");
        assert!(!raw.contains("9999999"), "{raw}");
        let value = parse_json(&json_out);
        let workflows = value["workflows"].as_array().unwrap();
        assert_eq!(workflows.len(), 2);
        assert_eq!(workflows[0]["repository"], "example-org/alpha");
        assert_eq!(workflows[1]["repository"], "example-org/zeta");
        assert!(workflows
            .iter()
            .all(|item| item["path"] == ".github/workflows/ci.yml"));
        assert_eq!(workflows[0]["net"], "4.000000");
        assert_eq!(workflows[1]["net"], "4.000000");
        assert_eq!(value["total_net"], "8.000000");
        assert_eq!(
            money_micros(value["total_net"].as_str().unwrap()),
            8_000_000
        );
    }
}

#[test]
fn copilot_rows_stay_out_of_the_actions_total_and_budget() {
    for product in ["copilot", "Copilot", "COPILOT"] {
        let body = format!(
            "{HEADER}\n\
2026-09-01,actions,actions_linux,10,minutes,0.4,4,0,4,ada,example-org,example-org/app,.github/workflows/ci.yml\n\
2026-09-15,{product},copilot_premium_request,2,requests,1.5,3,0,3,ada,example-org,example-org/app,.github/workflows/ci.yml\n\
2026-09-20,Actions,actions_macos,1,MINUTES,1,1,0,1,ada,example-org,example-org/other,.github/workflows/ci.yml\n"
        );
        let path = write_usage(&format!("copilot-{product}.csv"), &body);
        let text_out = run(&["report", "--usage", &path]);
        assert_exit(&text_out, 0);
        let text = stdout(&text_out);
        assert!(text.contains("Total net: 5.00\n"), "{product}\n{text}");
        assert!(text.contains("Other products: 3.00\n"), "{product}\n{text}");
        assert!(text.contains("Total minutes: 11\n"), "{product}\n{text}");
        assert!(
            !text.contains("copilot_premium_request"),
            "{product}\n{text}"
        );
        assert_eq!(
            text.matches(".github/workflows/ci.yml").count(),
            2,
            "{product}\n{text}"
        );

        let inside = run(&["report", "--usage", &path, "--budget", "5"]);
        assert_exit(&inside, 0);
        assert!(!stdout(&inside).contains("Budget exceeded"), "{product}");

        let above_actions_only = run(&["report", "--usage", &path, "--budget", "7"]);
        assert_exit(&above_actions_only, 0);
        assert!(
            !stdout(&above_actions_only).contains("Budget exceeded"),
            "{product}"
        );

        let over = run(&["report", "--usage", &path, "--budget", "4", "--json"]);
        assert_exit(&over, 1);
        assert_eq!(stderr(&over), "Budget exceeded: 5.00 > 4.00\n");
        let value = parse_json(&over);
        assert_eq!(value["budget_exceeded"], true);
        assert_eq!(value["total_net"], "5.000000");
        assert_eq!(value["other_products_net"], "3.000000");
        assert_eq!(value["budget"], "4.000000");
        let actions: i64 = value["workflows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| money_micros(item["net"].as_str().unwrap()))
            .sum();
        assert_eq!(actions, money_micros(value["total_net"].as_str().unwrap()));
        assert_ne!(
            actions + money_micros(value["other_products_net"].as_str().unwrap()),
            actions
        );
    }
}

#[test]
fn seven_decimal_places_exit_2_without_the_cell() {
    let columns = [
        ("quantity", 3, "1.2345678"),
        ("applied_cost_per_quantity", 5, "0.0000001"),
        ("gross_amount", 6, "1e-7"),
        ("discount_amount", 7, "-0.0000001"),
        ("net_amount", 8, "12345678e-7"),
    ];
    for (column, index, token) in columns {
        for line_end in ["\n", "\r\n"] {
            let good = [
                "2026-09-01",
                "actions",
                "actions_linux",
                "1",
                "minutes",
                "0.000001",
                "1.234567",
                "0",
                "1.234567",
                "ada",
                "example-org",
                "example-org/app",
                "a.yml",
            ];
            let mut bad = good;
            bad[index] = token;
            let name = format!(
                "seven-{}-{}.csv",
                column,
                if line_end == "\n" { "lf" } else { "crlf" }
            );
            let body = format!(
                "{HEADER}{line_end}{}{line_end}{}{line_end}",
                good.join(","),
                bad.join(",")
            );
            let path = write_usage(&name, &body);
            let output = run(&["report", "--usage", &path]);
            assert_exit(&output, 2);
            let err = stderr(&output);
            assert!(
                err.contains(&format!("line 3: {column} has more than 6 decimal places")),
                "{column} {line_end:?}\n{err}"
            );
            assert!(!err.contains(token), "{column}\n{err}");
            assert!(!err.contains("1.2345678"), "{column}\n{err}");
            assert!(stdout(&output).is_empty(), "{column}\n{}", stdout(&output));
        }
    }
}

#[test]
fn non_numeric_money_exits_2_without_the_cell() {
    for (column, index, token) in [
        ("quantity", 3, "NOT_A_NUMBER"),
        ("applied_cost_per_quantity", 5, "NaN"),
        ("gross_amount", 6, "inf"),
        ("discount_amount", 7, "Infinity"),
        ("net_amount", 8, "12abc34"),
    ] {
        let mut fields = [
            "2026-09-01",
            "actions",
            "actions_linux",
            "1",
            "minutes",
            "0.006",
            "1",
            "0",
            "1",
            "ada",
            "example-org",
            "example-org/app",
            "a.yml",
        ];
        fields[index] = token;
        let path = write_usage(
            &format!("nan-{column}.csv"),
            &format!("{HEADER}\n{}\n", fields.join(",")),
        );
        let output = run(&["report", "--usage", &path]);
        assert_exit(&output, 2);
        let err = stderr(&output);
        assert!(
            err.contains(&format!("line 2: {column} is not a number")),
            "{column}\n{err}"
        );
        assert!(!err.contains(token), "{column}\n{err}");
        assert!(stdout(&output).is_empty(), "{}", stdout(&output));
    }
}

#[test]
fn missing_and_empty_fields() {
    for column in [
        "date",
        "product",
        "sku",
        "quantity",
        "unit_type",
        "applied_cost_per_quantity",
        "gross_amount",
        "discount_amount",
        "net_amount",
        "username",
        "organization",
        "repository",
        "workflow_path",
    ] {
        let headers = HEADER
            .split(',')
            .filter(|name| *name != column)
            .collect::<Vec<_>>()
            .join(",");
        let path = write_usage(
            &format!("missing-{column}.csv"),
            &format!("{headers}\nsecret-cell\n"),
        );
        let output = run(&["report", "--usage", &path]);
        assert_exit(&output, 2);
        let err = stderr(&output);
        assert!(
            err.contains(&format!("line 1: missing column {column}")),
            "{column}\n{err}"
        );
        assert!(!err.contains("secret-cell"), "{column}\n{err}");
    }

    let empty_numbers = format!(
        "{HEADER}\n\
2026-09-01,actions,actions_linux,,minutes,0.006,1,0,1,ada,example-org,example-org/app,a.yml\n"
    );
    let path = write_usage("empty-quantity.csv", &empty_numbers);
    let output = run(&["report", "--usage", &path]);
    assert_exit(&output, 2);
    assert!(stderr(&output).contains("line 2: quantity is not a number"));

    let blanks = format!(
        "{HEADER}\r\n\
,copilot,copilot_premium_request,1,requests,1,2,0,2,,, ,\r\n\
2026-09-08,ACTIONS,actions_linux,3,MINUTES,0.006,1,0,1, ,Example-Org,example-org/other, .github/workflows/ci.yml\r\n"
    );
    let path = write_usage("empty-text-crlf.csv", &blanks);
    let output = run(&["report", "--usage", &path, "--json"]);
    assert_exit(&output, 0);
    let value = parse_json(&output);
    assert_eq!(value["total_net"], "1.000000");
    assert_eq!(value["other_products_net"], "2.000000");
    assert_eq!(value["period"]["from"], "2026-09-08");
    assert_eq!(value["period"]["to"], "2026-09-08");
    assert_eq!(
        value["repositories"],
        serde_json::json!(["example-org/other"])
    );
    let workflows = value["workflows"].as_array().unwrap();
    assert_eq!(workflows.len(), 1);
    assert_eq!(workflows[0]["repository"], "example-org/other");
    assert_eq!(workflows[0]["path"], ".github/workflows/ci.yml");
    assert_eq!(workflows[0]["minutes"].as_i64(), Some(3));
    let text = run(&["report", "--usage", &path]);
    assert!(stdout(&text).contains("Other products: 2.00\n"));
    assert!(stdout(&text).contains("example-org/other .github/workflows/ci.yml"));
}
