//! `ci-bill fetch` against a local TCP server.
//!
//! The binary reads `GITHUB_API_URL`, so these tests never call the public API.

use std::fs;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

const TOKEN: &str = "ghs_A5fetch_7c1e9b2d4a6f4083";
const OTHER_TOKEN: &str = "ghs_OtherToken_99aa11bb";

#[derive(Clone)]
struct Hit {
    target: String,
    headers: Vec<(String, String)>,
    at: Instant,
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

struct TestServer {
    addr: SocketAddr,
    hits: Arc<Mutex<Vec<Hit>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl TestServer {
    fn spawn(handler: impl Fn(&Hit, usize) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let hits = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let hits_thread = Arc::clone(&hits);
        let stop_thread = Arc::clone(&stop);
        let handler = Arc::new(handler);
        let thread = thread::spawn(move || {
            while !stop_thread.load(Ordering::SeqCst) {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                if stop_thread.load(Ordering::SeqCst) {
                    break;
                }
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                let _ = stream.set_nodelay(true);
                let Some(hit) = read_request(&mut stream) else {
                    continue;
                };
                let earlier = {
                    let mut guard = hits_thread.lock().expect("hits");
                    let earlier = guard.len();
                    guard.push(hit);
                    earlier
                };
                let reply = {
                    let guard = hits_thread.lock().expect("hits");
                    handler(&guard[earlier], earlier)
                };
                write_reply(&mut stream, &reply);
            }
        });
        Self {
            addr,
            hits,
            stop,
            thread: Some(thread),
        }
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn hits(&self) -> Vec<Hit> {
        self.hits.lock().expect("hits").clone()
    }

    fn clear(&self) {
        self.hits.lock().expect("hits").clear();
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Hit> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut tmp).ok()?;
        if read == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..read]);
        if let Some(position) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
            break position;
        }
        if buf.len() > 1024 * 1024 {
            return None;
        }
    };
    let header_text = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut lines = header_text.split("\r\n");
    let request = lines.next()?;
    let mut parts = request.split(' ');
    let _method = parts.next()?;
    let target = parts.next()?.to_string();
    let mut headers = Vec::new();
    let mut content_length = 0usize;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line.split_once(':')?;
        let name = name.trim().to_string();
        let value = value.trim().to_string();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse().unwrap_or(0);
        }
        headers.push((name, value));
    }
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < content_length {
        let read = stream.read(&mut tmp).ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..read]);
    }
    Some(Hit {
        target,
        headers,
        at: Instant::now(),
    })
}

fn write_reply(stream: &mut TcpStream, reply: &Reply) {
    let reason = match reply.status {
        200 => "OK",
        301 => "Moved Permanently",
        302 => "Found",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Server Error",
        _ => "Response",
    };
    let mut head = format!("HTTP/1.1 {} {reason}\r\n", reply.status);
    let mut saw_length = false;
    let mut saw_connection = false;
    for (name, value) in &reply.headers {
        if name.eq_ignore_ascii_case("content-length") {
            saw_length = true;
        }
        if name.eq_ignore_ascii_case("connection") {
            saw_connection = true;
        }
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    if !saw_length {
        head.push_str(&format!("Content-Length: {}\r\n", reply.body.len()));
    }
    if !saw_connection {
        head.push_str("Connection: close\r\n");
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&reply.body);
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Write);
}

fn header<'a>(hit: &'a Hit, name: &str) -> &'a str {
    hit.headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
        .unwrap_or("")
}

fn path_only(target: &str) -> &str {
    let mut text = target;
    if let Some(rest) = text
        .strip_prefix("http://")
        .or_else(|| text.strip_prefix("https://"))
    {
        if let Some(index) = rest.find('/') {
            text = &rest[index..];
        }
    }
    text.split('?').next().unwrap_or(text)
}

