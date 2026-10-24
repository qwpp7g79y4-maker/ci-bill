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

/// Write a single custom workflow file plus a usage row that covers it, for
/// flow-style YAML shapes the `samples/` fixtures do not exercise.
fn write_flow_workflow(dir: &Path, yaml: &str) -> PathBuf {
    let workflows = dir.join(".github/workflows");
    fs::create_dir_all(&workflows).expect("workflows dir");
    let path = workflows.join("lint.yml");
    fs::write(&path, yaml).expect("write workflow");
    fs::write(
        dir.join("usage-sample.csv"),
        "date,product,sku,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,net_amount,username,organization,repository,workflow_path\n\
2026-09-01,actions,actions_macos,100,minutes,0.062,6.2,0,6.2,ada,example-org,example-org/app,.github/workflows/lint.yml\n",
    )
    .expect("write usage");
    path
}

/// Re-run `ci-bill report` on the patched tree: it must still exit 0 (the
/// YAML is still valid) and the named finding text must be gone.
fn assert_fixed(dir: &Path, gone_finding: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_ci-bill"))
        .current_dir(dir)
        .args([
            "report",
            "--usage",
            "usage-sample.csv",
            "--workflows",
            ".github/workflows",
        ])
        .output()
        .expect("run ci-bill");
    assert!(
        output.status.success(),
        "patched YAML did not parse, stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    assert!(
        !stdout.contains(gone_finding),
        "{gone_finding:?} still present:\n{stdout}"
    );
}

#[test]
fn fix_diff_pure_flow_lint_on_runner_edits_only_the_value() {
    let dir = scratch("flow-pure");
    write_flow_workflow(
        &dir,
        "jobs: { lint: { runs-on: macos-latest, timeout-minutes: 30, steps: [ { run: \"echo lint\" } ] } }\n",
    );
    git_init_and_commit(&dir);

    let diff = run_fix_diff(&dir);
    assert!(
        diff.contains("-jobs: { lint: { runs-on: macos-latest, timeout-minutes: 30, steps: [ { run: \"echo lint\" } ] } }\n"),
        "{diff}"
    );
    assert!(
        diff.contains("+jobs: { lint: { runs-on: ubuntu-latest, timeout-minutes: 30, steps: [ { run: \"echo lint\" } ] } }\n"),
        "{diff}"
    );

    let patched = apply_diff(&dir, &diff);
    assert!(patched.contains("runs-on: ubuntu-latest"), "{patched}");
    assert!(!patched.contains("macos-latest"), "{patched}");
    assert!(
        patched.contains("steps: [ { run: \"echo lint\" } ] } }"),
        "{patched}"
    );
    assert_fixed(&dir, "lint on macOS");
}

#[test]
fn fix_diff_pure_flow_missing_timeout_inserts_a_flow_entry() {
    let dir = scratch("flow-pure-timeout");
    write_flow_workflow(
        &dir,
        "jobs: { lint: { runs-on: ubuntu-latest, steps: [ { run: \"echo lint\" } ] } }\n",
    );
    git_init_and_commit(&dir);

    let diff = run_fix_diff(&dir);
    assert!(
        diff.contains("+jobs: { lint: { timeout-minutes: 30, runs-on: ubuntu-latest, steps: [ { run: \"echo lint\" } ] } }\n"),
        "{diff}"
    );

    let patched = apply_diff(&dir, &diff);
    assert!(patched.contains("timeout-minutes: 30"), "{patched}");
    assert!(
        patched.contains("steps: [ { run: \"echo lint\" } ] } }"),
        "{patched}"
    );
    assert_fixed(&dir, "default timeout");
}

#[test]
fn fix_diff_flow_job_inside_block_jobs_fixes_both_findings() {
    let dir = scratch("flow-job-in-block-jobs");
    write_flow_workflow(
        &dir,
        "jobs:\n  lint: { runs-on: macos-latest, steps: [ { run: \"echo lint\" } ] }\n",
    );
    git_init_and_commit(&dir);

    let first = run_fix_diff(&dir);
    apply_diff(&dir, &first);
    let second = run_fix_diff(&dir);
    let patched = apply_diff(&dir, &second);

    assert!(patched.contains("runs-on: ubuntu-latest"), "{patched}");
    assert!(!patched.contains("macos-latest"), "{patched}");
    assert!(patched.contains("timeout-minutes: 30"), "{patched}");
    assert!(
        patched.contains("steps: [ { run: \"echo lint\" } ]"),
        "{patched}"
    );
    assert_fixed(&dir, "lint on macOS");
    assert_fixed(&dir, "default timeout");
}

#[test]
fn fix_diff_block_job_inside_flow_jobs_fixes_both_findings() {
    let dir = scratch("block-job-in-flow-jobs");
    write_flow_workflow(
        &dir,
        "jobs: { lint: {\n    runs-on: macos-latest,\n    steps: [ { run: \"echo lint\" } ]\n  } }\n",
    );
    git_init_and_commit(&dir);

    let first = run_fix_diff(&dir);
    apply_diff(&dir, &first);
    let second = run_fix_diff(&dir);
    let patched = apply_diff(&dir, &second);

    assert!(patched.contains("runs-on: ubuntu-latest"), "{patched}");
    assert!(!patched.contains("macos-latest"), "{patched}");
    assert!(patched.contains("timeout-minutes: 30"), "{patched}");
    assert!(
        patched.contains("steps: [ { run: \"echo lint\" } ]"),
        "{patched}"
    );
    assert_fixed(&dir, "lint on macOS");
    assert_fixed(&dir, "default timeout");
}

