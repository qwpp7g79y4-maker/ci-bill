use std::fs;
use std::path::{Path, PathBuf};

/// One workflow file, scanned with a block-style indentation reader.
///
/// Line numbers are 1-based positions in the file. Comments and blank lines
/// are ignored and do not shift the numbers of the lines that remain.
/// Structure this scanner does not understand is skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workflow {
    pub path: PathBuf,
    /// Line of the top-level `on:` key, when present.
    pub on_line: Option<usize>,
    pub has_pull_request: bool,
    /// Top-level `concurrency.cancel-in-progress` is the literal `true`.
    pub cancel_in_progress: bool,
    pub jobs: Vec<Job>,
}

/// One job under top-level `jobs:`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub name: String,
    /// Line of the job key.
    pub line: usize,
    /// Job-level `name:` value, when it is written separately from the key.
    pub display_name: Option<String>,
    pub runs_on: Option<RunsOn>,
    pub has_timeout_minutes: bool,
    /// Text of `run` steps, one step per line. Multiline scripts keep their lines.
    pub steps_text: String,
}

/// `runs-on` for a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunsOn {
    /// Line of the `runs-on` key.
    pub line: usize,
    /// Value as written, for example `ubuntu-latest` or `[self-hosted, macos]`.
    pub value: String,
    pub labels: Vec<String>,
}

/// The workflow directory or a workflow file could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanError {
    pub path: PathBuf,
    kind: ScanErrorKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanErrorKind {
    Directory,
    File,
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            ScanErrorKind::Directory => write!(f, "cannot read directory"),
            ScanErrorKind::File => write!(f, "cannot read file"),
        }
    }
}

impl std::error::Error for ScanError {}

impl ScanError {
    fn directory(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            kind: ScanErrorKind::Directory,
        }
    }

    fn file(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            kind: ScanErrorKind::File,
        }
    }
}

/// Read `*.yml` and `*.yaml` files directly in `dir`, in path order.
///
/// Nested directories are not walked. An unreadable directory or file is an
/// error. A file whose YAML is not a workflow still returns a [`Workflow`]
/// with whatever top-level fields were recognized.
pub fn scan_dir(dir: &Path) -> Result<Vec<Workflow>, ScanError> {
    let entries = fs::read_dir(dir).map_err(|_| ScanError::directory(dir))?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| ScanError::directory(dir))?;
        let path = entry.path();
        if path.is_file() && is_workflow_file(&path) {
            paths.push(path);
        }
    }
    paths.sort();

    let mut workflows = Vec::with_capacity(paths.len());
    for path in paths {
        let text = fs::read_to_string(&path).map_err(|_| ScanError::file(&path))?;
        workflows.push(scan_yaml(&path, &text));
    }
    Ok(workflows)
}

/// Scan one workflow document. Never panics on malformed text.
pub fn scan_yaml(path: &Path, text: &str) -> Workflow {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    interpret(path, &preprocess(text))
}

fn is_workflow_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("yml") || ext.eq_ignore_ascii_case("yaml"))
}

struct Line {
    number: usize,
    indent: usize,
    key: Option<String>,
    value: Option<String>,
    is_list: bool,
    block_text: Option<String>,
}

struct Block {
    key_indent: usize,
    content_indent: Option<usize>,
    text: String,
    started: bool,
}

/// A `:` separates a key only when the next character is whitespace, `[`, `{`,
/// or the end of the line. That leaves `http://` and `${{ }}` expressions intact.
fn split_colon(body: &str) -> Option<(&str, &str)> {
    let bytes = body.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'"' && !in_single {
            in_double = !in_double;
            index += 1;
            continue;
        }
        if byte == b'\'' && !in_double {
            if in_single && bytes.get(index + 1) == Some(&b'\'') {
                index += 2;
                continue;
            }
            in_single = !in_single;
            index += 1;
            continue;
        }
        if byte == b':' && !in_single && !in_double {
            let next = bytes.get(index + 1).copied();
            let separates =
                next.is_none_or(|next| next.is_ascii_whitespace() || next == b'[' || next == b'{');
            if separates {
                return Some((&body[..index], &body[index + 1..]));
            }
        }
        index += 1;
    }
    None
}

