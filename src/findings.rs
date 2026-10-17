use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::money;
use crate::usage::UsageRow;
use crate::workflow::{
    scan_yaml, Job, Matrix, RemoteWorkflow, ReusableCall, ReusableSource, RunsOn, ScanError,
    Workflow,
};

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
    /// Job calls `./.github/workflows/<file>` and that file is not in the scan.
    CalleeNotFound,
    /// A reusable workflow calls back into a workflow already being expanded.
    ReusableCycle,
    /// Remote reusable workflow. Shown on the `uses:` line when the cache has no file.
    RemoteWorkflow,
    /// Job-level `uses:` is not a local or remote reusable workflow.
    InvalidUses,
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
            Self::CalleeNotFound => "F6",
            Self::ReusableCycle => "F7",
            Self::RemoteWorkflow => "F10",
            Self::InvalidUses => "F11",
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
    /// Workflow path this finding is listed under.
    ///
    /// Invoice findings set this together with `repository`. Findings copied
    /// from a local reusable workflow set it to the caller
    /// (`.github/workflows/<file>`) and leave `repository` empty. `path` and
    /// `line` stay on the callee file.
    pub workflow_path: Option<String>,
}

const LINT_TOOLS: [&str; 5] = ["lint", "fmt", "clippy", "eslint", "prettier"];

/// Findings for the scanned workflows, ordered by path, line, and kind.
///
/// A job `uses: ./.github/workflows/<file>.yml` (or `.yaml`) loads that file
/// from this scan. A job `uses: owner/repo/.github/workflows/<file>.yml@ref`
/// loads `<owner>/<repo>/<path>@<ref>` from the runs cache. Findings for the
/// callee are listed on the caller. The finding file and line stay on the
/// callee. A missing local file, a call cycle, an invalid uses value, and a
/// remote call with no cache file are findings on the `uses:` line. Costs stay
/// on the caller.
pub fn findings(workflows: &[Workflow]) -> Vec<Finding> {
    findings_with_cache(workflows, None)
        .expect("findings without a runs directory do not read files")
}

/// [`findings`] plus remote reusable workflows stored under `runs`.
///
/// `runs` is the `--runs` directory. A missing callee file is a finding. An
/// unreadable or invalid cache file is an error and names that file.
pub fn findings_with_cache(
    workflows: &[Workflow],
    runs: Option<&Path>,
) -> Result<Vec<Finding>, ScanError> {
    let mut found = Vec::new();
    for workflow in workflows {
        emit_workflow_findings(&mut found, workflow, None);
        let mut stack = vec![CallFrame {
            path: workflow.path.clone(),
            label: display_name(&workflow.path).to_string(),
        }];
        let expand = Expand {
            workflows,
            root: workflow,
            caller_key: usage_key(&workflow.path),
            runs,
            repo: None,
        };
        expand_calls(&mut found, workflow, &mut stack, &expand)?;
    }
    dedupe_findings(&mut found);
    found.sort_by(cmp_finding);
    Ok(found)
}

fn emit_workflow_findings(found: &mut Vec<Finding>, workflow: &Workflow, caller_key: Option<&str>) {
    let start = found.len();
    if pulls_without_cancel(workflow) {
        if let Some(line) = workflow.on_line {
            push(
                found,
                &workflow.path,
                line,
                FindingKind::PullRequestConcurrency,
                pr_cancel_message(workflow),
            );
        }
    }
    for dup in &workflow.duplicate_keys {
        push(
            found,
            &workflow.path,
            dup.second,
            FindingKind::DuplicateKey,
            &duplicate_message(&dup.key, dup.first, dup.second),
        );
    }
    for job in &workflow.jobs {
        if let Some(invalid) = &job.invalid_uses {
            push_at(
                found,
                &workflow.path,
                invalid.line,
                FindingKind::InvalidUses,
                &invalid_uses_message(&job.name, &invalid.detail),
                Some(usage_key(&workflow.path)),
            );
        }
        if job.uses.is_none() && job.invalid_uses.is_none() {
            note_job(found, &workflow.path, job);
        }
    }
    if let Some(key) = caller_key {
        for item in &mut found[start..] {
            item.workflow_path = Some(key.to_string());
        }
    }
}

fn duplicate_message(key: &str, first: usize, second: usize) -> String {
    format!("duplicate key {key} (line {first} and {second})")
}

struct CallFrame {
    path: PathBuf,
    label: String,
}

struct Expand<'a> {
    workflows: &'a [Workflow],
    root: &'a Workflow,
    caller_key: String,
    runs: Option<&'a Path>,
    /// Set while expanding a file loaded from the runs cache.
    repo: Option<RemoteWorkflow>,
}

impl Expand<'_> {
    fn with_repo(&self, repo: RemoteWorkflow) -> Expand<'_> {
        Expand {
            workflows: self.workflows,
            root: self.root,
            caller_key: self.caller_key.clone(),
            runs: self.runs,
            repo: Some(repo),
        }
    }
}

/// What to record when a cache file is absent.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CacheMiss {
    /// Remote call: list the job, the uses line, and the full uses text.
    ShowJob,
    /// Local call inside a cached repo: the callee file is not in the cache.
    NotFound,
}

/// Walk `uses:` from `current`, attributing every callee finding to `root`.
///
/// The stack is the chain of files already entered. A call back into that
/// chain records a cycle and returns, so a loop cannot run forever.
fn expand_calls(
    found: &mut Vec<Finding>,
    current: &Workflow,
    stack: &mut Vec<CallFrame>,
    exp: &Expand<'_>,
) -> Result<(), ScanError> {
    for job in &current.jobs {
        let Some(call) = &job.uses else {
            continue;
        };
        if let Some(matrix) = &job.matrix {
            if let Some(message) = matrix_message(matrix) {
                push_at(
                    found,
                    &current.path,
                    matrix.line,
                    FindingKind::MatrixJobs,
                    &message,
                    Some(exp.caller_key.clone()),
                );
            }
        }
        let site = CallSite { current, job, call };
        match &call.source {
            ReusableSource::Local => expand_local(found, site, stack, exp)?,
            ReusableSource::Remote(remote) => {
                enter_cached(found, site, stack, exp, remote, CacheMiss::ShowJob)?;
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct CallSite<'a> {
    current: &'a Workflow,
    job: &'a Job,
    call: &'a ReusableCall,
}

fn expand_local(
    found: &mut Vec<Finding>,
    site: CallSite<'_>,
    stack: &mut Vec<CallFrame>,
    exp: &Expand<'_>,
) -> Result<(), ScanError> {
    let CallSite { current, job, call } = site;
    if let Some(repo) = &exp.repo {
        let remote = RemoteWorkflow {
            owner: repo.owner.clone(),
            repo: repo.repo.clone(),
            workflow_path: format!(".github/workflows/{}", call.file_name),
            reference: repo.reference.clone(),
        };
        return enter_cached(
            found,
            CallSite { current, job, call },
            stack,
            exp,
            &remote,
            CacheMiss::NotFound,
        );
    }
    match find_callee(exp.workflows, &call.file_name) {
        None => push_at(
            found,
            &current.path,
            call.line,
            FindingKind::CalleeNotFound,
            &missing_callee_message(&job.name, &call.spec),
            Some(exp.caller_key.clone()),
        ),
        Some(callee) => {
            if stack.iter().any(|frame| frame.path == callee.path) {
                push_at(
                    found,
                    &current.path,
                    call.line,
                    FindingKind::ReusableCycle,
                    &cycle_message(stack, display_name(&callee.path)),
                    Some(exp.caller_key.clone()),
                );
                return Ok(());
            }
            emit_workflow_findings(found, callee, Some(&exp.caller_key));
            stack.push(CallFrame {
                path: callee.path.clone(),
                label: display_name(&callee.path).to_string(),
            });
            expand_calls(found, callee, stack, exp)?;
            stack.pop();
        }
    }
    Ok(())
}

fn enter_cached(
    found: &mut Vec<Finding>,
    site: CallSite<'_>,
    stack: &mut Vec<CallFrame>,
    exp: &Expand<'_>,
    remote: &RemoteWorkflow,
    miss: CacheMiss,
) -> Result<(), ScanError> {
    let CallSite { current, job, call } = site;
    let Some(runs) = exp.runs else {
        note_cache_miss(found, site, exp, miss);
        return Ok(());
    };
    let Some(path) = find_cached_file(runs, &remote.cache_relative())? else {
        note_cache_miss(found, site, exp, miss);
        return Ok(());
    };
    if stack.iter().any(|frame| frame.path == path) {
        push_at(
            found,
            &current.path,
            call.line,
            FindingKind::ReusableCycle,
            &cycle_message(stack, &call.spec),
            Some(exp.caller_key.clone()),
        );
        return Ok(());
    }
    let callee = load_cached(&path)?;
    let before = found.len();
    emit_workflow_findings(found, &callee, Some(&exp.caller_key));
    stack.push(CallFrame {
        path: callee.path.clone(),
        label: call.spec.clone(),
    });
    let nested = exp.with_repo(remote.clone());
    expand_calls(found, &callee, stack, &nested)?;
    stack.pop();
    if miss == CacheMiss::ShowJob && found.len() == before {
        show_remote_job(found, CallSite { current, job, call }, exp);
    }
    Ok(())
}

fn note_cache_miss(
    found: &mut Vec<Finding>,
    site: CallSite<'_>,
    exp: &Expand<'_>,
    miss: CacheMiss,
) {
    match miss {
        CacheMiss::ShowJob => show_remote_job(found, site, exp),
        CacheMiss::NotFound => push_at(
            found,
            &site.current.path,
            site.call.line,
            FindingKind::CalleeNotFound,
            &missing_callee_message(&site.job.name, &site.call.spec),
            Some(exp.caller_key.clone()),
        ),
    }
}

fn show_remote_job(found: &mut Vec<Finding>, site: CallSite<'_>, exp: &Expand<'_>) {
    push_at(
        found,
        &site.current.path,
        site.call.line,
        FindingKind::RemoteWorkflow,
        &remote_job_message(&site.job.name, &site.call.spec),
        Some(exp.caller_key.clone()),
    );
}

fn remote_job_message(job: &str, spec: &str) -> String {
    format!("job {job} uses {spec}")
}

fn invalid_uses_message(job: &str, detail: &str) -> String {
    format!("invalid reusable workflow uses: job {job} uses {detail}")
}

fn find_cached_file(runs: &Path, relative: &Path) -> Result<Option<PathBuf>, ScanError> {
    if !runs.is_dir() {
        return Ok(None);
    }
    let exact = runs.join(relative);
    if exact.is_file() {
        return Ok(Some(exact));
    }
    let Some(parts) = relative_parts(relative) else {
        return Ok(None);
    };
    let mut current = runs.to_path_buf();
    for (index, part) in parts.iter().enumerate() {
        let want_file = index + 1 == parts.len();
        match child_ignore_ascii_case(&current, part, want_file)? {
            Some(next) => current = next,
            None => return Ok(None),
        }
    }
    if current.is_file() {
        Ok(Some(current))
    } else {
        Ok(None)
    }
}

fn relative_parts(relative: &Path) -> Option<Vec<std::ffi::OsString>> {
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_os_string()),
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => return None,
        }
    }
    Some(parts)
}

fn child_ignore_ascii_case(
    dir: &Path,
    name: &OsStr,
    want_file: bool,
) -> Result<Option<PathBuf>, ScanError> {
    let entries = fs::read_dir(dir).map_err(|_| ScanError::directory(dir))?;
    let mut exact = None;
    let mut folded = None;
    for entry in entries {
        let entry = entry.map_err(|_| ScanError::directory(dir))?;
        let entry_name = entry.file_name();
        if !entry_name.eq_ignore_ascii_case(name) {
            continue;
        }
        let path = entry.path();
        let matches_kind = if want_file {
            path.is_file()
        } else {
            path.is_dir()
        };
        if !matches_kind {
            continue;
        }
        if entry_name == name {
            exact = Some(path);
        } else if folded.is_none() {
            folded = Some(path);
        }
    }
    Ok(exact.or(folded))
}

fn load_cached(path: &Path) -> Result<Workflow, ScanError> {
    let text = fs::read_to_string(path).map_err(|_| ScanError::file(path))?;
    scan_yaml(path, &text)
}

fn missing_callee_message(job: &str, spec: &str) -> String {
    format!("callee not found: job {job} uses {spec}")
}

fn cycle_message(stack: &[CallFrame], next: &str) -> String {
    let mut names: Vec<&str> = stack.iter().map(|frame| frame.label.as_str()).collect();
    names.push(next);
    let chain = names.join(" -> ");
    format!("reusable workflow cycle: {chain}")
}

fn display_name(path: &Path) -> &str {
    file_name(path).unwrap_or("workflow")
}

fn usage_key(path: &Path) -> String {
    format!(".github/workflows/{}", display_name(path))
}

fn file_name(path: &Path) -> Option<&str> {
    path.file_name().and_then(|name| name.to_str())
}

fn find_callee<'a>(workflows: &'a [Workflow], file_name_wanted: &str) -> Option<&'a Workflow> {
    if let Some(exact) = workflows
        .iter()
        .find(|workflow| file_name(&workflow.path) == Some(file_name_wanted))
    {
        return Some(exact);
    }
    workflows.iter().find(|workflow| {
        file_name(&workflow.path).is_some_and(|name| name.eq_ignore_ascii_case(file_name_wanted))
    })
}

