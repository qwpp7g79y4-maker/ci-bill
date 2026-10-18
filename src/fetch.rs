//! Download Actions runs, jobs, and referenced reusable workflows.
//!
//! `fetch` reads `GH_TOKEN` or `GITHUB_TOKEN` and writes the cache that
//! `--runs` already reads. The token is removed from saved bodies and from
//! error text.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::api::{self, is_runs_file, job_file_run_id};
use crate::workflow::{scan_yaml, RemoteWorkflow, ReusableSource};

const ACCEPT_JSON: &str = "application/vnd.github+json";
const ACCEPT_RAW: &str = "application/vnd.github.raw";
const MAX_BODY: u64 = 32 * 1024 * 1024;
const MAX_PAGES: usize = 10_000;
const ATTEMPTS: u32 = 5;

/// Failure from `fetch`. `code` is the process exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchError {
    message: String,
    code: u8,
}

impl FetchError {
    pub fn code(&self) -> u8 {
        self.code
    }

    fn usage(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: 3,
        }
    }

    fn input(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: 2,
        }
    }
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FetchError {}

/// A UTC calendar day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UtcDate {
    year: i32,
    month: u32,
    day: u32,
}

impl UtcDate {
    fn unix(self) -> u64 {
        days_from_civil(self.year, self.month, self.day) as u64 * 86400
    }

    /// Start of the next UTC day.
    fn next_unix(self) -> u64 {
        self.unix().saturating_add(86400)
    }

    fn text(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    fn from_unix(unix: u64) -> Self {
        let (year, month, day) = civil_from_days((unix / 86400) as i64);
        Self { year, month, day }
    }
}

/// `OWNER/REPO` for `--repo`.
pub(crate) fn parse_repo_arg(text: &str) -> Result<String, String> {
    let (owner, repo) = split_repo(text)?;
    Ok(format!("{owner}/{repo}"))
}

/// `YYYY-MM-DD` for `--since` and `--until`.
pub(crate) fn parse_utc_date(text: &str) -> Result<UtcDate, String> {
    parse_date(text).ok_or_else(|| "expected a UTC date YYYY-MM-DD".to_string())
}

/// Fetch one repository into `out`.
///
/// The return value is the human-readable summary, including its trailing newline.
pub fn fetch(
    repo: &str,
    out: &Path,
    since: UtcDate,
    until: Option<UtcDate>,
) -> Result<String, FetchError> {
    let (owner, name) = split_repo(repo).map_err(FetchError::usage)?;
    let until = until.unwrap_or_else(today_utc);
    if since.unix() > until.unix() {
        return Err(FetchError::usage("--until is before --since"));
    }
    let secrets = secrets_from_env()?;
    let base = api_base().map_err(|message| redact_error(message, &secrets.needles))?;
    if out.exists() && !out.is_dir() {
        return Err(path_error(out, "cannot read directory"));
    }
    let cache = if out.is_dir() {
        load_cache(out)?
    } else {
        Cache::default()
    };
    let client = Client::open(&base, &owner, &name, &secrets)?;
    let window = Window {
        start: since.unix(),
        end: until.next_unix(),
    };
    let repo_body = client.get_json(&client.repo_url(), "repository not found")?;
    let default_branch = default_branch(&repo_body)?;
    let run_pages = client.fetch_run_pages(window)?;
    let incoming = incoming_runs(&run_pages)?;
    let merged = merge_runs(&cache, &incoming);
    let job_pages = client.fetch_needed_jobs(&cache, &merged.jobs)?;
    let files = client.workflow_files(out, &merged.discovered, &default_branch)?;
    commit(
        out,
        &repo_body,
        &RunWrite {
            cache: &cache,
            pages: &run_pages,
            merged: &merged.runs,
            rewrite_runs: merged.rewrite_runs,
        },
        &job_pages,
        &files,
    )?;
    let jobs = job_pages.iter().map(|page| page.jobs).sum::<usize>();
    Ok(format!(
        "{owner}/{name} {} .. {}\nruns {}\njobs {jobs}\nworkflow files {}\n",
        since.text(),
        until.text(),
        merged.run_count,
        files.len()
    ))
}

fn redact_error(error: FetchError, needles: &[String]) -> FetchError {
    FetchError {
        message: redact_all_str(&error.message, needles),
        code: error.code,
    }
}

fn today_utc() -> UtcDate {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    UtcDate::from_unix(secs)
}

struct Secrets {
    /// Value sent in `Authorization`. `GH_TOKEN` wins when both are set.
    auth: String,
    /// Every configured token, longest first, removed from saved bytes and errors.
    needles: Vec<String>,
}

fn secrets_from_env() -> Result<Secrets, FetchError> {
    let mut auth = None;
    let mut needles = Vec::new();
    for name in ["GH_TOKEN", "GITHUB_TOKEN"] {
        let Ok(value) = std::env::var(name) else {
            continue;
        };
        let token = value.trim();
        if token.is_empty() {
            continue;
        }
        if token
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        {
            return Err(FetchError::usage("set GH_TOKEN or GITHUB_TOKEN"));
        }
        if auth.is_none() {
            auth = Some(token.to_string());
        }
        if !needles.iter().any(|item| item == token) {
            needles.push(token.to_string());
        }
    }
    let Some(auth) = auth else {
        return Err(FetchError::usage("set GH_TOKEN or GITHUB_TOKEN"));
    };
    needles.sort_by_key(|item| std::cmp::Reverse(item.len()));
    Ok(Secrets { auth, needles })
}

fn api_base() -> Result<String, FetchError> {
    match std::env::var("GITHUB_API_URL") {
        Ok(value) => {
            let base = value.trim().trim_end_matches('/').to_string();
            if origin_of(&base).is_some()
                && (base.starts_with("https://") || base.starts_with("http://"))
            {
                Ok(base)
            } else {
                Err(FetchError::usage(
                    "GITHUB_API_URL must be an http or https URL",
                ))
            }
        }
        Err(_) => Ok("https://api.github.com".to_string()),
    }
}

fn split_repo(text: &str) -> Result<(String, String), String> {
    let text = text.trim();
    let Some((owner, repo)) = text.split_once('/') else {
        return Err("expected OWNER/REPO".to_string());
    };
    if owner.contains('/') || repo.contains('/') || !valid_name(owner) || !valid_name(repo) {
        return Err("expected OWNER/REPO".to_string());
    }
    Ok((owner.to_string(), repo.to_string()))
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    let mut last = first;
    for character in chars {
        if !(character.is_ascii_alphanumeric()
            || character == '.'
            || character == '_'
            || character == '-')
        {
            return false;
        }
        last = character;
    }
    last.is_ascii_alphanumeric()
}

fn parse_date(text: &str) -> Option<UtcDate> {
    let text = text.trim();
    if text.len() != 10 {
        return None;
    }
    let bytes = text.as_bytes();
    if bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year = i32::try_from(parse_digits(&bytes[0..4])?).ok()?;
    let month = parse_digits(&bytes[5..7])?;
    let day = parse_digits(&bytes[8..10])?;
    if !valid_ymd(year, month, day) {
        return None;
    }
    Some(UtcDate { year, month, day })
}

fn parse_digits(bytes: &[u8]) -> Option<u32> {
    let mut value = 0u32;
    for byte in bytes {
        let digit = byte.wrapping_sub(b'0');
        if digit > 9 {
            return None;
        }
        value = value * 10 + u32::from(digit);
    }
    Some(value)
}

fn valid_ymd(year: i32, month: u32, day: u32) -> bool {
    if !(1970..=9999).contains(&year) || day == 0 {
        return false;
    }
    let max = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap(year) => 29,
        2 => 28,
        _ => return false,
    };
    day <= max
}

