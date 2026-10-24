use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ci-bill"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(args)
        .output()
        .expect("run ci-bill")
}

fn run_with_stdin(args: &[&str], input: &[u8]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ci-bill"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ci-bill");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(input)
        .expect("write ci-bill stdin");
    child.wait_with_output().expect("wait for ci-bill")
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn tmp(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("prices")
        .join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    path
}

fn write(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, bytes).unwrap();
}

const HEADER: &str = "date,product,sku,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,net_amount,username,organization,repository,workflow_path";

fn usage_csv(rows: &[&str]) -> String {
    let mut text = String::from(HEADER);
    text.push('\n');
    for row in rows {
        text.push_str(row);
        text.push('\n');
    }
    text
}

fn quote_json(args: &[&str]) -> Value {
    let output = run(args);
    let out = stdout(&output);
    let err = stderr(&output);
    assert!(
        output.status.success(),
        "args: {args:?}\nstderr:\n{err}\nstdout:\n{out}"
    );
    assert!(err.is_empty(), "{err}");
    serde_json::from_str(&out).unwrap_or_else(|err| panic!("{err}\n{out}"))
}

fn rate_of(sku: &str, table: &Value) -> (String, bool) {
    let rows = table["skus"].as_array().unwrap();
    let row = rows
        .iter()
        .find(|row| row["sku"] == sku)
        .unwrap_or_else(|| panic!("missing {sku} in price file"));
    let text = row["usd_per_minute"].as_str().unwrap();
    let micros = decimal_micros(text);
    (
        format_micros(micros),
        row["billed_on_public"].as_bool().unwrap(),
    )
}

fn decimal_micros(text: &str) -> i64 {
    let negative = text.starts_with('-');
    let text = text.trim_start_matches(['+', '-']);
    let (whole, frac) = text.split_once('.').unwrap_or((text, ""));
    assert!(frac.len() <= 6, "{text}");
    let mut padded = frac.to_string();
    padded.push_str(&"0".repeat(6 - frac.len()));
    let value = whole.parse::<i64>().unwrap() * 1_000_000 + padded.parse::<i64>().unwrap();
    if negative {
        -value
    } else {
        value
    }
}

fn format_micros(value: i64) -> String {
    let neg = value < 0;
    let abs = value.unsigned_abs();
    let sign = if neg && abs != 0 { "-" } else { "" };
    format!("{sign}{}.{:06}", abs / 1_000_000, abs % 1_000_000)
}

fn assert_label(raw: &str, sku: &str, table: &Value) {
    let value = quote_json(&["prices", "--label", raw, "--json"]);
    assert_eq!(value["sku"], sku, "{raw}\n{value}");
    let (rate, billed) = rate_of(sku, table);
    assert_eq!(value["usd_per_minute"], rate, "{raw}");
    assert_eq!(value["billed_on_public"], billed, "{raw}");
    assert_eq!(value["source"], "table", "{raw}");
    assert!(value.get("repository").is_none(), "{raw}");
    let labels = value["labels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap().to_ascii_lowercase())
        .collect::<Vec<_>>();
    assert!(!labels.is_empty(), "{raw}");
}

#[test]
fn prices_text_and_json_match_snapshots() {
    let text = run(&["prices"]);
    let out = stdout(&text);
    let err = stderr(&text);
    assert!(text.status.success(), "{err}\n{out}");
    assert!(err.is_empty(), "{err}");
    assert_eq!(out, include_str!("snapshots/prices.txt"));

    let json = run(&["prices", "--json"]);
    let out = stdout(&json);
    let err = stderr(&json);
    assert!(json.status.success(), "{err}\n{out}");
    assert!(err.is_empty(), "{err}");
    assert_eq!(out, include_str!("snapshots/prices.json"));
    let value: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["retrieved"], "2026-10-17");
    assert_eq!(value["effective_from"], "2026-01-01");
    assert_eq!(value["currency"], "USD");
    assert_eq!(value["unit"], "minute");
    assert!(value["sources"].as_array().unwrap().len() >= 2);
    for sku in value["skus"].as_array().unwrap() {
        let rate = sku["usd_per_minute"].as_str().unwrap();
        assert!(
            rate.contains('.') && rate.split_once('.').unwrap().1.len() == 6,
            "{rate}"
        );
        assert!(!rate.contains("0000000001"), "{rate}");
        assert!(!rate.contains('e'), "{rate}");
    }
}

