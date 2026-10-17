use std::path::{Path, PathBuf};
use std::process::Command;

const LINT_YML: &str = "\
name: Lint
on:
  pull_request:
jobs:
  lint:
    runs-on: macos-latest
    steps:
      - run: cargo clippy -- -D warnings
";

const DOCS_YML: &str = "\
name: Docs
on: push
jobs:
  docs:
    runs-on: ubuntu-latest
    steps:
      - run: cargo doc --no-deps
";

const NIGHTLY_YML: &str = "\
name: Nightly
on: push
jobs:
  nightly:
    runs-on: ubuntu-latest
    steps:
      - run: echo nightly
";

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

fn parse_json(output: &std::process::Output) -> serde_json::Value {
    let text = stdout(output);
    serde_json::from_str(text.trim_end()).unwrap_or_else(|err| panic!("{err}\n{text}"))
}

fn sample(extra: &[&str]) -> std::process::Output {
    let mut args = vec![
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--workflows",
        "samples/.github/workflows",
    ];
    args.extend_from_slice(extra);
    report(&args)
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let marker = format!("\n{start}\n");
    let at = text
        .find(&marker)
        .unwrap_or_else(|| panic!("missing {start} in {text}"));
    let rest = &text[at + marker.len()..];
    let stop = format!("\n{end}\n");
    match rest.find(&stop) {
        Some(end_at) => &rest[..end_at],
        None => rest,
    }
}

#[test]
fn sample_top_1_keeps_the_docs_finding_with_its_workflow() {
    let output = sample(&["--top", "1"]);
    assert_exit(&output, 0);
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("4 more workflows (54.06 USD)\n"), "{text}");
    assert!(text.contains("Findings outside the top 1\n"), "{text}");
    assert!(!text.contains("Workflows without usage"), "{text}");
    assert!(text.contains("Total net: 284.21\n"), "{text}");
    assert!(text.contains("80.98%"), "{text}");
    assert!(text.contains("230.14"), "{text}");
    assert!(!text.contains("44.23"), "{text}");
    assert!(!text.contains("7.72"), "{text}");
    assert!(!text.contains("2.11"), "{text}");
    assert!(!text.contains("(no workflow)"), "{text}");

    let outside = text
        .find("\nFindings outside the top 1\n")
        .expect("outside heading");
    let lint = text
        .find("  F1 samples/.github/workflows/lint.yml:8: lint on macOS")
        .expect("lint finding");
    assert!(lint < outside, "{text}");
    assert!(
        text[..outside].contains("F3 samples/.github/workflows/lint.yml:2:"),
        "{text}"
    );
    assert!(
        text[..outside].contains("F2 samples/.github/workflows/lint.yml:7:"),
        "{text}"
    );

    let block = between(&text, "Findings outside the top 1", "Runs");
    assert!(
        block.contains(
            "example-org/sample-app .github/workflows/docs.yml\n  F2 samples/.github/workflows/docs.yml:4: default timeout is 360 minutes"
        ),
        "{block}"
    );
    assert!(!block.contains("lint.yml"), "{block}");
    assert_eq!(text.matches("docs.yml:4").count(), 1, "{text}");
}

