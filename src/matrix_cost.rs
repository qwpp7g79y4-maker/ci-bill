//! Matrix rounding loss from saved Actions jobs.
//!
//! Each run's jobs are matched to a `strategy.matrix` by the job-name prefix
//! `job (` or by a `name:` template. Billed minutes are the whole minutes
//! already charged for the job. Raw minutes are the elapsed time. The
//! difference, priced at that runner's SKU rate, is the rounding loss.
//! F4 then states the executed count, the runner rate, and that loss.
//! Jobs that match neither a prefix nor a template still contribute their
//! minutes and cost on the `matrix:` line.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use crate::api::{parse_github_time, workflow_paths_match, ApiData, Job as ApiJob};
use crate::findings::{Finding, FindingKind, SavingsMethod};
use crate::job_cost::bill_job;
use crate::money::Micros;
use crate::rank::WorkflowTotal;
use crate::workflow::{Matrix, ReusableSource, Workflow};

const NS_PER_MIN: i128 = 60 * 1_000_000_000;

/// F4 findings measured from `api`, one per matrix that executed at least one job.
///
/// Counts are taken per run, then added. An inherited copy, a job that is
/// still in progress, and a job whose conclusion is `skipped` add nothing.
/// `workflows` supplies the usage-report path and repository when a run links
/// to exactly one of them.
pub fn findings(api: &ApiData, workflows: &[WorkflowTotal], scanned: &[Workflow]) -> Vec<Finding> {
    let mut accs: Vec<Acc> = Vec::new();
    for run in &api.runs {
        let Some(root) = source_for_run(scanned, &run.path) else {
            continue;
        };
        let mut matrix_sites = Vec::new();
        let mut plain_sites = Vec::new();
        let mut stack = Vec::new();
        collect(
            root,
            scanned,
            0,
            &mut stack,
            &mut matrix_sites,
            &mut plain_sites,
        );
        if matrix_sites.is_empty() {
            continue;
        }
        let (workflow_path, repository) =
            placement(workflows, &run.path, run.repository.as_deref());
        let jobs: Vec<&ApiJob> = api
            .jobs
            .iter()
            .filter(|job| job.run_id == run.id && executed(api, job))
            .collect();
        let mut pending = Vec::new();
        for job in jobs {
            if let Some(index) = best_match(&matrix_sites, &job.name) {
                let slot = acc_index(&mut accs, &matrix_sites[index], &workflow_path, &repository);
                accs[slot].add(api, job);
            } else if best_match(&plain_sites, &job.name).is_none() {
                pending.push(job);
            }
        }
        for job in pending {
            if let Some(index) = fallback_index(&matrix_sites, job) {
                let slot = acc_index(&mut accs, &matrix_sites[index], &workflow_path, &repository);
                accs[slot].add(api, job);
            }
        }
    }
    let mut found: Vec<Finding> = accs
        .into_iter()
        .filter(|acc| acc.n > 0)
        .map(Acc::into_finding)
        .collect();
    found.sort_by(|left, right| {
        (
            left.path.as_path(),
            left.line,
            left.workflow_path.as_deref(),
            left.repository.as_deref(),
            left.message.as_str(),
        )
            .cmp(&(
                right.path.as_path(),
                right.line,
                right.workflow_path.as_deref(),
                right.repository.as_deref(),
                right.message.as_str(),
            ))
    });
    found
}

/// Drop YAML matrix estimates that `measured` replaces.
///
/// A measured finding and an estimate share a file and a `matrix:` line.
/// The measured text is the one the report keeps.
pub fn supersede(existing: &mut Vec<Finding>, measured: &[Finding]) {
    if measured.is_empty() {
        return;
    }
    existing.retain(|finding| {
        finding.kind != FindingKind::MatrixJobs
            || !measured
                .iter()
                .any(|item| item.path == finding.path && item.line == finding.line)
    });
}

struct Site {
    path: PathBuf,
    line: usize,
    key: String,
    display: Option<String>,
    axis_values: Vec<String>,
    depth: u8,
}

struct Acc {
    path: PathBuf,
    line: usize,
    workflow_path: String,
    repository: Option<String>,
    n: u64,
    runs: HashSet<u64>,
    jobs: HashSet<u64>,
    groups: BTreeMap<String, Group>,
}

struct Group {
    sku: Option<String>,
    rate: Option<Micros>,
    billed: u64,
    raw_ns: i128,
}

struct Pick {
    index: usize,
    depth_rank: u8,
    len: usize,
}

impl Acc {
    fn new(site: &Site, workflow_path: &str, repository: &Option<String>) -> Self {
        Self {
            path: site.path.clone(),
            line: site.line,
            workflow_path: workflow_path.to_string(),
            repository: repository.clone(),
            n: 0,
            runs: HashSet::new(),
            jobs: HashSet::new(),
            groups: BTreeMap::new(),
        }
    }

    fn add(&mut self, api: &ApiData, job: &ApiJob) {
        if !self.jobs.insert(job.id) {
            return;
        }
        self.runs.insert(job.run_id);
        self.n = self.n.saturating_add(1);
        let bill = bill_job(api, job);
        let key = bill.sku.clone().unwrap_or_default();
        let group = self.groups.entry(key).or_insert_with(|| Group {
            sku: bill.sku.clone(),
            rate: bill.usd_per_minute,
            billed: 0,
            raw_ns: 0,
        });
        group.billed = group.billed.saturating_add(bill.minutes);
        group.raw_ns = group.raw_ns.saturating_add(raw_nanos(job));
    }

    fn into_finding(self) -> Finding {
        let runs = self.runs.len();
        let groups = ordered_groups(&self.groups);
        let savings = groups.iter().fold(Micros::ZERO, |total, group| {
            Micros(total.0.saturating_add(group.loss_usd().0))
        });
        Finding {
            path: self.path,
            line: self.line,
            kind: FindingKind::MatrixJobs,
            message: message(self.n, runs, &groups),
            repository: self.repository,
            workflow_path: Some(self.workflow_path),
            savings,
            method: SavingsMethod::Rounding,
            anchor: None,
        }
    }
}

