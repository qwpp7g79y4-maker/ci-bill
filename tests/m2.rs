use std::process::Command;

const HEADER: &str = "date,product,sku,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,net_amount,username,organization,repository,workflow_path";

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

fn report_text(body: &str, name: &str) -> (String, String) {
    let path = write_usage(name, body);
    let output = run(&["report", "--usage", &path]);
    assert_exit(&output, 0);
    (path, stdout(&output))
}

fn report_json(body: &str, name: &str) -> (String, serde_json::Value) {
    let path = write_usage(name, body);
    let output = run(&["report", "--usage", &path, "--json"]);
    assert_exit(&output, 0);
    (path, parse_json(&output))
}

/// 118 * 0.062 = 7.316. The CSV net is the total even when the unit price is tweaked.
const BALANCED_LINT: &str = "2026-09-01,actions,actions_macos,118,minutes,0.062,7.316,0,7.316,user,example-org,example-org/app,.github/workflows/lint.yml";
const TWEAKED_LINT: &str = "2026-09-01,actions,actions_macos,118,minutes,0.080,7.316,0,7.316,user,example-org,example-org/app,.github/workflows/lint.yml";
const BALANCED_CI: &str = "2026-09-02,actions,actions_linux,10,minutes,0.1,1,0,1,user,example-org,example-org/app,.github/workflows/ci.yml";