fn unquote(value: &str) -> &str {
    let value = value.trim();
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &value[1..value.len() - 1];
        }
    }
    value
}

fn normalize_key(raw: &str) -> Option<String> {
    let key = unquote(raw.trim());
    if key.is_empty() || key.chars().any(char::is_whitespace) {
        None
    } else {
        Some(key.to_string())
    }
}

fn split_key_value(body: &str) -> Option<(String, Option<String>)> {
    let (key_raw, value_raw) = split_colon(body)?;
    let key = normalize_key(key_raw)?;
    let value = value_raw.trim();
    if value.is_empty() {
        Some((key, None))
    } else {
        Some((key, Some(value.to_string())))
    }
}

fn strip_inline_comment(content: &str) -> &str {
    let bytes = content.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'"' && !in_single {
            in_double = !in_double;
            index += 1;
            continue;
        }
        if byte == b'\'' && !in_double {
            if in_single && bytes.get(index + 1) == Some(&b'\'') {
                index += 2;
                continue;
            }
            in_single = !in_single;
            index += 1;
            continue;
        }
        if byte == b'#'
            && !in_single
            && !in_double
            && (index == 0 || bytes[index - 1].is_ascii_whitespace())
        {
            return content[..index].trim_end();
        }
        index += 1;
    }
    content.trim_end()
}

fn is_block_scalar(value: &str) -> bool {
    let value = value.trim();
    let rest = if let Some(rest) = value.strip_prefix('|') {
        rest
    } else if let Some(rest) = value.strip_prefix('>') {
        rest
    } else {
        return false;
    };
    let rest = rest.strip_prefix(['+', '-']).unwrap_or(rest);
    rest.chars().all(|c| c.is_ascii_digit())
}

fn is_ignorable(raw: &str) -> bool {
    let trimmed = raw.trim();
    trimmed.is_empty() || trimmed.starts_with('#')
}

fn space_indent(raw: &str) -> Option<usize> {
    let mut count = 0;
    for character in raw.chars() {
        if character == ' ' {
            count += 1;
        } else if character == '\t' {
            return None;
        } else {
            break;
        }
    }
    Some(count)
}

fn parse_content(number: usize, indent: usize, content: &str) -> Option<Line> {
    let (is_list, body) = match content.strip_prefix('-') {
        Some(rest) if rest.is_empty() || rest.starts_with(char::is_whitespace) => {
            (true, rest.trim_start())
        }
        _ => (false, content),
    };
    if let Some((key, value)) = split_key_value(body) {
        return Some(Line {
            number,
            indent,
            key: Some(key),
            value,
            is_list,
            block_text: None,
        });
    }
    if is_list {
        let value = if body.is_empty() {
            None
        } else {
            Some(unquote(body).trim().to_string())
        };
        return Some(Line {
            number,
            indent,
            key: None,
            value,
            is_list: true,
            block_text: None,
        });
    }
    None
}

fn append_block_line(block: &mut Block, text: &str) {
    if block.started {
        block.text.push('\n');
    }
    block.started = true;
    block.text.push_str(text.trim_end());
}

/// Inside `|` or `>`, `#` is script text. The block ends at the next content
/// line that is indented less than the script body.
fn keep_block_line(block: &mut Block, raw: &str) -> bool {
    if raw.trim().is_empty() {
        if block.started {
            block.text.push('\n');
        }
        return true;
    }
    if raw.trim_start_matches(' ').starts_with('\t') {
        append_block_line(block, raw.trim());
        return true;
    }
    let Some(indent) = space_indent(raw) else {
        return false;
    };
    if let Some(content_indent) = block.content_indent {
        if indent >= content_indent && content_indent <= raw.len() {
            append_block_line(block, &raw[content_indent..]);
            return true;
        }
        return false;
    }
    if indent > block.key_indent {
        block.content_indent = Some(indent);
        append_block_line(block, &raw[indent..]);
        return true;
    }
    false
}

