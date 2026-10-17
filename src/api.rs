//! Saved GitHub Actions API responses.
//!
//! `--runs DIR` reads `runs-*.json` (a `GET /repos/{owner}/{repo}/actions/runs`
//! body) and `jobs-<run_id>*.json` (a
//! `GET /repos/{owner}/{repo}/actions/runs/{run_id}/jobs?filter=all` body).
//! Several files are pages of one list. Only files directly in the directory
//! are read. Bodies from `attempts/{n}/jobs` are a different list and are
//! not mixed in.
//!
//! A run `path` is stored so it matches a CSV `workflow_path`: the `@ref`
//! suffix (including `@refs/heads/...`) and an `owner/repo/` prefix are removed.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::money::Micros;

/// Phrase used when a job has not finished.
pub const IN_PROGRESS: &str = "in progress";

/// Workflow path used when a job has no matching run record.
pub const UNKNOWN_WORKFLOW: &str = "(unknown workflow)";

/// One workflow run from `workflow_runs[]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowRun {
    pub id: u64,
    pub path: String,
    pub run_attempt: u64,
    pub run_number: u64,
    pub event: String,
    pub conclusion: Option<String>,
    pub run_started_at: Option<String>,
}

impl WorkflowRun {
    /// This run's linked path is the CSV `workflow_path` (ASCII case-insensitive).
    pub fn links_to(&self, workflow_path: &str) -> bool {
        workflow_paths_match(&self.path, workflow_path)
    }
}

/// One job from `jobs[]`.
///
/// `completed_at` is `None` when the field is missing or null. That job is
/// still in progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: u64,
    pub run_id: u64,
    pub run_attempt: u64,
    pub name: String,
    pub labels: Vec<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub conclusion: Option<String>,
    pub runner_name: Option<String>,
}

impl Job {
    /// A finished job has a completion time. An in-progress job does not count.
    pub fn counts(&self) -> bool {
        self.completed_at.is_some()
    }
}

/// Runs and jobs loaded from one directory.
///
/// `jobs` keeps finished jobs and in-progress jobs. [`Job::counts`] selects
/// the finished ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiData {
    pub runs: Vec<WorkflowRun>,
    pub jobs: Vec<Job>,
}

impl ApiData {
    /// Finished jobs, in storage order.
    pub fn counted_jobs(&self) -> impl Iterator<Item = &Job> {
        self.jobs.iter().filter(|job| job.counts())
    }

    /// Jobs with no `completed_at`, in storage order.
    pub fn in_progress_jobs(&self) -> impl Iterator<Item = &Job> {
        self.jobs.iter().filter(|job| !job.counts())
    }

    /// Linked workflow path for `run_id`, or [`UNKNOWN_WORKFLOW`].
    pub fn workflow_path(&self, run_id: u64) -> String {
        self.runs
            .iter()
            .find(|run| run.id == run_id)
            .map(|run| linked_workflow_path(&run.path))
            .unwrap_or_else(|| UNKNOWN_WORKFLOW.to_string())
    }

    /// A job on the latest attempt whose `started_at` is before `run_started_at`.
    ///
    /// GitHub can carry a finished job onto the new attempt. That copy adds
    /// no cost. A job from an earlier attempt stays billable. A missing run,
    /// a different attempt, or a missing or unreadable timestamp is not a copy.
    pub fn is_inherited_copy(&self, job: &Job) -> bool {
        let Some(run) = self.runs.iter().find(|run| run.id == job.run_id) else {
            return false;
        };
        job.run_attempt == run.run_attempt && started_before_run(job, run)
    }

    /// `Some(0)` when `job` is an inherited copy.
    pub fn inherited_copy_net(&self, job: &Job) -> Option<Micros> {
        self.is_inherited_copy(job).then_some(Micros::ZERO)
    }

    /// Finished jobs that add cost. Inherited copies are omitted.
    pub fn billable_jobs(&self) -> impl Iterator<Item = &Job> {
        self.jobs
            .iter()
            .filter(|job| job.counts() && !self.is_inherited_copy(job))
    }
}

/// Drop `@ref` and an `owner/repo/` prefix so a run path matches a CSV workflow path.
///
/// `@main` and `@refs/heads/feature/x` both leave the workflow file. The prefix
/// is removed only when exactly two segments sit in front of `.github/workflows`.
/// Matching later is ASCII case-insensitive; this function keeps the workflow
/// path's own case.
pub fn linked_workflow_path(raw: &str) -> String {
    let trimmed = raw.trim().trim_matches('\u{feff}').trim_matches('\r');
    let without_ref = trimmed
        .split_once('@')
        .map(|(path, _reference)| path)
        .unwrap_or(trimmed);
    let without_ref = without_ref.trim().trim_matches('\r');
    let slashed = without_ref.replace('\\', "/");
    strip_owner_repo_prefix(&slashed)
}

/// Both paths refer to the same workflow file after linking.
pub fn workflow_paths_match(left: &str, right: &str) -> bool {
    let left = linked_workflow_path(left);
    let right = linked_workflow_path(right);
    !left.is_empty() && left.eq_ignore_ascii_case(&right)
}

/// The runs directory or one JSON file could not be read. Exit code 2.
///
/// Messages name the kind of problem. They do not include the file body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub path: PathBuf,
    kind: ApiErrorKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApiErrorKind {
    Directory,
    File,
    Json,
    DuplicateId,
    RunMismatch,
}

impl ApiError {
    fn directory(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            kind: ApiErrorKind::Directory,
        }
    }

    fn file(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            kind: ApiErrorKind::File,
        }
    }

    fn json(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            kind: ApiErrorKind::Json,
        }
    }

    fn duplicate(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            kind: ApiErrorKind::DuplicateId,
        }
    }

    fn mismatch(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            kind: ApiErrorKind::RunMismatch,
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self.kind {
            ApiErrorKind::Directory => "cannot read directory",
            ApiErrorKind::File => "cannot read file",
            ApiErrorKind::Json => "invalid JSON",
            ApiErrorKind::DuplicateId => "duplicate id",
            ApiErrorKind::RunMismatch => "run id does not match file name",
        };
        f.write_str(message)
    }
}

impl std::error::Error for ApiError {}

/// Read every runs page and `jobs?filter=all` page in `dir`.
///
/// Names are sorted, then pages are merged. A later file that repeats a run
/// id is an error. A repeated job id counts once: the first record in file-name
/// order, then array order, is kept. Each job's `run_id` must match the
/// `jobs-<run_id>` prefix of its file (`jobs-15-2.json` is run 15,
/// `jobs-150.json` is run 150). Files from `attempts/{n}/jobs` are not read.
pub fn read_dir(dir: &Path) -> Result<ApiData, ApiError> {
    let mut runs = Vec::new();
    let mut jobs = Vec::new();
    let mut seen_runs = HashSet::new();
    let mut seen_jobs = HashSet::new();

    for path in matching_files(dir)? {
        let name = path
            .file_name()
            .and_then(|item| item.to_str())
            .ok_or_else(|| ApiError::file(&path))?;
        let text = read_text(&path)?;
        if is_runs_file(name) {
            let parsed = parse_runs(&text).map_err(|_| ApiError::json(&path))?;
            for run in parsed {
                if !seen_runs.insert(run.id) {
                    return Err(ApiError::duplicate(&path));
                }
                runs.push(run);
            }
        } else {
            let file_run_id = job_file_run_id(name).ok_or_else(|| ApiError::file(&path))?;
            let parsed = parse_jobs(&text).map_err(|_| ApiError::json(&path))?;
            for job in parsed {
                if job.run_id != file_run_id {
                    return Err(ApiError::mismatch(&path));
                }
                // The same job id on another filter=all page counts once.
                if !seen_jobs.insert(job.id) {
                    continue;
                }
                jobs.push(job);
            }
        }
    }

    runs.sort_by(cmp_run);
    jobs.sort_by(cmp_job);
    Ok(ApiData { runs, jobs })
}

fn matching_files(dir: &Path) -> Result<Vec<PathBuf>, ApiError> {
    let entries = fs::read_dir(dir).map_err(|_| ApiError::directory(dir))?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| ApiError::directory(dir))?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|item| item.to_str()) else {
            continue;
        };
        if !is_runs_file(name) && !is_filter_all_jobs_name(name) {
            continue;
        }
        if !path.is_file() {
            return Err(ApiError::file(&path));
        }
        paths.push(path);
    }
    paths.sort();
    Ok(paths)
}

fn read_text(path: &Path) -> Result<String, ApiError> {
    let bytes = fs::read(path).map_err(|_| ApiError::file(path))?;
    let without_bom = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    String::from_utf8(without_bom.to_vec()).map_err(|_| ApiError::json(path))
}

