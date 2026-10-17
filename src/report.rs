use std::fmt::Write;
use std::path::Path;

use crate::api::{linked_workflow_path, ApiData, Job, WorkflowRun, IN_PROGRESS, UNKNOWN_WORKFLOW};
use crate::findings::Finding;
use crate::money::{percent_fixed, Micros};
use crate::rank::{Bill, WorkflowTotal};

/// Text report. Money is rounded to cents here. Totals are sums of raw micros.
///
/// `bill.total_net` and `bill.total_minutes` are the Actions bill. `workflows`
/// is the full ranked list. `top` limits the table to that many rows; each
/// share is still that workflow's part of the Actions net. Workflows past the
/// limit are one line, `N more workflows (X USD)`, and their findings are
/// listed under `Findings outside the top N`. Other products are one line,
/// outside the Actions total.
/// Each workflow row shows net, gross, and discount, so a storage row whose
/// net is zero still shows a positive gross.
/// Findings attach to a workflow when the YAML path ends in
/// `.github/workflows/<file>` and that suffix matches `workflow_path`.
/// Invoice findings attach by repository and workflow path instead.
/// A callee finding with `workflow_path` set and no repository is listed on
/// that caller. Its file and line stay on the callee.
/// A finding with no usage row is listed under "Workflows without usage".
pub fn render(
    workflows: &[WorkflowTotal],
    bill: &Bill,
    findings: Option<&[Finding]>,
    top: Option<usize>,
) -> String {
    render_report(workflows, bill, findings, None, top)
}

/// Text report plus saved Actions runs and jobs.
///
/// Run paths are linked to a CSV workflow path: an `@ref` suffix and an
/// `owner/repo/` prefix are removed. Finished jobs are listed under "Jobs".
/// A job with no `completed_at` is listed under "In progress" and is not
/// included with the finished jobs. A job on the latest attempt that started
/// before the run started is an inherited copy and costs 0.00.
pub fn render_with_runs(
    workflows: &[WorkflowTotal],
    bill: &Bill,
    findings: Option<&[Finding]>,
    api: &ApiData,
    top: Option<usize>,
) -> String {
    render_report(workflows, bill, findings, Some(api), top)
}

fn render_report(
    workflows: &[WorkflowTotal],
    bill: &Bill,
    findings: Option<&[Finding]>,
    api: Option<&ApiData>,
    top: Option<usize>,
) -> String {
    let limit = shown_limit(workflows.len(), top);
    let attached = attach(workflows, findings);
    let shown = &workflows[..limit];
    let omitted = &workflows[limit..];
    let shown_findings = &attached.by_workflow[..limit];
    let omitted_findings = &attached.by_workflow[limit..];
    let mut out = String::new();
    let _ = writeln!(out, "USD (from net_amount)");
    let _ = writeln!(out, "{}", period_line(bill));
    let _ = writeln!(out, "{}", repositories_line(bill));
    let _ = writeln!(out);
    let _ = writeln!(out, "Total net: {}", bill.total_net.format_cents());
    let _ = writeln!(
        out,
        "Total minutes: {}",
        bill.total_minutes.format_quantity()
    );
    let _ = writeln!(
        out,
        "Other products: {}",
        bill.other_products_net.format_cents()
    );
    let _ = writeln!(out);

    let blocks: Vec<Vec<Row>> = shown
        .iter()
        .map(|workflow| workflow_rows(workflow, bill.total_net))
        .collect();
    let widths = widths(&blocks);
    emit_row(
        &mut out,
        &Row {
            name: "Workflow".to_string(),
            net: "Net".to_string(),
            gross: "Gross".to_string(),
            discount: "Discount".to_string(),
            minutes: "Minutes".to_string(),
            share: "Share".to_string(),
        },
        widths,
    );
    for (rows, found) in blocks.iter().zip(shown_findings) {
        for row in rows {
            emit_row(&mut out, row, widths);
        }
        for finding in found {
            let _ = writeln!(out, "  {}", finding_text(finding));
        }
    }

    if let Some(limit_n) = top.filter(|_| !omitted.is_empty()) {
        append_outside(&mut out, omitted, omitted_findings, limit_n);
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

/// One JSON object. Money is a 6-decimal string. Minutes stay a JSON number when whole.
///
/// `workflows` lists only the rows `--top` keeps. When that limit omits
/// workflows, `outside_top` holds their count, their net, and the same
/// workflow objects, findings included. `unattached_findings` is only
/// findings with no usage row. Every workflow object has `steps`, an array
/// of `{name, line}` from the scanned file with the same
/// `.github/workflows/<file>` name, in job order. A workflow with no scanned
/// file has an empty `steps` array.
pub fn render_json(
    workflows: &[WorkflowTotal],
    bill: &Bill,
    findings: Option<&[Finding]>,
    budget: Option<Micros>,
    top: Option<usize>,
    scanned: &[crate::workflow::Workflow],
) -> String {
    render_json_report(workflows, bill, findings, budget, None, top, scanned)
}

/// JSON report plus `workflow_runs` and `in_progress`.
///
/// Finished jobs sit on their run. An in-progress job is only in
/// `in_progress`, with `counts` false and message "in progress".
/// `path` is the linked workflow path. An inherited copy has
/// `inherited_copy` true, `billable` false, and `net_amount` `"0.000000"`.
pub fn render_json_with_runs(
    workflows: &[WorkflowTotal],
    bill: &Bill,
    findings: Option<&[Finding]>,
    budget: Option<Micros>,
    api: &ApiData,
    top: Option<usize>,
    scanned: &[crate::workflow::Workflow],
) -> String {
    render_json_report(workflows, bill, findings, budget, Some(api), top, scanned)
}

fn render_json_report(
    workflows: &[WorkflowTotal],
    bill: &Bill,
    findings: Option<&[Finding]>,
    budget: Option<Micros>,
    api: Option<&ApiData>,
    top: Option<usize>,
    scanned: &[crate::workflow::Workflow],
) -> String {
    let limit = shown_limit(workflows.len(), top);
    let attached = attach(workflows, findings);
    let shown = &workflows[..limit];
    let omitted = &workflows[limit..];
    let shown_findings = &attached.by_workflow[..limit];
    let omitted_findings = &attached.by_workflow[limit..];
    let shown_json: Vec<serde_json::Value> = shown
        .iter()
        .zip(shown_findings)
        .map(|(workflow, found)| workflow_json(workflow, bill.total_net, found, scanned))
        .collect();
    let unattached: Vec<serde_json::Value> = attached
        .unattached
        .iter()
        .map(|finding| finding_json(finding))
        .collect();
    let mut value = serde_json::json!({
        "schema_version": 2,
        "currency": "USD",
        "money_unit": "USD",
        "period": period_json(bill),
        "repositories": bill.repositories,
        "total_net": bill.total_net.to_decimal_string(),
        "total_minutes": minutes_json(bill.total_minutes),
        "other_products_net": bill.other_products_net.to_decimal_string(),
        "workflows": shown_json,
        "unattached_findings": unattached,
    });
    if let Some(api) = api {
        let (runs, in_progress) = api_json(api);
        value["workflow_runs"] = runs;
        value["in_progress"] = in_progress;
    }
    if !omitted.is_empty() {
        let outside: Vec<serde_json::Value> = omitted
            .iter()
            .zip(omitted_findings)
            .map(|(workflow, found)| workflow_json(workflow, bill.total_net, found, scanned))
            .collect();
        value["outside_top"] = serde_json::json!({
            "count": omitted.len(),
            "net": omitted_net(omitted).to_decimal_string(),
            "workflows": outside,
        });
    }
    if let Some(amount) = budget {
        value["budget"] = serde_json::Value::from(amount.to_decimal_string());
        value["budget_exceeded"] = serde_json::Value::from(bill.total_net > amount);
    }
    let mut text = serde_json::to_string_pretty(&value).expect("report json");
    text.push('\n');
    text
}

/// Status line when the Actions net total is above the budget. Amounts use cent rounding.
pub(crate) fn budget_exceeded_line(total_net: Micros, budget: Micros) -> String {
    format!(
        "Budget exceeded: {} > {}",
        total_net.format_cents(),
        budget.format_cents()
    )
}

fn period_line(bill: &Bill) -> String {
    match (&bill.period_from, &bill.period_to) {
        (Some(from), Some(to)) => format!("Period: {from} .. {to}"),
        _ => "Period: (none)".to_string(),
    }
}

fn repositories_line(bill: &Bill) -> String {
    if bill.repositories.is_empty() {
        "Repositories: (none)".to_string()
    } else {
        format!("Repositories: {}", bill.repositories.join(", "))
    }
}

fn period_json(bill: &Bill) -> serde_json::Value {
    match (&bill.period_from, &bill.period_to) {
        (Some(from), Some(to)) => serde_json::json!({ "from": from, "to": to }),
        _ => serde_json::Value::Null,
    }
}

fn minutes_json(value: Micros) -> serde_json::Value {
    match value.whole_units() {
        Some(whole) => serde_json::Value::from(whole),
        None => serde_json::Value::String(value.to_decimal_string()),
    }
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
                .position(|workflow| belongs_to(workflow, finding))
            {
                by_workflow[index].push(finding);
            } else {
                unattached.push(finding);
            }
        }
    }
    // The caller already lists this callee finding. Invoice rows carry a
    // repository, so an unmatched invoice row stays in the unattached list.
    unattached.retain(|finding| {
        finding.repository.is_some()
            || !by_workflow
                .iter()
                .flatten()
                .any(|placed| same_spot(finding, placed))
    });
    let mut unique: Vec<&Finding> = Vec::new();
    for finding in unattached {
        if finding.repository.is_some() || !unique.iter().any(|kept| same_spot(finding, kept)) {
            unique.push(finding);
        }
    }
    Attached {
        by_workflow,
        unattached: unique,
    }
}

