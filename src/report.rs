use std::fmt::Write;
use std::path::Path;

use crate::api::{ApiData, Job, WorkflowRun, IN_PROGRESS, UNKNOWN_WORKFLOW};
use crate::findings::Finding;
use crate::rank::WorkflowTotal;

/// Text report. Money is rounded to cents here. Totals are sums of raw amounts.
///
/// `total_net` and `total_minutes` are the whole bill. `workflows` may already
/// be limited by `--top`; each share is still that workflow's part of `total_net`.
/// Findings attach to a workflow when the YAML path ends in
/// `.github/workflows/<file>` and that suffix matches `workflow_path`.
/// Anything left over is listed under "Workflows without usage".
pub fn render(
    workflows: &[WorkflowTotal],
    total_net: f64,
    total_minutes: f64,
    findings: Option<&[Finding]>,
) -> String {
    render_report(workflows, total_net, total_minutes, findings, None)
}

/// Text report plus saved Actions runs and jobs.
///
/// Finished jobs are listed under "Jobs". A job with no `completed_at` is
/// listed under "In progress" and is not included with the finished jobs.
pub fn render_with_runs(
    workflows: &[WorkflowTotal],
    total_net: f64,
    total_minutes: f64,
    findings: Option<&[Finding]>,
    api: &ApiData,
) -> String {
    render_report(workflows, total_net, total_minutes, findings, Some(api))
}

fn render_report(
    workflows: &[WorkflowTotal],
    total_net: f64,
    total_minutes: f64,
    findings: Option<&[Finding]>,
    api: Option<&ApiData>,
) -> String {
    let attached = attach(workflows, findings);
    let mut out = String::new();
    let _ = writeln!(out, "USD (from net_amount)");
    let _ = writeln!(out);
    let _ = writeln!(out, "Total net: {}", format_amount(total_net));
    let _ = writeln!(out, "Total minutes: {}", format_number(total_minutes));
    let _ = writeln!(out);

    let blocks: Vec<Vec<Row>> = workflows
        .iter()
        .map(|workflow| workflow_rows(workflow, total_net))
        .collect();
    let widths = widths(&blocks);
    emit_row(
        &mut out,
        &Row {
            name: "Workflow".to_string(),
            net: "Net".to_string(),
            minutes: "Minutes".to_string(),
            share: "Share".to_string(),
        },
        widths,
    );
    for (rows, found) in blocks.iter().zip(&attached.by_workflow) {
        for row in rows {
            emit_row(&mut out, row, widths);
        }
        for finding in found {
            let _ = writeln!(out, "  {}", finding_text(finding));
        }
    }

    if !attached.unattached.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "Workflows without usage");
        for finding in &attached.unattached {
            let _ = writeln!(out, "{}", finding_text(finding));
        }
    }
    if let Some(api) = api {
        append_api(&mut out, api);
    }
    out
}

/// One JSON object. Numbers are the raw totals, not the rounded text amounts.
pub fn render_json(
    workflows: &[WorkflowTotal],
    total_net: f64,
    total_minutes: f64,
    findings: Option<&[Finding]>,
    budget: Option<f64>,
) -> String {
    render_json_report(workflows, total_net, total_minutes, findings, budget, None)
}

/// JSON report plus `workflow_runs` and `in_progress`.
///
/// Finished jobs sit on their run. An in-progress job is only in
/// `in_progress`, with `counts` false and message "in progress".
pub fn render_json_with_runs(
    workflows: &[WorkflowTotal],
    total_net: f64,
    total_minutes: f64,
    findings: Option<&[Finding]>,
    budget: Option<f64>,
    api: &ApiData,
) -> String {
    render_json_report(
        workflows,
        total_net,
        total_minutes,
        findings,
        budget,
        Some(api),
    )
}

fn render_json_report(
    workflows: &[WorkflowTotal],
    total_net: f64,
    total_minutes: f64,
    findings: Option<&[Finding]>,
    budget: Option<f64>,
    api: Option<&ApiData>,
) -> String {
    let attached = attach(workflows, findings);
    let workflow_json: Vec<serde_json::Value> = workflows
        .iter()
        .zip(&attached.by_workflow)
        .map(|(workflow, found)| workflow_json(workflow, total_net, found))
        .collect();
    let unattached: Vec<serde_json::Value> = attached
        .unattached
        .iter()
        .map(|finding| finding_json(finding))
        .collect();
    let mut value = serde_json::json!({
        "schema_version": 1,
        "currency": "USD",
        "total_net": finite(total_net),
        "total_minutes": finite(total_minutes),
        "workflows": workflow_json,
        "unattached_findings": unattached,
    });
    if let Some(api) = api {
        let (runs, in_progress) = api_json(api);
        value["workflow_runs"] = runs;
        value["in_progress"] = in_progress;
    }
    if let Some(amount) = budget {
        value["budget"] = serde_json::Value::from(amount);
        value["budget_exceeded"] = serde_json::Value::from(total_net > amount);
    }
    let mut text = serde_json::to_string_pretty(&value).expect("finite report json");
    text.push('\n');
    text
}