fn leap(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Window {
    /// Inclusive unix second.
    start: u64,
    /// Exclusive unix second.
    end: u64,
}

impl Window {
    /// Half-open halves, so a run on the boundary is requested once.
    fn split(self) -> Option<(Window, Window)> {
        let span = self.end.saturating_sub(self.start);
        if span <= 1 {
            return None;
        }
        let mid = self.start + span / 2;
        if mid <= self.start || mid >= self.end {
            return None;
        }
        Some((
            Window {
                start: self.start,
                end: mid,
            },
            Window {
                start: mid,
                end: self.end,
            },
        ))
    }

    fn created_param(self) -> String {
        if self.start % 86400 == 0 && self.end % 86400 == 0 && self.end > self.start {
            let start_day = format_day(self.start);
            let last_day = format_day(self.end - 86400);
            format!("{start_day}..{last_day}")
        } else {
            let start = format_rfc3339(self.start);
            let last = format_rfc3339(self.end.saturating_sub(1));
            format!("{start}..{last}")
        }
    }
}

fn format_day(unix: u64) -> String {
    UtcDate::from_unix(unix).text()
}

fn format_rfc3339(unix: u64) -> String {
    let day = format_day(unix);
    let secs = unix % 86400;
    let hour = secs / 3600;
    let minute = (secs % 3600) / 60;
    let second = secs % 60;
    format!("{day}T{hour:02}:{minute:02}:{second:02}Z")
}

struct Client {
    agent: ureq::Agent,
    base: String,
    owner: String,
    repo: String,
    token: String,
    secrets: Vec<String>,
}

impl Client {
    fn open(base: &str, owner: &str, repo: &str, secrets: &Secrets) -> Result<Self, FetchError> {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(60)))
            .http_status_as_error(false)
            .https_only(false)
            .max_redirects(0)
            .build();
        let agent: ureq::Agent = config.into();
        Ok(Self {
            agent,
            base: base.to_string(),
            owner: owner.to_string(),
            repo: repo.to_string(),
            token: secrets.auth.clone(),
            secrets: secrets.needles.clone(),
        })
    }

    fn redact_owned(&self, bytes: &[u8]) -> Vec<u8> {
        let mut current = bytes.to_vec();
        for secret in &self.secrets {
            current = redact_bytes(&current, secret);
        }
        current
    }

    fn redact_text(&self, text: &str) -> String {
        String::from_utf8(self.redact_owned(text.as_bytes()))
            .unwrap_or_else(|_| "invalid response".to_string())
    }

    fn repo_url(&self) -> String {
        format!(
            "{}/repos/{}/{}",
            self.base,
            encode_component(&self.owner),
            encode_component(&self.repo)
        )
    }

    fn runs_url(&self, window: Window) -> String {
        format!(
            "{}/repos/{}/{}/actions/runs?per_page=100&created={}",
            self.base,
            encode_component(&self.owner),
            encode_component(&self.repo),
            encode_component(&window.created_param())
        )
    }

    fn jobs_url(&self, id: u64) -> String {
        format!(
            "{}/repos/{}/{}/actions/runs/{id}/jobs?filter=all&per_page=100",
            self.base,
            encode_component(&self.owner),
            encode_component(&self.repo)
        )
    }

    fn contents_url(&self, remote: &RemoteWorkflow) -> String {
        format!(
            "{}/repos/{}/{}/contents/{}?ref={}",
            self.base,
            encode_component(&remote.owner),
            encode_component(&remote.repo),
            encode_path(&remote.workflow_path),
            encode_component(&remote.reference)
        )
    }

    fn get_json(&self, url: &str, missing: &str) -> Result<Vec<u8>, FetchError> {
        let wire = self.get(url, ACCEPT_JSON)?;
        if wire.status == 200 {
            return Ok(wire.body);
        }
        if wire.status == 404 {
            return Err(FetchError::input(missing));
        }
        Err(FetchError::input(format!(
            "the GitHub API returned HTTP {}",
            wire.status
        )))
    }

    fn get_file(&self, url: &str) -> Result<Option<Vec<u8>>, FetchError> {
        let wire = self.get(url, ACCEPT_RAW)?;
        if wire.status == 404 {
            return Ok(None);
        }
        if wire.status != 200 {
            return Err(FetchError::input(format!(
                "the GitHub API returned HTTP {}",
                wire.status
            )));
        }
        self.decode_contents(&wire)
    }

    fn decode_contents(&self, wire: &Wire) -> Result<Option<Vec<u8>>, FetchError> {
        match contents_bytes(&wire.content_type, &wire.body)? {
            Contents::Bytes(bytes) => Ok(Some(self.redact_owned(&bytes))),
            Contents::Download(url) => {
                let next = self.get(&url, ACCEPT_RAW)?;
                if next.status == 404 {
                    return Ok(None);
                }
                if next.status != 200 {
                    return Err(FetchError::input(format!(
                        "the GitHub API returned HTTP {}",
                        next.status
                    )));
                }
                match contents_bytes(&next.content_type, &next.body)? {
                    Contents::Bytes(bytes) => Ok(Some(self.redact_owned(&bytes))),
                    Contents::Download(_) => Err(invalid_contents()),
                }
            }
        }
    }

    fn fetch_run_pages(&self, window: Window) -> Result<Vec<RunPage>, FetchError> {
        let mut seen = HashSet::new();
        self.fetch_window(window, &mut seen)
    }

    fn fetch_window(
        &self,
        window: Window,
        seen: &mut HashSet<(u64, u64)>,
    ) -> Result<Vec<RunPage>, FetchError> {
        if window.end <= window.start || !seen.insert((window.start, window.end)) {
            return Ok(Vec::new());
        }
        let url = self.runs_url(window);
        let first = self.get_json_wire(&url)?;
        let page = parse_runs_page(&first, &url)?;
        if page_too_wide(&page) {
            if let Some((left, right)) = window.split() {
                let mut pages = self.fetch_window(left, seen)?;
                pages.extend(self.fetch_window(right, seen)?);
                return Ok(pages);
            }
        }
        let mut pages = Vec::new();
        let mut count = page.runs.len();
        let mut next = page.next.clone();
        let mut seen_urls = HashSet::new();
        seen_urls.insert(url);
        pages.push(page);
        while let Some(url) = next {
            if pages.len() >= MAX_PAGES || !seen_urls.insert(url.clone()) {
                break;
            }
            let wire = self.get_json_wire(&url)?;
            let page = parse_runs_page(&wire, &url)?;
            count += page.runs.len();
            if count >= 1000 || page.total.unwrap_or(0) >= 1000 {
                if let Some((left, right)) = window.split() {
                    let mut split_pages = self.fetch_window(left, seen)?;
                    split_pages.extend(self.fetch_window(right, seen)?);
                    return Ok(split_pages);
                }
            }
            next = page.next.clone();
            pages.push(page);
        }
        Ok(pages)
    }

    fn get_json_wire(&self, url: &str) -> Result<Wire, FetchError> {
        let wire = self.get(url, ACCEPT_JSON)?;
        if wire.status == 200 {
            return Ok(wire);
        }
        Err(FetchError::input(format!(
            "the GitHub API returned HTTP {}",
            wire.status
        )))
    }

    fn fetch_needed_jobs(
        &self,
        cache: &Cache,
        jobs: &[(u64, u64)],
    ) -> Result<Vec<JobBatch>, FetchError> {
        let mut batches = Vec::new();
        for &(id, attempt) in jobs {
            let stored = cache
                .runs
                .get(&id)
                .map(|run| run.attempt)
                .or_else(|| cache.job_attempt.get(&id).copied());
            if !should_fetch_jobs(stored, cache.has_jobs.contains(&id), attempt) {
                continue;
            }
            batches.push(self.fetch_job_batch(id)?);
        }
        Ok(batches)
    }

    fn fetch_job_batch(&self, id: u64) -> Result<JobBatch, FetchError> {
        let mut url = self.jobs_url(id);
        let mut seen = HashSet::new();
        let mut bodies = Vec::new();
        let mut jobs = 0usize;
        loop {
            if bodies.len() >= MAX_PAGES || !seen.insert(url.clone()) {
                break;
            }
            let wire = self.get_json_wire(&url)?;
            let page = parse_jobs_page(&wire, id, &url)?;
            jobs += page.jobs;
            let next = page.next;
            bodies.push(wire.body);
            let Some(next_url) = next else {
                break;
            };
            url = next_url;
        }
        Ok(JobBatch { id, bodies, jobs })
    }

    fn workflow_files(
        &self,
        out: &Path,
        runs: &[Value],
        default_branch: &str,
    ) -> Result<Vec<(PathBuf, Vec<u8>)>, FetchError> {
        let mut queue = VecDeque::new();
        let mut seen = HashSet::new();
        for run in runs {
            if let Some(remote) = remote_from_run(run, &self.owner, &self.repo, default_branch) {
                enqueue(&mut queue, &mut seen, remote);
            }
        }
        let mut files = Vec::new();
        while let Some(remote) = queue.pop_front() {
            let Some(bytes) = self.get_file(&self.contents_url(&remote))? else {
                continue;
            };
            if let Some(path) = safe_cache_path(out, &remote) {
                files.push((path, bytes.clone()));
            }
            if let Ok(text) = std::str::from_utf8(&bytes) {
                if let Ok(workflow) = scan_yaml(Path::new("workflow.yml"), text) {
                    for job in workflow.jobs {
                        let Some(call) = job.uses else {
                            continue;
                        };
                        match call.source {
                            ReusableSource::Remote(callee) => {
                                enqueue(&mut queue, &mut seen, callee);
                            }
                            ReusableSource::Local => {
                                enqueue(
                                    &mut queue,
                                    &mut seen,
                                    RemoteWorkflow {
                                        owner: remote.owner.clone(),
                                        repo: remote.repo.clone(),
                                        workflow_path: format!(
                                            ".github/workflows/{}",
                                            call.file_name
                                        ),
                                        reference: remote.reference.clone(),
                                    },
                                );
                            }
                        }
                    }
                }
            }
        }
        Ok(files)
    }

    fn get(&self, url: &str, accept: &str) -> Result<Wire, FetchError> {
        let mut current = url.to_string();
        let mut send_auth = same_origin(&self.base, &current);
        let mut seen = HashSet::new();
        for _ in 0..ATTEMPTS {
            if !seen.insert(current.clone()) {
                return Err(FetchError::input("the GitHub API returned HTTP 302"));
            }
            let wire = self.request(&current, accept, send_auth)?;
            if matches!(wire.status, 301 | 302 | 303 | 307 | 308) {
                let loc = wire.location.clone().ok_or_else(|| {
                    FetchError::input(format!("the GitHub API returned HTTP {}", wire.status))
                })?;
                let next = resolve_url(&wire.url, &loc);
                if !(next.starts_with("http://") || next.starts_with("https://")) {
                    return Err(FetchError::input(format!(
                        "the GitHub API returned HTTP {}",
                        wire.status
                    )));
                }
                send_auth = same_origin(&self.base, &next);
                current = next;
                continue;
            }
            return Ok(wire);
        }
        Err(FetchError::input("the GitHub API returned HTTP 302"))
    }

    fn request(&self, url: &str, accept: &str, send_auth: bool) -> Result<Wire, FetchError> {
        let secrets = self.secrets.clone();
        retry(
            || self.request_once(url, accept, send_auth),
            thread::sleep,
            SystemTime::now,
            &secrets,
        )
    }

    fn request_once(&self, url: &str, accept: &str, send_auth: bool) -> Result<Wire, FetchError> {
        let auth = format!("Bearer {}", self.token);
        let mut call = self
            .agent
            .get(url)
            .header("Accept", accept)
            .header("User-Agent", "ci-bill")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("Connection", "close");
        if send_auth {
            call = call.header("Authorization", &auth);
        }
        let mut response = call.call().map_err(|err| self.network(&err))?;
        let status = response.status().as_u16();
        let remaining = header_value(response.headers(), "x-ratelimit-remaining");
        let reset = header_value(response.headers(), "x-ratelimit-reset");
        let retry_after = header_value(response.headers(), "retry-after");
        let location =
            header_value(response.headers(), "location").map(|value| self.redact_text(&value));
        let content_type = header_value(response.headers(), "content-type").unwrap_or_default();
        let link = header_values(response.headers(), "link")
            .into_iter()
            .map(|value| self.redact_text(&value))
            .collect();
        let final_url = response
            .headers()
            .get("x-final-url")
            .and_then(|value| value.to_str().ok())
            .unwrap_or(url)
            .to_string();
        let mut body = read_limited(response.body_mut()).map_err(|err| self.network(&err))?;
        body = self.redact_owned(&body);
        Ok(Wire {
            status,
            remaining,
            reset,
            retry_after,
            location,
            content_type,
            link,
            body,
            url: self.redact_text(&final_url),
        })
    }

    fn network(&self, err: &impl std::fmt::Display) -> FetchError {
        FetchError::input(self.redact_text(&format!("cannot reach the GitHub API: {err}")))
    }
}