#[test]
fn sample_top_1_json_keeps_the_docs_finding_on_that_workflow() {
    let output = sample(&["--json", "--top", "1"]);
    assert_exit(&output, 0);
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(!text.contains("0000000001"), "{text}");
    assert!(!text.contains("9999999"), "{text}");
    assert!(!text.contains("USD (from net_amount)"), "{text}");
    let value = parse_json(&output);
    assert_eq!(value["schema_version"], 2);
    assert_eq!(value["total_net"], "284.208000");
    assert_eq!(value["workflows"].as_array().unwrap().len(), 1);
    assert_eq!(value["workflows"][0]["path"], ".github/workflows/lint.yml");
    assert_eq!(value["workflows"][0]["net"], "230.144000");
    assert_eq!(
        value["workflows"][0]["findings"].as_array().unwrap().len(),
        3
    );
    assert_eq!(value["outside_top"]["count"], 4);
    assert_eq!(value["outside_top"]["net"], "54.064000");
    let omitted = value["outside_top"]["workflows"].as_array().unwrap();
    let paths: Vec<_> = omitted
        .iter()
        .map(|item| item["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        vec![
            ".github/workflows/ci.yml",
            ".github/workflows/release.yml",
            ".github/workflows/docs.yml",
            "(no workflow)",
        ]
    );
    assert!(omitted[0]["findings"].as_array().unwrap().is_empty());
    assert!(omitted[1]["findings"].as_array().unwrap().is_empty());
    assert!(omitted[3]["findings"].as_array().unwrap().is_empty());
    let docs = &omitted[2];
    assert_eq!(docs["repository"], "example-org/sample-app");
    assert_eq!(docs["net"], "2.112000");
    assert_eq!(docs["findings"].as_array().unwrap().len(), 1);
    assert_eq!(docs["findings"][0]["id"], "F2");
    assert_eq!(docs["findings"][0]["line"], 4);
    assert_eq!(
        docs["findings"][0]["file"],
        "samples/.github/workflows/docs.yml"
    );
    assert_eq!(
        docs["findings"][0]["message"],
        "default timeout is 360 minutes"
    );
    assert!(value["unattached_findings"].as_array().unwrap().is_empty());
    let mut net = money_micros(value["workflows"][0]["net"].as_str().unwrap());
    for workflow in omitted {
        net += money_micros(workflow["net"].as_str().unwrap());
    }
    assert_eq!(net, money_micros("284.208000"));
}

#[test]
fn sample_top_0_summarizes_every_workflow() {
    let output = sample(&["--top", "0"]);
    assert_exit(&output, 0);
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("5 more workflows (284.21 USD)\n"), "{text}");
    assert!(text.contains("Findings outside the top 0\n"), "{text}");
    assert!(!text.contains("Workflows without usage"), "{text}");
    assert!(text.contains("Total net: 284.21\n"), "{text}");
    assert!(!text.contains("230.14"), "{text}");
    assert!(!text.contains("44.23"), "{text}");
    assert!(!text.contains("2.11"), "{text}");
    assert!(!text.contains("80.98%"), "{text}");

    let outside = text
        .find("\nFindings outside the top 0\n")
        .expect("outside heading");
    assert!(!text[..outside].contains("lint.yml"), "{text}");
    assert!(!text[..outside].contains("docs.yml"), "{text}");
    let block = &text[outside..];
    let lint = block
        .find("F1 samples/.github/workflows/lint.yml:8: lint on macOS")
        .expect("lint");
    let docs = block
        .find("F2 samples/.github/workflows/docs.yml:4: default timeout is 360 minutes")
        .expect("docs");
    assert!(lint < docs, "{block}");
    assert!(
        block.contains("example-org/sample-app .github/workflows/lint.yml\n"),
        "{block}"
    );
    assert!(
        block.contains("example-org/sample-app .github/workflows/docs.yml\n"),
        "{block}"
    );
    assert_eq!(block.matches("docs.yml:4").count(), 1, "{block}");

    let json = sample(&["--json", "--top", "0"]);
    assert_exit(&json, 0);
    let value = parse_json(&json);
    assert!(value["workflows"].as_array().unwrap().is_empty(), "{value}");
    assert_eq!(value["outside_top"]["count"], 5);
    assert_eq!(value["outside_top"]["net"], "284.208000");
    let omitted = value["outside_top"]["workflows"].as_array().unwrap();
    assert_eq!(omitted.len(), 5);
    assert_eq!(omitted[0]["path"], ".github/workflows/lint.yml");
    assert_eq!(omitted[0]["findings"].as_array().unwrap().len(), 3);
    assert_eq!(omitted[3]["path"], ".github/workflows/docs.yml");
    assert_eq!(omitted[3]["findings"][0]["id"], "F2");
    assert_eq!(omitted[3]["findings"][0]["line"], 4);
    assert_eq!(omitted[4]["path"], "(no workflow)");
    assert!(omitted[4]["findings"].as_array().unwrap().is_empty());
    assert!(value["unattached_findings"].as_array().unwrap().is_empty());
}

