use std::path::Path;

use crate::money;
use crate::usage::UsageRow;
use crate::workflow::{Job, Matrix, RunsOn, Workflow};

/// Why a workflow or job was flagged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FindingKind {
    /// Lint-class job on a macOS or Windows runner.
    LintOnRunner,
    /// Job has no `timeout-minutes`.
    MissingTimeout,
    /// `pull_request` without `concurrency.cancel-in-progress: true`.
    PullRequestConcurrency,
    /// Matrix expands to at least six billed jobs, or an axis length is an expression.
    MatrixJobs,
    /// A usage row disagrees with quantity * unit price, or with gross - discount.
    InvoiceMismatch,
}

impl FindingKind {
    /// Short id used in the text report.
    pub fn code(self) -> &'static str {
        match self {
            Self::LintOnRunner => "F1",
            Self::MissingTimeout => "F2",
            Self::PullRequestConcurrency => "F3",
            Self::MatrixJobs => "F4",
            Self::InvoiceMismatch => "F9",
        }
    }
}

/// One cause, pointing at a file line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub path: std::path::PathBuf,
    pub line: usize,
    pub kind: FindingKind,
    pub message: String,
    /// Set on invoice findings so the row stays on its repository workflow.
    pub repository: Option<String>,
    /// Workflow path from the usage row, including `(no workflow)`.
    pub workflow_path: Option<String>,
}

const LINT_TOOLS: [&str; 5] = ["lint", "fmt", "clippy", "eslint", "prettier"];

/// Findings for the scanned workflows, ordered by path, line, and kind.
pub fn findings(workflows: &[Workflow]) -> Vec<Finding> {
    let mut found = Vec::new();
    for workflow in workflows {
        if workflow.has_pull_request && !workflow.cancel_in_progress {
            if let Some(line) = workflow.on_line {
                push(
                    &mut found,
                    &workflow.path,
                    line,
                    FindingKind::PullRequestConcurrency,
                    "pull_request without concurrency cancel-in-progress",
                );
            }
        }
        for job in &workflow.jobs {
            note_job(&mut found, &workflow.path, job);
        }
    }
    found.sort_by(cmp_finding);
    found
}

/// Invoice check for every usage row.
///
/// `gross` must match `quantity * applied_cost_per_quantity` after that product
/// is rounded to micros. One micro of slack covers a half-micro tie. `net` must
/// equal `gross - discount` exactly. The CSV `net` is left unchanged.
///
/// One finding per mismatched row. The message names the SKU, the date, and
/// both sides of each failed comparison.
pub fn invoice_findings(path: &Path, rows: &[UsageRow]) -> Vec<Finding> {
    let mut found = Vec::new();
    for row in rows {
        let Some(message) = mismatch_message(row) else {
            continue;
        };
        found.push(Finding {
            path: path.to_path_buf(),
            line: usize::try_from(row.line).unwrap_or(usize::MAX),
            kind: FindingKind::InvoiceMismatch,
            message,
            repository: Some(row.repository.clone()),
            workflow_path: Some(row.workflow_path.clone()),
        });
    }
    found
}

/// YAML findings plus invoice findings, ordered by path, line, kind, and message.
///
/// `None` means the workflow directory was not scanned and no row mismatched.
/// An empty scan stays `Some`, so the report can tell those cases apart.
pub fn combine(yaml: Option<Vec<Finding>>, invoice: Vec<Finding>) -> Option<Vec<Finding>> {
    let mut all = match yaml {
        Some(items) => items,
        None if invoice.is_empty() => return None,
        None => Vec::new(),
    };
    all.extend(invoice);
    all.sort_by(cmp_finding);
    Some(all)
}

fn cmp_finding(left: &Finding, right: &Finding) -> std::cmp::Ordering {
    (&left.path, left.line, left.kind, left.message.as_str()).cmp(&(
        &right.path,
        right.line,
        right.kind,
        right.message.as_str(),
    ))
}

/// Absolute slack after the product has been rounded to micros.
const GROSS_TOLERANCE_MICROS: i128 = 1;

fn mismatch_message(row: &UsageRow) -> Option<String> {
    let mut parts = Vec::new();
    let expected_gross = money::rounded_product(row.quantity, row.applied_cost_per_quantity);
    if (i128::from(row.gross.0) - expected_gross).abs() > GROSS_TOLERANCE_MICROS {
        parts.push(format!(
            "gross {} != quantity * applied_cost_per_quantity {}",
            row.gross.to_decimal_string(),
            money::format_micros(expected_gross),
        ));
    }
    let expected_net = i128::from(row.gross.0) - i128::from(row.discount.0);
    if expected_net != i128::from(row.net.0) {
        parts.push(format!(
            "net {} != gross - discount {}",
            row.net.to_decimal_string(),
            money::format_micros(expected_net),
        ));
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!(
        "invoice row mismatch: sku {}, date {}, {}",
        debug_cell(&row.sku),
        debug_cell(&row.date),
        parts.join(", ")
    ))
}