fn attach_block(lines: &mut [Line], block: Block) {
    let Some(last) = lines.last_mut() else {
        return;
    };
    let text = block.text.trim_end_matches(['\r', '\n']);
    if !text.is_empty() {
        last.block_text = Some(text.to_string());
    }
}

fn preprocess(text: &str) -> Vec<Line> {
    let raw_lines: Vec<&str> = text.split('\n').collect();
    let mut lines = Vec::new();
    let mut block: Option<Block> = None;
    let mut index = 0;
    while index < raw_lines.len() {
        let number = index + 1;
        let raw = raw_lines[index]
            .strip_suffix('\r')
            .unwrap_or(raw_lines[index]);
        if block.is_some() {
            let keep = block
                .as_mut()
                .is_some_and(|open| keep_block_line(open, raw));
            if keep {
                index += 1;
                continue;
            }
            if let Some(finished) = block.take() {
                attach_block(&mut lines, finished);
            }
            continue;
        }

        index += 1;
        if is_ignorable(raw) {
            continue;
        }
        let Some(indent) = space_indent(raw) else {
            continue;
        };
        if indent > raw.len() {
            continue;
        }
        let content = strip_inline_comment(&raw[indent..]);
        if content.is_empty() {
            continue;
        }
        let Some(mut parsed) = parse_content(number, indent, content) else {
            continue;
        };
        let opens_block = parsed.value.as_deref().is_some_and(is_block_scalar);
        if opens_block {
            parsed.value = None;
        }
        lines.push(parsed);
        if opens_block {
            block = Some(Block {
                key_indent: indent,
                content_indent: None,
                text: String::new(),
                started: false,
            });
        }
    }
    if let Some(finished) = block.take() {
        attach_block(&mut lines, finished);
    }
    lines
}

struct Frame {
    indent: isize,
    key: String,
}

struct Context {
    parent_key: String,
    parent_is_root: bool,
    on_is_root: bool,
    jobs_are_root: bool,
    concurrency_is_root: bool,
    parent_is_job: bool,
    in_steps: bool,
    in_runs_on: bool,
    job_name: Option<String>,
}

fn root_section_index(stack: &[Frame], name: &str) -> Option<usize> {
    stack.iter().enumerate().find_map(|(index, frame)| {
        if index > 0 && frame.key == name && stack[index - 1].key.is_empty() {
            Some(index)
        } else {
            None
        }
    })
}

fn in_job_runs_on(stack: &[Frame], jobs_at: Option<usize>) -> bool {
    let Some(jobs_at) = jobs_at else {
        return false;
    };
    let Some(after_job) = stack.get(jobs_at + 2..) else {
        return false;
    };
    match after_job {
        [frame] if frame.key == "runs-on" => true,
        [runs_on, labels] if runs_on.key == "runs-on" && labels.key == "labels" => true,
        _ => false,
    }
}

fn context_for(stack: &[Frame]) -> Context {
    let parent_key = stack
        .last()
        .map(|frame| frame.key.clone())
        .unwrap_or_default();
    let jobs_at = root_section_index(stack, "jobs");
    let job_name = jobs_at.and_then(|index| {
        stack.get(index + 1).and_then(|frame| {
            if frame.key.is_empty() || frame.key == "-" {
                None
            } else {
                Some(frame.key.clone())
            }
        })
    });
    Context {
        parent_is_root: stack.len() == 1,
        on_is_root: root_section_index(stack, "on").is_some(),
        jobs_are_root: jobs_at.is_some(),
        concurrency_is_root: root_section_index(stack, "concurrency").is_some(),
        parent_is_job: jobs_at.is_some_and(|index| stack.len() == index + 2),
        in_steps: jobs_at.is_some_and(|index| {
            stack
                .iter()
                .skip(index + 2)
                .any(|frame| frame.key == "steps")
        }),
        in_runs_on: in_job_runs_on(stack, jobs_at),
        parent_key,
        job_name,
    }
}