#[test]
fn sample_top_larger_than_the_workflow_count_matches_the_full_report() {
    let full = sample(&[]);
    assert_exit(&full, 0);
    let full_text = stdout(&full);
    let full_json = sample(&["--json"]);
    assert_exit(&full_json, 0);
    let full_json_text = stdout(&full_json);
    assert!(full_text.contains("docs.yml:4"), "{full_text}");
    assert!(
        !full_text.contains("Workflows without usage"),
        "{full_text}"
    );
    assert!(!full_text.contains("more workflow"), "{full_text}");
    assert!(!full_text.contains("Findings outside"), "{full_text}");
    assert!(parse_json(&full_json).get("outside_top").is_none());

    for limit in ["5", "6", "100"] {
        let output = sample(&["--top", limit]);
        assert_exit(&output, 0);
        assert_eq!(stdout(&output), full_text, "top {limit}");
        assert!(stderr(&output).is_empty(), "top {limit}");
        let json = sample(&["--json", "--top", limit]);
        assert_exit(&json, 0);
        assert_eq!(stdout(&json), full_json_text, "top {limit}");
        let value = parse_json(&json);
        assert!(value.get("outside_top").is_none(), "top {limit} {value}");
        assert_eq!(value["workflows"].as_array().unwrap().len(), 5);
        assert_eq!(value["workflows"][3]["path"], ".github/workflows/docs.yml");
        assert_eq!(value["workflows"][3]["findings"][0]["id"], "F2");
        assert!(value["unattached_findings"].as_array().unwrap().is_empty());
    }
}

#[test]
fn sample_top_1_with_saved_runs_keeps_jobs_after_the_summary() {
    let output = report(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--workflows",
        "samples/.github/workflows",
        "--runs",
        "tests/fixtures/api",
        "--top",
        "1",
    ]);
    assert_exit(&output, 0);
    let text = stdout(&output);
    assert!(text.contains("4 more workflows (54.06 USD)\n"), "{text}");
    let outside = text
        .find("\nFindings outside the top 1\n")
        .expect("outside");
    let runs = text.find("\nRuns\n").expect("runs");
    let jobs = text.find("\nJobs\n").expect("jobs");
    assert!(outside < runs && runs < jobs, "{text}");
    assert!(!text.contains("Workflows without usage"), "{text}");
    assert!(text.contains("docs.yml:4"), "{text}");
    let outside_body = &text[outside..runs];
    assert!(outside_body.contains("docs.yml:4"), "{outside_body}");

    let json = report(&[
        "report",
        "--usage",
        "samples/usage-sample.csv",
        "--workflows",
        "samples/.github/workflows",
        "--runs",
        "tests/fixtures/api",
        "--json",
        "--top",
        "1",
    ]);
    assert_exit(&json, 0);
    let value = parse_json(&json);
    assert_eq!(value["outside_top"]["count"], 4);
    assert!(value.get("workflow_runs").is_some(), "{value}");
    assert!(value.get("in_progress").is_some(), "{value}");
    assert_eq!(
        value["outside_top"]["workflows"][2]["findings"][0]["id"],
        "F2"
    );
    assert!(value["unattached_findings"].as_array().unwrap().is_empty());
}

struct Variant {
    name: &'static str,
    line_end: &'static str,
    yaml_crlf: bool,
    headers: &'static [&'static str],
    product: &'static str,
    minutes: &'static str,
    lint_sku: &'static str,
    docs_sku: &'static str,
    docs_file: &'static str,
    empty_fields: bool,
}

const PLAIN_HEADERS: &[&str] = &[
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
];