#[derive(Debug)]
struct Wire {
    status: u16,
    remaining: Option<String>,
    reset: Option<String>,
    retry_after: Option<String>,
    location: Option<String>,
    content_type: String,
    link: Vec<String>,
    body: Vec<u8>,
    url: String,
}

#[derive(Debug)]
enum Decision {
    Proceed,
    Wait(Duration),
    Unauthorized,
    Permission,
    Stop(u16),
}

fn retry<F, S, N>(
    mut call: F,
    mut sleep: S,
    mut now: N,
    needles: &[String],
) -> Result<Wire, FetchError>
where
    F: FnMut() -> Result<Wire, FetchError>,
    S: FnMut(Duration),
    N: FnMut() -> SystemTime,
{
    for attempt in 1..=ATTEMPTS {
        let wire = call()?;
        match decide(&wire, attempt, now()) {
            Decision::Proceed => return Ok(wire),
            Decision::Wait(delay) => {
                if attempt == ATTEMPTS {
                    return Err(rate_limit(needles));
                }
                sleep(delay);
            }
            Decision::Unauthorized => {
                return Err(FetchError::input(
                    "unauthorized; GH_TOKEN or GITHUB_TOKEN was rejected",
                ));
            }
            Decision::Permission => {
                return Err(FetchError::input(
                    "permission denied; the token needs the actions:read scope, or the repo scope",
                ));
            }
            Decision::Stop(code) => {
                return Err(FetchError::input(format!(
                    "the GitHub API returned HTTP {code}"
                )));
            }
        }
    }
    Err(rate_limit(needles))
}

fn rate_limit(needles: &[String]) -> FetchError {
    FetchError::input(redact_all_str("rate limit; saved files were kept", needles))
}