fn split_top_level(inner: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut braces: usize = 0;
    let mut brackets: usize = 0;
    let mut in_single = false;
    let mut in_double = false;
    for (index, character) in inner.char_indices() {
        if character == '"' && !in_single {
            in_double = !in_double;
            continue;
        }
        if character == '\'' && !in_double {
            in_single = !in_single;
            continue;
        }
        if in_single || in_double {
            continue;
        }
        match character {
            '{' => braces += 1,
            '}' => braces = braces.saturating_sub(1),
            '[' => brackets += 1,
            ']' => brackets = brackets.saturating_sub(1),
            _ if character == separator && braces == 0 && brackets == 0 => {
                parts.push(&inner[start..index]);
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&inner[start..]);
    parts
}

fn parse_flow_seq(value: &str) -> Option<Vec<String>> {
    let value = value.trim();
    let inner = value.strip_prefix('[')?.strip_suffix(']')?;
    let mut items = Vec::new();
    for part in split_top_level(inner, ',') {
        let part = part.trim();
        if !part.is_empty() {
            items.push(unquote(part).to_string());
        }
    }
    Some(items)
}

fn parse_flow_map(value: &str) -> Option<Vec<(String, String)>> {
    let value = value.trim();
    let inner = value.strip_prefix('{')?.strip_suffix('}')?;
    let mut entries = Vec::new();
    for part in split_top_level(inner, ',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (key_raw, raw_value) = split_colon(part)?;
        let key = normalize_key(key_raw)?;
        entries.push((key, raw_value.trim().to_string()));
    }
    Some(entries)
}

fn value_has_pull_request(value: &str) -> bool {
    let value = value.trim();
    if value.starts_with('[') {
        return parse_flow_seq(value)
            .is_some_and(|items| items.iter().any(|item| item == "pull_request"));
    }
    if value.starts_with('{') {
        return parse_flow_map(value)
            .is_some_and(|entries| entries.iter().any(|(key, _)| key == "pull_request"));
    }
    unquote(value) == "pull_request"
}

fn is_true(value: &str) -> bool {
    unquote(value).eq_ignore_ascii_case("true")
}

fn labels_from_inline(value: &str) -> Vec<String> {
    let value = value.trim();
    if value.is_empty() {
        return Vec::new();
    }
    if value.starts_with('[') {
        return parse_flow_seq(value).unwrap_or_default();
    }
    if value.starts_with('{') {
        return parse_flow_map(value)
            .and_then(|entries| {
                entries
                    .into_iter()
                    .find(|(key, _)| key == "labels")
                    .map(|(_, raw)| raw)
            })
            .and_then(|raw| parse_flow_seq(&raw))
            .unwrap_or_default();
    }
    vec![unquote(value).to_string()]
}

fn runs_on_value(raw: &str, labels: &[String]) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        if labels.is_empty() {
            String::new()
        } else {
            format!("[{}]", labels.join(", "))
        }
    } else if trimmed.starts_with('[') || trimmed.starts_with('{') {
        trimmed.to_string()
    } else {
        unquote(trimmed).to_string()
    }
}

fn line_body(line: &Line) -> String {
    if let Some(text) = &line.block_text {
        text.clone()
    } else {
        line.value
            .as_deref()
            .map(|value| unquote(value).to_string())
            .unwrap_or_default()
    }
}

fn job_mut<'a>(jobs: &'a mut [Job], name: &str) -> Option<&'a mut Job> {
    jobs.iter_mut().rev().find(|job| job.name == name)
}