#[test]
fn sample_has_no_f9_and_shows_storage_gross() {
    let output = run(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--workflows",
        "samples/.github/workflows",
    ]);
    assert_exit(&output, 0);
    let text = stdout(&output);
    assert!(!text.contains("F9"), "{text}");
    assert!(!text.contains("invoice row mismatch"), "{text}");
    assert!(text.contains("Gross"), "{text}");
    assert!(text.contains("Discount"), "{text}");
    let storage = text
        .lines()
        .find(|line| line.contains("actions_storage"))
        .expect("storage sku");
    assert!(storage.contains("0.00"), "{storage}");
    assert_eq!(storage.matches("0.01").count(), 2, "{storage}");
    let workflow = text
        .lines()
        .find(|line| line.contains("(no workflow)"))
        .expect("storage workflow");
    assert_eq!(workflow.matches("0.01").count(), 2, "{workflow}");

    let json = run(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--workflows",
        "samples/.github/workflows",
        "--json",
    ]);
    assert_exit(&json, 0);
    let value = parse_json(&json);
    assert_eq!(value["schema_version"], 2);
    assert_eq!(value["total_net"], "284.208000");
    let findings: Vec<_> = value["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|workflow| workflow["findings"].as_array().unwrap())
        .collect();
    assert!(findings.iter().all(|finding| finding["id"] != "F9"));
    let storage_json = value["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|workflow| workflow["path"] == "(no workflow)")
        .unwrap();
    assert_eq!(storage_json["net"], "0.000000");
    assert_eq!(storage_json["gross"], "0.005040");
    assert_eq!(storage_json["discount"], "0.005040");
}

#[test]
fn quantity_118_times_0_062_is_7_316() {
    for line_end in ["\n", "\r\n"] {
        let name = if line_end == "\n" {
            "m2-118-lf.csv"
        } else {
            "m2-118-crlf.csv"
        };
        let body = format!("{HEADER}{line_end}{BALANCED_LINT}{line_end}");
        let (path, value) = report_json(&body, name);
        assert_eq!(value["total_net"], "7.316000", "{line_end:?}");
        assert_eq!(value["workflows"][0]["net"], "7.316000");
        assert_eq!(value["workflows"][0]["gross"], "7.316000");
        assert_eq!(value["workflows"][0]["discount"], "0.000000");
        assert!(value["workflows"][0]["findings"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(value["unattached_findings"].as_array().unwrap().is_empty());
        let text = run(&["report", "--usage", &path]);
        assert_exit(&text, 0);
        let rendered = stdout(&text);
        assert!(!rendered.contains("F9"), "{rendered}");
        assert!(!rendered.contains("invoice row mismatch"), "{rendered}");
        assert!(rendered.contains("Total net: 7.32\n"), "{rendered}");
    }
}

#[test]
fn tweaked_unit_price_flags_f9_and_keeps_csv_net() {
    let body = format!("{HEADER}\n{TWEAKED_LINT}\n{BALANCED_CI}\n");
    let (path, value) = report_json(&body, "m2-tweaked.json.csv");
    assert_eq!(value["schema_version"], 2);
    assert_eq!(value["money_unit"], "USD");
    assert_eq!(value["total_net"], "8.316000", "{value}");
    assert_ne!(value["total_net"], "10.440000");
    let workflows = value["workflows"].as_array().unwrap();
    assert_eq!(workflows[0]["path"], ".github/workflows/lint.yml");
    assert_eq!(workflows[0]["net"], "7.316000");
    assert_eq!(workflows[0]["gross"], "7.316000");
    assert_ne!(workflows[0]["gross"], "9.440000");
    let f9 = &workflows[0]["findings"][0];
    assert_eq!(f9["id"], "F9");
    assert_eq!(f9["file"], path);
    assert_eq!(f9["line"], 2);
    assert_eq!(
        f9["message"],
        "invoice row mismatch: sku \"actions_macos\", date \"2026-09-01\", gross 7.316000 != quantity * applied_cost_per_quantity 9.440000"
    );
    assert!(workflows[1]["findings"].as_array().unwrap().is_empty());
    assert_eq!(workflows[1]["net"], "1.000000");

    let text = run(&["report", "--usage", &path]);
    assert_exit(&text, 0);
    let rendered = stdout(&text);
    assert!(rendered.contains("Total net: 8.32\n"), "{rendered}");
    assert!(rendered.contains("9.440000"), "{rendered}");
    assert!(!rendered.contains("Total net: 10.44"), "{rendered}");
    assert!(!rendered.contains("Total net: 9.44"), "{rendered}");
    let lint = rendered
        .find("example-org/app .github/workflows/lint.yml")
        .unwrap();
    let ci = rendered
        .find("example-org/app .github/workflows/ci.yml")
        .unwrap();
    let f9_at = rendered.find("F9 ").expect("f9 line");
    assert!(lint < f9_at && f9_at < ci, "{rendered}");
    assert!(rendered[f9_at..].contains(&format!("{path}:2: invoice row mismatch")));
    assert_eq!(
        rendered.matches("invoice row mismatch").count(),
        1,
        "{rendered}"
    );
    assert!(!rendered.contains("Workflows without usage"), "{rendered}");
}

#[test]
fn tweaked_price_survives_crlf_column_order_and_header_case() {
    let header = [
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
    // repo, path, net, gross, discount, price, quantity, unit, sku, product, date, user, org
    let tweaked = "example-org/app,.github/workflows/lint.yml,7.316,7.316,0,0.080,118,MINUTES,Actions_MacOS,ACTIONS,2026-09-01,user,example-org";
    let balanced = "example-org/app,.github/workflows/ci.yml,1,1,0,0.1,10,minutes,actions_linux,actions,2026-09-02,user,example-org";
    let body = format!("{header}\r\n{tweaked}\r\n{balanced}\r\n");
    let (path, value) = report_json(&body, "m2-tweaked-crlf.csv");
    assert_eq!(value["total_net"], "8.316000", "{value}");
    assert_eq!(value["workflows"][0]["net"], "7.316000");
    assert_eq!(value["workflows"][0]["gross"], "7.316000");
    let f9 = &value["workflows"][0]["findings"][0];
    assert_eq!(f9["id"], "F9");
    assert_eq!(f9["file"], path);
    assert_eq!(f9["line"], 2);
    assert_eq!(
        f9["message"],
        "invoice row mismatch: sku \"Actions_MacOS\", date \"2026-09-01\", gross 7.316000 != quantity * applied_cost_per_quantity 9.440000"
    );
    assert!(value["workflows"][1]["findings"]
        .as_array()
        .unwrap()
        .is_empty());
    let text = stdout(&run(&["report", "--usage", &path]));
    assert!(text.contains("F9 "), "{text}");
    assert!(text.contains("Actions_MacOS"), "{text}");
    assert!(text.contains("9.440000"), "{text}");
    assert!(!text.contains("Total net: 10.44"), "{text}");
}

#[test]
fn net_mismatch_keeps_the_csv_net() {
    let row = "2026-09-01,actions,actions_macos,118,minutes,0.062,7.316,0,7.000,user,example-org,example-org/app,.github/workflows/lint.yml";
    let body = format!("{HEADER}\n{row}\n");
    let (_, value) = report_json(&body, "m2-net.csv");
    assert_eq!(value["total_net"], "7.000000");
    assert_ne!(value["total_net"], "7.316000");
    assert_eq!(value["workflows"][0]["net"], "7.000000");
    assert_eq!(value["workflows"][0]["gross"], "7.316000");
    assert_eq!(
        value["workflows"][0]["findings"][0]["message"],
        "invoice row mismatch: sku \"actions_macos\", date \"2026-09-01\", net 7.000000 != gross - discount 7.316000"
    );
}

#[test]
fn discount_row_matches_when_net_equals_gross_minus_discount() {
    let row = "2026-09-01,actions,actions_macos,118,minutes,0.062,7.316,1,6.316,user,example-org,example-org/app,.github/workflows/lint.yml";
    let body = format!("{HEADER}\n{row}\n");
    let (_, value) = report_json(&body, "m2-discount.csv");
    assert_eq!(value["total_net"], "6.316000");
    assert_eq!(value["workflows"][0]["gross"], "7.316000");
    assert_eq!(value["workflows"][0]["discount"], "1.000000");
    assert_eq!(value["workflows"][0]["net"], "6.316000");
    assert!(value["workflows"][0]["findings"]
        .as_array()
        .unwrap()
        .is_empty());
    let (_, text) = report_text(&body, "m2-discount.txt.csv");
    assert!(!text.contains("F9"), "{text}");
    assert!(text.contains("1.00"), "{text}");
    assert!(text.contains("6.32"), "{text}");
}

#[test]
fn both_checks_fail_in_one_finding() {
    let row = "2026-09-14,actions,actions_macos,118,minutes,0.080,7.316,1,7.316,user,example-org,example-org/app,.github/workflows/lint.yml";
    let body = format!("{HEADER}\r\n{row}\r\n");
    let (path, value) = report_json(&body, "m2-both.csv");
    assert_eq!(value["total_net"], "7.316000");
    assert_ne!(value["total_net"], "6.316000");
    assert_ne!(value["total_net"], "9.440000");
    let f9 = &value["workflows"][0]["findings"][0];
    assert_eq!(f9["line"], 2);
    assert_eq!(f9["file"], path);
    assert_eq!(
        f9["message"],
        "invoice row mismatch: sku \"actions_macos\", date \"2026-09-14\", gross 7.316000 != quantity * applied_cost_per_quantity 9.440000, net 7.316000 != gross - discount 6.316000"
    );
    assert_eq!(
        value["workflows"][0]["findings"].as_array().unwrap().len(),
        1
    );
}

#[test]
fn one_micro_is_quiet_and_two_micros_flag() {
    let quiet = "2026-09-01,actions,actions_macos,118,minutes,0.062,7.316001,0,7.316001,user,example-org,example-org/app,.github/workflows/lint.yml";
    let (_, quiet_json) = report_json(&format!("{HEADER}\n{quiet}\n"), "m2-plus-one.csv");
    assert!(quiet_json["workflows"][0]["findings"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(quiet_json["total_net"], "7.316001");

    let loud = "2026-09-01,actions,actions_macos,118,minutes,0.062,7.316002,0,7.316002,user,example-org,example-org/app,.github/workflows/lint.yml";
    let (_, loud_json) = report_json(&format!("{HEADER}\n{loud}\n"), "m2-plus-two.csv");
    assert_eq!(loud_json["total_net"], "7.316002");
    assert!(loud_json["workflows"][0]["findings"][0]["message"]
        .as_str()
        .unwrap()
        .contains("gross 7.316002 != quantity * applied_cost_per_quantity 7.316000"));
}

#[test]
fn half_micro_rounds_before_the_one_micro_tolerance() {
    let within = "2026-09-03,actions,actions_storage,0.5,gigabyte-hours,0.000001,0.000002,0,0.000002,user,example-org,example-org/app,";
    let (_, within_json) = report_json(&format!("{HEADER}\n{within}\n"), "m2-half-ok.csv");
    assert_eq!(within_json["workflows"][0]["path"], "(no workflow)");
    assert!(within_json["workflows"][0]["findings"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(within_json["total_net"], "0.000002");

    let outside = "2026-09-03,actions,actions_storage,0.5,gigabyte-hours,0.000001,0.000003,0,0.000003,user,example-org,example-org/app,";
    let (_, outside_json) = report_json(&format!("{HEADER}\n{outside}\n"), "m2-half-bad.csv");
    assert_eq!(outside_json["total_net"], "0.000003");
    assert!(outside_json["workflows"][0]["findings"][0]["message"]
        .as_str()
        .unwrap()
        .contains("gross 0.000003 != quantity * applied_cost_per_quantity 0.000001"));
}

#[test]
fn empty_workflow_sku_and_date_still_flag() {
    let row = ",ACTIONS,,0.5,GIGABYTE-HOURS,0.000336,1,0,1,user,example-org,example-org/app,   ";
    let body = format!("{HEADER}\r\n{row}\r\n");
    let (path, value) = report_json(&body, "m2-empty-text.csv");
    assert_eq!(value["total_net"], "1.000000");
    assert_eq!(value["workflows"][0]["path"], "(no workflow)");
    assert_eq!(value["workflows"][0]["repository"], "example-org/app");
    let f9 = &value["workflows"][0]["findings"][0];
    assert_eq!(f9["id"], "F9");
    assert_eq!(f9["line"], 2);
    assert_eq!(f9["file"], path);
    assert!(
        f9["message"]
            .as_str()
            .unwrap()
            .contains("sku \"\", date \"\""),
        "{f9}"
    );
    assert!(
        f9["message"]
            .as_str()
            .unwrap()
            .contains("gross 1.000000 != quantity * applied_cost_per_quantity 0.000168"),
        "{f9}"
    );
    let (_, text) = report_text(&body, "m2-empty-text-out.csv");
    assert!(text.contains("(no workflow)"), "{text}");
    assert!(text.contains("F9 "), "{text}");
    assert!(!text.contains("Workflows without usage"), "{text}");
}

#[test]
fn missing_or_empty_money_field_exits_2_without_f9() {
    for column in [
        "quantity",
        "applied_cost_per_quantity",
        "gross_amount",
        "discount_amount",
        "net_amount",
    ] {
        let headers = HEADER
            .split(',')
            .filter(|name| *name != column)
            .collect::<Vec<_>>()
            .join(",");
        let path = write_usage(
            &format!("m2-missing-{column}.csv"),
            &format!("{headers}\r\nsecret-cell\r\n"),
        );
        let output = run(&["report", "--usage", &path]);
        assert_exit(&output, 2);
        let err = stderr(&output);
        assert!(
            err.contains(&format!("line 1: missing column {column}")),
            "{column}\n{err}"
        );
        assert!(!err.contains("secret-cell"), "{err}");
        assert!(!err.contains("invoice row mismatch"), "{err}");
        assert!(stdout(&output).is_empty(), "{}", stdout(&output));
    }

    for (column, index) in [
        ("quantity", 3),
        ("applied_cost_per_quantity", 5),
        ("gross_amount", 6),
        ("discount_amount", 7),
        ("net_amount", 8),
    ] {
        let mut fields = [
            "2026-09-01",
            "actions",
            "actions_macos",
            "118",
            "minutes",
            "0.062",
            "7.316",
            "0",
            "7.316",
            "user",
            "example-org",
            "example-org/app",
            ".github/workflows/lint.yml",
        ];
        fields[index] = "";
        let path = write_usage(
            &format!("m2-empty-{column}.csv"),
            &format!("{HEADER}\n{}\n", fields.join(",")),
        );
        let output = run(&["report", "--usage", &path]);
        assert_exit(&output, 2);
        let err = stderr(&output);
        assert!(
            err.contains(&format!("line 2: {column} is not a number")),
            "{column}\n{err}"
        );
        assert!(!err.contains("invoice row mismatch"), "{err}");
        assert!(stdout(&output).is_empty(), "{column}");
    }
}

#[test]
fn two_repositories_keep_f9_on_the_mismatched_row() {
    let header = "SKU,Date,QUANTITY,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,net_amount,product,username,organization,repository,workflow_path";
    let alpha = "actions_macos,2026-09-01,118,minutes,0.080,7.316,0,7.316,actions,user,example-org,example-org/alpha,.github/workflows/ci.yml";
    let zeta = "actions_macos,2026-09-01,118,minutes,0.062,7.316,0,7.316,ACTIONS,user,example-org,example-org/zeta,.github/workflows/ci.yml";
    let body = format!("{header}\r\n{zeta}\r\n{alpha}\r\n");
    let (path, value) = report_json(&body, "m2-two-repo.csv");
    assert_eq!(value["total_net"], "14.632000");
    let workflows = value["workflows"].as_array().unwrap();
    assert_eq!(workflows[0]["repository"], "example-org/alpha");
    assert_eq!(workflows[1]["repository"], "example-org/zeta");
    assert_eq!(workflows[0]["findings"][0]["line"], 3);
    assert_eq!(workflows[0]["findings"][0]["file"], path);
    assert!(workflows[0]["findings"][0]["message"]
        .as_str()
        .unwrap()
        .contains("9.440000"));
    assert!(workflows[1]["findings"].as_array().unwrap().is_empty());

    let text = stdout(&run(&["report", "--usage", &path]));
    let alpha_at = text
        .find("example-org/alpha .github/workflows/ci.yml")
        .unwrap();
    let zeta_at = text
        .find("example-org/zeta .github/workflows/ci.yml")
        .unwrap();
    let f9_at = text.find("F9 ").unwrap();
    assert!(alpha_at < f9_at && f9_at < zeta_at, "{text}");
    assert_eq!(text.matches("F9 ").count(), 1, "{text}");
    assert!(!text.contains("Workflows without usage"), "{text}");
}

#[test]
fn later_crlf_row_uses_the_physical_line() {
    let bad = "2026-09-03,actions,actions_linux,10,minutes,0.5,1,0,1,user,example-org,example-org/app,.github/workflows/ci.yml";
    let body = format!("{HEADER}\r\n{BALANCED_LINT}\r\n{bad}\r\n");
    let (path, value) = report_json(&body, "m2-line-3.csv");
    assert_eq!(value["total_net"], "8.316000");
    let workflows = value["workflows"].as_array().unwrap();
    let ci = workflows
        .iter()
        .find(|workflow| workflow["path"] == ".github/workflows/ci.yml")
        .unwrap();
    assert_eq!(ci["net"], "1.000000");
    assert_eq!(ci["findings"][0]["line"], 3);
    assert_eq!(ci["findings"][0]["file"], path);
    let lint = workflows
        .iter()
        .find(|workflow| workflow["path"] == ".github/workflows/lint.yml")
        .unwrap();
    assert!(lint["findings"].as_array().unwrap().is_empty());
}

#[test]
fn other_product_mismatch_is_reported_and_stays_out_of_the_actions_total() {
    let actions = "2026-09-01,actions,actions_linux,10,minutes,0.1,1,0,1,user,example-org,example-org/app,.github/workflows/ci.yml";
    let same_path = "2026-09-02,Copilot,copilot_premium_request,2,requests,1.5,1,0,1,user,example-org,example-org/app,.github/workflows/ci.yml";
    let other_path = "2026-09-03,COPILOT,copilot_premium_request,2,requests,1.5,1,0,1,user,example-org,example-org/app,.github/workflows/other.yml";
    let body = format!("{HEADER}\n{actions}\n{same_path}\n{other_path}\n");
    let (path, value) = report_json(&body, "m2-copilot.csv");
    assert_eq!(value["total_net"], "1.000000");
    assert_eq!(value["other_products_net"], "2.000000");
    assert_eq!(value["workflows"].as_array().unwrap().len(), 1);
    assert_eq!(value["workflows"][0]["net"], "1.000000");
    assert_eq!(value["workflows"][0]["gross"], "1.000000");
    let attached = &value["workflows"][0]["findings"][0];
    assert_eq!(attached["id"], "F9");
    assert_eq!(attached["line"], 3);
    assert_eq!(attached["file"], path);
    assert!(attached["message"]
        .as_str()
        .unwrap()
        .contains("sku \"copilot_premium_request\""));
    assert!(attached["message"]
        .as_str()
        .unwrap()
        .contains("gross 1.000000 != quantity * applied_cost_per_quantity 3.000000"));
    let loose = &value["unattached_findings"][0];
    assert_eq!(loose["id"], "F9");
    assert_eq!(loose["line"], 4);
    assert_eq!(loose["file"], path);
    let text = stdout(&run(&["report", "--usage", &path]));
    assert!(text.contains("Other products: 2.00\n"), "{text}");
    assert!(text.contains("Total net: 1.00\n"), "{text}");
    let workflow = text
        .find("example-org/app .github/workflows/ci.yml")
        .unwrap();
    let attached_at = text.find("F9 ").unwrap();
    let heading = text.find("Workflows without usage\n").unwrap();
    assert!(workflow < attached_at && attached_at < heading, "{text}");
    assert_eq!(text.matches("invoice row mismatch").count(), 2, "{text}");
}
