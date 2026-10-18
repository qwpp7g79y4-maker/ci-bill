//! Re-run waste inside one workflow run.
//!
//! Jobs that share a run id and a job name are one execution sequence. The
//! lowest attempt, then the earliest start, is the original execution. Every
//! later job with a different id is waste, except an inherited copy. The cost
//! is that job's reconciled amount (its share of the usage-report net, or the
//! list price when the run has no matching usage row).
//!
//! A workflow with waste gets one finding, `re-runs cost X USD (N extra
//! executions)`, and one finding per extra execution at that job's file and
//! line. A name that matches no scanned job is still reported, on the workflow
//! file, with its cost.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::api::{parse_github_time, workflow_paths_match, ApiData, Job as ApiJob};
use crate::findings::{Finding, FindingKind, SavingsMethod};
use crate::job_cost::bill_job;
use crate::money::Micros;
use crate::rank::WorkflowTotal;
use crate::reconcile::{self, Reconciliation};
use crate::workflow::{ReusableSource, Workflow};

/// Findings for extra executions in `api`.
///
/// Names are compared without ASCII case. File order does not choose the
/// original execution. An empty result means no run repeated a job.
pub fn findings(api: &ApiData, workflows: &[WorkflowTotal], scanned: &[Workflow]) -> Vec<Finding> {
    let reconciliation = reconcile::reconcile(workflows, Some(api), scanned);
    let mut buckets: Vec<Bucket> = Vec::new();
    for extra in collect_extras(api, workflows, scanned, &reconciliation) {
        if let Some(bucket) = buckets.iter_mut().find(|bucket| {
            bucket.repository == extra.repository && bucket.workflow_path == extra.workflow_path
        }) {
            bucket.extras.push(extra);
        } else {
            buckets.push(Bucket {
                file: extra.summary_file.clone(),
                repository: extra.repository.clone(),
                workflow_path: extra.workflow_path.clone(),
                extras: vec![extra],
            });
        }
    }
    let mut found = Vec::new();
    for bucket in buckets {
        let total = bucket.extras.iter().fold(Micros::ZERO, |total, extra| {
            Micros(total.0.saturating_add(extra.cost.0))
        });
        let count = bucket.extras.len();
        found.push(make_finding(
            &bucket.file,
            1,
            &summary_message(total, count),
            bucket.repository.clone(),
            &bucket.workflow_path,
            total,
        ));
        for extra in bucket.extras {
            found.push(make_finding(
                &extra.file,
                extra.line,
                &job_message(&extra.name, extra.job_id, extra.attempt, extra.cost),
                extra.repository,
                &extra.workflow_path,
                extra.cost,
            ));
        }
    }
    found
}

struct Bucket {
    file: PathBuf,
    repository: Option<String>,
    workflow_path: String,
    extras: Vec<Extra>,
}

struct Extra {
    name: String,
    job_id: u64,
    attempt: u64,
    cost: Micros,
    file: PathBuf,
    line: usize,
    summary_file: PathBuf,
    repository: Option<String>,
    workflow_path: String,
}

fn collect_extras(
    api: &ApiData,
    workflows: &[WorkflowTotal],
    scanned: &[Workflow],
    reconciliation: &Reconciliation,
) -> Vec<Extra> {
    let mut groups: Vec<Vec<&ApiJob>> = Vec::new();
    for job in &api.jobs {
        if let Some(group) = groups.iter_mut().find(|group| same_sequence(group[0], job)) {
            group.push(job);
        } else {
            groups.push(vec![job]);
        }
    }
    let mut extras = Vec::new();
    for mut group in groups {
        group.sort_by(|left, right| execution_order(left, right));
        let mut seen = HashSet::new();
        if let Some(first) = group.first() {
            seen.insert(first.id);
        }
        for job in group.into_iter().skip(1) {
            if !seen.insert(job.id) || api.is_inherited_copy(job) {
                continue;
            }
            extras.push(extra_for(api, workflows, scanned, reconciliation, job));
        }
    }
    extras
}

fn same_sequence(left: &ApiJob, right: &ApiJob) -> bool {
    left.run_id == right.run_id && same_name(&left.name, &right.name)
}

fn same_name(left: &str, right: &str) -> bool {
    left.trim().eq_ignore_ascii_case(right.trim())
}

/// Lowest attempt, then earliest start, then lowest job id.
///
/// A missing or unreadable start sorts after a real timestamp, so a timed
/// execution stays the original when the attempt is the same.
fn execution_order(left: &ApiJob, right: &ApiJob) -> Ordering {
    left.run_attempt
        .cmp(&right.run_attempt)
        .then_with(|| start_rank(left).cmp(&start_rank(right)))
        .then(left.id.cmp(&right.id))
}