#[test]
fn every_documented_label_resolves() {
    let file =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("prices/actions.json"))
            .unwrap();
    let table: Value = serde_json::from_str(&file).unwrap();
    let cases = [
        ("self-hosted", "self_hosted"),
        ("[self-hosted, macos]", "self_hosted"),
        ("[macos, self-hosted]", "self_hosted"),
        ("self-hosted, macos", "self_hosted"),
        ("[\"self-hosted\", \"macos\"]", "self_hosted"),
        ("[ self-hosted , macos ]", "self_hosted"),
        ("ubuntu-slim", "actions_linux_slim"),
        ("ubuntu-latest", "actions_linux"),
        ("ubuntu-22.04", "actions_linux"),
        ("ubuntu-24.04", "actions_linux"),
        ("ubuntu-26.04", "actions_linux"),
        ("ubuntu-22.04-arm", "actions_linux_arm"),
        ("ubuntu-24.04-arm", "actions_linux_arm"),
        ("ubuntu-26.04-arm", "actions_linux_arm"),
        ("windows-11-arm", "actions_windows_arm"),
        ("windows-11-vs2026-arm", "actions_windows_arm"),
        ("macos-latest-xlarge", "actions_macos_xl"),
        ("macos-14-xlarge", "actions_macos_xl"),
        ("macos-15-xlarge", "actions_macos_xl"),
        ("macos-26-xlarge", "actions_macos_xl"),
        ("xcode-27-xlarge", "actions_macos_xl"),
        ("macos-latest-large", "actions_macos_l"),
        ("macos-14-large", "actions_macos_l"),
        ("macos-15-large", "actions_macos_l"),
        ("macos-26-large", "actions_macos_l"),
        ("ubuntu-latest", "actions_linux"),
        ("macos-latest", "actions_macos"),
        ("macos-14", "actions_macos"),
        ("macos-15", "actions_macos"),
        ("macos-26", "actions_macos"),
        ("macos-15-intel", "actions_macos"),
        ("macos-26-intel", "actions_macos"),
        ("xcode-27", "actions_macos"),
        ("windows-latest", "actions_windows"),
        ("windows-2022", "actions_windows"),
        ("windows-2025", "actions_windows"),
        ("windows-2025-vs2026", "actions_windows"),
        ("ubuntu-22.04-4core", "actions_linux_4_core"),
        ("ubuntu-24.04-8core", "actions_linux_8_core"),
        ("ubuntu-24.04-16core", "actions_linux_16_core"),
        ("ubuntu-24.04-32core", "actions_linux_32_core"),
        ("ubuntu-24.04-64core", "actions_linux_64_core"),
        ("ubuntu-24.04-96core", "actions_linux_96_core"),
        ("ubuntu-24.04-16-core", "actions_linux_16_core"),
        ("ubuntu-24.04-16cores", "actions_linux_16_core"),
        (
            "ubuntu-24.04-2core-advanced",
            "actions_linux_2_core_advanced",
        ),
        ("ubuntu-24.04-2core-arm", "actions_linux_2_core_arm"),
        ("ubuntu-24.04-4core-arm", "actions_linux_4_core_arm"),
        ("ubuntu-24.04-8core-arm", "actions_linux_8_core_arm"),
        ("ubuntu-24.04-16core-arm", "actions_linux_16_core_arm"),
        ("ubuntu-24.04-32core-arm", "actions_linux_32_core_arm"),
        ("ubuntu-24.04-64core-arm", "actions_linux_64_core_arm"),
        ("ubuntu-22.04-4core-gpu", "actions_linux_4_core_gpu"),
        ("windows-2022-4core", "actions_windows_4_core"),
        ("windows-2022-8core", "actions_windows_8_core"),
        ("windows-2025-16core", "actions_windows_16_core"),
        ("windows-2025-32core", "actions_windows_32_core"),
        ("windows-2025-64core", "actions_windows_64_core"),
        ("windows-2025-96core", "actions_windows_96_core"),
        ("windows-2022-2core-arm", "actions_windows_2_core_arm"),
        ("windows-2022-4core-arm", "actions_windows_4_core_arm"),
        ("windows-2022-8core-arm", "actions_windows_8_core_arm"),
        ("windows-2022-16core-arm", "actions_windows_16_core_arm"),
        ("windows-2022-32core-arm", "actions_windows_32_core_arm"),
        ("windows-2022-64core-arm", "actions_windows_64_core_arm"),
        ("windows-2022-4core-gpu", "actions_windows_4_core_gpu"),
    ];
    for (label, sku) in cases {
        assert_label(label, sku, &table);
        assert_label(&label.to_ascii_uppercase(), sku, &table);
    }
}