fn same_spot(left: &Finding, right: &Finding) -> bool {
    left.path == right.path
        && left.line == right.line
        && left.kind == right.kind
        && left.message == right.message
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

fn belongs_to(workflow: &WorkflowTotal, finding: &Finding) -> bool {
    if let Some(workflow_path) = &finding.workflow_path {
        let path_ok = workflow_path_matches(&workflow.workflow_path, workflow_path);
        return match &finding.repository {
            Some(repository) => path_ok && workflow.repository == *repository,
            None => path_ok,
        };
    }
    let Some(key) = workflow_key(&finding.path) else {
        return false;
    };
    workflow_path_matches(&workflow.workflow_path, &key)
}

fn workflow_path_matches(workflow_path: &str, key: &str) -> bool {
    workflow_path == key || workflow_path.ends_with(&format!("/{key}"))
}

fn workflow_json(
    workflow: &WorkflowTotal,
    total_net: Micros,
    findings: &[&Finding],
    scanned: &[crate::workflow::Workflow],
) -> serde_json::Value {
    let skus: Vec<serde_json::Value> = workflow
        .skus
        .iter()
        .map(|sku| {
            serde_json::json!({
                "sku": sku.sku,
                "net": sku.net.to_decimal_string(),
                "minutes": minutes_json(sku.minutes),
            })
        })
        .collect();
    let found: Vec<serde_json::Value> = findings.iter().copied().map(finding_json).collect();
    let steps = step_values(&steps_for(scanned, &workflow.workflow_path));
    serde_json::json!({
        "repository": workflow.repository,
        "path": workflow.workflow_path,
        "net": workflow.net.to_decimal_string(),
        "gross": workflow.gross.to_decimal_string(),
        "discount": workflow.discount.to_decimal_string(),
        "minutes": minutes_json(workflow.minutes),
        "share": percent_fixed(workflow.net, total_net, 6),
        "skus": skus,
        "findings": found,
        "steps": steps,
    })
}

fn step_values(steps: &[crate::workflow::Step]) -> Vec<serde_json::Value> {
    steps
        .iter()
        .map(|step| {
            serde_json::json!({
                "name": step.name,
                "line": step.line,
            })
        })
        .collect()
}

/// Steps from every scanned file whose name is `.github/workflows/<file>`
/// for this usage path. Matching ignores ASCII case. Jobs stay in file order.
fn steps_for(
    scanned: &[crate::workflow::Workflow],
    workflow_path: &str,
) -> Vec<crate::workflow::Step> {
    let mut steps = Vec::new();
    for workflow in scanned {
        let Some(file_name) = workflow.path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if file_name.is_empty() || !same_workflow_file(workflow_path, file_name) {
            continue;
        }
        for job in &workflow.jobs {
            steps.extend(job.steps.iter().cloned());
        }
    }
    steps
}

fn same_workflow_file(workflow_path: &str, file_name: &str) -> bool {
    let key = format!(".github/workflows/{file_name}");
    let normalized = workflow_path.replace('\\', "/");
    if normalized.eq_ignore_ascii_case(&key) {
        return true;
    }
    normalized.char_indices().any(|(index, separator)| {
        separator == '/' && normalized[index + 1..].eq_ignore_ascii_case(&key)
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

/// How many ranked workflows the table shows. `None` shows every workflow.
fn shown_limit(len: usize, top: Option<usize>) -> usize {
    match top {
        Some(limit) => limit.min(len),
        None => len,
    }
}

fn omitted_net(workflows: &[WorkflowTotal]) -> Micros {
    workflows.iter().map(|workflow| workflow.net).sum()
}

fn more_workflows_line(count: usize, net: Micros) -> String {
    let noun = if count == 1 { "workflow" } else { "workflows" };
    format!("{count} more {noun} ({} USD)", net.format_cents())
}

/// Summary of workflows `--top` left out, then their findings.
///
/// The money is the sum of those workflows' nets, rounded to cents only here.
/// A workflow with no findings still counts in the summary. The findings block
/// is omitted when none of them have a finding.
fn append_outside(
    out: &mut String,
    omitted: &[WorkflowTotal],
    findings: &[Vec<&Finding>],
    top: usize,
) {
    if omitted.is_empty() {
        return;
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "{}",
        more_workflows_line(omitted.len(), omitted_net(omitted))
    );
    if findings.iter().all(|group| group.is_empty()) {
        return;
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "Findings outside the top {top}");
    for (workflow, group) in omitted.iter().zip(findings) {
        if group.is_empty() {
            continue;
        }
        let _ = writeln!(out, "{}", workflow_label(workflow));
        for finding in group {
            let _ = writeln!(out, "  {}", finding_text(finding));
        }
    }
}

struct Row {
    name: String,
    net: String,
    gross: String,
    discount: String,
    minutes: String,
    share: String,
}

#[derive(Clone, Copy)]
struct Widths {
    name: usize,
    net: usize,
    gross: usize,
    discount: usize,
    minutes: usize,
    share: usize,
}

fn workflow_label(workflow: &WorkflowTotal) -> String {
    if workflow.repository.is_empty() {
        workflow.workflow_path.clone()
    } else {
        format!("{} {}", workflow.repository, workflow.workflow_path)
    }
}

fn workflow_rows(workflow: &WorkflowTotal, total_net: Micros) -> Vec<Row> {
    let mut rows = vec![Row {
        name: workflow_label(workflow),
        net: workflow.net.format_cents(),
        gross: workflow.gross.format_cents(),
        discount: workflow.discount.format_cents(),
        minutes: workflow.minutes.format_quantity(),
        share: format_share(workflow.net, total_net),
    }];
    for sku in &workflow.skus {
        rows.push(Row {
            name: format!("  {}", sku.sku),
            net: sku.net.format_cents(),
            gross: sku.gross.format_cents(),
            discount: sku.discount.format_cents(),
            minutes: sku.minutes.format_quantity(),
            share: String::new(),
        });
    }
    rows
}

fn widths(blocks: &[Vec<Row>]) -> Widths {
    let mut widths = Widths {
        name: "Workflow".len(),
        net: "Net".len(),
        gross: "Gross".len(),
        discount: "Discount".len(),
        minutes: "Minutes".len(),
        share: "Share".len(),
    };
    for row in blocks.iter().flatten() {
        widths.name = widths.name.max(row.name.len());
        widths.net = widths.net.max(row.net.len());
        widths.gross = widths.gross.max(row.gross.len());
        widths.discount = widths.discount.max(row.discount.len());
        widths.minutes = widths.minutes.max(row.minutes.len());
        widths.share = widths.share.max(row.share.len());
    }
    widths
}

fn emit_row(out: &mut String, row: &Row, widths: Widths) {
    let _ = writeln!(
        out,
        "{:<name_w$}  {:>net_w$}  {:>gross_w$}  {:>discount_w$}  {:>min_w$}  {:>share_w$}",
        row.name,
        row.net,
        row.gross,
        row.discount,
        row.minutes,
        row.share,
        name_w = widths.name,
        net_w = widths.net,
        gross_w = widths.gross,
        discount_w = widths.discount,
        min_w = widths.minutes,
        share_w = widths.share,
    );
}

fn format_share(net: Micros, total: Micros) -> String {
    format!("{}%", percent_fixed(net, total, 2))
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
            let path = displayed_run_path(run);
            format!(
                "{id}  {path}  attempt {attempt}  number {number}  {event}  {conclusion}  {started}",
                id = run.id,
                attempt = run.run_attempt,
                number = run.run_number,
                event = run.event,
            )
        })
        .collect()
}

fn counted_lines(api: &ApiData) -> Vec<String> {
    finished_jobs(api)
        .into_iter()
        .map(|job| {
            let completed = job.completed_at.as_deref().unwrap_or("-");
            let conclusion = job.conclusion.as_deref().unwrap_or("-");
            format!(
                "{}  completed {completed}  {conclusion}{}",
                job_prefix(api, job),
                copy_suffix(api, job),
            )
        })
        .collect()
}

fn progress_lines(api: &ApiData) -> Vec<String> {
    in_progress_jobs(api)
        .into_iter()
        .map(|job| {
            format!(
                "{}  {IN_PROGRESS}{}",
                job_prefix(api, job),
                copy_suffix(api, job)
            )
        })
        .collect()
}

fn job_prefix(api: &ApiData, job: &Job) -> String {
    let path = displayed_run_path_for_id(api, job.run_id);
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
        displayed_run_path(left)
            .cmp(&displayed_run_path(right))
            .then(left.run_number.cmp(&right.run_number))
            .then(left.run_attempt.cmp(&right.run_attempt))
            .then(left.id.cmp(&right.id))
    });
    runs
}

