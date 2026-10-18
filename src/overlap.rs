//! Savings from cancelling a run that is still going when a newer one starts.
//!
//! Two runs match when they are the same workflow and they share a head branch
//! or a pull request number. Repository names are compared without ASCII case
//! when both runs have one. A later `run_started_at` that falls before the
//! earlier run's last job completion is the cancel point. The earlier run
//! contributes nothing when its conclusion is `cancelled`.
//!
//! Remaining minutes are billed minutes the earlier jobs would not have
//! charged if they had been cancelled at that instant: jobs already finished
//! stay as billed, jobs not yet started bill nothing, and a job in progress
//! keeps only the whole minutes up to the cancel point. The price is that
//! workflow's own usage rate for the SKU when the report has one, otherwise
//! the runner table. On a public repository a standard runner's table rate is
//! zero. The total is one finding on the workflow `concurrency:` line, or on
//! `on:` when the file has no concurrency key.

use std::path::PathBuf;

use crate::api::{parse_github_time, workflow_paths_match, ApiData, Job as ApiJob, WorkflowRun};
use crate::findings::{Finding, FindingKind, SavingsMethod};
use crate::job_cost::{bill_job, billed_minutes, minutes_if_cancelled_at};
use crate::money::{div_half_away, Micros};
use crate::rank::WorkflowTotal;
use crate::reconcile::list_price_invoice;
use crate::workflow::Workflow;

/// One finding per workflow whose earlier run kept spending after a later start.
///
/// An empty result means no priced overlap. A cancelled earlier run, a run
/// that is still in progress, and a run with no branch and no pull request
/// add nothing.
pub fn findings(api: &ApiData, workflows: &[WorkflowTotal], scanned: &[Workflow]) -> Vec<Finding> {
    let mut buckets: Vec<Bucket> = Vec::new();
    for older in &api.runs {
        if is_cancelled(older) {
            continue;
        }
        let Some(older_start) = started_ns(older) else {
            continue;
        };
        let Some(older_finish) = finished_ns(api, older) else {
            continue;
        };
        let Some(cancel_at) = earliest_interrupt(api, older, older_start, older_finish) else {
            continue;
        };
        let (workflow_path, repository) =
            placement(workflows, &older.path, older.repository.as_deref());
        let (minutes, usd) = price_tail(
            api,
            workflows,
            older,
            cancel_at,
            repository.as_deref(),
            &workflow_path,
        );
        if usd.0 <= 0 || minutes == 0 {
            continue;
        }
        if let Some(bucket) = buckets
            .iter_mut()
            .find(|bucket| same_workflow(bucket, &repository, &workflow_path))
        {
            bucket.minutes = bucket.minutes.saturating_add(minutes);
            bucket.usd += usd;
        } else {
            let (file, line) = locate(scanned, &workflow_path);
            buckets.push(Bucket {
                file,
                line,
                repository,
                workflow_path,
                minutes,
                usd,
            });
        }
    }
    let mut found: Vec<Finding> = buckets.into_iter().map(Bucket::into_finding).collect();
    found.sort_by(|left, right| {
        (
            left.path.as_path(),
            left.line,
            left.repository.as_deref().unwrap_or(""),
            left.workflow_path.as_deref().unwrap_or(""),
            left.message.as_str(),
        )
            .cmp(&(
                right.path.as_path(),
                right.line,
                right.repository.as_deref().unwrap_or(""),
                right.workflow_path.as_deref().unwrap_or(""),
                right.message.as_str(),
            ))
    });
    found
}

struct Bucket {
    file: PathBuf,
    line: usize,
    repository: Option<String>,
    workflow_path: String,
    minutes: u64,
    usd: Micros,
}

impl Bucket {
    fn into_finding(self) -> Finding {
        Finding {
            path: self.file,
            line: self.line,
            kind: FindingKind::OverlapSavings,
            message: saving_message(self.usd, self.minutes),
            repository: self.repository,
            workflow_path: Some(self.workflow_path),
            savings: self.usd,
            method: SavingsMethod::Overlap,
            anchor: None,
        }
    }
}