/// Status line when the net total is above the budget. Amounts use cent rounding.
pub(crate) fn budget_exceeded_line(total_net: f64, budget: f64) -> String {
    format!(
        "Budget exceeded: {} > {}",
        format_amount(total_net),
        format_amount(budget)
    )
}

struct Attached<'a> {
    by_workflow: Vec<Vec<&'a Finding>>,
    unattached: Vec<&'a Finding>,
}

fn attach<'a>(workflows: &[WorkflowTotal], findings: Option<&'a [Finding]>) -> Attached<'a> {
    let mut by_workflow = vec![Vec::new(); workflows.len()];
    let mut unattached = Vec::new();
    if let Some(findings) = findings {
        for finding in findings {
            if let Some(index) = workflows
                .iter()
                .position(|workflow| belongs_to(&workflow.workflow_path, finding))
            {
                by_workflow[index].push(finding);
            } else {
                unattached.push(finding);
            }
        }
    }
    Attached {
        by_workflow,
        unattached,
    }
}

/// `.github/workflows/<file>` taken from the YAML path.
fn workflow_key(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let workflows = path.parent()?;
    if workflows.file_name()?.to_str()? != "workflows" {
        return None;
    }
    if workflows.parent()?.file_name()?.to_str()? != ".github" {
        return None;
    }
    Some(format!(".github/workflows/{name}"))
}

fn belongs_to(workflow_path: &str, finding: &Finding) -> bool {
    let Some(key) = workflow_key(&finding.path) else {
        return false;
    };
    workflow_path == key || workflow_path.ends_with(&format!("/{key}"))
}

fn workflow_json(
    workflow: &WorkflowTotal,
    total_net: f64,
    findings: &[&Finding],
) -> serde_json::Value {
    let skus: Vec<serde_json::Value> = workflow
        .skus
        .iter()
        .map(|sku| {
            serde_json::json!({
                "sku": sku.sku,
                "net": finite(sku.net),
                "minutes": finite(sku.minutes),
            })
        })
        .collect();
    let found: Vec<serde_json::Value> = findings.iter().copied().map(finding_json).collect();
    serde_json::json!({
        "path": workflow.workflow_path,
        "net": finite(workflow.net),
        "gross": finite(workflow.gross),
        "minutes": finite(workflow.minutes),
        "share": finite(share_ratio(workflow.net, total_net)),
        "skus": skus,
        "findings": found,
    })
}

fn finding_json(finding: &Finding) -> serde_json::Value {
    serde_json::json!({
        "id": finding.kind.code(),
        "file": finding.path.display().to_string(),
        "line": finding.line,
        "message": finding.message,
    })
}

fn finding_text(finding: &Finding) -> String {
    format!(
        "{} {}:{}: {}",
        finding.kind.code(),
        finding.path.display(),
        finding.line,
        finding.message
    )
}

struct Row {
    name: String,
    net: String,
    minutes: String,
    share: String,
}

#[derive(Clone, Copy)]
struct Widths {
    name: usize,
    net: usize,
    minutes: usize,
    share: usize,
}

fn workflow_rows(workflow: &WorkflowTotal, total_net: f64) -> Vec<Row> {
    let mut rows = vec![Row {
        name: workflow.workflow_path.clone(),
        net: format_amount(workflow.net),
        minutes: format_number(workflow.minutes),
        share: format_share(workflow.net, total_net),
    }];
    for sku in &workflow.skus {
        rows.push(Row {
            name: format!("  {}", sku.sku),
            net: format_amount(sku.net),
            minutes: format_number(sku.minutes),
            share: String::new(),
        });
    }
    rows
}

fn widths(blocks: &[Vec<Row>]) -> Widths {
    let mut widths = Widths {
        name: "Workflow".len(),
        net: "Net".len(),
        minutes: "Minutes".len(),
        share: "Share".len(),
    };
    for row in blocks.iter().flatten() {
        widths.name = widths.name.max(row.name.len());
        widths.net = widths.net.max(row.net.len());
        widths.minutes = widths.minutes.max(row.minutes.len());
        widths.share = widths.share.max(row.share.len());
    }
    widths
}

