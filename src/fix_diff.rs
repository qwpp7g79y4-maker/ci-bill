//! Unified diff for the top finding this tool knows how to fix.
//!
//! F1 changes `runs-on` to `ubuntu-latest`. F2 adds `timeout-minutes`. F3
//! adds a `concurrency` block with `cancel-in-progress: true`. The finding
//! list is already sorted by savings, highest first, so the first finding of
//! one of these kinds is the one priced highest.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use crate::findings::{Finding, FindingKind};

const CONTEXT: usize = 3;
const DEFAULT_TIMEOUT_MINUTES: &str = "30";

/// Diff for the highest-savings finding this module can fix, or `None` when
/// no such finding is present, or its file can no longer be read.
pub fn render(found: &[Finding], workflows_dir: Option<&Path>) -> Option<String> {
    let finding = found.iter().find(|item| {
        matches!(
            item.kind,
            FindingKind::LintOnRunner
                | FindingKind::MissingTimeout
                | FindingKind::PullRequestConcurrency
        )
    })?;
    match finding.kind {
        FindingKind::LintOnRunner => lint_diff(finding, workflows_dir),
        FindingKind::MissingTimeout => timeout_diff(finding),
        FindingKind::PullRequestConcurrency => concurrency_diff(finding),
        _ => None,
    }
}

/// The file and line this finding's fix must change.
///
/// Most findings already point `path`/`line` at the real `runs-on`, job, or
/// `on:` key. A lint finding raised through a local composite action instead
/// points at the action step, and keeps the job's `runs-on` line in `anchor`.
/// That line lives in the calling workflow file, named in `workflow_path`
/// (`.github/workflows/<file>`), found under `workflows_dir`.
fn resolve_target(finding: &Finding, workflows_dir: Option<&Path>) -> Option<(PathBuf, usize)> {
    match finding.anchor {
        Some(anchor_line) => {
            let dir = workflows_dir?;
            let logical = finding.workflow_path.as_deref()?;
            let file_name = logical.rsplit('/').next().filter(|name| !name.is_empty())?;
            Some((dir.join(file_name), anchor_line))
        }
        None => Some((finding.path.clone(), finding.line)),
    }
}

fn lint_diff(finding: &Finding, workflows_dir: Option<&Path>) -> Option<String> {
    let (path, line) = resolve_target(finding, workflows_dir)?;
    let text = fs::read_to_string(&path).ok()?;
    let lines = split_lines(&text);
    let key_idx = line.checked_sub(1)?;
    if key_idx >= lines.len() {
        return None;
    }
    let end_idx = block_extent(&lines, key_idx);
    if end_idx == key_idx + 1 {
        // The value lives on this one physical line: a plain block scalar,
        // or a scalar inside a flow mapping. Replace just the value so a
        // trailing comma, closing brace, or comment on the same line (flow
        // siblings, or a block comment) survives untouched.
        let bare = strip_cr(lines[key_idx].0);
        let (value_start, value_end) = find_value_span(bare, "runs-on")?;
        let mut new_line = String::with_capacity(bare.len() + "ubuntu-latest".len());
        new_line.push_str(&bare[..value_start]);
        new_line.push_str("ubuntu-latest");
        new_line.push_str(&bare[value_end..]);
        return Some(build_diff(
            &path,
            &lines,
            text.contains("\r\n"),
            key_idx,
            end_idx,
            &[new_line],
        ));
    }
    let indent = " ".repeat(leading_spaces(lines[key_idx].0));
    let new_line = format!("{indent}runs-on: ubuntu-latest");
    Some(build_diff(
        &path,
        &lines,
        text.contains("\r\n"),
        key_idx,
        end_idx,
        &[new_line],
    ))
}