fn is_runs_file(name: &str) -> bool {
    let Some(middle) = name
        .strip_prefix("runs-")
        .and_then(|rest| rest.strip_suffix(".json"))
    else {
        return false;
    };
    !middle.is_empty()
}

/// `jobs-<run_id>*.json` from `jobs?filter=all`, not from `attempts/{n}/jobs`.
///
/// A file-name segment `attempt` or `attempts` is the per-attempt endpoint
/// (`jobs-15-attempts-1.json`). Those files are left unread. Pagination names
/// such as `jobs-15-2.json` stay included. Subdirectories, including
/// `attempts/{n}/`, are not listed.
fn is_filter_all_jobs_name(name: &str) -> bool {
    if name_has_attempt_segment(name) {
        return false;
    }
    job_file_run_id(name).is_some()
}

fn name_has_attempt_segment(name: &str) -> bool {
    name.split(|character: char| !character.is_ascii_alphanumeric())
        .any(|part| part.eq_ignore_ascii_case("attempt") || part.eq_ignore_ascii_case("attempts"))
}

/// Run id encoded by `jobs-<run_id>*.json`.
///
/// The id is the leading digit run. `jobs-15-2.json` is run 15 and
/// `jobs-150.json` is run 150.
fn job_file_run_id(name: &str) -> Option<u64> {
    let rest = name.strip_prefix("jobs-")?.strip_suffix(".json")?;
    if rest.is_empty() {
        return None;
    }
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    rest[..end].parse().ok()
}

fn parse_runs(text: &str) -> Result<Vec<WorkflowRun>, ()> {
    let value: Value = serde_json::from_str(text).map_err(|_| ())?;
    let items = value
        .get("workflow_runs")
        .and_then(Value::as_array)
        .ok_or(())?;
    items.iter().map(parse_run).collect()
}

fn parse_run(value: &Value) -> Result<WorkflowRun, ()> {
    let object = value.as_object().ok_or(())?;
    Ok(WorkflowRun {
        id: require_u64(object, "id")?,
        path: linked_workflow_path(&require_string(object, "path")?),
        run_attempt: require_u64(object, "run_attempt")?,
        run_number: require_u64(object, "run_number")?,
        event: require_string(object, "event")?,
        conclusion: optional_string(object, "conclusion")?,
        run_started_at: optional_string(object, "run_started_at")?,
    })
}

fn parse_jobs(text: &str) -> Result<Vec<Job>, ()> {
    let value: Value = serde_json::from_str(text).map_err(|_| ())?;
    let items = value.get("jobs").and_then(Value::as_array).ok_or(())?;
    items.iter().map(parse_job).collect()
}

fn parse_job(value: &Value) -> Result<Job, ()> {
    let object = value.as_object().ok_or(())?;
    Ok(Job {
        id: require_u64(object, "id")?,
        run_id: require_u64(object, "run_id")?,
        run_attempt: require_u64(object, "run_attempt")?,
        name: require_string(object, "name")?,
        labels: optional_labels(object)?,
        started_at: optional_string(object, "started_at")?,
        completed_at: optional_string(object, "completed_at")?,
        conclusion: optional_string(object, "conclusion")?,
        runner_name: optional_string(object, "runner_name")?,
    })
}

fn require_u64(object: &Map<String, Value>, key: &str) -> Result<u64, ()> {
    object.get(key).and_then(Value::as_u64).ok_or(())
}

fn require_string(object: &Map<String, Value>, key: &str) -> Result<String, ()> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(())
}

fn optional_string(object: &Map<String, Value>, key: &str) -> Result<Option<String>, ()> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(()),
    }
}

fn optional_labels(object: &Map<String, Value>) -> Result<Vec<String>, ()> {
    match object.get("labels") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().map(str::to_owned).ok_or(()))
            .collect(),
        Some(_) => Err(()),
    }
}

fn strip_owner_repo_prefix(path: &str) -> String {
    let Some(index) = find_workflows_marker(path) else {
        return path.to_string();
    };
    let prefix = path[..index].trim_end_matches('/');
    if prefix.is_empty() {
        return path[index..].to_string();
    }
    let parts: Vec<&str> = prefix.split('/').filter(|part| !part.is_empty()).collect();
    if parts.len() == 2 {
        path[index..].to_string()
    } else {
        path.to_string()
    }
}

fn find_workflows_marker(path: &str) -> Option<usize> {
    let bytes = path.as_bytes();
    const NEEDLE: &[u8] = b".github/workflows";
    if bytes.len() < NEEDLE.len() {
        return None;
    }
    let last = bytes.len() - NEEDLE.len();
    let mut index = 0;
    while index <= last {
        if eq_ignore_ascii(&bytes[index..index + NEEDLE.len()], NEEDLE) {
            let at_boundary = index == 0 || bytes[index - 1] == b'/';
            let after = index + NEEDLE.len();
            let at_end = after == bytes.len() || bytes[after] == b'/';
            if at_boundary && at_end {
                return Some(index);
            }
        }
        index += 1;
    }
    None
}

fn eq_ignore_ascii(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left_byte, right_byte)| left_byte.eq_ignore_ascii_case(right_byte))
}

fn started_before_run(job: &Job, run: &WorkflowRun) -> bool {
    let Some(started) = job.started_at.as_deref().and_then(parse_github_time) else {
        return false;
    };
    let Some(run_started) = run.run_started_at.as_deref().and_then(parse_github_time) else {
        return false;
    };
    started < run_started
}

/// Nanoseconds since the Unix epoch. `None` when the text is not a timestamp.
fn parse_github_time(raw: &str) -> Option<i128> {
    let text = raw.trim().trim_matches('\r');
    let bytes = text.as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    if bytes[4] != b'-' || bytes[7] != b'-' || bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    let year = parse_digits(&bytes[0..4])?;
    let month = parse_digits(&bytes[5..7])?;
    let day = parse_digits(&bytes[8..10])?;
    if !matches!(bytes[10], b'T' | b't' | b' ') {
        return None;
    }
    let hour = parse_digits(&bytes[11..13])?;
    let minute = parse_digits(&bytes[14..16])?;
    let second = parse_digits(&bytes[17..19])?;
    if !valid_ymd(year, month, day) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let mut index = 19;
    let mut nanos: i128 = 0;
    if index < bytes.len() && bytes[index] == b'.' {
        index += 1;
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index == start {
            return None;
        }
        nanos = fraction_nanos(&text[start..index])?;
    }
    let offset_minutes = parse_offset(&bytes[index..])?;
    let days = days_from_civil(i32::try_from(year).ok()?, month, day);
    let seconds = days
        .checked_mul(86_400)?
        .checked_add(i64::from(hour) * 3_600)?
        .checked_add(i64::from(minute) * 60)?
        .checked_add(i64::from(second))?;
    let utc = seconds.checked_sub(offset_minutes.checked_mul(60)?)?;
    Some(i128::from(utc) * 1_000_000_000 + nanos)
}

fn fraction_nanos(digits: &str) -> Option<i128> {
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let mut padded = String::new();
    for (index, byte) in digits.bytes().enumerate() {
        if index == 9 {
            break;
        }
        padded.push(char::from(byte));
    }
    while padded.len() < 9 {
        padded.push('0');
    }
    padded.parse().ok()
}

fn parse_offset(bytes: &[u8]) -> Option<i64> {
    if bytes == b"Z" || bytes == b"z" {
        return Some(0);
    }
    if bytes.is_empty() {
        return None;
    }
    let sign: i64 = match bytes[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let rest = &bytes[1..];
    let (hours, minutes) = match rest.len() {
        2 => (parse_digits(rest)?, 0),
        4 => (parse_digits(&rest[0..2])?, parse_digits(&rest[2..4])?),
        5 => {
            if rest[2] != b':' {
                return None;
            }
            (parse_digits(&rest[0..2])?, parse_digits(&rest[3..5])?)
        }
        _ => return None,
    };
    if hours > 23 || minutes > 59 {
        return None;
    }
    Some(sign * (i64::from(hours) * 60 + i64::from(minutes)))
}

fn parse_digits(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() {
        return None;
    }
    let mut value: u32 = 0;
    for byte in bytes {
        let digit = byte.checked_sub(b'0')?;
        if digit > 9 {
            return None;
        }
        value = value.checked_mul(10)?.checked_add(u32::from(digit))?;
    }
    Some(value)
}

fn valid_ymd(year: u32, month: u32, day: u32) -> bool {
    if !(1..=12).contains(&month) || day < 1 {
        return false;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let max = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    };
    day <= max
}

/// Days since 1970-01-01. Howard Hinnant's civil-from-days algorithm.
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let mut y = i64::from(year);
    if month <= 2 {
        y -= 1;
    }
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = u64::try_from(y - era * 400).unwrap_or(0);
    let month_adj = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * month_adj + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + u64::from(doy);
    era * 146097 + i64::try_from(doe).unwrap_or(0) - 719468
}