/// Primary limit (`x-ratelimit-remaining: 0`) waits until `x-ratelimit-reset`.
/// `retry-after` waits that many seconds. A secondary limit with neither header
/// waits 60 seconds and then doubles, for at most five attempts.
fn decide(wire: &Wire, attempt: u32, now: SystemTime) -> Decision {
    if wire.status != 403 && wire.status != 429 {
        if wire.status == 401 {
            return Decision::Unauthorized;
        }
        if (500..600).contains(&wire.status) {
            return Decision::Stop(wire.status);
        }
        return Decision::Proceed;
    }
    if let Some(raw) = wire.retry_after.as_deref() {
        if let Some(delay) = parse_retry_after(raw, now) {
            return Decision::Wait(delay);
        }
    }
    if wire.remaining.as_deref().is_some_and(remaining_is_zero) {
        let delay = match wire.reset.as_deref().and_then(parse_u64_header) {
            Some(reset) => Duration::from_secs(reset.saturating_sub(unix_now(now))),
            None => secondary_delay(attempt),
        };
        return Decision::Wait(delay);
    }
    if wire.status == 429 || contains_secondary(&wire.body) {
        return Decision::Wait(secondary_delay(attempt));
    }
    if wire.status == 403 {
        return Decision::Permission;
    }
    Decision::Wait(secondary_delay(attempt))
}

fn remaining_is_zero(value: &str) -> bool {
    let text = value.trim();
    !text.is_empty() && text.bytes().all(|byte| byte == b'0')
}

fn secondary_delay(attempt: u32) -> Duration {
    let shift = attempt.saturating_sub(1).min(16);
    Duration::from_secs(60u64.saturating_mul(1u64 << shift))
}

fn contains_secondary(body: &[u8]) -> bool {
    const NEEDLE: &[u8] = b"secondary rate limit";
    if body.len() < NEEDLE.len() {
        return false;
    }
    body.windows(NEEDLE.len()).any(|window| {
        window
            .iter()
            .zip(NEEDLE)
            .all(|(byte, needle)| byte.to_ascii_lowercase() == *needle)
    })
}

fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let text = value.trim();
    if let Ok(seconds) = text.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let when = parse_http_date(text)?;
    Some(Duration::from_secs(when.saturating_sub(unix_now(now))))
}

fn parse_u64_header(value: &str) -> Option<u64> {
    value.trim().parse().ok()
}