const SHUFFLED_HEADERS: &[&str] = &[
    "WORKFLOW_PATH",
    "Repository",
    "NET_AMOUNT",
    "discount_amount",
    "GROSS_AMOUNT",
    "quantity",
    "Unit_Type",
    "SKU",
    "PRODUCT",
    "Date",
    "USERNAME",
    "organization",
    "applied_cost_per_quantity",
    "cost_center_name",
];

fn variants() -> Vec<Variant> {
    vec![
        Variant {
            name: "lf",
            line_end: "\n",
            yaml_crlf: false,
            headers: PLAIN_HEADERS,
            product: "actions",
            minutes: "minutes",
            lint_sku: "actions_macos",
            docs_sku: "actions_linux",
            docs_file: "docs.yml",
            empty_fields: false,
        },
        Variant {
            name: "crlf",
            line_end: "\r\n",
            yaml_crlf: false,
            headers: PLAIN_HEADERS,
            product: "actions",
            minutes: "minutes",
            lint_sku: "actions_macos",
            docs_sku: "actions_linux",
            docs_file: "docs.yml",
            empty_fields: false,
        },
        Variant {
            name: "crlf-yaml",
            line_end: "\r\n",
            yaml_crlf: true,
            headers: PLAIN_HEADERS,
            product: "actions",
            minutes: "minutes",
            lint_sku: "actions_macos",
            docs_sku: "actions_linux",
            docs_file: "docs.yml",
            empty_fields: false,
        },
        Variant {
            name: "shuffled",
            line_end: "\n",
            yaml_crlf: false,
            headers: SHUFFLED_HEADERS,
            product: "actions",
            minutes: "minutes",
            lint_sku: "actions_macos",
            docs_sku: "actions_linux",
            docs_file: "docs.yml",
            empty_fields: false,
        },
        Variant {
            name: "capitals",
            line_end: "\n",
            yaml_crlf: false,
            headers: SHUFFLED_HEADERS,
            product: "ACTIONS",
            minutes: "MINUTES",
            lint_sku: "Actions_Macos",
            docs_sku: "Actions_Linux",
            docs_file: "Docs.yml",
            empty_fields: false,
        },
        Variant {
            name: "empty-fields",
            line_end: "\n",
            yaml_crlf: false,
            headers: PLAIN_HEADERS,
            product: "actions",
            minutes: "minutes",
            lint_sku: "actions_macos",
            docs_sku: "actions_linux",
            docs_file: "docs.yml",
            empty_fields: true,
        },
        Variant {
            name: "crlf-shuffled-capitals-empty",
            line_end: "\r\n",
            yaml_crlf: true,
            headers: SHUFFLED_HEADERS,
            product: "Actions",
            minutes: "Minutes",
            lint_sku: "ACTIONS_MACOS",
            docs_sku: "ACTIONS_LINUX",
            docs_file: "Docs.yml",
            empty_fields: true,
        },
    ]
}

struct Fixture {
    usage: PathBuf,
    workflows: PathBuf,
    docs_path: PathBuf,
    nightly_path: PathBuf,
    lint_path: PathBuf,
    docs_key: String,
}

fn write_fixture(variant: &Variant) -> Fixture {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("top")
        .join(variant.name);
    let _ = std::fs::remove_dir_all(&root);
    let workflows = root.join(".github").join("workflows");
    std::fs::create_dir_all(&workflows).unwrap();
    let lint_path = workflows.join("lint.yml");
    let docs_path = workflows.join(variant.docs_file);
    let nightly_path = workflows.join("nightly.yml");
    std::fs::write(&lint_path, yaml_body(LINT_YML, variant.yaml_crlf)).unwrap();
    std::fs::write(&docs_path, yaml_body(DOCS_YML, variant.yaml_crlf)).unwrap();
    std::fs::write(&nightly_path, yaml_body(NIGHTLY_YML, variant.yaml_crlf)).unwrap();
    let usage = root.join("usage.csv");
    std::fs::write(&usage, usage_csv(variant)).unwrap();
    Fixture {
        usage,
        workflows,
        docs_path,
        nightly_path,
        lint_path,
        docs_key: format!(".github/workflows/{}", variant.docs_file),
    }
}