fn in_progress_jobs(api: &ApiData) -> Vec<&Job> {
    let mut jobs: Vec<&Job> = api.in_progress_jobs().collect();
    sort_jobs(api, &mut jobs);
    jobs
}

fn sort_jobs(api: &ApiData, jobs: &mut [&Job]) {
    jobs.sort_by(|left, right| {
        let left_path = displayed_run_path_for_id(api, left.run_id);
        let right_path = displayed_run_path_for_id(api, right.run_id);
        left_path
            .cmp(&right_path)
            .then(left.name.cmp(&right.name))
            .then(left.run_id.cmp(&right.run_id))
            .then(left.run_attempt.cmp(&right.run_attempt))
            .then(left.id.cmp(&right.id))
    });
}

fn displayed_run_path(run: &WorkflowRun) -> String {
    let linked = linked_workflow_path(&run.path);
    if linked.is_empty() || run.links_to(&linked) {
        linked
    } else {
        run.path.clone()
    }
}

fn displayed_run_path_for_id(api: &ApiData, run_id: u64) -> String {
    match api.runs.iter().find(|run| run.id == run_id) {
        Some(run) => displayed_run_path(run),
        None => api.workflow_path(run_id),
    }
}

fn copy_suffix(api: &ApiData, job: &Job) -> String {
    match api.inherited_copy_net(job) {
        Some(net) => format!("  inherited copy  cost {}", net.format_cents()),
        None => String::new(),
    }
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
    let in_progress: Vec<serde_json::Value> = in_progress_jobs(api)
        .into_iter()
        .map(|job| {
            let mut value = job_json(api, job, false);
            value["workflow_path"] =
                serde_json::Value::from(displayed_run_path_for_id(api, job.run_id));
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
    let mut jobs: Vec<&Job> = finished_jobs(api)
        .into_iter()
        .filter(|job| job.run_id == run_id)
        .collect();
    jobs.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(left.run_attempt.cmp(&right.run_attempt))
            .then(left.id.cmp(&right.id))
    });
    jobs.into_iter()
        .map(|job| job_json(api, job, true))
        .collect()
}

/// Finished jobs that add cost, then inherited copies, in report order.
///
/// A copy stays visible with its own cost. It is not part of `billable_jobs`.
fn finished_jobs(api: &ApiData) -> Vec<&Job> {
    let copy_ids: Vec<u64> = api
        .counted_jobs()
        .filter(|job| api.is_inherited_copy(job))
        .map(|job| job.id)
        .collect();
    let mut jobs: Vec<&Job> = api.billable_jobs().collect();
    jobs.extend(api.counted_jobs().filter(|job| copy_ids.contains(&job.id)));
    sort_jobs(api, &mut jobs);
    jobs
}

