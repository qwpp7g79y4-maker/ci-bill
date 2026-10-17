use std::path::Path;

use crate::workflow::{Job, RunsOn, Workflow};

/// Why a workflow or job was flagged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FindingKind {
    /// Lint-class job on a macOS or Windows runner.
    LintOnRunner,
    /// Job has no `timeout-minutes`.
    MissingTimeout,
    /// `pull_request` without `concurrency.cancel-in-progress: true`.
    PullRequestConcurrency,
}

/// One cause, pointing at a file line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub path: std::path::PathBuf,
    pub line: usize,
    pub kind: FindingKind,
    pub message: String,
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
    found.sort_by(|left, right| {
        (&left.path, left.line, left.kind, left.message.as_str()).cmp(&(
            &right.path,
            right.line,
            right.kind,
            right.message.as_str(),
        ))
    });
    found
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
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::{scan_dir, scan_yaml};
    use std::path::Path;

    fn analyze(yaml: &str) -> Vec<Finding> {
        findings(&[scan_yaml(Path::new("fixture.yml"), yaml)])
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
}
