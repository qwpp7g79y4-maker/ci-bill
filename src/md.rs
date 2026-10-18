//! One-page Markdown teardown.
//!
//! Period, repositories, the Actions net total, the five costliest jobs,
//! every finding with its savings, and a unified diff for the
//! highest-savings finding this tool knows how to fix. A section with
//! nothing to show (no jobs, no findings, no fixable finding) is left out.

use std::fmt::Write as _;
use std::path::Path;

use crate::api::ApiData;
use crate::findings::Finding;
use crate::fix_diff;
use crate::rank::{Bill, WorkflowTotal};
use crate::reconcile::{self, JobCost};
use crate::report::{period_line, repositories_line};
use crate::workflow::Workflow;

const TOP_JOBS: usize = 5;

/// Render the Markdown teardown.
///
/// Job and finding tables come from the full ranked workflow list; `--top`
/// only shortens the text and JSON tables, not this summary. `workflows_dir`
/// is passed through to the diff so a lint finding raised through a local
/// composite action still resolves to the caller's `runs-on` line.
pub fn render(
    workflows: &[WorkflowTotal],
    bill: &Bill,
    findings: Option<&[Finding]>,
    api: Option<&ApiData>,
    scanned: &[Workflow],
    workflows_dir: Option<&Path>,
) -> String {
    let recon = reconcile::reconcile(workflows, api, scanned);
    let mut out = String::new();
    let _ = writeln!(out, "# ci-bill teardown");
    let _ = writeln!(out);
    let _ = writeln!(out, "{}", period_line(bill));
    let _ = writeln!(out, "{}", repositories_line(bill));
    let _ = writeln!(out);
    let _ = writeln!(out, "Total net: {} USD", bill.total_net.format_cents());

    write_top_jobs(&mut out, &recon);
    write_findings(&mut out, findings, workflows_dir);
    out
}

fn write_top_jobs(out: &mut String, recon: &reconcile::Reconciliation) {
    let jobs = top_jobs(recon);
    if jobs.is_empty() {
        return;
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "## Top jobs");
    let _ = writeln!(out);
    let _ = writeln!(out, "| Job | File:Line | Amount (USD) |");
    let _ = writeln!(out, "| --- | --- | --- |");
    for job in &jobs {
        let _ = writeln!(
            out,
            "| {} | {} | {} |",
            escape_cell(job_name(job)),
            escape_cell(&job_location(job)),
            job.amount.micros.format_cents(),
        );
    }
}

fn write_findings(out: &mut String, findings: Option<&[Finding]>, workflows_dir: Option<&Path>) {
    let Some(found) = findings.filter(|list| !list.is_empty()) else {
        return;
    };
    let _ = writeln!(out);
    let _ = writeln!(out, "## Findings");
    let _ = writeln!(out);
    let _ = writeln!(out, "| Finding | File:Line | Message | Savings (USD) |");
    let _ = writeln!(out, "| --- | --- | --- | --- |");
    for finding in found {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            finding.kind.code(),
            escape_cell(&format!("{}:{}", finding.path.display(), finding.line)),
            escape_cell(&finding.message),
            finding.savings.format_cents(),
        );
    }
    if let Some(diff) = fix_diff::render(found, workflows_dir) {
        let _ = writeln!(out);
        let _ = writeln!(out, "## Suggested fix");
        let _ = writeln!(out);
        let _ = writeln!(out, "```diff");
        out.push_str(&diff);
        if !diff.ends_with('\n') {
            out.push('\n');
        }
        let _ = writeln!(out, "```");
    }
}

/// The costliest jobs across every reconciled workflow, highest amount
/// first. Ties break on file, then line, then name, so the order never
/// depends on hash or scan order.
fn top_jobs(recon: &reconcile::Reconciliation) -> Vec<&JobCost> {
    let mut jobs: Vec<&JobCost> = recon.workflows.iter().flat_map(|cost| &cost.jobs).collect();
    jobs.sort_by(|left, right| {
        right
            .amount
            .micros
            .cmp(&left.amount.micros)
            .then_with(|| left.file.cmp(&right.file))
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.name.cmp(&right.name))
    });
    jobs.truncate(TOP_JOBS);
    jobs
}

