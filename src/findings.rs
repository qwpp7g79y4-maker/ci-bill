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
    /// `pull_request` or `pull_request_target` without cancel-in-progress.
    PullRequestConcurrency,
    /// Matrix expands to at least six billed jobs, or an axis length is an expression.
    MatrixJobs,
    /// The same mapping key is written more than once. The later value is kept.
    DuplicateKey,
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
            Self::DuplicateKey => "F8",
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
        if pulls_without_cancel(workflow) {
            if let Some(line) = workflow.on_line {
                push(
                    &mut found,
                    &workflow.path,
                    line,
                    FindingKind::PullRequestConcurrency,
                    pr_cancel_message(workflow),
                );
            }
        }
        for dup in &workflow.duplicate_keys {
            push(
                &mut found,
                &workflow.path,
                dup.second,
                FindingKind::DuplicateKey,
                &format!(
                    "duplicate key {} (line {} and {})",
                    dup.key, dup.first, dup.second
                ),
            );
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

/// Workflow-level cancel, or cancel on any job, covers the workflow.
fn pulls_without_cancel(workflow: &Workflow) -> bool {
    let pulls = workflow.has_pull_request || workflow.has_pull_request_target;
    pulls && !workflow_cancels(workflow)
}

fn workflow_cancels(workflow: &Workflow) -> bool {
    workflow.cancel_in_progress || workflow.jobs.iter().any(|job| job.cancel_in_progress)
}

fn pr_cancel_message(workflow: &Workflow) -> &'static str {
    if workflow.has_pull_request {
        "pull_request without concurrency cancel-in-progress"
    } else {
        "pull_request_target without concurrency cancel-in-progress"
    }
}