impl Group {
    fn loss_ns(&self) -> i128 {
        let billed = i128::from(self.billed).saturating_mul(NS_PER_MIN);
        billed.saturating_sub(self.raw_ns).max(0)
    }

    fn loss_usd(&self) -> Micros {
        match self.rate {
            Some(rate) => nanos_times_rate(self.loss_ns(), rate),
            None => Micros::ZERO,
        }
    }

    fn clause(&self) -> String {
        let billed = self.billed.to_string();
        let raw = format_minutes(self.raw_ns);
        let loss = format_minutes(self.loss_ns());
        let usd = format_usd(self.loss_usd());
        let head = format!(
            "{billed} billed {} vs {raw} raw {}; rounding loss {loss} {}",
            minute_word(&billed),
            minute_word(&raw),
            minute_word(&loss),
        );
        match self.rate {
            Some(rate) => format!("{head} at {} USD/min ({usd} USD)", format_rate(rate)),
            None => format!("{head} ({usd} USD)"),
        }
    }
}

fn ordered_groups(groups: &BTreeMap<String, Group>) -> Vec<&Group> {
    let mut items: Vec<&Group> = groups.values().collect();
    items.sort_by(|left, right| match (&left.sku, &right.sku) {
        (Some(left_sku), Some(right_sku)) => left_sku.cmp(right_sku),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    items
}

fn message(n: u64, runs: usize, groups: &[&Group]) -> String {
    let jobs = if n == 1 { "job" } else { "jobs" };
    let mut text = format!("matrix runs {n} {jobs}");
    if runs > 1 {
        text.push_str(&format!(" across {runs} runs"));
    }
    for group in groups {
        text.push_str("; ");
        text.push_str(&group.clause());
    }
    text
}

fn minute_word(shown: &str) -> &'static str {
    if shown == "1" {
        "minute"
    } else {
        "minutes"
    }
}

fn format_minutes(ns: i128) -> String {
    if ns <= 0 {
        return "0".to_string();
    }
    let whole = ns / NS_PER_MIN;
    let rem = ns % NS_PER_MIN;
    if rem == 0 {
        return whole.to_string();
    }
    let thousandths = (rem * 1_000 + NS_PER_MIN / 2) / NS_PER_MIN;
    if thousandths <= 0 {
        return whole.to_string();
    }
    if thousandths >= 1_000 {
        return (whole + 1).to_string();
    }
    let digits = format!("{thousandths:03}");
    let trimmed = digits.trim_end_matches('0');
    format!("{whole}.{trimmed}")
}

fn format_usd(amount: Micros) -> String {
    if amount.0 == 0 {
        return "0.00".to_string();
    }
    let text = amount.to_decimal_string();
    let trimmed = text.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() {
        "0.00".to_string()
    } else if let Some(rest) = trimmed.strip_prefix('.') {
        format!("0.{rest}")
    } else {
        trimmed.to_string()
    }
}

/// Runner rates in the price table use three decimal places.
fn format_rate(rate: Micros) -> String {
    let abs = rate.0.unsigned_abs();
    let mut thousandths = abs / 1_000;
    if abs % 1_000 >= 500 {
        thousandths = thousandths.saturating_add(1);
    }
    let whole = thousandths / 1_000;
    let frac = thousandths % 1_000;
    format!("{whole}.{frac:03}")
}

fn nanos_times_rate(loss_ns: i128, rate: Micros) -> Micros {
    if loss_ns <= 0 || rate.0 <= 0 {
        return Micros::ZERO;
    }
    let product = loss_ns.saturating_mul(i128::from(rate.0));
    let div = product.div_euclid(NS_PER_MIN);
    let rem = product.rem_euclid(NS_PER_MIN);
    let rounded = if rem.saturating_mul(2) >= NS_PER_MIN {
        div.saturating_add(1)
    } else {
        div
    };
    Micros(i64::try_from(rounded).unwrap_or(i64::MAX))
}

fn raw_nanos(job: &ApiJob) -> i128 {
    let start = job.started_at.as_deref().and_then(parse_github_time);
    let end = job.completed_at.as_deref().and_then(parse_github_time);
    match (start, end) {
        (Some(start), Some(end)) if end > start => end - start,
        _ => 0,
    }
}

fn executed(api: &ApiData, job: &ApiJob) -> bool {
    if api.is_inherited_copy(job) || !job.counts() {
        return false;
    }
    !job.conclusion
        .as_deref()
        .is_some_and(|raw| raw.trim().eq_ignore_ascii_case("skipped"))
}

fn acc_index(
    accs: &mut Vec<Acc>,
    site: &Site,
    workflow_path: &str,
    repository: &Option<String>,
) -> usize {
    if let Some(index) = accs.iter().position(|acc| {
        acc.path == site.path
            && acc.line == site.line
            && acc.workflow_path == workflow_path
            && acc.repository == *repository
    }) {
        return index;
    }
    accs.push(Acc::new(site, workflow_path, repository));
    accs.len() - 1
}

fn placement(
    workflows: &[WorkflowTotal],
    path: &str,
    repo: Option<&str>,
) -> (String, Option<String>) {
    if let Some(row) = match_ranked(workflows, path, repo) {
        return (row.workflow_path.clone(), Some(row.repository.clone()));
    }
    let linked = crate::api::linked_workflow_path(path);
    (linked, repo.map(str::to_string))
}

fn match_ranked<'a>(
    workflows: &'a [WorkflowTotal],
    path: &str,
    repo: Option<&str>,
) -> Option<&'a WorkflowTotal> {
    let mut hits = Vec::new();
    for workflow in workflows {
        if !workflow_paths_match(&workflow.workflow_path, path) {
            continue;
        }
        if let Some(repo) = repo {
            if workflow.repository.eq_ignore_ascii_case(repo) {
                hits.push(workflow);
            }
        } else {
            hits.push(workflow);
        }
    }
    if hits.len() == 1 {
        Some(hits[0])
    } else {
        None
    }
}