fn same_workflow(bucket: &Bucket, repository: &Option<String>, workflow_path: &str) -> bool {
    repo_key(bucket.repository.as_deref()) == repo_key(repository.as_deref())
        && linked(&bucket.workflow_path).eq_ignore_ascii_case(&linked(workflow_path))
}

fn repo_key(repository: Option<&str>) -> String {
    repository.unwrap_or("").trim().to_ascii_lowercase()
}

fn linked(path: &str) -> String {
    crate::api::linked_workflow_path(path)
}

fn saving_message(amount: Micros, minutes: u64) -> String {
    let noun = if minutes == 1 { "minute" } else { "minutes" };
    format!(
        "cancel-in-progress would save {} USD ({minutes} {noun})",
        format_usd(amount)
    )
}

/// At least two digits, then only the extra digits that are not zero.
fn format_usd(amount: Micros) -> String {
    let text = amount.to_decimal_string();
    let Some((whole, frac)) = text.split_once('.') else {
        return text;
    };
    let trimmed = frac.trim_end_matches('0');
    if trimmed.len() >= 2 {
        format!("{whole}.{trimmed}")
    } else if trimmed.is_empty() {
        format!("{whole}.00")
    } else {
        format!("{whole}.{trimmed}0")
    }
}

fn is_cancelled(run: &WorkflowRun) -> bool {
    run.conclusion
        .as_deref()
        .is_some_and(|text| text.trim().eq_ignore_ascii_case("cancelled"))
}

fn started_ns(run: &WorkflowRun) -> Option<i128> {
    run.run_started_at.as_deref().and_then(parse_github_time)
}

/// Latest job completion, when every job of `run` has finished.
fn finished_ns(api: &ApiData, run: &WorkflowRun) -> Option<i128> {
    let mut latest = None;
    let mut seen = false;
    for job in api.jobs.iter().filter(|job| job.run_id == run.id) {
        seen = true;
        if !job.counts() {
            return None;
        }
        let end = parse_github_time(job.completed_at.as_deref()?)?;
        latest = Some(latest.map_or(end, |time: i128| time.max(end)));
    }
    seen.then_some(latest).flatten()
}

fn earliest_interrupt(
    api: &ApiData,
    older: &WorkflowRun,
    older_start: i128,
    older_finish: i128,
) -> Option<i128> {
    let mut earliest = None;
    for newer in &api.runs {
        if newer.id == older.id || !same_scope(older, newer) {
            continue;
        }
        let Some(start) = started_ns(newer) else {
            continue;
        };
        if start > older_start && start < older_finish {
            earliest = Some(earliest.map_or(start, |time: i128| time.min(start)));
        }
    }
    earliest
}

fn same_scope(left: &WorkflowRun, right: &WorkflowRun) -> bool {
    workflow_paths_match(&left.path, &right.path)
        && repositories_compatible(left, right)
        && (shares_pull_request(left, right) || same_branch(left, right))
}

fn repositories_compatible(left: &WorkflowRun, right: &WorkflowRun) -> bool {
    match (repo_name(left), repo_name(right)) {
        (Some(left_repo), Some(right_repo)) => left_repo.eq_ignore_ascii_case(right_repo),
        _ => true,
    }
}