fn emit_row(out: &mut String, row: &Row, widths: Widths) {
    let _ = writeln!(
        out,
        "{:<name_w$}  {:>net_w$}  {:>min_w$}  {:>share_w$}",
        row.name,
        row.net,
        row.minutes,
        row.share,
        name_w = widths.name,
        net_w = widths.net,
        min_w = widths.minutes,
        share_w = widths.share,
    );
}

/// Two decimal places, half up (away from zero). Display only.
fn format_amount(value: f64) -> String {
    if !value.is_finite() {
        return "0.00".to_string();
    }
    let negative = value.is_sign_negative();
    let scaled = value.abs() * 100.0 + 0.5;
    if !scaled.is_finite() {
        return "0.00".to_string();
    }
    let cents = scaled.floor() as u128;
    let sign = if negative && cents != 0 { "-" } else { "" };
    format!("{sign}{}.{:02}", cents / 100, cents % 100)
}

/// Round minutes to 3 decimal places for display, then drop trailing zeros.
fn format_number(value: f64) -> String {
    if !value.is_finite() {
        return "0".to_string();
    }
    let text = format!("{value:.3}");
    let trimmed = text.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-" {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

fn format_share(net: f64, total: f64) -> String {
    format!("{}%", format_amount(share_ratio(net, total)))
}

fn share_ratio(net: f64, total: f64) -> f64 {
    if !net.is_finite() || !total.is_finite() || total == 0.0 {
        0.0
    } else {
        net / total * 100.0
    }
}

fn append_api(out: &mut String, api: &ApiData) {
    let _ = writeln!(out);
    write_section(out, "Runs", &run_lines(api));
    let _ = writeln!(out);
    write_section(out, "Jobs", &counted_lines(api));
    let _ = writeln!(out);
    write_section(out, "In progress", &progress_lines(api));
}

fn write_section(out: &mut String, heading: &str, lines: &[String]) {
    let _ = writeln!(out, "{heading}");
    if lines.is_empty() {
        let _ = writeln!(out, "(none)");
        return;
    }
    for line in lines {
        let _ = writeln!(out, "{line}");
    }
}

fn run_lines(api: &ApiData) -> Vec<String> {
    sorted_runs(api)
        .into_iter()
        .map(|run| {
            let conclusion = run.conclusion.as_deref().unwrap_or(IN_PROGRESS);
            let started = run.run_started_at.as_deref().unwrap_or("-");
            format!(
                "{id}  {path}  attempt {attempt}  number {number}  {event}  {conclusion}  {started}",
                id = run.id,
                path = run.path,
                attempt = run.run_attempt,
                number = run.run_number,
                event = run.event,
            )
        })
        .collect()
}

fn counted_lines(api: &ApiData) -> Vec<String> {
    sorted_jobs(api, true)
        .into_iter()
        .map(|job| {
            let completed = job.completed_at.as_deref().unwrap_or("-");
            let conclusion = job.conclusion.as_deref().unwrap_or("-");
            format!(
                "{}  completed {completed}  {conclusion}",
                job_prefix(api, job)
            )
        })
        .collect()
}

fn progress_lines(api: &ApiData) -> Vec<String> {
    sorted_jobs(api, false)
        .into_iter()
        .map(|job| format!("{}  {IN_PROGRESS}", job_prefix(api, job)))
        .collect()
}

fn job_prefix(api: &ApiData, job: &Job) -> String {
    let path = api.workflow_path(job.run_id);
    let labels = if job.labels.is_empty() {
        "-".to_string()
    } else {
        job.labels.join(", ")
    };
    let runner = job.runner_name.as_deref().unwrap_or("-");
    let started = job.started_at.as_deref().unwrap_or("-");
    format!(
        "{id}  {name}  run {run_id}  {path}  attempt {attempt}  labels {labels}  runner {runner}  started {started}",
        id = job.id,
        name = job.name,
        run_id = job.run_id,
        attempt = job.run_attempt,
    )
}

fn sorted_runs(api: &ApiData) -> Vec<&WorkflowRun> {
    let mut runs: Vec<&WorkflowRun> = api.runs.iter().collect();
    runs.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.run_number.cmp(&right.run_number))
            .then(left.run_attempt.cmp(&right.run_attempt))
            .then(left.id.cmp(&right.id))
    });
    runs
}