fn source_for_run<'a>(scanned: &'a [Workflow], run_path: &str) -> Option<&'a Workflow> {
    scanned.iter().find(|workflow| {
        workflow
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                let key = format!(".github/workflows/{name}");
                workflow_paths_match(run_path, &key)
            })
    })
}

fn collect(
    workflow: &Workflow,
    scanned: &[Workflow],
    depth: u8,
    stack: &mut Vec<PathBuf>,
    matrix_sites: &mut Vec<Site>,
    plain_sites: &mut Vec<Site>,
) {
    if stack.iter().any(|path| path == &workflow.path) {
        return;
    }
    stack.push(workflow.path.clone());
    for job in &workflow.jobs {
        if let Some(matrix) = &job.matrix {
            matrix_sites.push(Site {
                path: workflow.path.clone(),
                line: matrix.line,
                key: job.name.clone(),
                display: job.display_name.clone(),
                axis_values: axis_values(matrix),
                depth,
            });
        } else {
            plain_sites.push(Site {
                path: workflow.path.clone(),
                line: job.line,
                key: job.name.clone(),
                display: job.display_name.clone(),
                axis_values: Vec::new(),
                depth,
            });
        }
        if let Some(call) = &job.uses {
            if matches!(call.source, ReusableSource::Local) {
                if let Some(callee) = find_workflow(scanned, &call.file_name) {
                    collect(
                        callee,
                        scanned,
                        depth.saturating_add(1),
                        stack,
                        matrix_sites,
                        plain_sites,
                    );
                }
            }
        }
    }
    stack.pop();
}

fn find_workflow<'a>(scanned: &'a [Workflow], file_name: &str) -> Option<&'a Workflow> {
    scanned.iter().find(|workflow| {
        workflow
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(file_name))
    })
}

fn axis_values(matrix: &Matrix) -> Vec<String> {
    let mut values = Vec::new();
    for axis in &matrix.axes {
        for entry in &axis.values {
            for label in entry {
                let text = label.trim().trim_matches('\r').to_ascii_lowercase();
                if !text.is_empty() {
                    values.push(text);
                }
            }
        }
    }
    values
}

fn best_match(sites: &[Site], api_name: &str) -> Option<usize> {
    let mut prefix: Option<Pick> = None;
    let mut template: Option<Pick> = None;
    let mut exact: Option<Pick> = None;
    for (index, site) in sites.iter().enumerate() {
        if let Some(display) = site.display.as_deref() {
            if expression_literals(display).is_none() && has_job_prefix(api_name, display) {
                consider(&mut prefix, index, site.depth, display.trim().len());
            }
        }
        if has_job_prefix(api_name, &site.key) {
            consider(&mut prefix, index, site.depth, site.key.trim().len());
        }
        if template_matches(site, api_name) {
            let len = site
                .display
                .as_deref()
                .and_then(expression_literals)
                .map(|literals| literals.iter().map(String::len).sum())
                .unwrap_or(0);
            consider(&mut template, index, site.depth, len);
        } else if let Some(display) = site.display.as_deref() {
            if expression_literals(display).is_none() && same_name(api_name, display) {
                consider(&mut exact, index, site.depth, display.trim().len());
            }
        }
        if same_name(api_name, &site.key) {
            consider(&mut exact, index, site.depth, site.key.trim().len());
        }
    }
    prefix.or(template).or(exact).map(|pick| pick.index)
}

fn consider(best: &mut Option<Pick>, index: usize, depth: u8, len: usize) {
    let candidate = Pick {
        index,
        depth_rank: u8::MAX - depth,
        len,
    };
    if best.as_ref().is_none_or(|current| {
        (candidate.depth_rank, candidate.len) > (current.depth_rank, current.len)
    }) {
        *best = Some(candidate);
    }
}

fn template_matches(site: &Site, api_name: &str) -> bool {
    let Some(display) = site.display.as_deref() else {
        return false;
    };
    let Some(literals) = expression_literals(display) else {
        return false;
    };
    if literals.iter().all(|part| part.trim().is_empty()) {
        let name = api_name.trim().trim_matches('\r').to_ascii_lowercase();
        return !name.is_empty() && site.axis_values.iter().any(|value| value == &name);
    }
    literals_match(&literals, api_name)
}

fn fallback_index(sites: &[Site], job: &ApiJob) -> Option<usize> {
    if sites.is_empty() {
        return None;
    }
    if sites.len() == 1 {
        return Some(0);
    }
    let labels = labels_of(job);
    let mut hits = Vec::new();
    for (index, site) in sites.iter().enumerate() {
        if labels
            .iter()
            .any(|label| site.axis_values.iter().any(|value| value == label))
        {
            hits.push(index);
        }
    }
    if hits.len() == 1 {
        Some(hits[0])
    } else {
        Some(0)
    }
}

fn labels_of(job: &ApiJob) -> Vec<String> {
    job.labels
        .iter()
        .map(|label| label.trim().trim_matches('\r').to_ascii_lowercase())
        .filter(|label| !label.is_empty())
        .collect()
}

fn has_job_prefix(api_name: &str, label: &str) -> bool {
    let label = label.trim().trim_matches('\r');
    if label.is_empty() {
        return false;
    }
    let api_name = api_name.trim().trim_matches('\r').to_ascii_lowercase();
    let label = label.to_ascii_lowercase();
    api_name
        .strip_prefix(&label)
        .is_some_and(|rest| rest.starts_with(" ("))
}

fn same_name(left: &str, right: &str) -> bool {
    left.trim()
        .trim_matches('\r')
        .eq_ignore_ascii_case(right.trim().trim_matches('\r'))
}

fn expression_literals(template: &str) -> Option<Vec<String>> {
    let template = template.replace('\r', "");
    if !template.contains("${{") {
        return None;
    }
    let mut literals = Vec::new();
    let mut rest = template.as_str();
    while let Some(start) = rest.find("${{") {
        literals.push(rest[..start].to_string());
        rest = &rest[start + 3..];
        let end = rest.find("}}")?;
        rest = &rest[end + 2..];
    }
    literals.push(rest.to_string());
    Some(literals)
}