fn unix_now(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn parse_http_date(text: &str) -> Option<u64> {
    let rest = text
        .trim()
        .split_once(',')
        .map(|(_, rest)| rest.trim())
        .unwrap_or(text.trim());
    let mut parts = rest.split_whitespace();
    let day: u32 = parts.next()?.parse().ok()?;
    let month = month_index(parts.next()?)?;
    let year: i32 = parts.next()?.parse().ok()?;
    let time = parts.next()?;
    let mut clock = time.split(':');
    let hour: u32 = clock.next()?.parse().ok()?;
    let minute: u32 = clock.next()?.parse().ok()?;
    let second: u32 = clock.next()?.parse().ok()?;
    if clock.next().is_some() {
        return None;
    }
    let zone = parts.next()?;
    if !zone.eq_ignore_ascii_case("GMT") && !zone.eq_ignore_ascii_case("UTC") {
        return None;
    }
    if !valid_ymd(year, month, day) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    if days < 0 {
        return None;
    }
    Some(days as u64 * 86400 + u64::from(hour) * 3600 + u64::from(minute) * 60 + u64::from(second))
}

fn month_index(name: &str) -> Option<u32> {
    Some(match name {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    })
}

fn header_value(headers: &ureq::http::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn header_values(headers: &ureq::http::HeaderMap, name: &str) -> Vec<String> {
    headers
        .get_all(name)
        .iter()
        .filter_map(|value| value.to_str().ok().map(str::to_string))
        .collect()
}

fn read_limited(body: &mut ureq::Body) -> Result<Vec<u8>, ureq::Error> {
    let mut reader = body.as_reader().take(MAX_BODY + 1);
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).map_err(ureq::Error::Io)?;
    if buf.len() as u64 > MAX_BODY {
        return Err(ureq::Error::BodyExceedsLimit(MAX_BODY));
    }
    Ok(buf)
}

struct RunPage {
    body: Vec<u8>,
    runs: Vec<Value>,
    total: Option<u64>,
    next: Option<String>,
}

fn page_too_wide(page: &RunPage) -> bool {
    page.total.unwrap_or(0) >= 1000 || page.runs.len() >= 1000
}

fn parse_runs_page(wire: &Wire, request_url: &str) -> Result<RunPage, FetchError> {
    let value = parse_json_value(&wire.body)?;
    let obj = value.as_object().ok_or_else(invalid_json)?;
    let total = obj.get("total_count").and_then(json_u64);
    let items = obj
        .get("workflow_runs")
        .and_then(Value::as_array)
        .ok_or_else(invalid_json)?;
    let mut runs = Vec::with_capacity(items.len());
    for item in items {
        validate_run(item)?;
        runs.push(item.clone());
    }
    let next = next_url(&wire.link, request_url);
    Ok(RunPage {
        body: wire.body.clone(),
        runs,
        total,
        next,
    })
}

fn validate_run(value: &Value) -> Result<(), FetchError> {
    let obj = value.as_object().ok_or_else(invalid_json)?;
    require_u64(obj, "id")?;
    require_string(obj, "path")?;
    require_u64(obj, "run_attempt")?;
    require_u64(obj, "run_number")?;
    require_string(obj, "event")?;
    optional_string(obj, "conclusion")?;
    optional_string(obj, "run_started_at")?;
    Ok(())
}

struct JobsPage {
    jobs: usize,
    next: Option<String>,
}

fn parse_jobs_page(wire: &Wire, run_id: u64, request_url: &str) -> Result<JobsPage, FetchError> {
    let value = parse_json_value(&wire.body)?;
    let obj = value.as_object().ok_or_else(invalid_json)?;
    let items = obj
        .get("jobs")
        .and_then(Value::as_array)
        .ok_or_else(invalid_json)?;
    for item in items {
        validate_job(item, run_id)?;
    }
    Ok(JobsPage {
        jobs: items.len(),
        next: next_url(&wire.link, request_url),
    })
}

fn validate_job(value: &Value, expected_run: u64) -> Result<(), FetchError> {
    let obj = value.as_object().ok_or_else(invalid_json)?;
    require_u64(obj, "id")?;
    let run_id = require_u64(obj, "run_id")?;
    if run_id != expected_run {
        return Err(FetchError::input("run id does not match file name"));
    }
    require_u64(obj, "run_attempt")?;
    require_string(obj, "name")?;
    optional_string(obj, "started_at")?;
    optional_string(obj, "completed_at")?;
    optional_string(obj, "conclusion")?;
    optional_string(obj, "runner_name")?;
    match obj.get("labels") {
        None | Some(Value::Null) => {}
        Some(Value::Array(items)) if items.iter().all(|item| item.as_str().is_some()) => {}
        Some(_) => return Err(invalid_json()),
    }
    Ok(())
}

fn require_u64(obj: &Map<String, Value>, key: &str) -> Result<u64, FetchError> {
    obj.get(key).and_then(json_u64).ok_or_else(invalid_json)
}

fn require_string(obj: &Map<String, Value>, key: &str) -> Result<(), FetchError> {
    obj.get(key)
        .and_then(Value::as_str)
        .ok_or_else(invalid_json)
        .map(|_| ())
}

fn optional_string(obj: &Map<String, Value>, key: &str) -> Result<(), FetchError> {
    match obj.get(key) {
        None | Some(Value::Null) | Some(Value::String(_)) => Ok(()),
        Some(_) => Err(invalid_json()),
    }
}

fn json_u64(value: &Value) -> Option<u64> {
    if let Some(number) = value.as_u64() {
        return Some(number);
    }
    let float = value.as_f64()?;
    if float.is_finite() && float >= 0.0 && float.fract() == 0.0 && float <= u64::MAX as f64 {
        Some(float as u64)
    } else {
        None
    }
}

fn parse_json_value(body: &[u8]) -> Result<Value, FetchError> {
    serde_json::from_slice(strip_bom(body)).map_err(|_| invalid_json())
}

fn invalid_json() -> FetchError {
    FetchError::input("invalid JSON from the GitHub API")
}

fn invalid_contents() -> FetchError {
    FetchError::input("invalid contents response")
}

fn default_branch(body: &[u8]) -> Result<String, FetchError> {
    let value = parse_json_value(body)?;
    let obj = value.as_object().ok_or_else(invalid_json)?;
    let branch = obj
        .get("default_branch")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("main");
    if !safe_reference(branch) {
        return Ok("main".to_string());
    }
    Ok(branch.to_string())
}

fn next_url(link_values: &[String], request_url: &str) -> Option<String> {
    for value in link_values {
        if let Some(url) = next_from_link(value) {
            return Some(resolve_url(request_url, &url));
        }
    }
    None
}

fn next_from_link(value: &str) -> Option<String> {
    for part in split_links(value) {
        let (url, params) = parse_link_part(part)?;
        let is_next = params
            .iter()
            .any(|(key, rel)| key.eq_ignore_ascii_case("rel") && rel_is_next(rel));
        if is_next {
            return Some(url);
        }
    }
    None
}

fn rel_is_next(value: &str) -> bool {
    value
        .split_whitespace()
        .any(|token| token.eq_ignore_ascii_case("next"))
}

fn split_links(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut angle = 0i32;
    let mut quote = false;
    for (index, byte) in value.bytes().enumerate() {
        match byte {
            b'"' => quote = !quote,
            b'<' if !quote => angle += 1,
            b'>' if !quote && angle > 0 => angle -= 1,
            b',' if !quote && angle == 0 => {
                let part = value[start..index].trim();
                if !part.is_empty() {
                    parts.push(part);
                }
                start = index + 1;
            }
            _ => {}
        }
    }
    let last = value[start..].trim();
    if !last.is_empty() {
        parts.push(last);
    }
    parts
}

fn parse_link_part(part: &str) -> Option<(String, Vec<(String, String)>)> {
    let rest = part.trim().strip_prefix('<')?;
    let (url, after) = rest.split_once('>')?;
    let mut params = Vec::new();
    for piece in after.split(';') {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        let (key, value) = piece.split_once('=')?;
        params.push((
            key.trim().to_string(),
            value.trim().trim_matches('"').to_string(),
        ));
    }
    Some((url.to_string(), params))
}

fn resolve_url(base: &str, link: &str) -> String {
    let link = link.trim();
    if link.starts_with("https://") || link.starts_with("http://") {
        return link.to_string();
    }
    if let Some(origin) = origin_of(base) {
        if let Some(rest) = link.strip_prefix("//") {
            let scheme = if base.starts_with("https://") {
                "https"
            } else {
                "http"
            };
            return format!("{scheme}://{rest}");
        }
        if link.starts_with('/') {
            return format!("{origin}{link}");
        }
    }
    link.to_string()
}

fn origin_of(url: &str) -> Option<&str> {
    let scheme = url.find("://")?;
    let after = scheme + 3;
    if after >= url.len() {
        return None;
    }
    let end = url[after..]
        .find(['/', '?', '#'])
        .map(|index| after + index)
        .unwrap_or(url.len());
    if end == after {
        return None;
    }
    Some(&url[..end])
}

fn same_origin(base: &str, url: &str) -> bool {
    match (origin_of(base), origin_of(url)) {
        (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
        _ => false,
    }
}

enum Contents {
    Bytes(Vec<u8>),
    Download(String),
}

fn contents_bytes(content_type: &str, body: &[u8]) -> Result<Contents, FetchError> {
    let Some(value) = contents_envelope(content_type, body)? else {
        return Ok(Contents::Bytes(body.to_vec()));
    };
    let obj = value.as_object().ok_or_else(invalid_contents)?;
    let encoding = obj
        .get("encoding")
        .and_then(Value::as_str)
        .unwrap_or("base64");
    let content = obj.get("content");
    let empty = match content {
        None | Some(Value::Null) => true,
        Some(Value::String(text)) => text.trim().is_empty(),
        Some(_) => return Err(invalid_contents()),
    };
    if empty || encoding.eq_ignore_ascii_case("none") {
        if let Some(url) = obj.get("download_url").and_then(Value::as_str) {
            if !url.is_empty() {
                return Ok(Contents::Download(url.to_string()));
            }
        }
        if empty {
            return Ok(Contents::Bytes(Vec::new()));
        }
    }
    if encoding.eq_ignore_ascii_case("base64") {
        let text = content.and_then(Value::as_str).unwrap_or("");
        let bytes = decode_base64(text).ok_or_else(invalid_contents)?;
        return Ok(Contents::Bytes(bytes));
    }
    if let Some(text) = content.and_then(Value::as_str) {
        return Ok(Contents::Bytes(text.as_bytes().to_vec()));
    }
    Err(invalid_contents())
}

fn contents_envelope(content_type: &str, body: &[u8]) -> Result<Option<Value>, FetchError> {
    let kind = content_type.to_ascii_lowercase();
    if kind.contains("github.raw") || kind.contains("text/plain") || kind.contains("yaml") {
        return Ok(None);
    }
    let bytes = strip_bom(body);
    let trimmed = trim_ascii_start(bytes);
    if !kind.contains("json") && !trimmed.starts_with(b"{") {
        return Ok(None);
    }
    if !trimmed.starts_with(b"{") {
        return Err(invalid_contents());
    }
    let value: Value = serde_json::from_slice(trimmed).map_err(|_| invalid_contents())?;
    let Some(obj) = value.as_object() else {
        return Err(invalid_contents());
    };
    let envelope = obj.contains_key("encoding")
        || obj.contains_key("download_url")
        || obj.get("type").and_then(Value::as_str) == Some("file")
        || (obj.contains_key("content") && obj.contains_key("sha"));
    if envelope {
        Ok(Some(value))
    } else if obj.contains_key("message") {
        Err(invalid_contents())
    } else {
        Ok(None)
    }
}

fn decode_base64(text: &str) -> Option<Vec<u8>> {
    fn value(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes: Vec<u8> = text
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    if bytes.is_empty() {
        return Some(Vec::new());
    }
    if bytes.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for (index, chunk) in bytes.chunks(4).enumerate() {
        let last = index + 1 == bytes.len() / 4;
        if !last && (chunk[2] == b'=' || chunk[3] == b'=') {
            return None;
        }
        if chunk[2] == b'=' && chunk[3] != b'=' {
            return None;
        }
        let a = value(chunk[0])?;
        let b = value(chunk[1])?;
        let c = if chunk[2] == b'=' {
            0
        } else {
            value(chunk[2])?
        };
        let d = if chunk[3] == b'=' {
            0
        } else {
            value(chunk[3])?
        };
        out.push((a << 2) | (b >> 4));
        if chunk[2] != b'=' {
            out.push((b << 4) | (c >> 2));
        }
        if chunk[3] != b'=' {
            out.push((c << 6) | d);
        }
    }
    Some(out)
}

#[derive(Clone)]
struct StoredRun {
    attempt: u64,
    value: Value,
}

#[derive(Default)]
struct Cache {
    runs: BTreeMap<u64, StoredRun>,
    runs_files: Vec<PathBuf>,
    has_jobs: BTreeSet<u64>,
    job_attempt: BTreeMap<u64, u64>,
}

fn load_cache(dir: &Path) -> Result<Cache, FetchError> {
    let mut cache = Cache::default();
    let entries = fs::read_dir(dir).map_err(|_| path_error(dir, "cannot read directory"))?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| path_error(dir, "cannot read directory"))?;
        paths.push(entry.path());
    }
    paths.sort();
    for path in paths {
        let Some(name) = path.file_name().and_then(|item| item.to_str()) else {
            continue;
        };
        if is_runs_file(name) {
            if !path.is_file() {
                return Err(path_error(&path, "cannot read file"));
            }
            cache.runs_files.push(path.clone());
            let text = read_file_text(&path)?;
            let value = parse_json_value(text.as_bytes())?;
            let items = value
                .get("workflow_runs")
                .and_then(Value::as_array)
                .ok_or_else(invalid_json)?;
            for item in items {
                validate_run(item)?;
                let id = item.get("id").and_then(json_u64).ok_or_else(invalid_json)?;
                let attempt = item
                    .get("run_attempt")
                    .and_then(json_u64)
                    .ok_or_else(invalid_json)?;
                if cache
                    .runs
                    .insert(
                        id,
                        StoredRun {
                            attempt,
                            value: item.clone(),
                        },
                    )
                    .is_some()
                {
                    return Err(FetchError::input(format!(
                        "{}: duplicate id",
                        path.display()
                    )));
                }
            }
        } else if let Some(run_id) = job_file_run_id(name) {
            if !path.is_file() {
                return Err(path_error(&path, "cannot read file"));
            }
            cache.has_jobs.insert(run_id);
            let text = read_file_text(&path)?;
            let value = parse_json_value(text.as_bytes())?;
            let items = value
                .get("jobs")
                .and_then(Value::as_array)
                .ok_or_else(invalid_json)?;
            for item in items {
                validate_job(item, run_id)?;
                let attempt = item
                    .get("run_attempt")
                    .and_then(json_u64)
                    .ok_or_else(invalid_json)?;
                let slot = cache.job_attempt.entry(run_id).or_insert(0);
                if attempt > *slot {
                    *slot = attempt;
                }
            }
        }
    }
    Ok(cache)
}

fn read_file_text(path: &Path) -> Result<String, FetchError> {
    let bytes = fs::read(path).map_err(|_| path_error(path, "cannot read file"))?;
    let bytes = strip_bom(&bytes);
    String::from_utf8(bytes.to_vec()).map_err(|_| path_error(path, "invalid JSON"))
}

struct Incoming {
    attempt: u64,
    value: Value,
}

struct Merged {
    runs: BTreeMap<u64, StoredRun>,
    jobs: Vec<(u64, u64)>,
    discovered: Vec<Value>,
    run_count: usize,
    rewrite_runs: bool,
}

fn incoming_runs(pages: &[RunPage]) -> Result<Vec<Incoming>, FetchError> {
    let mut out = Vec::new();
    for page in pages {
        for run in &page.runs {
            out.push(Incoming {
                attempt: run
                    .get("run_attempt")
                    .and_then(json_u64)
                    .ok_or_else(invalid_json)?,
                value: run.clone(),
            });
        }
    }
    Ok(out)
}

fn merge_runs(cache: &Cache, incoming: &[Incoming]) -> Merged {
    let mut best: BTreeMap<u64, StoredRun> = BTreeMap::new();
    let mut discovered = Vec::new();
    for run in incoming {
        let Some(id) = run.value.get("id").and_then(json_u64) else {
            continue;
        };
        discovered.push(run.value.clone());
        match best.get(&id) {
            Some(old) if run.attempt <= old.attempt => {}
            _ => {
                best.insert(
                    id,
                    StoredRun {
                        attempt: run.attempt,
                        value: run.value.clone(),
                    },
                );
            }
        }
    }
    let mut runs = cache.runs.clone();
    let mut jobs = Vec::new();
    for (id, run) in &best {
        let stored = cache
            .runs
            .get(id)
            .map(|item| item.attempt)
            .or_else(|| cache.job_attempt.get(id).copied());
        if should_fetch_jobs(stored, cache.has_jobs.contains(id), run.attempt) {
            jobs.push((*id, run.attempt));
        }
        match runs.get(id) {
            Some(old) if run.attempt <= old.attempt => {}
            _ => {
                runs.insert(*id, run.clone());
            }
        }
    }
    let run_count = best.len();
    let rewrite_runs = !cache.runs_files.is_empty() || !pages_unique_ids(incoming);
    Merged {
        runs,
        jobs,
        discovered,
        run_count,
        rewrite_runs,
    }
}

fn pages_unique_ids(incoming: &[Incoming]) -> bool {
    let mut seen = HashSet::new();
    incoming.iter().all(|run| {
        run.value
            .get("id")
            .and_then(json_u64)
            .is_some_and(|id| seen.insert(id))
    })
}

fn should_fetch_jobs(stored_attempt: Option<u64>, jobs_present: bool, new_attempt: u64) -> bool {
    !matches!(stored_attempt, Some(old) if jobs_present && new_attempt <= old)
}

struct JobBatch {
    id: u64,
    bodies: Vec<Vec<u8>>,
    jobs: usize,
}

struct RunWrite<'a> {
    cache: &'a Cache,
    pages: &'a [RunPage],
    merged: &'a BTreeMap<u64, StoredRun>,
    rewrite_runs: bool,
}