fn note_pull_request(workflow: &mut Workflow, line: &Line) {
    let key_matches = line.key.as_deref() == Some("pull_request");
    let list_matches = line.is_list
        && line
            .value
            .as_deref()
            .is_some_and(|value| unquote(value) == "pull_request");
    if key_matches || list_matches {
        workflow.has_pull_request = true;
    }
}

fn note_root(workflow: &mut Workflow, line: &Line) {
    match line.key.as_deref() {
        Some("on") if !line.is_list => {
            if workflow.on_line.is_none() {
                workflow.on_line = Some(line.number);
            }
            if line.value.as_deref().is_some_and(value_has_pull_request) {
                workflow.has_pull_request = true;
            }
        }
        Some("concurrency")
            if !line.is_list
                && line
                    .value
                    .as_deref()
                    .is_some_and(|value| flow_flag_is_true(value, "cancel-in-progress")) =>
        {
            workflow.cancel_in_progress = true;
        }
        _ => {}
    }
}

fn flow_flag_is_true(value: &str, wanted: &str) -> bool {
    parse_flow_map(value).is_some_and(|entries| {
        entries
            .iter()
            .any(|(key, raw)| key == wanted && is_true(raw))
    })
}

fn is_job_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|character| character.is_ascii_alphanumeric() || character == '_' || character == '-')
}

fn start_job(workflow: &mut Workflow, line: &Line) {
    let Some(name) = line.key.clone() else {
        return;
    };
    workflow.jobs.push(Job {
        name,
        line: line.number,
        display_name: None,
        runs_on: None,
        has_timeout_minutes: false,
        steps_text: String::new(),
    });
}

fn note_job_field(job: &mut Job, line: &Line) {
    match line.key.as_deref() {
        Some("runs-on") if job.runs_on.is_none() => {
            let raw = line.value.clone().unwrap_or_default();
            let labels = labels_from_inline(&raw);
            let value = runs_on_value(&raw, &labels);
            job.runs_on = Some(RunsOn {
                line: line.number,
                value,
                labels,
            });
        }
        Some("timeout-minutes") => job.has_timeout_minutes = true,
        Some("name") if job.display_name.is_none() => {
            let body = line_body(line);
            if !body.is_empty() {
                job.display_name = Some(body);
            }
        }
        _ => {}
    }
}

fn note_runs_on_child(runs_on: &mut RunsOn, line: &Line) {
    if !runs_on.value.is_empty() {
        return;
    }
    if line.is_list {
        let body = line_body(line);
        let label = body.trim();
        if !label.is_empty() && !label.starts_with('[') && !label.starts_with('{') {
            runs_on.labels.push(label.to_string());
        }
        return;
    }
    if line.key.as_deref() == Some("labels") {
        if let Some(value) = line.value.as_deref() {
            if let Some(labels) = parse_flow_seq(value) {
                runs_on.labels.extend(labels);
            }
        }
    }
}

fn note_run_step(job: &mut Job, line: &Line) {
    if line.key.as_deref() != Some("run") {
        return;
    }
    let text = line_body(line);
    if text.is_empty() {
        return;
    }
    if !job.steps_text.is_empty() {
        job.steps_text.push('\n');
    }
    job.steps_text.push_str(&text);
}

