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

/// Rebuild when HEAD moves, including a worktree whose `.git` is a file.
fn track_git_head() {
    let manifest = manifest_dir();
    let git = manifest.join(".git");
    let gitdir = if git.is_dir() {
        git
    } else if git.is_file() {
        let Ok(text) = std::fs::read_to_string(&git) else {
            return;
        };
        let Some(rest) = text.trim().strip_prefix("gitdir:") else {
            return;
        };
        let path = PathBuf::from(rest.trim());
        if path.is_absolute() {
            path
        } else {
            manifest.join(path)
        }
    } else {
        return;
    };
    let head = gitdir.join("HEAD");
    if !head.is_file() {
        return;
    }
    println!("cargo:rerun-if-changed={}", head.display());
    let Ok(text) = std::fs::read_to_string(&head) else {
        return;
    };
    let Some(rest) = text.trim().strip_prefix("ref:") else {
        return;
    };
    let ref_path = gitdir.join(rest.trim());
    if ref_path.is_file() {
        println!("cargo:rerun-if-changed={}", ref_path.display());
    }
}

fn git_sha() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(manifest_dir())
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .ok()?;
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