fn sorted_jobs(api: &ApiData, counts: bool) -> Vec<&Job> {
    let mut jobs: Vec<&Job> = if counts {
        api.counted_jobs().collect()
    } else {
        api.in_progress_jobs().collect()
    };
    jobs.sort_by(|left, right| {
        api.workflow_path(left.run_id)
            .cmp(api.workflow_path(right.run_id))
            .then(left.name.cmp(&right.name))
            .then(left.run_id.cmp(&right.run_id))
            .then(left.run_attempt.cmp(&right.run_attempt))
            .then(left.id.cmp(&right.id))
    });
    jobs
}

fn api_json(api: &ApiData) -> (serde_json::Value, serde_json::Value) {
    let mut runs: Vec<serde_json::Value> = sorted_runs(api)
        .into_iter()
        .map(|run| run_json(run, counted_job_values(api, run.id)))
        .collect();
    let known: std::collections::HashSet<u64> = api.runs.iter().map(|run| run.id).collect();
    let mut missing: Vec<u64> = Vec::new();
    for job in &api.jobs {
        if !known.contains(&job.run_id) && !missing.contains(&job.run_id) {
            missing.push(job.run_id);
        }
    }
    missing.sort_unstable();
    for run_id in missing {
        runs.push(serde_json::json!({
            "id": run_id,
            "path": UNKNOWN_WORKFLOW,
            "run_attempt": null,
            "run_number": null,
            "event": null,
            "conclusion": null,
            "run_started_at": null,
            "jobs": counted_job_values(api, run_id),
        }));
    }
    runs.sort_by(|left, right| run_sort_key(left).cmp(&run_sort_key(right)));
    let in_progress: Vec<serde_json::Value> = sorted_jobs(api, false)
        .into_iter()
        .map(|job| {
            let mut value = job_json(job, false);
            value["workflow_path"] = serde_json::Value::from(api.workflow_path(job.run_id));
            value["message"] = serde_json::Value::from(IN_PROGRESS);
            value
        })
        .collect();
    (
        serde_json::Value::Array(runs),
        serde_json::Value::Array(in_progress),
    )
}

fn counted_job_values(api: &ApiData, run_id: u64) -> Vec<serde_json::Value> {
    let mut jobs: Vec<&Job> = api
        .jobs
        .iter()
        .filter(|job| job.run_id == run_id && job.counts())
        .collect();
    jobs.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(left.run_attempt.cmp(&right.run_attempt))
            .then(left.id.cmp(&right.id))
    });
    jobs.into_iter().map(|job| job_json(job, true)).collect()
}

fn run_json(run: &WorkflowRun, jobs: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({
        "id": run.id,
        "path": run.path,
        "run_attempt": run.run_attempt,
        "run_number": run.run_number,
        "event": run.event,
        "conclusion": run.conclusion,
        "run_started_at": run.run_started_at,
        "jobs": jobs,
    })
}

fn run_sort_key(value: &serde_json::Value) -> (&str, u64, u64, u64) {
    (
        value["path"].as_str().unwrap_or(UNKNOWN_WORKFLOW),
        value["run_number"].as_u64().unwrap_or(0),
        value["run_attempt"].as_u64().unwrap_or(0),
        value["id"].as_u64().unwrap_or(0),
    )
}

fn job_json(job: &Job, counts: bool) -> serde_json::Value {
    serde_json::json!({
        "id": job.id,
        "run_id": job.run_id,
        "run_attempt": job.run_attempt,
        "name": job.name,
        "labels": job.labels,
        "started_at": job.started_at,
        "completed_at": job.completed_at,
        "conclusion": job.conclusion,
        "runner_name": job.runner_name,
        "counts": counts,
    })
}

