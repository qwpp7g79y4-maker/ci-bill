//! W-24: each error path of `prices`, `fetch` and the argument parser says
//! what went wrong and what to do next. Exit codes are the existing ones.
//! One test per path; the HTTP paths are in tests/fetch.rs.

use std::path::PathBuf;
use std::process::{Command, Output};

fn ci_bill(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ci-bill"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .args(args)
        .output()
        .expect("run ci-bill")
}

fn fails(args: &[&str], code: i32) -> String {
    let out = ci_bill(args);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(code), "{err}");
    assert!(out.stdout.is_empty(), "stdout not empty");
    assert!(!err.contains("panicked"), "{err}");
    err
}

fn has(err: &str, what: &str, next: &str) {
    assert!(err.contains(what), "missing {what:?}: {err}");
    assert!(err.contains(next), "missing next step {next:?}: {err}");
}

fn scratch(name: &str) -> PathBuf {
    let base = std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let path = base.join("ci-bill-w24").join(name);
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn prices_missing_runner_map_file() {
    let e = fails(&["prices", "--runner-map", "examples/no-such.csv"], 2);
    has(
        &e,
        "cannot read file",
        "what to do: check that the path exists",
    );
}

#[test]
fn prices_runner_map_without_columns() {
    let f = scratch("map").join("map.csv");
    std::fs::write(&f, "a,b\n1,2\n").unwrap();
    let e = fails(&["prices", "--runner-map", f.to_str().unwrap()], 2);
    has(
        &e,
        "missing column label",
        "what to do: fix the file named above",
    );
}

#[test]
fn prices_unknown_sku() {
    let e = fails(&["prices", "--sku", "NOPE"], 2);
    has(&e, "unknown sku NOPE", "what to do: run `ci-bill prices`");
}

#[test]
fn prices_workflow_without_usage() {
    let e = fails(
        &["prices", "--label", "ubuntu-latest", "--workflow", "a.yml"],
        3,
    );
    has(
        &e,
        "--workflow requires --usage",
        "what to do: add --usage CSV",
    );
}

#[test]
fn prices_repository_without_usage() {
    let e = fails(
        &["prices", "--label", "ubuntu-latest", "--repository", "a/b"],
        3,
    );
    has(
        &e,
        "--repository requires --usage",
        "what to do: add --usage CSV",
    );
}

#[test]
fn prices_usage_without_label_or_sku() {
    let e = fails(&["prices", "--usage", "examples/usage.csv"], 3);
    has(
        &e,
        "--usage requires --label or --sku",
        "what to do: add --label or --sku",
    );
}

#[test]
fn prices_label_and_sku_disagree() {
    let e = fails(
        &[
            "prices",
            "--label",
            "ubuntu-latest",
            "--sku",
            "actions_macos",
        ],
        3,
    );
    has(&e, "resolves to", "what to do: drop --sku");
}

#[test]
fn prices_usage_and_runner_map_both_on_stdin() {
    let e = fails(
        &[
            "prices",
            "--label",
            "ubuntu-latest",
            "--usage",
            "-",
            "--runner-map",
            "-",
        ],
        3,
    );
    has(
        &e,
        "can't both read standard input",
        "give one of them a file",
    );
}

#[test]
fn prices_unknown_label() {
    let e = fails(&["prices", "--label", "no-such-runner"], 3);
    has(&e, "unknown runner label: no-such-runner", "--runner-map");
}

#[test]
fn prices_unreadable_usage_file() {
    let e = fails(
        &[
            "prices",
            "--label",
            "ubuntu-latest",
            "--usage",
            "examples/no-such.csv",
        ],
        2,
    );
    has(
        &e,
        "cannot read file",
        "what to do: check that the path exists",
    );
}

#[test]
fn prices_workflow_in_two_repositories() {
    let f = scratch("two-repos").join("u.csv");
    let header = std::fs::read_to_string("examples/usage.csv").unwrap();
    let mut lines = header.lines();
    let head = lines.next().unwrap().to_string();
    let first = lines.next().unwrap().to_string();
    let cols: Vec<&str> = head.split(',').collect();
    let repo_at = cols.iter().position(|c| *c == "repository").unwrap();
    let mut other: Vec<String> = first.split(',').map(str::to_string).collect();
    other[repo_at] = "other-org/other-repo".to_string();
    std::fs::write(&f, format!("{head}\n{first}\n{}\n", other.join(","))).unwrap();
    let wf_at = cols.iter().position(|c| *c == "workflow_path").unwrap();
    let wf = first.split(',').nth(wf_at).unwrap().to_string();
    let sku_at = cols.iter().position(|c| *c == "sku").unwrap();
    let sku = first.split(',').nth(sku_at).unwrap().to_string();
    let e = fails(
        &[
            "prices",
            "--sku",
            &sku,
            "--usage",
            f.to_str().unwrap(),
            "--workflow",
            &wf,
        ],
        2,
    );
    has(
        &e,
        "matches more than one repository",
        "add --repository OWNER/REPO",
    );
}

#[test]
fn report_budget_not_a_number() {
    let e = fails(
        &["report", "--usage", "examples/usage.csv", "--budget", "abc"],
        3,
    );
    has(&e, "expected a finite USD amount", "for example 25.50");
}

#[test]
fn fetch_repo_not_owner_slash_repo() {
    let o = scratch("repo").join("o");
    let e = fails(
        &[
            "fetch",
            "--repo",
            "bad",
            "--out",
            o.to_str().unwrap(),
            "--since",
            "2026-01-01",
        ],
        3,
    );
    has(&e, "expected OWNER/REPO", "for example rust-lang/cargo");
}

#[test]
fn fetch_date_not_a_date() {
    let o = scratch("date").join("o");
    let e = fails(
        &[
            "fetch",
            "--repo",
            "a/b",
            "--out",
            o.to_str().unwrap(),
            "--since",
            "2026-13-01",
        ],
        3,
    );
    has(
        &e,
        "expected a UTC date YYYY-MM-DD",
        "for example 2026-01-31",
    );
}

#[test]
fn fetch_out_is_a_file() {
    let d = scratch("out-file");
    let f = d.join("afile");
    std::fs::write(&f, "x").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_ci-bill"))
        .args([
            "fetch",
            "--repo",
            "a/b",
            "--out",
            f.to_str().unwrap(),
            "--since",
            "2026-01-01",
        ])
        .env("GH_TOKEN", "ghs_w24_test_token")
        .env("GITHUB_API_URL", "http://127.0.0.1:9")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(2), "{err}");
    has(
        &err,
        "cannot read directory",
        "check that the path exists and is readable",
    );
}