fn apply_line(workflow: &mut Workflow, line: &Line, context: &Context) {
    if context.parent_is_root {
        note_root(workflow, line);
    }
    if context.on_is_root && context.parent_key == "on" {
        note_pull_request(workflow, line);
    }
    if context.concurrency_is_root
        && context.parent_key == "concurrency"
        && line.key.as_deref() == Some("cancel-in-progress")
        && is_true(&line_body(line))
    {
        workflow.cancel_in_progress = true;
    }
    if context.jobs_are_root
        && context.parent_key == "jobs"
        && !line.is_list
        && line.key.as_deref().is_some_and(is_job_name)
    {
        start_job(workflow, line);
    }
    if context.parent_is_job {
        if let Some(name) = &context.job_name {
            if let Some(job) = job_mut(&mut workflow.jobs, name) {
                note_job_field(job, line);
            }
        }
    }
    if context.in_runs_on {
        if let Some(name) = &context.job_name {
            if let Some(job) = job_mut(&mut workflow.jobs, name) {
                if let Some(runs_on) = &mut job.runs_on {
                    note_runs_on_child(runs_on, line);
                }
            }
        }
    }
    if context.in_steps {
        if let Some(name) = &context.job_name {
            if let Some(job) = job_mut(&mut workflow.jobs, name) {
                note_run_step(job, line);
            }
        }
    }
}