fn start_rank(job: &ApiJob) -> (bool, i128) {
    match job.started_at.as_deref().and_then(parse_github_time) {
        Some(time) => (false, time),
        None => (true, 0),
    }
}

fn extra_for(
    api: &ApiData,
    workflows: &[WorkflowTotal],
    scanned: &[Workflow],
    reconciliation: &Reconciliation,
    job: &ApiJob,
) -> Extra {
    let linked = api.workflow_path(job.run_id);
    let run_repo = api
        .runs
        .iter()
        .find(|run| run.id == job.run_id)
        .and_then(|run| run.repository.clone());
    let ranked = match_ranked(workflows, &linked, run_repo.as_deref());
    let workflow_path = ranked
        .map(|row| row.workflow_path.clone())
        .filter(|path| !path.is_empty())
        .unwrap_or(linked);
    let repository = ranked.map(|row| row.repository.clone()).or(run_repo);
    let summary_file = workflow_source(scanned, &workflow_path)
        .map(|workflow| workflow.path.clone())
        .unwrap_or_else(|| PathBuf::from(&workflow_path));
    let (file, line) = link_execution(scanned, &workflow_path, &job.name)
        .unwrap_or_else(|| (summary_file.clone(), 1));
    let cost = reconciliation
        .job(job.id)
        .map(|amount| amount.micros)
        .unwrap_or_else(|| bill_job(api, job).list_price);
    Extra {
        name: job.name.clone(),
        job_id: job.id,
        attempt: job.run_attempt,
        cost,
        file,
        line,
        summary_file,
        repository,
        workflow_path,
    }
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

fn workflow_source<'a>(scanned: &'a [Workflow], workflow_path: &str) -> Option<&'a Workflow> {
    scanned.iter().find(|workflow| {
        workflow
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| file_matches(workflow_path, name))
    })
}

fn file_matches(workflow_path: &str, file_name: &str) -> bool {
    let key = format!(".github/workflows/{file_name}");
    let normalized = workflow_path.replace('\\', "/");
    if normalized.eq_ignore_ascii_case(&key) {
        return true;
    }
    normalized.char_indices().any(|(index, separator)| {
        separator == '/' && normalized[index + 1..].eq_ignore_ascii_case(&key)
    })
}

fn link_execution(
    scanned: &[Workflow],
    workflow_path: &str,
    api_name: &str,
) -> Option<(PathBuf, usize)> {
    let root = workflow_source(scanned, workflow_path)?;
    let mut best: Option<Hit> = None;
    let mut stack = Vec::new();
    search_workflow(scanned, root, api_name, &mut stack, &mut best);
    best.map(|hit| (hit.path, hit.line))
}

struct Hit {
    path: PathBuf,
    line: usize,
    rank: u8,
    len: usize,
}

fn search_workflow(
    scanned: &[Workflow],
    workflow: &Workflow,
    api_name: &str,
    stack: &mut Vec<PathBuf>,
    best: &mut Option<Hit>,
) {
    if stack.iter().any(|path| path == &workflow.path) {
        return;
    }
    stack.push(workflow.path.clone());
    for job in &workflow.jobs {
        score_job(best, &workflow.path, job, api_name);
        if let Some(call) = &job.uses {
            if matches!(call.source, ReusableSource::Local) {
                if let Some(callee) = find_file(scanned, &call.file_name) {
                    search_workflow(scanned, callee, api_name, stack, best);
                }
            }
        }
    }
    stack.pop();
}

fn find_file<'a>(scanned: &'a [Workflow], file_name: &str) -> Option<&'a Workflow> {
    scanned.iter().find(|workflow| {
        workflow
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(file_name))
    })
}

const RANK_DISPLAY: u8 = 4;
const RANK_KEY: u8 = 3;
const RANK_PREFIX: u8 = 2;

fn score_job(best: &mut Option<Hit>, path: &Path, job: &crate::workflow::Job, api_name: &str) {
    let api_name = api_name.trim();
    if let Some(display) = job.display_name.as_deref() {
        if same_name(api_name, display) {
            consider(best, path, job.line, RANK_DISPLAY, display.trim().len());
        } else if has_matrix_suffix(api_name, display) {
            consider(best, path, job.line, RANK_PREFIX, display.trim().len());
        } else if let Some(literals) = expression_literals(display) {
            if literals_match(&literals, api_name) {
                let len = literals.iter().map(String::len).sum();
                consider(best, path, job.line, RANK_PREFIX, len);
            }
        }
    }
    if same_name(api_name, &job.name) {
        consider(best, path, job.line, RANK_KEY, job.name.trim().len());
    } else if has_matrix_suffix(api_name, &job.name) {
        consider(best, path, job.line, RANK_PREFIX, job.name.trim().len());
    }
}