fn literals_match(literals: &[String], api_name: &str) -> bool {
    if literals.iter().all(|part| part.is_empty()) {
        return false;
    }
    let api_name = api_name.trim().trim_matches('\r').to_ascii_lowercase();
    let parts: Vec<String> = literals
        .iter()
        .map(|part| part.to_ascii_lowercase())
        .collect();
    let mut rest = api_name.as_str();
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        let first = index == 0;
        let last = index + 1 == parts.len();
        if first {
            if !rest.starts_with(part.as_str()) {
                return false;
            }
            rest = &rest[part.len()..];
        } else if last {
            if !rest.ends_with(part.as_str()) {
                return false;
            }
        } else if let Some(pos) = rest.find(part.as_str()) {
            rest = &rest[pos + part.len()..];
        } else {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{ApiData, Job, WorkflowRun};
    use crate::findings::findings as yaml_findings;
    use crate::money::Micros;
    use crate::rank::rank;
    use crate::report::render_text;
    use crate::usage::UsageRow;
    use crate::workflow::scan_yaml;
    use std::path::Path;

    const PATH: &str = ".github/workflows/ci.yml";
    const REPO: &str = "acme/widgets";
    const LINUX_6: &str = "matrix runs 6 jobs; 12 billed minutes vs 7 raw minutes; rounding loss 5 minutes at 0.006 USD/min (0.03 USD)";
    const MACOS_6: &str = "matrix runs 6 jobs; 12 billed minutes vs 7 raw minutes; rounding loss 5 minutes at 0.062 USD/min (0.31 USD)";

    const YAML: &str = "\
name: CI
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    strategy:
      matrix:
        os: [ubuntu-latest, windows-latest]
        node: [14, 16, 18]
    steps:
      - run: echo build
";

    fn crlf(text: &str) -> String {
        text.replace('\n', "\r\n")
    }

    fn scanned(text: &str) -> Vec<Workflow> {
        vec![scan_yaml(Path::new("repo/.github/workflows/ci.yml"), text).unwrap()]
    }

    fn matrix_line(text: &str) -> usize {
        scanned(text).remove(0).jobs[0]
            .matrix
            .as_ref()
            .unwrap()
            .line
    }

    fn job_line(text: &str) -> usize {
        scanned(text).remove(0).jobs[0].line
    }

    fn run_record(id: u64, path: &str) -> WorkflowRun {
        WorkflowRun {
            id,
            path: path.to_string(),
            repository: Some(REPO.to_string()),
            run_attempt: 1,
            run_number: id,
            event: "push".to_string(),
            conclusion: Some("success".to_string()),
            run_started_at: Some("2026-04-01T00:00:00Z".to_string()),
            head_branch: None,
            pull_requests: Vec::new(),
        }
    }

    fn job(
        id: u64,
        run_id: u64,
        name: &str,
        labels: &[&str],
        started: &str,
        completed: &str,
        conclusion: Option<&str>,
    ) -> Job {
        Job {
            id,
            run_id,
            run_attempt: 1,
            name: name.to_string(),
            labels: labels.iter().map(|label| (*label).to_string()).collect(),
            started_at: Some(started.to_string()),
            completed_at: Some(completed.to_string()),
            conclusion: conclusion.map(str::to_string),
            runner_name: None,
        }
    }

    fn seventy(id: u64, name: &str, labels: &[&str]) -> Job {
        job(
            id,
            1,
            name,
            labels,
            "2026-04-01T00:00:00Z",
            "2026-04-01T00:01:10Z",
            Some("success"),
        )
    }

    fn prefix_name(index: usize) -> String {
        let os = if index < 3 {
            "ubuntu-latest"
        } else {
            "windows-latest"
        };
        let node = match index % 3 {
            0 => "14",
            1 => "16",
            _ => "18",
        };
        format!("build ({os}, {node})")
    }

    fn six(names: &[String], labels: &[&str]) -> Vec<Job> {
        names
            .iter()
            .enumerate()
            .map(|(index, name)| seventy((index + 1) as u64, name, labels))
            .collect()
    }

    fn measure(text: &str, jobs: Vec<Job>) -> Vec<Finding> {
        measure_runs(
            text,
            vec![run_record(1, "acme/widgets/.github/workflows/ci.yml@main")],
            jobs,
        )
    }

    fn measure_runs(text: &str, runs: Vec<WorkflowRun>, jobs: Vec<Job>) -> Vec<Finding> {
        let api = ApiData {
            public: false,
            runs,
            jobs,
        };
        let scanned = scanned(text);
        let ranking = rank(&[usage_row()], None);
        let mut yaml = yaml_findings(&scanned).unwrap();
        let measured = findings(&api, &ranking.workflows, &scanned);
        supersede(&mut yaml, &measured);
        let mut found = yaml;
        found.extend(measured);
        found.retain(|finding| finding.kind == FindingKind::MatrixJobs);
        found
    }

    fn usage_row() -> UsageRow {
        UsageRow {
            line: 2,
            date: "2026-04-01".to_string(),
            product: "actions".to_string(),
            sku: "actions_linux".to_string(),
            quantity: Micros(6 * Micros::SCALE),
            unit_type: "minutes".to_string(),
            applied_cost_per_quantity: Micros::parse("0.006").unwrap(),
            gross: Micros::parse("0.036").unwrap(),
            discount: Micros::ZERO,
            net: Micros::parse("0.036").unwrap(),
            username: "dev".to_string(),
            organization: "acme".to_string(),
            repository: REPO.to_string(),
            workflow_path: PATH.to_string(),
        }
    }

    fn only_message(found: &[Finding]) -> &str {
        assert_eq!(found.len(), 1, "{found:?}");
        found[0].message.as_str()
    }

    fn assert_public(text: &str) {
        let lower = text.to_ascii_lowercase();
        assert!(!lower.contains("not supported"), "{text}");
        assert!(!lower.contains("limitation"), "{text}");
        assert!(!lower.contains("skipped"), "{text}");
    }

    #[test]
    fn six_jobs_of_seventy_seconds_lose_five_minutes_at_the_linux_rate() {
        let names: Vec<String> = (0..6).map(prefix_name).collect();
        let found = measure(YAML, six(&names, &["ubuntu-latest"]));
        assert_eq!(only_message(&found), LINUX_6);
        assert_eq!(found[0].line, matrix_line(YAML));
        assert_ne!(found[0].line, job_line(YAML));
        assert_eq!(found[0].kind.code(), "F4");
        assert_eq!(found[0].savings, Micros::parse("0.030").unwrap());
        assert_eq!(found[0].method, crate::findings::SavingsMethod::Rounding);
        assert_eq!(found[0].workflow_path.as_deref(), Some(PATH));
        assert_eq!(found[0].repository.as_deref(), Some(REPO));
        assert!(found[0].path.ends_with("ci.yml"));
        assert_public(&found[0].message);
        assert!(!found[0].message.contains("at least"));
        assert!(!found[0].message.contains("each job is billed"));
    }

    #[test]
    fn job_order_and_conclusion_case_do_not_change_the_loss() {
        let mut names: Vec<String> = (0..6).map(prefix_name).collect();
        names.reverse();
        let mut jobs = six(&names, &["ubuntu-latest"]);
        jobs.reverse();
        for (index, job) in jobs.iter_mut().enumerate() {
            job.conclusion = Some(if index % 2 == 0 {
                "SUCCESS".to_string()
            } else {
                "failure".to_string()
            });
            job.id = 100 - index as u64;
        }
        let found = measure(YAML, jobs);
        assert_eq!(only_message(&found), LINUX_6);
    }

    #[test]
    fn capital_prefixes_and_labels_use_the_linux_rate() {
        let names: Vec<String> = (0..6)
            .map(|index| prefix_name(index).to_ascii_uppercase())
            .collect();
        let found = measure(YAML, six(&names, &["UBUNTU-LATEST"]));
        assert_eq!(only_message(&found), LINUX_6);
        let spaced = measure(YAML, six(&names, &[" ubuntu-22.04 "]));
        assert_eq!(only_message(&spaced), LINUX_6);
    }

    #[test]
    fn crlf_yaml_and_fields_keep_the_matrix_line_and_the_loss() {
        let yaml = crlf(YAML);
        assert_eq!(matrix_line(&yaml), matrix_line(YAML));
        let names: Vec<String> = (0..6)
            .map(|index| format!("{}\r", prefix_name(index)))
            .collect();
        let mut jobs = six(&names, &["ubuntu-latest\r"]);
        for job in &mut jobs {
            job.started_at = Some("2026-04-01T00:00:00Z\r".to_string());
            job.completed_at = Some("2026-04-01T00:01:10Z\r".to_string());
        }
        let found = measure(&yaml, jobs);
        assert_eq!(only_message(&found), LINUX_6);
        assert_eq!(found[0].line, matrix_line(YAML));
    }

    #[test]
    fn reordered_yaml_keys_still_point_at_the_matrix_line() {
        let yaml = "\
jobs:
  build:
    strategy:
      matrix:
        node: [14, 16, 18]
        os: [ubuntu-latest, windows-latest]
    timeout-minutes: 10
    runs-on: ubuntu-latest
";
        let names: Vec<String> = (0..6).map(prefix_name).collect();
        let found = measure(yaml, six(&names, &["ubuntu-latest"]));
        assert_eq!(only_message(&found), LINUX_6);
        assert_eq!(found[0].line, matrix_line(yaml));
        assert_ne!(found[0].line, job_line(yaml));
    }

    #[test]
    fn a_macos_matrix_shows_the_macos_rate() {
        let names: Vec<String> = (0..6).map(prefix_name).collect();
        let found = measure(YAML, six(&names, &["macos-latest"]));
        assert_eq!(only_message(&found), MACOS_6);
        assert!(found[0].message.contains("0.062 USD/min"));
        assert!(found[0].message.contains("(0.31 USD)"));
        let capitals = measure(YAML, six(&names, &["MACOS-LATEST"]));
        assert_eq!(only_message(&capitals), MACOS_6);
        let image = measure(YAML, six(&names, &["macos-14"]));
        assert_eq!(only_message(&image), MACOS_6);
    }

    #[test]
    fn larger_and_other_runners_use_their_own_rates() {
        let names: Vec<String> = (0..6).map(prefix_name).collect();
        let cases = [
            (vec!["macos-14-large"], "0.077 USD/min", "(0.385 USD)"),
            (vec!["MACOS-15-XLARGE"], "0.102 USD/min", "(0.51 USD)"),
            (vec!["windows-latest"], "0.010 USD/min", "(0.05 USD)"),
            (vec!["WINDOWS-LATEST"], "0.010 USD/min", "(0.05 USD)"),
            (vec!["windows-11-arm"], "0.010 USD/min", "(0.05 USD)"),
            (vec!["ubuntu-24.04-16core"], "0.042 USD/min", "(0.21 USD)"),
            (
                vec!["self-hosted", "macos-latest"],
                "0.000 USD/min",
                "(0.00 USD)",
            ),
        ];
        for (labels, rate, loss) in cases {
            let found = measure(YAML, six(&names, &labels));
            let message = only_message(&found);
            assert!(
                message.contains("12 billed minutes vs 7 raw minutes"),
                "{message}"
            );
            assert!(message.contains("rounding loss 5 minutes"), "{message}");
            assert!(message.contains(rate), "{labels:?} {message}");
            assert!(message.contains(loss), "{labels:?} {message}");
        }
    }

    #[test]
    fn jobs_without_a_prefix_are_reported_on_the_matrix_line() {
        let names: Vec<String> = (0..6).map(|index| format!("cell-{index}")).collect();
        let found = measure(YAML, six(&names, &["ubuntu-latest"]));
        assert_eq!(only_message(&found), LINUX_6);
        assert_eq!(found[0].line, matrix_line(YAML));
        assert_ne!(found[0].line, job_line(YAML));
        let blanks: Vec<String> = vec!["   ".to_string(); 6];
        let blank = measure(YAML, six(&blanks, &["ubuntu-latest"]));
        assert_eq!(only_message(&blank), LINUX_6);
        assert_eq!(blank[0].line, matrix_line(YAML));
    }

    #[test]
    fn a_name_template_counts_the_expanded_jobs() {
        let yaml = "\
name: CI
on: push
jobs:
  build:
    name: Build ${{ matrix.os }}
    runs-on: ${{ matrix.os }}
    timeout-minutes: 10
    strategy:
      matrix:
        os: [ubuntu-latest, windows-latest]
        node: [14, 16, 18]
";
        let names = [
            "Build ubuntu-latest",
            "Build windows-latest",
            "Build macos-latest",
            "BUILD UBUNTU-LATEST",
            "Build  ubuntu-22.04",
            "Build linux",
        ];
        let owned: Vec<String> = names.iter().map(|name| (*name).to_string()).collect();
        let found = measure(yaml, six(&owned, &["ubuntu-latest"]));
        assert_eq!(only_message(&found), LINUX_6);
        assert_eq!(found[0].line, matrix_line(yaml));

        let quoted = yaml.replace(
            "name: Build ${{ matrix.os }}",
            "name: \"Build ${{ matrix.os }}\"",
        );
        let quoted_found = measure(&quoted, six(&owned, &["ubuntu-latest"]));
        assert_eq!(only_message(&quoted_found), LINUX_6);

        let only_expr = "\
jobs:
  build:
    name: ${{ matrix.os }}
    timeout-minutes: 10
    strategy:
      matrix:
        os: [ubuntu-latest, windows-latest]
        node: [14, 16, 18]
";
        let values = [
            "ubuntu-latest",
            "UBUNTU-LATEST",
            "windows-latest",
            "ubuntu-latest",
            "windows-latest",
            "ubuntu-22.04",
        ];
        let owned: Vec<String> = values.iter().map(|name| (*name).to_string()).collect();
        let found = measure(only_expr, six(&owned, &["ubuntu-latest"]));
        assert_eq!(only_message(&found), LINUX_6);
        assert_eq!(found[0].line, matrix_line(only_expr));
    }

    #[test]
    fn missing_optional_fields_do_not_change_six_finished_jobs() {
        let names: Vec<String> = (0..6).map(prefix_name).collect();
        let mut jobs = six(&names, &["ubuntu-latest"]);
        for job in &mut jobs {
            job.conclusion = None;
            job.runner_name = None;
        }
        let mut running = seventy(20, "build (ubuntu-latest, 20)", &["ubuntu-latest"]);
        running.completed_at = None;
        running.conclusion = None;
        jobs.push(running);
        let mut blank_end = seventy(21, "build (ubuntu-latest, 21)", &["macos-latest"]);
        blank_end.completed_at = Some("  \r".to_string());
        jobs.push(blank_end);
        let mut skipped = seventy(22, "build (macos-latest, 14)", &["macos-latest"]);
        skipped.conclusion = Some(" Skipped\r".to_string());
        jobs.push(skipped);
        let mut copied = seventy(23, "build (ubuntu-latest, 14)", &["ubuntu-latest"]);
        copied.run_attempt = 1;
        copied.started_at = Some("2026-03-01T00:00:00Z".to_string());
        copied.completed_at = Some("2026-03-01T00:01:10Z".to_string());
        // Same attempt as the run, and started before the run: an inherited copy.
        // The six finished jobs start when the run starts, so they stay billable.
        jobs.push(copied);
        let found = measure(YAML, jobs);
        assert_eq!(only_message(&found), LINUX_6);
    }

    #[test]
    fn empty_labels_still_report_minutes_and_cost_on_the_matrix_line() {
        let names: Vec<String> = (0..6).map(|index| format!("cell-{index}")).collect();
        let found = measure(YAML, six(&names, &[]));
        assert_eq!(
            only_message(&found),
            "matrix runs 6 jobs; 12 billed minutes vs 7 raw minutes; rounding loss 5 minutes (0.00 USD)"
        );
        assert_eq!(found[0].line, matrix_line(YAML));
        assert!(!found[0].message.contains("USD/min"));
    }

    #[test]
    fn a_missing_start_bills_one_minute_of_loss() {
        let mut job = seventy(1, "build (ubuntu-latest, 14)", &["ubuntu-latest"]);
        job.started_at = None;
        let found = measure(YAML, vec![job]);
        assert_eq!(
            only_message(&found),
            "matrix runs 1 job; 1 billed minute vs 0 raw minutes; rounding loss 1 minute at 0.006 USD/min (0.006 USD)"
        );
    }

    #[test]
    fn exact_minutes_have_no_rounding_loss_and_sixty_one_seconds_do() {
        let names: Vec<String> = (0..6).map(prefix_name).collect();
        let mut exact = six(&names, &["ubuntu-latest"]);
        for job in &mut exact {
            job.completed_at = Some("2026-04-01T00:01:00Z".to_string());
        }
        let found = measure(YAML, exact);
        assert_eq!(
            only_message(&found),
            "matrix runs 6 jobs; 6 billed minutes vs 6 raw minutes; rounding loss 0 minutes at 0.006 USD/min (0.00 USD)"
        );
        let mut partial = six(&names, &["ubuntu-latest"]);
        for job in &mut partial {
            job.completed_at = Some("2026-04-01T00:01:01Z".to_string());
        }
        let found = measure(YAML, partial);
        assert_eq!(
            only_message(&found),
            "matrix runs 6 jobs; 12 billed minutes vs 6.1 raw minutes; rounding loss 5.9 minutes at 0.006 USD/min (0.0354 USD)"
        );
    }

    #[test]
    fn cancelled_jobs_of_seventy_seconds_round_the_same_way() {
        let names: Vec<String> = (0..6).map(prefix_name).collect();
        for conclusion in ["cancelled", "Cancelled", "CANCELLED", " cancelled\r"] {
            let mut jobs = six(&names, &["ubuntu-latest"]);
            for job in &mut jobs {
                job.conclusion = Some(conclusion.to_string());
            }
            let found = measure(YAML, jobs);
            assert_eq!(only_message(&found), LINUX_6, "{conclusion}");
        }
    }

    #[test]
    fn two_runs_add_their_executed_jobs() {
        let names: Vec<String> = (0..6).map(prefix_name).collect();
        let mut first = six(&names, &["ubuntu-latest"]);
        let mut second = six(&names, &["ubuntu-latest"]);
        for job in &mut second {
            job.id += 10;
            job.run_id = 2;
        }
        first.append(&mut second);
        let runs = vec![
            run_record(2, PATH),
            run_record(1, "acme/widgets/.github/workflows/ci.yml@refs/heads/main"),
        ];
        let found = measure_runs(YAML, runs, first);
        assert_eq!(
            only_message(&found),
            "matrix runs 12 jobs across 2 runs; 24 billed minutes vs 14 raw minutes; rounding loss 10 minutes at 0.006 USD/min (0.06 USD)"
        );
    }

    #[test]
    fn two_repositories_stay_separate() {
        let names: Vec<String> = (0..6).map(prefix_name).collect();
        let mut first = six(&names, &["ubuntu-latest"]);
        let mut second = six(&names, &["macos-latest"]);
        for job in &mut second {
            job.id += 10;
            job.run_id = 2;
        }
        first.append(&mut second);
        let mut other = run_record(2, "other/app/.github/workflows/ci.yml@main");
        other.repository = Some("other/app".to_string());
        let runs = vec![
            run_record(1, "acme/widgets/.github/workflows/ci.yml@main"),
            other,
        ];
        let row = usage_row();
        let mut other_row = usage_row();
        other_row.repository = "other/app".to_string();
        other_row.sku = "actions_macos".to_string();
        let api = ApiData {
            public: false,
            runs,
            jobs: first,
        };
        let scanned = scanned(YAML);
        let ranking = rank(&[row, other_row], None);
        let found = findings(&api, &ranking.workflows, &scanned);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].message, LINUX_6);
        assert_eq!(found[0].repository.as_deref(), Some(REPO));
        assert_eq!(found[1].message, MACOS_6);
        assert_eq!(found[1].repository.as_deref(), Some("other/app"));
    }

    #[test]
    fn a_prefix_does_not_swallow_a_longer_job_name() {
        let yaml = "\
jobs:
  build:
    timeout-minutes: 10
    strategy:
      matrix:
        os: [ubuntu-latest]
  builder:
    timeout-minutes: 10
    strategy:
      matrix:
        os: [macos-latest]
";
        let mut jobs = Vec::new();
        for index in 0..6 {
            jobs.push(seventy(
                index as u64 + 1,
                &format!("build (ubuntu-{index})"),
                &["ubuntu-latest"],
            ));
            let mut mac = seventy(
                index as u64 + 11,
                &format!("builder (mac-{index})"),
                &["macos-latest"],
            );
            mac.id = index as u64 + 11;
            jobs.push(mac);
        }
        let found = measure(yaml, jobs);
        assert_eq!(found.len(), 2, "{found:?}");
        let build = found.iter().find(|item| item.message == LINUX_6).unwrap();
        let builder = found.iter().find(|item| item.message == MACOS_6).unwrap();
        let workflow = scanned(yaml).remove(0);
        assert_eq!(build.line, workflow.jobs[0].matrix.as_ref().unwrap().line);
        assert_eq!(builder.line, workflow.jobs[1].matrix.as_ref().unwrap().line);
        assert_ne!(build.line, builder.line);
    }

    #[test]
    fn unmatched_names_follow_the_runner_label_to_that_matrix_line() {
        let yaml = "\
jobs:
  linux:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    strategy:
      matrix:
        os: [ubuntu-latest]
  mac:
    runs-on: macos-latest
    timeout-minutes: 10
    strategy:
      matrix:
        os: [macos-latest]
";
        let mut jobs = Vec::new();
        for index in 0..6 {
            jobs.push(seventy(
                index as u64 + 1,
                &format!("alpha-{index}"),
                &["ubuntu-latest"],
            ));
            jobs.push(seventy(
                index as u64 + 11,
                &format!("beta-{index}"),
                &["MACOS-LATEST"],
            ));
        }
        let found = measure(yaml, jobs);
        let workflow = scanned(yaml).remove(0);
        let linux_line = workflow.jobs[0].matrix.as_ref().unwrap().line;
        let mac_line = workflow.jobs[1].matrix.as_ref().unwrap().line;
        assert_ne!(linux_line, workflow.jobs[0].line);
        assert_ne!(mac_line, workflow.jobs[1].line);
        let linux = found.iter().find(|item| item.line == linux_line).unwrap();
        let mac = found.iter().find(|item| item.line == mac_line).unwrap();
        assert_eq!(linux.message, LINUX_6);
        assert_eq!(mac.message, MACOS_6);
    }

    #[test]
    fn a_sibling_job_outside_the_matrix_is_not_counted() {
        let yaml = "\
jobs:
  lint:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - run: echo lint
  build:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    strategy:
      matrix:
        os: [ubuntu-latest, windows-latest]
        node: [14, 16, 18]
";
        let names: Vec<String> = (0..6).map(prefix_name).collect();
        let mut jobs = six(&names, &["ubuntu-latest"]);
        jobs.push(seventy(30, "lint", &["macos-latest"]));
        let found = measure(yaml, jobs);
        assert_eq!(only_message(&found), LINUX_6);
        let workflow = scanned(yaml).remove(0);
        assert_eq!(
            found[0].line,
            workflow.jobs[1].matrix.as_ref().unwrap().line
        );
    }

    #[test]
    fn an_expression_axis_uses_the_executed_count() {
        let yaml = "\
jobs:
  build:
    timeout-minutes: 10
    strategy:
      matrix:
        os: [ubuntu-latest, windows-latest]
        node: ${{ fromJSON(needs.setup.outputs.node) }}
";
        let scanned = scanned(yaml);
        let yaml_only = yaml_findings(&scanned).unwrap();
        let estimate = yaml_only
            .iter()
            .find(|item| item.kind == FindingKind::MatrixJobs)
            .unwrap();
        assert!(
            estimate.message.contains("at least"),
            "{}",
            estimate.message
        );
        let names: Vec<String> = (0..6).map(prefix_name).collect();
        let found = measure(yaml, six(&names, &["ubuntu-latest"]));
        assert_eq!(only_message(&found), LINUX_6);
        assert!(!found[0].message.contains("at least"));
        assert_eq!(found[0].line, estimate.line);
    }

    #[test]
    fn flow_style_without_a_prefix_uses_the_matrix_line() {
        let yaml = "jobs: {build: {runs-on: ubuntu-latest, timeout-minutes: 10, strategy: {matrix: {os: [ubuntu-latest, windows-latest], node: [14, 16, 18]}}}}\n";
        let names: Vec<String> = (0..6).map(|index| format!("cell-{index}")).collect();
        let found = measure(yaml, six(&names, &["ubuntu-latest"]));
        assert_eq!(only_message(&found), LINUX_6);
        assert_eq!(found[0].line, matrix_line(yaml));
    }

    #[test]
    fn mixed_skus_price_each_rate() {
        let names: Vec<String> = (0..6).map(prefix_name).collect();
        let mut jobs = six(&names, &["ubuntu-latest"]);
        for job in jobs.iter_mut().skip(3) {
            job.labels = vec!["macos-latest".to_string()];
        }
        let found = measure(YAML, jobs);
        assert_eq!(
            only_message(&found),
            "matrix runs 6 jobs; 6 billed minutes vs 3.5 raw minutes; rounding loss 2.5 minutes at 0.006 USD/min (0.015 USD); 6 billed minutes vs 3.5 raw minutes; rounding loss 2.5 minutes at 0.062 USD/min (0.155 USD)"
        );
    }

    #[test]
    fn the_finding_is_listed_under_its_workflow() {
        let names: Vec<String> = (0..6).map(prefix_name).collect();
        let api = ApiData {
            public: false,
            runs: vec![run_record(1, "acme/widgets/.github/workflows/ci.yml@main")],
            jobs: six(&names, &["ubuntu-latest"]),
        };
        let scanned = scanned(YAML);
        let ranking = rank(&[usage_row()], None);
        let mut yaml = yaml_findings(&scanned).unwrap();
        let measured = findings(&api, &ranking.workflows, &scanned);
        supersede(&mut yaml, &measured);
        yaml.extend(measured);
        let text = render_text(
            &ranking.workflows,
            &ranking.bill,
            Some(&yaml),
            Some(&api),
            None,
            &scanned,
        );
        assert!(text.contains(LINUX_6), "{text}");
        assert!(text.contains("F4 "), "{text}");
        assert!(
            !text.contains("each job is billed in whole minutes"),
            "{text}"
        );
        assert!(!text.contains("Workflows without usage"), "{text}");
        assert_public(&text);
        let json = crate::report::render_json_with_runs(
            &ranking.workflows,
            &ranking.bill,
            Some(&yaml),
            None,
            &api,
            None,
            &scanned,
        );
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let message = &value["workflows"][0]["findings"][0]["message"];
        assert_eq!(message, LINUX_6);
        assert_eq!(value["workflows"][0]["findings"][0]["id"], "F4");
        assert_eq!(
            value["workflows"][0]["findings"][0]["line"],
            matrix_line(YAML)
        );
    }

    #[test]
    fn a_callee_matrix_is_measured_on_the_callee_line() {
        let caller = "\
name: Caller
on: push
jobs:
  call:
    uses: ./.github/workflows/wide.yml
";
        let callee = "\
name: Wide
on: workflow_call
jobs:
  build:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    strategy:
      matrix:
        os: [ubuntu-latest, windows-latest]
        node: [14, 16, 18]
  back:
    uses: ./.github/workflows/ci.yml
";
        let caller_wf = scan_yaml(Path::new("repo/.github/workflows/ci.yml"), caller).unwrap();
        let callee_wf = scan_yaml(Path::new("repo/.github/workflows/wide.yml"), callee).unwrap();
        let line = callee_wf.jobs[0].matrix.as_ref().unwrap().line;
        let names: Vec<String> = (0..6).map(prefix_name).collect();
        let api = ApiData {
            public: false,
            runs: vec![run_record(1, "acme/widgets/.github/workflows/ci.yml@main")],
            jobs: six(&names, &["ubuntu-latest"]),
        };
        let scanned = vec![caller_wf, callee_wf];
        let ranking = rank(&[usage_row()], None);
        let found = findings(&api, &ranking.workflows, &scanned);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].message, LINUX_6);
        assert_eq!(found[0].line, line);
        assert!(
            found[0].path.ends_with("wide.yml"),
            "{}",
            found[0].path.display()
        );
        assert_eq!(found[0].workflow_path.as_deref(), Some(PATH));
        assert_ne!(found[0].line, 1);
    }

    #[test]
    fn no_executed_matrix_jobs_leave_the_yaml_estimate() {
        let mut running = seventy(1, "build (ubuntu-latest, 14)", &["ubuntu-latest"]);
        running.completed_at = None;
        let api = ApiData {
            public: false,
            runs: vec![run_record(1, PATH)],
            jobs: vec![running],
        };
        let scanned = scanned(YAML);
        let mut yaml = yaml_findings(&scanned).unwrap();
        let measured = findings(&api, &[], &scanned);
        assert!(measured.is_empty(), "{measured:?}");
        supersede(&mut yaml, &measured);
        let estimate = yaml
            .iter()
            .find(|item| item.kind == FindingKind::MatrixJobs)
            .unwrap();
        assert!(estimate
            .message
            .contains("each job is billed in whole minutes"));
    }
}