fn run_json(run: &WorkflowRun, jobs: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({
        "id": run.id,
        "path": displayed_run_path(run),
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

fn job_json(api: &ApiData, job: &Job, counts: bool) -> serde_json::Value {
    let inherited = api.is_inherited_copy(job);
    let mut value = serde_json::json!({
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
        "inherited_copy": inherited,
        "billable": counts && !inherited,
    });
    if let Some(net) = api.inherited_copy_net(job) {
        value["net_amount"] = serde_json::Value::String(net.to_decimal_string());
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::findings::{Finding, FindingKind};
    use crate::money::Micros;
    use crate::rank::{Bill, SkuTotal, WorkflowTotal};
    use std::path::PathBuf;

    fn usd(text: &str) -> Micros {
        Micros::parse(text).unwrap()
    }

    fn bill(net: &str, minutes: &str) -> Bill {
        Bill {
            total_net: usd(net),
            total_minutes: usd(minutes),
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

    fn finding(path: &str, line: usize, kind: FindingKind, message: &str) -> Finding {
        Finding {
            path: PathBuf::from(path),
            line,
            kind,
            message: message.to_string(),
            repository: None,
            workflow_path: None,
        }
    }

    #[test]
    fn amounts_round_half_up_to_two_decimals() {
        assert_eq!(usd("0.005").format_cents(), "0.01");
        assert_eq!(usd("2.112").format_cents(), "2.11");
        assert_eq!(usd("0").format_cents(), "0.00");
    }

    #[test]
    fn report_lists_total_share_skus_and_findings() {
        let workflows = vec![
            workflow("a.yml", "3", "10", "actions_linux"),
            workflow("b.yml", "1", "0", "actions_storage"),
        ];
        let findings = vec![finding(
            "demo.yml",
            4,
            FindingKind::MissingTimeout,
            "default timeout is 360 minutes",
        )];
        let text = render(&workflows, &bill("4", "10"), Some(&findings), None);
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
            workflow(".github/workflows/lint.yml", "3", "10", "actions_macos"),
            workflow(".github/workflows/ci.yml", "1", "4", "actions_linux"),
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
        let text = render(&workflows, &bill("4", "14"), Some(&findings), None);
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
    fn storage_row_shows_gross_and_discount_when_net_is_zero() {
        let storage = WorkflowTotal {
            repository: "example-org/app".to_string(),
            workflow_path: "(no workflow)".to_string(),
            net: usd("0"),
            gross: usd("0.005040"),
            discount: usd("0.005040"),
            minutes: usd("0"),
            skus: vec![SkuTotal {
                sku: "actions_storage".to_string(),
                net: usd("0"),
                gross: usd("0.005040"),
                discount: usd("0.005040"),
                minutes: usd("0"),
            }],
        };
        let mut totals = bill("0", "0");
        totals.repositories = vec!["example-org/app".to_string()];
        let text = render(&[storage], &totals, None, None);
        assert!(text.contains("Gross"), "{text}");
        assert!(text.contains("Discount"), "{text}");
        let storage_line = text
            .lines()
            .find(|line| line.contains("actions_storage"))
            .expect("storage sku");
        assert!(storage_line.contains("0.00"), "{storage_line}");
        assert_eq!(storage_line.matches("0.01").count(), 2, "{storage_line}");
        let workflow_line = text
            .lines()
            .find(|line| line.contains("(no workflow)"))
            .expect("workflow");
        assert_eq!(workflow_line.matches("0.01").count(), 2, "{workflow_line}");
        assert!(workflow_line.contains("0.00"), "{workflow_line}");
        assert!(!text.contains("F9"), "{text}");
        assert!(!text.contains("invoice row mismatch"), "{text}");
    }

    #[test]
    fn invoice_finding_stays_on_its_repository_workflow() {
        let workflows = vec![
            WorkflowTotal {
                repository: "example-org/alpha".to_string(),
                workflow_path: ".github/workflows/ci.yml".to_string(),
                net: usd("7.316"),
                gross: usd("7.316"),
                discount: Micros::ZERO,
                minutes: usd("118"),
                skus: vec![SkuTotal {
                    sku: "actions_macos".to_string(),
                    net: usd("7.316"),
                    gross: usd("7.316"),
                    discount: Micros::ZERO,
                    minutes: usd("118"),
                }],
            },
            WorkflowTotal {
                repository: "example-org/zeta".to_string(),
                workflow_path: ".github/workflows/ci.yml".to_string(),
                net: usd("7.316"),
                gross: usd("7.316"),
                discount: Micros::ZERO,
                minutes: usd("118"),
                skus: vec![SkuTotal {
                    sku: "actions_macos".to_string(),
                    net: usd("7.316"),
                    gross: usd("7.316"),
                    discount: Micros::ZERO,
                    minutes: usd("118"),
                }],
            },
        ];
        let findings = vec![Finding {
            path: PathBuf::from("usage.csv"),
            line: 2,
            kind: FindingKind::InvoiceMismatch,
            message: "invoice row mismatch: sku \"actions_macos\", date \"2026-09-01\", gross 7.316000 != quantity * applied_cost_per_quantity 9.440000".to_string(),
            repository: Some("example-org/alpha".to_string()),
            workflow_path: Some(".github/workflows/ci.yml".to_string()),
        }];
        let mut totals = bill("14.632", "236");
        totals.repositories = vec![
            "example-org/alpha".to_string(),
            "example-org/zeta".to_string(),
        ];
        let text = render(&workflows, &totals, Some(&findings), None);
        let alpha = text
            .find("example-org/alpha .github/workflows/ci.yml")
            .expect("alpha");
        let zeta = text
            .find("example-org/zeta .github/workflows/ci.yml")
            .expect("zeta");
        let f9 = text
            .find("F9 usage.csv:2: invoice row mismatch")
            .expect("f9");
        assert!(alpha < f9 && f9 < zeta, "{text}");
        assert_eq!(text.matches("invoice row mismatch").count(), 1, "{text}");
        assert!(!text.contains("Workflows without usage"), "{text}");
    }

    #[test]
    fn share_uses_the_whole_bill() {
        let workflows = vec![workflow(
            ".github/workflows/lint.yml",
            "230.144",
            "3712",
            "actions_macos",
        )];
        let text = render(&workflows, &bill("284.208", "12208"), None, None);
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
        let text = render(&[], &bill("0", "0"), Some(&[]), None);
        assert!(text.contains("Total net: 0.00\n"), "{text}");
        assert!(!text.contains("Findings"), "{text}");
        assert!(!text.contains("Workflows without usage"), "{text}");
        assert!(!text.contains("Sample"), "{text}");
    }

    #[test]
    fn json_keeps_unrounded_numbers_and_nests_findings() {
        let workflows = vec![
            workflow(".github/workflows/lint.yml", "2.112", "10", "actions_macos"),
            workflow(".github/workflows/ci.yml", "1", "0", "actions_linux"),
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
        let text = render_json(
            &workflows,
            &bill("2.112", "10"),
            Some(&findings),
            None,
            None,
            &[],
        );
        assert!(!text.contains("USD (from net_amount)"), "{text}");
        assert!(!text.contains("0000000001"), "{text}");
        assert!(!text.contains("9999999"), "{text}");
        let value: serde_json::Value = serde_json::from_str(text.trim_end()).expect("json");
        assert_eq!(value["schema_version"], 2);
        assert_eq!(value["currency"], "USD");
        assert_eq!(value["money_unit"], "USD");
        assert_eq!(value["total_net"], "2.112000");
        assert_eq!(value["total_minutes"].as_i64(), Some(10));
        assert_eq!(value["other_products_net"], "0.000000");
        assert!(value["period"].is_null());
        assert_eq!(value["workflows"][0]["path"], ".github/workflows/lint.yml");
        assert_eq!(value["workflows"][0]["repository"], "");
        assert_eq!(value["workflows"][0]["net"], "2.112000");
        assert_eq!(value["workflows"][0]["gross"], "2.112000");
        assert_eq!(value["workflows"][0]["discount"], "0.000000");
        assert!(value["workflows"][0]["share"].is_string());
        assert_eq!(value["workflows"][0]["minutes"].as_i64(), Some(10));
        assert_eq!(value["workflows"][0]["skus"][0]["sku"], "actions_macos");
        assert_eq!(value["workflows"][0]["skus"][0]["net"], "2.112000");
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
            budget_exceeded_line(usd("10"), usd("9.5")),
            "Budget exceeded: 10.00 > 9.50"
        );
        assert_eq!(
            budget_exceeded_line(usd("0.005"), usd("0")),
            "Budget exceeded: 0.01 > 0.00"
        );
        assert_eq!(
            budget_exceeded_line(usd("10.004"), usd("10")),
            "Budget exceeded: 10.00 > 10.00"
        );
        assert!(usd("10.004") > usd("10"));
    }

    #[test]
    fn json_budget_fields_follow_the_net_total() {
        let workflows = vec![workflow("a.yml", "10", "1", "actions_linux")];
        let totals = bill("10", "1");

        let under = render_json(&workflows, &totals, None, Some(usd("10.125")), None, &[]);
        let under_value: serde_json::Value = serde_json::from_str(under.trim_end()).unwrap();
        assert_eq!(under_value["budget"], "10.125000");
        assert_eq!(under_value["budget_exceeded"], false);
        assert!(under_value["budget"].is_string());
        assert_eq!(under_value["schema_version"], 2);
        assert_eq!(under_value["currency"], "USD");
        assert_eq!(under_value["money_unit"], "USD");

        let equal = render_json(&workflows, &totals, None, Some(usd("10")), None, &[]);
        let equal_value: serde_json::Value = serde_json::from_str(equal.trim_end()).unwrap();
        assert_eq!(equal_value["budget_exceeded"], false);
        assert_eq!(equal_value["budget"], "10.000000");
        assert_eq!(equal_value["total_net"], "10.000000");

        let over = render_json(&workflows, &totals, None, Some(usd("9.5")), None, &[]);
        let over_value: serde_json::Value = serde_json::from_str(over.trim_end()).unwrap();
        assert_eq!(over_value["budget_exceeded"], true);
        assert_eq!(over_value["budget"], "9.500000");
        assert!(over_value["budget_exceeded"].is_boolean());
        assert_eq!(over_value["workflows"][0]["net"], "10.000000");

        let absent = render_json(&workflows, &totals, None, None, None, &[]);
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
            "3",
            "10",
            "actions_linux",
        )];
        let findings = vec![finding(
            "samples/.github/workflows/lint.yml",
            8,
            FindingKind::LintOnRunner,
            "lint on macOS",
        )];
        let api = sample_api();
        let text = render_with_runs(&workflows, &bill("3", "10"), Some(&findings), &api, None);
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
        let text = render_json_with_runs(
            &[],
            &bill("0", "0"),
            None,
            Some(usd("1.5")),
            &api,
            None,
            &[],
        );
        let value: serde_json::Value = serde_json::from_str(text.trim_end()).unwrap();
        assert_eq!(value["schema_version"], 2);
        assert_eq!(value["money_unit"], "USD");
        assert_eq!(value["budget"], "1.500000");
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
        let text = render_with_runs(&[], &bill("0", "0"), None, &api, None);
        assert!(text.contains("\nRuns\n(none)\n"), "{text}");
        assert!(text.contains("\nJobs\n(none)\n"), "{text}");
        assert!(text.contains("\nIn progress\n(none)\n"), "{text}");
    }

    #[test]
    fn report_without_runs_omits_actions_sections() {
        let text = render(&[], &bill("0", "0"), None, None);
        assert!(!text.contains("\nRuns\n"), "{text}");
        assert!(!text.contains("\nJobs\n"), "{text}");
        assert!(!text.contains("In progress"), "{text}");
        assert!(text.contains("Period: (none)\n"), "{text}");
        assert!(text.contains("Repositories: (none)\n"), "{text}");

        let json = render_json(&[], &bill("0", "0"), None, None, None, &[]);
        let value: serde_json::Value = serde_json::from_str(json.trim_end()).unwrap();
        assert!(value.get("workflow_runs").is_none(), "{value}");
        assert!(value.get("in_progress").is_none(), "{value}");
        assert!(value.get("outside_top").is_none(), "{value}");
    }

    fn named(repository: &str, path: &str, net: &str) -> WorkflowTotal {
        let mut workflow = workflow(path, net, "1", "actions_linux");
        workflow.repository = repository.to_string();
        workflow.gross = workflow.net;
        workflow
    }

    fn outside_heading(text: &str) -> usize {
        text.find("Findings outside the top")
            .unwrap_or_else(|| panic!("missing outside heading in {text}"))
    }

    #[test]
    fn top_one_keeps_an_omitted_finding_off_workflows_without_usage() {
        let workflows = vec![
            named("example-org/app", ".github/workflows/lint.yml", "10"),
            named("example-org/app", ".github/workflows/docs.yml", "2.112"),
            named("example-org/app", "(no workflow)", "0"),
        ];
        let findings = vec![
            finding(
                "samples/.github/workflows/lint.yml",
                8,
                FindingKind::LintOnRunner,
                "lint on macOS",
            ),
            finding(
                "samples/.github/workflows/docs.yml",
                4,
                FindingKind::MissingTimeout,
                "default timeout is 360 minutes",
            ),
            finding(
                "samples/.github/workflows/nightly.yml",
                3,
                FindingKind::MissingTimeout,
                "default timeout is 360 minutes",
            ),
        ];
        let totals = bill("12.112", "3");
        let text = render(&workflows, &totals, Some(&findings), Some(1));
        let outside = outside_heading(&text);
        let docs = text
            .find("F2 samples/.github/workflows/docs.yml:4: default timeout is 360 minutes")
            .expect("docs finding");
        let lint = text
            .find("  F1 samples/.github/workflows/lint.yml:8: lint on macOS")
            .expect("lint finding");
        let without = text
            .find("Workflows without usage")
            .expect("nightly has no usage row");
        let nightly = text
            .find("F2 samples/.github/workflows/nightly.yml:3: default timeout is 360 minutes")
            .expect("nightly");
        assert!(
            lint < outside && outside < docs && docs < without && without < nightly,
            "{text}"
        );
        assert!(text.contains("2 more workflows (2.11 USD)\n"), "{text}");
        assert!(text.contains("Findings outside the top 1\n"), "{text}");
        assert!(
            text.contains(
                "example-org/app .github/workflows/docs.yml\n  F2 samples/.github/workflows/docs.yml:4: default timeout is 360 minutes\n"
            ),
            "{text}"
        );
        assert_eq!(text.matches("docs.yml:4").count(), 1, "{text}");
        let without_block = &text[without..];
        assert!(!without_block.contains("docs.yml"), "{text}");
        assert!(text.contains("10.00"), "{text}");
        assert!(text.contains("Total net: 12.11\n"), "{text}");

        let full = render(&workflows, &totals, Some(&findings), None);
        assert!(!full.contains("more workflow"), "{full}");
        assert!(!full.contains("Findings outside"), "{full}");
        assert!(full.contains("Workflows without usage"), "{full}");
        assert!(full.find("docs.yml:4").unwrap() < full.find("Workflows without usage").unwrap());
    }

    #[test]
    fn top_zero_puts_every_usage_finding_outside_the_table() {
        let workflows = vec![
            named("example-org/app", ".github/workflows/lint.yml", "10"),
            named("example-org/app", ".github/workflows/docs.yml", "2.112"),
        ];
        let findings = vec![
            finding(
                "app/.github/workflows/lint.yml",
                8,
                FindingKind::LintOnRunner,
                "lint on macOS",
            ),
            finding(
                "app/.github/workflows/docs.yml",
                4,
                FindingKind::MissingTimeout,
                "default timeout is 360 minutes",
            ),
            finding(
                "app/.github/workflows/nightly.yml",
                2,
                FindingKind::MissingTimeout,
                "default timeout is 360 minutes",
            ),
        ];
        let totals = bill("12.112", "2");
        let text = render(&workflows, &totals, Some(&findings), Some(0));
        assert!(text.contains("2 more workflows (12.11 USD)\n"), "{text}");
        assert!(text.contains("Findings outside the top 0\n"), "{text}");
        let outside = outside_heading(&text);
        let lint = text.find("lint on macOS").unwrap();
        let docs = text.find("docs.yml:4").unwrap();
        let without = text.find("Workflows without usage").unwrap();
        assert!(outside < lint && lint < docs && docs < without, "{text}");
        assert!(!text.contains("10.00"), "{text}");
        let table = &text[..outside];
        assert!(!table.contains("lint.yml"), "{text}");
        assert!(!table.contains("docs.yml"), "{text}");
        let without_block = &text[without..];
        assert!(without_block.contains("nightly.yml:2"), "{without_block}");
        assert!(!without_block.contains("docs.yml"), "{text}");
        assert!(!without_block.contains("lint.yml"), "{text}");
    }

    #[test]
    fn top_at_or_above_the_workflow_count_matches_the_full_report() {
        let workflows = vec![
            named("example-org/app", ".github/workflows/lint.yml", "10"),
            named("example-org/app", ".github/workflows/docs.yml", "2.112"),
            named("", "(no workflow)", "0.004"),
        ];
        let findings = vec![finding(
            "repo/.github/workflows/docs.yml",
            4,
            FindingKind::MissingTimeout,
            "default timeout is 360 minutes",
        )];
        let totals = bill("12.116", "3");
        let full = render(&workflows, &totals, Some(&findings), None);
        let full_json = render_json(&workflows, &totals, Some(&findings), None, None, &[]);
        for limit in [3_usize, 4, 100] {
            let text = render(&workflows, &totals, Some(&findings), Some(limit));
            assert_eq!(text, full, "top {limit}");
            let json = render_json(&workflows, &totals, Some(&findings), None, Some(limit), &[]);
            assert_eq!(json, full_json, "top {limit}");
        }
        assert!(!full.contains("Workflows without usage"), "{full}");
        assert!(!full.contains("more workflow"), "{full}");
        assert!(!full.contains("Findings outside"), "{full}");
        assert!(full.contains("docs.yml:4"), "{full}");
        let value: serde_json::Value = serde_json::from_str(full_json.trim_end()).unwrap();
        assert!(value.get("outside_top").is_none(), "{value}");
        assert_eq!(value["workflows"].as_array().unwrap().len(), 3);
        assert_eq!(value["workflows"][1]["findings"][0]["line"], 4);
    }

    #[test]
    fn one_omitted_workflow_uses_a_singular_summary() {
        let workflows = vec![
            named("example-org/app", ".github/workflows/lint.yml", "10"),
            named("example-org/app", ".github/workflows/docs.yml", "2.112"),
        ];
        let findings = vec![finding(
            "app/.github/workflows/docs.yml",
            4,
            FindingKind::MissingTimeout,
            "default timeout is 360 minutes",
        )];
        let text = render(&workflows, &bill("12.112", "2"), Some(&findings), Some(1));
        assert!(text.contains("1 more workflow (2.11 USD)\n"), "{text}");
        assert!(!text.contains("1 more workflows"), "{text}");
        assert!(text.contains("Findings outside the top 1\n"), "{text}");
        assert!(!text.contains("Workflows without usage"), "{text}");
    }

    #[test]
    fn omitted_workflows_without_findings_skip_the_findings_block() {
        let workflows = vec![
            named("example-org/app", ".github/workflows/lint.yml", "3"),
            named("example-org/app", ".github/workflows/ci.yml", "1"),
        ];
        let text = render(&workflows, &bill("4", "2"), None, Some(1));
        assert!(text.contains("1 more workflow (1.00 USD)\n"), "{text}");
        assert!(!text.contains("Findings outside"), "{text}");
        assert!(!text.contains("Workflows without usage"), "{text}");
    }

    #[test]
    fn outside_summary_rounds_the_unrounded_sum() {
        let workflows = vec![
            named("example-org/app", ".github/workflows/lint.yml", "10"),
            named("example-org/app", ".github/workflows/ci.yml", "0.004"),
            named("example-org/app", ".github/workflows/docs.yml", "0.004"),
        ];
        let text = render(&workflows, &bill("10.008", "3"), None, Some(1));
        assert!(text.contains("2 more workflows (0.01 USD)\n"), "{text}");
        assert!(!text.contains("0.00 USD"), "{text}");

        let quiet = vec![
            named("example-org/app", ".github/workflows/lint.yml", "10"),
            named("example-org/app", ".github/workflows/ci.yml", "0.002"),
            named("example-org/app", ".github/workflows/docs.yml", "0.002"),
        ];
        let quiet_text = render(&quiet, &bill("10.004", "3"), None, Some(1));
        assert!(
            quiet_text.contains("2 more workflows (0.00 USD)\n"),
            "{quiet_text}"
        );
    }

    #[test]
    fn invoice_finding_on_an_omitted_workflow_stays_with_that_workflow() {
        let workflows = vec![
            named("example-org/alpha", ".github/workflows/lint.yml", "10"),
            named("example-org/zeta", ".github/workflows/ci.yml", "2.112"),
        ];
        let findings = vec![Finding {
            path: PathBuf::from("usage.csv"),
            line: 3,
            kind: FindingKind::InvoiceMismatch,
            message: "invoice row mismatch: sku \"actions_linux\", date \"2026-09-02\", gross 2.112000 != quantity * applied_cost_per_quantity 9.000000".to_string(),
            repository: Some("example-org/zeta".to_string()),
            workflow_path: Some(".github/workflows/ci.yml".to_string()),
        }];
        let text = render(&workflows, &bill("12.112", "2"), Some(&findings), Some(1));
        assert!(text.contains("1 more workflow (2.11 USD)\n"), "{text}");
        assert!(
            text.contains(
                "example-org/zeta .github/workflows/ci.yml\n  F9 usage.csv:3: invoice row mismatch"
            ),
            "{text}"
        );
        assert!(!text.contains("Workflows without usage"), "{text}");
        let outside = outside_heading(&text);
        assert!(text.find("F9 usage.csv:3").unwrap() > outside, "{text}");

        let value: serde_json::Value = serde_json::from_str(
            render_json(
                &workflows,
                &bill("12.112", "2"),
                Some(&findings),
                None,
                Some(1),
                &[],
            )
            .trim_end(),
        )
        .unwrap();
        assert!(value["unattached_findings"].as_array().unwrap().is_empty());
        assert_eq!(
            value["outside_top"]["workflows"][0]["repository"],
            "example-org/zeta"
        );
        assert_eq!(
            value["outside_top"]["workflows"][0]["findings"][0]["id"],
            "F9"
        );
        assert_eq!(value["workflows"].as_array().unwrap().len(), 1);
        assert!(value["workflows"][0]["findings"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn json_outside_top_keeps_findings_on_the_omitted_workflows() {
        let workflows = vec![
            named("example-org/app", ".github/workflows/lint.yml", "10"),
            named("example-org/app", ".github/workflows/docs.yml", "2.112"),
            named("example-org/app", "(no workflow)", "0"),
        ];
        let findings = vec![
            finding(
                "app/.github/workflows/lint.yml",
                8,
                FindingKind::LintOnRunner,
                "lint on macOS",
            ),
            finding(
                "app/.github/workflows/docs.yml",
                4,
                FindingKind::MissingTimeout,
                "default timeout is 360 minutes",
            ),
            finding(
                "app/.github/workflows/nightly.yml",
                2,
                FindingKind::MissingTimeout,
                "default timeout is 360 minutes",
            ),
        ];
        let totals = bill("12.112", "3");
        let text = render_json(
            &workflows,
            &totals,
            Some(&findings),
            Some(usd("1")),
            Some(1),
            &[],
        );
        assert!(!text.contains("0000000001"), "{text}");
        let value: serde_json::Value = serde_json::from_str(text.trim_end()).unwrap();
        assert_eq!(value["schema_version"], 2);
        assert_eq!(value["workflows"].as_array().unwrap().len(), 1);
        assert_eq!(value["workflows"][0]["path"], ".github/workflows/lint.yml");
        assert_eq!(value["workflows"][0]["findings"][0]["id"], "F1");
        assert_eq!(value["outside_top"]["count"], 2);
        assert_eq!(value["outside_top"]["net"], "2.112000");
        let omitted = value["outside_top"]["workflows"].as_array().unwrap();
        assert_eq!(omitted.len(), 2);
        assert_eq!(omitted[0]["path"], ".github/workflows/docs.yml");
        assert_eq!(omitted[0]["net"], "2.112000");
        assert_eq!(
            omitted[0]["findings"][0]["file"],
            "app/.github/workflows/docs.yml"
        );
        assert_eq!(omitted[0]["findings"][0]["line"], 4);
        assert_eq!(omitted[1]["path"], "(no workflow)");
        assert!(omitted[1]["findings"].as_array().unwrap().is_empty());
        assert_eq!(
            value["unattached_findings"][0]["file"],
            "app/.github/workflows/nightly.yml"
        );
        assert_eq!(value["unattached_findings"].as_array().unwrap().len(), 1);
        assert_eq!(value["budget"], "1.000000");
        assert_eq!(value["budget_exceeded"], true);

        let zero = render_json(&workflows, &totals, Some(&findings), None, Some(0), &[]);
        let zero_value: serde_json::Value = serde_json::from_str(zero.trim_end()).unwrap();
        assert!(zero_value["workflows"].as_array().unwrap().is_empty());
        assert_eq!(zero_value["outside_top"]["count"], 3);
        assert_eq!(zero_value["outside_top"]["net"], "12.112000");
        assert_eq!(
            zero_value["outside_top"]["workflows"][0]["findings"][0]["id"],
            "F1"
        );
        assert_eq!(
            zero_value["outside_top"]["workflows"][1]["findings"][0]["id"],
            "F2"
        );
        assert!(zero_value["outside_top"]["workflows"][2]["findings"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(
            zero_value["unattached_findings"].as_array().unwrap().len(),
            1
        );
    }

    #[test]
    fn runs_section_stays_after_findings_outside_the_top() {
        let workflows = vec![
            named("example-org/app", ".github/workflows/ci.yml", "3"),
            named("example-org/app", ".github/workflows/docs.yml", "1"),
        ];
        let findings = vec![finding(
            "samples/.github/workflows/docs.yml",
            4,
            FindingKind::MissingTimeout,
            "default timeout is 360 minutes",
        )];
        let api = sample_api();
        let text = render_with_runs(&workflows, &bill("4", "2"), Some(&findings), &api, Some(1));
        let outside = outside_heading(&text);
        let runs = text.find("\nRuns\n").unwrap();
        assert!(outside < runs, "{text}");
        assert!(text.contains("1 more workflow (1.00 USD)\n"), "{text}");
        assert!(
            section(&text, "In progress").contains("601  lint"),
            "{text}"
        );
        assert!(section(&text, "Jobs").contains("501  build"), "{text}");

        let json = render_json_with_runs(
            &workflows,
            &bill("4", "2"),
            Some(&findings),
            None,
            &api,
            Some(1),
            &[],
        );
        let value: serde_json::Value = serde_json::from_str(json.trim_end()).unwrap();
        assert_eq!(value["outside_top"]["count"], 1);
        assert!(value.get("workflow_runs").is_some(), "{value}");
        assert!(value.get("in_progress").is_some(), "{value}");
        assert_eq!(
            value["outside_top"]["workflows"][0]["findings"][0]["line"],
            4
        );
    }

    #[test]
    fn json_steps_follow_the_workflow_file_and_survive_top() {
        let lint = crate::workflow::scan_yaml(
            std::path::Path::new("repo/.github/workflows/lint.yml"),
            "jobs:\n  lint:\n    steps:\n      - name: Lint\n        run: |\n          echo one\n          echo two\n      - uses: actions/checkout@v4\n",
        )
        .unwrap();
        let docs = crate::workflow::scan_yaml(
            std::path::Path::new("repo/.github/workflows/docs.yml"),
            "jobs:\n  docs:\n    steps:\n      - run: cargo doc --no-deps\n",
        )
        .unwrap();
        let scanned = [lint, docs];
        let workflows = vec![
            named("example-org/alpha", ".github/workflows/lint.yml", "10"),
            named("example-org/beta", ".GitHub/Workflows/Lint.YML", "10"),
            named("example-org/app", ".github/workflows/docs.yml", "2"),
            named("example-org/app", "(no workflow)", "0"),
        ];
        let text = render(&workflows, &bill("22", "4"), None, None);
        assert!(!text.contains("echo one"), "{text}");
        assert!(!text.contains("actions/checkout@v4"), "{text}");
        assert!(text.contains("Workflow"), "{text}");
        assert!(!text.contains("steps"), "{text}");

        let json = render_json(&workflows, &bill("22", "4"), None, None, Some(1), &scanned);
        let value: serde_json::Value = serde_json::from_str(json.trim_end()).unwrap();
        let lint_steps = serde_json::json!([
            {"name": "Lint", "line": 4},
            {"name": "actions/checkout@v4", "line": 8}
        ]);
        assert_eq!(value["workflows"][0]["steps"], lint_steps);
        assert_eq!(value["workflows"][0]["path"], ".github/workflows/lint.yml");
        let outside = value["outside_top"]["workflows"].as_array().unwrap();
        assert_eq!(outside[0]["steps"], lint_steps);
        assert_eq!(outside[0]["path"], ".GitHub/Workflows/Lint.YML");
        assert_eq!(
            outside[1]["steps"],
            serde_json::json!([{"name": "cargo doc --no-deps", "line": 4}])
        );
        assert_eq!(outside[1]["path"], ".github/workflows/docs.yml");
        assert_eq!(outside[2]["steps"], serde_json::json!([]));
        assert_eq!(outside[2]["path"], "(no workflow)");

        let slash = named("example-org/app", ".github\\workflows\\docs.yml", "2");
        let slash_json = render_json(&[slash], &bill("2", "1"), None, None, None, &scanned);
        let slash_value: serde_json::Value = serde_json::from_str(slash_json.trim_end()).unwrap();
        assert_eq!(
            slash_value["workflows"][0]["steps"][0]["name"],
            "cargo doc --no-deps"
        );
        assert_eq!(slash_value["workflows"][0]["steps"][0]["line"], 4);

        let bare = render_json(&workflows, &bill("22", "4"), None, None, None, &[]);
        let bare_value: serde_json::Value = serde_json::from_str(bare.trim_end()).unwrap();
        assert_eq!(bare_value["workflows"][0]["steps"], serde_json::json!([]));
        assert!(!bare.contains("echo one"), "{bare}");
    }

    fn sample_job(
        id: u64,
        attempt: u64,
        name: &str,
        started: &str,
        completed: Option<&str>,
    ) -> Job {
        Job {
            id,
            run_id: 1,
            run_attempt: attempt,
            name: name.to_string(),
            labels: vec!["ubuntu-latest".to_string()],
            started_at: Some(started.to_string()),
            completed_at: completed.map(str::to_string),
            conclusion: completed.map(|_| "success".to_string()),
            runner_name: Some("runner-a".to_string()),
        }
    }

    #[test]
    fn linked_path_and_inherited_copy_show_zero_cost() {
        let api = ApiData {
            runs: vec![WorkflowRun {
                id: 1,
                path: "octocat/octo-repo/.github/workflows/ci.yml@refs/heads/feature/foo"
                    .to_string(),
                run_attempt: 2,
                run_number: 4,
                event: "push".to_string(),
                conclusion: Some("success".to_string()),
                run_started_at: Some("2026-04-01T12:00:00Z".to_string()),
            }],
            jobs: vec![
                sample_job(
                    10,
                    1,
                    "build",
                    "2026-04-01T10:00:00Z",
                    Some("2026-04-01T10:05:00Z"),
                ),
                sample_job(
                    11,
                    2,
                    "build",
                    "2026-04-01T10:00:00Z",
                    Some("2026-04-01T10:05:00Z"),
                ),
                sample_job(
                    12,
                    2,
                    "lint",
                    "2026-04-01T12:00:30Z",
                    Some("2026-04-01T12:01:00Z"),
                ),
                sample_job(13, 2, "pack", "2026-04-01T11:00:00Z", None),
            ],
        };
        let text = render_with_runs(&[], &bill("0", "0"), None, &api, None);
        assert!(
            !text.to_ascii_lowercase().contains("not supported"),
            "{text}"
        );
        assert!(!text.to_ascii_lowercase().contains("skipped"), "{text}");
        assert!(!text.contains('@'), "{text}");
        assert!(!text.contains("octocat"), "{text}");
        assert!(!text.contains("refs/heads"), "{text}");
        let runs = section(&text, "Runs");
        assert!(
            runs.contains(
                "1  .github/workflows/ci.yml  attempt 2  number 4  push  success  2026-04-01T12:00:00Z\n"
            ),
            "{runs}"
        );
        let jobs = section(&text, "Jobs");
        assert!(
            jobs.contains(
                "10  build  run 1  .github/workflows/ci.yml  attempt 1  labels ubuntu-latest  runner runner-a  started 2026-04-01T10:00:00Z  completed 2026-04-01T10:05:00Z  success\n"
            ),
            "{jobs}"
        );
        assert!(
            jobs.contains(
                "11  build  run 1  .github/workflows/ci.yml  attempt 2  labels ubuntu-latest  runner runner-a  started 2026-04-01T10:00:00Z  completed 2026-04-01T10:05:00Z  success  inherited copy  cost 0.00\n"
            ),
            "{jobs}"
        );
        assert!(
            jobs.contains(
                "12  lint  run 1  .github/workflows/ci.yml  attempt 2  labels ubuntu-latest  runner runner-a  started 2026-04-01T12:00:30Z  completed 2026-04-01T12:01:00Z  success\n"
            ),
            "{jobs}"
        );
        assert!(!jobs.contains("13  pack"), "{jobs}");
        let running = section(&text, "In progress");
        assert_eq!(
            running,
            "13  pack  run 1  .github/workflows/ci.yml  attempt 2  labels ubuntu-latest  runner runner-a  started 2026-04-01T11:00:00Z  in progress  inherited copy  cost 0.00\n"
        );

        let json = render_json_with_runs(&[], &bill("0", "0"), None, None, &api, None, &[]);
        assert!(
            !json.to_ascii_lowercase().contains("not supported"),
            "{json}"
        );
        assert!(!json.to_ascii_lowercase().contains("skipped"), "{json}");
        let value: serde_json::Value = serde_json::from_str(json.trim_end()).unwrap();
        assert_eq!(
            value["workflow_runs"][0]["path"],
            ".github/workflows/ci.yml"
        );
        let listed = value["workflow_runs"][0]["jobs"].as_array().unwrap();
        assert_eq!(listed.len(), 3);
        let by_id = |id: u64| {
            listed
                .iter()
                .find(|job| job["id"].as_u64() == Some(id))
                .unwrap()
        };
        assert_eq!(by_id(10)["inherited_copy"], false);
        assert_eq!(by_id(10)["billable"], true);
        assert_eq!(by_id(10)["counts"], true);
        assert!(by_id(10).get("net_amount").is_none());
        assert_eq!(by_id(11)["inherited_copy"], true);
        assert_eq!(by_id(11)["billable"], false);
        assert_eq!(by_id(11)["counts"], true);
        assert_eq!(by_id(11)["net_amount"], "0.000000");
        assert_eq!(by_id(12)["billable"], true);
        assert!(by_id(12).get("net_amount").is_none());
        let progress = &value["in_progress"][0];
        assert_eq!(progress["id"], 13);
        assert_eq!(progress["workflow_path"], ".github/workflows/ci.yml");
        assert_eq!(progress["inherited_copy"], true);
        assert_eq!(progress["billable"], false);
        assert_eq!(progress["counts"], false);
        assert_eq!(progress["message"], "in progress");
        assert_eq!(progress["net_amount"], "0.000000");
    }
}