fn commit(
    out: &Path,
    repo_body: &[u8],
    runs: &RunWrite<'_>,
    jobs: &[JobBatch],
    files: &[(PathBuf, Vec<u8>)],
) -> Result<(), FetchError> {
    fs::create_dir_all(out).map_err(|_| path_error(out, "cannot create directory"))?;
    write_bytes(&out.join("repo.json"), repo_body)?;
    if !runs.pages.is_empty() {
        if runs.rewrite_runs {
            write_merged_runs(out, runs.cache, runs.merged)?;
        } else {
            for (index, page) in runs.pages.iter().enumerate() {
                write_bytes(&out.join(format!("runs-{}.json", index + 1)), &page.body)?;
            }
        }
    }
    for batch in jobs {
        write_job_batch(out, batch)?;
    }
    for (path, bytes) in files {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|_| path_error(parent, "cannot create directory"))?;
        }
        write_bytes(path, bytes)?;
    }
    Ok(())
}

fn write_merged_runs(
    out: &Path,
    cache: &Cache,
    merged: &BTreeMap<u64, StoredRun>,
) -> Result<(), FetchError> {
    let values: Vec<&Value> = merged.values().map(|run| &run.value).collect();
    let mut temps = Vec::new();
    for (index, chunk) in values.chunks(100).enumerate() {
        let mut body = Map::new();
        body.insert("total_count".to_string(), Value::from(chunk.len()));
        body.insert(
            "workflow_runs".to_string(),
            Value::Array(chunk.iter().map(|value| (*value).clone()).collect()),
        );
        let bytes = serde_json::to_vec(&Value::Object(body)).map_err(|_| invalid_json())?;
        let temp = out.join(format!(".part-runs-{}.tmp", index + 1));
        if let Err(error) = write_bytes(&temp, &bytes) {
            remove_temps(&temps);
            return Err(error);
        }
        temps.push(temp);
    }
    for path in &cache.runs_files {
        if let Err(error) = fs::remove_file(path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                remove_temps(&temps);
                return Err(path_error(path, "cannot write file"));
            }
        }
    }
    for (index, temp) in temps.iter().enumerate() {
        let dest = out.join(format!("runs-{}.json", index + 1));
        if let Err(_error) = fs::rename(temp, &dest) {
            remove_temps(&temps);
            return Err(path_error(&dest, "cannot write file"));
        }
    }
    Ok(())
}