fn yaml_body(text: &str, crlf: bool) -> String {
    if crlf {
        text.replace('\n', "\r\n")
    } else {
        text.to_string()
    }
}

fn usage_csv(variant: &Variant) -> String {
    let docs_path = format!(".github/workflows/{}", variant.docs_file);
    let storage_date = if variant.empty_fields {
        ""
    } else {
        "2026-09-15"
    };
    let storage_user = if variant.empty_fields { "" } else { "ada" };
    let storage_org = if variant.empty_fields {
        ""
    } else {
        "example-org"
    };
    let storage_repo = if variant.empty_fields {
        ""
    } else {
        "example-org/app"
    };
    let rows = [
        cells([
            "2026-09-01",
            variant.product,
            variant.lint_sku,
            "100",
            variant.minutes,
            "0.062",
            "6.2",
            "0",
            "6.2",
            "ada",
            "example-org",
            "example-org/app",
            ".github/workflows/lint.yml",
        ]),
        cells([
            "2026-09-30",
            variant.product,
            variant.docs_sku,
            "352",
            variant.minutes,
            "0.006",
            "2.112",
            "0",
            "2.112",
            "ada",
            "example-org",
            "example-org/app",
            &docs_path,
        ]),
        cells([
            storage_date,
            variant.product,
            "actions_storage",
            "1",
            "gigabyte-hours",
            "0.004",
            "0.004",
            "0.004",
            "0",
            storage_user,
            storage_org,
            storage_repo,
            "",
        ]),
    ];
    let mut body = variant.headers.join(",");
    body.push_str(variant.line_end);
    for row in rows {
        let fields: Vec<&str> = variant
            .headers
            .iter()
            .map(|header| cell(&row, header))
            .collect();
        body.push_str(&fields.join(","));
        body.push_str(variant.line_end);
    }
    body
}

const FIELD_NAMES: [&str; 13] = [
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
];

struct Cells {
    values: [String; 13],
}

fn cells(values: [&str; 13]) -> Cells {
    Cells {
        values: values.map(str::to_string),
    }
}

fn cell<'a>(row: &'a Cells, header: &str) -> &'a str {
    let key = header.to_ascii_lowercase();
    if key == "cost_center_name" {
        return "";
    }
    let index = FIELD_NAMES
        .iter()
        .position(|name| *name == key)
        .unwrap_or_else(|| panic!("unknown column {header}"));
    &row.values[index]
}

fn run_fixture(fixture: &Fixture, extra: &[&str]) -> std::process::Output {
    let mut args = vec![
        "report",
        "--usage",
        fixture.usage.to_str().unwrap(),
        "--workflows",
        fixture.workflows.to_str().unwrap(),
    ];
    args.extend_from_slice(extra);
    report(&args)
}

fn finding_line(path: &Path, line: usize, id: &str, message: &str) -> String {
    format!("{id} {}:{}: {message}", path.display(), line)
}