fn job_name(job: &JobCost) -> &str {
    if job.name.is_empty() {
        "job"
    } else {
        job.name.as_str()
    }
}

fn job_location(job: &JobCost) -> String {
    if !job.file.is_empty() {
        if let Some(line) = job.line {
            return format!("{}:{line}", job.file);
        }
    }
    match job.job_id {
        Some(id) => format!("job {id}"),
        None => "-".to_string(),
    }
}

/// Escape a Markdown table cell: a `|` would otherwise end the cell early,
/// and a newline would otherwise end the row early. Both can appear here
/// because messages and job names are built from scanned YAML content.
fn escape_cell(text: &str) -> String {
    text.replace('|', "\\|").replace(['\n', '\r'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::findings::FindingKind;
    use crate::money::Micros;
    use crate::rank::SkuTotal;

    fn usd(text: &str) -> Micros {
        Micros::parse(text).unwrap()
    }

    fn bill(net: &str) -> Bill {
        Bill {
            total_net: usd(net),
            total_minutes: Micros::ZERO,
            other_products_net: Micros::ZERO,
            period_from: None,
            period_to: None,
            repositories: Vec::new(),
        }
    }

    fn workflow(path: &str, net: &str, minutes: &str, sku: &str) -> WorkflowTotal {
        let net = usd(net);
        let minutes = usd(minutes);
        WorkflowTotal {
            repository: String::new(),
            workflow_path: path.to_string(),
            net,
            gross: net,
            discount: Micros::ZERO,
            minutes,
            skus: vec![SkuTotal {
                sku: sku.to_string(),
                net,
                gross: net,
                discount: Micros::ZERO,
                minutes,
            }],
        }
    }

    fn finding(
        path: &str,
        line: usize,
        kind: FindingKind,
        message: &str,
        savings: &str,
    ) -> Finding {
        let mut item = Finding::record(path, line, kind, message);
        item.savings = usd(savings);
        item
    }

    #[test]
    fn page_has_period_repositories_and_total() {
        let mut totals = bill("10");
        totals.period_from = Some("2026-09-01".to_string());
        totals.period_to = Some("2026-09-30".to_string());
        totals.repositories = vec!["example-org/app".to_string()];
        let text = render(&[], &totals, None, None, &[], None);
        assert!(text.starts_with("# ci-bill teardown\n"), "{text}");
        assert!(
            text.contains("Period: 2026-09-01 .. 2026-09-30\n"),
            "{text}"
        );
        assert!(text.contains("Repositories: example-org/app\n"), "{text}");
        assert!(text.contains("Total net: 10.00 USD\n"), "{text}");
    }

    #[test]
    fn missing_period_and_repositories_still_render() {
        let text = render(&[], &bill("0"), None, None, &[], None);
        assert!(text.contains("Period: (none)\n"), "{text}");
        assert!(text.contains("Repositories: (none)\n"), "{text}");
    }

    #[test]
    fn no_workflows_or_findings_omits_their_sections() {
        let text = render(&[], &bill("0"), None, None, &[], None);
        assert!(!text.contains("## Top jobs"), "{text}");
        assert!(!text.contains("## Findings"), "{text}");
        assert!(!text.contains("## Suggested fix"), "{text}");
    }

    fn scanned_job(file: &str, net: &str) -> Workflow {
        let path = std::path::PathBuf::from(format!(".github/workflows/{file}"));
        let text = format!(
            "name: {file}\non: push\njobs:\n  build-{net}:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo hi\n"
        );
        crate::workflow::scan_yaml(&path, &text).expect("scan fixture yaml")
    }

    #[test]
    fn top_jobs_table_lists_the_five_costliest_jobs() {
        let nets = ["1", "2", "3", "4", "5", "6"];
        let workflows: Vec<WorkflowTotal> = nets
            .iter()
            .map(|net| {
                workflow(
                    &format!(".github/workflows/{net}.yml"),
                    net,
                    "10",
                    "actions_linux",
                )
            })
            .collect();
        let scanned: Vec<Workflow> = nets
            .iter()
            .map(|net| scanned_job(&format!("{net}.yml"), net))
            .collect();
        let totals = bill("21");
        let text = render(&workflows, &totals, None, None, &scanned, None);
        assert!(text.contains("## Top jobs\n"), "{text}");
        assert!(
            text.contains("| build-6 | .github/workflows/6.yml:5 | 6.00 |"),
            "{text}"
        );
        assert!(
            text.contains("| build-2 | .github/workflows/2.yml:5 | 2.00 |"),
            "{text}"
        );
        // Only the five costliest jobs are listed; the cheapest (net 1) is left out.
        assert!(!text.contains("build-1 "), "{text}");
        let jobs_section = text.split("## Top jobs").nth(1).unwrap();
        let jobs_section = match jobs_section.split_once("##") {
            Some((before, _)) => before,
            None => jobs_section,
        };
        assert_eq!(jobs_section.matches("| build-").count(), 5, "{text}");
    }

    #[test]
    fn findings_table_keeps_the_savings_sorted_order() {
        let findings = vec![
            finding(
                "a.yml",
                8,
                FindingKind::LintOnRunner,
                "lint on macOS",
                "207.872",
            ),
            finding(
                "a.yml",
                7,
                FindingKind::MissingTimeout,
                "default timeout is 360 minutes",
                "22.32",
            ),
        ];
        let text = render(&[], &bill("0"), Some(&findings), None, &[], None);
        let f1 = text.find("F1").expect("f1 row");
        let f2 = text.find("F2").expect("f2 row");
        assert!(f1 < f2, "{text}");
        assert!(
            text.contains("| F1 | a.yml:8 | lint on macOS | 207.87 |"),
            "{text}"
        );
        assert!(
            text.contains("| F2 | a.yml:7 | default timeout is 360 minutes | 22.32 |"),
            "{text}"
        );
    }

    #[test]
    fn empty_findings_list_omits_the_section() {
        let text = render(&[], &bill("0"), Some(&[]), None, &[], None);
        assert!(!text.contains("## Findings"), "{text}");
    }

    #[test]
    fn pipe_and_newline_in_message_are_escaped() {
        let findings = vec![finding(
            "a.yml",
            1,
            FindingKind::InvalidUses,
            "job a|b uses c\nd",
            "0",
        )];
        let text = render(&[], &bill("0"), Some(&findings), None, &[], None);
        assert!(text.contains("job a\\|b uses c d"), "{text}");
        let row = text
            .lines()
            .find(|line| line.starts_with("| F11"))
            .expect("row");
        // 5 delimiters plus the one escaped `\|` inside the message cell.
        assert_eq!(row.matches('|').count(), 6, "{row}");
    }

    #[test]
    fn fixable_finding_adds_a_suggested_fix_diff() {
        let dir = std::env::temp_dir().join("ci-bill-md-test-fix");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join("lint.yml");
        std::fs::write(
            &path,
            "name: Lint\non:\n  push:\njobs:\n  lint:\n    runs-on: macos-latest\n    steps:\n      - run: cargo fmt --check\n",
        )
        .expect("write fixture");
        let findings = vec![finding(
            path.to_str().unwrap(),
            6,
            FindingKind::LintOnRunner,
            "lint on macOS",
            "100",
        )];
        let text = render(&[], &bill("0"), Some(&findings), None, &[], None);
        assert!(text.contains("## Suggested fix\n"), "{text}");
        assert!(text.contains("```diff\n"), "{text}");
        assert!(text.contains("+    runs-on: ubuntu-latest\n"), "{text}");
        assert!(text.trim_end().ends_with("```"), "{text}");
    }

    #[test]
    fn unfixable_findings_omit_the_suggested_fix_section() {
        let findings = vec![finding(
            "a.yml",
            1,
            FindingKind::DuplicateKey,
            "duplicate key name (line 1 and 2)",
            "0",
        )];
        let text = render(&[], &bill("0"), Some(&findings), None, &[], None);
        assert!(text.contains("## Findings"), "{text}");
        assert!(!text.contains("## Suggested fix"), "{text}");
    }
}