#[test]
fn fix_diff_flow_quoted_key_and_value_edits_only_the_value() {
    let dir = scratch("flow-quoted");
    write_flow_workflow(
        &dir,
        "jobs: { lint: { \"runs-on\": \"macos-latest\", timeout-minutes: 30, steps: [ { run: \"echo lint\" } ] } }\n",
    );
    git_init_and_commit(&dir);

    let diff = run_fix_diff(&dir);
    let patched = apply_diff(&dir, &diff);
    assert!(patched.contains("\"runs-on\": ubuntu-latest"), "{patched}");
    assert!(!patched.contains("macos-latest"), "{patched}");
    assert_fixed(&dir, "lint on macOS");
}

#[test]
fn fix_diff_flow_comment_and_crlf_edit_only_the_value() {
    let dir = scratch("flow-comment-crlf");
    write_flow_workflow(
        &dir,
        "jobs:\r\n  lint:\r\n    runs-on: macos-latest # pick a runner\r\n    steps:\r\n      - run: echo lint\r\n",
    );
    git_init_and_commit(&dir);

    let first = run_fix_diff(&dir);
    apply_diff(&dir, &first);
    let second = run_fix_diff(&dir);
    let patched = apply_diff(&dir, &second);

    assert!(
        patched.contains("runs-on: ubuntu-latest # pick a runner\r\n"),
        "{patched}"
    );
    assert!(!patched.contains("macos-latest"), "{patched}");
    assert_fixed(&dir, "lint on macOS");
    assert_fixed(&dir, "default timeout");
}

#[test]
fn fix_diff_tab_after_colon_edits_only_the_value() {
    let dir = scratch("flow-tab");
    write_flow_workflow(
        &dir,
        "jobs: { lint: { runs-on: \tmacos-latest, timeout-minutes: 30, steps: [ { run: \"echo lint\" } ] } }\n",
    );
    git_init_and_commit(&dir);

    let diff = run_fix_diff(&dir);
    let patched = apply_diff(&dir, &diff);
    assert!(patched.contains("runs-on: \tubuntu-latest"), "{patched:?}");
    assert!(!patched.contains("macos-latest"), "{patched}");
    assert_fixed(&dir, "lint on macOS");
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

#[test]
fn fix_diff_multiline_flow_on_block_adds_concurrency_after_the_closing_brace() {
    // `on:` is a flow mapping that spans several lines, with its closing
    // `}` written back at the key's own indent (0). The indentation-only
    // heuristic for "where does this value end" mistook that `}` line for
    // the next sibling key and inserted the new block above it, between
    // the mapping's entries and its own closing brace.
    let dir = scratch("flow-on-block");
    let workflows = dir.join(".github/workflows");
    fs::create_dir_all(&workflows).expect("workflows dir");
    fs::write(
        workflows.join("ci.yml"),
        "name: CI\non: {\n  push: {},\n  pull_request: {}\n}\njobs:\n  build:\n    runs-on: ubuntu-latest\n    timeout-minutes: 10\n    steps:\n      - run: echo hi\n",
    )
    .expect("write workflow");
    fs::write(
        dir.join("usage-sample.csv"),
        "date,product,sku,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,net_amount,username,organization,repository,workflow_path\n\
2026-09-01,actions,actions_linux,100,minutes,0.008,0.8,0,0.8,ada,example-org,example-org/app,.github/workflows/ci.yml\n",
    )
    .expect("write usage");
    git_init_and_commit(&dir);

    let diff = run_fix_diff(&dir);
    assert!(diff.contains("+concurrency:\n"), "{diff}");
    // The closing `}` must stay a removed/kept line *before* the new block,
    // not get pushed below it.
    assert!(diff.contains(" }\n+concurrency:\n"), "{diff}");

    let patched = apply_diff(&dir, &diff);
    assert!(patched.contains("cancel-in-progress: true"), "{patched}");
    assert_fixed(&dir, "cancel-in-progress");
}

#[test]
fn fix_diff_multiline_flow_runs_on_sequence_replaces_the_whole_value() {
    // `runs-on: [ ... ]` written across several lines, with its closing `]`
    // back at the key's own indent, hit the same "}` line looks like the
    // next sibling" mistake: the fix left the sequence's brackets stranded
    // instead of replacing the whole value.
    let dir = scratch("flow-runs-on-seq");
    write_flow_workflow(
        &dir,
        "jobs:\n  lint:\n    timeout-minutes: 5\n    runs-on: [\n      macos-latest\n    ]\n    steps:\n      - run: cargo fmt --check\n",
    );
    git_init_and_commit(&dir);

    let diff = run_fix_diff(&dir);
    assert!(diff.contains("+    runs-on: ubuntu-latest\n"), "{diff}");
    assert!(diff.contains("-    runs-on: [\n"), "{diff}");
    assert!(diff.contains("-      macos-latest\n"), "{diff}");
    assert!(diff.contains("-    ]\n"), "{diff}");

    let patched = apply_diff(&dir, &diff);
    assert!(patched.contains("runs-on: ubuntu-latest"), "{patched}");
    assert!(!patched.contains('['), "{patched}");
    assert!(!patched.contains(']'), "{patched}");
    assert_fixed(&dir, "lint on macOS");
}