fn repo_name(run: &WorkflowRun) -> Option<&str> {
    run.repository
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn shares_pull_request(left: &WorkflowRun, right: &WorkflowRun) -> bool {
    left.pull_requests
        .iter()
        .any(|number| right.pull_requests.contains(number))
}

fn same_branch(left: &WorkflowRun, right: &WorkflowRun) -> bool {
    match (branch_key(left), branch_key(right)) {
        (Some(left_branch), Some(right_branch)) => left_branch == right_branch,
        _ => false,
    }
}

fn branch_key(run: &WorkflowRun) -> Option<String> {
    let text = run.head_branch.as_deref()?.trim().trim_matches('\u{feff}');
    if text.is_empty() {
        None
    } else {
        Some(text.to_ascii_lowercase())
    }
}

fn price_tail(
    api: &ApiData,
    workflows: &[WorkflowTotal],
    run: &WorkflowRun,
    cancel_at: i128,
    repository: Option<&str>,
    workflow_path: &str,
) -> (u64, Micros) {
    let mut minutes = 0_u64;
    let mut usd = Micros::ZERO;
    for job in api.jobs.iter().filter(|job| job.run_id == run.id) {
        let saved = saved_minutes(api, job, cancel_at);
        if saved == 0 {
            continue;
        }
        let bill = bill_job(api, job);
        let Some(sku) = bill.sku.as_deref() else {
            continue;
        };
        let per_minute = per_minute_rate(
            workflows,
            repository,
            workflow_path,
            sku,
            bill.usd_per_minute,
            api.public,
        );
        if per_minute.0 == 0 {
            continue;
        }
        minutes = minutes.saturating_add(saved);
        usd += multiply(saved, per_minute);
    }
    (minutes, usd)
}

fn saved_minutes(api: &ApiData, job: &ApiJob, cancel_at: i128) -> u64 {
    if api.is_inherited_copy(job) {
        return 0;
    }
    billed_minutes(job).saturating_sub(minutes_if_cancelled_at(job, cancel_at))
}

fn per_minute_rate(
    workflows: &[WorkflowTotal],
    repository: Option<&str>,
    workflow_path: &str,
    sku: &str,
    table_rate: Option<Micros>,
    public_repo: bool,
) -> Micros {
    if let Some(rate) = csv_rate(workflows, repository, workflow_path, sku) {
        return rate;
    }
    list_price_invoice(public_repo, Some(sku), table_rate.unwrap_or(Micros::ZERO))
}

fn csv_rate(
    workflows: &[WorkflowTotal],
    repository: Option<&str>,
    workflow_path: &str,
    sku: &str,
) -> Option<Micros> {
    let mut rates = Vec::new();
    for workflow in workflows {
        if !workflow_paths_match(&workflow.workflow_path, workflow_path) {
            continue;
        }
        if let Some(repository) = repository {
            if !workflow.repository.eq_ignore_ascii_case(repository) {
                continue;
            }
        }
        let Some(row) = workflow
            .skus
            .iter()
            .find(|item| item.sku.eq_ignore_ascii_case(sku))
        else {
            continue;
        };
        if row.minutes.0 <= 0 {
            continue;
        }
        let Some(num) = i128::from(row.net.0).checked_mul(i128::from(Micros::SCALE)) else {
            continue;
        };
        let Some(rate) = i64::try_from(div_half_away(num, i128::from(row.minutes.0))).ok() else {
            continue;
        };
        rates.push(Micros(rate));
    }
    if rates.len() == 1 {
        Some(rates[0])
    } else {
        None
    }
}

fn multiply(minutes: u64, rate: Micros) -> Micros {
    let minutes = i64::try_from(minutes).unwrap_or(i64::MAX);
    match rate.0.checked_mul(minutes) {
        Some(product) => Micros(product),
        None => Micros(i64::MAX),
    }
}

fn locate(scanned: &[Workflow], workflow_path: &str) -> (PathBuf, usize) {
    if let Some(workflow) = source_for(scanned, workflow_path) {
        (workflow.path.clone(), anchor_line(workflow))
    } else {
        (PathBuf::from(workflow_path), 1)
    }
}

fn source_for<'a>(scanned: &'a [Workflow], workflow_path: &str) -> Option<&'a Workflow> {
    scanned.iter().find(|workflow| {
        workflow
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                workflow_paths_match(workflow_path, &format!(".github/workflows/{name}"))
            })
    })
}

/// Workflow `concurrency:` line, else the earliest job `concurrency:` line, else `on:`.
fn anchor_line(workflow: &Workflow) -> usize {
    if let Some(line) = workflow.concurrency_line {
        return line;
    }
    if let Some(line) = workflow
        .jobs
        .iter()
        .filter_map(|job| job.concurrency_line)
        .min()
    {
        return line;
    }
    workflow.on_line.unwrap_or(1)
}