/// Quoted cell text so an empty SKU or date stays visible.
fn debug_cell(text: &str) -> String {
    format!("{text:?}")
}

fn note_job(found: &mut Vec<Finding>, path: &Path, job: &Job) {
    if let Some(runs_on) = &job.runs_on {
        if job_is_lint(job) {
            let (macos, windows) = runner_oses(runs_on);
            if macos {
                push(
                    found,
                    path,
                    runs_on.line,
                    FindingKind::LintOnRunner,
                    "lint on macOS",
                );
            }
            if windows {
                push(
                    found,
                    path,
                    runs_on.line,
                    FindingKind::LintOnRunner,
                    "lint on Windows",
                );
            }
        }
    }
    if !job.has_timeout_minutes {
        push(
            found,
            path,
            job.line,
            FindingKind::MissingTimeout,
            "default timeout is 360 minutes",
        );
    }
    if let Some(matrix) = &job.matrix {
        if let Some(message) = matrix_message(matrix) {
            push(found, path, matrix.line, FindingKind::MatrixJobs, &message);
        }
    }
}

/// Six or more concrete jobs, or any expression axis whose lower bound is at least one.
fn matrix_message(matrix: &Matrix) -> Option<String> {
    let wide = matrix.jobs >= 6 || (matrix.at_least && matrix.jobs > 0);
    if !wide {
        return None;
    }
    let noun = if matrix.jobs == 1 { "job" } else { "jobs" };
    let message = if matrix.at_least {
        format!(
            "matrix runs at least {} {noun}; each job is billed in whole minutes",
            matrix.jobs
        )
    } else {
        format!(
            "matrix runs {} {noun}; each job is billed in whole minutes",
            matrix.jobs
        )
    };
    Some(message)
}

fn job_is_lint(job: &Job) -> bool {
    mentions_lint_tool(&job.name)
        || job.display_name.as_deref().is_some_and(mentions_lint_tool)
        || mentions_lint_tool(&job.steps_text)
}

fn mentions_lint_tool(text: &str) -> bool {
    text.split(|character: char| !character.is_ascii_alphanumeric())
        .any(|token| {
            LINT_TOOLS
                .iter()
                .any(|tool| token.eq_ignore_ascii_case(tool))
        })
}

fn runner_oses(runs_on: &RunsOn) -> (bool, bool) {
    let mut macos = false;
    let mut windows = false;
    for label in &runs_on.labels {
        let label = label.to_ascii_lowercase();
        if label == "macos" || label.starts_with("macos-") {
            macos = true;
        } else if label == "windows" || label.starts_with("windows-") {
            windows = true;
        }
    }
    (macos, windows)
}