#[test]
fn standard_runners_are_free_on_public_and_larger_runners_are_billed() {
    let value = quote_json(&["prices", "--json"]);
    let standard = [
        "actions_linux",
        "actions_linux_arm",
        "actions_linux_slim",
        "actions_macos",
        "actions_windows",
        "actions_windows_arm",
        "self_hosted",
    ];
    let skus = value["skus"].as_array().unwrap();
    assert!(skus.len() > standard.len());
    for sku in skus {
        let name = sku["sku"].as_str().unwrap();
        let billed = sku["billed_on_public"].as_bool().unwrap();
        assert_eq!(billed, !standard.contains(&name), "{name}");
    }
    let slim = quote_json(&["prices", "--label", "ubuntu-slim", "--json"]);
    let plain = quote_json(&["prices", "--label", "ubuntu-latest", "--json"]);
    assert_ne!(slim["sku"], plain["sku"]);
    assert_eq!(slim["billed_on_public"], false);
    let advanced = quote_json(&["prices", "--label", "ubuntu-24.04-2core-advanced", "--json"]);
    assert_eq!(advanced["sku"], "actions_linux_2_core_advanced");
    assert_eq!(advanced["billed_on_public"], true);
    assert_ne!(advanced["usd_per_minute"], slim["usd_per_minute"]);
    let arm = quote_json(&["prices", "--label", "ubuntu-24.04-arm", "--json"]);
    let arm_large = quote_json(&["prices", "--label", "ubuntu-24.04-4core-arm", "--json"]);
    assert_eq!(arm["billed_on_public"], false);
    assert_eq!(arm_large["billed_on_public"], true);
    assert_ne!(arm["sku"], arm_large["sku"]);
}

#[test]
fn unknown_label_exits_3_and_names_it() {
    for label in [
        "not-a-runner",
        "macos",
        "windows",
        "ubuntu",
        "ubuntu-24.04-3core",
        "No-Such-Runner",
    ] {
        let output = run(&["prices", "--label", label]);
        let err = stderr(&output);
        let out = stdout(&output);
        assert_eq!(output.status.code(), Some(3), "{label}\n{err}\n{out}");
        assert!(out.is_empty(), "{label}\n{out}");
        assert!(err.contains(label), "{label}\n{err}");
        assert!(err.contains("unknown runner label"), "{err}");
    }
    let listed = run(&["prices", "--label", "[ubuntu-latest, not-a-runner]"]);
    let err = stderr(&listed);
    assert_eq!(listed.status.code(), Some(3), "{err}");
    assert!(err.contains("not-a-runner"), "{err}");
    assert!(
        !stdout(&listed).contains("actions_linux"),
        "{}",
        stdout(&listed)
    );

    let blank = run(&["prices", "--label", ""]);
    assert_eq!(blank.status.code(), Some(3));
    assert!(
        stderr(&blank).contains("unknown runner label"),
        "{}",
        stderr(&blank)
    );

    let known = run(&["prices", "--label", "ubuntu-latest"]);
    assert!(known.status.success(), "{}", stderr(&known));
}

#[test]
fn help_lists_price_flags() {
    let output = run(&["prices", "--help"]);
    let text = stdout(&output);
    assert!(output.status.success(), "{}", stderr(&output));
    for flag in [
        "--json",
        "--runner-map",
        "--label",
        "--sku",
        "--usage",
        "--repository",
        "--workflow",
    ] {
        assert!(text.contains(flag), "{flag}\n{text}");
    }
    let unknown = run(&["prices", "--no-such-option"]);
    assert_eq!(unknown.status.code(), Some(3));
}

