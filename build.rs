//! Asks git for the commit of this tree. Debug builds print it from
//! `--version`. Nothing here invents a commit: a missing git, a failed
//! command, or a value that is not a full sha leaves the version unchanged.
//! Release builds do not receive the commit.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(ci_bill_git_sha)");
    println!("cargo:rerun-if-changed=build.rs");
    track_git_head();
    if std::env::var("PROFILE").unwrap_or_default() != "debug" {
        return;
    }
    let Some(sha) = git_sha() else {
        return;
    };
    println!("cargo:rustc-cfg=ci_bill_git_sha");
    println!("cargo:rustc-env=CI_BILL_GIT_SHA={sha}");
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string()))
}

/// Rebuild when the commit changes. In a worktree, `HEAD` stays
/// `ref: refs/heads/...` and the sha lives in the common git dir.
fn track_git_head() {
    let Some(head) = git_path("HEAD") else {
        return;
    };
    println!("cargo:rerun-if-changed={}", head.display());
    let Ok(text) = std::fs::read_to_string(&head) else {
        return;
    };
    let Some(rest) = text.trim().strip_prefix("ref:") else {
        return;
    };
    let name = rest.trim();
    if let Some(path) = git_path(name) {
        println!("cargo:rerun-if-changed={}", path.display());
    } else if let Some(packed) = git_path("packed-refs") {
        println!("cargo:rerun-if-changed={}", packed.display());
    }
}

fn git_path(name: &str) -> Option<PathBuf> {
    let output = git(["rev-parse", "--git-path", name])?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let path = PathBuf::from(text.trim());
    let path = if path.is_absolute() {
        path
    } else {
        manifest_dir().join(path)
    };
    path.is_file().then_some(path)
}

fn git(args: [&str; 3]) -> Option<std::process::Output> {
    Command::new("git")
        .args(args)
        .current_dir(manifest_dir())
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .ok()
}

fn git_sha() -> Option<String> {
    let output = git(["rev-parse", "--verify", "HEAD"])?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let sha = text.trim();
    if sha.len() == 40 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Some(sha.to_string())
    } else {
        None
    }
}