fn write_job_batch(out: &Path, batch: &JobBatch) -> Result<(), FetchError> {
    let mut temps = Vec::new();
    for (index, body) in batch.bodies.iter().enumerate() {
        let temp = out.join(format!(".part-jobs-{}-{}.tmp", batch.id, index + 1));
        if let Err(error) = write_bytes(&temp, body) {
            remove_temps(&temps);
            return Err(error);
        }
        temps.push(temp);
    }
    let old = match job_paths(out, batch.id) {
        Ok(paths) => paths,
        Err(error) => {
            remove_temps(&temps);
            return Err(error);
        }
    };
    for path in &old {
        if let Err(error) = fs::remove_file(path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                remove_temps(&temps);
                return Err(path_error(path, "cannot write file"));
            }
        }
    }
    for (index, temp) in temps.iter().enumerate() {
        let name = if index == 0 {
            format!("jobs-{}.json", batch.id)
        } else {
            format!("jobs-{}-{}.json", batch.id, index + 1)
        };
        let dest = out.join(name);
        if let Err(_error) = fs::rename(temp, &dest) {
            remove_temps(&temps);
            return Err(path_error(&dest, "cannot write file"));
        }
    }
    Ok(())
}

fn job_paths(dir: &Path, run_id: u64) -> Result<Vec<PathBuf>, FetchError> {
    let entries = fs::read_dir(dir).map_err(|_| path_error(dir, "cannot read directory"))?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| path_error(dir, "cannot read directory"))?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|item| item.to_str()) else {
            continue;
        };
        if job_file_run_id(name) == Some(run_id) {
            paths.push(path);
        }
    }
    Ok(paths)
}

fn remove_temps(paths: &[PathBuf]) {
    for path in paths {
        let _ = fs::remove_file(path);
    }
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), FetchError> {
    fs::write(path, bytes).map_err(|_| path_error(path, "cannot write file"))
}

fn path_error(path: &Path, message: &str) -> FetchError {
    FetchError::input(format!("{}: {message}", path.display()))
}

fn remote_from_run(
    run: &Value,
    owner: &str,
    repo: &str,
    default_branch: &str,
) -> Option<RemoteWorkflow> {
    let raw = run.get("path")?.as_str()?.trim().trim_matches('\u{feff}');
    let (path, path_ref) = match raw.split_once('@') {
        Some((path, reference)) if !reference.trim().is_empty() => {
            (path.trim(), Some(reference.trim().to_string()))
        }
        _ => (raw, None),
    };
    let linked = api::linked_workflow_path(path);
    if !is_actions_workflow(&linked) {
        return None;
    }
    let reference = string_field(run, "head_sha")
        .or(path_ref)
        .unwrap_or_else(|| default_branch.to_string());
    if !safe_reference(&reference) {
        return None;
    }
    Some(RemoteWorkflow {
        owner: owner.to_string(),
        repo: repo.to_string(),
        workflow_path: linked.trim_start_matches('/').to_string(),
        reference,
    })
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn is_actions_workflow(path: &str) -> bool {
    const PREFIX: &str = ".github/workflows/";
    if path.len() < PREFIX.len() || !path[..PREFIX.len()].eq_ignore_ascii_case(PREFIX) {
        return false;
    }
    let file = &path[PREFIX.len()..];
    if file.is_empty() || file.contains('/') || file.contains('\\') || file.contains('\0') {
        return false;
    }
    match file.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => {
            ext.eq_ignore_ascii_case("yml") || ext.eq_ignore_ascii_case("yaml")
        }
        _ => false,
    }
}

fn safe_reference(reference: &str) -> bool {
    if reference.is_empty()
        || reference.contains('\\')
        || reference.contains('\0')
        || reference.starts_with('/')
        || reference.ends_with('/')
    {
        return false;
    }
    reference
        .split('/')
        .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn enqueue(
    queue: &mut VecDeque<RemoteWorkflow>,
    seen: &mut HashSet<String>,
    remote: RemoteWorkflow,
) {
    if !valid_name(&remote.owner) || !valid_name(&remote.repo) {
        return;
    }
    if !is_actions_workflow(&remote.workflow_path) || !safe_reference(&remote.reference) {
        return;
    }
    let key = format!(
        "{}\n{}\n{}\n{}",
        remote.owner, remote.repo, remote.workflow_path, remote.reference
    );
    if seen.insert(key) {
        queue.push_back(remote);
    }
}

fn safe_cache_path(out: &Path, remote: &RemoteWorkflow) -> Option<PathBuf> {
    let relative = remote.cache_relative();
    if relative.as_os_str().is_empty() {
        return None;
    }
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    Some(out.join(relative))
}

fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes)
}

fn trim_ascii_start(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    &bytes[start..]
}

fn encode_path(path: &str) -> String {
    path.split('/')
        .map(encode_component)
        .collect::<Vec<_>>()
        .join("/")
}

fn encode_component(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            const HEX: &[u8] = b"0123456789ABCDEF";
            out.push('%');
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    out
}

fn redact_all_str(text: &str, needles: &[String]) -> String {
    let mut current = text.as_bytes().to_vec();
    for needle in needles {
        current = redact_bytes(&current, needle);
    }
    String::from_utf8(current).unwrap_or_else(|_| "invalid response".to_string())
}