fn consider(best: &mut Option<Hit>, path: &Path, line: usize, rank: u8, len: usize) {
    let hit = Hit {
        path: path.to_path_buf(),
        line,
        rank,
        len,
    };
    if best
        .as_ref()
        .is_none_or(|old| (hit.rank, hit.len) > (old.rank, old.len))
    {
        *best = Some(hit);
    }
}

fn has_matrix_suffix(api_name: &str, label: &str) -> bool {
    let label = label.trim();
    if label.is_empty() {
        return false;
    }
    let api_name = api_name.trim().to_ascii_lowercase();
    let label = label.to_ascii_lowercase();
    api_name
        .strip_prefix(&label)
        .is_some_and(|rest| rest.starts_with(" ("))
}

fn expression_literals(template: &str) -> Option<Vec<String>> {
    if !template.contains("${{") {
        return None;
    }
    let mut literals = Vec::new();
    let mut rest = template;
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
    if literals.iter().all(String::is_empty) {
        return false;
    }
    let api_name = api_name.trim().to_ascii_lowercase();
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

fn summary_message(cost: Micros, count: usize) -> String {
    format!(
        "re-runs cost {} USD ({count} extra executions)",
        cost.format_cents()
    )
}

fn job_message(name: &str, id: u64, attempt: u64, cost: Micros) -> String {
    let shown = {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            "(no name)"
        } else {
            trimmed
        }
    };
    format!(
        "re-run of {shown} (job {id}, attempt {attempt}) costs {} USD",
        cost.format_cents()
    )
}

