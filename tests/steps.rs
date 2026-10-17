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

fn assert_no_limitation(text: &str) {
    let lower = text.to_ascii_lowercase();
    assert!(!lower.contains("not supported"), "{text}");
    assert!(!lower.contains("unsupported"), "{text}");
    assert!(!lower.contains("skipped"), "{text}");
    assert!(!lower.contains("not implemented"), "{text}");
}

fn scratch(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("steps")
        .join(name);
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, text).unwrap();
}

fn crlf(text: &str) -> String {
    text.replace('\n', "\r\n")
}

fn usage_row(path: &str, quantity: &str, gross: &str) -> String {
    format!(
        "{path},example-org/app,,,{gross},0,{gross},0.006000,MINUTES,{quantity},actions_linux,ACTIONS,2026-09-01\n"
    )
}

fn usage(rows: &[(&str, &str, &str)]) -> String {
    let mut csv = String::from(
        "WORKFLOW_PATH,REPOSITORY,ORGANIZATION,USERNAME,NET_AMOUNT,DISCOUNT_AMOUNT,GROSS_AMOUNT,APPLIED_COST_PER_QUANTITY,UNIT_TYPE,QUANTITY,SKU,PRODUCT,DATE\n",
    );
    for (path, quantity, gross) in rows {
        csv.push_str(&usage_row(path, quantity, gross));
    }
    csv
}

fn json_report(root: &Path) -> serde_json::Value {
    let output = report(&[
        "report",
        "--usage",
        root.join("usage.csv").to_str().unwrap(),
        "--workflows",
        root.join("workflows").to_str().unwrap(),
        "--json",
    ]);
    assert_exit(&output, 0);
    let err = stderr(&output);
    assert!(err.is_empty(), "{err}");
    let text = stdout(&output);
    assert_no_limitation(&text);
    assert!(!text.contains("USD (from net_amount)"), "{text}");
    serde_json::from_str(text.trim_end()).expect("json")
}

fn workflow<'a>(json: &'a serde_json::Value, path: &str) -> &'a serde_json::Value {
    json["workflows"]
        .as_array()
        .expect("workflows")
        .iter()
        .chain(
            json.get("outside_top")
                .and_then(|item| item.get("workflows"))
                .and_then(|item| item.as_array())
                .into_iter()
                .flatten(),
        )
        .find(|item| item["path"] == path)
        .unwrap_or_else(|| panic!("missing {path} in {json}"))
}

fn assert_steps(json: &serde_json::Value, path: &str, expected: &[(&str, u64)]) {
    let steps = workflow(json, path)["steps"]
        .as_array()
        .unwrap_or_else(|| panic!("steps for {path}"));
    assert_eq!(steps.len(), expected.len(), "{path}: {steps:?}");
    for (step, (name, line)) in steps.iter().zip(expected) {
        let object = step.as_object().expect("step object");
        assert_eq!(object.len(), 2, "{path}: {step}");
        assert!(
            object.contains_key("name") && object.contains_key("line"),
            "{step}"
        );
        assert_eq!(step["name"], *name, "{path}: {step}");
        assert_eq!(step["line"], *line, "{path}: {step}");
        assert!(step["line"].is_number(), "{step}");
    }
}