fn timeout_diff(finding: &Finding) -> Option<String> {
    let path = finding.path.clone();
    let text = fs::read_to_string(&path).ok()?;
    let lines = split_lines(&text);
    let key_idx = finding.line.checked_sub(1)?;
    if key_idx >= lines.len() {
        return None;
    }
    if let Some(brace_end) = flow_job_brace(&path, &text, lines[key_idx].0, finding.line) {
        // The job's value is a flow mapping on this line: add `timeout-minutes`
        // as a new flow entry right after its opening brace. Appending an
        // indented line below it, as the block case does, is not valid YAML
        // once the job's whole value is already a complete flow node.
        let bare = strip_cr(lines[key_idx].0);
        let mut new_line = String::with_capacity(bare.len() + 24);
        new_line.push_str(&bare[..brace_end]);
        let _ = write!(new_line, " timeout-minutes: {DEFAULT_TIMEOUT_MINUTES},");
        new_line.push_str(&bare[brace_end..]);
        return Some(build_diff(
            &path,
            &lines,
            text.contains("\r\n"),
            key_idx,
            key_idx + 1,
            &[new_line],
        ));
    }
    let job_indent = leading_spaces(lines[key_idx].0);
    let child_indent = lines[key_idx + 1..]
        .iter()
        .map(|(content, _)| *content)
        .find(|content| !content.trim().is_empty())
        .map(leading_spaces)
        .filter(|indent| *indent > job_indent)
        .unwrap_or(job_indent + 2);
    let indent = " ".repeat(child_indent);
    let new_line = format!("{indent}timeout-minutes: {DEFAULT_TIMEOUT_MINUTES}");
    Some(build_diff(
        &path,
        &lines,
        text.contains("\r\n"),
        key_idx + 1,
        key_idx + 1,
        &[new_line],
    ))
}

fn strip_cr(line: &str) -> &str {
    line.strip_suffix('\r').unwrap_or(line)
}

/// Index right after `key`'s occurrence in `line`, skipping one matching
/// closing quote when `key` was itself quoted (`"runs-on"`, `'runs-on'`).
fn skip_closing_quote(line: &str, key_pos: usize, after_key: usize) -> usize {
    let bytes = line.as_bytes();
    let opening_quote = key_pos
        .checked_sub(1)
        .and_then(|idx| bytes.get(idx))
        .copied()
        .filter(|ch| *ch == b'"' || *ch == b'\'');
    match (opening_quote, bytes.get(after_key)) {
        (Some(open), Some(close)) if open == *close => after_key + 1,
        _ => after_key,
    }
}

/// Byte range of `key`'s value on `line`: a bare or quoted key, any spaces
/// or tabs after the colon, then a quoted or bare scalar value. A bare value
/// ends at a flow delimiter (`,`, `}`, `]`), a comment, or the end of line;
/// trailing whitespace before that point is not part of the value.
fn find_value_span(line: &str, key: &str) -> Option<(usize, usize)> {
    let key_pos = line.find(key)?;
    let after_key = skip_closing_quote(line, key_pos, key_pos + key.len());
    let colon = line[after_key..].find(':')? + after_key;
    let bytes = line.as_bytes();
    let mut start = colon + 1;
    while bytes
        .get(start)
        .is_some_and(|ch| *ch == b' ' || *ch == b'\t')
    {
        start += 1;
    }
    let end = match bytes.get(start) {
        Some(b'"') => find_quoted_end(bytes, start, b'"', true)?,
        Some(b'\'') => find_quoted_end(bytes, start, b'\'', false)?,
        _ => find_bare_end(bytes, start),
    };
    Some((start, end))
}

/// Exclusive end of a quoted scalar starting at `start` (the opening quote).
/// Double-quoted scalars use `\"` to escape the quote; single-quoted
/// scalars double it (`''`).
fn find_quoted_end(
    bytes: &[u8],
    start: usize,
    quote: u8,
    backslash_escapes: bool,
) -> Option<usize> {
    let mut idx = start + 1;
    while idx < bytes.len() {
        if backslash_escapes && bytes[idx] == b'\\' {
            idx += 2;
            continue;
        }
        if bytes[idx] == quote {
            if !backslash_escapes && bytes.get(idx + 1) == Some(&quote) {
                idx += 2;
                continue;
            }
            return Some(idx + 1);
        }
        idx += 1;
    }
    None
}