#[test]
fn csv_rate_wins_over_the_table() {
    let table = quote_json(&["prices", "--label", "ubuntu-latest", "--json"]);
    assert_eq!(table["source"], "table");
    assert_eq!(table["usd_per_minute"], "0.006000");

    let path = tmp("csv-wins/basic.csv");
    write(
        &path,
        usage_csv(&[
            "2026-09-01,actions,actions_linux,10,minutes,0.008,0.080,0,0.080,user,example-org,example-org/sample-app,.github/workflows/ci.yml",
            "2026-09-01,actions,actions_linux,10,minutes,0.006,0.060,0,0.060,user,example-org,example-org/sample-app,.github/workflows/lint.yml",
            "2026-09-01,actions,actions_macos,5,minutes,0.080,0.400,0,0.400,user,example-org,example-org/sample-app,.github/workflows/ci.yml",
        ])
        .as_bytes(),
    );
    let value = quote_json(&[
        "prices",
        "--label",
        "ubuntu-latest",
        "--json",
        "--usage",
        path.to_str().unwrap(),
        "--repository",
        "example-org/sample-app",
        "--workflow",
        ".github/workflows/ci.yml",
    ]);
    assert_eq!(value["sku"], "actions_linux");
    assert_eq!(value["usd_per_minute"], "0.008000");
    assert_eq!(value["source"], "csv");
    assert_ne!(value["usd_per_minute"], table["usd_per_minute"]);
    assert_eq!(value["repository"], "example-org/sample-app");
    assert_eq!(value["workflow_path"], ".github/workflows/ci.yml");

    let lint = quote_json(&[
        "prices",
        "--label",
        "ubuntu-latest",
        "--json",
        "--usage",
        path.to_str().unwrap(),
        "--workflow",
        ".github/workflows/lint.yml",
    ]);
    assert_eq!(lint["usd_per_minute"], "0.006000");
    assert_eq!(lint["source"], "csv");

    let no_workflow = quote_json(&[
        "prices",
        "--label",
        "ubuntu-latest",
        "--json",
        "--usage",
        path.to_str().unwrap(),
        "--repository",
        "example-org/sample-app",
    ]);
    assert_eq!(no_workflow["source"], "table");
    assert_eq!(no_workflow["usd_per_minute"], "0.006000");
}