#[test]
fn json_reports_step_names_and_lines_for_every_form() {
    let root = scratch("forms");
    let workflows = root.join("workflows");
    let files = [
        (
            "block.yml",
            "\
jobs:
  lint:
    steps:
      - name: Checkout
        uses: actions/checkout@v4
      - name: say \"hi\"
        run: echo hi
",
        ),
        (
            "multiline.yml",
            "\
jobs:
  lint:
    steps:
      - run: |
          echo one
          echo two
      - name: Script
        run: |
          cargo test -- --test-threads=1
          echo done
",
        ),
        (
            "nameless.yml",
            "\
jobs:
  lint:
    steps:
      - uses: actions/checkout@v4
      - run: cargo test
      - run: cargo test
        uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        run: npm test
",
        ),
        (
            "flow.yml",
            "\
jobs:
  lint:
    steps: [{name: Lint, run: cargo fmt --check}, {uses: actions/checkout@v4}]
",
        ),
        (
            "inline.yml",
            "jobs: {lint: {steps: [{name: Checkout, uses: actions/checkout@v4}, {run: cargo test}]}}\n",
        ),
        (
            "order.yml",
            "\
jobs:
  lint:
    steps:
      - run: cargo test
        name: Run the suite
      - run: echo hidden
        uses: actions/checkout@v4
      - name: First
        NAME: Second
        run: cargo test
",
        ),
        (
            "empty.yml",
            "\
jobs:
  bare:
    runs-on: ubuntu-latest
  listed:
    steps: []
  blank:
    steps:
      - name: \"\"
        run: cargo test
      - name: \"  \"
        run: echo spaces
      - name:
        run: echo hi
      - name: null
        run: echo null-name
      - name: ~
        uses: actions/checkout@v4
      - id: setup
      - run: echo kept
",
        ),
        (
            "nulls.yml",
            "\
jobs:
  absent:
    steps: null
  tilde:
    steps: ~
  emptykey:
    steps:
",
        ),
        (
            "capitals.yml",
            "\
jobs:
  lint:
    Steps:
      - NAME: Checkout
        USES: actions/checkout@v4
      - RUN: |
          echo one
          echo two
",
        ),
        (
            "quoted.yml",
            "\
jobs:
  lint:
    steps:
      - \"name\": \"Lint\"
        \"run\": \"cargo fmt --check\"
      - 'run': 'cargo test'
",
        ),
        (
            "twojobs.yml",
            "\
jobs:
  lint:
    steps:
      - name: Lint
        run: cargo fmt --check
  test:
    steps:
      - run: cargo test
",
        ),
        (
            "nosteps.yml",
            "\
jobs:
  lint:
    runs-on: ubuntu-latest
",
        ),
        (
            "scalar.yml",
            "\
jobs:
  lint:
    steps: cargo test
",
        ),
        (
            "mapping.yml",
            "\
jobs:
  lint:
    steps:
      name: Only
      run: cargo test
",
        ),
    ];
    let mut rows = Vec::new();
    for (name, text) in files {
        write(&workflows.join(name), text);
        rows.push((format!(".github/workflows/{name}"), "1", "0.006000"));
    }
    write(
        &workflows.join("crlf.yml"),
        &crlf(
            "\
jobs:
  lint:
    steps:
      - name: Checkout
        uses: actions/checkout@v4
      - run: |
          echo one
          echo two
",
        ),
    );
    rows.push((".github/workflows/crlf.yml".to_string(), "1", "0.006000"));
    rows.push((String::new(), "1", "0.006000"));
    rows.push((".github/workflows/missing.yml".to_string(), "1", "0.006000"));
    let row_refs: Vec<(&str, &str, &str)> = rows
        .iter()
        .map(|(path, quantity, gross)| (path.as_str(), *quantity, *gross))
        .collect();
    write(&root.join("usage.csv"), &crlf(&usage(&row_refs)));

    let json = json_report(&root);
    assert_eq!(json["schema_version"], 2);
    assert_steps(
        &json,
        ".github/workflows/block.yml",
        &[("Checkout", 4), ("say \"hi\"", 6)],
    );
    assert_steps(
        &json,
        ".github/workflows/multiline.yml",
        &[("echo one", 4), ("Script", 7)],
    );
    assert_steps(
        &json,
        ".github/workflows/nameless.yml",
        &[
            ("actions/checkout@v4", 4),
            ("cargo test", 5),
            ("actions/checkout@v4", 7),
            ("actions/setup-node@v4", 8),
        ],
    );
    assert_steps(
        &json,
        ".github/workflows/flow.yml",
        &[("Lint", 3), ("actions/checkout@v4", 3)],
    );
    assert_steps(
        &json,
        ".github/workflows/inline.yml",
        &[("Checkout", 1), ("cargo test", 1)],
    );
    assert_steps(
        &json,
        ".github/workflows/order.yml",
        &[
            ("Run the suite", 5),
            ("actions/checkout@v4", 7),
            ("Second", 9),
        ],
    );
    assert_steps(
        &json,
        ".github/workflows/empty.yml",
        &[
            ("cargo test", 9),
            ("echo spaces", 11),
            ("echo hi", 13),
            ("echo null-name", 15),
            ("actions/checkout@v4", 17),
            ("", 18),
            ("echo kept", 19),
        ],
    );
    assert_steps(&json, ".github/workflows/nulls.yml", &[]);
    assert_steps(
        &json,
        ".github/workflows/capitals.yml",
        &[("Checkout", 4), ("echo one", 6)],
    );
    assert_steps(
        &json,
        ".github/workflows/quoted.yml",
        &[("Lint", 4), ("cargo test", 6)],
    );
    assert_steps(
        &json,
        ".github/workflows/twojobs.yml",
        &[("Lint", 4), ("cargo test", 8)],
    );
    assert_steps(&json, ".github/workflows/nosteps.yml", &[]);
    assert_steps(&json, ".github/workflows/scalar.yml", &[("cargo test", 3)]);
    assert_steps(&json, ".github/workflows/mapping.yml", &[("Only", 4)]);
    assert_steps(
        &json,
        ".github/workflows/crlf.yml",
        &[("Checkout", 4), ("echo one", 6)],
    );
    assert_steps(&json, "(no workflow)", &[]);
    assert_steps(&json, ".github/workflows/missing.yml", &[]);

    let text_out = report(&[
        "report",
        "--usage",
        root.join("usage.csv").to_str().unwrap(),
        "--workflows",
        workflows.to_str().unwrap(),
    ]);
    assert_exit(&text_out, 0);
    let text = stdout(&text_out);
    assert!(stderr(&text_out).is_empty(), "{}", stderr(&text_out));
    assert_no_limitation(&text);
    assert!(text.contains("USD (from net_amount)\n"), "{text}");
    assert!(text.contains(".github/workflows/multiline.yml"), "{text}");
    assert!(!text.contains("echo one"), "{text}");
    assert!(!text.contains("Run the suite"), "{text}");
    assert!(!text.contains("say \"hi\""), "{text}");
    assert!(!text.starts_with('{'), "{text}");
}