/// Exclusive end of a bare scalar starting at `start`: up to (not including)
/// a flow delimiter, a whitespace-led comment, or the end of line, with
/// trailing whitespace trimmed.
fn find_bare_end(bytes: &[u8], start: usize) -> usize {
    let mut idx = start;
    while idx < bytes.len() {
        match bytes[idx] {
            b',' | b'}' | b']' => break,
            b'#' if idx > start && matches!(bytes[idx - 1], b' ' | b'\t') => break,
            _ => idx += 1,
        }
    }
    let mut end = idx;
    while end > start && matches!(bytes[end - 1], b' ' | b'\t') {
        end -= 1;
    }
    end
}

/// When `job_line`'s job value is a flow mapping (`name: { ... }`), the
/// index right after that mapping's opening brace. `None` for a block-style
/// job, so the caller falls back to inserting an indented line below it.
fn flow_job_brace(path: &Path, text: &str, line: &str, job_line: usize) -> Option<usize> {
    let workflow = crate::workflow::scan_yaml(path, text).ok()?;
    let job = workflow.jobs.iter().find(|job| job.line == job_line)?;
    let bare = strip_cr(line);
    let key_pos = bare.find(job.name.as_str())?;
    let after_key = skip_closing_quote(bare, key_pos, key_pos + job.name.len());
    let colon = bare[after_key..].find(':')? + after_key;
    let bytes = bare.as_bytes();
    let mut idx = colon + 1;
    while bytes.get(idx).is_some_and(|ch| *ch == b' ' || *ch == b'\t') {
        idx += 1;
    }
    if bytes.get(idx) == Some(&b'{') {
        Some(idx + 1)
    } else {
        None
    }
}

fn concurrency_diff(finding: &Finding) -> Option<String> {
    let path = finding.path.clone();
    let text = fs::read_to_string(&path).ok()?;
    let lines = split_lines(&text);
    let key_idx = finding.line.checked_sub(1)?;
    if key_idx >= lines.len() {
        return None;
    }
    let end_idx = block_extent(&lines, key_idx);
    let new_lines = [
        "concurrency:".to_string(),
        "  group: ${{ github.workflow }}-${{ github.ref }}".to_string(),
        "  cancel-in-progress: true".to_string(),
    ];
    Some(build_diff(
        &path,
        &lines,
        text.contains("\r\n"),
        end_idx,
        end_idx,
        &new_lines,
    ))
}

/// Split into lines without the trailing `\n`. A line kept from a `\r\n`
/// ending keeps its `\r`, so a context or removed line reproduces the exact
/// original bytes. The bool is whether the line had a trailing `\n` in the
/// file; the last line of a file saved without one carries `false`.
fn split_lines(text: &str) -> Vec<(&str, bool)> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        match rest.find('\n') {
            Some(idx) => {
                out.push((&rest[..idx], true));
                rest = &rest[idx + 1..];
            }
            None => {
                out.push((rest, false));
                rest = "";
            }
        }
    }
    out
}

fn leading_spaces(line: &str) -> usize {
    line.chars().take_while(|ch| *ch == ' ').count()
}

/// Exclusive end of the value that starts right after `key_idx`: every line
/// more indented than the key, with blank lines bridging over to further
/// indented content. Returns `key_idx + 1` when the key's value is written
/// on its own line (a scalar, a flow list, or a flow map).
fn block_extent(lines: &[(&str, bool)], key_idx: usize) -> usize {
    let key_indent = leading_spaces(lines[key_idx].0);
    let mut end = key_idx + 1;
    while end < lines.len() {
        let content = lines[end].0;
        if content.trim().is_empty() || leading_spaces(content) > key_indent {
            end += 1;
        } else {
            break;
        }
    }
    end
}

#[allow(clippy::too_many_arguments)]
fn build_diff(
    path: &Path,
    lines: &[(&str, bool)],
    crlf: bool,
    start: usize,
    end: usize,
    new_lines: &[String],
) -> String {
    let ctx_start = start.saturating_sub(CONTEXT);
    let ctx_end = (end + CONTEXT).min(lines.len());
    let old_count = ctx_end - ctx_start;
    let new_count = (start - ctx_start) + new_lines.len() + (ctx_end - end);
    let display = path.display();
    let mut out = String::new();
    let _ = writeln!(out, "--- a/{display}");
    let _ = writeln!(out, "+++ b/{display}");
    let _ = writeln!(
        out,
        "@@ -{},{} +{},{} @@",
        ctx_start + 1,
        old_count,
        ctx_start + 1,
        new_count
    );
    for line in &lines[ctx_start..start] {
        emit(&mut out, ' ', *line);
    }
    for line in &lines[start..end] {
        emit(&mut out, '-', *line);
    }
    let suffix = if crlf { "\r" } else { "" };
    for content in new_lines {
        let _ = writeln!(out, "+{content}{suffix}");
    }
    for line in &lines[end..ctx_end] {
        emit(&mut out, ' ', *line);
    }
    out
}

