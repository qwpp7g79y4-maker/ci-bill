//! `--fix-diff` prints a unified diff for the highest-savings finding. These
//! tests copy `samples/`, tweak it so a chosen finding kind is the one with
//! the highest savings, and check that the printed diff applies cleanly with
//! `git apply --check`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(name: &str) -> PathBuf {
    let base = std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let path = base.join("ci-bill-fix-diff-cli").join(name);
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("scratch dir");
    path
}

/// Copy `samples/usage-sample.csv` and `samples/.github/workflows/*` into `dir`.
fn copy_samples(dir: &Path) {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    fs::copy(
        manifest.join("samples/usage-sample.csv"),
        dir.join("usage-sample.csv"),
    )
    .expect("copy usage csv");
    let workflows = dir.join(".github/workflows");
    fs::create_dir_all(&workflows).expect("workflows dir");
    for entry in fs::read_dir(manifest.join("samples/.github/workflows")).expect("read samples") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_file() {
            fs::copy(&path, workflows.join(path.file_name().unwrap())).expect("copy workflow");
        }
    }
}

fn replace_in_file(path: &Path, from: &str, to: &str) {
    let text = fs::read_to_string(path).expect("read workflow");
    assert!(text.contains(from), "{path:?} has no {from:?}:\n{text}");
    fs::write(path, text.replace(from, to)).expect("write workflow");
}

fn git(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("run git")
}

fn git_init_and_commit(dir: &Path) {
    let status = |output: Output| {
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    status(git(dir, &["init", "-q"]));
    status(git(dir, &["add", "-A"]));
    status(git(
        dir,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    ));
}

fn run_fix_diff(dir: &Path) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_ci-bill"))
        .current_dir(dir)
        .args([
            "report",
            "--usage",
            "usage-sample.csv",
            "--workflows",
            ".github/workflows",
            "--fix-diff",
        ])
        .output()
        .expect("run ci-bill");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stderr:\n{stderr}\nstdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(stderr.is_empty(), "{stderr}");
    String::from_utf8(output.stdout).expect("utf-8 stdout")
}

/// Apply `diff` to the working tree at `dir` with `git apply --check`, then
/// for real, and return the resulting file content so the caller can check
/// the fix actually lands as intended.
fn apply_diff(dir: &Path, diff: &str) -> String {
    let diff_path = dir.join("fix.diff");
    fs::write(&diff_path, diff).expect("write diff");
    let check = git(dir, &["apply", "--check", "fix.diff"]);
    assert!(
        check.status.success(),
        "git apply --check failed: {}\ndiff:\n{diff}",
        String::from_utf8_lossy(&check.stderr)
    );
    let apply = git(dir, &["apply", "fix.diff"]);
    assert!(
        apply.status.success(),
        "git apply failed: {}\ndiff:\n{diff}",
        String::from_utf8_lossy(&apply.stderr)
    );
    let touched = diff
        .lines()
        .find_map(|line| line.strip_prefix("--- a/"))
        .expect("diff names a file");
    fs::read_to_string(dir.join(touched)).expect("read patched file")
}

#[test]
fn fix_diff_moves_lint_off_macos() {
    let dir = scratch("f1");
    copy_samples(&dir);
    git_init_and_commit(&dir);

    let diff = run_fix_diff(&dir);
    assert!(
        diff.starts_with("--- a/.github/workflows/lint.yml\n"),
        "{diff}"
    );
    assert!(diff.contains("-    runs-on: macos-latest\n"), "{diff}");
    assert!(diff.contains("+    runs-on: ubuntu-latest\n"), "{diff}");

    let patched = apply_diff(&dir, &diff);
    assert!(patched.contains("runs-on: ubuntu-latest"), "{patched}");
    assert!(!patched.contains("macos-latest"), "{patched}");
}

#[test]
fn fix_diff_adds_timeout_minutes() {
    let dir = scratch("f2");
    copy_samples(&dir);
    // Take F1 off the table so a missing-timeout finding is the highest-savings one.
    replace_in_file(
        &dir.join(".github/workflows/lint.yml"),
        "runs-on: macos-latest",
        "runs-on: ubuntu-latest",
    );
    git_init_and_commit(&dir);

    let diff = run_fix_diff(&dir);
    assert!(diff.contains("+    timeout-minutes: 30\n"), "{diff}");
    assert!(
        !diff
            .lines()
            .any(|line| line.starts_with('-') && !line.starts_with("---")),
        "a timeout fix only adds a line: {diff}"
    );

    let patched = apply_diff(&dir, &diff);
    assert!(patched.contains("timeout-minutes: 30"), "{patched}");
}

#[test]
fn fix_diff_adds_concurrency_cancel_in_progress() {
    let dir = scratch("f3");
    copy_samples(&dir);
    // Take F1 and both F2s off the table so the pull_request-without-cancel
    // finding on lint.yml is the only, and so the highest-savings, finding.
    let lint = dir.join(".github/workflows/lint.yml");
    let docs = dir.join(".github/workflows/docs.yml");
    replace_in_file(&lint, "runs-on: macos-latest", "runs-on: ubuntu-latest");
    replace_in_file(
        &lint,
        "runs-on: ubuntu-latest\n",
        "runs-on: ubuntu-latest\n    timeout-minutes: 20\n",
    );
    replace_in_file(
        &docs,
        "runs-on: ubuntu-latest\n",
        "runs-on: ubuntu-latest\n    timeout-minutes: 20\n",
    );
    git_init_and_commit(&dir);

    let diff = run_fix_diff(&dir);
    assert!(
        diff.starts_with("--- a/.github/workflows/lint.yml\n"),
        "{diff}"
    );
    assert!(diff.contains("+concurrency:\n"), "{diff}");
    assert!(
        diff.contains("+  group: ${{ github.workflow }}-${{ github.ref }}\n"),
        "{diff}"
    );
    assert!(diff.contains("+  cancel-in-progress: true\n"), "{diff}");

    let patched = apply_diff(&dir, &diff);
    assert!(patched.contains("cancel-in-progress: true"), "{patched}");
}