fn make_finding(
    path: &Path,
    line: usize,
    message: &str,
    repository: Option<String>,
    workflow_path: &str,
    cost: Micros,
) -> Finding {
    Finding {
        path: path.to_path_buf(),
        line,
        kind: FindingKind::RerunWaste,
        message: message.to_string(),
        repository,
        workflow_path: Some(workflow_path.to_string()),
        savings: cost,
        method: SavingsMethod::Rerun,
        anchor: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{ApiData, Job, WorkflowRun};
    use crate::money::Micros;
    use crate::rank::rank;
    use crate::report::render_text;
    use crate::usage::UsageRow;
    use crate::workflow::scan_yaml;
    use std::path::Path;

    const PATH: &str = ".github/workflows/ci.yml";
    const REPO: &str = "acme/widgets";

    fn usd(text: &str) -> Micros {
        Micros::parse(text).unwrap()
    }

    fn row(minutes: i64, net: &str) -> UsageRow {
        UsageRow {
            line: 2,
            date: "2026-04-01".to_string(),
            product: "actions".to_string(),
            sku: "actions_linux".to_string(),
            quantity: Micros(minutes * Micros::SCALE),
            unit_type: "minutes".to_string(),
            applied_cost_per_quantity: usd("0.006"),
            gross: usd(net),
            discount: Micros::ZERO,
            net: usd(net),
            username: "dev".to_string(),
            organization: "acme".to_string(),
            repository: REPO.to_string(),
            workflow_path: PATH.to_string(),
        }
    }

    fn run_record(attempt: u64) -> WorkflowRun {
        WorkflowRun {
            id: 9,
            path: PATH.to_string(),
            repository: Some(REPO.to_string()),
            run_attempt: attempt,
            run_number: 9,
            event: "push".to_string(),
            conclusion: Some("success".to_string()),
            run_started_at: Some("2026-04-02T00:00:00Z".to_string()),
            head_branch: None,
            pull_requests: Vec::new(),
        }
    }

    fn job(
        id: u64,
        attempt: u64,
        name: &str,
        started: Option<&str>,
        completed: Option<&str>,
        labels: &[&str],
    ) -> Job {
        Job {
            id,
            run_id: 9,
            run_attempt: attempt,
            name: name.to_string(),
            labels: labels.iter().map(|label| (*label).to_string()).collect(),
            started_at: started.map(str::to_string),
            completed_at: completed.map(str::to_string),
            conclusion: Some("success".to_string()),
            runner_name: Some("runner-a".to_string()),
        }
    }

    fn ten(id: u64, attempt: u64, name: &str, day: &str) -> Job {
        let started = format!("2026-04-{day}T00:00:00Z");
        let completed = format!("2026-04-{day}T00:10:00Z");
        job(
            id,
            attempt,
            name,
            Some(&started),
            Some(&completed),
            &["ubuntu-latest"],
        )
    }

    const YAML: &str = "\
name: CI
on: push
jobs:
  lint:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - run: echo lint
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - run: echo test
";

    fn scanned(text: &str) -> Vec<Workflow> {
        vec![scan_yaml(Path::new("repo/.github/workflows/ci.yml"), text).unwrap()]
    }

    fn waste(jobs: Vec<Job>, minutes: i64, net: &str) -> (Vec<Finding>, Vec<WorkflowTotal>) {
        let api = ApiData {
            public: false,
            runs: vec![run_record(2)],
            jobs,
        };
        let scanned = scanned(YAML);
        let ranking = rank(&[row(minutes, net)], None).unwrap();
        let found = findings(&api, &ranking.workflows, &scanned);
        (found, ranking.workflows)
    }

    fn messages(found: &[Finding]) -> Vec<&str> {
        found
            .iter()
            .filter(|finding| finding.kind == FindingKind::RerunWaste)
            .map(|finding| finding.message.as_str())
            .collect()
    }

    fn job_line(text: &str, name: &str) -> usize {
        scan_yaml(Path::new("repo/.github/workflows/ci.yml"), text)
            .unwrap()
            .jobs
            .into_iter()
            .find(|job| job.name == name)
            .unwrap()
            .line
    }

    #[test]
    fn partial_rerun_counts_only_the_repeated_job() {
        let jobs = vec![
            ten(1, 1, "lint", "01"),
            ten(3, 2, "test", "02"),
            ten(2, 1, "test", "01"),
        ];
        let (found, _) = waste(jobs, 30, "0.180");
        let lines = messages(&found);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines.contains(&"re-runs cost 0.06 USD (1 extra executions)"));
        assert!(lines.contains(&"re-run of test (job 3, attempt 2) costs 0.06 USD"));
        assert!(lines.iter().all(|line| !line.contains("lint")), "{lines:?}");
        let test_hit = found
            .iter()
            .find(|item| item.message.contains("job 3"))
            .unwrap();
        assert_eq!(test_hit.line, job_line(YAML, "test"));
        assert!(test_hit.path.ends_with("ci.yml"));
        assert_eq!(test_hit.kind.code(), "F5");
        assert_eq!(test_hit.savings, Micros::parse("0.060").unwrap());
        assert_eq!(test_hit.method, crate::findings::SavingsMethod::Rerun);
        let summary = found.iter().find(|item| item.line == 1).unwrap();
        assert_eq!(summary.savings, Micros::parse("0.060").unwrap());
        assert_eq!(summary.method, crate::findings::SavingsMethod::Rerun);
        assert_eq!(summary.workflow_path.as_deref(), Some(PATH));
        assert_eq!(summary.repository.as_deref(), Some(REPO));
    }

    #[test]
    fn full_rerun_counts_every_job_once() {
        let jobs = vec![
            ten(4, 2, "lint", "02"),
            ten(5, 2, "test", "02"),
            ten(1, 1, "lint", "01"),
            ten(2, 1, "test", "01"),
        ];
        let (found, _) = waste(jobs, 40, "0.240");
        let lines = messages(&found);
        assert!(lines.contains(&"re-runs cost 0.12 USD (2 extra executions)"));
        assert!(lines.contains(&"re-run of lint (job 4, attempt 2) costs 0.06 USD"));
        assert!(lines.contains(&"re-run of test (job 5, attempt 2) costs 0.06 USD"));
        assert_eq!(lines.len(), 3, "{lines:?}");
        let lint = found
            .iter()
            .find(|item| item.message.contains("job 4"))
            .unwrap();
        let test = found
            .iter()
            .find(|item| item.message.contains("job 5"))
            .unwrap();
        assert_eq!(lint.line, job_line(YAML, "lint"));
        assert_eq!(test.line, job_line(YAML, "test"));
    }

    #[test]
    fn an_inherited_copy_is_not_an_extra_execution() {
        let mut copied = ten(4, 2, "lint", "01");
        copied.started_at = Some("2026-04-01T00:00:00Z".to_string());
        copied.completed_at = Some("2026-04-01T00:10:00Z".to_string());
        let jobs = vec![
            copied,
            ten(1, 1, "lint", "01"),
            ten(2, 1, "test", "01"),
            ten(5, 2, "test", "02"),
        ];
        let (found, _) = waste(jobs, 30, "0.180");
        let lines = messages(&found);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines.iter().all(|line| !line.contains("lint")), "{lines:?}");
        assert!(lines.contains(&"re-run of test (job 5, attempt 2) costs 0.06 USD"));

        let only_copy = vec![ten(1, 1, "lint", "01"), {
            let mut copy = ten(4, 2, "lint", "01");
            copy.started_at = Some("2026-04-01T12:00:00Z".to_string());
            copy
        }];
        let (found, _) = waste(only_copy, 10, "0.060");
        assert!(messages(&found).is_empty(), "{found:?}");
    }

    #[test]
    fn an_unlinkable_name_stays_on_the_workflow_file_with_its_cost() {
        let jobs = vec![
            ten(7, 1, "custom-check", "01"),
            ten(8, 2, "custom-check", "02"),
        ];
        let (found, _) = waste(jobs, 20, "0.120");
        let lines = messages(&found);
        assert!(lines.contains(&"re-runs cost 0.06 USD (1 extra executions)"));
        assert!(lines.contains(&"re-run of custom-check (job 8, attempt 2) costs 0.06 USD"));
        let job_hit = found
            .iter()
            .find(|item| item.message.contains("job 8"))
            .unwrap();
        assert_eq!(job_hit.line, 1);
        assert!(
            job_hit.path.ends_with("ci.yml"),
            "{}",
            job_hit.path.display()
        );
        assert_ne!(job_hit.line, job_line(YAML, "lint"));
        assert_ne!(job_hit.line, job_line(YAML, "test"));
    }

    #[test]
    fn capitals_crlf_and_attempt_order_do_not_change_the_extra() {
        let yaml = YAML.replace('\n', "\r\n");
        let mut later = ten(3, 2, "TEST", "02");
        later.labels = vec!["UBUNTU-LATEST".to_string()];
        later.conclusion = Some("SUCCESS".to_string());
        later.runner_name = None;
        let jobs = vec![later, ten(2, 1, "Test", "01"), ten(1, 1, "LINT", "01")];
        let api = ApiData {
            public: false,
            runs: vec![WorkflowRun {
                repository: Some("ACME/WIDGETS".to_string()),
                path: ".GitHub/Workflows/CI.yml".to_string(),
                ..run_record(2)
            }],
            jobs,
        };
        let scanned = scanned(&yaml);
        let ranking = rank(&[row(30, "0.180")], None).unwrap();
        let found = findings(&api, &ranking.workflows, &scanned);
        let lines = messages(&found);
        assert!(lines.contains(&"re-runs cost 0.06 USD (1 extra executions)"));
        assert!(lines.contains(&"re-run of TEST (job 3, attempt 2) costs 0.06 USD"));
        assert!(lines
            .iter()
            .all(|line| !line.to_ascii_lowercase().contains("lint")));
        let hit = found
            .iter()
            .find(|item| item.message.contains("job 3"))
            .unwrap();
        assert_eq!(hit.line, job_line(&yaml, "test"));
        assert_eq!(hit.repository.as_deref(), Some(REPO));
        assert_eq!(hit.workflow_path.as_deref(), Some(PATH));
    }

    #[test]
    fn missing_fields_still_report_the_extra_execution() {
        let mut bare = job(8, 2, "   ", None, None, &[]);
        bare.conclusion = None;
        bare.runner_name = None;
        bare.labels.clear();
        let jobs = vec![ten(7, 1, "", "01"), bare];
        let (found, _) = waste(jobs, 10, "0.060");
        let lines = messages(&found);
        assert!(
            lines.contains(&"re-run of (no name) (job 8, attempt 2) costs 0.00 USD"),
            "{lines:?}"
        );
        assert!(lines.contains(&"re-runs cost 0.00 USD (1 extra executions)"));
        let hit = found
            .iter()
            .find(|item| item.message.contains("job 8"))
            .unwrap();
        assert_eq!(hit.line, 1);
        assert!(!hit.message.to_ascii_lowercase().contains("skipped"));
    }

    #[test]
    fn three_attempts_count_two_extras_and_attempt_beats_the_clock() {
        let mut early_clock = ten(9, 3, "test", "02");
        early_clock.started_at = Some("2026-01-01T00:00:00Z".to_string());
        early_clock.completed_at = Some("2026-01-01T00:10:00Z".to_string());
        let jobs = vec![
            early_clock,
            ten(3, 2, "test", "03"),
            ten(2, 1, "test", "04"),
        ];
        let (found, _) = waste(jobs, 30, "0.180");
        let lines = messages(&found);
        assert!(lines.contains(&"re-runs cost 0.12 USD (2 extra executions)"));
        assert!(lines.iter().any(|line| line.contains("job 3, attempt 2")));
        assert!(lines.iter().any(|line| line.contains("job 9, attempt 3")));
        assert!(lines.iter().all(|line| !line.contains("job 2")));
    }

    #[test]
    fn the_same_attempt_uses_start_time_then_job_id() {
        let mut second = ten(12, 1, "test", "01");
        second.started_at = Some("2026-04-01T00:05:00Z".to_string());
        second.completed_at = Some("2026-04-01T00:15:00Z".to_string());
        let mut first = ten(11, 1, "test", "01");
        first.started_at = Some("2026-04-01T00:00:00Z".to_string());
        let (found, _) = waste(vec![second, first], 20, "0.120");
        let lines = messages(&found);
        assert!(lines.contains(&"re-run of test (job 12, attempt 1) costs 0.06 USD"));
        assert!(lines.iter().all(|line| !line.contains("job 11")));

        let mut no_start = ten(14, 1, "test", "01");
        no_start.started_at = None;
        let (found, _) = waste(vec![no_start, ten(13, 1, "test", "01")], 20, "0.120");
        assert!(messages(&found).iter().any(|line| line.contains("job 14")));
    }

    #[test]
    fn separate_runs_do_not_share_a_job_name() {
        let mut other = ten(2, 1, "test", "03");
        other.run_id = 10;
        let api = ApiData {
            public: false,
            runs: vec![
                run_record(1),
                WorkflowRun {
                    id: 10,
                    run_number: 10,
                    run_attempt: 1,
                    ..run_record(1)
                },
            ],
            jobs: vec![ten(1, 1, "test", "01"), other],
        };
        let scanned = scanned(YAML);
        let ranking = rank(&[row(20, "0.120")], None).unwrap();
        let found = findings(&api, &ranking.workflows, &scanned);
        assert!(messages(&found).is_empty(), "{found:?}");
    }

    #[test]
    fn extras_from_two_runs_of_one_workflow_add_up() {
        let mut second = ten(4, 2, "test", "04");
        second.run_id = 10;
        let mut original = ten(3, 1, "test", "03");
        original.run_id = 10;
        let api = ApiData {
            public: false,
            runs: vec![
                run_record(2),
                WorkflowRun {
                    id: 10,
                    run_number: 10,
                    ..run_record(2)
                },
            ],
            jobs: vec![
                ten(1, 1, "test", "01"),
                ten(2, 2, "test", "02"),
                original,
                second,
            ],
        };
        let scanned = scanned(YAML);
        let ranking = rank(&[row(40, "0.240")], None).unwrap();
        let found = findings(&api, &ranking.workflows, &scanned);
        let lines = messages(&found);
        assert!(lines.contains(&"re-runs cost 0.12 USD (2 extra executions)"));
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.contains("re-run of test"))
                .count(),
            2
        );
    }

    #[test]
    fn two_repositories_keep_their_own_re_runs() {
        let mut other_run = run_record(2);
        other_run.id = 10;
        other_run.repository = Some("acme/other".to_string());
        let mut other_job = ten(4, 2, "test", "02");
        other_job.run_id = 10;
        let mut other_first = ten(3, 1, "test", "01");
        other_first.run_id = 10;
        let api = ApiData {
            public: false,
            runs: vec![run_record(2), other_run],
            jobs: vec![
                ten(1, 1, "test", "01"),
                ten(2, 2, "test", "02"),
                other_first,
                other_job,
            ],
        };
        let mut second = row(20, "0.120");
        second.repository = "acme/other".to_string();
        let ranking = rank(&[row(20, "0.120"), second], None).unwrap();
        let found = findings(&api, &ranking.workflows, &scanned(YAML));
        let repos: Vec<_> = found
            .iter()
            .filter(|item| item.message.starts_with("re-runs cost"))
            .map(|item| item.repository.clone())
            .collect();
        assert_eq!(repos.len(), 2, "{found:?}");
        assert!(repos.iter().any(|repo| repo.as_deref() == Some(REPO)));
        assert!(repos
            .iter()
            .any(|repo| repo.as_deref() == Some("acme/other")));
    }

    #[test]
    fn the_cost_is_the_invoice_share_not_the_list_price() {
        let jobs = vec![ten(1, 1, "test", "01"), ten(2, 2, "test", "02")];
        let (found, _) = waste(jobs, 20, "0.300");
        let lines = messages(&found);
        assert!(
            lines.contains(&"re-run of test (job 2, attempt 2) costs 0.15 USD"),
            "{lines:?}"
        );
        assert!(lines.contains(&"re-runs cost 0.15 USD (1 extra executions)"));
    }

    #[test]
    fn list_price_is_used_when_the_run_has_no_usage_row() {
        let api = ApiData {
            public: false,
            runs: vec![run_record(2)],
            jobs: vec![ten(1, 1, "test", "01"), ten(2, 2, "test", "02")],
        };
        let found = findings(&api, &[], &scanned(YAML));
        let lines = messages(&found);
        assert!(lines.contains(&"re-runs cost 0.06 USD (1 extra executions)"));
        let text = render_text(
            &[],
            &rank(&[], None).unwrap().bill,
            Some(&found),
            Some(&api),
            None,
            &[],
        );
        assert!(text.contains("Workflows without usage"), "{text}");
        assert!(text.contains("re-run of test (job 2, attempt 2) costs 0.06 USD"));
        assert!(
            !text.to_ascii_lowercase().contains("not supported"),
            "{text}"
        );
        assert!(!text.to_ascii_lowercase().contains("limitation"), "{text}");
    }

    #[test]
    fn matrix_cells_are_distinct_and_a_repeated_cell_links_to_the_job() {
        let yaml = "\
name: CI
on: push
jobs:
  test:
    name: Unit tests
    runs-on: ${{ matrix.os }}
    timeout-minutes: 10
    strategy:
      matrix:
        os: [ubuntu-latest, windows-latest]
    steps:
      - run: echo test
";
        let scanned = scanned(yaml);
        let test_line = job_line(yaml, "test");
        let once = vec![
            ten(1, 1, "test (ubuntu-latest)", "01"),
            ten(2, 1, "test (windows-latest)", "01"),
        ];
        let (found, _) = {
            let api = ApiData {
                public: false,
                runs: vec![run_record(1)],
                jobs: once,
            };
            let ranking = rank(&[row(20, "0.120")], None).unwrap();
            (
                findings(&api, &ranking.workflows, &scanned),
                ranking.workflows,
            )
        };
        assert!(messages(&found).is_empty(), "{found:?}");

        let repeated = vec![
            ten(1, 1, "Unit tests (ubuntu-latest)", "01"),
            ten(3, 2, "Unit tests (ubuntu-latest)", "02"),
            ten(2, 1, "test (windows-latest)", "01"),
        ];
        let api = ApiData {
            public: false,
            runs: vec![run_record(2)],
            jobs: repeated,
        };
        let ranking = rank(&[row(30, "0.180")], None).unwrap();
        let found = findings(&api, &ranking.workflows, &scanned);
        let lines = messages(&found);
        assert!(lines.contains(&"re-runs cost 0.06 USD (1 extra executions)"));
        let hit = found
            .iter()
            .find(|item| item.message.contains("job 3"))
            .unwrap();
        assert_eq!(hit.line, test_line);
        assert!(hit.message.contains("Unit tests (ubuntu-latest)"));
    }

    #[test]
    fn a_name_template_links_to_the_job_line() {
        let yaml = "\
name: CI
on: push
jobs:
  test:
    name: Test ${{ matrix.os }}
    runs-on: ${{ matrix.os }}
    timeout-minutes: 10
    steps:
      - run: echo test
";
        let api = ApiData {
            public: false,
            runs: vec![run_record(2)],
            jobs: vec![
                ten(1, 1, "Test ubuntu-latest", "01"),
                ten(2, 2, "Test UBUNTU-LATEST", "02"),
            ],
        };
        let scanned = scanned(yaml);
        let ranking = rank(&[row(20, "0.120")], None).unwrap();
        let found = findings(&api, &ranking.workflows, &scanned);
        let hit = found
            .iter()
            .find(|item| item.message.contains("job 2"))
            .unwrap();
        assert_eq!(hit.line, job_line(yaml, "test"));
    }

    #[test]
    fn a_local_callee_job_uses_the_callee_line_under_the_caller() {
        let caller = "\
name: Caller
on: push
jobs:
  call:
    uses: ./.github/workflows/lint.yml
";
        let callee = "\
name: Lint
on: push
jobs:
  lint:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - run: echo lint
";
        let caller_path = Path::new("repo/.github/workflows/caller.yml");
        let callee_path = Path::new("repo/.github/workflows/lint.yml");
        let scanned = vec![
            scan_yaml(caller_path, caller).unwrap(),
            scan_yaml(callee_path, callee).unwrap(),
        ];
        let lint_line = scanned[1].jobs[0].line;
        let api = ApiData {
            public: false,
            runs: vec![WorkflowRun {
                path: ".github/workflows/caller.yml".to_string(),
                ..run_record(2)
            }],
            jobs: vec![ten(1, 1, "lint", "01"), ten(2, 2, "lint", "02")],
        };
        let mut usage = row(20, "0.120");
        usage.workflow_path = ".github/workflows/caller.yml".to_string();
        let ranking = rank(&[usage], None).unwrap();
        let found = findings(&api, &ranking.workflows, &scanned);
        let hit = found
            .iter()
            .find(|item| item.message.contains("job 2"))
            .unwrap();
        assert_eq!(hit.line, lint_line);
        assert!(hit.path.ends_with("lint.yml"), "{}", hit.path.display());
        assert_eq!(
            hit.workflow_path.as_deref(),
            Some(".github/workflows/caller.yml")
        );
        let summary = found.iter().find(|item| item.line == 1).unwrap();
        assert!(
            summary.path.ends_with("caller.yml"),
            "{}",
            summary.path.display()
        );
    }

    #[test]
    fn flow_style_links_to_the_job_key_line() {
        let yaml =
            "name: CI\non: push\njobs: {test: {runs-on: ubuntu-latest, timeout-minutes: 10}}\n";
        let api = ApiData {
            public: false,
            runs: vec![run_record(2)],
            jobs: vec![ten(1, 1, "test", "01"), ten(2, 2, "test", "02")],
        };
        let scanned = scanned(yaml);
        let ranking = rank(&[row(20, "0.120")], None).unwrap();
        let found = findings(&api, &ranking.workflows, &scanned);
        let hit = found
            .iter()
            .find(|item| item.message.contains("job 2"))
            .unwrap();
        assert_eq!(hit.line, job_line(yaml, "test"));
    }

    #[test]
    fn a_single_execution_has_no_re_run_finding() {
        let (found, workflows) = waste(
            vec![ten(1, 1, "lint", "01"), ten(2, 1, "test", "01")],
            20,
            "0.120",
        );
        assert!(found.is_empty(), "{found:?}");
        let api = ApiData {
            public: false,
            runs: vec![run_record(1)],
            jobs: vec![ten(1, 1, "lint", "01"), ten(2, 1, "test", "01")],
        };
        let text = render_text(
            &workflows,
            &rank(&[row(20, "0.120")], None).unwrap().bill,
            Some(&found),
            Some(&api),
            None,
            &scanned(YAML),
        );
        assert!(!text.contains("F5"), "{text}");
        assert!(!text.contains("re-runs cost"), "{text}");
    }

    #[test]
    fn findings_outside_the_top_still_include_the_re_run() {
        let api = ApiData {
            public: false,
            runs: vec![run_record(2)],
            jobs: vec![ten(1, 1, "test", "01"), ten(2, 2, "test", "02")],
        };
        let cheap = row(20, "0.120");
        let mut rich = row(20, "9.000");
        rich.workflow_path = ".github/workflows/release.yml".to_string();
        rich.sku = "actions_macos".to_string();
        let ranking = rank(&[rich, cheap], None).unwrap();
        let scanned = scanned(YAML);
        let found = findings(&api, &ranking.workflows, &scanned);
        let text = render_text(
            &ranking.workflows,
            &ranking.bill,
            Some(&found),
            Some(&api),
            Some(1),
            &scanned,
        );
        let outside = text
            .find("Findings outside the top 1")
            .expect("outside block");
        let without = text.find("Workflows without usage");
        assert!(text[outside..].contains("re-runs cost 0.06 USD (1 extra executions)"));
        assert!(text[outside..].contains("ci.yml:"));
        if let Some(without) = without {
            assert!(outside < without, "{text}");
            assert!(!text[without..].contains("F5"), "{text}");
        }
        assert!(
            !text.to_ascii_lowercase().contains("not supported"),
            "{text}"
        );
    }

    #[test]
    fn a_skipped_extra_counts_and_the_message_names_the_job() {
        let mut skipped = ten(2, 2, "test", "02");
        skipped.conclusion = Some("skipped".to_string());
        skipped.completed_at = Some("2026-04-02T00:00:00Z".to_string());
        let api = ApiData {
            public: false,
            runs: vec![run_record(2)],
            jobs: vec![ten(1, 1, "test", "01"), skipped],
        };
        let ranking = rank(&[row(10, "0.060")], None).unwrap();
        let found = findings(&api, &ranking.workflows, &scanned(YAML));
        let lines = messages(&found);
        assert!(lines.iter().any(|line| line.contains("job 2, attempt 2")));
        assert!(lines.iter().any(|line| line.starts_with("re-runs cost")));
        assert!(lines
            .iter()
            .all(|line| !line.to_ascii_lowercase().contains("skipped")));
    }
}
