//! Saved GitHub Actions API responses.
//!
//! `--runs DIR` reads `runs-*.json` (a `GET /repos/{owner}/{repo}/actions/runs`
//! body) and `jobs-<run_id>*.json` (a
//! `GET /repos/{owner}/{repo}/actions/runs/{run_id}/jobs?filter=all` body).
//! Several files are pages of one list. Only files directly in the directory
//! are read.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

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

    /// Workflow path for `run_id`, or [`UNKNOWN_WORKFLOW`].
    pub fn workflow_path(&self, run_id: u64) -> &str {
        self.runs
            .iter()
            .find(|run| run.id == run_id)
            .map(|run| run.path.as_str())
            .unwrap_or(UNKNOWN_WORKFLOW)
    }
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

/// Read every runs page and jobs page in `dir`.
///
/// Names are sorted, then pages are merged. A later file that repeats a run
/// id or a job id is an error. Each job's `run_id` must match the
/// `jobs-<run_id>` prefix of its file (`jobs-15-2.json` is run 15,
/// `jobs-150.json` is run 150).
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
                if !seen_jobs.insert(job.id) {
                    return Err(ApiError::duplicate(&path));
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
        if !is_runs_file(name) && job_file_run_id(name).is_none() {
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
        path: require_string(object, "path")?,
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

        let dir = scratch("duplicate-jobs");
        write_json(
            &dir,
            "jobs-7-1.json",
            &jobs_body(vec![job_value(70, 7, "build")]),
        );
        write_json(
            &dir,
            "jobs-7-2.json",
            &jobs_body(vec![job_value(70, 7, "test")]),
        );
        let err = read_dir(&dir).unwrap_err();
        assert_eq!(err.to_string(), "duplicate id");
        assert_eq!(err.path, dir.join("jobs-7-2.json"));
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
}