fn created_of(target: &str) -> String {
    let Some((_, rest)) = target.split_once("created=") else {
        return String::new();
    };
    let raw = rest.split(['&', ' ']).next().unwrap_or("");
    percent_decode(raw)
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) =
                (hex_digit(bytes[index + 1]), hex_digit(bytes[index + 2]))
            {
                out.push((high << 4) | low);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn is_repo(target: &str) -> bool {
    path_only(target) == "/repos/acme/demo"
}

fn is_runs(target: &str) -> bool {
    let path = path_only(target);
    path.contains("/actions/runs") && !path.contains("/jobs")
}

fn is_jobs(target: &str) -> bool {
    path_only(target).contains("/jobs")
}

fn json_reply(status: u16, body: Vec<u8>, extra: Vec<(String, String)>) -> Reply {
    let mut headers = vec![("Content-Type".to_string(), "application/json".to_string())];
    headers.extend(extra);
    Reply {
        status,
        headers,
        body,
    }
}

fn ok_json(body: impl Into<Vec<u8>>) -> Reply {
    json_reply(200, body.into(), Vec::new())
}

fn repo_json(extra: &str) -> Vec<u8> {
    format!("{{\"private\":true,\"default_branch\":\"main\"{extra}}}\n").into_bytes()
}

fn run_value(id: u64, attempt: u64, path: &str, event: &str) -> Value {
    json!({
        "event": event,
        "path": path,
        "run_attempt": attempt,
        "run_number": 1,
        "id": id
    })
}

fn runs_page(total: Option<u64>, runs: Vec<Value>) -> Value {
    let mut body = serde_json::Map::new();
    if let Some(total) = total {
        body.insert("total_count".to_string(), json!(total));
    }
    body.insert("workflow_runs".to_string(), Value::Array(runs));
    Value::Object(body)
}

fn job_value(id: u64, run_id: u64, attempt: u64, name: &str) -> Value {
    json!({
        "id": id,
        "run_id": run_id,
        "run_attempt": attempt,
        "name": name
    })
}

fn jobs_page(jobs: Vec<Value>) -> Vec<u8> {
    serde_json::to_vec(&json!({"jobs": jobs})).expect("jobs")
}

fn one_job(run_id: u64, attempt: u64) -> Reply {
    ok_json(jobs_page(vec![job_value(
        run_id + 10,
        run_id,
        attempt,
        "build",
    )]))
}

fn empty_runs() -> Reply {
    ok_json(serde_json::to_vec(&runs_page(Some(0), Vec::new())).expect("runs"))
}

fn scratch(name: &str) -> PathBuf {
    let base = std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let path = base.join("ci-bill-fetch").join(name);
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("scratch");
    path
}

enum TokenMode {
    None,
    Gh,
    Github,
    Both,
    BlankGh,
    EmptyPair,
}

fn run_fetch(url: &str, mode: TokenMode, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ci-bill"));
    command
        .args(args)
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env("GITHUB_API_URL", url);
    match mode {
        TokenMode::None => {}
        TokenMode::Gh => {
            command.env("GH_TOKEN", TOKEN);
        }
        TokenMode::Github => {
            command.env("GITHUB_TOKEN", OTHER_TOKEN);
        }
        TokenMode::Both => {
            command.env("GH_TOKEN", TOKEN);
            command.env("GITHUB_TOKEN", OTHER_TOKEN);
        }
        TokenMode::BlankGh => {
            command.env("GH_TOKEN", " \t");
            command.env("GITHUB_TOKEN", OTHER_TOKEN);
        }
        TokenMode::EmptyPair => {
            command.env("GH_TOKEN", "");
            command.env("GITHUB_TOKEN", "");
        }
    }
    command.output().expect("run ci-bill")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn targets_of(hits: &[Hit]) -> String {
    hits.iter()
        .map(|hit| hit.target.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_success(output: &Output, hits: &[Hit]) {
    assert!(
        output.status.success(),
        "exit {:?}\nstdout:\n{}\nstderr:\n{}\nhits:\n{}",
        output.status.code(),
        stdout_of(output),
        stderr_of(output),
        targets_of(hits)
    );
    assert!(
        stderr_of(output).is_empty(),
        "stderr:\n{}",
        stderr_of(output)
    );
}

fn assert_code(output: &Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout:\n{}\nstderr:\n{}",
        stdout_of(output),
        stderr_of(output)
    );
}

fn assert_hidden(out: &Path, output: &Output, hits: &[Hit], tokens: &[&str]) {
    let mut files = Vec::new();
    walk_files(out, &mut files);
    let stdout = stdout_of(output);
    let stderr = stderr_of(output);
    for token in tokens {
        assert!(!stdout.contains(token), "stdout contains a token");
        assert!(!stderr.contains(token), "stderr contains a token");
        for path in &files {
            let bytes = fs::read(path).expect("read");
            let text = String::from_utf8_lossy(&bytes);
            assert!(!text.contains(token), "{} contains a token", path.display());
        }
        for hit in hits {
            assert!(!hit.target.contains(token), "url contains a token");
        }
    }
}

fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if !dir.exists() {
        return;
    }
    let entries = fs::read_dir(dir).expect("read dir");
    for entry in entries {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            walk_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn assert_auth(hits: &[Hit], token: &str) {
    let expected = format!("Bearer {token}");
    for hit in hits {
        assert_eq!(header(hit, "authorization"), expected, "{}", hit.target);
        assert_eq!(header(hit, "user-agent"), "ci-bill", "{}", hit.target);
        assert_eq!(
            header(hit, "x-github-api-version"),
            "2022-11-28",
            "{}",
            hit.target
        );
    }
}

fn assert_report_reads(out: &Path) {
    let csv = out.join("usage.csv");
    fs::write(
        &csv,
        "date,product,sku,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,net_amount,username,organization,repository,workflow_path,cost_center_name\n2026-09-01,actions,actions_linux,1,minutes,0.006,0.006,0,0.006,user,acme,acme/demo,.github/workflows/ci.yml,\n",
    )
    .expect("csv");
    let output = Command::new(env!("CARGO_BIN_EXE_ci-bill"))
        .args([
            "report",
            "--usage",
            csv.to_str().expect("utf8"),
            "--runs",
            out.to_str().expect("utf8"),
        ])
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GITHUB_API_URL")
        .output()
        .expect("report");
    assert!(
        output.status.success(),
        "report exit {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        stdout_of(&output),
        stderr_of(&output)
    );
}

fn saved_json(path: &Path) -> Value {
    let bytes = fs::read(path).expect("read");
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    serde_json::from_slice(bytes).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

fn seed_keep(out: &Path) {
    fs::create_dir_all(out).expect("out");
    fs::write(out.join("keep.txt"), b"KEEP\n").expect("seed");
}

fn assert_kept(out: &Path) {
    assert_eq!(fs::read(out.join("keep.txt")).expect("keep"), b"KEEP\n");
    assert!(!out.join("repo.json").exists(), "repo.json was written");
    assert!(!out.join("runs-1.json").exists(), "runs were written");
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

fn utc_date_string(unix: u64) -> String {
    let (year, month, day) = civil_from_days((unix / 86400) as i64);
    format!("{year:04}-{month:02}-{day:02}")
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

fn http_date(unix: u64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let date = utc_date_string(unix);
    let mut parts = date.split('-');
    let year = parts.next().expect("year");
    let month: usize = parts.next().expect("month").parse().expect("month");
    let day = parts.next().expect("day");
    let seconds = unix % 86400;
    let hour = seconds / 3600;
    let minute = (seconds % 3600) / 60;
    let second = seconds % 60;
    format!(
        "Wed, {day} {month_name} {year} {hour:02}:{minute:02}:{second:02} GMT",
        month_name = MONTHS[month - 1]
    )
}

fn fetch_args(out: &Path, since: &str, until: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "fetch".to_string(),
        "--repo".to_string(),
        "acme/demo".to_string(),
        "--out".to_string(),
        out.display().to_string(),
        "--since".to_string(),
        since.to_string(),
    ];
    if let Some(until) = until {
        args.push("--until".to_string());
        args.push(until.to_string());
    }
    args
}

fn run_owned(url: &str, mode: TokenMode, args: &[String]) -> Output {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_fetch(url, mode, &refs)
}

#[test]
fn fetch_help_lists_the_options() {
    let output = run_fetch("http://127.0.0.1:9", TokenMode::None, &["fetch", "--help"]);
    assert_success(&output, &[]);
    let text = stdout_of(&output);
    assert!(text.contains("--repo"), "{text}");
    assert!(text.contains("--out"), "{text}");
    assert!(text.contains("--since"), "{text}");
    assert!(text.contains("--until"), "{text}");
}

#[test]
fn usage_errors_exit_3_without_a_connection() {
    let server = TestServer::spawn(|_, _| ok_json(repo_json("")));
    let url = server.url();
    let out = scratch("usage").join("out");

    let unknown = run_fetch(
        &url,
        TokenMode::Gh,
        &[
            "fetch",
            "--repo",
            "acme/demo",
            "--out",
            out.to_str().unwrap(),
            "--since",
            "2026-01-01",
            "--no-such-option",
        ],
    );
    assert_code(&unknown, 3);
    assert!(
        stderr_of(&unknown).contains("no-such-option"),
        "{}",
        stderr_of(&unknown)
    );

    let missing = run_fetch(&url, TokenMode::Gh, &["fetch", "--repo", "acme/demo"]);
    assert_code(&missing, 3);

    for repo in ["noslash", "acme/", "acme/demo/extra", "acme / demo", "../x"] {
        let output = run_fetch(
            &url,
            TokenMode::Gh,
            &[
                "fetch",
                "--repo",
                repo,
                "--out",
                out.to_str().unwrap(),
                "--since",
                "2026-01-01",
            ],
        );
        assert_code(&output, 3);
        assert!(
            stderr_of(&output).contains("OWNER/REPO"),
            "{repo}: {}",
            stderr_of(&output)
        );
        assert_hidden(&out, &output, &server.hits(), &[TOKEN]);
    }

    for date in ["2026-02-29", "2026-1-01", "01-01-2026", "2026-13-01"] {
        let output = run_fetch(
            &url,
            TokenMode::Gh,
            &[
                "fetch",
                "--repo",
                "acme/demo",
                "--out",
                out.to_str().unwrap(),
                "--since",
                date,
            ],
        );
        assert_code(&output, 3);
        assert!(
            stderr_of(&output).contains("YYYY-MM-DD"),
            "{date}: {}",
            stderr_of(&output)
        );
    }

    let order = run_fetch(
        &url,
        TokenMode::None,
        &[
            "fetch",
            "--repo",
            "acme/demo",
            "--out",
            out.to_str().unwrap(),
            "--since",
            "2026-01-02",
            "--until",
            "2026-01-01",
        ],
    );
    assert_code(&order, 3);
    assert!(
        stderr_of(&order).contains("--until is before --since"),
        "{}",
        stderr_of(&order)
    );

    let bad_token = Command::new(env!("CARGO_BIN_EXE_ci-bill"))
        .args([
            "fetch",
            "--repo",
            "acme/demo",
            "--out",
            out.to_str().unwrap(),
            "--since",
            "2026-01-01",
        ])
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env("GH_TOKEN", "ghs_Bad Space_9911")
        .env("GITHUB_API_URL", &url)
        .output()
        .expect("run");
    assert_code(&bad_token, 3);
    assert!(stderr_of(&bad_token).contains("set GH_TOKEN or GITHUB_TOKEN"));
    assert!(!stderr_of(&bad_token).contains("ghs_Bad Space_9911"));
    assert!(server.hits().is_empty(), "{}", targets_of(&server.hits()));
}

#[test]
fn missing_token_does_not_connect() {
    let server = TestServer::spawn(|_, _| ok_json(repo_json("")));
    let out = scratch("missing-token").join("out");
    let args = fetch_args(&out, "2026-01-01", Some("2026-01-01"));
    for mode in [TokenMode::None, TokenMode::EmptyPair] {
        server.clear();
        let output = run_owned(&server.url(), mode, &args);
        assert_code(&output, 3);
        assert!(
            stderr_of(&output).contains("set GH_TOKEN or GITHUB_TOKEN"),
            "{}",
            stderr_of(&output)
        );
        assert!(server.hits().is_empty(), "{}", targets_of(&server.hits()));
        assert!(!out.exists());
    }
}

#[test]
fn bad_api_url_exits_3_without_a_connection() {
    let server = TestServer::spawn(|_, _| ok_json(repo_json("")));
    let out = scratch("bad-url").join("out");
    let args = [
        "fetch",
        "--repo",
        "acme/demo",
        "--out",
        out.to_str().unwrap(),
        "--since",
        "2026-01-01",
    ];
    for url in ["", "ftp://example.test", "not a url"] {
        let output = run_fetch(url, TokenMode::Gh, &args);
        assert_code(&output, 3);
        assert!(
            stderr_of(&output).contains("GITHUB_API_URL must be an http or https URL"),
            "{url}: {}",
            stderr_of(&output)
        );
        assert!(!stderr_of(&output).contains(TOKEN));
    }
    assert!(server.hits().is_empty(), "{}", targets_of(&server.hits()));
}

#[test]
fn pagination_keeps_raw_pages_and_hides_the_token() {
    let server = TestServer::spawn(|hit, _| {
        if is_repo(&hit.target) {
            return ok_json(
                format!(
                    "{{\r\n  \"private\": true,\r\n  \"default_branch\": \"main\",\r\n  \"description\": \"{TOKEN}\"\r\n}}\r\n"
                )
                .into_bytes(),
            );
        }
        if is_runs(&hit.target) {
            assert_eq!(header(hit, "accept"), "application/vnd.github+json");
            assert!(hit.target.contains("per_page=100"), "{}", hit.target);
            assert_eq!(created_of(&hit.target), "2026-01-01..2026-01-01");
            if hit.target.contains("page=2") {
                return ok_json(
                    b"{\"workflow_runs\":[{\"id\":150,\"event\":\"push\",\"path\":\"logs/build\",\"run_attempt\":1,\"run_number\":8,\"conclusion\":\"success\",\"run_started_at\":\"2026-01-01T00:00:00Z\"}],\"total_count\":2}".to_vec(),
                );
            }
            let body_text = format!(
                "{{\r\n  \"workflow_runs\": [\r\n    {{\r\n      \"event\": \"PUSH\",\r\n      \"path\": \"logs/build\",\r\n      \"run_attempt\": 1,\r\n      \"run_number\": 7,\r\n      \"id\": 15,\r\n      \"conclusion\": null,\r\n      \"note\": \"{TOKEN}\"\r\n    }}\r\n  ],\r\n  \"total_count\": 2\r\n}}\r\n"
            );
            let mut body = vec![0xEF, 0xBB, 0xBF];
            body.extend(body_text.into_bytes());
            let host = header(hit, "host");
            let next = format!(
                "http://{host}/repos/acme/demo/actions/runs?per_page=100&created=2026-01-01..2026-01-01&page=2"
            );
            return json_reply(
                200,
                body,
                vec![
                    (
                        "Link".to_string(),
                        format!("<http://{host}/repos/acme/demo/actions/runs?per_page=100&page=9>; rel=\"last\""),
                    ),
                    ("Link".to_string(), format!("<{next}>; rel=\"NEXT\"")),
                ],
            );
        }
        if is_jobs(&hit.target) {
            assert!(hit.target.contains("filter=all"), "{}", hit.target);
            assert!(hit.target.contains("per_page=100"), "{}", hit.target);
            assert_eq!(header(hit, "accept"), "application/vnd.github+json");
            if hit.target.contains("/runs/15/jobs") && !hit.target.contains("page=2") {
                let jobs = format!(
                    "{{\"jobs\":[{{\"name\":\"lint\",\"run_id\":15,\"run_attempt\":1,\"id\":151,\"labels\":[],\"completed_at\":null,\"conclusion\":\"\"}},\r\n{{\"name\":\"test\",\"run_attempt\":1,\"run_id\":15,\"id\":152,\"runner_name\":\"{TOKEN}\"}}]}}"
                );
                let host = header(hit, "host");
                return json_reply(
                    200,
                    jobs.into_bytes(),
                    vec![(
                        "Link".to_string(),
                        format!(
                            "</repos/acme/demo/actions/runs/15/jobs?filter=all&per_page=100&page=2>; rel=\"next\", <http://{host}/last>; rel=\"last\""
                        ),
                    )],
                );
            }
            if hit.target.contains("/runs/15/jobs") {
                return ok_json(jobs_page(vec![json!({
                    "id": 153,
                    "run_id": 15,
                    "run_attempt": 1,
                    "name": "pack",
                    "labels": ["UBUNTU-LATEST"],
                    "started_at": null
                })]));
            }
            if hit.target.contains("/runs/150/jobs") {
                return ok_json(
                    serde_json::to_vec(&json!({
                        "jobs": [
                            {
                                "id": 1501,
                                "run_id": 150,
                                "run_attempt": 1,
                                "name": "Build",
                                "completed_at": null
                            }
                        ]
                    }))
                    .expect("job"),
                );
            }
        }
        panic!("unexpected {}", hit.target);
    });

    let out = scratch("pagination").join("out");
    let args = fetch_args(&out, "2026-01-01", Some("2026-01-01"));
    let url = format!("{}/", server.url());
    let output = run_owned(&url, TokenMode::Gh, &args);
    let hits = server.hits();
    assert_success(&output, &hits);
    assert_auth(&hits, TOKEN);
    let text = stdout_of(&output);
    assert!(
        text.contains("acme/demo 2026-01-01 .. 2026-01-01\n"),
        "{text}"
    );
    assert!(text.contains("runs 2\n"), "{text}");
    assert!(text.contains("jobs 4\n"), "{text}");
    assert!(text.contains("workflow files 0\n"), "{text}");

    let runs1 = fs::read(out.join("runs-1.json")).expect("runs-1");
    assert!(runs1.starts_with(b"\xEF\xBB\xBF"));
    let runs1_text = String::from_utf8_lossy(&runs1);
    assert!(runs1_text.contains("\r\n"), "first runs page keeps CRLF");
    let path_at = runs1_text.find("\"path\"").expect("path");
    let id_at = runs1_text.find("\"id\"").expect("id");
    assert!(path_at < id_at, "path stays before id");
    assert!(runs1_text.contains("[redacted]"));
    assert!(runs1_text.contains("PUSH"));

    let runs2 = fs::read_to_string(out.join("runs-2.json")).expect("runs-2");
    assert!(!runs2.contains('\r'));
    assert!(runs2.find("\"id\"").unwrap() < runs2.find("\"event\"").unwrap());
    assert!(runs2.contains("\"event\":\"push\""));

    let jobs15 = fs::read_to_string(out.join("jobs-15.json")).expect("jobs-15");
    assert!(jobs15.contains("\r\n"));
    assert!(jobs15.contains("[redacted]"));
    assert!(jobs15.contains("\"labels\":[]"));
    assert!(jobs15.find("\"name\":\"lint\"").unwrap() < jobs15.find("\"id\":151").unwrap());
    let jobs152 = saved_json(&out.join("jobs-15-2.json"));
    assert_eq!(jobs152["jobs"][0]["labels"][0], "UBUNTU-LATEST");
    assert!(jobs152["jobs"][0].get("completed_at").is_none());
    let jobs150 = saved_json(&out.join("jobs-150.json"));
    assert!(jobs150["jobs"][0].get("labels").is_none());
    assert!(jobs150["jobs"][0]["completed_at"].is_null());
    assert_eq!(jobs150["jobs"][0]["name"], "Build");

    let repo = fs::read_to_string(out.join("repo.json")).expect("repo");
    assert!(repo.contains("\r\n"));
    assert!(repo.contains("\"private\": true"));
    assert!(repo.contains("[redacted]"));
    assert_hidden(&out, &output, &hits, &[TOKEN]);
    assert!(hits.iter().any(|hit| hit.target.contains("page=2")));
    assert!(hits.iter().filter(|hit| is_jobs(&hit.target)).count() >= 3);
    assert_report_reads(&out);
}

#[test]
fn total_count_below_1000_follows_the_next_link() {
    let server = TestServer::spawn(|hit, _| {
        if is_repo(&hit.target) {
            return ok_json(repo_json(""));
        }
        if is_runs(&hit.target) {
            assert_eq!(created_of(&hit.target), "2026-01-01..2026-01-02");
            assert!(!created_of(&hit.target).contains('T'), "{}", hit.target);
            if hit.target.contains("page=2") {
                return ok_json(
                    serde_json::to_vec(&runs_page(
                        None,
                        vec![run_value(2, 1, "logs/second", "push")],
                    ))
                    .expect("page"),
                );
            }
            let host = header(hit, "host");
            return json_reply(
                200,
                serde_json::to_vec(&runs_page(
                    Some(999),
                    vec![run_value(1, 1, "logs/first", "push")],
                ))
                .expect("page"),
                vec![(
                    "Link".to_string(),
                    format!("<http://{host}/repos/acme/demo/actions/runs?per_page=100&created=2026-01-01..2026-01-02&page=2>; rel=\"next\""),
                )],
            );
        }
        if is_jobs(&hit.target) {
            let id = job_run_id(&hit.target);
            return one_job(id, 1);
        }
        panic!("unexpected {}", hit.target);
    });
    let out = scratch("below-1000").join("out");
    let output = run_owned(
        &server.url(),
        TokenMode::Gh,
        &fetch_args(&out, "2026-01-01", Some("2026-01-02")),
    );
    let hits = server.hits();
    assert_success(&output, &hits);
    assert!(
        stdout_of(&output).contains("runs 2\n"),
        "{}",
        stdout_of(&output)
    );
    assert!(saved_json(&out.join("runs-1.json"))["workflow_runs"][0]["id"] == 1);
    assert!(saved_json(&out.join("runs-2.json"))["workflow_runs"][0]["id"] == 2);
    assert!(hits
        .iter()
        .all(|hit| !created_of(&hit.target).contains('T')));
    assert_hidden(&out, &output, &hits, &[TOKEN]);
}

#[test]
fn a_window_at_1000_is_split_into_days() {
    let server = TestServer::spawn(|hit, _| {
        if is_repo(&hit.target) {
            return ok_json(repo_json(""));
        }
        if is_jobs(&hit.target) {
            assert!(!hit.target.contains("/runs/424242/"), "{}", hit.target);
            return one_job(job_run_id(&hit.target), 1);
        }
        if is_runs(&hit.target) {
            match created_of(&hit.target).as_str() {
                "2026-01-01..2026-01-02" => {
                    return json_reply(
                        200,
                        serde_json::to_vec(&json!({
                            "total_count": 1000,
                            "workflow_runs": [{
                                "id": 424242,
                                "event": "push",
                                "path": "wide.yml",
                                "run_attempt": 1,
                                "run_number": 1,
                                "note": "WIDEWINDOW"
                            }]
                        }))
                        .expect("wide"),
                        vec![("Link".to_string(), "</poison>; rel=\"next\"".to_string())],
                    );
                }
                "2026-01-01..2026-01-01" => {
                    return ok_json(
                        serde_json::to_vec(&runs_page(
                            Some(1),
                            vec![run_value(11, 1, "logs/day-one", "push")],
                        ))
                        .expect("left"),
                    );
                }
                "2026-01-02..2026-01-02" => {
                    return ok_json(
                        serde_json::to_vec(&runs_page(
                            Some(1),
                            vec![run_value(12, 1, "logs/day-two", "push")],
                        ))
                        .expect("right"),
                    );
                }
                other => panic!("created {other} from {}", hit.target),
            }
        }
        panic!("unexpected {}", hit.target);
    });
    let out = scratch("split-1000").join("out");
    let output = run_owned(
        &server.url(),
        TokenMode::Gh,
        &fetch_args(&out, "2026-01-01", Some("2026-01-02")),
    );
    let hits = server.hits();
    assert_success(&output, &hits);
    assert!(
        stdout_of(&output).contains("runs 2\n"),
        "{}",
        stdout_of(&output)
    );
    let blob = read_tree(&out);
    assert!(!blob.contains("WIDEWINDOW"));
    assert!(!blob.contains("424242"));
    assert!(blob.contains("logs/day-one"));
    assert!(blob.contains("logs/day-two"));
    assert!(hits
        .iter()
        .any(|hit| created_of(&hit.target) == "2026-01-01..2026-01-01"));
    assert!(hits
        .iter()
        .any(|hit| created_of(&hit.target) == "2026-01-02..2026-01-02"));
    assert!(hits.iter().all(|hit| !hit.target.contains("poison")));
    assert!(hits.iter().all(|hit| !hit.target.contains("/runs/424242/")));
    assert_hidden(&out, &output, &hits, &[TOKEN]);
}

#[test]
fn a_three_day_window_splits_on_a_fractional_count() {
    let left = "2026-01-01T00:00:00Z..2026-01-02T11:59:59Z";
    let right = "2026-01-02T12:00:00Z..2026-01-03T23:59:59Z";
    let server = TestServer::spawn(move |hit, _| {
        if is_repo(&hit.target) {
            return ok_json(repo_json(""));
        }
        if is_jobs(&hit.target) {
            return one_job(job_run_id(&hit.target), 1);
        }
        if is_runs(&hit.target) {
            match created_of(&hit.target).as_str() {
                "2026-01-01..2026-01-03" => {
                    return ok_json(
                        serde_json::to_vec(&json!({
                            "total_count": 1000.0,
                            "workflow_runs": [{
                                "id": 424242,
                                "event": "push",
                                "path": "wide.yml",
                                "run_attempt": 1,
                                "run_number": 1,
                                "note": "WIDEWINDOW"
                            }]
                        }))
                        .expect("wide"),
                    );
                }
                value if value == left => {
                    return ok_json(
                        serde_json::to_vec(&runs_page(
                            Some(1),
                            vec![run_value(21, 1, "logs/left", "Push")],
                        ))
                        .expect("left"),
                    );
                }
                value if value == right => {
                    return ok_json(
                        serde_json::to_vec(&runs_page(
                            Some(1),
                            vec![run_value(22, 1, "logs/right", "push")],
                        ))
                        .expect("right"),
                    );
                }
                other => panic!("created {other} from {}", hit.target),
            }
        }
        panic!("unexpected {}", hit.target);
    });
    let out = scratch("split-3day").join("out");
    let output = run_owned(
        &server.url(),
        TokenMode::Gh,
        &fetch_args(&out, "2026-01-01", Some("2026-01-03")),
    );
    let hits = server.hits();
    assert_success(&output, &hits);
    let blob = read_tree(&out);
    assert!(!blob.contains("WIDEWINDOW"));
    assert!(blob.contains("logs/left"));
    assert!(blob.contains("logs/right"));
    assert!(blob.contains("Push"));
    assert!(hits.iter().any(|hit| created_of(&hit.target) == left));
    assert!(hits.iter().any(|hit| created_of(&hit.target) == right));
    assert_eq!(stdout_of(&output).matches("runs 2\n").count(), 1);
    assert_hidden(&out, &output, &hits, &[TOKEN]);
}

#[test]
fn a_leap_day_is_sent_as_a_real_date() {
    let server = TestServer::spawn(|hit, _| {
        if is_repo(&hit.target) {
            return ok_json(b"{\"private\":false}\n".to_vec());
        }
        if is_runs(&hit.target) {
            assert_eq!(created_of(&hit.target), "2024-02-29..2024-02-29");
            return empty_runs();
        }
        panic!("unexpected {}", hit.target);
    });
    let out = scratch("leap").join("out");
    let output = run_owned(
        &server.url(),
        TokenMode::Gh,
        &fetch_args(&out, "2024-02-29", Some("2024-02-29")),
    );
    let hits = server.hits();
    assert_success(&output, &hits);
    assert!(fs::read_to_string(out.join("repo.json"))
        .expect("repo")
        .contains("\"private\":false"));
    assert!(stdout_of(&output).contains("runs 0\n"));
    assert_hidden(&out, &output, &hits, &[TOKEN]);
}

#[test]
fn primary_limit_waits_until_reset() {
    assert_rate_wait(
        "primary",
        Duration::from_millis(1500),
        Duration::from_secs(12),
        |hit, earlier| {
            if earlier == 0 {
                assert!(is_repo(&hit.target));
                return json_reply(
                    403,
                    b"{\"message\":\"api rate limit exceeded\"}".to_vec(),
                    vec![
                        ("X-RateLimit-Remaining".to_string(), "0".to_string()),
                        (
                            "X-RateLimit-Reset".to_string(),
                            format!("{}", unix_now() + 2),
                        ),
                    ],
                );
            }
            quiet_success(hit)
        },
    );
}

#[test]
fn primary_limit_wins_when_the_body_also_says_secondary() {
    assert_rate_wait(
        "primary-wins",
        Duration::from_millis(1500),
        Duration::from_secs(12),
        |hit, earlier| {
            if earlier == 0 {
                return json_reply(
                    403,
                    b"{\"message\":\"secondary rate limit\"}".to_vec(),
                    vec![
                        ("X-RateLimit-Remaining".to_string(), " 0 ".to_string()),
                        (
                            "X-RateLimit-Reset".to_string(),
                            format!("{}", unix_now() + 2),
                        ),
                        ("Retry-After".to_string(), "30".to_string()),
                    ],
                );
            }
            quiet_success(hit)
        },
    );
}

#[test]
fn secondary_limit_honors_retry_after() {
    assert_rate_wait(
        "retry-after",
        Duration::from_millis(800),
        Duration::from_secs(15),
        |hit, earlier| {
            if earlier == 0 {
                return json_reply(
                    403,
                    format!("{{\"message\":\"Secondary Rate Limit\",\"echo\":\"{TOKEN}\"}}")
                        .into_bytes(),
                    vec![
                        ("X-RateLimit-Remaining".to_string(), "50".to_string()),
                        ("RETRY-AFTER".to_string(), "1".to_string()),
                    ],
                );
            }
            quiet_success(hit)
        },
    );
}

#[test]
fn secondary_status_429_honors_retry_after() {
    assert_rate_wait(
        "retry-429",
        Duration::from_millis(800),
        Duration::from_secs(15),
        |hit, earlier| {
            if earlier == 0 {
                return json_reply(
                    429,
                    b"too many requests".to_vec(),
                    vec![("Retry-After".to_string(), "1".to_string())],
                );
            }
            quiet_success(hit)
        },
    );
}

#[test]
fn secondary_limit_honors_an_http_date() {
    assert_rate_wait(
        "http-date",
        Duration::from_millis(800),
        Duration::from_secs(15),
        |hit, earlier| {
            if earlier == 0 {
                let when = http_date(unix_now() + 1);
                return json_reply(
                    429,
                    b"secondary rate limit".to_vec(),
                    vec![("Retry-After".to_string(), when)],
                );
            }
            quiet_success(hit)
        },
    );
}

#[test]
fn secondary_limit_without_headers_waits_one_minute() {
    assert_rate_wait(
        "secondary-bare",
        Duration::from_secs(59),
        Duration::from_secs(90),
        |hit, earlier| {
            if earlier == 0 {
                return json_reply(
                    403,
                    format!("You have exceeded a SECONDARY RATE LIMIT. {TOKEN}").into_bytes(),
                    Vec::new(),
                );
            }
            quiet_success(hit)
        },
    );
}

fn quiet_success(hit: &Hit) -> Reply {
    if is_repo(&hit.target) {
        return ok_json(format!(
            "{{\"private\":true,\"description\":\"{TOKEN}\"}}\n"
        ));
    }
    if is_runs(&hit.target) {
        return empty_runs();
    }
    panic!("unexpected {}", hit.target);
}

fn assert_rate_wait<F>(name: &str, low: Duration, high: Duration, handler: F)
where
    F: Fn(&Hit, usize) -> Reply + Send + Sync + 'static,
{
    let server = TestServer::spawn(handler);
    let out = scratch(name).join("out");
    let output = run_owned(
        &server.url(),
        TokenMode::Gh,
        &fetch_args(&out, "2026-01-01", Some("2026-01-01")),
    );
    let hits = server.hits();
    assert_success(&output, &hits);
    assert!(hits.len() >= 2, "{}", targets_of(&hits));
    let waited = hits[1].at.saturating_duration_since(hits[0].at);
    assert!(
        waited >= low && waited < high,
        "waited {waited:?} for {name}, hits:\n{}",
        targets_of(&hits)
    );
    assert_hidden(&out, &output, &hits, &[TOKEN]);
    assert!(fs::read_to_string(out.join("repo.json"))
        .expect("repo")
        .contains("[redacted]"));
    assert_auth(&hits, TOKEN);
}

#[test]
fn permission_denied_exits_2_and_leaves_the_directory() {
    let server = TestServer::spawn(|hit, _| {
        if is_repo(&hit.target) {
            return ok_json(repo_json(&format!(",\"description\":\"{TOKEN}\"")));
        }
        if is_runs(&hit.target) {
            return ok_json(
                serde_json::to_vec(&runs_page(
                    Some(1),
                    vec![run_value(5, 1, "logs/build", "push")],
                ))
                .expect("runs"),
            );
        }
        if is_jobs(&hit.target) {
            return json_reply(
                403,
                format!("{{\"message\":\"Resource not accessible by integration\",\"echo\":\"{TOKEN}\"}}")
                    .into_bytes(),
                Vec::new(),
            );
        }
        panic!("unexpected {}", hit.target);
    });
    let out = scratch("permission").join("out");
    seed_keep(&out);
    let output = run_owned(
        &server.url(),
        TokenMode::Gh,
        &fetch_args(&out, "2026-01-01", Some("2026-01-01")),
    );
    let hits = server.hits();
    assert_code(&output, 2);
    let err = stderr_of(&output);
    assert!(err.contains("actions:read"), "{err}");
    assert!(err.contains("repo"), "{err}");
    assert!(!err.contains(TOKEN), "{err}");
    assert_eq!(hits.len(), 3, "{}", targets_of(&hits));
    assert_kept(&out);
    assert_hidden(&out, &output, &hits, &[TOKEN]);
}

#[test]
fn unauthorized_exits_2_and_leaves_the_directory() {
    let server = TestServer::spawn(|hit, _| {
        assert!(is_repo(&hit.target), "{}", hit.target);
        json_reply(
            401,
            format!("{{\"message\":\"Bad credentials\",\"echo\":\"{TOKEN}\"}}").into_bytes(),
            Vec::new(),
        )
    });
    let out = scratch("unauthorized").join("out");
    seed_keep(&out);
    let output = run_owned(
        &server.url(),
        TokenMode::Gh,
        &fetch_args(&out, "2026-01-01", Some("2026-01-01")),
    );
    let hits = server.hits();
    assert_code(&output, 2);
    let err = stderr_of(&output);
    assert!(err.contains("unauthorized"), "{err}");
    assert!(!err.contains(TOKEN));
    assert_eq!(hits.len(), 1, "{}", targets_of(&hits));
    assert_kept(&out);
    assert_hidden(&out, &output, &hits, &[TOKEN]);
}

#[test]
fn a_server_error_is_returned_without_a_long_wait() {
    let server =
        TestServer::spawn(|_, _| json_reply(500, b"{\"message\":\"boom\"}".to_vec(), Vec::new()));
    let started = Instant::now();
    let out = scratch("http-500").join("out");
    seed_keep(&out);
    let output = run_owned(
        &server.url(),
        TokenMode::Gh,
        &fetch_args(&out, "2026-01-01", Some("2026-01-01")),
    );
    assert_code(&output, 2);
    assert!(
        stderr_of(&output).contains("HTTP 500"),
        "{}",
        stderr_of(&output)
    );
    assert!(started.elapsed() < Duration::from_secs(20));
    assert_eq!(server.hits().len(), 1, "{}", targets_of(&server.hits()));
    assert_kept(&out);
}

#[test]
fn invalid_runs_json_writes_nothing() {
    let server = TestServer::spawn(|hit, _| {
        if is_repo(&hit.target) {
            return ok_json(repo_json(&format!(",\"description\":\"{TOKEN}\"")));
        }
        if is_runs(&hit.target) {
            return ok_json(b"not-json".to_vec());
        }
        panic!("unexpected {}", hit.target);
    });
    let out = scratch("bad-json").join("out");
    seed_keep(&out);
    let output = run_owned(
        &server.url(),
        TokenMode::Gh,
        &fetch_args(&out, "2026-01-01", Some("2026-01-01")),
    );
    assert_code(&output, 2);
    assert!(
        stderr_of(&output).contains("invalid JSON"),
        "{}",
        stderr_of(&output)
    );
    assert!(!stderr_of(&output).contains(TOKEN));
    assert_kept(&out);
}

#[test]
fn a_missing_repository_exits_2() {
    let server = TestServer::spawn(|_, _| {
        json_reply(404, b"{\"message\":\"Not Found\"}".to_vec(), Vec::new())
    });
    let out = scratch("missing-repo").join("out");
    seed_keep(&out);
    let output = run_owned(
        &server.url(),
        TokenMode::Gh,
        &fetch_args(&out, "2026-01-01", Some("2026-01-01")),
    );
    assert_code(&output, 2);
    assert!(
        stderr_of(&output).contains("repository not found"),
        "{}",
        stderr_of(&output)
    );
    assert_kept(&out);
    assert_eq!(server.hits().len(), 1);
}

#[test]
fn an_empty_run_list_still_writes_repo_json() {
    let server = TestServer::spawn(|hit, _| {
        if is_repo(&hit.target) {
            return ok_json(b"{\"private\": false,\"default_branch\":\"main\"}\n".to_vec());
        }
        if is_runs(&hit.target) {
            return ok_json(b"{\"total_count\":0,\"workflow_runs\":[]}\n".to_vec());
        }
        panic!("unexpected {}", hit.target);
    });
    let out = scratch("empty-runs").join("out");
    let output = run_owned(
        &server.url(),
        TokenMode::Gh,
        &fetch_args(&out, "2026-01-01", Some("2026-01-01")),
    );
    let hits = server.hits();
    assert_success(&output, &hits);
    assert!(stdout_of(&output).contains("runs 0\n"));
    assert!(stdout_of(&output).contains("jobs 0\n"));
    assert_eq!(
        fs::read(out.join("repo.json")).expect("repo"),
        b"{\"private\": false,\"default_branch\":\"main\"}\n"
    );
    assert!(hits.iter().all(|hit| !is_jobs(&hit.target)));
    assert_report_reads(&out);
}

#[test]
fn omitted_until_ends_on_the_utc_day() {
    let server = TestServer::spawn(|hit, _| {
        if is_repo(&hit.target) {
            return ok_json(repo_json(""));
        }
        if is_runs(&hit.target) {
            return empty_runs();
        }
        panic!("unexpected {}", hit.target);
    });
    let out = scratch("until-today").join("out");
    let output = run_owned(
        &server.url(),
        TokenMode::Gh,
        &fetch_args(&out, "2020-01-01", None),
    );
    let hits = server.hits();
    assert_success(&output, &hits);
    let now = unix_now();
    let today = utc_date_string(now);
    let tomorrow = utc_date_string(now + 86400);
    let created = hits
        .iter()
        .map(|hit| created_of(&hit.target))
        .find(|value| value.starts_with("2020-01-01.."))
        .expect("created");
    assert!(
        created == format!("2020-01-01..{today}") || created == format!("2020-01-01..{tomorrow}"),
        "{created}"
    );
    let text = stdout_of(&output);
    assert!(
        text.contains(&format!("2020-01-01 .. {today}"))
            || text.contains(&format!("2020-01-01 .. {tomorrow}")),
        "{text}"
    );
}

#[test]
fn resume_does_not_fetch_jobs_for_the_same_attempt() {
    let server = TestServer::spawn(|hit, _| script_attempt(hit, 1, "lint", 1));
    let out = scratch("resume").join("out");
    let args = fetch_args(&out, "2026-01-01", Some("2026-01-01"));
    let first = run_owned(&server.url(), TokenMode::Gh, &args);
    assert_success(&first, &server.hits());
    let jobs = fs::read(out.join("jobs-42.json")).expect("jobs");
    assert!(String::from_utf8_lossy(&jobs).contains("lint"));
    server.clear();
    let second = run_owned(&server.url(), TokenMode::Gh, &args);
    let hits = server.hits();
    assert_success(&second, &hits);
    assert!(
        hits.iter().all(|hit| !is_jobs(&hit.target)),
        "jobs were requested again:\n{}",
        targets_of(&hits)
    );
    assert!(hits.iter().any(|hit| is_runs(&hit.target)));
    assert_eq!(fs::read(out.join("jobs-42.json")).expect("jobs"), jobs);
    assert_hidden(&out, &second, &hits, &[TOKEN]);
    assert_report_reads(&out);
}

#[test]
fn an_empty_job_list_is_enough_for_resume() {
    let server = TestServer::spawn(|hit, _| {
        if is_repo(&hit.target) {
            return ok_json(repo_json(""));
        }
        if is_runs(&hit.target) {
            return ok_json(
                serde_json::to_vec(&runs_page(
                    Some(1),
                    vec![run_value(77, 1, "logs/empty", "push")],
                ))
                .expect("runs"),
            );
        }
        if is_jobs(&hit.target) {
            return ok_json(b"{\"jobs\":[]}".to_vec());
        }
        panic!("unexpected {}", hit.target);
    });
    let out = scratch("empty-jobs").join("out");
    let args = fetch_args(&out, "2026-01-01", Some("2026-01-01"));
    let first = run_owned(&server.url(), TokenMode::Gh, &args);
    assert_success(&first, &server.hits());
    assert_eq!(
        fs::read(out.join("jobs-77.json")).expect("jobs"),
        b"{\"jobs\":[]}"
    );
    server.clear();
    let second = run_owned(&server.url(), TokenMode::Gh, &args);
    let hits = server.hits();
    assert_success(&second, &hits);
    assert!(
        hits.iter().all(|hit| !is_jobs(&hit.target)),
        "{}",
        targets_of(&hits)
    );
    assert_eq!(
        fs::read(out.join("jobs-77.json")).expect("jobs"),
        b"{\"jobs\":[]}"
    );
}

#[test]
fn a_higher_attempt_fetches_jobs_again() {
    let attempt = Arc::new(Mutex::new(1u64));
    let attempt_server = Arc::clone(&attempt);
    let server = TestServer::spawn(move |hit, _| {
        let current = *attempt_server.lock().expect("attempt");
        let name = if current == 1 { "first" } else { "second" };
        let job_id = if current == 1 { 1 } else { 2 };
        script_attempt(hit, current, name, job_id)
    });
    let out = scratch("higher-attempt").join("out");
    let args = fetch_args(&out, "2026-01-01", Some("2026-01-01"));
    let first = run_owned(&server.url(), TokenMode::Gh, &args);
    assert_success(&first, &server.hits());
    assert!(fs::read_to_string(out.join("jobs-42.json"))
        .expect("jobs")
        .contains("first"));
    *attempt.lock().expect("attempt") = 2;
    server.clear();
    let second = run_owned(&server.url(), TokenMode::Gh, &args);
    let hits = server.hits();
    assert_success(&second, &hits);
    assert!(
        hits.iter().any(|hit| is_jobs(&hit.target)),
        "jobs were not requested:\n{}",
        targets_of(&hits)
    );
    let jobs = fs::read_to_string(out.join("jobs-42.json")).expect("jobs");
    assert!(jobs.contains("second"), "{jobs}");
    assert!(jobs.contains("[redacted]"), "{jobs}");
    assert!(!jobs.contains("first"), "{jobs}");
    assert!(!out.join("jobs-42-2.json").exists());
    let runs = saved_json(&out.join("runs-1.json"));
    let ids: Vec<u64> = runs["workflow_runs"]
        .as_array()
        .expect("runs")
        .iter()
        .map(|run| run["id"].as_u64().expect("id"))
        .collect();
    assert_eq!(ids, vec![42]);
    assert_eq!(runs["workflow_runs"][0]["run_attempt"], 2);
    assert_hidden(&out, &second, &hits, &[TOKEN]);
    assert_report_reads(&out);
}

#[test]
fn duplicate_ids_in_one_fetch_keep_the_higher_attempt() {
    let server = TestServer::spawn(|hit, _| {
        if is_repo(&hit.target) {
            return ok_json(repo_json(""));
        }
        if is_runs(&hit.target) && !hit.target.contains("page=2") {
            let host = header(hit, "host");
            return json_reply(
                200,
                serde_json::to_vec(&runs_page(
                    Some(2),
                    vec![run_value(5, 1, "logs/build", "push")],
                ))
                .expect("page"),
                vec![(
                    "Link".to_string(),
                    format!("<http://{host}/repos/acme/demo/actions/runs?per_page=100&created=2026-01-01..2026-01-01&page=2>; rel=\"next\""),
                )],
            );
        }
        if is_runs(&hit.target) {
            return ok_json(
                serde_json::to_vec(&runs_page(
                    Some(2),
                    vec![json!({
                        "id": 5,
                        "path": "logs/build",
                        "run_attempt": 3,
                        "run_number": 1,
                        "event": "push",
                        "note": TOKEN
                    })],
                ))
                .expect("page"),
            );
        }
        if is_jobs(&hit.target) {
            assert!(hit.target.contains("/runs/5/jobs"), "{}", hit.target);
            return ok_json(jobs_page(vec![job_value(9, 5, 3, "once")]));
        }
        panic!("unexpected {}", hit.target);
    });
    let out = scratch("duplicate-id").join("out");
    let output = run_owned(
        &server.url(),
        TokenMode::Gh,
        &fetch_args(&out, "2026-01-01", Some("2026-01-01")),
    );
    let hits = server.hits();
    assert_success(&output, &hits);
    assert!(
        stdout_of(&output).contains("runs 1\n"),
        "{}",
        stdout_of(&output)
    );
    let runs = saved_json(&out.join("runs-1.json"));
    assert_eq!(runs["workflow_runs"].as_array().expect("runs").len(), 1);
    assert_eq!(runs["workflow_runs"][0]["run_attempt"], 3);
    assert!(runs.to_string().contains("[redacted]"));
    assert!(!out.join("runs-2.json").exists());
    assert_eq!(
        hits.iter().filter(|hit| is_jobs(&hit.target)).count(),
        1,
        "{}",
        targets_of(&hits)
    );
    assert_hidden(&out, &output, &hits, &[TOKEN]);
    assert_report_reads(&out);
}

#[test]
fn callee_workflows_are_stored_on_the_cache_path() {
    let sha = "a".repeat(40);
    let sha_for_handler = sha.clone();
    let server = TestServer::spawn(move |hit, _| {
        let sha = sha_for_handler.as_str();
        if is_repo(&hit.target) {
            return ok_json(format!(
                "{{\"private\":true,\"default_branch\":\"trunk\",\"note\":\"{TOKEN}\"}}\n"
            ));
        }
        if is_runs(&hit.target) {
            return ok_json(
                serde_json::to_vec(&runs_page(
                    Some(3),
                    vec![
                        json!({
                            "id": 7,
                            "path": ".github/workflows/ci.yml@main",
                            "run_attempt": 1,
                            "run_number": 1,
                            "event": "push",
                            "head_sha": sha
                        }),
                        json!({
                            "id": 8,
                            "path": "docs/readme.md",
                            "run_attempt": 1,
                            "run_number": 2,
                            "event": "push"
                        }),
                        json!({
                            "id": 9,
                            "path": ".github/workflows/nightly.yml",
                            "run_attempt": 1,
                            "run_number": 3,
                            "event": "schedule",
                            "conclusion": null
                        }),
                    ],
                ))
                .expect("runs"),
            );
        }
        if is_jobs(&hit.target) {
            return one_job(job_run_id(&hit.target), 1);
        }
        let path = path_only(&hit.target);
        if path.ends_with("/contents/.github/workflows/ci.yml") {
            assert!(hit.target.contains(&format!("ref={sha}")), "{}", hit.target);
            assert_eq!(header(hit, "accept"), "application/vnd.github.raw");
            let body = format!(
                "name: CI\r\njobs:\r\n  lint:\r\n    uses: other/lib/.github/workflows/lint.yml@v1\r\n  bad:\r\n    uses: not a workflow\r\n  again:\r\n    uses: Other/Lib/.github/workflows/Lint.yml@V1\r\n  broken:\r\n    uses: fourth/lib/.github/workflows/bad.yml@v1\r\n  missing:\r\n    uses: ./.github/workflows/missing.yml\r\n  steps:\r\n    runs-on: ubuntu-latest\r\n    steps:\r\n      - uses: actions/checkout@v4\r\n# {TOKEN}\r\n"
            );
            return raw_reply(body.into_bytes());
        }
        if path.ends_with("/contents/.github/workflows/nightly.yml") {
            assert!(hit.target.contains("ref=trunk"), "{}", hit.target);
            return raw_reply(
                b"name: NightlyCallee\njobs:\n  build:\n    runs-on: ubuntu-latest\n".to_vec(),
            );
        }
        if path.ends_with("/contents/.github/workflows/missing.yml") {
            return json_reply(404, b"{\"message\":\"Not Found\"}".to_vec(), Vec::new());
        }
        if path.contains("/repos/other/lib/contents/.github/workflows/lint.yml") {
            assert!(hit.target.contains("ref=v1"), "{}", hit.target);
            let yaml = format!(
                "name: LintCallee\njobs:\n  more:\n    uses: ./.github/workflows/more.yml\n  box:\n    uses: third/box/.github/workflows/build.yml@release/1.2\n  cycle:\n    uses: acme/demo/.github/workflows/ci.yml@{sha}\n"
            );
            let wrapped = wrap_base64(&base64_encode(yaml.as_bytes()));
            return ok_json(
                serde_json::to_vec(&json!({
                    "type": "file",
                    "encoding": "BASE64",
                    "content": wrapped,
                    "sha": "abc"
                }))
                .expect("envelope"),
            );
        }
        if path.contains("/repos/other/lib/contents/.github/workflows/more.yml") {
            assert!(hit.target.contains("ref=v1"), "{}", hit.target);
            return raw_reply(
                b"name: MoreCallee\njobs:\n  build:\n    runs-on: ubuntu-latest\n".to_vec(),
            );
        }
        if path.contains("/repos/Other/Lib/contents/.github/workflows/Lint.yml") {
            assert!(hit.target.contains("ref=V1"), "{}", hit.target);
            return raw_reply(b"name: CaseCallee\n".to_vec());
        }
        if path.contains("/repos/fourth/lib/contents/.github/workflows/bad.yml") {
            return raw_reply(b":\n  - [\n".to_vec());
        }
        if path.contains("/repos/third/box/contents/.github/workflows/build.yml") {
            let host = header(hit, "host");
            return ok_json(
                format!(
                    "{{\"encoding\":\"none\",\"content\":null,\"download_url\":\"http://{host}/raw/build.yml\"}}"
                )
                .into_bytes(),
            );
        }
        if path == "/raw/build.yml" {
            assert_eq!(header(hit, "authorization"), format!("Bearer {TOKEN}"));
            return raw_reply(format!("name: BuildCallee\n# {TOKEN}\n").into_bytes());
        }
        panic!("unexpected {}", hit.target);
    });
    let out = scratch("callees").join("out");
    let output = run_owned(
        &server.url(),
        TokenMode::Gh,
        &fetch_args(&out, "2026-01-01", Some("2026-01-01")),
    );
    let hits = server.hits();
    assert_success(&output, &hits);
    assert!(
        stdout_of(&output).contains("workflow files 7\n"),
        "{}",
        stdout_of(&output)
    );
    let ci = out.join(format!("acme/demo/.github/workflows/ci.yml@{sha}"));
    let ci_text = fs::read_to_string(&ci).expect("ci");
    assert!(ci_text.contains("\r\n"));
    assert!(ci_text.contains("[redacted]"));
    assert!(ci_text.contains("other/lib/.github/workflows/lint.yml@v1"));
    let lint =
        fs::read_to_string(out.join("other/lib/.github/workflows/lint.yml@v1")).expect("lint");
    assert!(lint.contains("name: LintCallee"), "{lint}");
    assert!(lint.contains("./.github/workflows/more.yml"), "{lint}");
    assert!(!lint.contains("bmFtZQ"));
    assert_eq!(
        fs::read_to_string(out.join("other/lib/.github/workflows/more.yml@v1")).expect("more"),
        "name: MoreCallee\njobs:\n  build:\n    runs-on: ubuntu-latest\n"
    );
    let build = fs::read_to_string(out.join("third/box/.github/workflows/build.yml@release/1.2"))
        .expect("build");
    assert!(build.contains("name: BuildCallee"), "{build}");
    assert!(build.contains("[redacted]"), "{build}");
    assert_eq!(
        fs::read_to_string(out.join("Other/Lib/.github/workflows/Lint.yml@V1")).expect("case"),
        "name: CaseCallee\n"
    );
    assert_eq!(
        fs::read_to_string(out.join("fourth/lib/.github/workflows/bad.yml@v1")).expect("bad"),
        ":\n  - [\n"
    );
    assert!(out
        .join("acme/demo/.github/workflows/nightly.yml@trunk")
        .is_file());
    assert!(!out
        .join(format!("acme/demo/.github/workflows/missing.yml@{sha}"))
        .exists());
    let ci_fetches = hits
        .iter()
        .filter(|hit| hit.target.contains("/contents/.github/workflows/ci.yml"))
        .count();
    assert_eq!(ci_fetches, 1, "{}", targets_of(&hits));
    assert!(hits.iter().any(|hit| {
        created_of(&hit.target).is_empty()
            && (hit.target.contains("ref=release%2F1.2") || hit.target.contains("ref=release/1.2"))
    }));
    assert!(hits
        .iter()
        .all(|hit| !hit.target.contains("checkout") && !hit.target.contains("readme.md")));
    assert!(hits.iter().any(|hit| hit.target.contains("ref=trunk")));
    assert!(hits.iter().all(|hit| !hit.target.contains("ref=main")));
    assert_hidden(&out, &output, &hits, &[TOKEN]);
    assert_auth(&hits, TOKEN);
    assert_report_reads(&out);
}

#[test]
fn github_token_is_used_when_gh_token_is_blank_or_absent() {
    let server = TestServer::spawn(|hit, _| {
        if is_repo(&hit.target) {
            return ok_json(format!(
                "{{\"private\":true,\"description\":\"{OTHER_TOKEN}\"}}\n"
            ));
        }
        if is_runs(&hit.target) {
            return empty_runs();
        }
        panic!("unexpected {}", hit.target);
    });
    for (name, mode) in [
        ("github-token-blank", TokenMode::BlankGh),
        ("github-token-only", TokenMode::Github),
    ] {
        server.clear();
        let out = scratch(name).join("out");
        let output = run_owned(
            &server.url(),
            mode,
            &fetch_args(&out, "2026-01-01", Some("2026-01-01")),
        );
        let hits = server.hits();
        assert_success(&output, &hits);
        assert_auth(&hits, OTHER_TOKEN);
        assert!(fs::read_to_string(out.join("repo.json"))
            .expect("repo")
            .contains("[redacted]"));
        assert_hidden(&out, &output, &hits, &[OTHER_TOKEN, TOKEN]);
    }
}

#[test]
fn gh_token_wins_and_both_tokens_stay_out_of_the_cache() {
    let server = TestServer::spawn(|hit, _| {
        if is_repo(&hit.target) {
            return ok_json(format!(
                "{{\"private\":true,\"gh\":\"{TOKEN}\",\"hub\":\"{OTHER_TOKEN}\"}}\n"
            ));
        }
        if is_runs(&hit.target) {
            return empty_runs();
        }
        panic!("unexpected {}", hit.target);
    });
    let out = scratch("both-tokens").join("out");
    let output = run_owned(
        &server.url(),
        TokenMode::Both,
        &fetch_args(&out, "2026-01-01", Some("2026-01-01")),
    );
    let hits = server.hits();
    assert_success(&output, &hits);
    assert_auth(&hits, TOKEN);
    for hit in &hits {
        assert!(!header(hit, "authorization").contains(OTHER_TOKEN));
    }
    let repo = fs::read_to_string(out.join("repo.json")).expect("repo");
    assert!(repo.contains("[redacted]"));
    assert!(!repo.contains(TOKEN));
    assert!(!repo.contains(OTHER_TOKEN));
    assert_hidden(&out, &output, &hits, &[TOKEN, OTHER_TOKEN]);
}

fn script_attempt(hit: &Hit, attempt: u64, name: &str, job_id: u64) -> Reply {
    if is_repo(&hit.target) {
        return ok_json(repo_json(""));
    }
    if is_runs(&hit.target) {
        return ok_json(
            serde_json::to_vec(&runs_page(
                Some(1),
                vec![run_value(42, attempt, "logs/build", "push")],
            ))
            .expect("runs"),
        );
    }
    if is_jobs(&hit.target) {
        assert!(hit.target.contains("/runs/42/jobs"), "{}", hit.target);
        assert!(hit.target.contains("filter=all"), "{}", hit.target);
        let mut job = job_value(job_id, 42, attempt, name);
        job["note"] = json!(TOKEN);
        return ok_json(jobs_page(vec![job]));
    }
    panic!("unexpected {}", hit.target);
}

fn job_run_id(target: &str) -> u64 {
    let path = path_only(target);
    let rest = path.split("/actions/runs/").nth(1).expect("run id");
    rest.split('/').next().expect("id").parse().expect("id")
}

fn raw_reply(body: Vec<u8>) -> Reply {
    Reply {
        status: 200,
        headers: vec![(
            "Content-Type".to_string(),
            "application/vnd.github.raw".to_string(),
        )],
        body,
    }
}

fn read_tree(dir: &Path) -> String {
    let mut files = Vec::new();
    walk_files(dir, &mut files);
    let mut blob = String::new();
    for path in files {
        blob.push_str(&String::from_utf8_lossy(&fs::read(&path).expect("read")));
        blob.push('\n');
    }
    blob
}

fn base64_encode(data: &[u8]) -> String {
    const ALPHA: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut index = 0;
    while index + 3 <= data.len() {
        let value = (u32::from(data[index]) << 16)
            | (u32::from(data[index + 1]) << 8)
            | u32::from(data[index + 2]);
        out.push(ALPHA[((value >> 18) & 63) as usize] as char);
        out.push(ALPHA[((value >> 12) & 63) as usize] as char);
        out.push(ALPHA[((value >> 6) & 63) as usize] as char);
        out.push(ALPHA[(value & 63) as usize] as char);
        index += 3;
    }
    if index < data.len() {
        let remaining = data.len() - index;
        let mut value = u32::from(data[index]) << 16;
        if remaining == 2 {
            value |= u32::from(data[index + 1]) << 8;
        }
        out.push(ALPHA[((value >> 18) & 63) as usize] as char);
        out.push(ALPHA[((value >> 12) & 63) as usize] as char);
        if remaining == 2 {
            out.push(ALPHA[((value >> 6) & 63) as usize] as char);
            out.push('=');
        } else {
            out.push('=');
            out.push('=');
        }
    }
    out
}

fn wrap_base64(text: &str) -> String {
    let mut out = String::new();
    for (index, character) in text.chars().enumerate() {
        if index > 0 && index % 16 == 0 {
            out.push('\n');
        }
        out.push(character);
    }
    out
}