fn push(found: &mut Vec<Finding>, path: &Path, line: usize, kind: FindingKind, message: &str) {
    found.push(Finding {
        path: path.to_path_buf(),
        line,
        kind,
        message: message.to_string(),
        repository: None,
        workflow_path: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::UsageRow;
    use crate::workflow::{scan_dir, scan_yaml};
    use std::path::Path;

    fn analyze(yaml: &str) -> Vec<Finding> {
        let workflow = scan_yaml(Path::new("fixture.yml"), yaml).expect("valid fixture");
        findings(&[workflow])
    }

    fn of_kind(found: &[Finding], kind: FindingKind) -> Vec<&Finding> {
        found.iter().filter(|item| item.kind == kind).collect()
    }

    #[test]
    fn f1_flags_lint_tools_on_macos_and_windows() {
        let cases = [
            ("lint", "macos-latest", "lint on macOS"),
            ("fmt", "windows-latest", "lint on Windows"),
            ("clippy", "macos-14", "lint on macOS"),
            ("eslint", "Windows-2022", "lint on Windows"),
            ("prettier", "macOS", "lint on macOS"),
        ];
        for (name, runner, message) in cases {
            let yaml = format!(
                "jobs:\n  {name}:\n    runs-on: {runner}\n    steps:\n      - run: echo hi\n"
            );
            let found = analyze(&yaml);
            let f1 = of_kind(&found, FindingKind::LintOnRunner);
            assert_eq!(f1.len(), 1, "{name} on {runner}: {found:?}");
            assert_eq!(f1[0].line, 3, "{name}");
            assert_eq!(f1[0].message, message, "{name}");
            assert_eq!(f1[0].path, Path::new("fixture.yml"));
        }

        let yaml = r#"jobs:
  build:
    name: Check
    runs-on: [self-hosted, macos]
    steps:
      - run: npx prettier --check .
  package:
    runs-on: windows-latest
    steps:
      - name: Style
        run: cargo clippy -- -D warnings
"#;
        let found = analyze(yaml);
        let f1 = of_kind(&found, FindingKind::LintOnRunner);
        assert_eq!(f1.len(), 2, "{found:?}");
        assert_eq!(f1[0].line, 4);
        assert_eq!(f1[0].message, "lint on macOS");
        assert_eq!(f1[1].line, 8);
        assert_eq!(f1[1].message, "lint on Windows");
    }

    #[test]
    fn f1_ignores_ubuntu_and_non_lint_jobs() {
        let yaml = r#"jobs:
  lint:
    runs-on: ubuntu-latest
    steps:
      - run: cargo fmt --check
  test:
    runs-on: macos-latest
    steps:
      - run: cargo test
  hosted:
    runs-on: [self-hosted, linux]
    steps:
      - run: cargo clippy -- -D warnings
  formatted:
    runs-on: windows-latest
    steps:
      - run: echo formatted
"#;
        let found = analyze(yaml);
        assert!(
            of_kind(&found, FindingKind::LintOnRunner).is_empty(),
            "{found:?}"
        );
    }

    #[test]
    fn f2_flags_jobs_without_timeout_only() {
        let yaml = r#"jobs:
  slow:
    runs-on: ubuntu-latest
    steps:
      - timeout-minutes: 5
        run: cargo test
  fast:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - run: cargo test
"#;
        let found = analyze(yaml);
        let f2 = of_kind(&found, FindingKind::MissingTimeout);
        assert_eq!(f2.len(), 1, "{found:?}");
        assert_eq!(f2[0].line, 2);
        assert_eq!(f2[0].message, "default timeout is 360 minutes");
        assert_eq!(f2[0].path, Path::new("fixture.yml"));
    }

    #[test]
    fn f3_flags_pull_request_without_cancel() {
        let positive = r#"on:
  pull_request:
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - run: cargo test
"#;
        let found = analyze(positive);
        let f3 = of_kind(&found, FindingKind::PullRequestConcurrency);
        assert_eq!(f3.len(), 1, "{found:?}");
        assert_eq!(f3[0].line, 1);
        assert_eq!(
            f3[0].message,
            "pull_request without concurrency cancel-in-progress"
        );
        assert!(of_kind(&found, FindingKind::MissingTimeout).is_empty());

        let inline = analyze(
            "on: [push, pull_request]\njobs:\n  test:\n    timeout-minutes: 1\n    runs-on: ubuntu-latest\n",
        );
        let f3 = of_kind(&inline, FindingKind::PullRequestConcurrency);
        assert_eq!(f3.len(), 1, "{inline:?}");
        assert_eq!(f3[0].line, 1);
    }

    #[test]
    fn f3_ignores_cancel_and_non_pull_request() {
        let cancelled = r#"on:
  pull_request:
concurrency:
  group: ci
  cancel-in-progress: true
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - run: cargo test
"#;
        let found = analyze(cancelled);
        assert!(
            of_kind(&found, FindingKind::PullRequestConcurrency).is_empty(),
            "{found:?}"
        );

        let push_only =
            "on: push\njobs:\n  test:\n    timeout-minutes: 1\n    runs-on: ubuntu-latest\n";
        let found = analyze(push_only);
        assert!(of_kind(&found, FindingKind::PullRequestConcurrency).is_empty());
        assert!(of_kind(&found, FindingKind::MissingTimeout).is_empty());
    }

    #[test]
    fn ci_fixture_has_no_f2_or_f3() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/.github/workflows");
        let workflows = scan_dir(&dir).unwrap();
        let ci = workflows
            .into_iter()
            .find(|workflow| {
                workflow.path.file_name().and_then(|name| name.to_str()) == Some("ci.yml")
            })
            .expect("ci.yml fixture");
        let found = findings(&[ci]);
        assert!(
            found.iter().all(|item| {
                !matches!(
                    item.kind,
                    FindingKind::MissingTimeout | FindingKind::PullRequestConcurrency
                )
            }),
            "{found:?}"
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn f1_f2_f3_apply_to_flow_style() {
        let flagged = "on: {pull_request: {}}\njobs: {lint: {runs-on: macos-latest, steps: [{run: cargo fmt --check}]}}\n";
        let found = analyze(flagged);
        let f1 = of_kind(&found, FindingKind::LintOnRunner);
        assert_eq!(f1.len(), 1, "{found:?}");
        assert_eq!(f1[0].line, 2);
        assert_eq!(f1[0].message, "lint on macOS");
        let f2 = of_kind(&found, FindingKind::MissingTimeout);
        assert_eq!(f2.len(), 1, "{found:?}");
        assert_eq!(f2[0].line, 2);
        assert_eq!(f2[0].message, "default timeout is 360 minutes");
        let f3 = of_kind(&found, FindingKind::PullRequestConcurrency);
        assert_eq!(f3.len(), 1, "{found:?}");
        assert_eq!(f3[0].line, 1);
        assert_eq!(
            f3[0].message,
            "pull_request without concurrency cancel-in-progress"
        );

        let windows = "on: [push, pull_request]\njobs: {lint: {runs-on: windows-latest, timeout-minutes: 5, steps: [{run: cargo clippy -- -D warnings}]}}\n";
        let found = analyze(windows);
        let f1 = of_kind(&found, FindingKind::LintOnRunner);
        assert_eq!(f1.len(), 1, "{found:?}");
        assert_eq!(f1[0].line, 2);
        assert_eq!(f1[0].message, "lint on Windows");
        assert_eq!(
            of_kind(&found, FindingKind::PullRequestConcurrency).len(),
            1,
            "{found:?}"
        );
        assert_eq!(
            of_kind(&found, FindingKind::PullRequestConcurrency)[0].line,
            1
        );
        assert!(
            of_kind(&found, FindingKind::MissingTimeout).is_empty(),
            "{found:?}"
        );

        let clean = "on: {pull_request: {}}\nconcurrency: {group: ci, cancel-in-progress: true}\njobs: {test: {runs-on: ubuntu-latest, timeout-minutes: 5, steps: [{run: cargo test}]}}\n";
        let found = analyze(clean);
        assert!(found.is_empty(), "{found:?}");

        let ubuntu_lint = "on: push\njobs: {lint: {runs-on: ubuntu-latest, timeout-minutes: 1, steps: [{run: cargo fmt --check}]}}\n";
        let found = analyze(ubuntu_lint);
        assert!(found.is_empty(), "{found:?}");

        let map_runner = "jobs:\n  lint:\n    runs-on: {group: large, labels: [self-hosted, macos]}\n    timeout-minutes: 5\n    steps:\n      - run: cargo fmt --check\n";
        let found = analyze(map_runner);
        let f1 = of_kind(&found, FindingKind::LintOnRunner);
        assert_eq!(f1.len(), 1, "{found:?}");
        assert_eq!(f1[0].line, 3);
        assert_eq!(f1[0].message, "lint on macOS");
        assert!(
            of_kind(&found, FindingKind::MissingTimeout).is_empty(),
            "{found:?}"
        );

        let block_map = "jobs:\n  prettier:\n    runs-on:\n      group: large\n      labels:\n        - self-hosted\n        - windows\n    timeout-minutes: 5\n    steps:\n      - run: npx prettier --check .\n";
        let found = analyze(block_map);
        let f1 = of_kind(&found, FindingKind::LintOnRunner);
        assert_eq!(f1.len(), 1, "{found:?}");
        assert_eq!(f1[0].line, 3);
        assert_eq!(f1[0].message, "lint on Windows");

        let merged = "defaults: &defaults\n  runs-on: ubuntu-latest\n  timeout-minutes: 10\n  steps:\n    - run: cargo test\non: pull_request\njobs:\n  lint:\n    <<: *defaults\n    runs-on: macos-latest\n    steps:\n      - run: cargo clippy -- -D warnings\n";
        let found = analyze(merged);
        let f1 = of_kind(&found, FindingKind::LintOnRunner);
        assert_eq!(f1.len(), 1, "{found:?}");
        assert_eq!(f1[0].line, 10);
        assert_eq!(f1[0].message, "lint on macOS");
        assert!(
            of_kind(&found, FindingKind::MissingTimeout).is_empty(),
            "{found:?}"
        );
        let f3 = of_kind(&found, FindingKind::PullRequestConcurrency);
        assert_eq!(f3.len(), 1, "{found:?}");
        assert_eq!(f3[0].line, 6);

        let inherited = "defaults: &defaults\n  name: clippy\n  runs-on: macos-latest\njobs:\n  build:\n    <<: *defaults\n";
        let found = analyze(inherited);
        let f1 = of_kind(&found, FindingKind::LintOnRunner);
        assert_eq!(f1.len(), 1, "{found:?}");
        assert_eq!(f1[0].line, 3);
        assert_eq!(f1[0].message, "lint on macOS");
        let f2 = of_kind(&found, FindingKind::MissingTimeout);
        assert_eq!(f2.len(), 1, "{found:?}");
        assert_eq!(f2[0].line, 5);
    }

    fn matrix_case(yaml: &str) -> (Vec<Finding>, Matrix) {
        let workflow = scan_yaml(Path::new("fixture.yml"), yaml).expect("valid fixture");
        let matrix = workflow.jobs[0].matrix.clone().expect("matrix");
        let found = findings(&[workflow]);
        (found, matrix)
    }

    fn f4_message(jobs: u64) -> String {
        format!("matrix runs {jobs} jobs; each job is billed in whole minutes")
    }

    fn f4_at_least(jobs: u64) -> String {
        let noun = if jobs == 1 { "job" } else { "jobs" };
        format!("matrix runs at least {jobs} {noun}; each job is billed in whole minutes")
    }

    #[test]
    fn f4_flags_two_by_three() {
        let yaml = "jobs:\n  build:\n    runs-on: ubuntu-latest\n    timeout-minutes: 5\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16, 18]\n";
        let (found, matrix) = matrix_case(yaml);
        assert_eq!(matrix.jobs, 6);
        assert!(!matrix.at_least);
        let f4 = of_kind(&found, FindingKind::MatrixJobs);
        assert_eq!(f4.len(), 1, "{found:?}");
        assert_eq!(f4[0].line, 6);
        assert_eq!(f4[0].path, Path::new("fixture.yml"));
        assert_eq!(f4[0].message, f4_message(6));
        assert!(!f4[0].message.contains("at least"));
        assert!(of_kind(&found, FindingKind::MissingTimeout).is_empty());
        assert!(of_kind(&found, FindingKind::LintOnRunner).is_empty());

        let block = "jobs:\n  build:\n    runs-on: ubuntu-latest\n    timeout-minutes: 5\n    strategy:\n      matrix:\n        os:\n          - ubuntu-latest\n          - windows-latest\n        node:\n          - 14\n          - 16\n          - 18\n";
        let (found, matrix) = matrix_case(block);
        assert_eq!(matrix.jobs, 6);
        assert!(!matrix.at_least);
        let f4 = of_kind(&found, FindingKind::MatrixJobs);
        assert_eq!(f4.len(), 1, "{found:?}");
        assert_eq!(f4[0].line, 6);
        assert_eq!(f4[0].message, f4_message(6));

        let bill = crate::rank::Bill {
            total_net: crate::money::Micros::ZERO,
            total_minutes: crate::money::Micros::ZERO,
            other_products_net: crate::money::Micros::ZERO,
            period_from: None,
            period_to: None,
            repositories: Vec::new(),
        };
        let text = crate::report::render(&[], &bill, Some(&found), None);
        assert!(
            text.contains(
                "F4 fixture.yml:6: matrix runs 6 jobs; each job is billed in whole minutes"
            ),
            "{text}"
        );
        let json = crate::report::render_json(&[], &bill, Some(&found), None, None);
        let value: serde_json::Value = serde_json::from_str(&json).expect("json");
        assert_eq!(value["unattached_findings"][0]["id"], "F4");
        assert_eq!(value["unattached_findings"][0]["line"], 6);
        assert_eq!(value["unattached_findings"][0]["file"], "fixture.yml");
        assert_eq!(value["unattached_findings"][0]["message"], f4_message(6));
    }

    #[test]
    fn f4_ignores_two_by_two() {
        let yaml = "jobs:\n  build:\n    runs-on: ubuntu-latest\n    timeout-minutes: 5\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16]\n";
        let (found, matrix) = matrix_case(yaml);
        assert_eq!(matrix.jobs, 4);
        assert!(!matrix.at_least);
        assert!(
            of_kind(&found, FindingKind::MatrixJobs).is_empty(),
            "{found:?}"
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn f4_ignores_two_by_three_with_one_exclude() {
        let yaml = "jobs:\n  build:\n    runs-on: ubuntu-latest\n    timeout-minutes: 5\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16, 18]\n        exclude:\n          - os: ubuntu-latest\n            node: 14\n";
        let (found, matrix) = matrix_case(yaml);
        assert_eq!(matrix.line, 6);
        assert_eq!(matrix.jobs, 5);
        assert!(!matrix.at_least);
        assert!(
            of_kind(&found, FindingKind::MatrixJobs).is_empty(),
            "{found:?}"
        );

        let flow = "jobs:\n  build:\n    timeout-minutes: 5\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16, 18]\n        exclude: [{os: ubuntu-latest, node: 14}]\n";
        let (found, matrix) = matrix_case(flow);
        assert_eq!(matrix.jobs, 5);
        assert!(of_kind(&found, FindingKind::MatrixJobs).is_empty());
    }

    #[test]
    fn f4_flags_two_by_two_plus_two_new_includes() {
        let yaml = "jobs:\n  build:\n    runs-on: ubuntu-latest\n    timeout-minutes: 5\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16]\n        include:\n          - os: macos-latest\n            node: 18\n          - os: macos-latest\n            node: 20\n";
        let (found, matrix) = matrix_case(yaml);
        assert_eq!(matrix.jobs, 6);
        assert!(!matrix.at_least);
        let f4 = of_kind(&found, FindingKind::MatrixJobs);
        assert_eq!(f4.len(), 1, "{found:?}");
        assert_eq!(f4[0].line, 6);
        assert_eq!(f4[0].path, Path::new("fixture.yml"));
        assert_eq!(f4[0].message, f4_message(6));

        let flow = "jobs:\n  build:\n    timeout-minutes: 5\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16]\n        include: [{os: macos-latest, node: 18}, {os: macos-latest, node: 20}]\n";
        let (found, matrix) = matrix_case(flow);
        assert_eq!(matrix.jobs, 6);
        let f4 = of_kind(&found, FindingKind::MatrixJobs);
        assert_eq!(f4.len(), 1, "{found:?}");
        assert_eq!(f4[0].line, 5);
        assert_eq!(f4[0].message, f4_message(6));
    }

    #[test]
    fn f4_expression_axis_says_at_least() {
        let wide = "jobs:\n  build:\n    timeout-minutes: 5\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16, 18]\n        extra: ${{ fromJSON(needs.setup.outputs.extra) }}\n";
        let (found, matrix) = matrix_case(wide);
        assert_eq!(matrix.jobs, 6);
        assert!(matrix.at_least);
        let f4 = of_kind(&found, FindingKind::MatrixJobs);
        assert_eq!(f4.len(), 1, "{found:?}");
        assert_eq!(f4[0].line, 5);
        assert_eq!(f4[0].message, f4_at_least(6));

        let rest = "jobs:\n  build:\n    timeout-minutes: 5\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: ${{ fromJSON(needs.setup.outputs.node) }}\n";
        let (found, matrix) = matrix_case(rest);
        assert_eq!(matrix.jobs, 2);
        assert!(matrix.at_least);
        let f4 = of_kind(&found, FindingKind::MatrixJobs);
        assert_eq!(f4.len(), 1, "{found:?}");
        assert_eq!(f4[0].line, 5);
        assert_eq!(f4[0].message, f4_at_least(2));

        let quoted = "jobs:\n  build:\n    timeout-minutes: 5\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: \"${{ fromJSON(needs.setup.outputs.node) }}\"\n";
        let (found, matrix) = matrix_case(quoted);
        assert_eq!(matrix.jobs, 2);
        assert!(matrix.at_least);
        assert_eq!(
            of_kind(&found, FindingKind::MatrixJobs)[0].message,
            f4_at_least(2)
        );

        let only = "jobs:\n  build:\n    timeout-minutes: 5\n    strategy:\n      matrix:\n        node: ${{ fromJSON(needs.setup.outputs.node) }}\n";
        let (found, matrix) = matrix_case(only);
        assert_eq!(matrix.jobs, 1);
        assert!(matrix.at_least);
        let f4 = of_kind(&found, FindingKind::MatrixJobs);
        assert_eq!(f4.len(), 1, "{found:?}");
        assert_eq!(f4[0].message, f4_at_least(1));

        let list_item = "jobs:\n  build:\n    timeout-minutes: 5\n    strategy:\n      matrix:\n        os: [ubuntu-latest, \"${{ matrix.extra }}\"]\n        node: [14, 16, 18]\n";
        let (found, matrix) = matrix_case(list_item);
        assert_eq!(matrix.jobs, 6);
        assert!(!matrix.at_least);
        assert_eq!(
            of_kind(&found, FindingKind::MatrixJobs)[0].message,
            f4_message(6)
        );
    }

    #[test]
    fn f4_flow_style_matrix_uses_matrix_line() {
        let inline = "jobs: {build: {timeout-minutes: 5, strategy: {matrix: {os: [ubuntu-latest, windows-latest], node: [14, 16, 18]}}}}\n";
        let (found, matrix) = matrix_case(inline);
        assert_eq!(matrix.jobs, 6);
        assert!(!matrix.at_least);
        let f4 = of_kind(&found, FindingKind::MatrixJobs);
        assert_eq!(f4.len(), 1, "{found:?}");
        assert_eq!(f4[0].line, 1);
        assert_eq!(f4[0].message, f4_message(6));

        let split = "jobs:\n  build:\n    timeout-minutes: 5\n    strategy:\n      matrix: {os: [ubuntu-latest, windows-latest], node: [14, 16, 18]}\n";
        let (found, matrix) = matrix_case(split);
        assert_eq!(matrix.line, 5);
        assert_eq!(matrix.jobs, 6);
        let f4 = of_kind(&found, FindingKind::MatrixJobs);
        assert_eq!(f4.len(), 1, "{found:?}");
        assert_eq!(f4[0].line, 5);
        assert_eq!(f4[0].path, Path::new("fixture.yml"));
        assert_eq!(f4[0].message, f4_message(6));
    }

    #[test]
    fn f4_expanding_include_and_partial_exclude_keep_real_counts() {
        let expanded = "jobs:\n  build:\n    timeout-minutes: 5\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16, 18]\n        include:\n          - os: ubuntu-latest\n            node: 14\n            npm: 6\n";
        let (found, matrix) = matrix_case(expanded);
        assert_eq!(matrix.jobs, 6);
        assert_eq!(
            of_kind(&found, FindingKind::MatrixJobs)[0].message,
            f4_message(6)
        );

        let partial = "jobs:\n  build:\n    timeout-minutes: 5\n    strategy:\n      matrix:\n        os: [macos-latest, windows-latest]\n        version: [12, 14, 16]\n        environment: [staging, production]\n        exclude:\n          - os: macos-latest\n            version: 12\n            environment: production\n          - os: windows-latest\n            version: 16\n";
        let (found, matrix) = matrix_case(partial);
        assert_eq!(matrix.jobs, 9);
        assert!(!matrix.at_least);
        assert_eq!(
            of_kind(&found, FindingKind::MatrixJobs)[0].message,
            f4_message(9)
        );

        let added_back = "jobs:\n  build:\n    timeout-minutes: 5\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16]\n        exclude:\n          - os: ubuntu-latest\n            node: 14\n        include:\n          - os: ubuntu-latest\n            node: 14\n            npm: 6\n";
        let (found, matrix) = matrix_case(added_back);
        assert_eq!(matrix.jobs, 4);
        assert!(of_kind(&found, FindingKind::MatrixJobs).is_empty());
    }

    #[test]
    fn f4_only_flags_the_wide_matrix_job() {
        let yaml = "jobs:\n  small:\n    timeout-minutes: 5\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16]\n  wide:\n    timeout-minutes: 5\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16, 18]\n  lint:\n    runs-on: macos-latest\n    strategy:\n      matrix:\n        node: [14, 16, 18, 20, 22, 24]\n";
        let workflow = scan_yaml(Path::new("fixture.yml"), yaml).expect("valid");
        assert_eq!(workflow.jobs[0].matrix.as_ref().unwrap().jobs, 4);
        assert_eq!(workflow.jobs[1].matrix.as_ref().unwrap().jobs, 6);
        assert_eq!(workflow.jobs[2].matrix.as_ref().unwrap().jobs, 6);
        let found = findings(&[workflow]);
        let f4 = of_kind(&found, FindingKind::MatrixJobs);
        assert_eq!(f4.len(), 2, "{found:?}");
        assert_eq!(f4[0].line, 11);
        assert_eq!(f4[0].message, f4_message(6));
        assert_eq!(f4[1].line, 17);
        assert_eq!(f4[1].message, f4_message(6));
        let f1 = of_kind(&found, FindingKind::LintOnRunner);
        assert_eq!(f1.len(), 1, "{found:?}");
        assert_eq!(f1[0].line, 15);
        assert_eq!(f1[0].message, "lint on macOS");
        let f2 = of_kind(&found, FindingKind::MissingTimeout);
        assert_eq!(f2.len(), 1, "{found:?}");
        assert_eq!(f2[0].line, 14);
    }

    fn usd(text: &str) -> crate::money::Micros {
        crate::money::Micros::parse(text).unwrap()
    }

    fn usage_row(
        sku: &str,
        date: &str,
        quantity: &str,
        price: &str,
        gross: &str,
        discount: &str,
        net: &str,
    ) -> UsageRow {
        UsageRow {
            line: 2,
            date: date.to_string(),
            product: "actions".to_string(),
            sku: sku.to_string(),
            quantity: usd(quantity),
            unit_type: "minutes".to_string(),
            applied_cost_per_quantity: usd(price),
            gross: usd(gross),
            discount: usd(discount),
            net: usd(net),
            username: String::new(),
            organization: "example-org".to_string(),
            repository: "example-org/app".to_string(),
            workflow_path: ".github/workflows/lint.yml".to_string(),
        }
    }

    #[test]
    fn invoice_accepts_118_minutes_at_0_062() {
        let row = usage_row(
            "actions_macos",
            "2026-09-01",
            "118",
            "0.062",
            "7.316",
            "0",
            "7.316",
        );
        let found = invoice_findings(Path::new("usage.csv"), std::slice::from_ref(&row));
        assert!(found.is_empty(), "{found:?}");
        assert_eq!(row.net, usd("7.316"));
        assert_eq!(
            crate::money::rounded_product(row.quantity, row.applied_cost_per_quantity),
            row.gross.0 as i128
        );
    }

    #[test]
    fn invoice_allows_one_micro_and_flags_two() {
        let exact = usage_row(
            "actions_macos",
            "2026-09-01",
            "118",
            "0.062",
            "7.316",
            "0",
            "7.316",
        );
        for (gross, expect_finding) in [
            ("7.315999", false),
            ("7.316000", false),
            ("7.316001", false),
            ("7.315998", true),
            ("7.316002", true),
        ] {
            let mut row = exact.clone();
            row.gross = usd(gross);
            row.net = usd(gross);
            let found = invoice_findings(Path::new("usage.csv"), &[row]);
            assert_eq!(found.is_empty(), !expect_finding, "{gross} {found:?}");
            if expect_finding {
                assert_eq!(found[0].kind.code(), "F9");
                assert!(
                    found[0].message.starts_with("invoice row mismatch:"),
                    "{found:?}"
                );
                assert!(
                    found[0].message.contains("sku \"actions_macos\""),
                    "{found:?}"
                );
                assert!(
                    found[0].message.contains("date \"2026-09-01\""),
                    "{found:?}"
                );
                assert!(
                    found[0].message.contains(&format!(
                        "gross {gross} != quantity * applied_cost_per_quantity 7.316000"
                    )),
                    "{found:?}"
                );
                assert!(!found[0].message.contains("gross - discount"), "{found:?}");
            }
        }
    }

    #[test]
    fn invoice_rounds_a_half_micro_before_the_tolerance() {
        // 0.5 * 0.000001 = 0.5 micros, which rounds to 1 micro (0.000001).
        let price = "0.000001";
        for (gross, net, flagged) in [
            ("0.000000", "0.000000", false),
            ("0.000001", "0.000001", false),
            ("0.000002", "0.000002", false),
            ("0.000003", "0.000003", true),
            ("-0.000001", "-0.000001", true),
        ] {
            let row = usage_row(
                "actions_storage",
                "2026-09-02",
                "0.5",
                price,
                gross,
                "0",
                net,
            );
            let found = invoice_findings(Path::new("usage.csv"), &[row]);
            assert_eq!(found.is_empty(), !flagged, "{gross} {found:?}");
        }

        let negative = usage_row(
            "actions_storage",
            "2026-09-02",
            "-0.5",
            price,
            "-0.000001",
            "0",
            "-0.000001",
        );
        assert!(invoice_findings(Path::new("usage.csv"), &[negative]).is_empty());
    }

    #[test]
    fn invoice_net_must_equal_gross_minus_discount() {
        let matched = usage_row(
            "actions_macos",
            "2026-09-01",
            "118",
            "0.062",
            "7.316",
            "1",
            "6.316",
        );
        assert!(invoice_findings(Path::new("usage.csv"), &[matched]).is_empty());

        let off_by_one = usage_row(
            "actions_macos",
            "2026-09-01",
            "118",
            "0.062",
            "7.316",
            "0",
            "7.315999",
        );
        let found = invoice_findings(Path::new("usage.csv"), &[off_by_one]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0]
                .message
                .contains("net 7.315999 != gross - discount 7.316000"),
            "{found:?}"
        );
        assert!(!found[0].message.contains("quantity *"), "{found:?}");

        let both = usage_row(
            "Actions_MacOS",
            "2026-09-14",
            "118",
            "0.080",
            "7.316",
            "1",
            "7.316",
        );
        let found = invoice_findings(Path::new("bill.csv"), &[both]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].kind, FindingKind::InvoiceMismatch);
        assert_eq!(found[0].line, 2);
        assert_eq!(found[0].path, Path::new("bill.csv"));
        assert_eq!(found[0].repository.as_deref(), Some("example-org/app"));
        assert_eq!(
            found[0].workflow_path.as_deref(),
            Some(".github/workflows/lint.yml")
        );
        assert_eq!(
            found[0].message,
            "invoice row mismatch: sku \"Actions_MacOS\", date \"2026-09-14\", gross 7.316000 != quantity * applied_cost_per_quantity 9.440000, net 7.316000 != gross - discount 6.316000"
        );
    }

    #[test]
    fn invoice_keeps_an_empty_sku_and_date() {
        let row = usage_row("", "", "10", "0.5", "1", "0", "1");
        let found = invoice_findings(Path::new("usage.csv"), &[row]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].message.contains("sku \"\""), "{found:?}");
        assert!(found[0].message.contains("date \"\""), "{found:?}");
        assert!(
            found[0]
                .message
                .contains("gross 1.000000 != quantity * applied_cost_per_quantity 5.000000"),
            "{found:?}"
        );
    }

    #[test]
    fn sample_usage_has_no_invoice_mismatch() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/usage-sample.csv");
        let rows = crate::usage::read_usage(&path).unwrap();
        let found = invoice_findings(&path, &rows);
        assert!(found.is_empty(), "{found:?}");
        assert_eq!(rows[0].line, 2);
        assert_eq!(rows[0].quantity, usd("118"));
        assert_eq!(rows[0].applied_cost_per_quantity, usd("0.062"));
        assert_eq!(rows[0].gross, usd("7.316"));
        assert_eq!(rows[0].net, usd("7.316"));
    }
}