#[test]
fn top_limits_cover_crlf_column_order_empty_fields_and_capitals() {
    for variant in variants() {
        let fixture = write_fixture(&variant);
        let docs = finding_line(
            &fixture.docs_path,
            4,
            "F2",
            "default timeout is 360 minutes",
        );
        let nightly = finding_line(
            &fixture.nightly_path,
            4,
            "F2",
            "default timeout is 360 minutes",
        );
        let lint_f1 = finding_line(&fixture.lint_path, 6, "F1", "lint on macOS");
        let name = variant.name;

        let full = run_fixture(&fixture, &[]);
        assert_exit(&full, 0);
        let full_text = stdout(&full);
        assert!(!full_text.contains("more workflow"), "{name}\n{full_text}");
        assert!(
            !full_text.contains("Findings outside"),
            "{name}\n{full_text}"
        );
        assert!(full_text.contains(&docs), "{name}\n{full_text}");
        assert!(full_text.contains(&nightly), "{name}\n{full_text}");
        assert!(
            full_text.find(&docs).unwrap() < full_text.find("Workflows without usage").unwrap(),
            "{name}\n{full_text}"
        );
        let without = between(&full_text, "Workflows without usage", "Runs");
        assert!(without.contains(&nightly), "{name}\n{without}");
        assert!(!without.contains(&docs), "{name}\n{without}");
        assert!(
            full_text.contains("Total net: 8.31\n"),
            "{name}\n{full_text}"
        );
        assert!(
            full_text.contains("Total minutes: 452\n"),
            "{name}\n{full_text}"
        );
        assert!(full_text.contains(variant.lint_sku), "{name}\n{full_text}");
        assert!(full_text.contains(variant.docs_sku), "{name}\n{full_text}");

        let top_1 = run_fixture(&fixture, &["--top", "1"]);
        assert_exit(&top_1, 0);
        let text = stdout(&top_1);
        assert!(
            text.contains("2 more workflows (2.11 USD)\n"),
            "{name}\n{text}"
        );
        assert!(
            text.contains("Findings outside the top 1\n"),
            "{name}\n{text}"
        );
        assert!(text.contains("Total net: 8.31\n"), "{name}\n{text}");
        assert!(text.contains("Total minutes: 452\n"), "{name}\n{text}");
        assert!(text.contains(&lint_f1), "{name}\n{text}");
        let outside_at = text.find("\nFindings outside the top 1\n").unwrap();
        assert!(text[..outside_at].contains(&lint_f1), "{name}\n{text}");
        assert!(!text[..outside_at].contains(&docs), "{name}\n{text}");
        let outside = between(
            &text,
            "Findings outside the top 1",
            "Workflows without usage",
        );
        let unused = between(&text, "Workflows without usage", "Runs");
        assert!(outside.contains(&docs), "{name}\n{outside}");
        assert!(
            outside.contains(&format!("example-org/app {}\n", fixture.docs_key)),
            "{name}\n{outside}"
        );
        assert!(!outside.contains(&nightly), "{name}\n{outside}");
        assert!(!outside.contains("lint.yml"), "{name}\n{outside}");
        assert!(unused.contains(&nightly), "{name}\n{unused}");
        assert!(!unused.contains(variant.docs_file), "{name}\n{unused}");
        assert!(!unused.contains("lint.yml"), "{name}\n{unused}");
        assert!(text.contains("6.20"), "{name}\n{text}");
        assert!(!text.contains("\n2.11"), "{name}\n{text}");

        let top_0 = run_fixture(&fixture, &["--top", "0"]);
        assert_exit(&top_0, 0);
        let zero = stdout(&top_0);
        assert!(
            zero.contains("3 more workflows (8.31 USD)\n"),
            "{name}\n{zero}"
        );
        assert!(
            zero.contains("Findings outside the top 0\n"),
            "{name}\n{zero}"
        );
        let zero_outside = between(
            &zero,
            "Findings outside the top 0",
            "Workflows without usage",
        );
        let zero_unused = between(&zero, "Workflows without usage", "Runs");
        assert!(zero_outside.contains(&docs), "{name}\n{zero_outside}");
        assert!(zero_outside.contains(&lint_f1), "{name}\n{zero_outside}");
        assert!(!zero_outside.contains(&nightly), "{name}\n{zero_outside}");
        assert!(zero_unused.contains(&nightly), "{name}\n{zero_unused}");
        assert!(
            !zero_unused.contains(variant.docs_file),
            "{name}\n{zero_unused}"
        );
        assert!(!zero.contains("6.20"), "{name}\n{zero}");

        for limit in ["3", "4", "100"] {
            let output = run_fixture(&fixture, &["--top", limit]);
            assert_exit(&output, 0);
            assert_eq!(stdout(&output), full_text, "{name} top {limit}");
        }

        let json_full = run_fixture(&fixture, &["--json"]);
        assert_exit(&json_full, 0);
        let full_value = parse_json(&json_full);
        assert!(full_value.get("outside_top").is_none(), "{name}");
        assert_eq!(full_value["total_net"], "8.312000");
        assert_eq!(full_value["total_minutes"], 452);
        assert!(full_value["unattached_findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["file"] == fixture.nightly_path.display().to_string()));
        assert!(full_value["workflows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["path"] == fixture.docs_key && item["findings"][0]["id"] == "F2"));

        let json_top = run_fixture(&fixture, &["--json", "--top", "1"]);
        assert_exit(&json_top, 0);
        let value = parse_json(&json_top);
        assert!(!stdout(&json_top).contains("0000000001"), "{name}");
        assert_eq!(value["workflows"].as_array().unwrap().len(), 1);
        assert_eq!(value["workflows"][0]["path"], ".github/workflows/lint.yml");
        assert_eq!(value["workflows"][0]["skus"][0]["sku"], variant.lint_sku);
        assert_eq!(value["workflows"][0]["net"], "6.200000");
        assert_eq!(
            value["workflows"][0]["findings"].as_array().unwrap().len(),
            3
        );
        assert_eq!(value["outside_top"]["count"], 2);
        assert_eq!(value["outside_top"]["net"], "2.112000");
        let omitted = value["outside_top"]["workflows"].as_array().unwrap();
        assert_eq!(omitted[0]["path"], fixture.docs_key);
        assert_eq!(omitted[0]["repository"], "example-org/app");
        assert_eq!(omitted[0]["skus"][0]["sku"], variant.docs_sku);
        assert_eq!(omitted[0]["findings"][0]["id"], "F2");
        assert_eq!(omitted[0]["findings"][0]["line"], 4);
        assert_eq!(
            omitted[0]["findings"][0]["file"],
            fixture.docs_path.display().to_string()
        );
        assert_eq!(omitted[1]["path"], "(no workflow)");
        assert!(omitted[1]["findings"].as_array().unwrap().is_empty());
        if variant.empty_fields {
            assert_eq!(omitted[1]["repository"], "");
        }
        let loose = value["unattached_findings"].as_array().unwrap();
        assert_eq!(loose.len(), 1, "{name} {value}");
        assert_eq!(loose[0]["file"], fixture.nightly_path.display().to_string());
        assert_eq!(loose[0]["line"], 4);
        assert!(loose
            .iter()
            .all(|item| item["file"] != fixture.docs_path.display().to_string()));

        let json_zero = run_fixture(&fixture, &["--json", "--top", "0"]);
        assert_exit(&json_zero, 0);
        let zero_value = parse_json(&json_zero);
        assert!(zero_value["workflows"].as_array().unwrap().is_empty());
        assert_eq!(zero_value["outside_top"]["count"], 3);
        assert_eq!(zero_value["outside_top"]["net"], "8.312000");
        assert_eq!(
            zero_value["outside_top"]["workflows"][1]["findings"][0]["id"],
            "F2"
        );
        assert_eq!(
            zero_value["unattached_findings"].as_array().unwrap().len(),
            1
        );

        for limit in ["3", "4", "100"] {
            let output = run_fixture(&fixture, &["--json", "--top", limit]);
            assert_exit(&output, 0);
            assert_eq!(
                stdout(&output),
                stdout(&json_full),
                "{name} json top {limit}"
            );
        }
    }
}

fn money_micros(text: &str) -> i64 {
    let (whole, frac) = text.split_once('.').unwrap_or_else(|| panic!("{text}"));
    assert_eq!(frac.len(), 6, "{text}");
    whole.parse::<i64>().unwrap() * 1_000_000 + frac.parse::<i64>().unwrap()
}