fn placement(
    workflows: &[WorkflowTotal],
    path: &str,
    repo: Option<&str>,
) -> (String, Option<String>) {
    if let Some(row) = match_ranked(workflows, path, repo) {
        return (row.workflow_path.clone(), Some(row.repository.clone()));
    }
    (linked(path), repo.map(str::to_string))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Job;
    use crate::money::Micros;
    use crate::rank::rank;
    use crate::report::render_text;
    use crate::usage::UsageRow;
    use crate::workflow::scan_yaml;
    use std::path::Path;

    const PATH: &str = ".github/workflows/ci.yml";
    const REPO: &str = "acme/widgets";
    const EXACT: &str = "cancel-in-progress would save 0.012 USD (2 minutes)";
    const ON_YAML: &str = "\
on: push
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - run: echo test
";

    fn usd(text: &str) -> Micros {
        Micros::parse(text).unwrap()
    }

    fn job(
        id: u64,
        run_id: u64,
        labels: &[&str],
        started: &str,
        completed: &str,
        conclusion: &str,
    ) -> Job {
        Job {
            id,
            run_id,
            run_attempt: 1,
            name: "test".to_string(),
            labels: labels.iter().map(|label| (*label).to_string()).collect(),
            started_at: Some(started.to_string()),
            completed_at: Some(completed.to_string()),
            conclusion: Some(conclusion.to_string()),
            runner_name: None,
        }
    }

    fn run_of(
        id: u64,
        path: &str,
        repository: Option<&str>,
        conclusion: Option<&str>,
        started: Option<&str>,
        branch: Option<&str>,
        pulls: &[u64],
    ) -> WorkflowRun {
        WorkflowRun {
            id,
            path: path.to_string(),
            repository: repository.map(str::to_string),
            run_attempt: 1,
            run_number: id,
            event: "pull_request".to_string(),
            conclusion: conclusion.map(str::to_string),
            run_started_at: started.map(str::to_string),
            head_branch: branch.map(str::to_string),
            pull_requests: pulls.to_vec(),
        }
    }

    fn canonical() -> ApiData {
        ApiData {
            public: false,
            runs: vec![
                run_of(
                    1,
                    PATH,
                    Some(REPO),
                    Some("success"),
                    Some("2026-04-01T00:00:00Z"),
                    Some("feature"),
                    &[7],
                ),
                run_of(
                    2,
                    PATH,
                    Some(REPO),
                    Some("success"),
                    Some("2026-04-01T00:01:00Z"),
                    Some("feature"),
                    &[7],
                ),
            ],
            jobs: vec![
                job(
                    10,
                    1,
                    &["ubuntu-latest"],
                    "2026-04-01T00:00:00Z",
                    "2026-04-01T00:03:00Z",
                    "success",
                ),
                job(
                    11,
                    2,
                    &["ubuntu-latest"],
                    "2026-04-01T00:01:00Z",
                    "2026-04-01T00:02:00Z",
                    "success",
                ),
            ],
        }
    }

    fn scanned(yaml: &str) -> Vec<Workflow> {
        vec![scan_yaml(Path::new("repo/.github/workflows/ci.yml"), yaml).unwrap()]
    }

    fn messages(api: &ApiData, yaml: &str) -> Vec<String> {
        findings(api, &[], &scanned(yaml))
            .into_iter()
            .map(|item| format!("{}:{}: {}", item.path.display(), item.line, item.message))
            .collect()
    }

    fn only_message(api: &ApiData, yaml: &str) -> String {
        let found = findings(api, &[], &scanned(yaml));
        assert_eq!(found.len(), 1, "{found:?}");
        found[0].message.clone()
    }

    #[test]
    fn overlap_on_the_on_line_is_two_linux_minutes() {
        let workflow = scan_yaml(Path::new("ci.yml"), ON_YAML).unwrap();
        assert_eq!(workflow.on_line, Some(1));
        assert_eq!(workflow.concurrency_line, None);
        let found = findings(&canonical(), &[], &scanned(ON_YAML));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].kind.code(), "F3$");
        assert_eq!(found[0].savings, Micros::parse("0.012").unwrap());
        assert_eq!(found[0].method, crate::findings::SavingsMethod::Overlap);
        assert_eq!(found[0].line, 1);
        assert_eq!(found[0].message, EXACT);
        assert!(
            found[0].path.ends_with("ci.yml"),
            "{}",
            found[0].path.display()
        );
        assert_eq!(found[0].repository.as_deref(), Some(REPO));
        assert_eq!(found[0].workflow_path.as_deref(), Some(PATH));

        let mut reversed = canonical();
        reversed.runs.reverse();
        reversed.jobs.reverse();
        assert_eq!(only_message(&reversed, ON_YAML), EXACT);
        let crlf = ON_YAML.replace('\n', "\r\n");
        assert_eq!(only_message(&canonical(), &crlf), EXACT);
        assert_eq!(findings(&canonical(), &[], &scanned(&crlf))[0].line, 1);
    }

    #[test]
    fn overlap_uses_the_concurrency_line_when_the_file_has_one() {
        let yaml = "\
name: CI
on: push
concurrency:
  group: ci
  cancel-in-progress: false
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - run: echo test
";
        let workflow = scan_yaml(Path::new("ci.yml"), yaml).unwrap();
        assert_eq!(workflow.on_line, Some(2));
        assert_eq!(workflow.concurrency_line, Some(3));
        let found = findings(&canonical(), &[], &scanned(yaml));
        assert_eq!(found[0].line, 3, "{found:?}");
        assert_eq!(found[0].message, EXACT);

        let flow = "\
on: [push]
concurrency: {group: ci, cancel-in-progress: false}
jobs: {test: {runs-on: ubuntu-latest, timeout-minutes: 5, steps: [{run: echo test}]}}
";
        let flow_workflow = scan_yaml(Path::new("ci.yml"), flow).unwrap();
        assert_eq!(flow_workflow.concurrency_line, Some(2));
        assert_eq!(findings(&canonical(), &[], &scanned(flow))[0].line, 2);

        let job_level = "\
on: push
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    concurrency:
      group: ci
    steps:
      - run: echo test
";
        let job_workflow = scan_yaml(Path::new("ci.yml"), job_level).unwrap();
        assert_eq!(job_workflow.concurrency_line, None);
        assert_eq!(job_workflow.jobs[0].concurrency_line, Some(6));
        assert_eq!(findings(&canonical(), &[], &scanned(job_level))[0].line, 6);

        let capital = "\
on: push
Concurrency:
  group: ci
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - run: echo test
";
        let capital_workflow = scan_yaml(Path::new("ci.yml"), capital).unwrap();
        assert_eq!(capital_workflow.concurrency_line, Some(2));
        assert_eq!(findings(&canonical(), &[], &scanned(capital))[0].line, 2);

        let duplicate = "\
on: push
concurrency:
  cancel-in-progress: false
concurrency:
  group: ci
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 1
    steps:
      - run: echo test
";
        let duplicate_workflow = scan_yaml(Path::new("ci.yml"), duplicate).unwrap();
        assert_eq!(duplicate_workflow.concurrency_line, Some(4));
        assert_eq!(findings(&canonical(), &[], &scanned(duplicate))[0].line, 4);
    }

    #[test]
    fn a_cancelled_older_run_has_no_amount() {
        for conclusion in [
            "cancelled",
            "Cancelled",
            "CANCELLED",
            " cancelled ",
            "cancelled\r",
        ] {
            let mut api = canonical();
            api.runs[0].conclusion = Some(conclusion.to_string());
            assert!(messages(&api, ON_YAML).is_empty(), "{conclusion}: {api:?}");
        }
    }

    #[test]
    fn other_conclusions_and_a_cancelled_newer_run_still_count() {
        for conclusion in ["failure", "FAILURE", "timed_out", "neutral", ""] {
            let mut api = canonical();
            api.runs[0].conclusion = if conclusion.is_empty() {
                None
            } else {
                Some(conclusion.to_string())
            };
            assert_eq!(only_message(&api, ON_YAML), EXACT, "{conclusion}");
        }
        let mut newer_cancelled = canonical();
        newer_cancelled.runs[1].conclusion = Some("CANCELLED".to_string());
        assert_eq!(only_message(&newer_cancelled, ON_YAML), EXACT);
    }

    #[test]
    fn branch_and_pull_request_identity_covers_empty_and_capital_fields() {
        let mut missing = canonical();
        missing.runs[0].head_branch = None;
        missing.runs[1].head_branch = None;
        missing.runs[0].pull_requests.clear();
        missing.runs[1].pull_requests.clear();
        assert!(messages(&missing, ON_YAML).is_empty(), "{missing:?}");

        let mut blank_branch = canonical();
        blank_branch.runs[0].head_branch = Some("  \r".to_string());
        blank_branch.runs[1].head_branch = Some(String::new());
        blank_branch.runs[0].pull_requests = vec![7];
        blank_branch.runs[1].pull_requests = vec![7, 9];
        assert_eq!(only_message(&blank_branch, ON_YAML), EXACT);

        let mut branch_only = canonical();
        branch_only.runs[0].pull_requests = vec![1];
        branch_only.runs[1].pull_requests = vec![2];
        assert_eq!(only_message(&branch_only, ON_YAML), EXACT);

        let mut pr_only = canonical();
        pr_only.runs[0].head_branch = Some("feature".to_string());
        pr_only.runs[1].head_branch = Some("other".to_string());
        pr_only.runs[0].pull_requests = vec![7];
        pr_only.runs[1].pull_requests = vec![7];
        assert_eq!(only_message(&pr_only, ON_YAML), EXACT);

        let mut neither = canonical();
        neither.runs[1].head_branch = Some("other".to_string());
        neither.runs[1].pull_requests = vec![8];
        assert!(messages(&neither, ON_YAML).is_empty());

        let mut capitals = canonical();
        capitals.runs[0].head_branch = Some(" FEATURE ".to_string());
        capitals.runs[1].head_branch = Some("feature".to_string());
        capitals.runs[0].path = "ACME/WIDGETS/.GitHub/Workflows/CI.yml@MAIN".to_string();
        capitals.runs[1].event = "PULL_REQUEST".to_string();
        assert_eq!(only_message(&capitals, ON_YAML), EXACT);

        let mut other_workflow = canonical();
        other_workflow.runs[1].path = ".github/workflows/lint.yml".to_string();
        assert!(messages(&other_workflow, ON_YAML).is_empty());

        let mut other_repo = canonical();
        other_repo.runs[1].repository = Some("other/app".to_string());
        assert!(messages(&other_repo, ON_YAML).is_empty());

        let mut one_repo = canonical();
        one_repo.runs[1].repository = None;
        assert_eq!(only_message(&one_repo, ON_YAML), EXACT);

        let mut same_case = canonical();
        same_case.runs[1].repository = Some("ACME/WIDGETS".to_string());
        assert_eq!(only_message(&same_case, ON_YAML), EXACT);
    }

    #[test]
    fn missing_times_and_equal_boundaries_have_no_amount() {
        let mut no_start = canonical();
        no_start.runs[0].run_started_at = None;
        assert!(messages(&no_start, ON_YAML).is_empty());

        let mut blank_start = canonical();
        blank_start.runs[1].run_started_at = Some("  ".to_string());
        assert!(messages(&blank_start, ON_YAML).is_empty());

        let mut open = canonical();
        open.jobs[0].completed_at = None;
        assert!(messages(&open, ON_YAML).is_empty());

        let mut garbage = canonical();
        garbage.jobs[0].completed_at = Some("yesterday".to_string());
        assert!(messages(&garbage, ON_YAML).is_empty());

        let mut equal = canonical();
        equal.runs[1].run_started_at = Some("2026-04-01T00:00:00Z".to_string());
        assert!(messages(&equal, ON_YAML).is_empty());

        let mut touches_end = canonical();
        touches_end.runs[1].run_started_at = Some("2026-04-01T00:03:00Z".to_string());
        assert!(messages(&touches_end, ON_YAML).is_empty());

        let mut inside_minute = canonical();
        inside_minute.jobs[0].completed_at = Some("2026-04-01T00:02:00Z".to_string());
        inside_minute.runs[1].run_started_at = Some("2026-04-01T00:01:59Z".to_string());
        assert!(
            messages(&inside_minute, ON_YAML).is_empty(),
            "{inside_minute:?}"
        );
    }

    #[test]
    fn partial_minutes_use_the_difference_in_billed_minutes() {
        let mut api = canonical();
        api.jobs[0].completed_at = Some("2026-04-01T00:02:00Z".to_string());
        api.runs[1].run_started_at = Some("2026-04-01T00:00:30Z".to_string());
        api.jobs[1].started_at = Some("2026-04-01T00:00:30Z".to_string());
        assert_eq!(
            only_message(&api, ON_YAML),
            "cancel-in-progress would save 0.006 USD (1 minute)"
        );
    }

    #[test]
    fn parallel_runners_and_jobs_that_have_not_started_add_up() {
        let mut api = canonical();
        api.jobs = vec![
            job(
                10,
                1,
                &["ubuntu-latest"],
                "2026-04-01T00:00:00Z",
                "2026-04-01T00:05:00Z",
                "success",
            ),
            job(
                12,
                1,
                &["macos-latest"],
                "2026-04-01T00:00:00Z",
                "2026-04-01T00:02:00Z",
                "SUCCESS",
            ),
        ];
        assert_eq!(
            only_message(&api, ON_YAML),
            "cancel-in-progress would save 0.086 USD (5 minutes)"
        );

        api.jobs = vec![
            job(
                10,
                1,
                &["UBUNTU-LATEST"],
                "2026-04-01T00:00:00Z",
                "2026-04-01T00:04:00Z",
                "success",
            ),
            job(
                12,
                1,
                &["ubuntu-latest"],
                "2026-04-01T00:02:00Z",
                "2026-04-01T00:02:00Z",
                "success",
            ),
        ];
        assert_eq!(
            only_message(&api, ON_YAML),
            "cancel-in-progress would save 0.024 USD (4 minutes)"
        );
    }

    #[test]
    fn the_first_later_run_is_the_only_cancel_point() {
        let api = ApiData {
            public: false,
            runs: vec![
                run_of(
                    3,
                    PATH,
                    Some(REPO),
                    Some("success"),
                    Some("2026-04-01T00:06:00Z"),
                    Some("feature"),
                    &[7],
                ),
                run_of(
                    1,
                    PATH,
                    Some(REPO),
                    Some("success"),
                    Some("2026-04-01T00:00:00Z"),
                    Some("feature"),
                    &[7],
                ),
                run_of(
                    2,
                    PATH,
                    Some(REPO),
                    Some("success"),
                    Some("2026-04-01T00:03:00Z"),
                    Some("feature"),
                    &[7],
                ),
            ],
            jobs: vec![
                job(
                    10,
                    1,
                    &["ubuntu-latest"],
                    "2026-04-01T00:00:00Z",
                    "2026-04-01T00:10:00Z",
                    "success",
                ),
                job(
                    11,
                    2,
                    &["ubuntu-latest"],
                    "2026-04-01T00:03:00Z",
                    "2026-04-01T00:09:00Z",
                    "success",
                ),
                job(
                    12,
                    3,
                    &["ubuntu-latest"],
                    "2026-04-01T00:06:00Z",
                    "2026-04-01T00:08:00Z",
                    "success",
                ),
            ],
        };
        assert_eq!(
            only_message(&api, ON_YAML),
            "cancel-in-progress would save 0.06 USD (10 minutes)"
        );
    }

    #[test]
    fn copies_skipped_jobs_and_unknown_labels_do_not_add_minutes() {
        let mut copied = job(
            1,
            1,
            &["ubuntu-latest"],
            "2026-04-01T00:00:00Z",
            "2026-04-01T00:20:00Z",
            "success",
        );
        copied.run_attempt = 2;
        let mut real = job(
            2,
            1,
            &["ubuntu-latest"],
            "2026-04-01T00:10:00Z",
            "2026-04-01T00:13:00Z",
            "success",
        );
        real.run_attempt = 2;
        let mut api = canonical();
        api.runs[0].run_attempt = 2;
        api.runs[0].run_started_at = Some("2026-04-01T00:10:00Z".to_string());
        api.runs[1].run_started_at = Some("2026-04-01T00:11:00Z".to_string());
        api.jobs = vec![copied, real];
        assert_eq!(only_message(&api, ON_YAML), EXACT);

        let mut skipped = canonical();
        skipped.jobs[0].conclusion = Some("skipped".to_string());
        assert!(messages(&skipped, ON_YAML).is_empty());

        let mut hosted = canonical();
        hosted.jobs[0].labels = vec!["self-hosted".to_string(), "macos-latest".to_string()];
        assert!(messages(&hosted, ON_YAML).is_empty());

        let mut unknown = canonical();
        unknown.jobs[0].labels = vec!["custom-runner".to_string()];
        assert!(messages(&unknown, ON_YAML).is_empty());

        let mut no_start = canonical();
        no_start.jobs[0].started_at = None;
        assert_eq!(
            only_message(&no_start, ON_YAML),
            "cancel-in-progress would save 0.006 USD (1 minute)"
        );
    }

    #[test]
    fn offsets_and_a_missing_workflow_file_keep_the_amount() {
        let mut api = canonical();
        api.runs[0].run_started_at = Some("2026-04-01T02:00:00+02:00\r".to_string());
        api.jobs[0].started_at = Some("2026-04-01T02:00:00+02:00".to_string());
        api.jobs[0].completed_at = Some("2026-04-01T02:03:00+02:00".to_string());
        assert_eq!(only_message(&api, ON_YAML), EXACT);

        let found = findings(&canonical(), &[], &[]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].line, 1);
        assert_eq!(found[0].path, PathBuf::from(PATH));
        assert_eq!(found[0].message, EXACT);
    }

    #[test]
    fn two_repositories_stay_separate() {
        let mut right = canonical();
        right.runs[0].id = 3;
        right.runs[1].id = 4;
        right.runs[0].repository = Some("other/app".to_string());
        right.runs[1].repository = Some("other/app".to_string());
        right.jobs[0].id = 20;
        right.jobs[0].run_id = 3;
        right.jobs[1].id = 21;
        right.jobs[1].run_id = 4;
        let mut api = canonical();
        api.runs.extend(right.runs);
        api.jobs.extend(right.jobs);
        let found = findings(&api, &[], &scanned(ON_YAML));
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found.iter().all(|item| item.message == EXACT));
        assert_eq!(found[0].repository.as_deref(), Some(REPO));
        assert_eq!(found[1].repository.as_deref(), Some("other/app"));
    }

    #[test]
    fn a_public_standard_runner_saves_nothing_and_a_larger_runner_keeps_its_rate() {
        let mut public = canonical();
        public.public = true;
        assert!(messages(&public, ON_YAML).is_empty(), "{public:?}");

        let mut larger = canonical();
        larger.public = true;
        larger.jobs[0].labels = vec!["ubuntu-24.04-16core".to_string()];
        larger.jobs[1].labels = vec!["UBUNTU-24.04-16CORE".to_string()];
        assert_eq!(
            only_message(&larger, ON_YAML),
            "cancel-in-progress would save 0.084 USD (2 minutes)"
        );
    }

    fn row(minutes: i64, net: &str) -> UsageRow {
        UsageRow {
            line: 2,
            date: "2026-04-01".to_string(),
            product: "actions".to_string(),
            sku: "actions_linux".to_string(),
            quantity: Micros(minutes * Micros::SCALE),
            unit_type: "minutes".to_string(),
            applied_cost_per_quantity: usd("0.100000"),
            gross: usd(net),
            discount: Micros::ZERO,
            net: usd(net),
            username: String::new(),
            organization: "acme".to_string(),
            repository: REPO.to_string(),
            workflow_path: PATH.to_string(),
        }
    }

    #[test]
    fn the_usage_rate_wins_over_the_table_including_on_a_public_repository() {
        let ranking = rank(&[row(10, "1.000000")], None);
        let scanned = scanned(ON_YAML);
        let found = findings(&canonical(), &ranking.workflows, &scanned);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            found[0].message,
            "cancel-in-progress would save 0.20 USD (2 minutes)"
        );
        let text = render_text(
            &ranking.workflows,
            &ranking.bill,
            Some(&found),
            None,
            None,
            &scanned,
        );
        assert!(
            text.contains("ci.yml:1: cancel-in-progress would save 0.20 USD (2 minutes)"),
            "{text}"
        );
        assert!(!text.contains("Workflows without usage"), "{text}");

        let mut public = canonical();
        public.public = true;
        let found = findings(&public, &ranking.workflows, &scanned);
        assert_eq!(
            found[0].message,
            "cancel-in-progress would save 0.20 USD (2 minutes)"
        );
    }
}