fn emit(out: &mut String, marker: char, (content, has_newline): (&str, bool)) {
    let _ = writeln!(out, "{marker}{content}");
    if !has_newline {
        out.push_str("\\ No newline at end of file\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::findings::Finding;
    use std::fs;

    fn scratch(name: &str) -> PathBuf {
        let base = std::env::var_os("CARGO_TARGET_TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let path = base.join("ci-bill-fix-diff").join(name);
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("scratch dir");
        path
    }

    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, text).expect("write fixture");
        path
    }

    fn finding(path: &Path, line: usize, kind: FindingKind, message: &str) -> Finding {
        Finding::record(path, line, kind, message)
    }

    const LINT_YML: &str = "name: Lint\non:\n  push:\n  pull_request:\n\njobs:\n  lint:\n    runs-on: macos-latest\n    steps:\n      - uses: actions/checkout@v4\n      - run: cargo fmt --check\n";

    #[test]
    fn lint_on_runner_changes_runs_on_to_ubuntu_latest() {
        let dir = scratch("lint");
        let path = write(&dir, "lint.yml", LINT_YML);
        let found = [finding(
            &path,
            8,
            FindingKind::LintOnRunner,
            "lint on macOS",
        )];
        let diff = render(&found, None).expect("diff");
        assert!(diff.contains("-    runs-on: macos-latest\n"), "{diff}");
        assert!(diff.contains("+    runs-on: ubuntu-latest\n"), "{diff}");
        assert!(diff.starts_with(&format!("--- a/{}\n", path.display())));
    }

    #[test]
    fn missing_timeout_inserts_timeout_minutes_under_the_job() {
        let dir = scratch("timeout");
        let path = write(&dir, "docs.yml", LINT_YML);
        let found = [finding(
            &path,
            7,
            FindingKind::MissingTimeout,
            "default timeout is 360 minutes",
        )];
        let diff = render(&found, None).expect("diff");
        assert!(diff.contains("+    timeout-minutes: 30\n"), "{diff}");
        assert!(
            !diff
                .lines()
                .any(|line| line.starts_with('-') && !line.starts_with("---")),
            "no removed lines: {diff}"
        );
    }

    #[test]
    fn flow_lint_on_runner_edits_only_the_value() {
        let dir = scratch("flow-lint");
        let path = write(
            &dir,
            "lint.yml",
            "jobs: { lint: { runs-on: macos-latest, steps: [ { run: \"echo lint\" } ] } }\n",
        );
        let found = [finding(
            &path,
            1,
            FindingKind::LintOnRunner,
            "lint on macOS",
        )];
        let diff = render(&found, None).expect("diff");
        assert!(
            diff.contains(
                "+jobs: { lint: { runs-on: ubuntu-latest, steps: [ { run: \"echo lint\" } ] } }\n"
            ),
            "{diff}"
        );
        assert!(
            !diff.contains("-jobs: { lint: { runs-on: macos-latest,\n"),
            "{diff}"
        );
    }

    #[test]
    fn flow_lint_on_runner_keeps_a_trailing_comma_and_comment() {
        let dir = scratch("flow-lint-comma");
        let path = write(
            &dir,
            "lint.yml",
            "jobs: { lint: {\n    runs-on: macos-latest, # pick a runner\n    steps: [ { run: \"echo lint\" } ]\n  } }\n",
        );
        let found = [finding(
            &path,
            2,
            FindingKind::LintOnRunner,
            "lint on macOS",
        )];
        let diff = render(&found, None).expect("diff");
        assert!(
            diff.contains("+    runs-on: ubuntu-latest, # pick a runner\n"),
            "{diff}"
        );
    }

    #[test]
    fn flow_missing_timeout_inserts_a_flow_entry_after_the_brace() {
        let dir = scratch("flow-timeout");
        let path = write(
            &dir,
            "lint.yml",
            "jobs: { lint: { runs-on: ubuntu-latest, steps: [ { run: \"echo lint\" } ] } }\n",
        );
        let found = [finding(
            &path,
            1,
            FindingKind::MissingTimeout,
            "default timeout is 360 minutes",
        )];
        let diff = render(&found, None).expect("diff");
        assert!(
            diff.contains(
                "+jobs: { lint: { timeout-minutes: 30, runs-on: ubuntu-latest, steps: [ { run: \"echo lint\" } ] } }\n"
            ),
            "{diff}"
        );
    }

    #[test]
    fn pull_request_concurrency_adds_cancel_in_progress_block() {
        let dir = scratch("concurrency");
        let path = write(&dir, "ci.yml", LINT_YML);
        let found = [finding(
            &path,
            2,
            FindingKind::PullRequestConcurrency,
            "pull_request without concurrency cancel-in-progress",
        )];
        let diff = render(&found, None).expect("diff");
        assert!(diff.contains("+concurrency:\n"), "{diff}");
        assert!(
            diff.contains("+  group: ${{ github.workflow }}-${{ github.ref }}\n"),
            "{diff}"
        );
        assert!(diff.contains("+  cancel-in-progress: true\n"), "{diff}");
    }

    #[test]
    fn skips_leading_findings_it_cannot_fix() {
        let dir = scratch("order");
        let path = write(&dir, "lint.yml", LINT_YML);
        let found = [
            finding(&path, 1, FindingKind::DuplicateKey, "duplicate key name"),
            finding(&path, 8, FindingKind::LintOnRunner, "lint on macOS"),
            finding(
                &path,
                2,
                FindingKind::PullRequestConcurrency,
                "pull_request without concurrency cancel-in-progress",
            ),
        ];
        let diff = render(&found, None).expect("diff");
        assert!(diff.contains("+    runs-on: ubuntu-latest\n"), "{diff}");
    }

    #[test]
    fn no_fixable_finding_yields_no_diff() {
        let dir = scratch("none");
        let path = write(&dir, "lint.yml", LINT_YML);
        let found = [finding(
            &path,
            1,
            FindingKind::DuplicateKey,
            "duplicate key name (line 1 and 1)",
        )];
        assert!(render(&found, None).is_none());
    }

    #[test]
    fn crlf_file_keeps_crlf_in_added_lines() {
        let dir = scratch("crlf");
        let path = write(&dir, "lint.yml", &LINT_YML.replace('\n', "\r\n"));
        let found = [finding(
            &path,
            8,
            FindingKind::LintOnRunner,
            "lint on macOS",
        )];
        let diff = render(&found, None).expect("diff");
        assert!(diff.contains("-    runs-on: macos-latest\r\n"), "{diff}");
        assert!(diff.contains("+    runs-on: ubuntu-latest\r\n"), "{diff}");
    }

    #[test]
    fn composite_action_lint_finding_fixes_the_caller_runs_on() {
        let dir = scratch("composite");
        let workflows = dir.join(".github/workflows");
        fs::create_dir_all(&workflows).expect("workflows dir");
        let caller_path = workflows.join("lint.yml");
        fs::write(&caller_path, LINT_YML).expect("caller");
        let action_path = dir.join(".github/actions/lint/action.yml");
        fs::create_dir_all(action_path.parent().unwrap()).expect("action dir");
        fs::write(
            &action_path,
            "runs:\n  using: composite\n  steps:\n    - run: cargo fmt --check\n",
        )
        .expect("action");
        let mut item = finding(&action_path, 4, FindingKind::LintOnRunner, "lint on macOS");
        item.anchor = Some(8);
        item.workflow_path = Some(".github/workflows/lint.yml".to_string());
        let diff = render(&[item], Some(&workflows)).expect("diff");
        assert!(
            diff.starts_with(&format!("--- a/{}\n", caller_path.display())),
            "{diff}"
        );
        assert!(diff.contains("+    runs-on: ubuntu-latest\n"), "{diff}");
    }
}