fn cmp_run(left: &WorkflowRun, right: &WorkflowRun) -> std::cmp::Ordering {
    left.path
        .cmp(&right.path)
        .then(left.run_number.cmp(&right.run_number))
        .then(left.run_attempt.cmp(&right.run_attempt))
        .then(left.id.cmp(&right.id))
}

fn cmp_job(left: &Job, right: &Job) -> std::cmp::Ordering {
    left.run_id
        .cmp(&right.run_id)
        .then(left.name.cmp(&right.name))
        .then(left.run_attempt.cmp(&right.run_attempt))
        .then(left.id.cmp(&right.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join("ci-bill-api-unit").join(name);
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn write_json(dir: &Path, name: &str, value: &Value) {
        fs::write(dir.join(name), serde_json::to_vec_pretty(value).unwrap()).unwrap();
    }

    fn run_value(id: u64, path: &str) -> Value {
        serde_json::json!({
            "id": id,
            "name": "CI",
            "path": path,
            "run_attempt": 1,
            "run_number": id,
            "event": "push",
            "status": "completed",
            "conclusion": "success",
            "run_started_at": "2026-03-01T00:00:00Z",
            "actor": {"login": "dev", "id": 1},
            "previous_attempt_url": null
        })
    }

    fn job_value(id: u64, run_id: u64, name: &str) -> Value {
        serde_json::json!({
            "id": id,
            "run_id": run_id,
            "run_attempt": 1,
            "name": name,
            "status": "completed",
            "conclusion": "success",
            "started_at": "2026-03-01T00:00:00Z",
            "completed_at": "2026-03-01T00:01:00Z",
            "labels": ["ubuntu-latest"],
            "runner_name": "runner-a",
            "runner_id": 4,
            "steps": [{"name": "checkout", "conclusion": "success"}]
        })
    }

    fn runs_body(runs: Vec<Value>) -> Value {
        serde_json::json!({"total_count": runs.len(), "workflow_runs": runs})
    }

    fn jobs_body(jobs: Vec<Value>) -> Value {
        serde_json::json!({"total_count": jobs.len(), "jobs": jobs})
    }

    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/api")
    }

    #[test]
    fn in_progress_phrase_is_stable() {
        assert_eq!(IN_PROGRESS, "in progress");
        assert_eq!(UNKNOWN_WORKFLOW, "(unknown workflow)");
    }

    #[test]
    fn job_file_names_use_the_leading_run_id() {
        assert_eq!(job_file_run_id("jobs-15.json"), Some(15));
        assert_eq!(job_file_run_id("jobs-15-2.json"), Some(15));
        assert_eq!(job_file_run_id("jobs-15_page3.json"), Some(15));
        assert_eq!(job_file_run_id("jobs-15-page-3.json"), Some(15));
        assert_eq!(job_file_run_id("jobs-015.json"), Some(15));
        assert_eq!(job_file_run_id("jobs-150.json"), Some(150));
        assert_eq!(job_file_run_id("jobs-150-2.json"), Some(150));
        assert_eq!(job_file_run_id("jobs-0.json"), Some(0));
        assert_eq!(job_file_run_id("jobs-.json"), None);
        assert_eq!(job_file_run_id("jobs-x.json"), None);
        assert_eq!(job_file_run_id("jobs-15.txt"), None);
        assert_eq!(job_file_run_id("job-15.json"), None);
        assert_eq!(job_file_run_id("Jobs-15.json"), None);
        assert_eq!(job_file_run_id("jobs-15.json.bak"), None);
        assert_eq!(job_file_run_id("runs-15.json"), None);
        assert!(is_runs_file("runs-1.json"));
        assert!(is_runs_file("runs-02.json"));
        assert!(is_runs_file("runs-page-3.json"));
        assert!(!is_runs_file("runs-.json"));
        assert!(!is_runs_file("runs.json"));
        assert!(!is_runs_file("runs-1.JSON"));
        assert!(!is_runs_file("runs-1.txt"));
        assert!(!is_runs_file("my-runs-1.json"));
    }

    #[test]
    fn fixture_pages_are_read() {
        let readme = fs::read_to_string(fixture_dir().join("README.md")).unwrap();
        assert!(
            readme.to_ascii_lowercase().contains("sample"),
            "fixture README should mark the data as sample"
        );

        let data = read_dir(&fixture_dir()).unwrap();
        assert_eq!(
            data.runs,
            vec![
                WorkflowRun {
                    id: 1001,
                    path: ".github/workflows/ci.yml".to_string(),
                    run_attempt: 1,
                    run_number: 40,
                    event: "push".to_string(),
                    conclusion: Some("success".to_string()),
                    run_started_at: Some("2026-03-01T10:00:00Z".to_string()),
                },
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
                    id: 1003,
                    path: ".github/workflows/release.yml".to_string(),
                    run_attempt: 1,
                    run_number: 7,
                    event: "release".to_string(),
                    conclusion: Some("failure".to_string()),
                    run_started_at: Some("2026-03-03T12:00:00Z".to_string()),
                },
            ]
        );
        assert_eq!(
            data.jobs,
            vec![
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
                    id: 502,
                    run_id: 1001,
                    run_attempt: 1,
                    name: "test".to_string(),
                    labels: vec!["ubuntu-latest".to_string(), "linux".to_string()],
                    started_at: Some("2026-03-01T10:02:06Z".to_string()),
                    completed_at: Some("2026-03-01T10:04:00Z".to_string()),
                    conclusion: Some("success".to_string()),
                    runner_name: Some("GitHub Actions 89".to_string()),
                },
                Job {
                    id: 602,
                    run_id: 1002,
                    run_attempt: 2,
                    name: "fmt".to_string(),
                    labels: vec!["ubuntu-latest".to_string()],
                    started_at: Some("2026-03-02T11:00:12Z".to_string()),
                    completed_at: Some("2026-03-02T11:01:12Z".to_string()),
                    conclusion: Some("success".to_string()),
                    runner_name: Some("GitHub Actions 91".to_string()),
                },
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
                    id: 701,
                    run_id: 1003,
                    run_attempt: 1,
                    name: "publish".to_string(),
                    labels: vec!["windows-latest".to_string()],
                    started_at: Some("2026-03-03T12:00:01Z".to_string()),
                    completed_at: Some("2026-03-03T12:08:01Z".to_string()),
                    conclusion: Some("failure".to_string()),
                    runner_name: None,
                },
            ]
        );

        let counted: Vec<u64> = data.counted_jobs().map(|job| job.id).collect();
        let running: Vec<u64> = data.in_progress_jobs().map(|job| job.id).collect();
        assert_eq!(counted, vec![501, 502, 602, 701]);
        assert_eq!(running, vec![601]);
        let lint = data.jobs.iter().find(|job| job.id == 601).unwrap();
        assert!(!lint.counts());
        assert!(lint.completed_at.is_none());
        assert_eq!(data.workflow_path(1002), ".github/workflows/lint.yml");
        assert!(data
            .jobs
            .iter()
            .any(|job| job.counts() && job.name == "fmt"));
    }

    #[test]
    fn pagination_merges_every_page() {
        let dir = scratch("pages");
        write_json(
            &dir,
            "runs-2.json",
            &runs_body(vec![run_value(2, ".github/workflows/b.yml")]),
        );
        write_json(
            &dir,
            "runs-10.json",
            &runs_body(vec![run_value(10, ".github/workflows/c.yml")]),
        );
        write_json(
            &dir,
            "runs-page-3.json",
            &runs_body(vec![run_value(3, ".github/workflows/a.yml")]),
        );
        write_json(&dir, "runs-9.json", &runs_body(vec![]));
        write_json(
            &dir,
            "jobs-2.json",
            &jobs_body(vec![job_value(20, 2, "build")]),
        );
        write_json(
            &dir,
            "jobs-2-2.json",
            &jobs_body(vec![job_value(21, 2, "test")]),
        );
        let mut queued = job_value(22, 2, "lint");
        queued["completed_at"] = Value::Null;
        queued["conclusion"] = Value::Null;
        write_json(&dir, "jobs-2_page3.json", &jobs_body(vec![queued]));
        write_json(&dir, "jobs-10.json", &jobs_body(vec![]));
        write_json(
            &dir,
            "jobs-3.json",
            &jobs_body(vec![job_value(30, 3, "pack")]),
        );

        let nested = dir.join("nested");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("runs-1.json"), b"{").unwrap();
        fs::write(dir.join("README.md"), "sample notes {").unwrap();
        fs::write(dir.join("notes.json"), b"{").unwrap();
        fs::write(dir.join("runs.json"), b"{").unwrap();
        fs::write(dir.join("runs-1.txt"), b"{").unwrap();

        let data = read_dir(&dir).unwrap();
        let run_ids: Vec<u64> = data.runs.iter().map(|run| run.id).collect();
        assert_eq!(run_ids, vec![3, 2, 10]);
        assert_eq!(
            data.runs[0].path, ".github/workflows/a.yml",
            "path order, not file-name order"
        );
        let job_ids: Vec<u64> = data.jobs.iter().map(|job| job.id).collect();
        assert_eq!(job_ids, vec![20, 22, 21, 30]);
        assert_eq!(
            data.counted_jobs().map(|job| job.id).collect::<Vec<_>>(),
            vec![20, 21, 30]
        );
        assert_eq!(
            data.in_progress_jobs()
                .map(|job| job.id)
                .collect::<Vec<_>>(),
            vec![22]
        );
        assert!(!data.jobs.iter().find(|job| job.id == 22).unwrap().counts());
    }

    #[test]
    fn jobs_150_is_a_different_run_from_jobs_15() {
        let dir = scratch("id-boundary");
        write_json(
            &dir,
            "jobs-15.json",
            &jobs_body(vec![job_value(1, 15, "one")]),
        );
        write_json(
            &dir,
            "jobs-15-2.json",
            &jobs_body(vec![job_value(2, 15, "two")]),
        );
        write_json(
            &dir,
            "jobs-15_page3.json",
            &jobs_body(vec![job_value(3, 15, "three")]),
        );
        write_json(
            &dir,
            "jobs-150.json",
            &jobs_body(vec![job_value(4, 150, "wide")]),
        );
        write_json(
            &dir,
            "jobs-150-2.json",
            &jobs_body(vec![job_value(5, 150, "wider")]),
        );

        let data = read_dir(&dir).unwrap();
        let for_15: Vec<u64> = data
            .jobs
            .iter()
            .filter(|job| job.run_id == 15)
            .map(|job| job.id)
            .collect();
        let for_150: Vec<u64> = data
            .jobs
            .iter()
            .filter(|job| job.run_id == 150)
            .map(|job| job.id)
            .collect();
        assert_eq!(for_15, vec![1, 3, 2]);
        assert_eq!(for_150, vec![4, 5]);
        assert!(data.runs.is_empty());
        assert_eq!(data.workflow_path(15), UNKNOWN_WORKFLOW);
    }

    #[test]
    fn optional_fields_null_or_absent_follow_completed_at() {
        let dir = scratch("optional");
        let mut runs = vec![
            run_value(100, ".github/workflows/ci.yml"),
            run_value(101, ".github/workflows/lint.yml"),
            run_value(102, ".github/workflows/docs.yml"),
            run_value(103, ".github/workflows/release.yml"),
        ];
        runs[1]["conclusion"] = Value::Null;
        runs[2]["run_started_at"] = Value::Null;
        runs[2]["conclusion"] = serde_json::json!("success");
        runs[3].as_object_mut().unwrap().remove("run_started_at");
        runs[3].as_object_mut().unwrap().remove("conclusion");
        write_json(&dir, "runs-1.json", &runs_body(runs));

        let mut jobs = Vec::new();
        let mut push = |id: u64, name: &str, edit: fn(&mut Value)| {
            let mut job = job_value(id, 100, name);
            edit(&mut job);
            jobs.push(job);
        };
        push(1, "full", |_| {});
        push(2, "null-completed", |job| {
            job["completed_at"] = Value::Null;
            job["conclusion"] = Value::Null;
            job["status"] = serde_json::json!("completed");
        });
        push(3, "absent-completed", |job| {
            let object = job.as_object_mut().unwrap();
            object.remove("completed_at");
            object.remove("conclusion");
            object.remove("started_at");
        });
        push(4, "null-conclusion", |job| {
            job["conclusion"] = Value::Null;
        });
        push(5, "absent-conclusion", |job| {
            job.as_object_mut().unwrap().remove("conclusion");
        });
        push(6, "null-runner", |job| {
            job["runner_name"] = Value::Null;
        });
        push(7, "absent-runner", |job| {
            job.as_object_mut().unwrap().remove("runner_name");
        });
        push(8, "null-started", |job| {
            job["started_at"] = Value::Null;
        });
        push(9, "absent-started", |job| {
            job.as_object_mut().unwrap().remove("started_at");
        });
        push(10, "null-labels", |job| {
            job["labels"] = Value::Null;
        });
        push(11, "absent-labels", |job| {
            job.as_object_mut().unwrap().remove("labels");
        });
        push(12, "empty-labels", |job| {
            job["labels"] = serde_json::json!([]);
        });
        push(13, "status-running-but-finished", |job| {
            job["status"] = serde_json::json!("in_progress");
        });
        push(14, "status-done-but-open", |job| {
            job["status"] = serde_json::json!("completed");
            job["completed_at"] = Value::Null;
        });
        write_json(&dir, "jobs-100.json", &jobs_body(jobs));

        let data = read_dir(&dir).unwrap();
        assert_eq!(data.runs.len(), 4);
        assert!(data
            .runs
            .iter()
            .any(|run| run.id == 101 && run.conclusion.is_none()));
        assert!(data
            .runs
            .iter()
            .any(|run| run.id == 102 && run.run_started_at.is_none()));
        assert!(data
            .runs
            .iter()
            .any(|run| run.id == 103 && run.conclusion.is_none() && run.run_started_at.is_none()));

        let by_id = |id: u64| data.jobs.iter().find(|job| job.id == id).unwrap();
        for id in [1, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13] {
            assert!(by_id(id).counts(), "job {id} should count");
        }
        for id in [2, 3, 14] {
            assert!(!by_id(id).counts(), "job {id} should be in progress");
            assert!(by_id(id).completed_at.is_none(), "job {id}");
        }
        assert_eq!(
            data.in_progress_jobs()
                .map(|job| job.id)
                .collect::<Vec<_>>(),
            vec![3, 2, 14]
        );
        assert!(by_id(4).conclusion.is_none());
        assert!(by_id(5).conclusion.is_none());
        assert!(by_id(6).runner_name.is_none());
        assert!(by_id(7).runner_name.is_none());
        assert!(by_id(8).started_at.is_none());
        assert!(by_id(9).started_at.is_none());
        assert!(by_id(10).labels.is_empty());
        assert!(by_id(11).labels.is_empty());
        assert!(by_id(12).labels.is_empty());
        assert!(by_id(13).completed_at.is_some());
        assert_eq!(by_id(1).labels, vec!["ubuntu-latest".to_string()]);
    }

    #[test]
    fn each_required_run_field_must_be_present_and_typed() {
        for key in ["id", "path", "run_attempt", "run_number", "event"] {
            let dir = scratch(&format!("run-missing-{key}"));
            let mut run = run_value(1, ".github/workflows/ci.yml");
            run.as_object_mut().unwrap().remove(key);
            write_json(&dir, "runs-1.json", &runs_body(vec![run]));
            let err = read_dir(&dir).unwrap_err();
            assert_eq!(err.to_string(), "invalid JSON", "{key}");
            assert_eq!(err.path, dir.join("runs-1.json"));
        }

        let bad = [
            ("id", serde_json::json!("1")),
            ("id", Value::Null),
            ("id", serde_json::json!(-1)),
            ("id", serde_json::json!(1.5)),
            ("id", Value::Bool(true)),
            ("path", serde_json::json!(1)),
            ("path", Value::Null),
            ("run_attempt", serde_json::json!("1")),
            ("run_number", serde_json::json!([])),
            ("event", Value::Bool(false)),
            ("conclusion", serde_json::json!(1)),
            ("run_started_at", serde_json::json!(true)),
        ];
        for (index, (key, replacement)) in bad.iter().enumerate() {
            let dir = scratch(&format!("run-type-{index}"));
            let mut run = run_value(1, ".github/workflows/ci.yml");
            run[key] = replacement.clone();
            write_json(&dir, "runs-4.json", &runs_body(vec![run]));
            let err = read_dir(&dir).unwrap_err();
            assert_eq!(err.to_string(), "invalid JSON", "{key}");
            assert_eq!(err.path, dir.join("runs-4.json"));
        }
    }

    #[test]
    fn each_required_job_field_must_be_present_and_typed() {
        for key in ["id", "run_id", "run_attempt", "name"] {
            let dir = scratch(&format!("job-missing-{key}"));
            let mut job = job_value(10, 4, "build");
            job.as_object_mut().unwrap().remove(key);
            write_json(&dir, "jobs-4.json", &jobs_body(vec![job]));
            let err = read_dir(&dir).unwrap_err();
            assert_eq!(err.to_string(), "invalid JSON", "{key}");
            assert_eq!(err.path, dir.join("jobs-4.json"));
        }

        let bad = [
            ("id", serde_json::json!("10")),
            ("run_id", serde_json::json!(1.5)),
            ("run_attempt", Value::Null),
            ("name", serde_json::json!(10)),
            ("labels", serde_json::json!("ubuntu-latest")),
            ("labels", serde_json::json!([1])),
            ("started_at", serde_json::json!(0)),
            ("completed_at", serde_json::json!(60)),
            ("conclusion", serde_json::json!(["success"])),
            ("runner_name", serde_json::json!(8)),
        ];
        for (index, (key, replacement)) in bad.iter().enumerate() {
            let dir = scratch(&format!("job-type-{index}"));
            let mut job = job_value(10, 4, "build");
            job[key] = replacement.clone();
            write_json(&dir, "jobs-4-2.json", &jobs_body(vec![job]));
            let err = read_dir(&dir).unwrap_err();
            assert_eq!(err.to_string(), "invalid JSON", "{key}");
            assert_eq!(err.path, dir.join("jobs-4-2.json"));
            assert!(!err.to_string().contains("ubuntu-latest"));
        }
    }

    #[test]
    fn broken_json_names_the_file_and_hides_the_body() {
        let secret = "LEAKED_SECRET_TOKEN";
        let cases = [
            ("runs-1.json", format!("{{ {secret}")),
            ("runs-2.json", secret.to_string()),
            ("runs-3.json", format!(r#"{{"workflow_runs":"{secret}"}}"#)),
            ("runs-4.json", "[]".to_string()),
            ("runs-5.json", "null".to_string()),
            ("runs-6.json", String::new()),
            (
                "runs-7.json",
                format!(r#"{{"workflow_runs":[1,"{secret}"]}}"#),
            ),
            (
                "jobs-9.json",
                format!(r#"{{"jobs":[{{"name":"{secret}"}}]}}"#),
            ),
            ("jobs-9-2.json", "{".to_string()),
            ("jobs-8.json", "null".to_string()),
        ];
        for (index, (name, body)) in cases.iter().enumerate() {
            let dir = scratch(&format!("broken-{index}"));
            fs::write(dir.join(name), body).unwrap();
            let err = read_dir(&dir).unwrap_err();
            assert_eq!(err.to_string(), "invalid JSON", "{name}");
            assert_eq!(err.path, dir.join(name));
            assert!(!err.to_string().contains(secret), "{name}");
        }

        let dir = scratch("second-page-bad");
        write_json(
            &dir,
            "runs-1.json",
            &runs_body(vec![run_value(1, ".github/workflows/ci.yml")]),
        );
        fs::write(
            dir.join("runs-2.json"),
            format!(r#"{{"workflow_runs":"{secret}"}}"#),
        )
        .unwrap();
        let err = read_dir(&dir).unwrap_err();
        assert_eq!(err.path, dir.join("runs-2.json"));
        assert_eq!(err.to_string(), "invalid JSON");
        assert!(!err.to_string().contains(secret));
    }

    #[test]
    fn duplicate_ids_name_the_later_file() {
        let dir = scratch("duplicate-runs");
        write_json(
            &dir,
            "runs-1.json",
            &runs_body(vec![run_value(5, ".github/workflows/a.yml")]),
        );
        write_json(
            &dir,
            "runs-2.json",
            &runs_body(vec![run_value(5, ".github/workflows/b.yml")]),
        );
        let err = read_dir(&dir).unwrap_err();
        assert_eq!(err.to_string(), "duplicate id");
        assert_eq!(err.path, dir.join("runs-2.json"));

        let dir = scratch("duplicate-in-file");
        write_json(
            &dir,
            "runs-1.json",
            &runs_body(vec![
                run_value(5, ".github/workflows/a.yml"),
                run_value(5, ".github/workflows/b.yml"),
            ]),
        );
        let err = read_dir(&dir).unwrap_err();
        assert_eq!(err.to_string(), "duplicate id");
        assert_eq!(err.path, dir.join("runs-1.json"));
    }

    #[test]
    fn job_run_id_must_match_the_file_name() {
        let dir = scratch("mismatch");
        write_json(
            &dir,
            "jobs-15.json",
            &jobs_body(vec![job_value(1, 16, "build")]),
        );
        let err = read_dir(&dir).unwrap_err();
        assert_eq!(err.to_string(), "run id does not match file name");
        assert_eq!(err.path, dir.join("jobs-15.json"));
        assert!(!err.to_string().contains("build"));

        let dir = scratch("mismatch-150");
        write_json(
            &dir,
            "jobs-150.json",
            &jobs_body(vec![job_value(1, 15, "build")]),
        );
        let err = read_dir(&dir).unwrap_err();
        assert_eq!(err.to_string(), "run id does not match file name");
        assert_eq!(err.path, dir.join("jobs-150.json"));
    }

    #[test]
    fn utf8_bom_is_accepted() {
        let dir = scratch("bom");
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend(br#"{"total_count":0,"workflow_runs":[]}"#);
        fs::write(dir.join("runs-1.json"), bytes).unwrap();
        let data = read_dir(&dir).unwrap();
        assert!(data.runs.is_empty());
        assert!(data.jobs.is_empty());
    }

    #[test]
    fn invalid_utf8_is_invalid_json() {
        let dir = scratch("utf8");
        fs::write(dir.join("jobs-3.json"), [0xFF, 0xFE, b'{']).unwrap();
        let err = read_dir(&dir).unwrap_err();
        assert_eq!(err.to_string(), "invalid JSON");
        assert_eq!(err.path, dir.join("jobs-3.json"));
    }

    #[test]
    fn missing_directory_and_file_path_fail() {
        let missing = scratch("missing-parent").join("nope");
        let err = read_dir(&missing).unwrap_err();
        assert_eq!(err.to_string(), "cannot read directory");
        assert_eq!(err.path, missing);

        let dir = scratch("file-not-dir");
        let file = dir.join("runs-1.json");
        fs::write(&file, b"{}").unwrap();
        let err = read_dir(&file).unwrap_err();
        assert_eq!(err.to_string(), "cannot read directory");
        assert_eq!(err.path, file);
    }

    #[test]
    fn directory_named_like_a_page_cannot_be_read() {
        let dir = scratch("dir-page");
        fs::create_dir(dir.join("runs-1.json")).unwrap();
        let err = read_dir(&dir).unwrap_err();
        assert_eq!(err.to_string(), "cannot read file");
        assert_eq!(err.path, dir.join("runs-1.json"));
    }

    #[test]
    fn empty_directory_has_no_runs_or_jobs() {
        let dir = scratch("empty");
        let data = read_dir(&dir).unwrap();
        assert!(data.runs.is_empty());
        assert!(data.jobs.is_empty());
        assert_eq!(data.counted_jobs().count(), 0);
        assert_eq!(data.in_progress_jobs().count(), 0);
    }

    #[test]
    fn non_object_list_items_are_invalid_json() {
        let dir = scratch("bad-items");
        fs::write(dir.join("runs-1.json"), br#"{"workflow_runs":[null]}"#).unwrap();
        let err = read_dir(&dir).unwrap_err();
        assert_eq!(err.to_string(), "invalid JSON");
        assert_eq!(err.path, dir.join("runs-1.json"));

        fs::write(dir.join("jobs-1.json"), br#"{"jobs":[{}]}"#).unwrap();
        fs::remove_file(dir.join("runs-1.json")).unwrap();
        let err = read_dir(&dir).unwrap_err();
        assert_eq!(err.to_string(), "invalid JSON");
        assert_eq!(err.path, dir.join("jobs-1.json"));
    }

    fn assert_links(raw: &str, stored: &str, csv_path: &str, matches: bool) {
        let linked = linked_workflow_path(raw);
        assert_eq!(linked, stored, "{raw}");
        assert_eq!(linked_workflow_path(&linked), linked, "{raw}");
        assert_eq!(workflow_paths_match(raw, csv_path), matches, "{raw}");
        assert_eq!(workflow_paths_match(&linked, csv_path), matches, "{raw}");
    }

    #[test]
    fn run_paths_link_to_csv_workflow_paths() {
        let cases = [
            (
                ".github/workflows/build.yml@main",
                ".github/workflows/build.yml",
                ".github/workflows/build.yml",
                true,
            ),
            (
                ".github/workflows/ci.yml@refs/heads/main",
                ".github/workflows/ci.yml",
                ".github/workflows/ci.yml",
                true,
            ),
            (
                ".github/workflows/ci.yml@refs/heads/feature/foo",
                ".github/workflows/ci.yml",
                ".github/workflows/ci.yml",
                true,
            ),
            (
                ".github/workflows/ci.yml@REFS/HEADS/Feature/Foo-1",
                ".github/workflows/ci.yml",
                ".github/workflows/ci.yml",
                true,
            ),
            (
                "octocat/octo-repo/.github/workflows/ci.yml@main",
                ".github/workflows/ci.yml",
                ".github/workflows/ci.yml",
                true,
            ),
            (
                "octocat/octo-repo/.github/workflows/ci.yml@refs/heads/main",
                ".github/workflows/ci.yml",
                ".github/workflows/ci.yml",
                true,
            ),
            (
                "OWNER/REPO/.GitHub/Workflows/CI.yml@MAIN",
                ".GitHub/Workflows/CI.yml",
                ".github/workflows/ci.yml",
                true,
            ),
            (
                "Owner/Repo/.github/workflows/ci.yml@Refs/Heads/Main",
                ".github/workflows/ci.yml",
                ".GITHUB/WORKFLOWS/CI.YML",
                true,
            ),
            (
                "acme/widgets/.github/workflows/release.yaml@refs/tags/v1.2.3",
                ".github/workflows/release.yaml",
                ".github/workflows/release.yaml",
                true,
            ),
            (
                "acme/widgets/.github/workflows/ci.yml@refs/pull/3/merge",
                ".github/workflows/ci.yml",
                ".github/workflows/ci.yml",
                true,
            ),
            (
                ".github/workflows/ci.yml@0123456789abcdef",
                ".github/workflows/ci.yml",
                ".github/workflows/ci.yml",
                true,
            ),
            (
                "  .github/workflows/ci.yml@main  ",
                ".github/workflows/ci.yml",
                ".github/workflows/ci.yml",
                true,
            ),
            (
                ".github/workflows/ci.yml@main\r",
                ".github/workflows/ci.yml",
                ".github/workflows/ci.yml",
                true,
            ),
            (
                "\u{feff}.github/workflows/ci.yml@main",
                ".github/workflows/ci.yml",
                ".github/workflows/ci.yml",
                true,
            ),
            (
                "owner\\repo\\.github\\workflows\\ci.yml@main",
                ".github/workflows/ci.yml",
                ".github/workflows/ci.yml",
                true,
            ),
            (
                ".github/workflows/ci.yml",
                ".github/workflows/ci.yml",
                ".github/workflows/ci.yml",
                true,
            ),
            (
                "owner/repo/.github/workflows/ci.yml",
                ".github/workflows/ci.yml",
                ".github/workflows/ci.yml",
                true,
            ),
            (
                "my.org/repo.name/.github/workflows/ci.yml@refs/heads/main",
                ".github/workflows/ci.yml",
                ".github/workflows/ci.yml",
                true,
            ),
            (
                "owner/repo/custom.yml@main",
                "owner/repo/custom.yml",
                ".github/workflows/custom.yml",
                false,
            ),
            (
                "extra/owner/repo/.github/workflows/ci.yml@main",
                "extra/owner/repo/.github/workflows/ci.yml",
                ".github/workflows/ci.yml",
                false,
            ),
            (
                "only-repo/.github/workflows/ci.yml@main",
                "only-repo/.github/workflows/ci.yml",
                ".github/workflows/ci.yml",
                false,
            ),
            (
                "owner/repo/.github/workflow/ci.yml@main",
                "owner/repo/.github/workflow/ci.yml",
                ".github/workflows/ci.yml",
                false,
            ),
            ("@main", "", ".github/workflows/ci.yml", false),
            ("", "", ".github/workflows/ci.yml", false),
            (
                ".github/workflows/ci.yml@main",
                ".github/workflows/ci.yml",
                "",
                false,
            ),
        ];
        for (raw, stored, csv_path, matches) in cases {
            assert_links(raw, stored, csv_path, matches);
        }
    }

    #[test]
    fn read_dir_links_main_refs_heads_and_owner_repo_prefixes() {
        let dir = scratch("link-forms");
        write_json(
            &dir,
            "runs-1.json",
            &runs_body(vec![
                run_value(1, ".github/workflows/build.yml@main"),
                run_value(2, ".github/workflows/ci.yml@refs/heads/main"),
                run_value(3, "octocat/octo-repo/.github/workflows/lint.yml@main"),
                run_value(
                    4,
                    "octocat/octo-repo/.github/workflows/release.yml@refs/heads/feature/foo",
                ),
            ]),
        );
        let data = read_dir(&dir).unwrap();
        let path = |id: u64| {
            data.runs
                .iter()
                .find(|run| run.id == id)
                .unwrap()
                .path
                .clone()
        };
        assert_eq!(path(1), ".github/workflows/build.yml");
        assert_eq!(path(2), ".github/workflows/ci.yml");
        assert_eq!(path(3), ".github/workflows/lint.yml");
        assert_eq!(path(4), ".github/workflows/release.yml");
        assert!(data
            .runs
            .iter()
            .any(|run| run.id == 1 && run.links_to(".github/workflows/build.yml")));
        assert!(data
            .runs
            .iter()
            .any(|run| { run.id == 2 && run.links_to(".github/workflows/ci.yml") }));
        assert!(data
            .runs
            .iter()
            .any(|run| { run.id == 3 && run.links_to(".GitHub/Workflows/Lint.yml") }));
        assert!(data
            .runs
            .iter()
            .any(|run| { run.id == 4 && run.links_to(".github/workflows/release.yml") }));
        assert!(!data.runs.iter().any(|run| run.path.contains('@')));
        assert!(!data.runs.iter().any(|run| run.path.contains("octocat")));
        assert_eq!(data.workflow_path(2), ".github/workflows/ci.yml");
        assert_eq!(data.workflow_path(3), ".github/workflows/lint.yml");
    }

    #[test]
    fn linked_paths_accept_crlf_reordered_keys_empty_fields_and_case() {
        let dir = scratch("link-variants");
        let crlf = "{\r\n\"total_count\": 2,\r\n\"workflow_runs\": [\r\n{\r\n\"run_number\": 8,\r\n\"event\": \"push\",\r\n\"conclusion\": null,\r\n\"id\": 8,\r\n\"run_started_at\": \"2026-04-01T00:00:00Z\",\r\n\"run_attempt\": 1,\r\n\"path\": \"OWNER/REPO/.GitHub/Workflows/CI.yml@MAIN\"\r\n},\r\n{\r\n\"path\": \"\",\r\n\"id\": 9,\r\n\"run_attempt\": 2,\r\n\"run_number\": 9,\r\n\"event\": \"pull_request\"\r\n}\r\n]\r\n}\r\n";
        fs::write(dir.join("runs-1.json"), crlf).unwrap();
        let reversed = r#"{"workflow_runs":[{"run_started_at":"2026-05-01T01:02:03Z","conclusion":"success","event":"workflow_dispatch","run_number":10,"run_attempt":3,"path":".github/workflows/docs.yml@refs/heads/main","id":10,"html_url":"https://example.invalid/hidden"}]}"#;
        fs::write(dir.join("runs-2.json"), reversed).unwrap();

        let data = read_dir(&dir).unwrap();
        let by_id = |id: u64| data.runs.iter().find(|run| run.id == id).unwrap();
        assert_eq!(by_id(8).path, ".GitHub/Workflows/CI.yml");
        assert!(by_id(8).links_to(".github/workflows/ci.yml"));
        assert!(by_id(8).conclusion.is_none());
        assert_eq!(by_id(8).run_attempt, 1);
        assert_eq!(by_id(9).path, "");
        assert!(!by_id(9).links_to(".github/workflows/ci.yml"));
        assert!(by_id(9).conclusion.is_none());
        assert!(by_id(9).run_started_at.is_none());
        assert_eq!(by_id(10).path, ".github/workflows/docs.yml");
        assert!(by_id(10).links_to(".github/workflows/docs.yml"));
        assert_eq!(by_id(10).event, "workflow_dispatch");
        assert!(!format!("{data:?}").contains("example.invalid"));
    }

    #[test]
    fn duplicate_job_id_counts_once() {
        let dir = scratch("duplicate-jobs-order");
        write_json(
            &dir,
            "jobs-7-b.json",
            &jobs_body(vec![job_value(70, 7, "from-later-file")]),
        );
        write_json(
            &dir,
            "jobs-7-a.json",
            &jobs_body(vec![job_value(70, 7, "from-earlier-file")]),
        );
        write_json(
            &dir,
            "jobs-7.json",
            &jobs_body(vec![
                job_value(71, 7, "first-in-array"),
                job_value(71, 7, "second-in-array"),
            ]),
        );
        let data = read_dir(&dir).unwrap();
        assert_eq!(data.jobs.iter().filter(|job| job.id == 70).count(), 1);
        assert_eq!(data.jobs.iter().filter(|job| job.id == 71).count(), 1);
        assert_eq!(
            data.jobs.iter().find(|job| job.id == 70).unwrap().name,
            "from-earlier-file"
        );
        assert_eq!(
            data.jobs.iter().find(|job| job.id == 71).unwrap().name,
            "first-in-array"
        );
        assert_eq!(data.jobs.len(), 2);

        let dir = scratch("duplicate-jobs-fields");
        let mut first = job_value(70, 7, "kept");
        first.as_object_mut().unwrap().remove("started_at");
        first["labels"] = serde_json::json!(["ubuntu-latest"]);
        let mut second = job_value(70, 7, "dropped");
        second["labels"] = serde_json::json!(["windows-latest"]);
        second["started_at"] = serde_json::json!("2026-04-01T00:00:00Z");
        let crlf_first = serde_json::to_string(&jobs_body(vec![first]))
            .unwrap()
            .replace('\n', "\r\n");
        fs::write(dir.join("jobs-7-1.json"), crlf_first).unwrap();
        let reversed = format!(
            "{{\"jobs\":[{}]}}",
            r#"{"runner_name":"other","conclusion":"failure","completed_at":"2026-04-01T00:02:00Z","labels":["windows-latest"],"started_at":"2026-04-01T00:00:00Z","name":"dropped","run_attempt":1,"run_id":7,"id":70}"#
        );
        fs::write(dir.join("jobs-7-2.json"), reversed).unwrap();
        let data = read_dir(&dir).unwrap();
        assert_eq!(data.jobs.len(), 1);
        let kept = &data.jobs[0];
        assert_eq!(kept.name, "kept");
        assert!(kept.started_at.is_none());
        assert_eq!(kept.labels, vec!["ubuntu-latest".to_string()]);
        assert_eq!(data.billable_jobs().count(), 1);

        let dir = scratch("duplicate-still-checks-run-id");
        write_json(
            &dir,
            "jobs-15.json",
            &jobs_body(vec![job_value(5, 15, "build")]),
        );
        write_json(
            &dir,
            "jobs-16.json",
            &jobs_body(vec![job_value(5, 99, "other")]),
        );
        let err = read_dir(&dir).unwrap_err();
        assert_eq!(err.to_string(), "run id does not match file name");
        assert_eq!(err.path, dir.join("jobs-16.json"));
    }

    #[test]
    fn attempt_endpoint_files_are_not_mixed_with_filter_all() {
        let dir = scratch("attempt-files");
        write_json(
            &dir,
            "jobs-9.json",
            &jobs_body(vec![job_value(1, 9, "filter-all")]),
        );
        write_json(
            &dir,
            "jobs-9-2.json",
            &jobs_body(vec![job_value(2, 9, "page-two")]),
        );
        fs::write(dir.join("jobs-9-attempts-1.json"), b"{").unwrap();
        fs::write(dir.join("jobs-9-attempt-2.json"), b"null").unwrap();
        fs::write(
            dir.join("jobs-9-ATTEMPTS-3.json"),
            br#"{"jobs":[{"id":3}]}"#,
        )
        .unwrap();
        let nested = dir.join("attempts").join("1");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("jobs-9.json"), b"{").unwrap();
        let upper = dir.join("Attempts").join("2");
        fs::create_dir_all(&upper).unwrap();
        fs::write(
            upper.join("jobs-9.json"),
            serde_json::to_vec(&jobs_body(vec![job_value(4, 9, "attempt-only")])).unwrap(),
        )
        .unwrap();

        let data = read_dir(&dir).unwrap();
        let ids: Vec<u64> = data.jobs.iter().map(|job| job.id).collect();
        assert_eq!(ids, vec![1, 2]);
        assert_eq!(data.jobs[0].name, "filter-all");
        assert_eq!(data.jobs[1].name, "page-two");
        assert!(!is_filter_all_jobs_name("jobs-9-attempts-1.json"));
        assert!(!is_filter_all_jobs_name("jobs-9-attempt-2.json"));
        assert!(!is_filter_all_jobs_name("jobs-9-ATTEMPTS-3.json"));
        assert!(is_filter_all_jobs_name("jobs-9.json"));
        assert!(is_filter_all_jobs_name("jobs-9-2.json"));
        assert!(is_filter_all_jobs_name("jobs-15_page3.json"));
    }

    fn stamp_job(
        id: u64,
        run_id: u64,
        attempt: u64,
        name: &str,
        started: Option<&str>,
        completed: Option<&str>,
    ) -> Value {
        let mut job = job_value(id, run_id, name);
        job["run_attempt"] = serde_json::json!(attempt);
        match started {
            Some(text) => job["started_at"] = serde_json::json!(text),
            None => {
                job.as_object_mut().unwrap().remove("started_at");
            }
        }
        match completed {
            Some(text) => job["completed_at"] = serde_json::json!(text),
            None => job["completed_at"] = Value::Null,
        }
        job
    }

    fn run_with_start(id: u64, path: &str, attempt: u64, started: Option<&str>) -> Value {
        let mut run = run_value(id, path);
        run["run_attempt"] = serde_json::json!(attempt);
        match started {
            Some(text) => run["run_started_at"] = serde_json::json!(text),
            None => {
                run.as_object_mut().unwrap().remove("run_started_at");
            }
        }
        run
    }

    #[test]
    fn inherited_copy_on_the_last_attempt_costs_zero() {
        let dir = scratch("copy-main");
        write_json(
            &dir,
            "runs-1.json",
            &runs_body(vec![run_with_start(
                9,
                ".github/workflows/ci.yml@refs/heads/main",
                2,
                Some("2026-04-01T12:00:00Z"),
            )]),
        );
        write_json(
            &dir,
            "jobs-9.json",
            &jobs_body(vec![
                stamp_job(
                    1,
                    9,
                    1,
                    "test",
                    Some("2026-04-01T10:00:00Z"),
                    Some("2026-04-01T10:10:00Z"),
                ),
                stamp_job(
                    2,
                    9,
                    2,
                    "test",
                    Some("2026-04-01T10:00:00Z"),
                    Some("2026-04-01T10:10:00Z"),
                ),
                stamp_job(
                    3,
                    9,
                    2,
                    "lint",
                    Some("2026-04-01T12:00:30Z"),
                    Some("2026-04-01T12:02:00Z"),
                ),
                stamp_job(
                    4,
                    9,
                    2,
                    "pack",
                    Some("2026-04-01T12:00:00Z"),
                    Some("2026-04-01T12:01:00Z"),
                ),
                stamp_job(5, 9, 2, "queue", Some("2026-04-01T11:00:00Z"), None),
            ]),
        );
        let data = read_dir(&dir).unwrap();
        assert!(data.runs[0].links_to(".github/workflows/ci.yml"));
        let copy = data.jobs.iter().find(|job| job.id == 2).unwrap();
        assert!(data.is_inherited_copy(copy));
        assert_eq!(
            data.inherited_copy_net(copy).unwrap().to_decimal_string(),
            "0.000000"
        );
        assert_eq!(
            data.inherited_copy_net(copy).unwrap().format_cents(),
            "0.00"
        );
        assert!(copy.counts());
        let queued = data.jobs.iter().find(|job| job.id == 5).unwrap();
        assert!(data.is_inherited_copy(queued));
        assert!(!queued.counts());
        assert_eq!(
            data.inherited_copy_net(queued).unwrap().to_decimal_string(),
            "0.000000"
        );
        for id in [1, 3, 4] {
            let job = data.jobs.iter().find(|job| job.id == id).unwrap();
            assert!(!data.is_inherited_copy(job), "{id}");
            assert!(data.inherited_copy_net(job).is_none(), "{id}");
        }
        let billable: Vec<u64> = data.billable_jobs().map(|job| job.id).collect();
        assert_eq!(billable, vec![3, 4, 1]);
        assert_eq!(data.jobs.iter().filter(|job| job.id == 2).count(), 1);

        let cases = [
            ("absent-started", None, Some("2026-04-01T12:00:00Z"), false),
            (
                "empty-started",
                Some(""),
                Some("2026-04-01T12:00:00Z"),
                false,
            ),
            (
                "blank-started",
                Some("   "),
                Some("2026-04-01T12:00:00Z"),
                false,
            ),
            (
                "bad-started",
                Some("not-a-time"),
                Some("2026-04-01T12:00:00Z"),
                false,
            ),
            (
                "equal-fraction",
                Some("2026-04-01T12:00:00.000Z"),
                Some("2026-04-01T12:00:00Z"),
                false,
            ),
            (
                "equal-offset",
                Some("2026-04-01T14:00:00+02:00"),
                Some("2026-04-01T12:00:00Z"),
                false,
            ),
            (
                "earlier-offset",
                Some("2026-04-01T13:59:59+02:00"),
                Some("2026-04-01T12:00:00Z"),
                true,
            ),
            (
                "earlier-fraction",
                Some("2026-04-01T11:59:59.999Z"),
                Some("2026-04-01T12:00:00.000Z"),
                true,
            ),
        ];
        for (name, started, run_started, is_copy) in cases {
            let dir = scratch(&format!("copy-{name}"));
            write_json(
                &dir,
                "runs-1.json",
                &runs_body(vec![run_with_start(
                    1,
                    "OWNER/REPO/.github/workflows/ci.yml@REFS/HEADS/Main",
                    2,
                    run_started,
                )]),
            );
            let job = stamp_job(20, 1, 2, "test", started, Some("2026-04-01T12:30:00Z"));
            write_json(&dir, "jobs-1.json", &jobs_body(vec![job]));
            let data = read_dir(&dir).unwrap();
            assert!(data.runs[0].links_to(".github/workflows/ci.yml"), "{name}");
            let job = &data.jobs[0];
            assert_eq!(data.is_inherited_copy(job), is_copy, "{name}");
            if is_copy {
                assert_eq!(data.billable_jobs().count(), 0, "{name}");
                assert_eq!(
                    data.inherited_copy_net(job).unwrap().to_decimal_string(),
                    "0.000000",
                    "{name}"
                );
            } else {
                assert_eq!(data.billable_jobs().count(), 1, "{name}");
                assert!(data.inherited_copy_net(job).is_none(), "{name}");
            }
        }

        let dir = scratch("copy-null-started");
        write_json(
            &dir,
            "runs-1.json",
            &runs_body(vec![run_with_start(
                1,
                ".github/workflows/ci.yml@main",
                2,
                Some("2026-04-01T12:00:00Z"),
            )]),
        );
        let mut job = stamp_job(
            21,
            1,
            2,
            "test",
            Some("2026-04-01T10:00:00Z"),
            Some("2026-04-01T10:05:00Z"),
        );
        job["started_at"] = Value::Null;
        write_json(&dir, "jobs-1.json", &jobs_body(vec![job]));
        let data = read_dir(&dir).unwrap();
        assert!(!data.is_inherited_copy(&data.jobs[0]));
        assert_eq!(data.billable_jobs().count(), 1);

        let dir = scratch("copy-missing-run-start");
        let mut run = run_with_start(1, ".github/workflows/ci.yml@main", 2, None);
        run["run_started_at"] = Value::Null;
        write_json(&dir, "runs-a.json", &runs_body(vec![run.clone()]));
        write_json(
            &dir,
            "jobs-1.json",
            &jobs_body(vec![stamp_job(
                22,
                1,
                2,
                "test",
                Some("2026-04-01T10:00:00Z"),
                Some("2026-04-01T10:05:00Z"),
            )]),
        );
        let data = read_dir(&dir).unwrap();
        assert!(!data.is_inherited_copy(&data.jobs[0]));

        let dir = scratch("copy-empty-run-start");
        write_json(
            &dir,
            "runs-1.json",
            &runs_body(vec![run_with_start(
                1,
                ".github/workflows/ci.yml@main",
                2,
                Some(""),
            )]),
        );
        write_json(
            &dir,
            "jobs-1.json",
            &jobs_body(vec![stamp_job(
                23,
                1,
                2,
                "test",
                Some("2026-04-01T10:00:00Z"),
                Some("2026-04-01T10:05:00Z"),
            )]),
        );
        let data = read_dir(&dir).unwrap();
        assert!(!data.is_inherited_copy(&data.jobs[0]));
        assert_eq!(data.billable_jobs().count(), 1);

        let dir = scratch("copy-absent-run-start");
        write_json(
            &dir,
            "runs-1.json",
            &runs_body(vec![run_with_start(
                1,
                ".github/workflows/ci.yml@main",
                2,
                None,
            )]),
        );
        write_json(
            &dir,
            "jobs-1.json",
            &jobs_body(vec![stamp_job(
                24,
                1,
                2,
                "test",
                Some("2026-04-01T10:00:00Z"),
                Some("2026-04-01T10:05:00Z"),
            )]),
        );
        let data = read_dir(&dir).unwrap();
        assert!(data.runs[0].run_started_at.is_none());
        assert!(!data.is_inherited_copy(&data.jobs[0]));

        let dir = scratch("copy-other-attempt");
        write_json(
            &dir,
            "runs-1.json",
            &runs_body(vec![run_with_start(
                1,
                ".github/workflows/ci.yml@main",
                2,
                Some("2026-04-01T12:00:00Z"),
            )]),
        );
        write_json(
            &dir,
            "jobs-1.json",
            &jobs_body(vec![stamp_job(
                25,
                1,
                3,
                "test",
                Some("2026-04-01T10:00:00Z"),
                Some("2026-04-01T10:05:00Z"),
            )]),
        );
        let data = read_dir(&dir).unwrap();
        assert!(!data.is_inherited_copy(&data.jobs[0]));
        assert_eq!(data.billable_jobs().count(), 1);

        let dir = scratch("copy-no-run");
        write_json(
            &dir,
            "jobs-4.json",
            &jobs_body(vec![stamp_job(
                26,
                4,
                1,
                "orphan",
                Some("2026-04-01T10:00:00Z"),
                Some("2026-04-01T10:05:00Z"),
            )]),
        );
        let data = read_dir(&dir).unwrap();
        assert!(!data.is_inherited_copy(&data.jobs[0]));
        assert_eq!(data.workflow_path(4), UNKNOWN_WORKFLOW);
        assert_eq!(data.billable_jobs().count(), 1);

        let dir = scratch("copy-duplicate-and-crlf");
        write_json(
            &dir,
            "runs-1.json",
            &runs_body(vec![run_with_start(
                3,
                ".github/workflows/ci.yml@main",
                2,
                Some("2026-04-01T12:00:00Z"),
            )]),
        );
        let copy = stamp_job(
            30,
            3,
            2,
            "test",
            Some("2026-04-01T10:00:00Z"),
            Some("2026-04-01T11:00:00Z"),
        );
        let pretty = serde_json::to_vec_pretty(&jobs_body(vec![copy])).unwrap();
        let mut crlf = Vec::with_capacity(pretty.len() + 16);
        for byte in pretty {
            if byte == b'\n' {
                crlf.push(b'\r');
            }
            crlf.push(byte);
        }
        fs::write(dir.join("jobs-3-1.json"), crlf).unwrap();
        fs::write(
            dir.join("jobs-3-2.json"),
            format!(
                "{{\"total_count\":1,\"jobs\":[{}]}}",
                r#"{"completed_at":"2026-04-01T12:06:00Z","started_at":"2026-04-01T12:05:00Z","name":"renamed","labels":[],"conclusion":"success","runner_name":"runner-a","run_attempt":2,"run_id":3,"id":30}"#
            ),
        )
        .unwrap();
        let data = read_dir(&dir).unwrap();
        assert_eq!(data.jobs.len(), 1);
        assert_eq!(data.jobs[0].name, "test");
        assert!(data.is_inherited_copy(&data.jobs[0]));
        assert_eq!(data.billable_jobs().count(), 0);
        assert_eq!(
            data.inherited_copy_net(&data.jobs[0])
                .unwrap()
                .to_decimal_string(),
            "0.000000"
        );
    }

    #[test]
    fn github_timestamps_order_by_utc_instant() {
        assert_eq!(parse_github_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_github_time("1970-01-01T00:00:01Z"),
            Some(1_000_000_000)
        );
        assert_eq!(
            parse_github_time("2026-03-01T10:00:00Z"),
            parse_github_time("2026-03-01T10:00:00.000Z")
        );
        assert_eq!(
            parse_github_time("2026-03-01T10:00:00Z"),
            parse_github_time("2026-03-01T12:00:00+02:00")
        );
        assert_eq!(
            parse_github_time("2026-03-01T10:00:00Z"),
            parse_github_time("2026-03-01T10:00:00+0000")
        );
        assert_eq!(
            parse_github_time("2026-03-01T10:00:00Z"),
            parse_github_time("2026-03-01T11:00:00+01")
        );
        assert!(
            parse_github_time("2026-03-01T09:59:59Z") < parse_github_time("2026-03-01T10:00:00Z")
        );
        assert!(parse_github_time("2024-02-29T00:00:00Z").is_some());
        for bad in [
            "",
            "   ",
            "2026-13-01T00:00:00Z",
            "2026-02-29T00:00:00Z",
            "2025-02-29T00:00:00Z",
            "2026-03-01T24:00:00Z",
            "2026-03-01T10:00:00",
            "2026-03-01T10:00:00Zextra",
            "yesterday",
            "2026-03-01T10:00:60Z",
            "2026-03-01t10:00:00",
        ] {
            assert_eq!(parse_github_time(bad), None, "{bad}");
        }
        assert!(parse_github_time("2026-03-01t10:00:00Z").is_some());
        assert!(parse_github_time("  2026-03-01T10:00:00Z\r").is_some());
    }
}