fn finite(value: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::findings::{Finding, FindingKind};
    use crate::rank::{SkuTotal, WorkflowTotal};
    use std::path::PathBuf;

    fn workflow(path: &str, net: f64, minutes: f64, sku: &str) -> WorkflowTotal {
        WorkflowTotal {
            workflow_path: path.to_string(),
            net,
            gross: net,
            minutes,
            skus: vec![SkuTotal {
                sku: sku.to_string(),
                net,
                gross: net,
                minutes,
            }],
        }
    }

    fn finding(path: &str, line: usize, kind: FindingKind, message: &str) -> Finding {
        Finding {
            path: PathBuf::from(path),
            line,
            kind,
            message: message.to_string(),
        }
    }

    #[test]
    fn amounts_round_half_up_to_two_decimals() {
        assert_eq!(format_amount(0.005), "0.01");
        assert_eq!(format_amount(2.112), "2.11");
        assert_eq!(format_amount(0.0), "0.00");
    }

    #[test]
    fn report_lists_total_share_skus_and_findings() {
        let workflows = vec![
            workflow("a.yml", 3.0, 10.0, "actions_linux"),
            workflow("b.yml", 1.0, 0.0, "actions_storage"),
        ];
        let findings = vec![finding(
            "demo.yml",
            4,
            FindingKind::MissingTimeout,
            "default timeout is 360 minutes",
        )];
        let text = render(&workflows, 4.0, 10.0, Some(&findings));
        assert!(text.starts_with("USD (from net_amount)\n"), "{text}");
        assert!(text.contains("Total net: 4.00\n"), "{text}");
        assert!(text.contains("Total minutes: 10\n"), "{text}");
        assert!(text.contains("75.00%"), "{text}");
        assert!(text.contains("25.00%"), "{text}");
        assert!(text.contains("actions_linux"), "{text}");
        assert!(text.contains("actions_storage"), "{text}");
        assert!(
            text.contains(
                "Workflows without usage\nF2 demo.yml:4: default timeout is 360 minutes\n"
            ),
            "{text}"
        );
        assert!(text.find("a.yml").unwrap() < text.find("Workflows without usage").unwrap());
        assert!(!text.contains("Sample"), "{text}");
    }

    #[test]
    fn finding_is_listed_under_its_workflow() {
        let workflows = vec![
            workflow(".github/workflows/lint.yml", 3.0, 10.0, "actions_macos"),
            workflow(".github/workflows/ci.yml", 1.0, 4.0, "actions_linux"),
        ];
        let findings = vec![
            finding(
                "samples/.github/workflows/lint.yml",
                8,
                FindingKind::LintOnRunner,
                "lint on macOS",
            ),
            finding(
                "samples/.github/workflows/nightly.yml",
                2,
                FindingKind::MissingTimeout,
                "default timeout is 360 minutes",
            ),
        ];
        let text = render(&workflows, 4.0, 14.0, Some(&findings));
        let lint = text.find(".github/workflows/lint.yml").expect("lint row");
        let lint_finding = text
            .find("  F1 samples/.github/workflows/lint.yml:8: lint on macOS")
            .expect("lint finding");
        let ci = text.find(".github/workflows/ci.yml").expect("ci row");
        let heading = text
            .find("Workflows without usage")
            .expect("unattached heading");
        let nightly = text
            .find("F2 samples/.github/workflows/nightly.yml:2: default timeout is 360 minutes")
            .expect("nightly finding");
        assert!(lint < lint_finding && lint_finding < ci, "{text}");
        assert!(ci < heading && heading < nightly, "{text}");
        assert_eq!(text.matches("lint on macOS").count(), 1, "{text}");
    }

    #[test]
    fn share_uses_the_whole_bill() {
        let workflows = vec![workflow(
            ".github/workflows/lint.yml",
            230.144,
            3712.0,
            "actions_macos",
        )];
        let text = render(&workflows, 284.208, 12208.0, None);
        assert!(text.contains("Total net: 284.21\n"), "{text}");
        assert!(text.contains("80.98%"), "{text}");
        assert!(text.contains("230.14"), "{text}");
        assert!(!text.contains("230.144"), "{text}");
        assert!(!text.contains("284.208"), "{text}");
        assert!(!text.contains("100.00%"), "{text}");
        assert!(!text.contains("Findings"), "{text}");
        assert!(!text.contains("Workflows without usage"), "{text}");
        assert!(!text.contains("Sample"), "{text}");
    }

    #[test]
    fn empty_scan_has_no_finding_section() {
        let text = render(&[], 0.0, 0.0, Some(&[]));
        assert!(text.contains("Total net: 0.00\n"), "{text}");
        assert!(!text.contains("Findings"), "{text}");
        assert!(!text.contains("Workflows without usage"), "{text}");
        assert!(!text.contains("Sample"), "{text}");
    }

    #[test]
    fn json_keeps_unrounded_numbers_and_nests_findings() {
        let workflows = vec![
            workflow(".github/workflows/lint.yml", 2.112, 10.0, "actions_macos"),
            workflow(".github/workflows/ci.yml", 1.0, 0.0, "actions_linux"),
        ];
        let findings = vec![
            finding(
                "app/.github/workflows/lint.yml",
                8,
                FindingKind::LintOnRunner,
                "lint on macOS",
            ),
            finding(
                "app/.github/workflows/nightly.yml",
                3,
                FindingKind::MissingTimeout,
                "default timeout is 360 minutes",
            ),
        ];
        let text = render_json(&workflows, 2.112, 10.0, Some(&findings), None);
        assert!(!text.contains("USD (from net_amount)"), "{text}");
        let value: serde_json::Value = serde_json::from_str(text.trim_end()).expect("json");
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["currency"], "USD");
        assert_eq!(
            value["total_net"].as_f64().unwrap().to_bits(),
            2.112_f64.to_bits()
        );
        assert_eq!(
            value["total_minutes"].as_f64().unwrap().to_bits(),
            10.0_f64.to_bits()
        );
        assert_eq!(value["workflows"][0]["path"], ".github/workflows/lint.yml");
        assert_eq!(
            value["workflows"][0]["net"].as_f64().unwrap().to_bits(),
            2.112_f64.to_bits()
        );
        assert_eq!(
            value["workflows"][0]["gross"].as_f64().unwrap().to_bits(),
            2.112_f64.to_bits()
        );
        assert!(value["workflows"][0]["share"].is_number());
        assert!(value["workflows"][0]["minutes"].is_number());
        assert_eq!(value["workflows"][0]["skus"][0]["sku"], "actions_macos");
        assert_eq!(
            value["workflows"][0]["skus"][0]["net"]
                .as_f64()
                .unwrap()
                .to_bits(),
            2.112_f64.to_bits()
        );
        assert!(value["workflows"][0]["skus"][0].get("gross").is_none());
        assert_eq!(value["workflows"][0]["findings"][0]["id"], "F1");
        assert_eq!(
            value["workflows"][0]["findings"][0]["file"],
            "app/.github/workflows/lint.yml"
        );
        assert_eq!(value["workflows"][0]["findings"][0]["line"], 8);
        assert_eq!(
            value["workflows"][0]["findings"][0]["message"],
            "lint on macOS"
        );
        assert!(value["workflows"][1]["findings"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(value["unattached_findings"][0]["id"], "F2");
        assert_eq!(
            value["unattached_findings"][0]["file"],
            "app/.github/workflows/nightly.yml"
        );
        assert_eq!(value["unattached_findings"][0]["line"], 3);
        assert_eq!(value["unattached_findings"].as_array().unwrap().len(), 1);
        assert!(value.get("budget").is_none(), "{value}");
        assert!(value.get("budget_exceeded").is_none(), "{value}");
    }

    #[test]
    fn budget_exceeded_line_rounds_to_cents() {
        assert_eq!(
            budget_exceeded_line(10.0, 9.5),
            "Budget exceeded: 10.00 > 9.50"
        );
        assert_eq!(
            budget_exceeded_line(0.005, 0.0),
            "Budget exceeded: 0.01 > 0.00"
        );
    }

    #[test]
    fn json_budget_fields_follow_the_net_total() {
        let workflows = vec![workflow("a.yml", 10.0, 1.0, "actions_linux")];

        let under = render_json(&workflows, 10.0, 1.0, None, Some(10.125));
        let under_value: serde_json::Value = serde_json::from_str(under.trim_end()).unwrap();
        assert_eq!(
            under_value["budget"].as_f64().unwrap().to_bits(),
            10.125_f64.to_bits()
        );
        assert_eq!(under_value["budget_exceeded"], false);
        assert!(under_value["budget"].is_number());
        assert_eq!(under_value["schema_version"], 1);
        assert_eq!(under_value["currency"], "USD");

        let equal = render_json(&workflows, 10.0, 1.0, None, Some(10.0));
        let equal_value: serde_json::Value = serde_json::from_str(equal.trim_end()).unwrap();
        assert_eq!(equal_value["budget_exceeded"], false);
        assert_eq!(
            equal_value["budget"].as_f64().unwrap().to_bits(),
            10.0_f64.to_bits()
        );
        assert_eq!(
            equal_value["total_net"].as_f64().unwrap().to_bits(),
            10.0_f64.to_bits()
        );

        let over = render_json(&workflows, 10.0, 1.0, None, Some(9.5));
        let over_value: serde_json::Value = serde_json::from_str(over.trim_end()).unwrap();
        assert_eq!(over_value["budget_exceeded"], true);
        assert_eq!(
            over_value["budget"].as_f64().unwrap().to_bits(),
            9.5_f64.to_bits()
        );
        assert!(over_value["budget_exceeded"].is_boolean());
        assert_eq!(over_value["workflows"][0]["net"].as_f64(), Some(10.0));

        let absent = render_json(&workflows, 10.0, 1.0, None, None);
        let absent_value: serde_json::Value = serde_json::from_str(absent.trim_end()).unwrap();
        assert!(absent_value.get("budget").is_none(), "{absent_value}");
        assert!(
            absent_value.get("budget_exceeded").is_none(),
            "{absent_value}"
        );
    }

    fn sample_api() -> ApiData {
        ApiData {
            runs: vec![
                WorkflowRun {
                    id: 1002,
                    path: ".github/workflows/lint.yml".to_string(),
                    run_attempt: 2,
                    run_number: 41,
                    event: "pull_request".to_string(),
                    conclusion: None,
                    run_started_at: Some("2026-03-02T11:00:00Z".to_string()),
                },
                WorkflowRun {
                    id: 1001,
                    path: ".github/workflows/ci.yml".to_string(),
                    run_attempt: 1,
                    run_number: 40,
                    event: "push".to_string(),
                    conclusion: Some("success".to_string()),
                    run_started_at: Some("2026-03-01T10:00:00Z".to_string()),
                },
            ],
            jobs: vec![
                Job {
                    id: 601,
                    run_id: 1002,
                    run_attempt: 2,
                    name: "lint".to_string(),
                    labels: vec!["macos-latest".to_string()],
                    started_at: Some("2026-03-02T11:00:10Z".to_string()),
                    completed_at: None,
                    conclusion: None,
                    runner_name: Some("GitHub Actions 90".to_string()),
                },
                Job {
                    id: 501,
                    run_id: 1001,
                    run_attempt: 1,
                    name: "build".to_string(),
                    labels: vec!["ubuntu-latest".to_string()],
                    started_at: Some("2026-03-01T10:00:05Z".to_string()),
                    completed_at: Some("2026-03-01T10:02:05Z".to_string()),
                    conclusion: Some("success".to_string()),
                    runner_name: Some("GitHub Actions 88".to_string()),
                },
                Job {
                    id: 77,
                    run_id: 99,
                    run_attempt: 1,
                    name: "orphan".to_string(),
                    labels: vec![],
                    started_at: None,
                    completed_at: Some("2026-03-04T00:01:00Z".to_string()),
                    conclusion: None,
                    runner_name: None,
                },
            ],
        }
    }

    fn section<'a>(text: &'a str, heading: &str) -> &'a str {
        let marker = format!("\n{heading}\n");
        let start = text
            .find(&marker)
            .unwrap_or_else(|| panic!("missing {heading} in {text}"))
            + marker.len();
        let rest = &text[start..];
        let end = ["\nRuns\n", "\nJobs\n", "\nIn progress\n"]
            .into_iter()
            .filter_map(|item| rest.find(item))
            .min()
            .unwrap_or(rest.len());
        &rest[..end]
    }

    #[test]
    fn runs_section_splits_finished_jobs_from_in_progress() {
        let workflows = vec![workflow(
            ".github/workflows/ci.yml",
            3.0,
            10.0,
            "actions_linux",
        )];
        let findings = vec![finding(
            "samples/.github/workflows/lint.yml",
            8,
            FindingKind::LintOnRunner,
            "lint on macOS",
        )];
        let api = sample_api();
        let text = render_with_runs(&workflows, 3.0, 10.0, Some(&findings), &api);
        assert!(text.contains("USD (from net_amount)\n"), "{text}");
        assert!(text.contains("Total net: 3.00\n"), "{text}");
        let finding_at = text.find("lint on macOS").unwrap();
        let runs_at = text.find("\nRuns\n").unwrap();
        assert!(finding_at < runs_at, "{text}");

        let runs = section(&text, "Runs");
        assert!(
            runs.contains(
                "1001  .github/workflows/ci.yml  attempt 1  number 40  push  success  2026-03-01T10:00:00Z\n"
            ),
            "{runs}"
        );
        assert!(
            runs.contains(
                "1002  .github/workflows/lint.yml  attempt 2  number 41  pull_request  in progress  2026-03-02T11:00:00Z\n"
            ),
            "{runs}"
        );
        assert!(
            runs.find("1001").unwrap() < runs.find("1002").unwrap(),
            "{runs}"
        );

        let jobs = section(&text, "Jobs");
        assert!(
            jobs.contains(
                "501  build  run 1001  .github/workflows/ci.yml  attempt 1  labels ubuntu-latest  runner GitHub Actions 88  started 2026-03-01T10:00:05Z  completed 2026-03-01T10:02:05Z  success\n"
            ),
            "{jobs}"
        );
        assert!(
            jobs.contains(
                "77  orphan  run 99  (unknown workflow)  attempt 1  labels -  runner -  started -  completed 2026-03-04T00:01:00Z  -\n"
            ),
            "{jobs}"
        );
        assert!(!jobs.contains("\n601 "), "{jobs}");
        assert!(!jobs.contains("lint"), "{jobs}");

        let running = section(&text, "In progress");
        assert_eq!(
            running,
            "601  lint  run 1002  .github/workflows/lint.yml  attempt 2  labels macos-latest  runner GitHub Actions 90  started 2026-03-02T11:00:10Z  in progress\n"
        );
        assert!(!running.contains("completed"), "{running}");
        assert_eq!(running.matches("in progress").count(), 1, "{running}");
    }

    #[test]
    fn json_puts_in_progress_jobs_outside_the_counted_list() {
        let api = sample_api();
        let text = render_json_with_runs(&[], 0.0, 0.0, None, Some(1.5), &api);
        let value: serde_json::Value = serde_json::from_str(text.trim_end()).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["currency"], "USD");
        assert_eq!(value["budget_exceeded"], false);

        let runs = value["workflow_runs"].as_array().unwrap();
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0]["id"], 99);
        assert_eq!(runs[0]["path"], "(unknown workflow)");
        assert!(runs[0]["event"].is_null());
        assert!(runs[0]["run_attempt"].is_null());
        assert_eq!(runs[0]["jobs"][0]["id"], 77);
        assert_eq!(runs[0]["jobs"][0]["counts"], true);
        assert_eq!(runs[0]["jobs"][0]["name"], "orphan");

        assert_eq!(runs[1]["id"], 1001);
        assert_eq!(runs[1]["path"], ".github/workflows/ci.yml");
        assert_eq!(runs[1]["conclusion"], "success");
        assert_eq!(runs[1]["jobs"].as_array().unwrap().len(), 1);
        assert_eq!(runs[1]["jobs"][0]["id"], 501);
        assert_eq!(runs[1]["jobs"][0]["counts"], true);
        assert_eq!(runs[1]["jobs"][0]["name"], "build");
        assert!(runs[1]["jobs"][0]["completed_at"].is_string());

        assert_eq!(runs[2]["id"], 1002);
        assert!(runs[2]["conclusion"].is_null());
        assert!(runs[2]["jobs"].as_array().unwrap().is_empty());

        let running = value["in_progress"].as_array().unwrap();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0]["id"], 601);
        assert_eq!(running[0]["run_id"], 1002);
        assert_eq!(running[0]["name"], "lint");
        assert_eq!(running[0]["counts"], false);
        assert_eq!(running[0]["message"], "in progress");
        assert!(running[0]["completed_at"].is_null());
        assert_eq!(running[0]["workflow_path"], ".github/workflows/lint.yml");
        assert!(running[0]["labels"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("macos-latest")));

        let counted_ids: Vec<u64> = runs
            .iter()
            .flat_map(|run| run["jobs"].as_array().unwrap())
            .map(|job| job["id"].as_u64().unwrap())
            .collect();
        assert!(!counted_ids.contains(&601));
        assert!(counted_ids.contains(&501));
    }

    #[test]
    fn empty_runs_dir_prints_empty_sections() {
        let api = crate::api::ApiData {
            runs: Vec::new(),
            jobs: Vec::new(),
        };
        let text = render_with_runs(&[], 0.0, 0.0, None, &api);
        assert!(text.contains("\nRuns\n(none)\n"), "{text}");
        assert!(text.contains("\nJobs\n(none)\n"), "{text}");
        assert!(text.contains("\nIn progress\n(none)\n"), "{text}");
    }

    #[test]
    fn report_without_runs_omits_actions_sections() {
        let text = render(&[], 0.0, 0.0, None);
        assert!(!text.contains("\nRuns\n"), "{text}");
        assert!(!text.contains("\nJobs\n"), "{text}");
        assert!(!text.contains("In progress"), "{text}");
        assert!(!text.contains("(none)"), "{text}");

        let json = render_json(&[], 0.0, 0.0, None, None);
        let value: serde_json::Value = serde_json::from_str(json.trim_end()).unwrap();
        assert!(value.get("workflow_runs").is_none(), "{value}");
        assert!(value.get("in_progress").is_none(), "{value}");
    }
}