fn dedupe_findings(found: &mut Vec<Finding>) {
    let mut kept: Vec<Finding> = Vec::new();
    for item in found.drain(..) {
        if !kept.iter().any(|other| same_recorded(other, &item)) {
            kept.push(item);
        }
    }
    *found = kept;
}

fn same_recorded(left: &Finding, right: &Finding) -> bool {
    left.path == right.path
        && left.line == right.line
        && left.kind == right.kind
        && left.message == right.message
        && left.repository == right.repository
        && left.workflow_path == right.workflow_path
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
    push_at(found, path, line, kind, message, None);
}

fn push_at(
    found: &mut Vec<Finding>,
    path: &Path,
    line: usize,
    kind: FindingKind,
    message: &str,
    workflow_path: Option<String>,
) {
    found.push(Finding {
        path: path.to_path_buf(),
        line,
        kind,
        message: message.to_string(),
        repository: None,
        workflow_path,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::money::Micros;
    use crate::rank::rank;
    use crate::report::{render, render_json};
    use crate::usage::{read_usage, UsageRow};
    use crate::workflow::{scan_dir, scan_yaml, RemoteWorkflow, ReusableSource, Workflow};
    use std::fs;
    use std::path::{Path, PathBuf};

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

    const LINT_CALLEE: &str = "\
name: Lint
on: workflow_call
jobs:
  lint:
    runs-on: macos-latest
    timeout-minutes: 10
    steps:
      - run: cargo fmt --check
";

    const MATRIX_CALLEE: &str = "\
name: Wide
on: workflow_call
jobs:
  build:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    strategy:
      matrix:
        os: [ubuntu-latest, windows-latest]
        node: [14, 16, 18]
    steps:
      - run: echo build
";

    fn workflows_dir(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join("ci-bill-b4a").join(name);
        let _ = fs::remove_dir_all(&root);
        let dir = root.join(".github").join("workflows");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_workflow(dir: &Path, name: &str, text: &str, crlf_file: bool, bom: bool) {
        let mut body = if crlf_file {
            text.replace('\n', "\r\n")
        } else {
            text.to_string()
        };
        if bom {
            body.insert(0, '\u{feff}');
        }
        fs::write(dir.join(name), body).unwrap();
    }

    fn load_dir(dir: &Path) -> (Vec<Workflow>, Vec<Finding>) {
        let workflows = scan_dir(dir).expect("scan workflows");
        let found = findings(&workflows);
        (workflows, found)
    }

    fn job_named<'a>(workflows: &'a [Workflow], file: &str, job: &str) -> &'a crate::workflow::Job {
        let workflow = workflows
            .iter()
            .find(|item| item.path.file_name().and_then(|name| name.to_str()) == Some(file))
            .unwrap_or_else(|| panic!("missing {file}"));
        workflow
            .jobs
            .iter()
            .find(|item| item.name == job)
            .unwrap_or_else(|| panic!("missing job {job} in {file}"))
    }

    fn caller_key(file: &str) -> String {
        format!(".github/workflows/{file}")
    }

    fn attributed<'a>(
        found: &'a [Finding],
        caller_file: &str,
        kind: FindingKind,
    ) -> Vec<&'a Finding> {
        let key = caller_key(caller_file);
        found
            .iter()
            .filter(|item| item.kind == kind && item.workflow_path.as_deref() == Some(key.as_str()))
            .collect()
    }

    fn usage(path: &str, net: &str, price: &str) -> UsageRow {
        UsageRow {
            line: 2,
            date: "2026-09-01".into(),
            product: "actions".into(),
            sku: "actions_linux".into(),
            quantity: Micros::parse("10").unwrap(),
            unit_type: "minutes".into(),
            applied_cost_per_quantity: Micros::parse(price).unwrap(),
            gross: Micros::parse(net).unwrap(),
            discount: Micros::ZERO,
            net: Micros::parse(net).unwrap(),
            username: "ci".into(),
            organization: "example-org".into(),
            repository: "example-org/app".into(),
            workflow_path: path.into(),
        }
    }

    fn report_for(found: &[Finding], rows: &[UsageRow]) -> (String, serde_json::Value) {
        let ranking = rank(rows, None);
        let text = render(&ranking.workflows, &ranking.bill, Some(found), None);
        let json: serde_json::Value = serde_json::from_str(
            render_json(&ranking.workflows, &ranking.bill, Some(found), None, None).trim_end(),
        )
        .expect("json report");
        (text, json)
    }

    fn section_after<'a>(text: &'a str, label: &str) -> &'a str {
        let start = text
            .find(label)
            .unwrap_or_else(|| panic!("missing {label} in {text}"));
        &text[start..]
    }

    fn assert_no_timeout_on_call(found: &[Finding], workflow_file: &str, job_line: usize) {
        assert!(
            found.iter().all(|item| {
                !(item.kind == FindingKind::MissingTimeout
                    && item.line == job_line
                    && item.path.ends_with(workflow_file))
            }),
            "uses job must not be flagged for the default timeout: {found:?}"
        );
    }

    #[test]
    fn local_callee_macos_lint_is_f1_under_caller() {
        let callee_runs_on = 5;
        let cases = [
            ("lint-plain", "caller.yml", "lint.yml", "uses: ./.github/workflows/lint.yml\n", false, false, 5),
            ("lint-yaml", "caller.yaml", "lint.yaml", "uses: ./.github/workflows/lint.yaml\n", false, false, 5),
            ("lint-with", "caller.yml", "lint.yml", "uses: ./.github/workflows/lint.yml\nwith:\n  extra: \"1\"\n", false, false, 5),
            ("lint-secrets-inherit", "caller.yml", "lint.yml", "uses: ./.github/workflows/lint.yml\nsecrets: inherit\n", false, false, 5),
            ("lint-secrets-map", "caller.yml", "lint.yml", "uses: ./.github/workflows/lint.yml\nsecrets:\n  token: value\n", false, false, 5),
            ("lint-with-and-secrets", "caller.yml", "lint.yml", "uses: ./.github/workflows/lint.yml\nwith:\n  extra: \"1\"\nsecrets: inherit\n", false, false, 5),
            ("lint-empty-with-secrets", "caller.yml", "lint.yml", "uses: ./.github/workflows/lint.yml\nwith: {}\nsecrets: {}\n", false, false, 5),
            ("lint-secrets-before-uses", "caller.yml", "lint.yml", "secrets: inherit\nwith:\n  extra: \"1\"\nuses: ./.github/workflows/lint.yml\n", false, false, 8),
            ("lint-with-before-uses", "caller.yml", "lint.yml", "with:\n  extra: \"1\"\nuses: ./.github/workflows/lint.yml\nsecrets:\n  token: value\n", false, false, 7),
            ("lint-quoted", "caller.yml", "lint.yml", "uses: \"./.github/workflows/lint.yml\"\n", false, false, 5),
            ("lint-single-quoted", "caller.yml", "lint.yml", "uses: './.github/workflows/lint.yml'\n", false, false, 5),
            ("lint-capitals", "caller.yml", "lint.yml", "uses: ./.GitHub/Workflows/lint.YML\n", false, false, 5),
            ("lint-backslash", "caller.yml", "lint.yml", "uses: ./.github\\workflows\\lint.yml\n", false, false, 5),
            ("lint-dot-segments", "caller.yml", "lint.yml", "uses: ./.github/workflows/../workflows/./lint.yml\n", false, false, 5),
            ("lint-double-slash", "caller.yml", "lint.yml", "uses: ./.github//workflows/lint.yml\n", false, false, 5),
            ("lint-crlf", "caller.yml", "lint.yml", "uses: ./.github/workflows/lint.yml\nwith:\n  extra: \"1\"\nsecrets: inherit\n", true, false, 5),
            ("lint-bom", "caller.yml", "lint.yml", "uses: ./.github/workflows/lint.yml\n", false, true, 5),
            ("lint-block-scalar", "caller.yml", "lint.yml", "uses: |\n  ./.github/workflows/lint.yml\n", false, false, 5),
        ];
        for (name, caller_file, callee_file, job_body, crlf_file, bom, uses_line) in cases {
            let dir = workflows_dir(name);
            let caller = format!("name: Caller\non: push\njobs:\n  call-lint:\n{job_body}");
            let caller = indent_job_body(&caller);
            write_workflow(&dir, caller_file, &caller, crlf_file, false);
            write_workflow(&dir, callee_file, LINT_CALLEE, crlf_file, bom);
            let (workflows, found) = load_dir(&dir);
            let call = job_named(&workflows, caller_file, "call-lint");
            assert!(call.uses.is_some(), "{name}");
            assert_eq!(call.uses.as_ref().unwrap().line, uses_line, "{name}");
            assert_no_timeout_on_call(&found, caller_file, call.line);
            let f1 = attributed(&found, caller_file, FindingKind::LintOnRunner);
            assert_eq!(f1.len(), 1, "{name}: {found:?}");
            assert_eq!(f1[0].line, callee_runs_on, "{name}");
            assert!(
                f1[0].path.ends_with(callee_file),
                "{name}: {}",
                f1[0].path.display()
            );
            assert_eq!(f1[0].message, "lint on macOS", "{name}");
            let rows = [usage(&caller_key(caller_file), "0.06", "0.006")];
            let (text, json) = report_for(&found, &rows);
            let label = format!("example-org/app .github/workflows/{caller_file}");
            let section = section_after(&text, &label);
            let needle = format!(
                "F1 {}:{}: lint on macOS",
                f1[0].path.display(),
                callee_runs_on
            );
            assert!(section.contains(&needle), "{name}: {text}");
            assert!(!text.contains("Workflows without usage"), "{name}: {text}");
            assert_eq!(
                json["workflows"][0]["path"],
                caller_key(caller_file),
                "{name}"
            );
            assert_eq!(json["workflows"][0]["net"], "0.060000", "{name}");
            assert_eq!(json["workflows"].as_array().unwrap().len(), 1, "{name}");
            assert!(
                json["unattached_findings"].as_array().unwrap().is_empty(),
                "{name}"
            );
            let listed = json["workflows"][0]["findings"].as_array().unwrap();
            assert!(
                listed.iter().any(|item| {
                    item["id"] == "F1"
                        && item["line"] == callee_runs_on
                        && item["message"] == "lint on macOS"
                        && item["file"].as_str().unwrap().ends_with(callee_file)
                }),
                "{name}: {listed:?}"
            );
        }

        let flow_dir = workflows_dir("lint-flow");
        write_workflow(
            &flow_dir,
            "caller.yml",
            "on: push\njobs: {call-lint: {uses: ./.github/workflows/lint.yml, with: {extra: \"1\"}, secrets: inherit}}\n",
            false,
            false,
        );
        write_workflow(
            &flow_dir,
            "lint.yml",
            "on: workflow_call\njobs: {lint: {runs-on: macos-latest, timeout-minutes: 10, steps: [{run: cargo fmt --check}]}}\n",
            false,
            false,
        );
        let (workflows, found) = load_dir(&flow_dir);
        let call = job_named(&workflows, "caller.yml", "call-lint");
        let callee = job_named(&workflows, "lint.yml", "lint");
        let runs_on = callee.runs_on.as_ref().unwrap().line;
        let f1 = attributed(&found, "caller.yml", FindingKind::LintOnRunner);
        assert_eq!(f1.len(), 1, "{found:?}");
        assert_eq!(f1[0].line, runs_on);
        assert_eq!(
            call.uses.as_ref().unwrap().spec,
            "./.github/workflows/lint.yml"
        );
        let (text, _) = report_for(&found, &[usage(&caller_key("caller.yml"), "0.06", "0.006")]);
        assert!(text.contains("lint on macOS"), "{text}");
        assert!(text.contains(&format!("lint.yml:{runs_on}:")), "{text}");
        assert!(!text.contains("Workflows without usage"), "{text}");

        let nested = workflows_dir("lint-nested");
        write_workflow(
            &nested,
            "caller.yml",
            "name: Caller\non: push\njobs:\n  call-mid:\n    uses: ./.github/workflows/mid.yml\n    secrets: inherit\n",
            false,
            false,
        );
        write_workflow(
            &nested,
            "mid.yml",
            "name: Mid\non: workflow_call\njobs:\n  forward:\n    with:\n      extra: \"1\"\n    secrets:\n      token: value\n    uses: ./.github/workflows/lint.yml\n",
            false,
            false,
        );
        write_workflow(&nested, "lint.yml", LINT_CALLEE, false, false);
        let (workflows, found) = load_dir(&nested);
        let f1 = attributed(&found, "caller.yml", FindingKind::LintOnRunner);
        assert_eq!(f1.len(), 1, "{found:?}");
        assert_eq!(f1[0].line, callee_runs_on);
        assert!(f1[0].path.ends_with("lint.yml"), "{}", f1[0].path.display());
        assert_no_timeout_on_call(
            &found,
            "caller.yml",
            job_named(&workflows, "caller.yml", "call-mid").line,
        );
        assert_no_timeout_on_call(
            &found,
            "mid.yml",
            job_named(&workflows, "mid.yml", "forward").line,
        );
        let (text, _) = report_for(&found, &[usage(&caller_key("caller.yml"), "0.06", "0.006")]);
        assert!(
            section_after(&text, "example-org/app .github/workflows/caller.yml")
                .contains("lint.yml:5: lint on macOS"),
            "{text}"
        );
        assert!(!text.contains("Workflows without usage"), "{text}");

        let anchor = workflows_dir("lint-anchor");
        write_workflow(
            &anchor,
            "caller.yml",
            "call: &call\n  uses: ./.github/workflows/missing.yml\njobs:\n  call-lint:\n    <<: *call\n    uses: ./.github/workflows/lint.yml\n    secrets: inherit\n",
            false,
            false,
        );
        write_workflow(&anchor, "lint.yml", LINT_CALLEE, false, false);
        let (workflows, found) = load_dir(&anchor);
        let call = job_named(&workflows, "caller.yml", "call-lint");
        assert_eq!(
            call.uses.as_ref().unwrap().spec,
            "./.github/workflows/lint.yml"
        );
        assert!(
            attributed(&found, "caller.yml", FindingKind::CalleeNotFound).is_empty(),
            "{found:?}"
        );
        assert!(
            attributed(&found, "caller.yml", FindingKind::DuplicateKey).is_empty(),
            "{found:?}"
        );
        let f1 = attributed(&found, "caller.yml", FindingKind::LintOnRunner);
        assert_eq!(f1.len(), 1, "{found:?}");
        assert_eq!(f1[0].message, "lint on macOS");

        let windows = workflows_dir("lint-windows");
        write_workflow(
            &windows,
            "caller.yml",
            "name: Caller\non: push\njobs:\n  call-lint:\n    uses: ./.github/workflows/lint.yml\n",
            false,
            false,
        );
        write_workflow(
            &windows,
            "lint.yml",
            "name: Lint\non: workflow_call\njobs:\n  fmt:\n    runs-on: windows-latest\n    timeout-minutes: 10\n    steps:\n      - run: cargo fmt --check\n",
            false,
            false,
        );
        let (_, found) = load_dir(&windows);
        let f1 = attributed(&found, "caller.yml", FindingKind::LintOnRunner);
        assert_eq!(f1.len(), 1, "{found:?}");
        assert_eq!(f1[0].line, 5);
        assert_eq!(f1[0].message, "lint on Windows");

        let hosted = workflows_dir("lint-self-hosted");
        write_workflow(
            &hosted,
            "caller.yml",
            "name: Caller\non: push\njobs:\n  call-lint:\n    uses: ./.github/workflows/lint.yml\n",
            false,
            false,
        );
        write_workflow(
            &hosted,
            "lint.yml",
            "name: Lint\non: workflow_call\njobs:\n  lint:\n    runs-on: [self-hosted, macos]\n    timeout-minutes: 5\n    steps:\n      - run: cargo fmt --check\n",
            false,
            false,
        );
        let (_, found) = load_dir(&hosted);
        assert!(
            attributed(&found, "caller.yml", FindingKind::LintOnRunner).is_empty(),
            "{found:?}"
        );
        assert!(
            attributed(&found, "caller.yml", FindingKind::CalleeNotFound).is_empty(),
            "{found:?}"
        );

        let step = workflows_dir("lint-step-uses");
        write_workflow(
            &step,
            "caller.yml",
            "name: Caller\non: push\njobs:\n  lint:\n    runs-on: ubuntu-latest\n    timeout-minutes: 5\n    steps:\n      - uses: ./.github/workflows/lint.yml\n",
            false,
            false,
        );
        write_workflow(&step, "lint.yml", LINT_CALLEE, false, false);
        let (workflows, found) = load_dir(&step);
        assert!(job_named(&workflows, "caller.yml", "lint").uses.is_none());
        assert!(
            attributed(&found, "caller.yml", FindingKind::LintOnRunner).is_empty(),
            "{found:?}"
        );
        assert!(
            found
                .iter()
                .all(|item| item.kind != FindingKind::CalleeNotFound),
            "{found:?}"
        );

        let rows = vec![
            usage(".github/workflows/caller.yml", "0.06", "0.006"),
            usage(".github/workflows/lint.yml", "1.00", "0.100"),
        ];
        let dir = workflows_dir("lint-both-rows");
        write_workflow(
            &dir,
            "caller.yml",
            "name: Caller\non: push\njobs:\n  call-lint:\n    uses: ./.github/workflows/lint.yml\n    with:\n      extra: \"1\"\n    secrets: inherit\n",
            false,
            false,
        );
        write_workflow(&dir, "lint.yml", LINT_CALLEE, false, false);
        let (_, found) = load_dir(&dir);
        let ranking = rank(&rows, None);
        assert_eq!(ranking.workflows.len(), 2);
        let caller = ranking
            .workflows
            .iter()
            .find(|item| item.workflow_path == ".github/workflows/caller.yml")
            .unwrap();
        let lint = ranking
            .workflows
            .iter()
            .find(|item| item.workflow_path == ".github/workflows/lint.yml")
            .unwrap();
        assert_eq!(caller.net, Micros::parse("0.06").unwrap());
        assert_eq!(lint.net, Micros::parse("1.00").unwrap());
        let (text, json) = report_for(&found, &rows);
        let caller_at = text
            .find("example-org/app .github/workflows/caller.yml")
            .unwrap();
        let lint_at = text
            .find("example-org/app .github/workflows/lint.yml")
            .unwrap();
        assert!(lint_at < caller_at, "{text}");
        let lint_section = &text[lint_at..caller_at];
        let caller_section = &text[caller_at..];
        assert!(lint_section.contains("1.00"), "{lint_section}");
        assert!(caller_section.contains("0.06"), "{caller_section}");
        assert!(
            caller_section.contains("lint.yml:5: lint on macOS"),
            "{text}"
        );
        assert!(lint_section.contains("lint.yml:5: lint on macOS"), "{text}");
        assert!(!caller_section.contains("1.00"), "{caller_section}");
        assert_eq!(json["workflows"].as_array().unwrap().len(), 2);
        let caller_json = json["workflows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["path"] == ".github/workflows/caller.yml")
            .unwrap();
        assert_eq!(caller_json["net"], "0.060000");
        assert!(caller_json["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == "F1" && item["line"] == 5));
    }

    /// Indent every line under the job key by four spaces.
    ///
    /// Nested lines in `body` already carry their own two-space indent, so
    /// they land six spaces in, under a job key that is two spaces in.
    fn indent_job_body(text: &str) -> String {
        let mut out = String::new();
        let mut job_started = false;
        for line in text.lines() {
            if !job_started {
                out.push_str(line);
                out.push('\n');
                if line.starts_with("  ")
                    && !line.starts_with("    ")
                    && line.trim_end().ends_with(':')
                {
                    job_started = true;
                }
                continue;
            }
            if line.is_empty() {
                out.push('\n');
                continue;
            }
            out.push_str("    ");
            out.push_str(line);
            out.push('\n');
        }
        out
    }

    #[test]
    fn missing_local_callee_is_visible_at_the_uses_line() {
        let cases = [
            (
                "miss-yml",
                "caller.yml",
                "uses: ./.github/workflows/missing.yml\n",
                false,
                5,
                "call-lint",
                "./.github/workflows/missing.yml",
            ),
            (
                "miss-yaml",
                "caller.yaml",
                "uses: ./.github/workflows/missing.yaml\n",
                false,
                5,
                "call-lint",
                "./.github/workflows/missing.yaml",
            ),
            (
                "miss-with",
                "caller.yml",
                "uses: ./.github/workflows/missing.yml\nwith:\n  extra: \"1\"\n",
                false,
                5,
                "call-lint",
                "./.github/workflows/missing.yml",
            ),
            (
                "miss-secrets",
                "caller.yml",
                "secrets: inherit\nuses: ./.github/workflows/missing.yml\n",
                false,
                6,
                "call-lint",
                "./.github/workflows/missing.yml",
            ),
            (
                "miss-both",
                "caller.yml",
                "with: {}\nsecrets: {}\nuses: ./.github/workflows/gone.yaml\n",
                false,
                7,
                "call-lint",
                "./.github/workflows/gone.yaml",
            ),
            (
                "miss-crlf",
                "caller.yml",
                "uses: ./.github/workflows/missing.yml\nsecrets:\n  token: value\n",
                true,
                5,
                "call-lint",
                "./.github/workflows/missing.yml",
            ),
            (
                "miss-capitals",
                "caller.yml",
                "uses: ./.GitHub/Workflows/Missing.YML\n",
                false,
                5,
                "call-lint",
                "./.GitHub/Workflows/Missing.YML",
            ),
            (
                "miss-quoted",
                "caller.yml",
                "uses: \"./.github/workflows/missing.yml\"\n",
                false,
                5,
                "call-lint",
                "./.github/workflows/missing.yml",
            ),
            (
                "miss-empty-file-name-not",
                "caller.yml",
                "uses: ./.github/workflows/no-such.yml\nwith:\nsecrets: inherit\n",
                false,
                5,
                "call-lint",
                "./.github/workflows/no-such.yml",
            ),
        ];
        for (name, caller_file, job_body, crlf_file, uses_line, job, spec) in cases {
            let dir = workflows_dir(name);
            let caller = format!("name: Caller\non: push\njobs:\n  {job}:\n{job_body}");
            let caller = indent_job_body(&caller);
            write_workflow(&dir, caller_file, &caller, crlf_file, false);
            let (workflows, found) = load_dir(&dir);
            let call = job_named(&workflows, caller_file, job);
            assert_eq!(
                call.uses.as_ref().unwrap().line,
                uses_line,
                "{name}\n{caller}"
            );
            assert_eq!(call.uses.as_ref().unwrap().spec, spec, "{name}");
            let missing = attributed(&found, caller_file, FindingKind::CalleeNotFound);
            assert_eq!(missing.len(), 1, "{name}: {found:?}");
            assert_eq!(missing[0].line, uses_line, "{name}");
            assert!(missing[0].path.ends_with(caller_file), "{name}");
            assert!(
                missing[0].message.starts_with("callee not found"),
                "{name}: {}",
                missing[0].message
            );
            assert!(
                missing[0].message.contains(job),
                "{name}: {}",
                missing[0].message
            );
            assert!(
                missing[0].message.contains(spec),
                "{name}: {}",
                missing[0].message
            );
            assert_eq!(missing[0].kind.code(), "F6");
            let (text, json) =
                report_for(&found, &[usage(&caller_key(caller_file), "0.06", "0.006")]);
            let section = section_after(
                &text,
                &format!("example-org/app .github/workflows/{caller_file}"),
            );
            let needle = format!(
                "F6 {}:{}: {}",
                missing[0].path.display(),
                uses_line,
                missing[0].message
            );
            assert!(section.contains(&needle), "{name}: {text}");
            assert!(section.contains(job), "{name}: {text}");
            assert!(!text.contains("Workflows without usage"), "{name}: {text}");
            let listed = json["workflows"][0]["findings"].as_array().unwrap();
            assert_eq!(listed[0]["id"], "F6", "{name}");
            assert_eq!(listed[0]["line"], uses_line, "{name}");
            assert!(
                listed[0]["message"]
                    .as_str()
                    .unwrap()
                    .contains("callee not found"),
                "{name}"
            );
            assert!(
                json["unattached_findings"].as_array().unwrap().is_empty(),
                "{name}"
            );
        }

        let nested = workflows_dir("miss-nested");
        write_workflow(
            &nested,
            "caller.yml",
            "name: Caller\non: push\njobs:\n  call-mid:\n    uses: ./.github/workflows/mid.yml\n    with:\n      extra: \"1\"\n",
            false,
            false,
        );
        write_workflow(
            &nested,
            "mid.yml",
            "name: Mid\non: workflow_call\njobs:\n  forward:\n    secrets: inherit\n    uses: ./.github/workflows/missing.yml\n",
            true,
            false,
        );
        let (workflows, found) = load_dir(&nested);
        let forward = job_named(&workflows, "mid.yml", "forward");
        let missing = attributed(&found, "caller.yml", FindingKind::CalleeNotFound);
        assert_eq!(missing.len(), 1, "{found:?}");
        assert_eq!(missing[0].line, forward.uses.as_ref().unwrap().line);
        assert!(
            missing[0].path.ends_with("mid.yml"),
            "{}",
            missing[0].path.display()
        );
        assert!(
            missing[0].message.contains("callee not found"),
            "{}",
            missing[0].message
        );
        assert!(
            missing[0].message.contains("forward"),
            "{}",
            missing[0].message
        );
        let (text, _) = report_for(&found, &[usage(&caller_key("caller.yml"), "0.06", "0.006")]);
        assert!(
            section_after(&text, "example-org/app .github/workflows/caller.yml")
                .contains("callee not found"),
            "{text}"
        );
        assert!(
            text.contains(&format!("mid.yml:{}:", forward.uses.as_ref().unwrap().line)),
            "{text}"
        );
        assert!(!text.contains("Workflows without usage"), "{text}");

        let empty = workflows_dir("miss-empty-callee");
        write_workflow(
            &empty,
            "caller.yml",
            "name: Caller\non: push\njobs:\n  call-lint:\n    uses: ./.github/workflows/empty.yml\n    secrets: inherit\n",
            false,
            false,
        );
        write_workflow(
            &empty,
            "empty.yml",
            "name: Empty\non: workflow_call\n",
            false,
            false,
        );
        let (workflows, found) = load_dir(&empty);
        assert!(job_named(&workflows, "caller.yml", "call-lint")
            .uses
            .is_some());
        assert!(
            found
                .iter()
                .all(|item| item.kind != FindingKind::CalleeNotFound),
            "{found:?}"
        );

        let ignored = workflows_dir("miss-not-local");
        write_workflow(
            &ignored,
            "caller.yml",
            "name: Caller\non: push\njobs:\n  blank:\n    uses: \"\"\n    timeout-minutes: 1\n  remote:\n    uses: owner/repo/.github/workflows/x.yml@main\n    timeout-minutes: 1\n  action:\n    uses: actions/checkout@v4\n    timeout-minutes: 1\n  other:\n    uses: ./foo.yml\n    timeout-minutes: 1\n  list:\n    uses:\n      - ./.github/workflows/lint.yml\n    timeout-minutes: 1\n",
            false,
            false,
        );
        let (_, found) = load_dir(&ignored);
        assert!(
            found
                .iter()
                .all(|item| item.kind != FindingKind::CalleeNotFound),
            "{found:?}"
        );
        assert!(
            found
                .iter()
                .all(|item| item.kind != FindingKind::ReusableCycle),
            "{found:?}"
        );
    }

    #[test]
    fn reusable_cycle_stops_with_a_finding() {
        let pairs = [
            ("cycle-yml", "a.yml", "b.yml", false),
            ("cycle-yaml", "a.yaml", "b.yaml", false),
            ("cycle-crlf", "a.yml", "b.yml", true),
        ];
        for (name, a_file, b_file, crlf_file) in pairs {
            let dir = workflows_dir(name);
            let a = format!(
                "name: A\non: push\njobs:\n  call-b:\n    uses: ./.github/workflows/{b_file}\n    with:\n      extra: \"1\"\n    secrets: inherit\n"
            );
            let b = format!(
                "name: B\non: workflow_call\njobs:\n  lint:\n    runs-on: macos-latest\n    timeout-minutes: 10\n    steps:\n      - run: cargo fmt --check\n  call-a:\n    secrets:\n      token: value\n    uses: ./.github/workflows/{a_file}\n"
            );
            write_workflow(&dir, a_file, &a, crlf_file, false);
            write_workflow(&dir, b_file, &b, crlf_file, false);
            let (workflows, found) = load_dir(&dir);
            assert!(found.len() < 12, "{name}: {found:?}");
            let call_b = job_named(&workflows, a_file, "call-b");
            let call_a = job_named(&workflows, b_file, "call-a");
            let cycles = attributed(&found, a_file, FindingKind::ReusableCycle);
            assert_eq!(cycles.len(), 1, "{name}: {found:?}");
            assert_eq!(cycles[0].line, call_a.uses.as_ref().unwrap().line, "{name}");
            assert!(cycles[0].path.ends_with(b_file), "{name}");
            assert_eq!(cycles[0].kind.code(), "F7");
            assert!(
                cycles[0].message.contains("reusable workflow cycle"),
                "{name}"
            );
            assert!(
                cycles[0].message.contains(a_file),
                "{name}: {}",
                cycles[0].message
            );
            assert!(
                cycles[0].message.contains(b_file),
                "{name}: {}",
                cycles[0].message
            );
            assert!(cycles[0].message.contains(" -> "), "{name}");
            let f1 = attributed(&found, a_file, FindingKind::LintOnRunner);
            assert_eq!(f1.len(), 1, "{name}: {found:?}");
            assert!(f1[0].path.ends_with(b_file), "{name}");
            assert_eq!(f1[0].message, "lint on macOS");
            assert_no_timeout_on_call(&found, a_file, call_b.line);
            assert_no_timeout_on_call(&found, b_file, call_a.line);
            let (text, json) = report_for(&found, &[usage(&caller_key(a_file), "0.06", "0.006")]);
            let section = section_after(
                &text,
                &format!("example-org/app .github/workflows/{a_file}"),
            );
            assert!(
                section.contains("reusable workflow cycle"),
                "{name}: {text}"
            );
            assert!(section.contains("lint on macOS"), "{name}: {text}");
            assert!(section.contains(&format!("{b_file}:")), "{name}: {text}");
            let listed = json["workflows"][0]["findings"].as_array().unwrap();
            assert!(
                listed.iter().any(|item| item["id"] == "F7"),
                "{name}: {listed:?}"
            );
            assert!(
                listed
                    .iter()
                    .any(|item| item["id"] == "F1"
                        && item["file"].as_str().unwrap().ends_with(b_file)),
                "{name}"
            );
        }

        let self_dir = workflows_dir("cycle-self");
        write_workflow(
            &self_dir,
            "loop.yml",
            "name: Loop\non: push\njobs:\n  again:\n    uses: ./.github/workflows/loop.yml\n    secrets: inherit\n",
            false,
            false,
        );
        let (workflows, found) = load_dir(&self_dir);
        assert!(found.len() < 6, "{found:?}");
        let again = job_named(&workflows, "loop.yml", "again");
        let cycles = attributed(&found, "loop.yml", FindingKind::ReusableCycle);
        assert_eq!(cycles.len(), 1, "{found:?}");
        assert_eq!(cycles[0].line, again.uses.as_ref().unwrap().line);
        assert!(
            cycles[0].message.contains("loop.yml -> loop.yml"),
            "{}",
            cycles[0].message
        );
        let (text, _) = report_for(&found, &[usage(&caller_key("loop.yml"), "0.06", "0.006")]);
        assert!(
            section_after(&text, "example-org/app .github/workflows/loop.yml")
                .contains("reusable workflow cycle"),
            "{text}"
        );

        let three = workflows_dir("cycle-three");
        write_workflow(
            &three,
            "a.yml",
            "name: A\non: push\njobs:\n  call-b:\n    uses: ./.github/workflows/b.yml\n",
            false,
            false,
        );
        write_workflow(
            &three,
            "b.yml",
            "name: B\non: workflow_call\njobs:\n  call-c:\n    uses: ./.github/workflows/c.yml\n    with:\n      extra: \"1\"\n",
            false,
            false,
        );
        write_workflow(
            &three,
            "c.yml",
            "name: C\non: workflow_call\njobs:\n  lint:\n    runs-on: macos-latest\n    timeout-minutes: 10\n    steps:\n      - run: cargo fmt --check\n  call-b:\n    uses: ./.github/workflows/b.yml\n    secrets: inherit\n",
            false,
            false,
        );
        let (_, found) = load_dir(&three);
        assert!(found.len() < 16, "{found:?}");
        let cycles = attributed(&found, "a.yml", FindingKind::ReusableCycle);
        assert_eq!(cycles.len(), 1, "{found:?}");
        assert!(
            cycles[0].path.ends_with("c.yml"),
            "{}",
            cycles[0].path.display()
        );
        assert!(cycles[0].message.contains("b.yml"), "{}", cycles[0].message);
        assert!(cycles[0].message.contains("c.yml"), "{}", cycles[0].message);
        let f1 = attributed(&found, "a.yml", FindingKind::LintOnRunner);
        assert_eq!(f1.len(), 1, "{found:?}");
        assert!(f1[0].path.ends_with("c.yml"));
        let (text, _) = report_for(&found, &[usage(&caller_key("a.yml"), "0.06", "0.006")]);
        assert!(
            section_after(&text, "example-org/app .github/workflows/a.yml")
                .contains("reusable workflow cycle"),
            "{text}"
        );
        assert!(text.contains("lint on macOS"), "{text}");

        let flow = workflows_dir("cycle-flow");
        write_workflow(
            &flow,
            "a.yml",
            "on: push\njobs: {call-b: {uses: ./.github/workflows/b.yml, secrets: inherit}}\n",
            false,
            false,
        );
        write_workflow(
            &flow,
            "b.yml",
            "on: workflow_call\njobs: {call-a: {with: {extra: \"1\"}, uses: ./.github/workflows/a.yml}}\n",
            false,
            false,
        );
        let (_, found) = load_dir(&flow);
        assert!(found.len() < 8, "{found:?}");
        let cycles = attributed(&found, "a.yml", FindingKind::ReusableCycle);
        assert_eq!(cycles.len(), 1, "{found:?}");
        assert!(cycles[0].message.contains("a.yml"), "{}", cycles[0].message);
        assert!(cycles[0].message.contains("b.yml"), "{}", cycles[0].message);
    }

    #[test]
    fn callee_matrix_is_reported_under_caller() {
        let canonical_matrix_line = 8;
        let cases = [
            (
                "matrix-plain",
                "caller.yml",
                "wide.yml",
                "uses: ./.github/workflows/wide.yml\n",
                false,
            ),
            (
                "matrix-yaml",
                "caller.yaml",
                "wide.yaml",
                "uses: ./.github/workflows/wide.yaml\n",
                false,
            ),
            (
                "matrix-with-secrets",
                "caller.yml",
                "wide.yml",
                "secrets: inherit\nwith:\n  extra: \"1\"\nuses: ./.github/workflows/wide.yml\n",
                false,
            ),
            (
                "matrix-crlf",
                "caller.yml",
                "wide.yml",
                "uses: ./.github/workflows/wide.yml\nwith: {}\nsecrets: {}\n",
                true,
            ),
            (
                "matrix-capitals",
                "caller.yml",
                "wide.yml",
                "uses: ./.GitHub/Workflows/wide.YML\n",
                false,
            ),
            (
                "matrix-quoted",
                "caller.yml",
                "wide.yml",
                "uses: \"./.github/workflows/wide.yml\"\n",
                false,
            ),
        ];
        for (name, caller_file, callee_file, job_body, crlf_file) in cases {
            let dir = workflows_dir(name);
            let caller = format!("name: Caller\non: push\njobs:\n  call-wide:\n{job_body}");
            write_workflow(
                &dir,
                caller_file,
                &indent_job_body(&caller),
                crlf_file,
                false,
            );
            write_workflow(&dir, callee_file, MATRIX_CALLEE, crlf_file, false);
            let (workflows, found) = load_dir(&dir);
            let matrix_line = job_named(&workflows, callee_file, "build")
                .matrix
                .as_ref()
                .unwrap()
                .line;
            if !crlf_file && job_body.starts_with("uses:") {
                assert_eq!(matrix_line, canonical_matrix_line, "{name}");
            }
            let f4 = attributed(&found, caller_file, FindingKind::MatrixJobs);
            assert_eq!(f4.len(), 1, "{name}: {found:?}");
            assert_eq!(f4[0].line, matrix_line, "{name}");
            assert!(f4[0].path.ends_with(callee_file), "{name}");
            assert_eq!(f4[0].message, f4_message(6), "{name}");
            assert_eq!(f4[0].kind.code(), "F4");
            let (text, json) =
                report_for(&found, &[usage(&caller_key(caller_file), "0.06", "0.006")]);
            let section = section_after(
                &text,
                &format!("example-org/app .github/workflows/{caller_file}"),
            );
            let needle = format!(
                "F4 {}:{}: {}",
                f4[0].path.display(),
                matrix_line,
                f4_message(6)
            );
            assert!(section.contains(&needle), "{name}: {text}");
            assert!(!text.contains("Workflows without usage"), "{name}: {text}");
            assert_eq!(json["workflows"][0]["net"], "0.060000", "{name}");
            let listed = json["workflows"][0]["findings"].as_array().unwrap();
            assert!(
                listed.iter().any(|item| {
                    item["id"] == "F4"
                        && item["line"] == matrix_line
                        && item["message"] == f4_message(6)
                        && item["file"].as_str().unwrap().ends_with(callee_file)
                }),
                "{name}: {listed:?}"
            );
        }

        let narrow = workflows_dir("matrix-narrow");
        write_workflow(
            &narrow,
            "caller.yml",
            "name: Caller\non: push\njobs:\n  call-wide:\n    uses: ./.github/workflows/wide.yml\n    secrets: inherit\n",
            false,
            false,
        );
        write_workflow(
            &narrow,
            "wide.yml",
            "name: Wide\non: workflow_call\njobs:\n  build:\n    runs-on: ubuntu-latest\n    timeout-minutes: 5\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16]\n",
            false,
            false,
        );
        let (workflows, found) = load_dir(&narrow);
        assert_eq!(
            job_named(&workflows, "wide.yml", "build")
                .matrix
                .as_ref()
                .unwrap()
                .jobs,
            4
        );
        assert!(
            attributed(&found, "caller.yml", FindingKind::MatrixJobs).is_empty(),
            "{found:?}"
        );
        assert!(
            attributed(&found, "caller.yml", FindingKind::CalleeNotFound).is_empty(),
            "{found:?}"
        );

        let flow = workflows_dir("matrix-flow");
        write_workflow(
            &flow,
            "caller.yml",
            "on: push\njobs: {call-wide: {uses: ./.github/workflows/wide.yml, with: {extra: \"1\"}, secrets: inherit}}\n",
            false,
            false,
        );
        write_workflow(
            &flow,
            "wide.yml",
            "on: workflow_call\njobs: {build: {runs-on: ubuntu-latest, timeout-minutes: 5, strategy: {matrix: {os: [ubuntu-latest, windows-latest], node: [14, 16, 18]}}}}\n",
            false,
            false,
        );
        let (workflows, found) = load_dir(&flow);
        let matrix_line = job_named(&workflows, "wide.yml", "build")
            .matrix
            .as_ref()
            .unwrap()
            .line;
        let f4 = attributed(&found, "caller.yml", FindingKind::MatrixJobs);
        assert_eq!(f4.len(), 1, "{found:?}");
        assert_eq!(f4[0].line, matrix_line);
        assert_eq!(f4[0].message, f4_message(6));
        let (text, _) = report_for(&found, &[usage(&caller_key("caller.yml"), "0.06", "0.006")]);
        assert!(text.contains(&format!("wide.yml:{matrix_line}:")), "{text}");
        assert!(text.contains(&f4_message(6)), "{text}");
        assert!(!text.contains("Workflows without usage"), "{text}");

        let at_least = workflows_dir("matrix-at-least");
        write_workflow(
            &at_least,
            "caller.yml",
            "name: Caller\non: push\njobs:\n  call-wide:\n    uses: ./.github/workflows/wide.yml\n",
            true,
            false,
        );
        write_workflow(
            &at_least,
            "wide.yml",
            "name: Wide\non: workflow_call\njobs:\n  build:\n    timeout-minutes: 5\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: ${{ fromJSON(needs.setup.outputs.node) }}\n",
            true,
            false,
        );
        let (workflows, found) = load_dir(&at_least);
        let matrix_line = job_named(&workflows, "wide.yml", "build")
            .matrix
            .as_ref()
            .unwrap()
            .line;
        let f4 = attributed(&found, "caller.yml", FindingKind::MatrixJobs);
        assert_eq!(f4.len(), 1, "{found:?}");
        assert_eq!(f4[0].line, matrix_line);
        assert_eq!(f4[0].message, f4_at_least(2));
        let (text, json) = report_for(&found, &[usage(&caller_key("caller.yml"), "0.06", "0.006")]);
        assert!(
            section_after(&text, "example-org/app .github/workflows/caller.yml")
                .contains(&f4_at_least(2)),
            "{text}"
        );
        assert_eq!(
            json["workflows"][0]["findings"][0]["message"],
            f4_at_least(2)
        );

        let between = workflows_dir("matrix-between");
        write_workflow(
            &between,
            "caller.yml",
            "name: Caller\non: push\njobs:\n  call-wide:\n    uses: ./.github/workflows/wide.yml\n    with:\n      extra: \"1\"\n    secrets: inherit\n",
            false,
            false,
        );
        write_workflow(
            &between,
            "wide.yml",
            "name: Wide\non: workflow_call\njobs:\n  build:\n    runs-on: ubuntu-latest\n    timeout-minutes: 5\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16, 18]\n        exclude: ${{ inputs.rules }}\n",
            false,
            false,
        );
        let (workflows, found) = load_dir(&between);
        let matrix_line = job_named(&workflows, "wide.yml", "build")
            .matrix
            .as_ref()
            .unwrap()
            .line;
        let f4 = attributed(&found, "caller.yml", FindingKind::MatrixJobs);
        assert_eq!(f4.len(), 1, "{found:?}");
        assert_eq!(f4[0].line, matrix_line);
        assert_eq!(f4[0].message, f4_between(0, 6));
        let (text, _) = report_for(&found, &[usage(&caller_key("caller.yml"), "0.06", "0.006")]);
        assert!(text.contains(&f4_between(0, 6)), "{text}");
        assert!(!text.contains("Workflows without usage"), "{text}");

        let nested = workflows_dir("matrix-nested");
        write_workflow(
            &nested,
            "caller.yml",
            "name: Caller\non: push\njobs:\n  call-mid:\n    uses: ./.github/workflows/mid.yml\n",
            false,
            false,
        );
        write_workflow(
            &nested,
            "mid.yml",
            "name: Mid\non: workflow_call\njobs:\n  forward:\n    uses: ./.github/workflows/wide.yml\n    secrets: inherit\n",
            false,
            false,
        );
        write_workflow(&nested, "wide.yml", MATRIX_CALLEE, false, false);
        let (workflows, found) = load_dir(&nested);
        let matrix_line = job_named(&workflows, "wide.yml", "build")
            .matrix
            .as_ref()
            .unwrap()
            .line;
        let f4 = attributed(&found, "caller.yml", FindingKind::MatrixJobs);
        assert_eq!(f4.len(), 1, "{found:?}");
        assert_eq!(f4[0].line, matrix_line);
        assert!(f4[0].path.ends_with("wide.yml"));
        assert_eq!(f4[0].message, f4_message(6));
        let (text, _) = report_for(&found, &[usage(&caller_key("caller.yml"), "0.06", "0.006")]);
        assert!(
            section_after(&text, "example-org/app .github/workflows/caller.yml")
                .contains("wide.yml:8:"),
            "{text}"
        );
        assert!(!text.contains("Workflows without usage"), "{text}");

        let caller_matrix = workflows_dir("matrix-on-caller");
        write_workflow(
            &caller_matrix,
            "caller.yml",
            "name: Caller\non: push\njobs:\n  call-wide:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16, 18]\n    uses: ./.github/workflows/wide.yml\n    secrets: inherit\n",
            false,
            false,
        );
        write_workflow(
            &caller_matrix,
            "wide.yml",
            "name: Wide\non: workflow_call\njobs:\n  build:\n    runs-on: ubuntu-latest\n    timeout-minutes: 5\n    steps:\n      - run: echo build\n",
            false,
            false,
        );
        let (workflows, found) = load_dir(&caller_matrix);
        let call = job_named(&workflows, "caller.yml", "call-wide");
        let f4 = attributed(&found, "caller.yml", FindingKind::MatrixJobs);
        assert_eq!(f4.len(), 1, "{found:?}");
        assert_eq!(f4[0].line, call.matrix.as_ref().unwrap().line);
        assert!(f4[0].path.ends_with("caller.yml"));
        assert_eq!(f4[0].message, f4_message(6));
    }

    #[test]
    fn caller_costs_stay_on_the_caller_csv_path() {
        let dir = workflows_dir("costs-stay");
        write_workflow(
            &dir,
            "caller.yml",
            "name: Caller\non: [push, pull_request]\njobs:\n  call-lint:\n    uses: ./.github/workflows/lint.yml\n    with:\n      extra: \"1\"\n    secrets: inherit\n",
            true,
            false,
        );
        write_workflow(&dir, "lint.yml", LINT_CALLEE, true, false);
        let csv = dir.join("usage.csv");
        fs::write(
            &csv,
            "\
workflow_path,net_amount,gross_amount,discount_amount,quantity,unit_type,sku,product,date,repository,organization,username,applied_cost_per_quantity\r\n\
.github/workflows/caller.yml,0.06,0.06,0,10,Minutes,actions_linux,Actions,2026-09-01,example-org/app,example-org,ci,0.006\r\n\
.github/workflows/lint.yml,1.00,1.00,0,10,minutes,actions_macos,actions,2026-09-30,example-org/app,example-org,ci,0.100\r\n",
        )
        .unwrap();
        let rows = read_usage(&csv).unwrap();
        let (_, found) = load_dir(&dir);
        let ranking = rank(&rows, None);
        assert_eq!(ranking.workflows.len(), 2);
        assert_eq!(ranking.bill.period_from.as_deref(), Some("2026-09-01"));
        assert_eq!(ranking.bill.period_to.as_deref(), Some("2026-09-30"));
        let caller = ranking
            .workflows
            .iter()
            .find(|item| item.workflow_path == ".github/workflows/caller.yml")
            .unwrap();
        let lint = ranking
            .workflows
            .iter()
            .find(|item| item.workflow_path == ".github/workflows/lint.yml")
            .unwrap();
        assert_eq!(caller.net, Micros::parse("0.06").unwrap());
        assert_eq!(caller.gross, Micros::parse("0.06").unwrap());
        assert_eq!(lint.net, Micros::parse("1.00").unwrap());
        assert_ne!(caller.net, lint.net);
        let (text, json) = report_for(&found, &rows);
        let caller_at = text
            .find("example-org/app .github/workflows/caller.yml")
            .unwrap();
        let lint_at = text
            .find("example-org/app .github/workflows/lint.yml")
            .unwrap();
        assert!(lint_at < caller_at, "{text}");
        assert!(text[caller_at..].contains("0.06"), "{text}");
        assert!(
            text[caller_at..].contains("lint.yml:5: lint on macOS"),
            "{text}"
        );
        assert!(!text[caller_at..].contains("1.00"), "{text}");
        let caller_json = json["workflows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["path"] == ".github/workflows/caller.yml")
            .unwrap();
        assert_eq!(caller_json["net"], "0.060000");
        assert_eq!(caller_json["gross"], "0.060000");
    }

    struct RemoteCase {
        name: &'static str,
        caller_file: &'static str,
        job_body: &'static str,
        spec: &'static str,
        relative: &'static str,
        crlf: bool,
        bom: bool,
    }

    fn remote_cases() -> Vec<RemoteCase> {
        let path = "acme/tools/.github/workflows/lint.yml@v1";
        vec![
            RemoteCase {
                name: "tag",
                caller_file: "caller.yml",
                job_body: "uses: acme/tools/.github/workflows/lint.yml@v1\n",
                spec: path,
                relative: path,
                crlf: false,
                bom: false,
            },
            RemoteCase {
                name: "yaml-ext",
                caller_file: "caller.yaml",
                job_body: "uses: acme/tools/.github/workflows/lint.yaml@v1\n",
                spec: "acme/tools/.github/workflows/lint.yaml@v1",
                relative: "acme/tools/.github/workflows/lint.yaml@v1",
                crlf: false,
                bom: false,
            },
            RemoteCase {
                name: "tag-dotted",
                caller_file: "caller.yml",
                job_body: "uses: acme/tools/.github/workflows/lint.yml@v1.2.3\n",
                spec: "acme/tools/.github/workflows/lint.yml@v1.2.3",
                relative: "acme/tools/.github/workflows/lint.yml@v1.2.3",
                crlf: false,
                bom: false,
            },
            RemoteCase {
                name: "branch",
                caller_file: "caller.yml",
                job_body: "uses: acme/tools/.github/workflows/lint.yml@main\n",
                spec: "acme/tools/.github/workflows/lint.yml@main",
                relative: "acme/tools/.github/workflows/lint.yml@main",
                crlf: false,
                bom: false,
            },
            RemoteCase {
                name: "branch-slash",
                caller_file: "caller.yml",
                job_body: "uses: acme/tools/.github/workflows/lint.yml@feature/widget\n",
                spec: "acme/tools/.github/workflows/lint.yml@feature/widget",
                relative: "acme/tools/.github/workflows/lint.yml@feature/widget",
                crlf: false,
                bom: false,
            },
            RemoteCase {
                name: "refs-heads",
                caller_file: "caller.yml",
                job_body: "uses: acme/tools/.github/workflows/lint.yml@refs/heads/main\n",
                spec: "acme/tools/.github/workflows/lint.yml@refs/heads/main",
                relative: "acme/tools/.github/workflows/lint.yml@refs/heads/main",
                crlf: false,
                bom: false,
            },
            RemoteCase {
                name: "sha",
                caller_file: "caller.yml",
                job_body: "uses: acme/tools/.github/workflows/lint.yml@0123456789abcdef0123456789abcdef01234567\n",
                spec: "acme/tools/.github/workflows/lint.yml@0123456789abcdef0123456789abcdef01234567",
                relative: "acme/tools/.github/workflows/lint.yml@0123456789abcdef0123456789abcdef01234567",
                crlf: false,
                bom: false,
            },
            RemoteCase {
                name: "short-sha",
                caller_file: "caller.yml",
                job_body: "uses: acme/tools/.github/workflows/lint.yml@abc1234\n",
                spec: "acme/tools/.github/workflows/lint.yml@abc1234",
                relative: "acme/tools/.github/workflows/lint.yml@abc1234",
                crlf: false,
                bom: false,
            },
            RemoteCase {
                name: "with-after",
                caller_file: "caller.yml",
                job_body: "uses: acme/tools/.github/workflows/lint.yml@v1\nwith:\n  extra: \"1\"\n",
                spec: path,
                relative: path,
                crlf: false,
                bom: false,
            },
            RemoteCase {
                name: "secrets-before",
                caller_file: "caller.yml",
                job_body: "secrets: inherit\nwith:\n  extra: \"1\"\nuses: acme/tools/.github/workflows/lint.yml@v1\n",
                spec: path,
                relative: path,
                crlf: false,
                bom: false,
            },
            RemoteCase {
                name: "with-and-secrets",
                caller_file: "caller.yml",
                job_body: "uses: acme/tools/.github/workflows/lint.yml@v1\nwith:\n  extra: \"1\"\nsecrets: inherit\n",
                spec: path,
                relative: path,
                crlf: false,
                bom: false,
            },
            RemoteCase {
                name: "empty-with-secrets",
                caller_file: "caller.yml",
                job_body: "with: {}\nsecrets: {}\nuses: acme/tools/.github/workflows/lint.yml@v1\n",
                spec: path,
                relative: path,
                crlf: false,
                bom: false,
            },
            RemoteCase {
                name: "secrets-map-before",
                caller_file: "caller.yml",
                job_body: "secrets:\n  token: value\nuses: acme/tools/.github/workflows/lint.yml@v1\nwith:\n  extra: \"1\"\n",
                spec: path,
                relative: path,
                crlf: false,
                bom: false,
            },
            RemoteCase {
                name: "double-quoted",
                caller_file: "caller.yml",
                job_body: "uses: \"acme/tools/.github/workflows/lint.yml@v1\"\n",
                spec: path,
                relative: path,
                crlf: false,
                bom: false,
            },
            RemoteCase {
                name: "single-quoted",
                caller_file: "caller.yml",
                job_body: "uses: 'acme/tools/.github/workflows/lint.yml@v1'\n",
                spec: path,
                relative: path,
                crlf: false,
                bom: false,
            },
            RemoteCase {
                name: "capitals",
                caller_file: "caller.yml",
                job_body: "uses: Acme/Tools/.GitHub/Workflows/Lint.YML@V1\n",
                spec: "Acme/Tools/.GitHub/Workflows/Lint.YML@V1",
                relative: "Acme/Tools/.GitHub/Workflows/Lint.YML@V1",
                crlf: false,
                bom: false,
            },
            RemoteCase {
                name: "backslash",
                caller_file: "caller.yml",
                job_body: "uses: acme/tools/.github\\workflows\\lint.yml@v1\n",
                spec: "acme/tools/.github\\workflows\\lint.yml@v1",
                relative: path,
                crlf: false,
                bom: false,
            },
            RemoteCase {
                name: "crlf",
                caller_file: "caller.yml",
                job_body: "uses: acme/tools/.github/workflows/lint.yml@v1\nsecrets: inherit\nwith:\n  extra: \"1\"\n",
                spec: path,
                relative: path,
                crlf: true,
                bom: false,
            },
            RemoteCase {
                name: "bom",
                caller_file: "caller.yml",
                job_body: "uses: acme/tools/.github/workflows/lint.yml@v1\n",
                spec: path,
                relative: path,
                crlf: false,
                bom: true,
            },
            RemoteCase {
                name: "block-scalar",
                caller_file: "caller.yml",
                job_body: "uses: |\n  acme/tools/.github/workflows/lint.yml@v1\n",
                spec: path,
                relative: path,
                crlf: false,
                bom: false,
            },
        ]
    }

    fn b4b_dirs(name: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join("ci-bill-b4b").join(name);
        let _ = fs::remove_dir_all(&root);
        let workflows = root.join(".github").join("workflows");
        let runs = root.join("runs");
        fs::create_dir_all(&workflows).unwrap();
        fs::create_dir_all(&runs).unwrap();
        (workflows, runs)
    }

    fn path_from_slash(relative: &str) -> PathBuf {
        relative
            .split('/')
            .filter(|part| !part.is_empty())
            .collect()
    }

    fn write_cache(runs: &Path, relative: &str, text: &str, crlf_file: bool, bom: bool) {
        let path = runs.join(path_from_slash(relative));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut body = if crlf_file {
            text.replace('\n', "\r\n")
        } else {
            text.to_string()
        };
        if bom {
            body.insert(0, '\u{feff}');
        }
        fs::write(path, body).unwrap();
    }

    fn caller_yaml(job_body: &str) -> String {
        indent_job_body(&format!(
            "name: Caller\non: push\njobs:\n  call-lint:\n{job_body}"
        ))
    }

    fn uses_key_line(yaml: &str) -> usize {
        yaml.lines()
            .position(|line| line.contains("uses:"))
            .unwrap_or_else(|| panic!("no uses key in {yaml}"))
            + 1
    }

    fn remote_of(job: &crate::workflow::Job) -> RemoteWorkflow {
        match &job.uses.as_ref().expect("uses").source {
            ReusableSource::Remote(remote) => remote.clone(),
            ReusableSource::Local => panic!("expected a remote reusable workflow"),
        }
    }

    fn assert_no_limitation(text: &str) {
        let lower = text.to_ascii_lowercase();
        assert!(!lower.contains("not supported"), "{text}");
        assert!(!lower.contains("unsupported"), "{text}");
        assert!(!lower.contains("skipped"), "{text}");
        assert!(!lower.contains("not implemented"), "{text}");
    }

    fn assert_costs_stay_on_caller(
        found: &[Finding],
        caller_file: &str,
        needle: &str,
    ) -> (String, serde_json::Value) {
        let rows = [
            usage(&caller_key(caller_file), "0.06", "0.006"),
            usage(".github/workflows/other.yml", "1.00", "0.100"),
        ];
        let ranking = rank(&rows, None);
        assert_eq!(ranking.workflows.len(), 2);
        let caller = ranking
            .workflows
            .iter()
            .find(|item| item.workflow_path == caller_key(caller_file))
            .unwrap();
        let other = ranking
            .workflows
            .iter()
            .find(|item| item.workflow_path == ".github/workflows/other.yml")
            .unwrap();
        assert_eq!(caller.net, Micros::parse("0.06").unwrap());
        assert_eq!(other.net, Micros::parse("1.00").unwrap());
        assert_eq!(
            ranking.bill.total_net,
            Micros::parse("1.06").unwrap(),
            "remote calls must not add or move invoice net"
        );
        let (text, json) = report_for(found, &rows);
        assert_no_limitation(&text);
        assert_no_limitation(&json.to_string());
        let label = format!("example-org/app {}", caller_key(caller_file));
        let other_label = "example-org/app .github/workflows/other.yml";
        let other_at = text
            .find(other_label)
            .unwrap_or_else(|| panic!("missing {other_label} in {text}"));
        let caller_at = text
            .find(&label)
            .unwrap_or_else(|| panic!("missing {label} in {text}"));
        assert!(other_at < caller_at, "{text}");
        assert!(
            !text[other_at..caller_at].contains(needle),
            "{needle} landed on the other workflow:\n{text}"
        );
        assert!(
            text[caller_at..].contains(needle),
            "missing {needle} under the caller:\n{text}"
        );
        assert!(!text.contains("Workflows without usage"), "{text}");
        let caller_json = json["workflows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["path"] == caller_key(caller_file))
            .unwrap();
        assert_eq!(caller_json["net"], "0.060000");
        assert!(
            json["unattached_findings"].as_array().unwrap().is_empty(),
            "{json}"
        );
        (text, json)
    }

    #[test]
    fn remote_without_cache_shows_the_job_at_the_uses_line() {
        for case in remote_cases() {
            let (dir, runs) = b4b_dirs(&format!("miss-{}", case.name));
            let yaml = caller_yaml(case.job_body);
            let expected_line = uses_key_line(&yaml);
            write_workflow(&dir, case.caller_file, &yaml, case.crlf, case.bom);
            for runs_arg in [None, Some(runs.as_path())] {
                let scanned = scan_dir(&dir).expect("scan");
                let found = findings_with_cache(&scanned, runs_arg).expect("findings");
                let call = job_named(&scanned, case.caller_file, "call-lint");
                assert_eq!(call.uses.as_ref().unwrap().spec, case.spec, "{}", case.name);
                assert_eq!(
                    call.uses.as_ref().unwrap().line,
                    expected_line,
                    "{}",
                    case.name
                );
                assert_eq!(
                    remote_of(call).cache_relative(),
                    path_from_slash(case.relative),
                    "{}",
                    case.name
                );
                assert!(call.invalid_uses.is_none(), "{}", case.name);
                assert_no_timeout_on_call(&found, case.caller_file, call.line);
                let shown = attributed(&found, case.caller_file, FindingKind::RemoteWorkflow);
                assert_eq!(shown.len(), 1, "{}: {found:?}", case.name);
                assert_eq!(shown[0].line, expected_line, "{}", case.name);
                assert_eq!(shown[0].kind.code(), "F10");
                assert_eq!(
                    shown[0].message,
                    format!("job call-lint uses {}", case.spec),
                    "{}",
                    case.name
                );
                assert!(shown[0].path.ends_with(case.caller_file), "{}", case.name);
                assert!(
                    found
                        .iter()
                        .all(|item| item.kind != FindingKind::CalleeNotFound),
                    "{}: {found:?}",
                    case.name
                );
                let needle = format!(
                    "F10 {}:{}: job call-lint uses {}",
                    shown[0].path.display(),
                    expected_line,
                    case.spec
                );
                let (_text, json) = assert_costs_stay_on_caller(&found, case.caller_file, &needle);
                let listed = json["workflows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| item["path"] == caller_key(case.caller_file))
                    .unwrap()["findings"]
                    .as_array()
                    .unwrap()
                    .clone();
                assert!(
                    listed.iter().any(|item| {
                        item["id"] == "F10"
                            && item["line"] == expected_line
                            && item["message"] == format!("job call-lint uses {}", case.spec)
                    }),
                    "{}: {listed:?}",
                    case.name
                );
            }
        }

        let (dir, _) = b4b_dirs("miss-flow");
        let flow = "name: Caller\non: push\njobs: {call-lint: {secrets: inherit, with: {extra: \"1\"}, uses: acme/tools/.github/workflows/lint.yml@v1}}\n";
        write_workflow(&dir, "caller.yml", flow, false, false);
        let (workflows, found) = load_cached_pair(&dir, None);
        let call = job_named(&workflows, "caller.yml", "call-lint");
        let shown = attributed(&found, "caller.yml", FindingKind::RemoteWorkflow);
        assert_eq!(shown.len(), 1, "{found:?}");
        assert_eq!(shown[0].line, call.uses.as_ref().unwrap().line);
        assert_eq!(
            shown[0].message,
            "job call-lint uses acme/tools/.github/workflows/lint.yml@v1"
        );
        assert_eq!(shown[0].line, uses_key_line(flow));

        let (dir, _) = b4b_dirs("miss-two-jobs");
        write_workflow(
            &dir,
            "caller.yml",
            "name: Caller\non: push\njobs:\n  first:\n    uses: acme/tools/.github/workflows/lint.yml@v1\n  second:\n    with: {}\n    uses: other/repo/.github/workflows/build.yaml@release/1.2\n",
            false,
            false,
        );
        let (workflows, found) = load_cached_pair(&dir, None);
        let shown = attributed(&found, "caller.yml", FindingKind::RemoteWorkflow);
        assert_eq!(shown.len(), 2, "{found:?}");
        let first = job_named(&workflows, "caller.yml", "first");
        let second = job_named(&workflows, "caller.yml", "second");
        assert!(shown.iter().any(|item| {
            item.line == first.uses.as_ref().unwrap().line
                && item.message == "job first uses acme/tools/.github/workflows/lint.yml@v1"
        }));
        assert!(shown.iter().any(|item| {
            item.line == second.uses.as_ref().unwrap().line
                && item.message
                    == "job second uses other/repo/.github/workflows/build.yaml@release/1.2"
        }));
        assert_eq!(
            remote_of(second).cache_relative(),
            path_from_slash("other/repo/.github/workflows/build.yaml@release/1.2")
        );
    }

    fn load_cached_pair(dir: &Path, runs: Option<&Path>) -> (Vec<Workflow>, Vec<Finding>) {
        let workflows = scan_dir(dir).expect("scan workflows");
        let found = findings_with_cache(&workflows, runs).expect("findings");
        (workflows, found)
    }

    #[test]
    fn remote_cache_file_puts_callee_findings_under_the_caller() {
        for case in remote_cases() {
            let (dir, runs) = b4b_dirs(&format!("hit-{}", case.name));
            let yaml = caller_yaml(case.job_body);
            write_workflow(&dir, case.caller_file, &yaml, case.crlf, case.bom);
            write_cache(&runs, case.relative, LINT_CALLEE, case.crlf, case.bom);
            let (workflows, found) = load_cached_pair(&dir, Some(&runs));
            let call = job_named(&workflows, case.caller_file, "call-lint");
            assert_eq!(call.uses.as_ref().unwrap().spec, case.spec, "{}", case.name);
            assert_no_timeout_on_call(&found, case.caller_file, call.line);
            let f1 = attributed(&found, case.caller_file, FindingKind::LintOnRunner);
            assert_eq!(f1.len(), 1, "{}: {found:?}", case.name);
            assert_eq!(f1[0].line, 5, "{}", case.name);
            assert_eq!(f1[0].message, "lint on macOS", "{}", case.name);
            assert_eq!(f1[0].kind.code(), "F1");
            assert!(
                f1[0].path.ends_with(path_from_slash(case.relative)),
                "{}: {}",
                case.name,
                f1[0].path.display()
            );
            assert!(
                attributed(&found, case.caller_file, FindingKind::RemoteWorkflow).is_empty(),
                "{} still listed the call as uncached: {found:?}",
                case.name
            );
            assert!(
                found
                    .iter()
                    .all(|item| item.kind != FindingKind::CalleeNotFound),
                "{}",
                case.name
            );
            let needle = format!("F1 {}:5: lint on macOS", f1[0].path.display());
            let (_text, json) = assert_costs_stay_on_caller(&found, case.caller_file, &needle);
            let listed = json["workflows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["path"] == caller_key(case.caller_file))
                .unwrap()["findings"]
                .as_array()
                .unwrap()
                .clone();
            assert!(
                listed.iter().any(|item| {
                    item["id"] == "F1"
                        && item["line"] == 5
                        && item["message"] == "lint on macOS"
                        && item["file"]
                            .as_str()
                            .unwrap()
                            .ends_with(case.relative.rsplit('/').next().unwrap())
                }),
                "{}: {listed:?}",
                case.name
            );
        }

        let (dir, runs) = b4b_dirs("hit-flow");
        let flow = "on: push\njobs: {call-lint: {uses: acme/tools/.github/workflows/lint.yml@v1, secrets: inherit, with: {extra: \"1\"}}}\n";
        write_workflow(&dir, "caller.yml", flow, true, false);
        write_cache(
            &runs,
            "acme/tools/.github/workflows/lint.yml@v1",
            LINT_CALLEE,
            true,
            true,
        );
        let (_, found) = load_cached_pair(&dir, Some(&runs));
        let f1 = attributed(&found, "caller.yml", FindingKind::LintOnRunner);
        assert_eq!(f1.len(), 1, "{found:?}");
        assert_eq!(f1[0].line, 5);
        assert_eq!(f1[0].message, "lint on macOS");

        let (dir, runs) = b4b_dirs("hit-casefold");
        write_workflow(
            &dir,
            "caller.yml",
            "name: Caller\non: push\njobs:\n  call-lint:\n    uses: Acme/Tools/.GitHub/Workflows/Lint.YML@V1\n",
            false,
            false,
        );
        write_cache(
            &runs,
            "acme/tools/.github/workflows/lint.yml@v1",
            LINT_CALLEE,
            false,
            false,
        );
        let (_, found) = load_cached_pair(&dir, Some(&runs));
        let f1 = attributed(&found, "caller.yml", FindingKind::LintOnRunner);
        assert_eq!(f1.len(), 1, "{found:?}");
        assert!(
            f1[0]
                .path
                .ends_with(path_from_slash("acme/tools/.github/workflows/lint.yml@v1")),
            "{}",
            f1[0].path.display()
        );

        let (dir, runs) = b4b_dirs("hit-exact-wins");
        write_workflow(
            &dir,
            "caller.yml",
            "name: Caller\non: push\njobs:\n  call-lint:\n    uses: Acme/Tools/.GitHub/Workflows/Lint.YML@V1\n",
            false,
            false,
        );
        write_cache(
            &runs,
            "Acme/Tools/.GitHub/Workflows/Lint.YML@V1",
            LINT_CALLEE,
            false,
            false,
        );
        write_cache(
            &runs,
            "acme/tools/.github/workflows/lint.yml@v1",
            "name: Lint\non: workflow_call\njobs:\n  fmt:\n    runs-on: windows-latest\n    timeout-minutes: 10\n    steps:\n      - run: cargo fmt --check\n",
            false,
            false,
        );
        let (_, found) = load_cached_pair(&dir, Some(&runs));
        let f1 = attributed(&found, "caller.yml", FindingKind::LintOnRunner);
        assert_eq!(f1.len(), 1, "{found:?}");
        assert_eq!(f1[0].message, "lint on macOS");
        assert!(
            f1[0]
                .path
                .ends_with(path_from_slash("Acme/Tools/.GitHub/Workflows/Lint.YML@V1")),
            "{}",
            f1[0].path.display()
        );

        let bodies = [
            (
                "windows",
                "name: Lint\non: workflow_call\njobs:\n  fmt:\n    runs-on: windows-latest\n    timeout-minutes: 10\n    steps:\n      - run: cargo fmt --check\n",
                FindingKind::LintOnRunner,
                5,
                "lint on Windows",
            ),
            (
                "timeout",
                "name: Called\non: workflow_call\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo build\n",
                FindingKind::MissingTimeout,
                4,
                "default timeout is 360 minutes",
            ),
            (
                "pull-request",
                "name: Called\non: pull_request\njobs:\n  test:\n    runs-on: ubuntu-latest\n    timeout-minutes: 5\n    steps:\n      - run: cargo test\n",
                FindingKind::PullRequestConcurrency,
                2,
                "pull_request without concurrency cancel-in-progress",
            ),
            (
                "matrix",
                MATRIX_CALLEE,
                FindingKind::MatrixJobs,
                8,
                "matrix runs 6 jobs; each job is billed in whole minutes",
            ),
            (
                "duplicate",
                "name: Called\non: workflow_call\njobs:\n  lint:\n    runs-on: ubuntu-latest\n    runs-on: macos-latest\n    steps:\n      - run: cargo fmt --check\n",
                FindingKind::DuplicateKey,
                6,
                "duplicate key runs-on (line 5 and 6)",
            ),
        ];
        for (name, body, kind, line, message) in bodies {
            let (dir, runs) = b4b_dirs(&format!("hit-{name}"));
            write_workflow(
                &dir,
                "caller.yml",
                "name: Caller\non: push\njobs:\n  call-lint:\n    uses: acme/tools/.github/workflows/lint.yml@v1\n",
                false,
                false,
            );
            write_cache(
                &runs,
                "acme/tools/.github/workflows/lint.yml@v1",
                body,
                false,
                false,
            );
            let (workflows, found) = load_cached_pair(&dir, Some(&runs));
            let call = job_named(&workflows, "caller.yml", "call-lint");
            assert_no_timeout_on_call(&found, "caller.yml", call.line);
            let matched = attributed(&found, "caller.yml", kind);
            assert_eq!(matched.len(), 1, "{name}: {found:?}");
            assert_eq!(matched[0].line, line, "{name}");
            assert_eq!(matched[0].message, message, "{name}");
            assert!(
                matched[0]
                    .path
                    .ends_with(path_from_slash("acme/tools/.github/workflows/lint.yml@v1")),
                "{name}: {}",
                matched[0].path.display()
            );
            let needle = format!(
                "{} {}:{}: {message}",
                kind.code(),
                matched[0].path.display(),
                line
            );
            let _ = assert_costs_stay_on_caller(&found, "caller.yml", &needle);
        }
    }

    #[test]
    fn remote_cache_expands_nested_calls_and_stops_cycles() {
        let (dir, runs) = b4b_dirs("nested-remote");
        write_workflow(
            &dir,
            "caller.yml",
            "name: Caller\non: push\njobs:\n  call-mid:\n    secrets: inherit\n    uses: acme/tools/.github/workflows/mid.yml@v1\n",
            false,
            false,
        );
        write_cache(
            &runs,
            "acme/tools/.github/workflows/mid.yml@v1",
            "name: Mid\non: workflow_call\njobs:\n  forward:\n    with:\n      extra: \"1\"\n    uses: acme/tools/.github/workflows/lint.yml@v1\n",
            false,
            false,
        );
        write_cache(
            &runs,
            "acme/tools/.github/workflows/lint.yml@v1",
            LINT_CALLEE,
            false,
            false,
        );
        let (_, found) = load_cached_pair(&dir, Some(&runs));
        let f1 = attributed(&found, "caller.yml", FindingKind::LintOnRunner);
        assert_eq!(f1.len(), 1, "{found:?}");
        assert_eq!(f1[0].line, 5);
        assert!(
            f1[0].path.ends_with("lint.yml@v1"),
            "{}",
            f1[0].path.display()
        );
        assert!(
            attributed(&found, "caller.yml", FindingKind::ReusableCycle).is_empty(),
            "{found:?}"
        );

        let (dir, runs) = b4b_dirs("nested-local-inside-remote");
        write_workflow(
            &dir,
            "caller.yml",
            "name: Caller\non: push\njobs:\n  call-mid:\n    uses: acme/tools/.github/workflows/mid.yml@release/1.2\n",
            true,
            false,
        );
        write_cache(
            &runs,
            "acme/tools/.github/workflows/mid.yml@release/1.2",
            "name: Mid\non: workflow_call\njobs:\n  forward:\n    uses: ./.github/workflows/lint.yml\n    secrets: inherit\n",
            true,
            false,
        );
        write_cache(
            &runs,
            "acme/tools/.github/workflows/lint.yml@release/1.2",
            LINT_CALLEE,
            true,
            false,
        );
        let (_, found) = load_cached_pair(&dir, Some(&runs));
        let f1 = attributed(&found, "caller.yml", FindingKind::LintOnRunner);
        assert_eq!(f1.len(), 1, "{found:?}");
        assert_eq!(f1[0].message, "lint on macOS");
        assert!(
            f1[0].path.ends_with(path_from_slash(
                "acme/tools/.github/workflows/lint.yml@release/1.2"
            )),
            "{}",
            f1[0].path.display()
        );

        let (dir, runs) = b4b_dirs("nested-local-missing");
        write_workflow(
            &dir,
            "caller.yml",
            "name: Caller\non: push\njobs:\n  call-mid:\n    uses: acme/tools/.github/workflows/mid.yml@v1\n",
            false,
            false,
        );
        write_cache(
            &runs,
            "acme/tools/.github/workflows/mid.yml@v1",
            "name: Mid\non: workflow_call\njobs:\n  forward:\n    uses: ./.github/workflows/missing.yml\n",
            false,
            false,
        );
        let (_, found) = load_cached_pair(&dir, Some(&runs));
        let missing = attributed(&found, "caller.yml", FindingKind::CalleeNotFound);
        assert_eq!(missing.len(), 1, "{found:?}");
        assert!(
            missing[0].message.contains("callee not found"),
            "{}",
            missing[0].message
        );
        assert!(
            missing[0]
                .message
                .contains("./.github/workflows/missing.yml"),
            "{}",
            missing[0].message
        );
        assert!(
            missing[0].path.ends_with("mid.yml@v1"),
            "{}",
            missing[0].path.display()
        );
        assert_no_limitation(&missing[0].message);

        let (dir, runs) = b4b_dirs("cycle-two");
        write_workflow(
            &dir,
            "caller.yml",
            "name: Caller\non: push\njobs:\n  call-a:\n    uses: acme/tools/.github/workflows/a.yml@v1\n",
            false,
            false,
        );
        let spec_a = "acme/tools/.github/workflows/a.yml@v1";
        let spec_b = "acme/tools/.github/workflows/b.yml@v1";
        write_cache(
            &runs,
            spec_a,
            &format!(
                "name: A\non: workflow_call\njobs:\n  lint:\n    runs-on: macos-latest\n    timeout-minutes: 5\n    steps:\n      - run: cargo fmt --check\n  call-b:\n    uses: {spec_b}\n"
            ),
            false,
            false,
        );
        write_cache(
            &runs,
            spec_b,
            &format!("name: B\non: workflow_call\njobs:\n  call-a:\n    uses: {spec_a}\n    secrets: inherit\n"),
            false,
            false,
        );
        let (_, found) = load_cached_pair(&dir, Some(&runs));
        assert!(found.len() < 12, "{found:?}");
        let cycles = attributed(&found, "caller.yml", FindingKind::ReusableCycle);
        assert_eq!(cycles.len(), 1, "{found:?}");
        assert!(cycles[0].message.contains(spec_a), "{}", cycles[0].message);
        assert!(cycles[0].message.contains(spec_b), "{}", cycles[0].message);
        assert!(cycles[0].message.contains(" -> "), "{}", cycles[0].message);
        assert_eq!(cycles[0].kind.code(), "F7");
        let f1 = attributed(&found, "caller.yml", FindingKind::LintOnRunner);
        assert_eq!(f1.len(), 1, "{found:?}");
        assert_eq!(f1[0].message, "lint on macOS");

        let (dir, runs) = b4b_dirs("cycle-self");
        let spec = "acme/tools/.github/workflows/loop.yml@main";
        write_workflow(
            &dir,
            "caller.yml",
            &format!("name: Caller\non: push\njobs:\n  again:\n    uses: {spec}\n"),
            false,
            false,
        );
        write_cache(
            &runs,
            spec,
            &format!("name: Loop\non: workflow_call\njobs:\n  again:\n    uses: {spec}\n"),
            false,
            false,
        );
        let (_, found) = load_cached_pair(&dir, Some(&runs));
        assert!(found.len() < 6, "{found:?}");
        let cycles = attributed(&found, "caller.yml", FindingKind::ReusableCycle);
        assert_eq!(cycles.len(), 1, "{found:?}");
        assert!(
            cycles[0].message.contains(&format!("{spec} -> {spec}")),
            "{}",
            cycles[0].message
        );
    }

    #[test]
    fn remote_cache_errors_name_the_file_and_keep_a_visible_job() {
        let (dir, runs) = b4b_dirs("bad-yaml");
        write_workflow(
            &dir,
            "caller.yml",
            "name: Caller\non: push\njobs:\n  call-lint:\n    uses: acme/tools/.github/workflows/lint.yml@v1\n",
            false,
            false,
        );
        write_cache(
            &runs,
            "acme/tools/.github/workflows/lint.yml@v1",
            "name: Broken\non: push\njobs: [\nUNIQUE_SECRET_TOKEN\n",
            false,
            false,
        );
        let scanned = scan_dir(&dir).unwrap();
        let err = findings_with_cache(&scanned, Some(&runs)).unwrap_err();
        let message = err.to_string();
        assert!(message.starts_with("line "), "{message}");
        assert!(message.contains("invalid YAML"), "{message}");
        assert!(!message.contains("UNIQUE_SECRET_TOKEN"), "{message}");
        assert!(
            err.path
                .ends_with(path_from_slash("acme/tools/.github/workflows/lint.yml@v1")),
            "{}",
            err.path.display()
        );

        let (dir, runs) = b4b_dirs("bad-bytes");
        write_workflow(
            &dir,
            "caller.yml",
            "name: Caller\non: push\njobs:\n  call-lint:\n    uses: acme/tools/.github/workflows/lint.yml@v1\n",
            false,
            false,
        );
        let bytes = runs.join(path_from_slash("acme/tools/.github/workflows/lint.yml@v1"));
        fs::create_dir_all(bytes.parent().unwrap()).unwrap();
        fs::write(&bytes, [0xff, 0xfe, 0xfd]).unwrap();
        let scanned = scan_dir(&dir).unwrap();
        let err = findings_with_cache(&scanned, Some(&runs)).unwrap_err();
        assert_eq!(err.to_string(), "cannot read file");
        assert!(!format!("{err} {}", err.path.display()).contains("not supported"));

        let (dir, runs) = b4b_dirs("empty-callee");
        write_workflow(
            &dir,
            "caller.yml",
            "name: Caller\non: push\njobs:\n  call-lint:\n    uses: acme/tools/.github/workflows/lint.yml@v1\n",
            false,
            false,
        );
        write_cache(
            &runs,
            "acme/tools/.github/workflows/lint.yml@v1",
            "",
            false,
            false,
        );
        let (_, found) = load_cached_pair(&dir, Some(&runs));
        let shown = attributed(&found, "caller.yml", FindingKind::RemoteWorkflow);
        assert_eq!(shown.len(), 1, "{found:?}");
        assert_eq!(
            shown[0].message,
            "job call-lint uses acme/tools/.github/workflows/lint.yml@v1"
        );
        assert_eq!(shown[0].line, 5);

        let (dir, runs) = b4b_dirs("dir-callee");
        write_workflow(
            &dir,
            "caller.yml",
            "name: Caller\non: push\njobs:\n  call-lint:\n    uses: acme/tools/.github/workflows/lint.yml@v1\n",
            false,
            false,
        );
        fs::create_dir_all(runs.join(path_from_slash("acme/tools/.github/workflows/lint.yml@v1")))
            .unwrap();
        let (_, found) = load_cached_pair(&dir, Some(&runs));
        let shown = attributed(&found, "caller.yml", FindingKind::RemoteWorkflow);
        assert_eq!(shown.len(), 1, "{found:?}");
        assert_eq!(shown[0].line, 5);
        assert!(shown[0]
            .message
            .contains("acme/tools/.github/workflows/lint.yml@v1"));

        let (dir, runs) = b4b_dirs("invalid-inside-callee");
        write_workflow(
            &dir,
            "caller.yml",
            "name: Caller\non: push\njobs:\n  call-lint:\n    uses: acme/tools/.github/workflows/lint.yml@v1\n",
            false,
            false,
        );
        write_cache(
            &runs,
            "acme/tools/.github/workflows/lint.yml@v1",
            "name: Called\non: workflow_call\njobs:\n  bad:\n    uses: actions/checkout@v4\n",
            false,
            false,
        );
        let (_, found) = load_cached_pair(&dir, Some(&runs));
        let invalid = attributed(&found, "caller.yml", FindingKind::InvalidUses);
        assert_eq!(invalid.len(), 1, "{found:?}");
        assert_eq!(
            invalid[0].message,
            "invalid reusable workflow uses: job bad uses \"actions/checkout@v4\""
        );
        assert!(invalid[0].path.ends_with("lint.yml@v1"));
    }

    struct InvalidCase {
        name: &'static str,
        yaml: &'static str,
        job: &'static str,
        detail: &'static str,
        crlf: bool,
    }

    #[test]
    fn invalid_uses_text_is_a_finding_and_does_not_panic() {
        let cases = [
            InvalidCase {
                name: "empty",
                yaml: "name: Caller\non: push\njobs:\n  call-lint:\n    uses: \"\"\n",
                job: "call-lint",
                detail: "\"\"",
                crlf: false,
            },
            InvalidCase {
                name: "blank",
                yaml: "name: Caller\non: push\njobs:\n  call-lint:\n    uses: \"   \"\n",
                job: "call-lint",
                detail: "\"\"",
                crlf: false,
            },
            InvalidCase {
                name: "missing-ref",
                yaml: "name: Caller\non: push\njobs:\n  call-lint:\n    uses: acme/tools/.github/workflows/lint.yml\n",
                job: "call-lint",
                detail: "\"acme/tools/.github/workflows/lint.yml\"",
                crlf: false,
            },
            InvalidCase {
                name: "empty-ref",
                yaml: "name: Caller\non: push\njobs:\n  call-lint:\n    uses: acme/tools/.github/workflows/lint.yml@\n",
                job: "call-lint",
                detail: "\"acme/tools/.github/workflows/lint.yml@\"",
                crlf: false,
            },
            InvalidCase {
                name: "bad-extension",
                yaml: "name: Caller\non: push\njobs:\n  call-lint:\n    uses: acme/tools/.github/workflows/lint.txt@v1\n",
                job: "call-lint",
                detail: "\"acme/tools/.github/workflows/lint.txt@v1\"",
                crlf: false,
            },
            InvalidCase {
                name: "action",
                yaml: "name: Caller\non: push\njobs:\n  call-lint:\n    uses: actions/checkout@v4\n",
                job: "call-lint",
                detail: "\"actions/checkout@v4\"",
                crlf: false,
            },
            InvalidCase {
                name: "action-capitals",
                yaml: "name: Caller\non: push\njobs:\n  call-lint:\n    uses: ACTIONS/CHECKOUT@V4\n",
                job: "call-lint",
                detail: "\"ACTIONS/CHECKOUT@V4\"",
                crlf: true,
            },
            InvalidCase {
                name: "local-other",
                yaml: "name: Caller\non: push\njobs:\n  call-lint:\n    uses: ./foo.yml\n",
                job: "call-lint",
                detail: "\"./foo.yml\"",
                crlf: false,
            },
            InvalidCase {
                name: "url",
                yaml: "name: Caller\non: push\njobs:\n  call-lint:\n    uses: https://github.com/acme/tools/.github/workflows/lint.yml@v1\n",
                job: "call-lint",
                detail: "\"https://github.com/acme/tools/.github/workflows/lint.yml@v1\"",
                crlf: false,
            },
            InvalidCase {
                name: "owner-repo-only",
                yaml: "name: Caller\non: push\njobs:\n  call-lint:\n    uses: acme/tools@v1\n",
                job: "call-lint",
                detail: "\"acme/tools@v1\"",
                crlf: false,
            },
            InvalidCase {
                name: "list",
                yaml: "name: Caller\non: push\njobs:\n  call-lint:\n    uses:\n      - acme/tools/.github/workflows/lint.yml@v1\n",
                job: "call-lint",
                detail: "a list",
                crlf: false,
            },
            InvalidCase {
                name: "mapping",
                yaml: "name: Caller\non: push\njobs:\n  call-lint:\n    secrets: inherit\n    uses:\n      path: acme/tools/.github/workflows/lint.yml@v1\n",
                job: "call-lint",
                detail: "a mapping",
                crlf: false,
            },
            InvalidCase {
                name: "flow-list",
                yaml: "on: push\njobs: {call-lint: {uses: [acme/tools/.github/workflows/lint.yml@v1]}}\n",
                job: "call-lint",
                detail: "a list",
                crlf: true,
            },
            InvalidCase {
                name: "flow-mapping",
                yaml: "on: push\njobs: {call-lint: {uses: {path: acme/tools/.github/workflows/lint.yml@v1}}}\n",
                job: "call-lint",
                detail: "a mapping",
                crlf: false,
            },
            InvalidCase {
                name: "number",
                yaml: "name: Caller\non: push\njobs:\n  call-lint:\n    uses: 1\n",
                job: "call-lint",
                detail: "\"1\"",
                crlf: false,
            },
            InvalidCase {
                name: "bool",
                yaml: "name: Caller\non: push\njobs:\n  call-lint:\n    uses: true\n",
                job: "call-lint",
                detail: "\"true\"",
                crlf: false,
            },
            InvalidCase {
                name: "parent-segment",
                yaml: "name: Caller\non: push\njobs:\n  call-lint:\n    uses: acme/../tools/.github/workflows/lint.yml@v1\n",
                job: "call-lint",
                detail: "\"acme/../tools/.github/workflows/lint.yml@v1\"",
                crlf: false,
            },
            InvalidCase {
                name: "absolute",
                yaml: "name: Caller\non: push\njobs:\n  call-lint:\n    uses: /acme/tools/.github/workflows/lint.yml@v1\n",
                job: "call-lint",
                detail: "\"/acme/tools/.github/workflows/lint.yml@v1\"",
                crlf: false,
            },
            InvalidCase {
                name: "double-slash",
                yaml: "name: Caller\non: push\njobs:\n  call-lint:\n    uses: acme/tools/.github//workflows/lint.yml@v1\n",
                job: "call-lint",
                detail: "\"acme/tools/.github//workflows/lint.yml@v1\"",
                crlf: false,
            },
            InvalidCase {
                name: "ref-dotdot",
                yaml: "name: Caller\non: push\njobs:\n  call-lint:\n    uses: acme/tools/.github/workflows/lint.yml@foo/../../etc\n",
                job: "call-lint",
                detail: "\"acme/tools/.github/workflows/lint.yml@foo/../../etc\"",
                crlf: false,
            },
            InvalidCase {
                name: "missing-file",
                yaml: "name: Caller\non: push\njobs:\n  call-lint:\n    uses: acme/tools/.github/workflows/@v1\n",
                job: "call-lint",
                detail: "\"acme/tools/.github/workflows/@v1\"",
                crlf: false,
            },
            InvalidCase {
                name: "singular-workflow",
                yaml: "name: Caller\non: push\njobs:\n  call-lint:\n    uses: acme/tools/.github/workflow/lint.yml@v1\n",
                job: "call-lint",
                detail: "\"acme/tools/.github/workflow/lint.yml@v1\"",
                crlf: false,
            },
            InvalidCase {
                name: "unicode",
                yaml: "name: Caller\non: push\njobs:\n  call-lint:\n    uses: \"acmé/tools/.github/workflows/lint.yml@v1\"\n",
                job: "call-lint",
                detail: "\"acmé/tools/.github/workflows/lint.yml@v1\"",
                crlf: false,
            },
            InvalidCase {
                name: "only-at",
                yaml: "name: Caller\non: push\njobs:\n  call-lint:\n    uses: \"@v1\"\n",
                job: "call-lint",
                detail: "\"@v1\"",
                crlf: false,
            },
        ];
        for case in cases {
            let (dir, runs) = b4b_dirs(&format!("bad-{}", case.name));
            write_workflow(&dir, "caller.yml", case.yaml, case.crlf, false);
            let caught = std::panic::catch_unwind(|| load_cached_pair(&dir, Some(&runs)));
            let (workflows, found) = caught.expect("invalid uses panicked");
            let job = job_named(&workflows, "caller.yml", case.job);
            assert!(job.uses.is_none(), "{}", case.name);
            let invalid = job.invalid_uses.as_ref().expect(case.name);
            assert_eq!(invalid.detail, case.detail, "{}", case.name);
            assert_eq!(invalid.line, uses_key_line(case.yaml), "{}", case.name);
            let matched: Vec<_> = found
                .iter()
                .filter(|item| {
                    item.kind == FindingKind::InvalidUses && item.path.ends_with("caller.yml")
                })
                .collect();
            assert_eq!(matched.len(), 1, "{}: {found:?}", case.name);
            assert_eq!(matched[0].line, invalid.line, "{}", case.name);
            assert_eq!(matched[0].kind.code(), "F11");
            assert_eq!(
                matched[0].message,
                format!(
                    "invalid reusable workflow uses: job {} uses {}",
                    case.job, case.detail
                ),
                "{}",
                case.name
            );
            assert_no_limitation(&matched[0].message);
            assert!(
                of_kind(&found, FindingKind::MissingTimeout).is_empty(),
                "{}: {found:?}",
                case.name
            );
            let needle = format!(
                "F11 {}:{}: {}",
                matched[0].path.display(),
                matched[0].line,
                matched[0].message
            );
            let _ = assert_costs_stay_on_caller(&found, "caller.yml", &needle);
        }

        let long = format!("\"{}\"", "x".repeat(4000));
        let yaml = format!("name: Caller\non: push\njobs:\n  call-lint:\n    uses: {long}\n");
        let (dir, _) = b4b_dirs("bad-long");
        write_workflow(&dir, "caller.yml", &yaml, false, false);
        let caught = std::panic::catch_unwind(|| load_cached_pair(&dir, None));
        let (_, found) = caught.expect("long uses panicked");
        let matched = attributed(&found, "caller.yml", FindingKind::InvalidUses);
        assert_eq!(matched.len(), 1, "{found:?}");
        assert!(matched[0].message.contains(&"x".repeat(4000)));
        assert_no_limitation(&matched[0].message);

        let nasty = [
            "\0\0\0",
            "jobs: [\n",
            "uses: &\n",
            "jobs:\n  bad:\n    uses: \"unterminated",
            "a: &a\n  b: *a\n",
        ];
        for text in nasty {
            let parsed = std::panic::catch_unwind(|| scan_yaml(Path::new("odd.yml"), text));
            assert!(parsed.is_ok(), "parser panicked on {text:?}");
            if let Ok(Ok(workflow)) = parsed {
                let found = std::panic::catch_unwind(|| findings(&[workflow]));
                assert!(found.is_ok(), "findings panicked on {text:?}");
            }
        }

        let (dir, _) = b4b_dirs("step-uses");
        write_workflow(
            &dir,
            "caller.yml",
            "name: Caller\non: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    timeout-minutes: 5\n    steps:\n      - uses: acme/tools/.github/workflows/lint.yml@v1\n      - uses: actions/checkout@v4\n",
            false,
            false,
        );
        let (workflows, found) = load_cached_pair(&dir, None);
        let build = job_named(&workflows, "caller.yml", "build");
        assert!(build.uses.is_none());
        assert!(build.invalid_uses.is_none());
        assert!(
            of_kind(&found, FindingKind::InvalidUses).is_empty(),
            "{found:?}"
        );
        assert!(
            of_kind(&found, FindingKind::RemoteWorkflow).is_empty(),
            "{found:?}"
        );

        let (dir, _) = b4b_dirs("last-key-wins");
        write_workflow(
            &dir,
            "caller.yml",
            "name: Caller\non: push\njobs:\n  call-lint:\n    uses: actions/checkout@v4\n    uses: acme/tools/.github/workflows/lint.yml@v1\n",
            false,
            false,
        );
        let (workflows, found) = load_cached_pair(&dir, None);
        let call = job_named(&workflows, "caller.yml", "call-lint");
        assert_eq!(
            call.uses.as_ref().unwrap().spec,
            "acme/tools/.github/workflows/lint.yml@v1"
        );
        assert!(call.invalid_uses.is_none());
        assert_eq!(
            of_kind(&found, FindingKind::DuplicateKey).len(),
            1,
            "{found:?}"
        );
        let shown = attributed(&found, "caller.yml", FindingKind::RemoteWorkflow);
        assert_eq!(shown.len(), 1, "{found:?}");
        assert!(shown[0]
            .message
            .contains("acme/tools/.github/workflows/lint.yml@v1"));
        assert!(
            of_kind(&found, FindingKind::InvalidUses).is_empty(),
            "{found:?}"
        );
    }
}