fn note_job(found: &mut Vec<Finding>, path: &Path, job: &Job) {
    if let Some(runs_on) = &job.runs_on {
        note_runner(found, path, job, runs_on);
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

/// Six or more concrete jobs, a finite upper bound of at least six, or any
/// expression axis whose lower bound is at least one.
fn matrix_message(matrix: &Matrix) -> Option<String> {
    if let Some(hi) = matrix.jobs_hi {
        if hi < 6 {
            return None;
        }
        let noun = if hi == 1 { "job" } else { "jobs" };
        return Some(format!(
            "matrix runs between {} and {hi} {noun}; each job is billed in whole minutes",
            matrix.jobs
        ));
    }
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

fn note_runner(found: &mut Vec<Finding>, path: &Path, job: &Job, runs_on: &RunsOn) {
    if !job_is_lint(job) || self_hosted(&runs_on.labels) {
        return;
    }
    let (macos, windows) = label_set_os(&runs_on.labels);
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
    let Some(matrix) = &job.matrix else {
        return;
    };
    for hit in matrix_runner_hits(runs_on, matrix) {
        if hit.macos {
            push(
                found,
                path,
                runs_on.line,
                FindingKind::LintOnRunner,
                &axis_lint_message("macOS", runs_on.line, &hit.axis, hit.line),
            );
        }
        if hit.windows {
            push(
                found,
                path,
                runs_on.line,
                FindingKind::LintOnRunner,
                &axis_lint_message("Windows", runs_on.line, &hit.axis, hit.line),
            );
        }
    }
}

fn axis_lint_message(os: &str, runs_on_line: usize, axis: &str, axis_line: usize) -> String {
    format!("lint on {os} (runs-on line {runs_on_line}, axis {axis} line {axis_line})")
}

struct AxisHit {
    macos: bool,
    windows: bool,
    axis: String,
    line: usize,
}

fn matrix_runner_hits(runs_on: &RunsOn, matrix: &crate::workflow::Matrix) -> Vec<AxisHit> {
    let mut hits = Vec::new();
    for name in runs_on_matrix_names(runs_on) {
        let Some(axis) = matrix.axes.iter().find(|axis| axis.name == name) else {
            continue;
        };
        let mut macos = false;
        let mut windows = false;
        for labels in &axis.values {
            let (axis_macos, axis_windows) = label_set_os(labels);
            macos |= axis_macos;
            windows |= axis_windows;
        }
        if macos || windows {
            hits.push(AxisHit {
                macos,
                windows,
                axis: axis.name.clone(),
                line: axis.line,
            });
        }
    }
    hits
}

fn runs_on_matrix_names(runs_on: &RunsOn) -> Vec<String> {
    let mut names = matrix_axis_names(&runs_on.value);
    for label in &runs_on.labels {
        for name in matrix_axis_names(label) {
            push_unique(&mut names, name);
        }
    }
    names
}

fn push_unique(names: &mut Vec<String>, name: String) {
    if !names.iter().any(|item| item == &name) {
        names.push(name);
    }
}

/// `matrix.name` and `matrix['name']` / `matrix["name"]`, including inside a longer scalar.
fn matrix_axis_names(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut names = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if !starts_with_matrix(&chars, index) {
            index += 1;
            continue;
        }
        let boundary = index == 0 || !is_name_char(chars[index - 1]);
        let after = index + MATRIX.len();
        if !boundary {
            index += 1;
            continue;
        }
        if let Some(end) = dotted_axis_end(&chars, after) {
            push_unique(&mut names, chars[after + 1..end].iter().collect());
            index = end;
            continue;
        }
        if let Some((name, end)) = bracket_axis(&chars, after) {
            push_unique(&mut names, name);
            index = end;
            continue;
        }
        index += 1;
    }
    names
}

const MATRIX: &[char] = &['m', 'a', 't', 'r', 'i', 'x'];

fn starts_with_matrix(chars: &[char], index: usize) -> bool {
    index + MATRIX.len() <= chars.len() && chars[index..index + MATRIX.len()] == MATRIX[..]
}

fn is_name_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

fn dotted_axis_end(chars: &[char], after_matrix: usize) -> Option<usize> {
    if after_matrix >= chars.len() || chars[after_matrix] != '.' {
        return None;
    }
    let start = after_matrix + 1;
    if start >= chars.len() || !(chars[start].is_ascii_alphabetic() || chars[start] == '_') {
        return None;
    }
    let mut end = start + 1;
    while end < chars.len() && (is_name_char(chars[end]) || chars[end] == '-') {
        end += 1;
    }
    while end > start && chars[end - 1] == '-' {
        end -= 1;
    }
    if end == start {
        None
    } else {
        Some(end)
    }
}

fn bracket_axis(chars: &[char], after_matrix: usize) -> Option<(String, usize)> {
    let mut index = skip_ws(chars, after_matrix);
    if index >= chars.len() || chars[index] != '[' {
        return None;
    }
    index = skip_ws(chars, index + 1);
    if index >= chars.len() {
        return None;
    }
    let quote = chars[index];
    if quote != '\'' && quote != '"' {
        return None;
    }
    index += 1;
    let start = index;
    while index < chars.len() && chars[index] != quote {
        index += 1;
    }
    if index >= chars.len() {
        return None;
    }
    let name: String = chars[start..index].iter().collect();
    index = skip_ws(chars, index + 1);
    if name.is_empty() || index >= chars.len() || chars[index] != ']' {
        return None;
    }
    Some((name, index + 1))
}

fn skip_ws(chars: &[char], mut index: usize) -> usize {
    while index < chars.len() && chars[index].is_whitespace() {
        index += 1;
    }
    index
}

fn self_hosted(labels: &[String]) -> bool {
    labels
        .iter()
        .any(|label| label.eq_ignore_ascii_case("self-hosted"))
}

/// A label set that contains `self-hosted` is never a paid macOS or Windows runner.
fn label_set_os(labels: &[String]) -> (bool, bool) {
    if self_hosted(labels) {
        return (false, false);
    }
    let mut macos = false;
    let mut windows = false;
    for label in labels {
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
        assert_eq!(f1.len(), 1, "{found:?}");
        assert_eq!(f1[0].line, 8);
        assert_eq!(f1[0].message, "lint on Windows");
        assert!(
            f1.iter().all(|item| item.line != 4),
            "self-hosted label set is not paid macOS: {found:?}"
        );
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
        assert!(
            of_kind(&found, FindingKind::LintOnRunner).is_empty(),
            "self-hosted label set is not paid macOS: {found:?}"
        );
        assert!(
            of_kind(&found, FindingKind::MissingTimeout).is_empty(),
            "{found:?}"
        );

        let block_map = "jobs:\n  prettier:\n    runs-on:\n      group: large\n      labels:\n        - self-hosted\n        - windows\n    timeout-minutes: 5\n    steps:\n      - run: npx prettier --check .\n";
        let found = analyze(block_map);
        assert!(
            of_kind(&found, FindingKind::LintOnRunner).is_empty(),
            "self-hosted label set is not paid Windows: {found:?}"
        );

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

    fn f4_between(lo: u64, hi: u64) -> String {
        let noun = if hi == 1 { "job" } else { "jobs" };
        format!("matrix runs between {lo} and {hi} {noun}; each job is billed in whole minutes")
    }

    fn f8(key: &str, first: usize, second: usize) -> String {
        format!("duplicate key {key} (line {first} and {second})")
    }

    #[test]
    fn f8_reports_the_later_runs_on_value() {
        let yaml = "jobs:\n  lint:\n    runs-on: ubuntu-latest\n    runs-on: macos-latest\n    steps:\n      - run: cargo fmt --check\n";
        let found = analyze(yaml);
        let f8s = of_kind(&found, FindingKind::DuplicateKey);
        assert_eq!(f8s.len(), 1, "{found:?}");
        assert_eq!(f8s[0].line, 4);
        assert_eq!(f8s[0].message, f8("runs-on", 3, 4));
        let f1 = of_kind(&found, FindingKind::LintOnRunner);
        assert_eq!(f1.len(), 1, "{found:?}");
        assert_eq!(f1[0].line, 4);
        assert_eq!(f1[0].message, "lint on macOS");

        let crlf = analyze(&yaml.replace('\n', "\r\n"));
        let f8s = of_kind(&crlf, FindingKind::DuplicateKey);
        assert_eq!(f8s[0].message, f8("runs-on", 3, 4));
        assert_eq!(
            of_kind(&crlf, FindingKind::LintOnRunner)[0].message,
            "lint on macOS"
        );

        let reversed = analyze(
            "jobs:\n  lint:\n    runs-on: macos-latest\n    runs-on: ubuntu-latest\n    steps:\n      - run: cargo fmt --check\n",
        );
        assert_eq!(
            of_kind(&reversed, FindingKind::DuplicateKey)[0].message,
            f8("runs-on", 3, 4)
        );
        assert!(of_kind(&reversed, FindingKind::LintOnRunner).is_empty());

        let capitals = analyze(
            "jobs:\n  lint:\n    runs-on: ubuntu-latest\n    runs-on: MacOS-latest\n    steps:\n      - run: cargo fmt --check\n",
        );
        assert_eq!(
            of_kind(&capitals, FindingKind::LintOnRunner)[0].message,
            "lint on macOS"
        );
        assert_eq!(
            of_kind(&capitals, FindingKind::DuplicateKey)[0].message,
            f8("runs-on", 3, 4)
        );

        let flow = analyze(
            "jobs:\n  lint: {\n    runs-on: ubuntu-latest,\n    runs-on: macos-latest,\n    steps: [{run: cargo fmt --check}]\n  }\n",
        );
        assert_eq!(
            of_kind(&flow, FindingKind::DuplicateKey)[0].message,
            f8("runs-on", 3, 4)
        );
        assert_eq!(
            of_kind(&flow, FindingKind::LintOnRunner)[0].message,
            "lint on macOS"
        );

        let quoted = analyze(
            "jobs:\n  lint:\n    \"runs-on\": \"ubuntu-latest\"\n    \"runs-on\": \"macos-latest\"\n    steps:\n      - run: cargo fmt --check\n",
        );
        assert_eq!(
            of_kind(&quoted, FindingKind::DuplicateKey)[0].message,
            f8("runs-on", 3, 4)
        );

        let empty_then = analyze(
            "jobs:\n  lint:\n    runs-on: \"\"\n    runs-on: macos-latest\n    steps:\n      - run: cargo fmt --check\n",
        );
        assert_eq!(
            of_kind(&empty_then, FindingKind::LintOnRunner)[0].message,
            "lint on macOS"
        );

        let value_then_empty = analyze(
            "jobs:\n  lint:\n    runs-on: macos-latest\n    runs-on: \"\"\n    steps:\n      - run: cargo fmt --check\n",
        );
        assert!(of_kind(&value_then_empty, FindingKind::LintOnRunner).is_empty());
        assert_eq!(
            of_kind(&value_then_empty, FindingKind::DuplicateKey)[0].message,
            f8("runs-on", 3, 4)
        );

        let third = analyze(
            "jobs:\n  lint:\n    runs-on: ubuntu-latest\n    runs-on: windows-latest\n    runs-on: macos-latest\n    steps:\n      - run: cargo fmt --check\n",
        );
        let f8s = of_kind(&third, FindingKind::DuplicateKey);
        assert_eq!(f8s.len(), 2, "{third:?}");
        assert_eq!(f8s[0].message, f8("runs-on", 3, 4));
        assert_eq!(f8s[1].message, f8("runs-on", 4, 5));
        assert_eq!(of_kind(&third, FindingKind::LintOnRunner).len(), 1);

        let unchanged = analyze(
            "defaults: &defaults\n  runs-on: ubuntu-latest\njobs:\n  lint:\n    <<: *defaults\n    runs-on: macos-latest\n    steps:\n      - run: cargo fmt --check\n",
        );
        assert!(of_kind(&unchanged, FindingKind::DuplicateKey).is_empty());
        assert_eq!(
            of_kind(&unchanged, FindingKind::LintOnRunner)[0].message,
            "lint on macOS"
        );

        let explicit_first = analyze(
            "defaults: &defaults\n  runs-on: macos-latest\njobs:\n  lint:\n    runs-on: ubuntu-latest\n    <<: *defaults\n    steps:\n      - run: cargo fmt --check\n",
        );
        assert!(of_kind(&explicit_first, FindingKind::DuplicateKey).is_empty());
        assert!(of_kind(&explicit_first, FindingKind::LintOnRunner).is_empty());

        let missing = analyze(
            "jobs:\n  lint:\n    runs-on: ubuntu-latest\n    steps:\n      - run: cargo fmt --check\n",
        );
        assert!(of_kind(&missing, FindingKind::DuplicateKey).is_empty());
        assert!(of_kind(&missing, FindingKind::LintOnRunner).is_empty());
    }

    #[test]
    fn f4_dynamic_exclude_says_between_lower_and_upper() {
        let yaml = "jobs:\n  build:\n    runs-on: ubuntu-latest\n    timeout-minutes: 5\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16, 18]\n        exclude: ${{ inputs.rules }}\n";
        let (found, matrix) = matrix_case(yaml);
        assert_eq!(matrix.jobs, 0);
        assert_eq!(matrix.jobs_hi, Some(6));
        assert!(!matrix.at_least);
        let f4 = of_kind(&found, FindingKind::MatrixJobs);
        assert_eq!(f4.len(), 1, "{found:?}");
        assert_eq!(f4[0].line, 6);
        assert_eq!(f4[0].message, f4_between(0, 6));

        let crlf = yaml.replace('\n', "\r\n");
        let (found, matrix) = matrix_case(&crlf);
        assert_eq!(matrix.jobs_hi, Some(6));
        assert_eq!(
            of_kind(&found, FindingKind::MatrixJobs)[0].message,
            f4_between(0, 6)
        );

        let added = "jobs:\n  build:\n    timeout-minutes: 5\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16, 18]\n        exclude: ${{ inputs.rules }}\n        include:\n          - os: macos-latest\n            node: 20\n";
        let (found, matrix) = matrix_case(added);
        assert_eq!(matrix.jobs, 1);
        assert_eq!(matrix.jobs_hi, Some(7));
        assert_eq!(
            of_kind(&found, FindingKind::MatrixJobs)[0].message,
            f4_between(1, 7)
        );

        let narrow = "jobs:\n  build:\n    timeout-minutes: 5\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16]\n        exclude: ${{ inputs.rules }}\n";
        let (found, matrix) = matrix_case(narrow);
        assert_eq!(matrix.jobs, 0);
        assert_eq!(matrix.jobs_hi, Some(4));
        assert!(of_kind(&found, FindingKind::MatrixJobs).is_empty());

        let chain = "jobs:\n  build:\n    timeout-minutes: 5\n    strategy:\n      matrix:\n        include:\n          - os: macos-latest\n          - os: macos-latest\n            npm: \"6\"\n";
        let (found, matrix) = matrix_case(chain);
        assert_eq!(matrix.jobs, 1);
        assert!(matrix.jobs_hi.is_none());
        assert!(of_kind(&found, FindingKind::MatrixJobs).is_empty());
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

    fn joined(lines: &[&str]) -> String {
        let mut text = lines.join("\n");
        text.push('\n');
        text
    }

    fn crlf(text: &str) -> String {
        text.replace('\n', "\r\n")
    }

    fn axis_msg(os: &str, runs_on: usize, axis: &str, axis_line: usize) -> String {
        format!("lint on {os} (runs-on line {runs_on}, axis {axis} line {axis_line})")
    }

    fn assert_f1(found: &[Finding], line: usize, messages: &[&str]) {
        let f1 = of_kind(found, FindingKind::LintOnRunner);
        let got: Vec<&str> = f1.iter().map(|item| item.message.as_str()).collect();
        assert_eq!(got, messages, "{found:?}");
        assert!(f1.iter().all(|item| item.line == line), "{found:?}");
        assert!(
            f1.iter().all(|item| item.path == Path::new("fixture.yml")),
            "{found:?}"
        );
    }

    fn assert_no_f1(found: &[Finding]) {
        assert!(
            of_kind(found, FindingKind::LintOnRunner).is_empty(),
            "{found:?}"
        );
    }

    const F3_PR: &str = "pull_request without concurrency cancel-in-progress";
    const F3_TARGET: &str = "pull_request_target without concurrency cancel-in-progress";

    fn assert_f3(found: &[Finding], line: usize, message: &str) {
        let f3 = of_kind(found, FindingKind::PullRequestConcurrency);
        assert_eq!(f3.len(), 1, "{found:?}");
        assert_eq!(f3[0].line, line, "{found:?}");
        assert_eq!(f3[0].message, message, "{found:?}");
        assert_eq!(f3[0].path, Path::new("fixture.yml"));
    }

    fn assert_no_f3(found: &[Finding]) {
        assert!(
            of_kind(found, FindingKind::PullRequestConcurrency).is_empty(),
            "{found:?}"
        );
    }

    #[test]
    fn matrix_runs_on_couples_axis_and_include_values() {
        let positive = joined(&[
            "jobs:",                                       // 1
            "  lint:",                                     // 2
            "    runs-on: ${{ matrix.os }}",               // 3
            "    timeout-minutes: 5",                      // 4
            "    steps:",                                  // 5
            "      - run: cargo fmt --check",              // 6
            "    strategy:",                               // 7
            "      matrix:",                               // 8
            "        os: [ubuntu-latest, windows-latest]", // 9
            "        include:",                            // 10
            "          - os: macos-latest",                // 11
        ]);
        let windows = axis_msg("Windows", 3, "os", 9);
        let macos = axis_msg("macOS", 3, "os", 9);
        assert_f1(&analyze(&positive), 3, &[&windows, &macos]);
        assert_f1(&analyze(&crlf(&positive)), 3, &[&windows, &macos]);

        let include_only = joined(&[
            "jobs:",                          // 1
            "  lint:",                        // 2
            "    runs-on: ${{ matrix.os }}",  // 3
            "    timeout-minutes: 5",         // 4
            "    steps:",                     // 5
            "      - run: cargo fmt --check", // 6
            "    strategy:",                  // 7
            "      matrix:",                  // 8
            "        include:",               // 9
            "          - os: macos-latest",   // 10
            "            npm: \"9\"",         // 11
            "          - os: ubuntu-latest",  // 12
        ]);
        assert_f1(
            &analyze(&include_only),
            3,
            &[&axis_msg("macOS", 3, "os", 10)],
        );

        let include_first = joined(&[
            "jobs:",                          // 1
            "  lint:",                        // 2
            "    runs-on: ${{ matrix.os }}",  // 3
            "    timeout-minutes: 5",         // 4
            "    steps:",                     // 5
            "      - run: cargo fmt --check", // 6
            "    strategy:",                  // 7
            "      matrix:",                  // 8
            "        include:",               // 9
            "          - os: macos-latest",   // 10
            "        os: [ubuntu-latest]",    // 11
        ]);
        assert_f1(
            &analyze(&include_first),
            3,
            &[&axis_msg("macOS", 3, "os", 11)],
        );
        assert_f1(
            &analyze(&crlf(&include_first)),
            3,
            &[&axis_msg("macOS", 3, "os", 11)],
        );

        let expression_axis = joined(&[
            "jobs:",                          // 1
            "  lint:",                        // 2
            "    runs-on: ${{ matrix.os }}",  // 3
            "    timeout-minutes: 5",         // 4
            "    steps:",                     // 5
            "      - run: cargo fmt --check", // 6
            "    strategy:",                  // 7
            "      matrix:",                  // 8
            "        include:",               // 9
            "          - os: windows-latest", // 10
            "        os: ${{ inputs.os }}",   // 11
        ]);
        assert_f1(
            &analyze(&expression_axis),
            3,
            &[&axis_msg("Windows", 3, "os", 11)],
        );

        let capitals = joined(&[
            "jobs:",                                      // 1
            "  lint:",                                    // 2
            "    runs-on: ${{ matrix.OS }}",              // 3
            "    timeout-minutes: 5",                     // 4
            "    steps:",                                 // 5
            "      - run: cargo fmt --check",             // 6
            "    strategy:",                              // 7
            "      matrix:",                              // 8
            "        OS: [MacOS-latest, WINDOWS-latest]", // 9
            "        include:",                           // 10
            "          - OS: ubuntu-latest",              // 11
        ]);
        let windows = axis_msg("Windows", 3, "OS", 9);
        let macos = axis_msg("macOS", 3, "OS", 9);
        assert_f1(&analyze(&capitals), 3, &[&windows, &macos]);

        let reordered = joined(&[
            "jobs:",                          // 1
            "  lint:",                        // 2
            "    strategy:",                  // 3
            "      matrix:",                  // 4
            "        os: [macos-latest]",     // 5
            "    steps:",                     // 6
            "      - run: cargo fmt --check", // 7
            "    timeout-minutes: 5",         // 8
            "    runs-on: ${{ matrix.os }}",  // 9
        ]);
        assert_f1(&analyze(&reordered), 9, &[&axis_msg("macOS", 9, "os", 5)]);

        let split_values = joined(&[
            "jobs:",                                   // 1
            "  prettier:",                             // 2
            "    runs-on: ${{ matrix.os }}",           // 3
            "    timeout-minutes: 5",                  // 4
            "    steps:",                              // 5
            "      - run: npx prettier --check .",     // 6
            "    strategy:",                           // 7
            "      matrix:",                           // 8
            "        os: [self-hosted, macos-latest]", // 9
        ]);
        assert_f1(
            &analyze(&split_values),
            3,
            &[&axis_msg("macOS", 3, "os", 9)],
        );

        let reversed_split = joined(&[
            "jobs:",                                    // 1
            "  lint:",                                  // 2
            "    runs-on: ${{ matrix.os }}",            // 3
            "    timeout-minutes: 5",                   // 4
            "    steps:",                               // 5
            "      - run: cargo clippy -- -D warnings", // 6
            "    strategy:",                            // 7
            "      matrix:",                            // 8
            "        os: [WINDOWS-2022, Self-Hosted]",  // 9
        ]);
        assert_f1(
            &analyze(&reversed_split),
            3,
            &[&axis_msg("Windows", 3, "os", 9)],
        );

        let map_value = joined(&[
            "jobs:",                                    // 1
            "  lint:",                                  // 2
            "    runs-on: ${{ matrix.os }}",            // 3
            "    timeout-minutes: 5",                   // 4
            "    steps:",                               // 5
            "      - run: cargo fmt --check",           // 6
            "    strategy:",                            // 7
            "      matrix:",                            // 8
            "        os:",                              // 9
            "          - group: large",                 // 10
            "            labels: [self-hosted, macos]", // 11
            "          - group: large",                 // 12
            "            labels: [windows-latest]",     // 13
        ]);
        assert_f1(&analyze(&map_value), 3, &[&axis_msg("Windows", 3, "os", 9)]);

        let excluded = joined(&[
            "jobs:",                                     // 1
            "  lint:",                                   // 2
            "    runs-on: ${{ matrix.os }}",             // 3
            "    timeout-minutes: 5",                    // 4
            "    steps:",                                // 5
            "      - run: cargo fmt --check",            // 6
            "    strategy:",                             // 7
            "      matrix:",                             // 8
            "        os: [ubuntu-latest, macos-latest]", // 9
            "        exclude:",                          // 10
            "          - os: macos-latest",              // 11
        ]);
        assert_f1(&analyze(&excluded), 3, &[&axis_msg("macOS", 3, "os", 9)]);

        let flow = joined(&[
            "jobs: {",                                      // 1
            "  lint: {",                                    // 2
            "    runs-on: \"${{ matrix.os }}\",",           // 3
            "    timeout-minutes: 5,",                      // 4
            "    steps: [{run: cargo fmt --check}],",       // 5
            "    strategy: {",                              // 6
            "      matrix: {",                              // 7
            "        os: [ubuntu-latest, windows-latest],", // 8
            "        include: [{os: macos-latest}]",        // 9
            "      }",                                      // 10
            "    }",                                        // 11
            "  }",                                          // 12
            "}",                                            // 13
        ]);
        let windows = axis_msg("Windows", 3, "os", 8);
        let macos = axis_msg("macOS", 3, "os", 8);
        assert_f1(&analyze(&flow), 3, &[&windows, &macos]);
        assert_f1(&analyze(&crlf(&flow)), 3, &[&windows, &macos]);

        let ubuntu = joined(&[
            "jobs:",                                     // 1
            "  lint:",                                   // 2
            "    runs-on: ${{ matrix.os }}",             // 3
            "    timeout-minutes: 5",                    // 4
            "    steps:",                                // 5
            "      - run: cargo fmt --check",            // 6
            "    strategy:",                             // 7
            "      matrix:",                             // 8
            "        os: [ubuntu-latest, ubuntu-22.04]", // 9
            "        include:",                          // 10
            "          - os: ubuntu-24.04",              // 11
        ]);
        assert_no_f1(&analyze(&ubuntu));
        assert_no_f1(&analyze(&crlf(&ubuntu)));

        let nested_hosted = joined(&[
            "jobs:",                              // 1
            "  lint:",                            // 2
            "    runs-on: ${{ matrix.os }}",      // 3
            "    timeout-minutes: 5",             // 4
            "    steps:",                         // 5
            "      - run: cargo fmt --check",     // 6
            "    strategy:",                      // 7
            "      matrix:",                      // 8
            "        os:",                        // 9
            "          - [self-hosted, macos]",   // 10
            "          - [Self-Hosted, Windows]", // 11
        ]);
        assert_no_f1(&analyze(&nested_hosted));

        let include_hosted = joined(&[
            "jobs:",                                  // 1
            "  lint:",                                // 2
            "    runs-on: ${{ matrix.os }}",          // 3
            "    timeout-minutes: 5",                 // 4
            "    steps:",                             // 5
            "      - run: cargo fmt --check",         // 6
            "    strategy:",                          // 7
            "      matrix:",                          // 8
            "        include:",                       // 9
            "          - os: [self-hosted, macos]",   // 10
            "          - os: [windows, Self-Hosted]", // 11
        ]);
        assert_no_f1(&analyze(&include_hosted));

        let not_lint = joined(&[
            "jobs:",                                      // 1
            "  test:",                                    // 2
            "    runs-on: ${{ matrix.os }}",              // 3
            "    timeout-minutes: 5",                     // 4
            "    steps:",                                 // 5
            "      - run: cargo test",                    // 6
            "    strategy:",                              // 7
            "      matrix:",                              // 8
            "        os: [macos-latest, windows-latest]", // 9
        ]);
        assert_no_f1(&analyze(&not_lint));

        let other_axis = joined(&[
            "jobs:",                                      // 1
            "  lint:",                                    // 2
            "    runs-on: ${{ matrix.node }}",            // 3
            "    timeout-minutes: 5",                     // 4
            "    steps:",                                 // 5
            "      - run: cargo fmt --check",             // 6
            "    strategy:",                              // 7
            "      matrix:",                              // 8
            "        os: [macos-latest, windows-latest]", // 9
            "        node: ['14', '16']",                 // 10
        ]);
        assert_no_f1(&analyze(&other_axis));

        let wrong_case = joined(&[
            "jobs:",                                      // 1
            "  lint:",                                    // 2
            "    runs-on: ${{ matrix.os }}",              // 3
            "    timeout-minutes: 5",                     // 4
            "    steps:",                                 // 5
            "      - run: cargo fmt --check",             // 6
            "    strategy:",                              // 7
            "      matrix:",                              // 8
            "        OS: [MacOS-latest, WINDOWS-latest]", // 9
        ]);
        assert_no_f1(&analyze(&wrong_case));

        let missing = [
            joined(&[
                "jobs:",
                "  lint:",
                "    runs-on: ${{ matrix.os }}",
                "    timeout-minutes: 5",
                "    steps:",
                "      - run: cargo fmt --check",
            ]),
            joined(&[
                "jobs:",
                "  lint:",
                "    runs-on: ${{ matrix.os }}",
                "    timeout-minutes: 5",
                "    steps:",
                "      - run: cargo fmt --check",
                "    strategy:",
                "      matrix: {}",
            ]),
            joined(&[
                "jobs:",
                "  lint:",
                "    runs-on: ${{ matrix.os }}",
                "    timeout-minutes: 5",
                "    steps:",
                "      - run: cargo fmt --check",
                "    strategy:",
                "      matrix:",
                "        os: []",
            ]),
            joined(&[
                "jobs:",
                "  lint:",
                "    runs-on: ${{ matrix.os }}",
                "    timeout-minutes: 5",
                "    steps:",
                "      - run: cargo fmt --check",
                "    strategy:",
                "      matrix:",
                "        os: ${{ inputs.os }}",
            ]),
            joined(&[
                "jobs:",
                "  lint:",
                "    runs-on: \"\"",
                "    timeout-minutes: 5",
                "    steps:",
                "      - run: cargo fmt --check",
                "    strategy:",
                "      matrix:",
                "        os: [macos-latest]",
            ]),
            joined(&[
                "jobs:",
                "  lint:",
                "    timeout-minutes: 5",
                "    steps:",
                "      - run: cargo fmt --check",
                "    strategy:",
                "      matrix:",
                "        os: [macos-latest, windows-latest]",
            ]),
            joined(&[
                "jobs:",
                "  lint:",
                "    runs-on: ${{ matrix.os }}",
                "    timeout-minutes: 5",
                "    steps:",
                "      - run: cargo fmt --check",
                "    strategy:",
                "      matrix:",
                "        include: []",
            ]),
            joined(&[
                "jobs:",
                "  lint:",
                "    runs-on: ${{ matrix.missing }}",
                "    timeout-minutes: 5",
                "    steps:",
                "      - run: cargo fmt --check",
                "    strategy:",
                "      matrix:",
                "        os: [macos-latest]",
            ]),
            joined(&[
                "jobs:",
                "  lint:",
                "    runs-on: ${{ matrix.os }}",
                "    timeout-minutes: 5",
                "    steps:",
                "      - run: cargo fmt --check",
                "    strategy:",
                "      matrix:",
                "        include:",
                "          - os: \"\"",
            ]),
            String::new(),
            "{}\n".to_string(),
        ];
        for yaml in missing {
            assert_no_f1(&analyze(&yaml));
            assert_no_f1(&analyze(&crlf(&yaml)));
        }

        let flow_negative = joined(&[
            "jobs: {",
            "  lint: {",
            "    runs-on: \"${{ matrix.os }}\",",
            "    timeout-minutes: 5,",
            "    steps: [{run: cargo fmt --check}],",
            "    strategy: {matrix: {os: [ubuntu-latest]}}",
            "  }",
            "}",
        ]);
        assert_no_f1(&analyze(&flow_negative));
    }

    #[test]
    fn matrix_reference_inside_text_couples_that_axis() {
        let inside = joined(&[
            "jobs:",                                        // 1
            "  fmt:",                                       // 2
            "    runs-on: ${{ format('{0}', matrix.os) }}", // 3
            "    timeout-minutes: 5",                       // 4
            "    steps:",                                   // 5
            "      - run: cargo fmt --check",               // 6
            "    strategy:",                                // 7
            "      matrix:",                                // 8
            "        os: [ubuntu-latest, macos-14]",        // 9
        ]);
        assert_f1(&analyze(&inside), 3, &[&axis_msg("macOS", 3, "os", 9)]);
        assert_f1(
            &analyze(&crlf(&inside)),
            3,
            &[&axis_msg("macOS", 3, "os", 9)],
        );

        let both = joined(&[
            "jobs:",                                                 // 1
            "  lint:",                                               // 2
            "    runs-on: pre-${{ matrix.os }}-${{ matrix.image }}", // 3
            "    timeout-minutes: 5",                                // 4
            "    steps:",                                            // 5
            "      - run: cargo fmt --check",                        // 6
            "    strategy:",                                         // 7
            "      matrix:",                                         // 8
            "        os: [ubuntu-latest]",                           // 9
            "        image: [macos-latest, windows-latest]",         // 10
        ]);
        let windows = axis_msg("Windows", 3, "image", 10);
        let macos = axis_msg("macOS", 3, "image", 10);
        assert_f1(&analyze(&both), 3, &[&windows, &macos]);

        let bracket = joined(&[
            "jobs:",                            // 1
            "  eslint:",                        // 2
            "    runs-on: ${{ matrix['os'] }}", // 3
            "    timeout-minutes: 5",           // 4
            "    steps:",                       // 5
            "      - run: npx eslint .",        // 6
            "    strategy:",                    // 7
            "      matrix:",                    // 8
            "        os: [windows-latest]",     // 9
        ]);
        assert_f1(&analyze(&bracket), 3, &[&axis_msg("Windows", 3, "os", 9)]);

        let spaced = joined(&[
            "jobs:",                                // 1
            "  lint:",                              // 2
            "    name: Clippy",                     // 3
            "    runs-on: ${{ matrix[ \"OS\" ] }}", // 4
            "    timeout-minutes: 5",               // 5
            "    steps:",                           // 6
            "      - run: echo hi",                 // 7
            "    strategy:",                        // 8
            "      matrix:",                        // 9
            "        OS: [MacOS]",                  // 10
        ]);
        assert_f1(&analyze(&spaced), 4, &[&axis_msg("macOS", 4, "OS", 10)]);

        let extra = joined(&[
            "jobs:",                          // 1
            "  lint:",                        // 2
            "    runs-on:",                   // 3
            "      - ubuntu-latest",          // 4
            "      - ${{ matrix.extra }}",    // 5
            "    timeout-minutes: 5",         // 6
            "    steps:",                     // 7
            "      - run: cargo fmt --check", // 8
            "    strategy:",                  // 9
            "      matrix:",                  // 10
            "        extra: [windows]",       // 11
        ]);
        assert_f1(&analyze(&extra), 3, &[&axis_msg("Windows", 3, "extra", 11)]);

        let hosted_list = joined(&[
            "jobs:",                                      // 1
            "  lint:",                                    // 2
            "    runs-on:",                               // 3
            "      - ${{ matrix.os }}",                   // 4
            "      - self-hosted",                        // 5
            "    timeout-minutes: 5",                     // 6
            "    steps:",                                 // 7
            "      - run: cargo fmt --check",             // 8
            "    strategy:",                              // 9
            "      matrix:",                              // 10
            "        os: [macos-latest, windows-latest]", // 11
        ]);
        assert_no_f1(&analyze(&hosted_list));
        assert_no_f1(&analyze(&crlf(&hosted_list)));

        let not_matrix = joined(&[
            "jobs:",                            // 1
            "  lint:",                          // 2
            "    runs-on: ${{ notmatrix.os }}", // 3
            "    timeout-minutes: 5",           // 4
            "    steps:",                       // 5
            "      - run: cargo fmt --check",   // 6
            "    strategy:",                    // 7
            "      matrix:",                    // 8
            "        os: [macos-latest]",       // 9
        ]);
        assert_no_f1(&analyze(&not_matrix));

        let flow = joined(&[
            "jobs: {",                                                     // 1
            "  lint: {",                                                   // 2
            "    runs-on: \"runner-${{ matrix.os }}\",",                   // 3
            "    timeout-minutes: 5,",                                     // 4
            "    steps: [{run: cargo fmt --check}],",                      // 5
            "    strategy: {matrix: {os: [ubuntu-latest, MacOS-latest]}}", // 6
            "  }",                                                         // 7
            "}",                                                           // 8
        ]);
        assert_f1(&analyze(&flow), 3, &[&axis_msg("macOS", 3, "os", 6)]);

        let flow_negative = joined(&[
            "jobs: {",
            "  lint: {",
            "    runs-on: \"${{ matrix.node }}\",",
            "    timeout-minutes: 5,",
            "    steps: [{run: cargo fmt --check}],",
            "    strategy: {matrix: {os: [macos-latest], node: ['14']}}",
            "  }",
            "}",
        ]);
        assert_no_f1(&analyze(&flow_negative));
        assert_no_f1(&analyze(&crlf(&flow_negative)));
    }

    #[test]
    fn self_hosted_label_set_is_not_paid_macos_or_windows() {
        let negative = [
            "jobs:\n  lint:\n    runs-on: [self-hosted, macos]\n    timeout-minutes: 5\n    steps:\n      - run: cargo fmt --check\n",
            "jobs:\n  lint:\n    runs-on: [macos, self-hosted]\n    timeout-minutes: 5\n    steps:\n      - run: cargo fmt --check\n",
            "jobs:\n  lint:\n    runs-on: [Self-Hosted, MacOS]\n    timeout-minutes: 5\n    steps:\n      - run: cargo fmt --check\n",
            "jobs:\n  prettier:\n    runs-on: [WINDOWS, SELF-HOSTED]\n    timeout-minutes: 5\n    steps:\n      - run: npx prettier --check .\n",
            "jobs:\n  lint:\n    runs-on: [windows-latest, Self-Hosted]\n    timeout-minutes: 5\n    steps:\n      - run: cargo fmt --check\n",
            "jobs:\n  lint:\n    runs-on:\n      - self-hosted\n      - windows\n    timeout-minutes: 5\n    steps:\n      - run: cargo fmt --check\n",
            "jobs:\n  lint:\n    runs-on:\n      group: large\n      labels:\n        - windows\n        - self-hosted\n    timeout-minutes: 5\n    steps:\n      - run: cargo fmt --check\n",
            "jobs:\n  lint:\n    runs-on: {group: large, labels: [self-hosted, macos]}\n    timeout-minutes: 5\n    steps:\n      - run: cargo fmt --check\n",
            "jobs:\n  lint:\n    runs-on: {group: macos}\n    timeout-minutes: 5\n    steps:\n      - run: cargo fmt --check\n",
            "jobs:\n  lint:\n    runs-on: []\n    timeout-minutes: 5\n    steps:\n      - run: cargo fmt --check\n",
            "jobs:\n  lint:\n    runs-on: \"\"\n    timeout-minutes: 5\n    steps:\n      - run: cargo fmt --check\n",
            "jobs:\n  lint:\n    timeout-minutes: 5\n    steps:\n      - run: cargo fmt --check\n",
            "jobs: {lint: {runs-on: [self-hosted, macos], timeout-minutes: 5, steps: [{run: cargo fmt --check}]}}\n",
            "jobs: {lint: {runs-on: {group: large, labels: [macos, Self-Hosted]}, timeout-minutes: 5, steps: [{run: cargo fmt --check}]}}\n",
        ];
        for yaml in negative {
            assert_no_f1(&analyze(yaml));
            assert_no_f1(&analyze(&crlf(yaml)));
        }

        let paid = joined(&[
            "jobs:",                          // 1
            "  lint:",                        // 2
            "    runs-on: [MacOS-14]",        // 3
            "    timeout-minutes: 5",         // 4
            "    steps:",                     // 5
            "      - run: cargo fmt --check", // 6
        ]);
        assert_f1(&analyze(&paid), 3, &["lint on macOS"]);
        assert_f1(&analyze(&crlf(&paid)), 3, &["lint on macOS"]);

        let both = joined(&[
            "jobs:",                                       // 1
            "  lint:",                                     // 2
            "    runs-on: [macos-latest, windows-latest]", // 3
            "    timeout-minutes: 5",                      // 4
            "    steps:",                                  // 5
            "      - run: cargo fmt --check",              // 6
        ]);
        assert_f1(&analyze(&both), 3, &["lint on Windows", "lint on macOS"]);

        let exact = joined(&[
            "jobs:",                  // 1
            "  eslint:",              // 2
            "    runs-on: Windows",   // 3
            "    timeout-minutes: 5", // 4
            "    steps:",             // 5
            "      - run: echo hi",   // 6
        ]);
        assert_f1(&analyze(&exact), 3, &["lint on Windows"]);

        let map = joined(&[
            "jobs:",                          // 1
            "  lint:",                        // 2
            "    runs-on:",                   // 3
            "      group: large",             // 4
            "      labels: [macos-latest]",   // 5
            "    timeout-minutes: 5",         // 6
            "    steps:",                     // 7
            "      - run: cargo fmt --check", // 8
        ]);
        assert_f1(&analyze(&map), 3, &["lint on macOS"]);

        let flow = "jobs: {lint: {runs-on: {group: large, labels: [macos-latest, windows]}, timeout-minutes: 5, steps: [{run: cargo fmt --check}]}}\n";
        assert_f1(&analyze(flow), 1, &["lint on Windows", "lint on macOS"]);
        assert_f1(
            &analyze(&crlf(flow)),
            1,
            &["lint on Windows", "lint on macOS"],
        );
    }

    fn pr_with_cancel(value: &str) -> String {
        format!("on: pull_request\nconcurrency:\n  group: ci\n  cancel-in-progress: {value}\njobs:\n  test:\n    runs-on: ubuntu-latest\n    timeout-minutes: 1\n    steps:\n      - run: cargo test\n")
    }

    fn job_with_cancel(value: &str) -> String {
        format!("on: pull_request\njobs:\n  test:\n    runs-on: ubuntu-latest\n    timeout-minutes: 1\n    concurrency:\n      group: ci\n      cancel-in-progress: {value}\n    steps:\n      - run: cargo test\n")
    }

    #[test]
    fn cancel_expression_counts_as_true_for_f3() {
        let cases = [
            (r"${{ github.event_name == 'pull_request' }}", true),
            (r"${{github.event_name=='pull_request'}}", true),
            (r#"${{ github.event_name == "pull_request" }}"#, true),
            (r"${{  github.event_name   ==   'pull_request'  }}", true),
            (r"${{ GitHub.Event_Name == 'PULL_REQUEST' }}", true),
            (r"${{ github.event_name == 'pull_request_target' }}", true),
            (r#"${{ github.event_name == "Pull_Request_Target" }}"#, true),
            (
                r"${{ github.event_name == 'pull_request' || github.event_name == 'push' }}",
                true,
            ),
            (r"${{ false || github.event_name == 'pull_request' }}", true),
            (r"${{ (github.event_name == 'pull_request') }}", true),
            ("(github.event_name == 'pull_request')", true),
            ("true", true),
            ("TRUE", true),
            ("True", true),
            (r"${{ true }}", true),
            ("false", false),
            ("FALSE", false),
            (r"${{ false }}", false),
            (r"${{ github.event_name == 'push' }}", false),
            (r"${{ github.event_name != 'pull_request' }}", false),
            (r"${{ github.event_name == 'pull_request_review' }}", false),
            (
                r"${{ github.event_name == 'pull_request' && false }}",
                false,
            ),
            ("yes", false),
            ("", false),
            ("~", false),
            ("null", false),
        ];
        for (value, enables) in cases {
            for yaml in [
                pr_with_cancel(value),
                crlf(&pr_with_cancel(value)),
                job_with_cancel(value),
                crlf(&job_with_cancel(value)),
            ] {
                let found = analyze(&yaml);
                if enables {
                    assert_no_f3(&found);
                } else {
                    assert_f3(&found, 1, F3_PR);
                }
            }
        }

        let folded = "on: pull_request\nconcurrency:\n  cancel-in-progress: >-\n    ${{ github.event_name == 'pull_request' }}\njobs:\n  test:\n    runs-on: ubuntu-latest\n    timeout-minutes: 1\n";
        assert_no_f3(&analyze(folded));
        assert_no_f3(&analyze(&crlf(folded)));

        let literal_block = "on: pull_request\nconcurrency:\n  cancel-in-progress: |\n    ${{ github.event_name == \"pull_request\" }}\njobs:\n  test:\n    timeout-minutes: 1\n    runs-on: ubuntu-latest\n";
        assert_no_f3(&analyze(literal_block));

        let flow = "on: {pull_request: {}}\nconcurrency: {group: ci, cancel-in-progress: \"${{ github.event_name == 'pull_request' }}\"}\njobs: {test: {runs-on: ubuntu-latest, timeout-minutes: 1, steps: [{run: cargo test}]}}\n";
        assert_no_f3(&analyze(flow));
        assert_no_f3(&analyze(&crlf(flow)));

        let flow_job = "on: {pull_request: {}}\njobs: {test: {concurrency: {cancel-in-progress: \"${{  github.event_name == \\\"pull_request\\\"  }}\"}, runs-on: ubuntu-latest, timeout-minutes: 1}}\n";
        assert_no_f3(&analyze(flow_job));

        let flow_false = "on: [pull_request]\njobs: {test: {concurrency: {cancel-in-progress: false}, timeout-minutes: 1, runs-on: ubuntu-latest}}\n";
        assert_f3(&analyze(flow_false), 1, F3_PR);
        assert_f3(&analyze(&crlf(flow_false)), 1, F3_PR);

        let missing_key = "on: pull_request\nconcurrency:\n  group: ci\njobs:\n  test:\n    timeout-minutes: 1\n    runs-on: ubuntu-latest\n";
        assert_f3(&analyze(missing_key), 1, F3_PR);

        let empty_map = "on: pull_request\nconcurrency: {}\njobs:\n  test:\n    timeout-minutes: 1\n    runs-on: ubuntu-latest\n";
        assert_f3(&analyze(empty_map), 1, F3_PR);
        assert_f3(&analyze(&crlf(empty_map)), 1, F3_PR);

        let empty_value = "on: pull_request\nconcurrency:\njobs:\n  test:\n    timeout-minutes: 1\n    runs-on: ubuntu-latest\n";
        assert_f3(&analyze(empty_value), 1, F3_PR);

        let group_only = "on: pull_request\nconcurrency: ci\njobs:\n  test:\n    timeout-minutes: 1\n    runs-on: ubuntu-latest\n";
        assert_f3(&analyze(group_only), 1, F3_PR);

        let job_group_only = "on: pull_request\njobs:\n  test:\n    concurrency:\n      group: ci\n    timeout-minutes: 1\n    runs-on: ubuntu-latest\n";
        assert_f3(&analyze(job_group_only), 1, F3_PR);

        let last_wins = "on: pull_request\nconcurrency:\n  cancel-in-progress: false\n  cancel-in-progress: ${{ github.event_name == 'pull_request' }}\njobs:\n  test:\n    timeout-minutes: 1\n    runs-on: ubuntu-latest\n";
        let found = analyze(last_wins);
        assert_no_f3(&found);
        assert_eq!(of_kind(&found, FindingKind::DuplicateKey).len(), 1);

        let last_false = "on: pull_request\nconcurrency:\n  cancel-in-progress: ${{ github.event_name == 'pull_request' }}\n  cancel-in-progress: false\njobs:\n  test:\n    timeout-minutes: 1\n    runs-on: ubuntu-latest\n";
        let found = analyze(last_false);
        assert_f3(&found, 1, F3_PR);
        assert_eq!(of_kind(&found, FindingKind::DuplicateKey).len(), 1);

        let job_last_true = "on: pull_request\njobs:\n  test:\n    concurrency:\n      cancel-in-progress: false\n      cancel-in-progress: true\n    timeout-minutes: 1\n    runs-on: ubuntu-latest\n";
        assert_no_f3(&analyze(job_last_true));
        assert_no_f3(&analyze(&crlf(job_last_true)));
    }

    #[test]
    fn job_level_concurrency_counts_for_f3() {
        let one_job = joined(&[
            "on: pull_request",               // 1
            "jobs:",                          // 2
            "  test:",                        // 3
            "    runs-on: ubuntu-latest",     // 4
            "    timeout-minutes: 1",         // 5
            "    concurrency:",               // 6
            "      group: ci",                // 7
            "      cancel-in-progress: true", // 8
            "    steps:",                     // 9
            "      - run: cargo test",        // 10
        ]);
        assert_no_f3(&analyze(&one_job));
        assert_no_f3(&analyze(&crlf(&one_job)));

        let concurrency_first = joined(&[
            "on: pull_request",
            "jobs:",
            "  test:",
            "    concurrency:",
            "      cancel-in-progress: TRUE",
            "    runs-on: ubuntu-latest",
            "    timeout-minutes: 1",
        ]);
        assert_no_f3(&analyze(&concurrency_first));

        let one_of_two = joined(&[
            "on: pull_request",
            "jobs:",
            "  lint:",
            "    runs-on: ubuntu-latest",
            "    timeout-minutes: 1",
            "    concurrency:",
            "      cancel-in-progress: true",
            "    steps:",
            "      - run: cargo test",
            "  build:",
            "    runs-on: ubuntu-latest",
            "    timeout-minutes: 1",
            "    steps:",
            "      - run: cargo test",
        ]);
        assert_no_f3(&analyze(&one_of_two));
        assert_no_f3(&analyze(&crlf(&one_of_two)));

        let workflow_false_job_true = "on: pull_request\nconcurrency:\n  cancel-in-progress: false\njobs:\n  test:\n    concurrency:\n      cancel-in-progress: ${{ github.event_name == 'pull_request' }}\n    timeout-minutes: 1\n    runs-on: ubuntu-latest\n";
        assert_no_f3(&analyze(workflow_false_job_true));

        let workflow_true_job_false = "on: pull_request\nconcurrency:\n  cancel-in-progress: true\njobs:\n  test:\n    concurrency:\n      cancel-in-progress: false\n    timeout-minutes: 1\n    runs-on: ubuntu-latest\n";
        assert_no_f3(&analyze(workflow_true_job_false));
        assert_no_f3(&analyze(&crlf(workflow_true_job_false)));

        let anchored = "on: pull_request\ndefaults: &job\n  concurrency:\n    cancel-in-progress: true\n  timeout-minutes: 5\n  runs-on: ubuntu-latest\njobs:\n  test:\n    <<: *job\n    steps:\n      - run: cargo test\n";
        assert_no_f3(&analyze(anchored));

        let flow = "on: {pull_request: {}}\njobs: {test: {concurrency: {group: ci, cancel-in-progress: true}, runs-on: ubuntu-latest, timeout-minutes: 1, steps: [{run: cargo test}]}}\n";
        assert_no_f3(&analyze(flow));
        assert_no_f3(&analyze(&crlf(flow)));

        let job_false = joined(&[
            "on: pull_request",
            "jobs:",
            "  test:",
            "    concurrency:",
            "      cancel-in-progress: false",
            "    runs-on: ubuntu-latest",
            "    timeout-minutes: 1",
        ]);
        assert_f3(&analyze(&job_false), 1, F3_PR);
        assert_f3(&analyze(&crlf(&job_false)), 1, F3_PR);

        let neither = joined(&[
            "on: pull_request",
            "jobs:",
            "  lint:",
            "    runs-on: ubuntu-latest",
            "    timeout-minutes: 1",
            "  build:",
            "    runs-on: ubuntu-latest",
            "    timeout-minutes: 1",
        ]);
        assert_f3(&analyze(&neither), 1, F3_PR);

        let no_jobs = "on: pull_request\n";
        assert_f3(&analyze(no_jobs), 1, F3_PR);
        assert_f3(&analyze(&crlf(no_jobs)), 1, F3_PR);

        let empty_jobs = "on: pull_request\njobs: {}\n";
        assert_f3(&analyze(empty_jobs), 1, F3_PR);

        let flow_false = "on: [pull_request]\njobs: {lint: {concurrency: {cancel-in-progress: false}, timeout-minutes: 1}, test: {runs-on: ubuntu-latest, timeout-minutes: 1}}\n";
        assert_f3(&analyze(flow_false), 1, F3_PR);
    }

    #[test]
    fn pull_request_target_triggers_f3() {
        let positive = [
            "on: pull_request_target\njobs:\n  test:\n    runs-on: ubuntu-latest\n    timeout-minutes: 1\n",
            "on: \"pull_request_target\"\njobs:\n  test:\n    timeout-minutes: 1\n    runs-on: ubuntu-latest\n",
            "on: 'PULL_REQUEST_TARGET'\njobs:\n  test:\n    timeout-minutes: 1\n    runs-on: ubuntu-latest\n",
            "on: [push, pull_request_target]\njobs:\n  test:\n    timeout-minutes: 1\n    runs-on: ubuntu-latest\n",
            "on: [pull_request_target, push]\njobs:\n  test:\n    timeout-minutes: 1\n    runs-on: ubuntu-latest\n",
            "on: [Push, Pull_Request_Target]\njobs:\n  test:\n    timeout-minutes: 1\n    runs-on: ubuntu-latest\n",
            "on:\n  pull_request_target:\n    types: [opened]\njobs:\n  test:\n    timeout-minutes: 1\n    runs-on: ubuntu-latest\n",
            "on: {pull_request_target: {}}\njobs: {test: {runs-on: ubuntu-latest, timeout-minutes: 1}}\n",
            "on: PULL_REQUEST_TARGET\n",
        ];
        for yaml in positive {
            assert_f3(&analyze(yaml), 1, F3_TARGET);
            assert_f3(&analyze(&crlf(yaml)), 1, F3_TARGET);
        }

        let both = "on:\n  pull_request_target:\n  pull_request:\njobs:\n  test:\n    timeout-minutes: 1\n    runs-on: ubuntu-latest\n";
        assert_f3(&analyze(both), 1, F3_PR);
        assert_f3(&analyze(&crlf(both)), 1, F3_PR);

        let both_flow = "on: [pull_request, pull_request_target]\n";
        assert_f3(&analyze(both_flow), 1, F3_PR);

        let both_order = "on: {pull_request_target: {}, pull_request: {}}\n";
        assert_f3(&analyze(both_order), 1, F3_PR);

        let cancelled = "on: pull_request_target\nconcurrency:\n  group: ci\n  cancel-in-progress: true\njobs:\n  test:\n    runs-on: ubuntu-latest\n    timeout-minutes: 1\n";
        assert_no_f3(&analyze(cancelled));
        assert_no_f3(&analyze(&crlf(cancelled)));

        let expression = "on: pull_request_target\nconcurrency:\n  cancel-in-progress: ${{ github.event_name == 'pull_request' }}\njobs:\n  test:\n    timeout-minutes: 1\n    runs-on: ubuntu-latest\n";
        assert_no_f3(&analyze(expression));

        let target_expression = "on: PULL_REQUEST_TARGET\nconcurrency:\n  cancel-in-progress: ${{  github.event_name == \"pull_request_target\"  }}\njobs:\n  test:\n    timeout-minutes: 1\n    runs-on: ubuntu-latest\n";
        assert_no_f3(&analyze(target_expression));
        assert_no_f3(&analyze(&crlf(target_expression)));

        let job_level = "on: {pull_request_target: {types: [opened]}}\njobs: {test: {concurrency: {cancel-in-progress: true}, runs-on: ubuntu-latest, timeout-minutes: 1}}\n";
        assert_no_f3(&analyze(job_level));
        assert_no_f3(&analyze(&crlf(job_level)));

        let negative = [
            "on: push\njobs:\n  test:\n    timeout-minutes: 1\n    runs-on: ubuntu-latest\n",
            "on: pull_request_review\n",
            "on: workflow_dispatch\n",
            "on: [push, workflow_dispatch]\n",
            "on:\n",
            "on: {}\n",
            "on: []\n",
            "on: \"\"\n",
            "jobs:\n  test:\n    runs-on: ubuntu-latest\n    timeout-minutes: 1\n",
            "",
            "{}\n",
            "on: {push: null, workflow_dispatch: {}}\nconcurrency: {cancel-in-progress: false}\n",
        ];
        for yaml in negative {
            assert_no_f3(&analyze(yaml));
            assert_no_f3(&analyze(&crlf(yaml)));
        }

        let flow_positive = "on: {pull_request_target: {types: [opened, synchronize]}}\njobs: {lint: {runs-on: ubuntu-latest, timeout-minutes: 5, steps: [{run: cargo test}]}}\n";
        assert_f3(&analyze(flow_positive), 1, F3_TARGET);

        let flow_cancelled = "on: [pull_request_target]\nconcurrency: {cancel-in-progress: \"${{ github.event_name == 'pull_request_target' }}\"}\njobs: {test: {runs-on: ubuntu-latest, timeout-minutes: 1}}\n";
        assert_no_f3(&analyze(flow_cancelled));
        assert_no_f3(&analyze(&crlf(flow_cancelled)));
    }
}