fn interpret(path: &Path, lines: &[Line]) -> Workflow {
    let mut workflow = Workflow {
        path: path.to_path_buf(),
        on_line: None,
        has_pull_request: false,
        cancel_in_progress: false,
        jobs: Vec::new(),
    };
    let mut stack = vec![Frame {
        indent: -1,
        key: String::new(),
    }];
    for line in lines {
        while stack.len() > 1
            && stack
                .last()
                .is_some_and(|frame| frame.indent >= line.indent as isize)
        {
            stack.pop();
        }
        if stack.is_empty() {
            stack.push(Frame {
                indent: -1,
                key: String::new(),
            });
        }
        let context = context_for(&stack);
        apply_line(&mut workflow, line, &context);
        stack.push(Frame {
            indent: line.indent as isize,
            key: line.key.clone().unwrap_or_else(|| "-".to_string()),
        });
    }
    for job in &mut workflow.jobs {
        if let Some(runs_on) = &mut job.runs_on {
            if runs_on.value.is_empty() && !runs_on.labels.is_empty() {
                runs_on.value = format!("[{}]", runs_on.labels.join(", "));
            }
        }
    }
    workflow
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/.github/workflows")
    }

    fn by_name<'a>(workflows: &'a [Workflow], name: &str) -> &'a Workflow {
        workflows
            .iter()
            .find(|workflow| workflow.path.file_name().and_then(|item| item.to_str()) == Some(name))
            .unwrap_or_else(|| panic!("missing {name}"))
    }

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir()
            .join("ci-bill-workflow-tests")
            .join(name);
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn sample_lint_and_ci_keep_file_line_numbers() {
        let workflows = scan_dir(&sample_dir()).unwrap();
        assert_eq!(workflows.len(), 4);

        let lint = by_name(&workflows, "lint.yml");
        assert_eq!(lint.on_line, Some(2));
        assert!(lint.has_pull_request);
        assert!(!lint.cancel_in_progress);
        assert_eq!(lint.jobs.len(), 1);
        assert_eq!(lint.jobs[0].name, "lint");
        assert_eq!(lint.jobs[0].line, 7);
        assert_eq!(lint.jobs[0].display_name, None);
        let runs_on = lint.jobs[0].runs_on.as_ref().unwrap();
        assert_eq!(runs_on.line, 8);
        assert_eq!(runs_on.value, "macos-latest");
        assert_eq!(runs_on.labels, vec!["macos-latest".to_string()]);
        assert!(!lint.jobs[0].has_timeout_minutes);
        assert_eq!(
            lint.jobs[0].steps_text,
            "cargo fmt --check\ncargo clippy -- -D warnings"
        );

        let ci = by_name(&workflows, "ci.yml");
        assert_eq!(ci.on_line, Some(2));
        assert!(ci.has_pull_request);
        assert!(ci.cancel_in_progress);
        assert_eq!(ci.jobs.len(), 1);
        assert_eq!(ci.jobs[0].name, "test");
        assert_eq!(ci.jobs[0].line, 8);
        let runs_on = ci.jobs[0].runs_on.as_ref().unwrap();
        assert_eq!(runs_on.line, 9);
        assert_eq!(runs_on.value, "ubuntu-latest");
        assert!(ci.jobs[0].has_timeout_minutes);
        assert_eq!(ci.jobs[0].steps_text, "cargo test");

        let docs = by_name(&workflows, "docs.yml");
        assert_eq!(docs.on_line, Some(2));
        assert!(!docs.has_pull_request);
        assert!(!docs.cancel_in_progress);
        assert_eq!(docs.jobs[0].name, "docs");
        assert_eq!(docs.jobs[0].line, 4);
        assert_eq!(
            docs.jobs[0].runs_on.as_ref().unwrap().value,
            "ubuntu-latest"
        );
        assert!(!docs.jobs[0].has_timeout_minutes);

        let release = by_name(&workflows, "release.yml");
        assert!(!release.has_pull_request);
        assert!(!release.cancel_in_progress);
        assert_eq!(release.jobs[0].name, "build-windows");
        assert_eq!(release.jobs[0].line, 6);
        assert_eq!(
            release.jobs[0].runs_on.as_ref().unwrap().value,
            "windows-latest"
        );
        assert!(release.jobs[0].has_timeout_minutes);
        assert_eq!(release.jobs[0].steps_text, "cargo build --release");
    }

    #[test]
    fn comments_blank_lines_and_runs_on_list() {
        let fixture = r#"# leading comment

name: Lint # name comment

on: # triggers
  push:

  # blank and comment before pull_request
  pull_request:

concurrency:
  group: lint-${{ github.ref }}
  cancel-in-progress: false

jobs:

  # job comment
  lint:
    runs-on: [self-hosted, macos] # runners

    steps:
      - uses: actions/checkout@v4

      - run: cargo fmt --check
      - run: |
          cargo clippy -- -D warnings
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 15
    steps:
      - run: cargo test
"#;
        let workflow = scan_yaml(Path::new("lint-like.yml"), fixture);
        assert_eq!(workflow.on_line, Some(5));
        assert!(workflow.has_pull_request);
        assert!(!workflow.cancel_in_progress);
        assert_eq!(workflow.jobs.len(), 2);

        let lint = &workflow.jobs[0];
        assert_eq!(lint.name, "lint");
        assert_eq!(lint.line, 18);
        let runs_on = lint.runs_on.as_ref().unwrap();
        assert_eq!(runs_on.line, 19);
        assert_eq!(runs_on.value, "[self-hosted, macos]");
        assert_eq!(
            runs_on.labels,
            vec!["self-hosted".to_string(), "macos".to_string()]
        );
        assert!(!lint.has_timeout_minutes);
        assert_eq!(
            lint.steps_text,
            "cargo fmt --check\ncargo clippy -- -D warnings"
        );

        let test = &workflow.jobs[1];
        assert_eq!(test.name, "test");
        assert_eq!(test.line, 27);
        assert_eq!(test.runs_on.as_ref().unwrap().line, 28);
        assert_eq!(test.runs_on.as_ref().unwrap().value, "ubuntu-latest");
        assert!(test.has_timeout_minutes);
        assert_eq!(test.steps_text, "cargo test");
    }

    #[test]
    fn quoted_and_block_runs_on_lists() {
        let quoted = scan_yaml(
            Path::new("quoted.yml"),
            "jobs:\n  lint:\n    runs-on: [\"self-hosted\", \"macos\"]\n",
        );
        let runs_on = quoted.jobs[0].runs_on.as_ref().unwrap();
        assert_eq!(runs_on.line, 3);
        assert_eq!(
            runs_on.labels,
            vec!["self-hosted".to_string(), "macos".to_string()]
        );

        let blocked = scan_yaml(
            Path::new("block.yml"),
            r#"jobs:
  lint:
    runs-on:
      - self-hosted
      - macos
    steps:
      - run: |
          echo one
          echo two
"#,
        );
        let job = &blocked.jobs[0];
        assert_eq!(job.line, 2);
        let runs_on = job.runs_on.as_ref().unwrap();
        assert_eq!(runs_on.line, 3);
        assert_eq!(
            runs_on.labels,
            vec!["self-hosted".to_string(), "macos".to_string()]
        );
        assert_eq!(runs_on.value, "[self-hosted, macos]");
        assert_eq!(job.steps_text, "echo one\necho two");
    }

    #[test]
    fn inline_triggers_and_concurrency() {
        let pull_request = scan_yaml(Path::new("on.yml"), "on: [push, pull_request]\n");
        assert_eq!(pull_request.on_line, Some(1));
        assert!(pull_request.has_pull_request);
        assert!(!pull_request.cancel_in_progress);

        let other = scan_yaml(Path::new("on.yml"), "on: [push, pull_request_target]\n");
        assert!(!other.has_pull_request);

        let scalar = scan_yaml(Path::new("on.yml"), "on: pull_request\n");
        assert!(scalar.has_pull_request);

        let mapping = scan_yaml(
            Path::new("on.yml"),
            "on: {push: null, pull_request: {types: [opened]}}\nconcurrency: {group: ci, cancel-in-progress: true}\n",
        );
        assert!(mapping.has_pull_request);
        assert!(mapping.cancel_in_progress);
    }

    #[test]
    fn unknown_structure_is_skipped() {
        let samples = [
            "",
            "::::\n{{\n",
            "jobs: [\n",
            "jobs:\n  - just-a-list\n",
            "\tjobs:\n\t\tlint:\n",
            "on: {{{{pull_request:\n",
            "jobs:\n  good:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo ok\n  :::\n    not: [valid\n",
        ];
        for text in samples {
            let workflow = scan_yaml(Path::new("odd.yml"), text);
            let _ = (
                workflow.on_line,
                workflow.has_pull_request,
                workflow.cancel_in_progress,
                workflow.jobs.len(),
            );
        }
        let workflow = scan_yaml(
            Path::new("odd.yml"),
            "jobs:\n  good:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo ok\n  :::\n",
        );
        assert_eq!(workflow.jobs.len(), 1);
        assert_eq!(workflow.jobs[0].name, "good");
        assert_eq!(
            workflow.jobs[0].runs_on.as_ref().unwrap().value,
            "ubuntu-latest"
        );
        assert_eq!(workflow.jobs[0].steps_text, "echo ok");
    }

    #[test]
    fn reads_yml_and_yaml_but_not_nested_files() {
        let dir = scratch("workflow-ext");
        fs::write(
            dir.join("a.yml"),
            "jobs:\n  build:\n    runs-on: ubuntu-latest\n",
        )
        .unwrap();
        fs::write(dir.join("b.yaml"), "name: B\n").unwrap();
        fs::write(dir.join("notes.txt"), "jobs:\n  hidden:\n").unwrap();
        let nested = dir.join("nested");
        fs::create_dir(&nested).unwrap();
        fs::write(
            nested.join("c.yml"),
            "jobs:\n  hidden:\n    runs-on: macos-latest\n",
        )
        .unwrap();

        let found = scan_dir(&dir).unwrap();
        let names: Vec<_> = found
            .iter()
            .filter_map(|workflow| {
                workflow
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_string)
            })
            .collect();
        assert_eq!(names, vec!["a.yml".to_string(), "b.yaml".to_string()]);
        assert_eq!(found[0].jobs[0].name, "build");
        assert!(found[1].jobs.is_empty());
    }

    #[test]
    fn missing_directory_is_an_error() {
        let err = scan_dir(Path::new("/no/such/ci-bill-workflows")).unwrap_err();
        assert_eq!(err.to_string(), "cannot read directory");
        assert_eq!(err.path, Path::new("/no/such/ci-bill-workflows"));
    }
}