#[test]
fn csv_rate_variants_crlf_order_case_empty_and_missing() {
    let shuffled = "\
workflow_path,GROSS_AMOUNT,extra,net_amount,Unit_Type,quantity,SKU,Repository,Date,PRODUCT,username,organization,discount_amount,applied_cost_per_quantity\r\n\
.GitHub/Workflows/CI.yml,0.200,ignore,0.200,MINUTES,40,ACTIONS_LINUX,Example-Org/Sample-App,2026-09-01,Actions,user,Example-Org,0,0.005\r\n\
.github/workflows/other.yml,1.000,ignore,1.000,minutes,10,actions_linux,example-org/sample-app,2026-09-02,actions,user,example-org,0,0.100\r\n";
    let path = tmp("csv-wins/shuffled.csv");
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend(shuffled.as_bytes());
    write(&path, &bytes);
    let value = quote_json(&[
        "prices",
        "--label",
        "UBUNTU-LATEST",
        "--json",
        "--usage",
        path.to_str().unwrap(),
        "--repository",
        "example-org/sample-app",
        "--workflow",
        ".github/workflows/ci.yml",
    ]);
    assert_eq!(value["usd_per_minute"], "0.005000", "{value}");
    assert_eq!(value["source"], "csv");
    assert_eq!(value["repository"], "Example-Org/Sample-App");

    let weighted = tmp("csv-wins/weighted.csv");
    write(
        &weighted,
        usage_csv(&[
            "2026-09-01,actions,actions_linux,10,minutes,0.008,0.080,0,0.080,user,org,org/app,.github/workflows/ci.yml",
            "2026-09-02,actions,actions_linux,30,minutes,0.004,0.120,0,0.120,user,org,org/app,.github/workflows/ci.yml",
        ])
        .as_bytes(),
    );
    let value = quote_json(&[
        "prices",
        "--sku",
        "actions_linux",
        "--json",
        "--usage",
        weighted.to_str().unwrap(),
        "--workflow",
        ".github/workflows/ci.yml",
    ]);
    assert_eq!(value["usd_per_minute"], "0.005000", "{value}");
    assert_eq!(value["source"], "csv");

    let discount = tmp("csv-wins/discount.csv");
    write(
        &discount,
        usage_csv(&["2026-09-01,actions,actions_linux,100,minutes,0.006,0.600,0.100,0.500,user,org,org/app,ci.yml"]).as_bytes(),
    );
    let value = quote_json(&[
        "prices",
        "--label",
        "ubuntu-latest",
        "--json",
        "--usage",
        discount.to_str().unwrap(),
        "--workflow",
        "ci.yml",
    ]);
    assert_eq!(value["usd_per_minute"], "0.005000", "{value}");
    assert_eq!(value["source"], "csv");

    let free = tmp("csv-wins/zero-net.csv");
    write(
        &free,
        usage_csv(&["2026-09-01,actions,actions_linux,10,minutes,0.006,0.060,0.060,0,user,org,org/app,ci.yml"]).as_bytes(),
    );
    let value = quote_json(&[
        "prices",
        "--label",
        "ubuntu-latest",
        "--json",
        "--usage",
        free.to_str().unwrap(),
        "--workflow",
        "ci.yml",
    ]);
    assert_eq!(value["usd_per_minute"], "0.000000", "{value}");
    assert_eq!(value["source"], "csv");

    let zero_qty = tmp("csv-wins/zero-qty.csv");
    write(
        &zero_qty,
        usage_csv(&[
            "2026-09-01,actions,actions_linux,0,minutes,0.006,0,0,0,user,org,org/app,ci.yml",
        ])
        .as_bytes(),
    );
    let value = quote_json(&[
        "prices",
        "--label",
        "ubuntu-latest",
        "--json",
        "--usage",
        zero_qty.to_str().unwrap(),
        "--workflow",
        "ci.yml",
    ]);
    assert_eq!(value["source"], "table");
    assert_eq!(value["usd_per_minute"], "0.006000");

    let empty_path = tmp("csv-wins/empty-path.csv");
    write(
        &empty_path,
        usage_csv(&[
            "2026-09-01,actions,actions_linux,10,minutes,0.009,0.090,0,0.090,user,org,org/app,",
            "2026-09-01,copilot,actions_linux,10,minutes,0.020,0.200,0,0.200,user,org,org/app,ci.yml",
        ])
        .as_bytes(),
    );
    let named = quote_json(&[
        "prices",
        "--label",
        "ubuntu-latest",
        "--json",
        "--usage",
        empty_path.to_str().unwrap(),
        "--workflow",
        "ci.yml",
    ]);
    assert_eq!(named["source"], "table", "{named}");
    assert_eq!(named["usd_per_minute"], "0.006000");
    let unnamed = quote_json(&[
        "prices",
        "--label",
        "ubuntu-latest",
        "--json",
        "--usage",
        empty_path.to_str().unwrap(),
        "--workflow",
        "(no workflow)",
    ]);
    assert_eq!(unnamed["usd_per_minute"], "0.009000", "{unnamed}");
    assert_eq!(unnamed["source"], "csv");

    let rounding = tmp("csv-wins/rounding.csv");
    write(
        &rounding,
        usage_csv(&[
            "2026-09-01,actions,actions_linux,3,minutes,1,2,0,2,user,org,org/app,ci.yml",
            "2026-09-01,actions,actions_macos,3,minutes,1,1,0,1,user,org,org/app,ci.yml",
            "2026-09-01,actions,actions_windows,2,minutes,0.000001,0.000001,0,0.000001,user,org,org/app,ci.yml",
        ])
        .as_bytes(),
    );
    let two_thirds = quote_json(&[
        "prices",
        "--label",
        "ubuntu-latest",
        "--json",
        "--usage",
        rounding.to_str().unwrap(),
        "--workflow",
        "ci.yml",
    ]);
    assert_eq!(two_thirds["usd_per_minute"], "0.666667", "{two_thirds}");
    let one_third = quote_json(&[
        "prices",
        "--label",
        "macos-latest",
        "--json",
        "--usage",
        rounding.to_str().unwrap(),
        "--workflow",
        "ci.yml",
    ]);
    assert_eq!(one_third["usd_per_minute"], "0.333333", "{one_third}");
    let half = quote_json(&[
        "prices",
        "--label",
        "windows-latest",
        "--json",
        "--usage",
        rounding.to_str().unwrap(),
        "--workflow",
        "ci.yml",
    ]);
    assert_eq!(half["usd_per_minute"], "0.000001", "{half}");

    let missing = tmp("csv-wins/missing-net.csv");
    write(
        &missing,
        b"date,product,sku,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,username,organization,repository,workflow_path\n2026-09-01,actions,actions_linux,1,minutes,0.006,0.006,0,user,org,org/app,ci.yml\n",
    );
    let output = run(&[
        "prices",
        "--label",
        "ubuntu-latest",
        "--usage",
        missing.to_str().unwrap(),
        "--workflow",
        "ci.yml",
    ]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let err = stderr(&output);
    assert!(err.contains("missing column net_amount"), "{err}");
    assert!(stdout(&output).is_empty(), "{}", stdout(&output));

    let empty_net = tmp("csv-wins/empty-net.csv");
    write(
        &empty_net,
        usage_csv(&[
            "2026-09-01,actions,actions_linux,1,minutes,0.006,0.006,0,,user,org,org/app,ci.yml",
        ])
        .as_bytes(),
    );
    let output = run(&[
        "prices",
        "--label",
        "ubuntu-latest",
        "--usage",
        empty_net.to_str().unwrap(),
        "--workflow",
        "ci.yml",
    ]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let err = stderr(&output);
    assert!(err.contains("line 2"), "{err}");
    assert!(err.contains("net_amount is not a number"), "{err}");

    let empty_qty = tmp("csv-wins/empty-qty.csv");
    write(
        &empty_qty,
        usage_csv(&[
            "2026-09-01,actions,actions_linux,,minutes,0.006,0.006,0,0.006,user,org,org/app,ci.yml",
        ])
        .as_bytes(),
    );
    let output = run(&[
        "prices",
        "--label",
        "ubuntu-latest",
        "--usage",
        empty_qty.to_str().unwrap(),
        "--workflow",
        "ci.yml",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("quantity is not a number"),
        "{}",
        stderr(&output)
    );

    let absent = run(&[
        "prices",
        "--label",
        "ubuntu-latest",
        "--usage",
        "/no/such/ci-bill-usage.csv",
        "--workflow",
        "ci.yml",
    ]);
    assert_eq!(absent.status.code(), Some(2));
    assert!(
        stderr(&absent).contains("cannot read file"),
        "{}",
        stderr(&absent)
    );
}

#[test]
fn two_repositories_stay_separate() {
    let path = tmp("csv-wins/two-repos.csv");
    let body = "\
date,product,sku,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,net_amount,username,organization,repository,workflow_path\r\n\
2026-09-01,actions,actions_linux,10,minutes,0.008,0.080,0,0.080,user,org,org/one,.github/workflows/ci.yml\r\n\
2026-09-01,actions,actions_linux,10,minutes,0.004,0.040,0,0.040,user,org,org/two,.github/workflows/ci.yml\r\n";
    write(&path, body.as_bytes());
    let mixed = run(&[
        "prices",
        "--label",
        "ubuntu-latest",
        "--usage",
        path.to_str().unwrap(),
        "--workflow",
        ".github/workflows/ci.yml",
    ]);
    assert_eq!(mixed.status.code(), Some(2), "{}", stderr(&mixed));
    let err = stderr(&mixed);
    assert!(err.contains("org/one"), "{err}");
    assert!(err.contains("org/two"), "{err}");
    assert!(err.contains(".github/workflows/ci.yml"), "{err}");

    let one = quote_json(&[
        "prices",
        "--label",
        "ubuntu-latest",
        "--json",
        "--usage",
        path.to_str().unwrap(),
        "--repository",
        "ORG/ONE",
        "--workflow",
        ".github/workflows/ci.yml",
    ]);
    assert_eq!(one["usd_per_minute"], "0.008000", "{one}");
    assert_eq!(one["repository"], "org/one");
    let two = quote_json(&[
        "prices",
        "--label",
        "ubuntu-latest",
        "--json",
        "--usage",
        path.to_str().unwrap(),
        "--repository",
        "org/two",
        "--workflow",
        ".github/workflows/ci.yml",
    ]);
    assert_eq!(two["usd_per_minute"], "0.004000", "{two}");
}

#[test]
fn runner_map_overrides_and_rejects_bad_rows() {
    let path = tmp("runner-map/override.csv");
    write(
        &path,
        b"sku,note,label\nactions_macos,ignore,ubuntu-latest\nactions_linux,ignore,my-box\nactions_linux_16_core,ignore,ubuntu-latest\n",
    );
    let overridden = quote_json(&[
        "prices",
        "--label",
        "UBUNTU-LATEST",
        "--json",
        "--runner-map",
        path.to_str().unwrap(),
    ]);
    assert_eq!(overridden["sku"], "actions_linux_16_core", "{overridden}");
    assert_eq!(overridden["billed_on_public"], true);
    let custom = quote_json(&[
        "prices",
        "--label",
        "My-Box",
        "--json",
        "--runner-map",
        path.to_str().unwrap(),
    ]);
    assert_eq!(custom["sku"], "actions_linux");
    let hosted = quote_json(&[
        "prices",
        "--label",
        "[self-hosted, macos]",
        "--json",
        "--runner-map",
        path.to_str().unwrap(),
    ]);
    assert_eq!(hosted["sku"], "self_hosted");
    assert_eq!(hosted["usd_per_minute"], "0.000000");

    let crlf = tmp("runner-map/crlf.csv");
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend(b"SKU,LABEL\r\nACTIONS_MACOS,UBUNTU-LATEST\r\nself_hosted,SELF-HOSTED\r\nactions_windows,\"[self-hosted, macos]\"\r\n");
    write(&crlf, &bytes);
    let value = quote_json(&[
        "prices",
        "--label",
        "ubuntu-latest",
        "--json",
        "--runner-map",
        crlf.to_str().unwrap(),
    ]);
    assert_eq!(value["sku"], "actions_macos");
    let forced = quote_json(&[
        "prices",
        "--label",
        "[SELF-HOSTED, MACOS]",
        "--json",
        "--runner-map",
        crlf.to_str().unwrap(),
    ]);
    assert_eq!(forced["sku"], "actions_windows", "{forced}");
    let only = quote_json(&[
        "prices",
        "--label",
        "self-hosted",
        "--json",
        "--runner-map",
        crlf.to_str().unwrap(),
    ]);
    assert_eq!(only["sku"], "self_hosted");

    let usage = tmp("runner-map/usage.csv");
    write(
        &usage,
        usage_csv(&["2026-09-01,actions,actions_macos,4,minutes,0.050,0.200,0,0.200,user,org,org/app,ci.yml"]).as_bytes(),
    );
    let priced = quote_json(&[
        "prices",
        "--label",
        "ubuntu-latest",
        "--json",
        "--runner-map",
        crlf.to_str().unwrap(),
        "--usage",
        usage.to_str().unwrap(),
        "--workflow",
        "ci.yml",
    ]);
    assert_eq!(priced["sku"], "actions_macos");
    assert_eq!(priced["source"], "csv");
    assert_eq!(priced["usd_per_minute"], "0.050000", "{priced}");

    let empty_sku = tmp("runner-map/empty-sku.csv");
    write(&empty_sku, b"label,sku\nmy-box,\n");
    let output = run(&[
        "prices",
        "--label",
        "my-box",
        "--runner-map",
        empty_sku.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let err = stderr(&output);
    assert!(err.contains("line 2"), "{err}");
    assert!(err.contains("empty sku"), "{err}");
    assert!(!err.contains("my-box"), "{err}");

    let empty_label = tmp("runner-map/empty-label.csv");
    write(&empty_label, b"label,sku\r\n,actions_linux\r\n");
    let output = run(&["prices", "--runner-map", empty_label.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(stderr(&output).contains("line 2"), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("empty label"),
        "{}",
        stderr(&output)
    );

    let missing_col = tmp("runner-map/missing.csv");
    write(&missing_col, b"label,name\nmy-box,actions_linux\n");
    let output = run(&["prices", "--runner-map", missing_col.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("missing column sku"),
        "{}",
        stderr(&output)
    );

    let bad_sku = tmp("runner-map/bad-sku.csv");
    write(&bad_sku, b"label,sku\nmy-box,actions_not_real\n");
    let output = run(&[
        "prices",
        "--label",
        "my-box",
        "--runner-map",
        bad_sku.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let err = stderr(&output);
    assert!(err.contains("actions_not_real"), "{err}");
    assert!(err.contains("line 2"), "{err}");

    let unreadable = run(&["prices", "--runner-map", "/no/such/runner-map.csv"]);
    assert_eq!(unreadable.status.code(), Some(2));
    assert!(
        stderr(&unreadable).contains("cannot read file"),
        "{}",
        stderr(&unreadable)
    );

    let mismatch = run(&[
        "prices",
        "--label",
        "ubuntu-latest",
        "--sku",
        "actions_macos",
    ]);
    assert_eq!(mismatch.status.code(), Some(3), "{}", stderr(&mismatch));
    let err = stderr(&mismatch);
    assert!(err.contains("ubuntu-latest"), "{err}");
    assert!(err.contains("actions_linux"), "{err}");
    assert!(err.contains("actions_macos"), "{err}");

    let unknown_sku = run(&["prices", "--sku", "actions_not_real"]);
    assert_eq!(
        unknown_sku.status.code(),
        Some(2),
        "{}",
        stderr(&unknown_sku)
    );
    assert!(
        stderr(&unknown_sku).contains("actions_not_real"),
        "{}",
        stderr(&unknown_sku)
    );

    let needs_usage = run(&["prices", "--workflow", "ci.yml"]);
    assert_eq!(needs_usage.status.code(), Some(3));
    assert!(
        stderr(&needs_usage).contains("--workflow requires --usage"),
        "{}",
        stderr(&needs_usage)
    );
}

#[test]
fn runner_map_dash_reads_stdin() {
    let path = tmp("runner-map/stdin-source.csv");
    let csv = b"label,sku\nmy-box,actions_linux\n";
    write(&path, csv);

    let from_file = quote_json(&[
        "prices",
        "--label",
        "my-box",
        "--json",
        "--runner-map",
        path.to_str().unwrap(),
    ]);

    let output = run_with_stdin(
        &["prices", "--label", "my-box", "--json", "--runner-map", "-"],
        csv,
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
    let from_stdin: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(from_stdin, from_file);
}

#[test]
fn usage_and_runner_map_both_dash_is_rejected_before_reading_stdin() {
    let output = run_with_stdin(
        &[
            "prices",
            "--label",
            "my-box",
            "--runner-map",
            "-",
            "--usage",
            "-",
        ],
        b"label,sku\nmy-box,actions_linux\n",
    );
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert_eq!(
        stderr(&output),
        "ci-bill: --usage and --runner-map can't both read standard input; give one of them a file\n"
    );
    assert!(stdout(&output).is_empty(), "{}", stdout(&output));
}

#[test]
fn csv_net_amount_sum_overflow_is_a_clean_error_not_a_panic() {
    let path = tmp("overflow/huge.csv");
    write(
        &path,
        usage_csv(&[
            "2026-09-01,actions,actions_linux,1,minutes,9000000000000,9000000000000,0,9000000000000,user,example-org,example-org/sample-app,.github/workflows/ci.yml",
            "2026-09-02,actions,actions_linux,1,minutes,9000000000000,9000000000000,0,9000000000000,user,example-org,example-org/sample-app,.github/workflows/ci.yml",
        ])
        .as_bytes(),
    );
    let output = run(&[
        "prices",
        "--sku",
        "actions_linux",
        "--usage",
        path.to_str().unwrap(),
        "--workflow",
        ".github/workflows/ci.yml",
    ]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert!(stdout(&output).is_empty(), "{}", stdout(&output));
    let err = stderr(&output);
    assert!(err.contains("net_amount total is out of range"), "{err}");
}

#[test]
fn source_files_do_not_embed_runner_rates() {
    let file =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("prices/actions.json"))
            .unwrap();
    let table: Value = serde_json::from_str(&file).unwrap();
    let mut rates = Vec::new();
    for sku in table["skus"].as_array().unwrap() {
        let rate = sku["usd_per_minute"].as_str().unwrap();
        if rate.contains('.') {
            rates.push(rate.to_string());
        }
    }
    assert!(rates.len() > 10);
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for name in ["src/prices.rs", "src/main.rs"] {
        let text = std::fs::read_to_string(root.join(name)).unwrap();
        for rate in &rates {
            assert!(!text.contains(rate), "{name} contains embedded rate {rate}");
        }
    }
}