fn redact_bytes(bytes: &[u8], token: &str) -> Vec<u8> {
    let needle = token.as_bytes();
    if needle.is_empty() || bytes.len() < needle.len() {
        return bytes.to_vec();
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(needle) {
            out.extend_from_slice(b"[redacted]");
            index += needle.len();
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    out
}

fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let mut year = i64::from(year);
    if month <= 2 {
        year -= 1;
    }
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = (year - era * 400) as u64;
    let month_adj = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * month_adj + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + u64::from(doy);
    era * 146097 + doe as i64 - 719468
}

fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (year as i32, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(status: u16, body: &str) -> Wire {
        Wire {
            status,
            remaining: None,
            reset: None,
            retry_after: None,
            location: None,
            content_type: "application/json".to_string(),
            link: Vec::new(),
            body: body.as_bytes().to_vec(),
            url: "http://example.test/".to_string(),
        }
    }

    fn fixed_now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    #[test]
    fn dates_cover_leap_years_and_windows() {
        assert!(parse_date("2026-02-29").is_none());
        assert!(parse_date("2024-02-29").is_some());
        assert!(parse_date("2026-1-01").is_none());
        assert!(parse_date("01-01-2026").is_none());
        assert_eq!(parse_date(" 2026-01-01 ").unwrap().unix(), 1_767_225_600);
        let since = parse_date("2026-01-01").unwrap();
        let until = parse_date("2026-01-02").unwrap();
        let window = Window {
            start: since.unix(),
            end: until.next_unix(),
        };
        assert_eq!(window.created_param(), "2026-01-01..2026-01-02");
        let (left, right) = window.split().unwrap();
        assert_eq!(left.created_param(), "2026-01-01..2026-01-01");
        assert_eq!(right.created_param(), "2026-01-02..2026-01-02");
        let wide = Window {
            start: since.unix(),
            end: parse_date("2026-01-03").unwrap().next_unix(),
        };
        let (left, right) = wide.split().unwrap();
        assert_eq!(
            left.created_param(),
            "2026-01-01T00:00:00Z..2026-01-02T11:59:59Z"
        );
        assert_eq!(
            right.created_param(),
            "2026-01-02T12:00:00Z..2026-01-03T23:59:59Z"
        );
        assert_eq!(
            encode_component(&left.created_param()),
            "2026-01-01T00%3A00%3A00Z..2026-01-02T11%3A59%3A59Z"
        );
        assert!(Window { start: 10, end: 11 }.split().is_none());
        assert_eq!(UtcDate::from_unix(1_767_225_600).text(), "2026-01-01");
    }

    #[test]
    fn repo_names_reject_extra_slashes_and_dots() {
        assert!(parse_repo_arg("acme/demo").is_ok());
        assert!(parse_repo_arg(" Acme/Demo ").is_ok());
        assert!(parse_repo_arg("acme").is_err());
        assert!(parse_repo_arg("acme/demo/app").is_err());
        assert!(parse_repo_arg("acme/").is_err());
        assert!(parse_repo_arg("/demo").is_err());
        assert!(parse_repo_arg("./demo").is_err());
        assert!(parse_repo_arg("acme/.").is_err());
    }

    #[test]
    fn link_header_finds_next_in_any_order_and_case() {
        let value =
            r#"<https://example.test/last>; rel="last", <https://example.test/next>; rel="Next""#;
        assert_eq!(
            next_from_link(value).as_deref(),
            Some("https://example.test/next")
        );
        assert_eq!(
            next_from_link(r#"</repos/o/r?page=2>; rel=next"#).as_deref(),
            Some("/repos/o/r?page=2")
        );
        let resolved = next_url(
            &[r#"</repos/o/r/actions/runs?page=2&per_page=100>; rel="next""#.to_string()],
            "http://127.0.0.1:9/repos/o/r/actions/runs?page=1",
        );
        assert_eq!(
            resolved.as_deref(),
            Some("http://127.0.0.1:9/repos/o/r/actions/runs?page=2&per_page=100")
        );
        assert!(next_from_link(r#"<https://example.test/only>; rel="last""#).is_none());
    }

    #[test]
    fn base64_ignores_line_breaks_and_case_of_the_caller() {
        let text = "bmFtZTogTGludA==\n";
        assert_eq!(decode_base64(text).unwrap(), b"name: Lint");
        assert_eq!(decode_base64("").unwrap(), b"");
        assert!(decode_base64("====").is_none());
        assert!(decode_base64("bmFtZQ").is_none());
        let body = br#"{"encoding":"BASE64","content":"bmFtZTogTGludA==","type":"file"}"#;
        match contents_bytes("application/json", body).unwrap() {
            Contents::Bytes(bytes) => assert_eq!(bytes, b"name: Lint"),
            Contents::Download(_) => panic!("expected bytes"),
        }
        let raw = b"name: CI\r\n";
        match contents_bytes("application/vnd.github.raw", raw).unwrap() {
            Contents::Bytes(bytes) => assert_eq!(bytes, raw),
            Contents::Download(_) => panic!("expected raw bytes"),
        }
    }

    #[test]
    fn redaction_removes_every_token_copy() {
        let token = "ghs_secret_value";
        let body = format!(r#"{{"note":"{token}","again":"{token}"}}"#);
        let needles = vec![token.to_string()];
        let redacted = redact_all_str(&body, &needles);
        assert!(!redacted.contains(token));
        assert!(redacted.contains("[redacted]"));
        assert_eq!(redact_all_str("plain", &needles), "plain");
    }

    #[test]
    fn rate_limit_order_and_backoff() {
        let now = fixed_now();
        let mut ok = wire(200, "{}");
        ok.remaining = Some("0".to_string());
        assert!(matches!(decide(&ok, 1, now), Decision::Proceed));

        let mut primary = wire(403, r#"{"message":"secondary rate limit"}"#);
        primary.remaining = Some(" 0 ".to_string());
        primary.reset = Some("1700000005".to_string());
        primary.retry_after = Some("30".to_string());
        match decide(&primary, 1, now) {
            Decision::Wait(delay) => assert_eq!(delay, Duration::from_secs(30)),
            other => panic!("retry-after should win over remaining==0/reset: {other:?}"),
        }

        let mut primary_no_retry = wire(403, r#"{"message":"secondary rate limit"}"#);
        primary_no_retry.remaining = Some(" 0 ".to_string());
        primary_no_retry.reset = Some("1700000005".to_string());
        match decide(&primary_no_retry, 1, now) {
            Decision::Wait(delay) => assert_eq!(delay, Duration::from_secs(5)),
            other => panic!("remaining==0 without retry-after should wait until reset: {other:?}"),
        }

        let mut secondary_429_retry_after = wire(429, "secondary rate limit");
        secondary_429_retry_after.remaining = Some("0".to_string());
        secondary_429_retry_after.reset = Some("1700000999".to_string());
        secondary_429_retry_after.retry_after = Some("3".to_string());
        match decide(&secondary_429_retry_after, 1, now) {
            Decision::Wait(delay) => assert_eq!(delay, Duration::from_secs(3)),
            other => panic!("retry-after on 429 with remaining==0 should still win: {other:?}"),
        }

        let mut secondary = wire(429, "You have exceeded a SECONDARY RATE LIMIT.");
        secondary.retry_after = Some("2".to_string());
        secondary.remaining = Some("40".to_string());
        match decide(&secondary, 1, now) {
            Decision::Wait(delay) => assert_eq!(delay, Duration::from_secs(2)),
            other => panic!("retry-after should win: {other:?}"),
        }

        let bare = wire(403, "secondary rate limit");
        assert!(matches!(
            decide(&bare, 1, now),
            Decision::Wait(delay) if delay == Duration::from_secs(60)
        ));
        assert!(matches!(
            decide(&bare, 2, now),
            Decision::Wait(delay) if delay == Duration::from_secs(120)
        ));
        assert!(matches!(
            decide(&bare, 3, now),
            Decision::Wait(delay) if delay == Duration::from_secs(240)
        ));
        assert!(matches!(
            decide(&bare, 4, now),
            Decision::Wait(delay) if delay == Duration::from_secs(480)
        ));

        let mut http_date = wire(429, "secondary rate limit");
        http_date.retry_after = Some("Wed, 14 Nov 2023 22:13:22 GMT".to_string());
        match decide(&http_date, 1, now) {
            Decision::Wait(delay) => assert_eq!(delay, Duration::from_secs(2)),
            other => panic!("http-date retry-after: {other:?}"),
        }

        assert!(matches!(
            decide(&wire(403, "Resource not accessible"), 1, now),
            Decision::Permission
        ));
        assert!(matches!(
            decide(&wire(401, "bad"), 1, now),
            Decision::Unauthorized
        ));
        assert!(matches!(
            decide(&wire(500, "down"), 1, now),
            Decision::Stop(500)
        ));
        assert!(matches!(
            decide(&wire(404, "missing"), 1, now),
            Decision::Proceed
        ));

        let mut sleeps = Vec::new();
        let mut calls = 0;
        let needles = vec!["token".to_string()];
        let err = retry(
            || {
                calls += 1;
                Ok(wire(403, "secondary rate limit"))
            },
            |delay| sleeps.push(delay),
            || now,
            &needles,
        );
        assert!(err.is_err());
        assert_eq!(calls, 5);
        assert_eq!(
            sleeps,
            vec![
                Duration::from_secs(60),
                Duration::from_secs(120),
                Duration::from_secs(240),
                Duration::from_secs(480),
            ]
        );
        assert!(!err.unwrap_err().message.contains("token"));
    }

    #[test]
    fn jobs_are_fetched_again_only_when_the_attempt_is_higher_or_missing() {
        assert!(!should_fetch_jobs(Some(2), true, 2));
        assert!(!should_fetch_jobs(Some(3), true, 2));
        assert!(should_fetch_jobs(Some(1), true, 2));
        assert!(should_fetch_jobs(Some(2), false, 2));
        assert!(should_fetch_jobs(None, true, 1));
        assert!(should_fetch_jobs(None, false, 1));
    }

    #[test]
    fn a_one_second_window_uses_utc_timestamps() {
        let window = Window {
            start: 1_767_225_600,
            end: 1_767_225_601,
        };
        assert_eq!(
            window.created_param(),
            "2026-01-01T00:00:00Z..2026-01-01T00:00:00Z"
        );
    }
}