#[test]
fn json_keeps_steps_outside_the_top_and_on_a_capital_path() {
    let root = scratch("top");
    let workflows = root.join("workflows");
    write(
        &workflows.join("high.yml"),
        "\
jobs:
  build:
    steps:
      - run: echo high
",
    );
    write(
        &workflows.join("low.yml"),
        "\
jobs:
  build:
    steps:
      - name: Low
        run: echo low
",
    );
    let rows = [
        (".github/workflows/high.yml", "10", "0.060000"),
        (".GITHUB/WORKFLOWS/LOW.YML", "1", "0.006000"),
    ];
    write(&root.join("usage.csv"), &crlf(&usage(&rows)));

    let top = report(&[
        "report",
        "--usage",
        root.join("usage.csv").to_str().unwrap(),
        "--workflows",
        workflows.to_str().unwrap(),
        "--json",
        "--top",
        "1",
    ]);
    assert_exit(&top, 0);
    let top_text = stdout(&top);
    assert_no_limitation(&top_text);
    let json: serde_json::Value = serde_json::from_str(top_text.trim_end()).unwrap();
    assert_eq!(json["workflows"].as_array().unwrap().len(), 1);
    assert_steps(&json, ".github/workflows/high.yml", &[("echo high", 4)]);
    assert_steps(&json, ".GITHUB/WORKFLOWS/LOW.YML", &[("Low", 4)]);
    assert_eq!(json["outside_top"]["count"], 1);
    assert!(json["workflows"][0]["steps"].as_array().unwrap().len() == 1);

    let zero = report(&[
        "report",
        "--usage",
        root.join("usage.csv").to_str().unwrap(),
        "--workflows",
        workflows.to_str().unwrap(),
        "--json",
        "--top",
        "0",
    ]);
    assert_exit(&zero, 0);
    let zero_json: serde_json::Value = serde_json::from_str(stdout(&zero).trim_end()).unwrap();
    assert!(zero_json["workflows"].as_array().unwrap().is_empty());
    assert_steps(
        &zero_json,
        ".github/workflows/high.yml",
        &[("echo high", 4)],
    );
    assert_steps(&zero_json, ".GITHUB/WORKFLOWS/LOW.YML", &[("Low", 4)]);

    let text_out = report(&[
        "report",
        "--usage",
        root.join("usage.csv").to_str().unwrap(),
        "--workflows",
        workflows.to_str().unwrap(),
        "--top",
        "1",
    ]);
    assert_exit(&text_out, 0);
    let text = stdout(&text_out);
    assert_no_limitation(&text);
    assert!(text.contains("1 more workflow (0.01 USD)\n"), "{text}");
    assert!(!text.contains("echo high"), "{text}");
    assert!(!text.contains("echo low"), "{text}");
    assert!(text.contains("USD (from net_amount)\n"), "{text}");
}
