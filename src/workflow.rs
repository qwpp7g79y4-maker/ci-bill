use std::collections::{HashMap, HashSet};
use std::fs;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};

use yaml_rust2::parser::{MarkedEventReceiver, Parser};
use yaml_rust2::scanner::Marker;
use yaml_rust2::Event;

/// One workflow file.
///
/// Line numbers are 1-based positions of keys in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workflow {
    pub path: PathBuf,
    /// Line of the top-level `on:` key, when present.
    pub on_line: Option<usize>,
    pub has_pull_request: bool,
    /// `pull_request_target` is present under `on:`.
    pub has_pull_request_target: bool,
    /// Top-level `concurrency.cancel-in-progress` cancels in-progress runs.
    /// Literal `true` and `github.event_name == 'pull_request'` both count.
    pub cancel_in_progress: bool,
    pub jobs: Vec<Job>,
    /// Repeated mapping keys, in document order. The later value won.
    pub duplicate_keys: Vec<DuplicateKey>,
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
    /// `strategy.matrix`, when the job has one.
    pub matrix: Option<Matrix>,
    /// Job-level `concurrency.cancel-in-progress` cancels in-progress runs.
    pub cancel_in_progress: bool,
    /// `uses:` calling a local or remote reusable workflow.
    pub uses: Option<ReusableCall>,
    /// Job-level `uses:` that is not a reusable workflow call.
    pub invalid_uses: Option<InvalidUses>,
}

/// A job calling a reusable workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReusableCall {
    /// Line of the `uses:` key.
    pub line: usize,
    /// Scalar text, for example `./.github/workflows/lint.yml`.
    pub spec: String,
    /// File name the call asks for, for example `lint.yml`.
    pub file_name: String,
    /// Local scan or a remote repository pinned to a ref.
    pub source: ReusableSource,
}

/// Where the called workflow file lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReusableSource {
    /// `./.github/workflows/<file>.yml` in the scanned directory.
    Local,
    /// `owner/repo/.github/workflows/<file>.yml@ref` in the runs cache.
    Remote(RemoteWorkflow),
}

/// A reusable workflow in another repository, pinned to a tag, branch, or SHA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteWorkflow {
    pub owner: String,
    pub repo: String,
    /// Path inside the repository, for example `.github/workflows/lint.yml`.
    pub workflow_path: String,
    /// Tag, branch, or commit SHA. A branch may contain `/`.
    pub reference: String,
}

impl RemoteWorkflow {
    /// Relative runs-cache path `<owner>/<repo>/<workflow_path>@<reference>`.
    ///
    /// A reference that contains `/` continues as further path components.
    /// `lint.yml` at ref `release/1.2` is `lint.yml@release/1.2`.
    pub fn cache_relative(&self) -> PathBuf {
        let mut path = PathBuf::new();
        path.push(&self.owner);
        path.push(&self.repo);
        let mut parts = self
            .workflow_path
            .split('/')
            .filter(|part| !part.is_empty());
        let file = parts.next_back().unwrap_or("");
        for part in parts {
            path.push(part);
        }
        let mut refs = self.reference.split('/').filter(|part| !part.is_empty());
        let first = refs.next().unwrap_or("");
        path.push(format!("{file}@{first}"));
        for part in refs {
            path.push(part);
        }
        path
    }
}

/// Job-level `uses:` that is not a local or remote reusable workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidUses {
    /// Line of the `uses:` key.
    pub line: usize,
    /// Quoted scalar, or `a list` / `a mapping`.
    pub detail: String,
}

/// `runs-on` for a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunsOn {
    /// Line of the `runs-on` key.
    pub line: usize,
    /// Runner text, for example `ubuntu-latest` or `[self-hosted, macos]`.
    pub value: String,
    pub labels: Vec<String>,
}

/// How many jobs one `strategy.matrix` starts.
///
/// `jobs` is the expanded combination count. When an axis value is a
/// `${{ ... }}` expression, `at_least` is set and `jobs` is the product of the
/// other axes (each expression axis contributes at least one value).
/// A dynamic `exclude` stores a finite upper bound in `jobs_hi` and the lower
/// bound in `jobs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Matrix {
    /// Line of the `matrix:` key.
    pub line: usize,
    /// Exact count, or the lower bound when `at_least` or `jobs_hi` is set.
    pub jobs: u64,
    pub at_least: bool,
    /// Finite upper bound, present only when it is greater than `jobs`.
    pub jobs_hi: Option<u64>,
    /// Literal axis and include values, used to resolve `matrix.<name>` in `runs-on`.
    pub axes: Vec<MatrixAxis>,
}

/// One matrix axis, including keys that appear only under `include`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatrixAxis {
    pub name: String,
    /// Line of the axis key. Include-only keys use the first include key line.
    pub line: usize,
    /// Each entry is one literal label set. Expressions are left out.
    pub values: Vec<Vec<String>>,
}

/// One repeated mapping key. The later line wins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateKey {
    pub key: String,
    /// Line of the earlier occurrence.
    pub first: usize,
    /// Line of the later occurrence, whose value is kept.
    pub second: usize,
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
    Yaml { line: usize },
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            ScanErrorKind::Directory => write!(f, "cannot read directory"),
            ScanErrorKind::File => write!(f, "cannot read file"),
            ScanErrorKind::Yaml { line } => write!(f, "line {line}: invalid YAML"),
        }
    }
}

impl std::error::Error for ScanError {}

impl ScanError {
    pub(crate) fn directory(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            kind: ScanErrorKind::Directory,
        }
    }

    pub(crate) fn file(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            kind: ScanErrorKind::File,
        }
    }

    fn yaml(path: &Path, line: usize) -> Self {
        Self {
            path: path.to_path_buf(),
            kind: ScanErrorKind::Yaml { line: line.max(1) },
        }
    }
}

/// Read `*.yml` and `*.yaml` files directly in `dir`, in path order.
///
/// Nested directories are not walked. An unreadable directory or file is an
/// error. Invalid YAML is an error that names the file and the line.
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
        workflows.push(scan_yaml(&path, &text)?);
    }
    Ok(workflows)
}

/// Scan one workflow document.
///
/// Invalid YAML returns an error with the 1-based line and without the
/// document text. Every other input returns a workflow.
pub fn scan_yaml(path: &Path, text: &str) -> Result<Workflow, ScanError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let events = collect_events(text).map_err(|line| ScanError::yaml(path, line))?;
    let root = document_root(&events).map_err(|line| ScanError::yaml(path, line))?;
    Ok(workflow_from(path, root.as_ref()))
}

fn is_workflow_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("yml") || ext.eq_ignore_ascii_case("yaml"))
}

const MAX_NODE_DEPTH: usize = 128;

struct Collector {
    events: Vec<(Event, Marker)>,
}

impl MarkedEventReceiver for Collector {
    fn on_event(&mut self, ev: Event, mark: Marker) {
        self.events.push((ev, mark));
    }
}

fn collect_events(text: &str) -> Result<Vec<(Event, Marker)>, usize> {
    let caught = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mut collector = Collector { events: Vec::new() };
        let mut parser = Parser::new_from_str(text);
        match parser.load(&mut collector, true) {
            Ok(()) => Ok(collector.events),
            Err(err) => Err(err.marker().line().max(1)),
        }
    }));
    match caught {
        Ok(result) => result,
        Err(_) => Err(1),
    }
}

#[derive(Clone)]
enum Node {
    Scalar {
        value: String,
        line: usize,
    },
    Sequence {
        items: Vec<Node>,
    },
    Mapping {
        entries: Vec<Entry>,
        duplicates: Vec<DuplicateKey>,
    },
}

#[derive(Clone)]
struct Entry {
    key: String,
    line: usize,
    value: Node,
}

struct Cursor<'a> {
    events: &'a [(Event, Marker)],
    index: usize,
}

fn document_root(events: &[(Event, Marker)]) -> Result<Option<Node>, usize> {
    let mut cursor = Cursor { events, index: 0 };
    let mut chosen = None;
    while cursor.index < events.len() {
        let is_document = matches!(events[cursor.index].0, Event::DocumentStart);
        if !is_document {
            cursor.index += 1;
            continue;
        }
        cursor.index += 1;
        if cursor.index >= events.len() {
            break;
        }
        let boundary = matches!(
            events[cursor.index].0,
            Event::DocumentEnd | Event::StreamEnd
        );
        if boundary {
            continue;
        }
        let mut anchors = HashMap::new();
        let mut building = Vec::new();
        let node = cursor.parse_node(&mut anchors, &mut building, 0)?;
        if chosen.is_none() && matches!(node, Node::Mapping { .. }) {
            chosen = Some(node);
        }
    }
    Ok(chosen)
}

impl Cursor<'_> {
    fn parse_node(
        &mut self,
        anchors: &mut HashMap<usize, Node>,
        building: &mut Vec<usize>,
        depth: usize,
    ) -> Result<Node, usize> {
        if self.index >= self.events.len() {
            return Err(1);
        }
        let line = self.events[self.index].1.line().max(1);
        if depth > MAX_NODE_DEPTH {
            return Err(line);
        }
        let event = self.events[self.index].0.clone();
        self.index += 1;
        match event {
            Event::Scalar(value, _, anchor, _) => {
                let node = Node::Scalar { value, line };
                self.store_anchor(anchors, anchor, &node);
                Ok(node)
            }
            Event::SequenceStart(anchor, _) => {
                self.parse_sequence(anchors, building, depth, anchor, line)
            }
            Event::MappingStart(anchor, _) => {
                self.parse_mapping(anchors, building, depth, anchor, line)
            }
            Event::Alias(id) => {
                if building.contains(&id) {
                    return Err(line);
                }
                anchors.get(&id).cloned().ok_or(line)
            }
            _ => Err(line),
        }
    }

    fn parse_sequence(
        &mut self,
        anchors: &mut HashMap<usize, Node>,
        building: &mut Vec<usize>,
        depth: usize,
        anchor: usize,
        line: usize,
    ) -> Result<Node, usize> {
        self.push_anchor(building, anchor, line)?;
        let mut items = Vec::new();
        loop {
            if self.index >= self.events.len() {
                return Err(line);
            }
            if matches!(self.events[self.index].0, Event::SequenceEnd) {
                self.index += 1;
                break;
            }
            items.push(self.parse_node(anchors, building, depth + 1)?);
        }
        let node = Node::Sequence { items };
        self.finish_anchor(anchors, building, anchor, &node);
        Ok(node)
    }

    fn parse_mapping(
        &mut self,
        anchors: &mut HashMap<usize, Node>,
        building: &mut Vec<usize>,
        depth: usize,
        anchor: usize,
        line: usize,
    ) -> Result<Node, usize> {
        self.push_anchor(building, anchor, line)?;
        let mut raw = Vec::new();
        loop {
            if self.index >= self.events.len() {
                return Err(line);
            }
            if matches!(self.events[self.index].0, Event::MappingEnd) {
                self.index += 1;
                break;
            }
            let key = self.parse_node(anchors, building, depth + 1)?;
            let value = self.parse_node(anchors, building, depth + 1)?;
            if let Node::Scalar {
                value: key_text,
                line: key_line,
            } = key
            {
                raw.push(Entry {
                    key: key_text,
                    line: key_line,
                    value,
                });
            }
        }
        let (entries, duplicates) = resolve_merge(raw);
        let node = Node::Mapping {
            entries,
            duplicates,
        };
        self.finish_anchor(anchors, building, anchor, &node);
        Ok(node)
    }

    fn push_anchor(
        &self,
        building: &mut Vec<usize>,
        anchor: usize,
        line: usize,
    ) -> Result<(), usize> {
        if anchor == 0 {
            return Ok(());
        }
        if building.contains(&anchor) {
            return Err(line);
        }
        building.push(anchor);
        Ok(())
    }

    fn finish_anchor(
        &self,
        anchors: &mut HashMap<usize, Node>,
        building: &mut Vec<usize>,
        anchor: usize,
        node: &Node,
    ) {
        self.store_anchor(anchors, anchor, node);
        if anchor != 0 {
            building.pop();
        }
    }

    fn store_anchor(&self, anchors: &mut HashMap<usize, Node>, anchor: usize, node: &Node) {
        if anchor != 0 {
            anchors.insert(anchor, node.clone());
        }
    }
}

/// Last explicit value wins. `<<` fills only keys that were not written explicitly,
/// so an explicit key still wins whether it is written before or after the merge.
fn resolve_merge(entries: Vec<Entry>) -> (Vec<Entry>, Vec<DuplicateKey>) {
    let mut explicit: Vec<Entry> = Vec::new();
    let mut merge: Option<Node> = None;
    let mut merge_line: Option<usize> = None;
    let mut duplicates: Vec<DuplicateKey> = Vec::new();
    for item in entries {
        if item.key == "<<" {
            if let Some(previous) = merge_line {
                duplicates.push(DuplicateKey {
                    key: "<<".to_string(),
                    first: previous,
                    second: item.line,
                });
            }
            merge_line = Some(item.line);
            merge = Some(item.value);
            continue;
        }
        if let Some(pos) = explicit.iter().position(|entry| entry.key == item.key) {
            duplicates.push(DuplicateKey {
                key: item.key.clone(),
                first: explicit[pos].line,
                second: item.line,
            });
            explicit[pos] = item;
        } else {
            explicit.push(item);
        }
    }
    if let Some(value) = merge {
        let mut seen: HashSet<String> = explicit.iter().map(|entry| entry.key.clone()).collect();
        merge_node(&mut explicit, &mut seen, &value);
    }
    (explicit, duplicates)
}

fn collect_duplicates(node: &Node, out: &mut Vec<DuplicateKey>) {
    match node {
        Node::Mapping {
            entries,
            duplicates,
        } => {
            out.extend(duplicates.iter().cloned());
            for item in entries {
                collect_duplicates(&item.value, out);
            }
        }
        Node::Sequence { items } => {
            for item in items {
                collect_duplicates(item, out);
            }
        }
        Node::Scalar { .. } => {}
    }
}

fn dedupe_duplicates(items: Vec<DuplicateKey>) -> Vec<DuplicateKey> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for item in items {
        if seen.insert((item.key.clone(), item.first, item.second)) {
            out.push(item);
        }
    }
    out
}

fn merge_node(out: &mut Vec<Entry>, seen: &mut HashSet<String>, node: &Node) {
    match node {
        Node::Mapping { entries, .. } => {
            for item in entries {
                if item.key == "<<" {
                    merge_node(out, seen, &item.value);
                } else if seen.insert(item.key.clone()) {
                    out.push(item.clone());
                }
            }
        }
        Node::Sequence { items, .. } => {
            for item in items {
                merge_node(out, seen, item);
            }
        }
        Node::Scalar { .. } => {}
    }
}

fn entry<'a>(entries: &'a [Entry], key: &str) -> Option<&'a Entry> {
    entries.iter().find(|item| item.key == key)
}

fn workflow_from(path: &Path, root: Option<&Node>) -> Workflow {
    let mut duplicate_keys = Vec::new();
    if let Some(node) = root {
        collect_duplicates(node, &mut duplicate_keys);
    }
    let mut workflow = Workflow {
        path: path.to_path_buf(),
        on_line: None,
        has_pull_request: false,
        has_pull_request_target: false,
        cancel_in_progress: false,
        jobs: Vec::new(),
        duplicate_keys: dedupe_duplicates(duplicate_keys),
    };
    let Some(Node::Mapping { entries, .. }) = root else {
        return workflow;
    };
    for item in entries {
        match item.key.as_str() {
            "on" => note_on(&mut workflow, item),
            "concurrency" => note_concurrency(&mut workflow, &item.value),
            "jobs" => note_jobs(&mut workflow, &item.value),
            _ => {}
        }
    }
    workflow
}

fn note_on(workflow: &mut Workflow, item: &Entry) {
    if workflow.on_line.is_none() {
        workflow.on_line = Some(item.line);
    }
    if has_event(&item.value, "pull_request") {
        workflow.has_pull_request = true;
    }
    if has_event(&item.value, "pull_request_target") {
        workflow.has_pull_request_target = true;
    }
}

fn has_event(node: &Node, event: &str) -> bool {
    match node {
        Node::Scalar { value, .. } => value.eq_ignore_ascii_case(event),
        Node::Sequence { items, .. } => items.iter().any(
            |item| matches!(item, Node::Scalar { value, .. } if value.eq_ignore_ascii_case(event)),
        ),
        Node::Mapping { entries, .. } => entries
            .iter()
            .any(|item| item.key.eq_ignore_ascii_case(event)),
    }
}

fn note_concurrency(workflow: &mut Workflow, node: &Node) {
    if concurrency_enabled(node) {
        workflow.cancel_in_progress = true;
    }
}

fn concurrency_enabled(node: &Node) -> bool {
    let Node::Mapping { entries, .. } = node else {
        return false;
    };
    entry(entries, "cancel-in-progress").is_some_and(|item| scalar_enables_cancel(&item.value))
}

fn scalar_enables_cancel(node: &Node) -> bool {
    match node {
        Node::Scalar { value, .. } => expression_enables_cancel(value),
        _ => false,
    }
}

/// Literal `true`, or `github.event_name == 'pull_request'` (either quote, any
/// spacing, optional `${{ }}`). `pull_request_target` uses the same comparison.
/// An `||` chain counts when any clause does.
fn expression_enables_cancel(value: &str) -> bool {
    clause_enables_cancel(value.trim())
}

fn clause_enables_cancel(value: &str) -> bool {
    let unwrapped = unwrap_expr(value.trim());
    let text = strip_wrapping_parens(unwrapped.trim());
    if text.eq_ignore_ascii_case("true") {
        return true;
    }
    let parts = split_top_or(text);
    if parts.len() > 1 {
        return parts.iter().any(|part| clause_enables_cancel(part));
    }
    is_pull_event_comparison(text)
}

fn unwrap_expr(text: &str) -> &str {
    let mut current = text.trim();
    loop {
        let Some(rest) = current.strip_prefix("${{") else {
            return current;
        };
        let Some(inner) = rest.strip_suffix("}}") else {
            return current;
        };
        if inner.contains("${{") || inner.contains("}}") {
            return current;
        }
        current = inner.trim();
    }
}

fn strip_wrapping_parens(text: &str) -> &str {
    let mut current = text.trim();
    while current.starts_with('(') && current.ends_with(')') && parens_wrap_all(current) {
        current = current[1..current.len() - 1].trim();
    }
    current
}

fn parens_wrap_all(text: &str) -> bool {
    let mut depth = 0i32;
    for (index, character) in text.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
                if depth == 0 && index != text.len() - 1 {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}

fn split_top_or(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut quote: Option<char> = None;
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut index = 0;
    while index < chars.len() {
        let (byte, character) = chars[index];
        if let Some(open) = quote {
            if character == open {
                quote = None;
            }
            index += 1;
            continue;
        }
        if character == '\'' || character == '"' {
            quote = Some(character);
            index += 1;
            continue;
        }
        if character == '|' && chars.get(index + 1).is_some_and(|(_, next)| *next == '|') {
            parts.push(&text[start..byte]);
            start = chars[index + 1].0 + 1;
            index += 2;
            continue;
        }
        index += 1;
    }
    parts.push(&text[start..]);
    parts
}

fn is_pull_event_comparison(text: &str) -> bool {
    let Some(rest) = strip_prefix_ignore_ascii_case(text.trim(), "github.event_name") else {
        return false;
    };
    if rest.starts_with(|character: char| character.is_ascii_alphanumeric() || character == '_') {
        return false;
    }
    let rest = rest.trim_start();
    let Some(rest) = rest.strip_prefix("==") else {
        return false;
    };
    if rest.starts_with('=') {
        return false;
    }
    let rest = rest.trim_start();
    let Some((body, rest)) = quoted_literal(rest) else {
        return false;
    };
    if !rest.trim().is_empty() {
        return false;
    }
    body.eq_ignore_ascii_case("pull_request") || body.eq_ignore_ascii_case("pull_request_target")
}

fn strip_prefix_ignore_ascii_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let mut rest = text;
    for expected in prefix.chars() {
        let mut chars = rest.chars();
        let got = chars.next()?;
        if !got.eq_ignore_ascii_case(&expected) {
            return None;
        }
        rest = chars.as_str();
    }
    Some(rest)
}

fn quoted_literal(text: &str) -> Option<(&str, &str)> {
    let quote = text.chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    let rest = &text[quote.len_utf8()..];
    let end = rest.find(quote)?;
    Some((&rest[..end], &rest[end + quote.len_utf8()..]))
}

fn note_jobs(workflow: &mut Workflow, node: &Node) {
    let Node::Mapping { entries, .. } = node else {
        return;
    };
    for item in entries {
        if !is_job_name(&item.key) {
            continue;
        }
        let mut job = Job {
            name: item.key.clone(),
            line: item.line,
            display_name: None,
            runs_on: None,
            has_timeout_minutes: false,
            steps_text: String::new(),
            matrix: None,
            cancel_in_progress: false,
            uses: None,
            invalid_uses: None,
        };
        if let Node::Mapping { entries, .. } = &item.value {
            fill_job(&mut job, entries);
        }
        workflow.jobs.push(job);
    }
}

fn is_job_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|character| character.is_ascii_alphanumeric() || character == '_' || character == '-')
}

fn fill_job(job: &mut Job, entries: &[Entry]) {
    if let Some(item) = entry(entries, "runs-on") {
        job.runs_on = Some(runs_on_from(item.line, &item.value));
    }
    if entry(entries, "timeout-minutes").is_some() {
        job.has_timeout_minutes = true;
    }
    if let Some(item) = entry(entries, "name") {
        if let Some(text) = scalar_text(&item.value) {
            let text = trim_breaks(text);
            if !text.is_empty() {
                job.display_name = Some(text);
            }
        }
    }
    if let Some(item) = entry(entries, "steps") {
        job.steps_text = step_runs(&item.value);
    }
    if let Some(item) = entry(entries, "strategy") {
        job.matrix = matrix_from(&item.value);
    }
    if let Some(item) = entry(entries, "concurrency") {
        job.cancel_in_progress = concurrency_enabled(&item.value);
    }
    if let Some(item) = entry(entries, "uses") {
        match classify_uses(item.line, &item.value) {
            UsesClass::Call(call) => job.uses = Some(call),
            UsesClass::Invalid(invalid) => job.invalid_uses = Some(invalid),
        }
    }
}

enum UsesClass {
    Call(ReusableCall),
    Invalid(InvalidUses),
}

fn classify_uses(line: usize, node: &Node) -> UsesClass {
    match node {
        Node::Scalar { value, .. } => classify_uses_scalar(line, value),
        Node::Sequence { .. } => UsesClass::Invalid(InvalidUses {
            line,
            detail: "a list".to_string(),
        }),
        Node::Mapping { .. } => UsesClass::Invalid(InvalidUses {
            line,
            detail: "a mapping".to_string(),
        }),
    }
}

fn classify_uses_scalar(line: usize, raw: &str) -> UsesClass {
    let text = trim_breaks(raw);
    let text = text.trim();
    if text.is_empty() {
        return UsesClass::Invalid(InvalidUses {
            line,
            detail: "\"\"".to_string(),
        });
    }
    if let Some(call) = local_reusable_call(line, text) {
        return UsesClass::Call(call);
    }
    if let Some(call) = remote_reusable_call(line, text) {
        return UsesClass::Call(call);
    }
    UsesClass::Invalid(InvalidUses {
        line,
        detail: format!("{text:?}"),
    })
}

/// `./.github/workflows/<file>.yml` or `.yaml`, ignoring ASCII case on the
/// directory names and the extension. `..` and `.` are folded first.
fn local_reusable_call(line: usize, spec: &str) -> Option<ReusableCall> {
    if spec.is_empty() || spec.contains('@') || spec.contains("://") {
        return None;
    }
    let rest = spec.strip_prefix("./")?;
    let mut parts: Vec<&str> = Vec::new();
    for part in rest.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            parts.pop()?;
            continue;
        }
        parts.push(part);
    }
    if parts.len() != 3 {
        return None;
    }
    if !parts[0].eq_ignore_ascii_case(".github") || !parts[1].eq_ignore_ascii_case("workflows") {
        return None;
    }
    let file_name = parts[2];
    let ext = file_name.rsplit_once('.')?.1;
    if file_name.len() == ext.len() + 1 {
        return None;
    }
    if !ext.eq_ignore_ascii_case("yml") && !ext.eq_ignore_ascii_case("yaml") {
        return None;
    }
    Some(ReusableCall {
        line,
        spec: spec.to_string(),
        file_name: file_name.to_string(),
        source: ReusableSource::Local,
    })
}

/// `owner/repo/.github/workflows/<file>.yml@ref`, ref a tag, branch, or SHA.
///
/// Directory names and the extension are compared without ASCII case. `..` is
/// rejected. The stored spec is the trimmed scalar, not a rewritten path.
fn remote_reusable_call(line: usize, spec: &str) -> Option<ReusableCall> {
    if spec.contains("://")
        || spec
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        return None;
    }
    let (path, reference) = spec.split_once('@')?;
    if !valid_reference(reference) {
        return None;
    }
    let path = path.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return None;
        }
        parts.push(part);
    }
    if parts.len() != 5 {
        return None;
    }
    if !parts[2].eq_ignore_ascii_case(".github") || !parts[3].eq_ignore_ascii_case("workflows") {
        return None;
    }
    let file_name = parts[4];
    if !valid_github_name(parts[0])
        || !valid_github_name(parts[1])
        || !valid_workflow_file(file_name)
    {
        return None;
    }
    let workflow_path = format!("{}/{}/{}", parts[2], parts[3], file_name);
    Some(ReusableCall {
        line,
        spec: spec.to_string(),
        file_name: file_name.to_string(),
        source: ReusableSource::Remote(RemoteWorkflow {
            owner: parts[0].to_string(),
            repo: parts[1].to_string(),
            workflow_path,
            reference: reference.to_string(),
        }),
    })
}

fn valid_github_name(name: &str) -> bool {
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

fn valid_workflow_file(name: &str) -> bool {
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    if stem.is_empty() || stem.starts_with('.') || stem.contains("..") {
        return false;
    }
    if !ext.eq_ignore_ascii_case("yml") && !ext.eq_ignore_ascii_case("yaml") {
        return false;
    }
    stem.chars().all(|character| {
        character.is_ascii_alphanumeric()
            || character == '.'
            || character == '_'
            || character == '-'
    })
}

fn valid_reference(reference: &str) -> bool {
    if reference.is_empty()
        || reference.starts_with('/')
        || reference.ends_with('/')
        || reference.contains("//")
        || reference.contains('@')
        || reference.contains('\\')
    {
        return false;
    }
    if reference.chars().any(|character| {
        character.is_whitespace()
            || character.is_control()
            || matches!(
                character,
                '~' | '^' | ':' | '?' | '*' | '[' | ']' | '{' | '}'
            )
    }) {
        return false;
    }
    reference.split('/').all(valid_ref_segment)
}

fn valid_ref_segment(segment: &str) -> bool {
    if segment.is_empty()
        || segment == "."
        || segment == ".."
        || segment.ends_with('.')
        || segment.ends_with(".lock")
    {
        return false;
    }
    segment.chars().all(|character| {
        character.is_ascii_alphanumeric()
            || character == '.'
            || character == '_'
            || character == '-'
    })
}

fn runs_on_from(line: usize, node: &Node) -> RunsOn {
    match node {
        Node::Scalar { value, .. } => {
            let labels = if value.is_empty() {
                Vec::new()
            } else {
                vec![value.clone()]
            };
            RunsOn {
                line,
                value: value.clone(),
                labels,
            }
        }
        Node::Sequence { items, .. } => {
            let labels = scalar_list(items);
            RunsOn {
                line,
                value: bracket_list(&labels),
                labels,
            }
        }
        Node::Mapping { entries, .. } => {
            let labels = labels_of(entries);
            RunsOn {
                line,
                value: mapping_runs_on_value(entries, &labels),
                labels,
            }
        }
    }
}

fn scalar_list(items: &[Node]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| match item {
            Node::Scalar { value, .. } if !value.is_empty() => Some(value.clone()),
            _ => None,
        })
        .collect()
}

fn labels_of(entries: &[Entry]) -> Vec<String> {
    let Some(item) = entry(entries, "labels") else {
        return Vec::new();
    };
    match &item.value {
        Node::Scalar { value, .. } if !value.is_empty() => vec![value.clone()],
        Node::Sequence { items, .. } => scalar_list(items),
        _ => Vec::new(),
    }
}

fn mapping_runs_on_value(entries: &[Entry], labels: &[String]) -> String {
    let group = entry(entries, "group").and_then(|item| match &item.value {
        Node::Scalar { value, .. } if !value.is_empty() => Some(value.as_str()),
        _ => None,
    });
    let labels_are_list =
        entry(entries, "labels").is_some_and(|item| matches!(item.value, Node::Sequence { .. }));
    let labels_text = if labels.is_empty() {
        None
    } else if labels_are_list || labels.len() != 1 {
        Some(bracket_list(labels))
    } else {
        Some(labels[0].clone())
    };
    match (group, labels_text) {
        (Some(group), Some(labels)) => format!("{{group: {group}, labels: {labels}}}"),
        (Some(group), None) => format!("{{group: {group}}}"),
        (None, Some(labels)) => labels,
        (None, None) => "{}".to_string(),
    }
}

fn bracket_list(labels: &[String]) -> String {
    format!("[{}]", labels.join(", "))
}

fn step_runs(node: &Node) -> String {
    let Node::Sequence { items, .. } = node else {
        return String::new();
    };
    let mut runs = Vec::new();
    for item in items {
        let Node::Mapping { entries, .. } = item else {
            continue;
        };
        let Some(run) = entry(entries, "run") else {
            continue;
        };
        let Some(text) = scalar_text(&run.value) else {
            continue;
        };
        let text = trim_breaks(text);
        if !text.is_empty() {
            runs.push(text);
        }
    }
    runs.join("\n")
}

fn scalar_text(node: &Node) -> Option<&str> {
    match node {
        Node::Scalar { value, .. } => Some(value.as_str()),
        _ => None,
    }
}

fn trim_breaks(text: &str) -> String {
    text.trim_end_matches(['\n', '\r']).to_string()
}

/// Cross products larger than this are counted from the axis lengths.
/// Building every combination would allocate without bound.
const MAX_MATRIX_COMBINATIONS: u64 = 200_000;

struct AxisSpec {
    name: String,
    values: Vec<String>,
    dynamic: bool,
}

struct Table {
    rows: Vec<Row>,
    /// `exclude: ${{ ... }}` or an expression item. The concrete rows still apply.
    dynamic: bool,
}

type Row = Vec<(String, String)>;

fn matrix_from(strategy: &Node) -> Option<Matrix> {
    let Node::Mapping { entries, .. } = strategy else {
        return None;
    };
    let matrix = entry(entries, "matrix")?;
    Some(eval_matrix(matrix.line, &matrix.value))
}

fn eval_matrix(line: usize, node: &Node) -> Matrix {
    match node {
        Node::Scalar { value, .. } if is_github_expression(value) => Matrix {
            line,
            jobs: 1,
            at_least: true,
            jobs_hi: None,
            axes: Vec::new(),
        },
        Node::Mapping { entries, .. } => count_matrix(line, entries),
        _ => Matrix {
            line,
            jobs: 0,
            at_least: false,
            jobs_hi: None,
            axes: Vec::new(),
        },
    }
}

fn count_matrix(line: usize, entries: &[Entry]) -> Matrix {
    let mut axes = Vec::new();
    let mut include = Table {
        rows: Vec::new(),
        dynamic: false,
    };
    let mut exclude = Table {
        rows: Vec::new(),
        dynamic: false,
    };
    for item in entries {
        match item.key.as_str() {
            "include" => include = parse_table(&item.value),
            "exclude" => exclude = parse_table(&item.value),
            _ => axes.push(parse_axis(&item.key, &item.value)),
        }
    }
    let (jobs, jobs_hi, at_least) = combination_count(&axes, &include, &exclude);
    Matrix {
        line,
        jobs,
        at_least,
        jobs_hi,
        axes: runner_axes(entries),
    }
}

fn runner_axes(entries: &[Entry]) -> Vec<MatrixAxis> {
    let mut axes = Vec::new();
    for item in entries {
        match item.key.as_str() {
            "include" => note_include_runners(&item.value, &mut axes),
            "exclude" => {}
            _ => note_axis_definition(item.line, &item.key, &item.value, &mut axes),
        }
    }
    axes
}

fn note_axis_definition(line: usize, name: &str, node: &Node, axes: &mut Vec<MatrixAxis>) {
    note_axis(axes, name, line, true, axis_label_sets(node));
}

fn note_include_runners(node: &Node, axes: &mut Vec<MatrixAxis>) {
    match node {
        Node::Sequence { items } => {
            for item in items {
                note_include_row(item, axes);
            }
        }
        Node::Mapping { .. } => note_include_row(node, axes),
        _ => {}
    }
}

fn note_include_row(node: &Node, axes: &mut Vec<MatrixAxis>) {
    let Node::Mapping { entries, .. } = node else {
        return;
    };
    for item in entries {
        if let Some(labels) = runner_label_set(&item.value) {
            note_axis(axes, &item.key, item.line, false, vec![labels]);
        }
    }
}

fn note_axis(
    axes: &mut Vec<MatrixAxis>,
    name: &str,
    line: usize,
    is_definition: bool,
    sets: Vec<Vec<String>>,
) {
    if let Some(axis) = axes.iter_mut().find(|axis| axis.name == name) {
        if is_definition {
            axis.line = line;
        }
        axis.values.extend(sets);
        return;
    }
    if !is_definition && sets.is_empty() {
        return;
    }
    axes.push(MatrixAxis {
        name: name.to_string(),
        line,
        values: sets,
    });
}

/// One label set per list item. A nested list is one multi-label runner.
fn axis_label_sets(node: &Node) -> Vec<Vec<String>> {
    match node {
        Node::Sequence { items } => items.iter().filter_map(runner_label_set).collect(),
        Node::Scalar { value, .. } if value.is_empty() || is_github_expression(value) => Vec::new(),
        other => runner_label_set(other).into_iter().collect(),
    }
}

fn runner_label_set(node: &Node) -> Option<Vec<String>> {
    let labels = match node {
        Node::Scalar { value, .. } => {
            if value.is_empty() || is_github_expression(value) {
                return None;
            }
            vec![value.clone()]
        }
        Node::Sequence { items } => items
            .iter()
            .filter_map(|item| match item {
                Node::Scalar { value, .. } if !value.is_empty() && !is_github_expression(value) => {
                    Some(value.clone())
                }
                _ => None,
            })
            .collect(),
        Node::Mapping { entries, .. } => labels_of(entries),
    };
    if labels.is_empty() {
        None
    } else {
        Some(labels)
    }
}

fn parse_axis(name: &str, node: &Node) -> AxisSpec {
    match node {
        Node::Sequence { items, .. } => AxisSpec {
            name: name.to_string(),
            values: items.iter().map(canonical).collect(),
            dynamic: false,
        },
        Node::Scalar { value, .. } if is_github_expression(value) => AxisSpec {
            name: name.to_string(),
            values: Vec::new(),
            dynamic: true,
        },
        other => AxisSpec {
            name: name.to_string(),
            values: vec![canonical(other)],
            dynamic: false,
        },
    }
}

fn parse_table(node: &Node) -> Table {
    match node {
        Node::Scalar { value, .. } if is_github_expression(value) => Table {
            rows: Vec::new(),
            dynamic: true,
        },
        Node::Sequence { items, .. } => {
            let dynamic = items.iter().any(
                |item| matches!(item, Node::Scalar { value, .. } if is_github_expression(value)),
            );
            let mut rows = Vec::new();
            for item in items {
                if let Node::Mapping { entries, .. } = item {
                    rows.push(row_from(entries));
                }
            }
            Table { rows, dynamic }
        }
        Node::Mapping { entries, .. } => Table {
            rows: vec![row_from(entries)],
            dynamic: false,
        },
        _ => Table {
            rows: Vec::new(),
            dynamic: false,
        },
    }
}

fn row_from(entries: &[Entry]) -> Vec<(String, String)> {
    entries
        .iter()
        .map(|item| (item.key.clone(), canonical(&item.value)))
        .collect()
}

/// The whole scalar is one `${{ ... }}` expression, so its length is not in the file.
fn is_github_expression(value: &str) -> bool {
    let trimmed = value.trim();
    let Some(rest) = trimmed.strip_prefix("${{") else {
        return false;
    };
    let Some(inner) = rest.strip_suffix("}}") else {
        return false;
    };
    !inner.contains("${{") && !inner.contains("}}")
}

fn canonical(node: &Node) -> String {
    match node {
        Node::Scalar { value, .. } => value.clone(),
        Node::Sequence { items, .. } => {
            let parts: Vec<_> = items.iter().map(canonical).collect();
            format!("[{}]", parts.join(","))
        }
        Node::Mapping { entries, .. } => {
            let mut parts: Vec<_> = entries
                .iter()
                .map(|item| format!("{}={}", item.key, canonical(&item.value)))
                .collect();
            parts.sort();
            format!("{{{}}}", parts.join(","))
        }
    }
}

/// `(jobs, jobs_hi, at_least)`. `jobs_hi` is set only for a finite range.
fn combination_count(
    axes: &[AxisSpec],
    include: &Table,
    exclude: &Table,
) -> (u64, Option<u64>, bool) {
    let dynamic_axis = axes.iter().any(|axis| axis.dynamic);
    let concrete: Vec<&AxisSpec> = axes.iter().filter(|axis| !axis.dynamic).collect();
    let empty_axis = concrete.iter().any(|axis| axis.values.is_empty());
    let axis_names: HashSet<&str> = concrete.iter().map(|axis| axis.name.as_str()).collect();
    let dynamic_names: HashSet<&str> = axes
        .iter()
        .filter(|axis| axis.dynamic)
        .map(|axis| axis.name.as_str())
        .collect();
    let dynamic_only = dynamic_axis && concrete.is_empty() && !empty_axis;
    let unbounded = (dynamic_axis && !empty_axis) || include.dynamic;

    let (product, saturated) = axis_product(&concrete);
    let over_cap = saturated || product > MAX_MATRIX_COMBINATIONS;
    let original = if empty_axis || concrete.is_empty() || over_cap {
        Vec::new()
    } else {
        cartesian(&concrete)
    };

    if unbounded {
        let jobs = if dynamic_only && !exclude.dynamic {
            1
        } else if over_cap || empty_axis || concrete.is_empty() {
            let new = if dynamic_only {
                0
            } else {
                count_new_includes_analytical(&concrete, &include.rows, &axis_names)
            };
            if exclude.dynamic || empty_axis || concrete.is_empty() {
                new
            } else {
                product.saturating_add(new)
            }
        } else if exclude.dynamic {
            count_new_includes(&original, &include.rows, &axis_names)
        } else {
            let reduced = apply_excludes(&original, &exclude.rows, &dynamic_names);
            u64::try_from(reduced.len())
                .unwrap_or(u64::MAX)
                .saturating_add(count_new_includes(&reduced, &include.rows, &axis_names))
        };
        return (jobs, None, true);
    }

    if over_cap {
        return capped_bounds(&concrete, include, exclude, product, &axis_names);
    }

    let reduced = apply_excludes(&original, &exclude.rows, &dynamic_names);
    if exclude.dynamic {
        let lo = count_new_includes(&original, &include.rows, &axis_names);
        let hi = u64::try_from(reduced.len())
            .unwrap_or(u64::MAX)
            .saturating_add(count_new_includes(&reduced, &include.rows, &axis_names));
        return range(lo.min(hi), hi);
    }
    let jobs = u64::try_from(reduced.len())
        .unwrap_or(u64::MAX)
        .saturating_add(count_new_includes(&reduced, &include.rows, &axis_names));
    (jobs, None, false)
}

/// Above the combination cap the bounds use saturating products and spans.
/// Combinations are not allocated.
fn capped_bounds(
    concrete: &[&AxisSpec],
    include: &Table,
    exclude: &Table,
    product: u64,
    axis_names: &HashSet<&str>,
) -> (u64, Option<u64>, bool) {
    let (min_remove, max_remove) =
        exclusion_bounds(&exclude.rows, concrete, product, exclude.dynamic);
    let new = count_new_includes_analytical(concrete, &include.rows, axis_names);
    let matching = include
        .rows
        .iter()
        .filter(|row| matches_original_space(row, concrete))
        .count();
    let matching = u64::try_from(matching).unwrap_or(u64::MAX);
    // An include that fits the original space adds a job only when exclude
    // removed its target. That slack keeps the upper bound above the true count.
    let restore = if max_remove == 0 { 0 } else { matching };
    let hi = product
        .saturating_sub(min_remove)
        .saturating_add(new)
        .saturating_add(restore);
    let lo = product.saturating_sub(max_remove).saturating_add(new);
    range(lo.min(hi), hi)
}

fn range(lo: u64, hi: u64) -> (u64, Option<u64>, bool) {
    if hi > lo {
        (lo, Some(hi), false)
    } else {
        (lo, None, false)
    }
}

fn apply_excludes(original: &[Row], rows: &[Row], dynamic_names: &HashSet<&str>) -> Vec<Row> {
    if rows.is_empty() {
        return original.to_vec();
    }
    let mut kept = original.to_vec();
    kept.retain(|combo| {
        !rows
            .iter()
            .any(|row| exclude_hits(combo, row, dynamic_names))
    });
    kept
}

fn exclusion_bounds(rows: &[Row], axes: &[&AxisSpec], product: u64, dynamic: bool) -> (u64, u64) {
    if product == 0 {
        return (0, 0);
    }
    let mut min_remove = 0u64;
    let mut max_remove = 0u64;
    for row in rows {
        let span = exclusion_span(row, axes, product);
        min_remove = min_remove.max(span);
        max_remove = max_remove.saturating_add(span).min(product);
    }
    if dynamic {
        max_remove = product;
    }
    (min_remove.min(product), max_remove.min(product))
}

/// How many original combinations one exclude row removes.
///
/// The span is `product / product_of_named_axis_lengths` when every named
/// value exists. Overlapping rows are not subtracted twice; callers turn the
/// spans into a lower and an upper bound.
fn exclusion_span(row: &Row, axes: &[&AxisSpec], product: u64) -> u64 {
    if product == 0 || axes.is_empty() {
        return 0;
    }
    if row.is_empty() {
        return product;
    }
    let mut specified = 1u64;
    for (key, value) in row {
        let Some(axis) = axes.iter().find(|axis| axis.name == *key) else {
            return 0;
        };
        if !axis.values.iter().any(|item| item == value) {
            return 0;
        }
        let len = u64::try_from(axis.values.len()).unwrap_or(u64::MAX);
        if len == 0 || specified > u64::MAX / len {
            return product;
        }
        specified *= len;
    }
    if specified == 0 || specified > product {
        return 0;
    }
    product / specified
}

fn axis_product(axes: &[&AxisSpec]) -> (u64, bool) {
    if axes.is_empty() {
        return (0, false);
    }
    let mut total = 1u64;
    let mut saturated = false;
    for axis in axes {
        if axis.values.is_empty() {
            return (0, false);
        }
        let len = u64::try_from(axis.values.len()).unwrap_or(u64::MAX);
        if len != 0 && total > u64::MAX / len {
            saturated = true;
            total = u64::MAX;
        } else {
            total *= len;
        }
    }
    (total, saturated)
}

fn cartesian(axes: &[&AxisSpec]) -> Vec<Vec<(String, String)>> {
    let mut combos = vec![Vec::new()];
    for axis in axes {
        let mut next = Vec::with_capacity(combos.len().saturating_mul(axis.values.len()));
        for combo in &combos {
            for value in &axis.values {
                let mut fields = Vec::with_capacity(combo.len() + 1);
                fields.extend(combo.iter().cloned());
                fields.push((axis.name.clone(), value.clone()));
                next.push(fields);
            }
        }
        combos = next;
    }
    combos
}

fn exclude_hits(
    combo: &[(String, String)],
    row: &[(String, String)],
    dynamic_names: &HashSet<&str>,
) -> bool {
    // A partial row matches. A key that belongs to an expression axis might
    // match, so it does not keep the combination in the lower bound.
    row.iter().all(|pair| {
        dynamic_names.contains(pair.0.as_str())
            || combo
                .iter()
                .any(|field| field.0 == pair.0 && field.1 == pair.1)
    })
}

fn include_expands(
    combo: &[(String, String)],
    row: &[(String, String)],
    axis_names: &HashSet<&str>,
) -> bool {
    row.iter()
        .filter(|pair| axis_names.contains(pair.0.as_str()))
        .all(|pair| {
            combo
                .iter()
                .any(|field| field.0 == pair.0 && field.1 == pair.1)
        })
}

/// Include rows that add a job on top of `base`.
///
/// A row that only adds keys to an existing combination, including one created
/// by an earlier include, does not add a job. A row that sets an original axis
/// the combination does not have is a new job (it does not merge into that
/// include-created combination).
fn count_new_includes(base: &[Row], rows: &[Row], axis_names: &HashSet<&str>) -> u64 {
    let mut extras: Vec<Row> = Vec::new();
    for row in rows {
        if base
            .iter()
            .any(|combo| include_expands(combo, row, axis_names))
        {
            continue;
        }
        if extras
            .iter()
            .any(|combo| extra_accepts(combo, row, axis_names))
        {
            for combo in &mut extras {
                if extra_accepts(combo, row, axis_names) {
                    overlay(combo, row);
                }
            }
            continue;
        }
        extras.push(row.clone());
    }
    u64::try_from(extras.len()).unwrap_or(u64::MAX)
}

fn count_new_includes_analytical(
    axes: &[&AxisSpec],
    rows: &[Row],
    axis_names: &HashSet<&str>,
) -> u64 {
    let mut extras: Vec<Row> = Vec::new();
    for row in rows {
        if matches_original_space(row, axes) {
            continue;
        }
        if extras
            .iter()
            .any(|combo| extra_accepts(combo, row, axis_names))
        {
            for combo in &mut extras {
                if extra_accepts(combo, row, axis_names) {
                    overlay(combo, row);
                }
            }
            continue;
        }
        extras.push(row.clone());
    }
    u64::try_from(extras.len()).unwrap_or(u64::MAX)
}

fn matches_original_space(row: &Row, axes: &[&AxisSpec]) -> bool {
    if axes.is_empty() || axes.iter().any(|axis| axis.values.is_empty()) {
        return false;
    }
    for (key, value) in row {
        if let Some(axis) = axes.iter().find(|axis| axis.name == *key) {
            if !axis.values.iter().any(|item| item == value) {
                return false;
            }
        }
    }
    true
}

/// An include-created combination accepts a later row when shared keys match
/// and every original axis in the row is already present with the same value.
fn extra_accepts(combo: &Row, row: &Row, axis_names: &HashSet<&str>) -> bool {
    for (key, value) in row {
        if axis_names.contains(key.as_str()) {
            match combo.iter().find(|(name, _)| name == key) {
                Some((_, existing)) if existing == value => {}
                _ => return false,
            }
        } else if let Some((_, existing)) = combo.iter().find(|(name, _)| name == key) {
            if existing != value {
                return false;
            }
        }
    }
    true
}

fn overlay(combo: &mut Row, row: &Row) {
    for (key, value) in row {
        if let Some((_, existing)) = combo.iter_mut().find(|(name, _)| name == key) {
            *existing = value.clone();
        } else {
            combo.push((key.clone(), value.clone()));
        }
    }
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

    fn must_scan(name: &str, text: &str) -> Workflow {
        scan_yaml(Path::new(name), text).unwrap_or_else(|err| panic!("{name}: {err}"))
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
        let workflow = must_scan("lint-like.yml", fixture);
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
        let quoted = must_scan(
            "quoted.yml",
            "jobs:\n  lint:\n    runs-on: [\"self-hosted\", \"macos\"]\n",
        );
        let runs_on = quoted.jobs[0].runs_on.as_ref().unwrap();
        assert_eq!(runs_on.line, 3);
        assert_eq!(
            runs_on.labels,
            vec!["self-hosted".to_string(), "macos".to_string()]
        );

        let blocked = must_scan(
            "block.yml",
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
        let pull_request = must_scan("on.yml", "on: [push, pull_request]\n");
        assert_eq!(pull_request.on_line, Some(1));
        assert!(pull_request.has_pull_request);
        assert!(!pull_request.cancel_in_progress);

        let other = must_scan("on.yml", "on: [push, pull_request_target]\n");
        assert!(!other.has_pull_request);
        assert!(other.has_pull_request_target);

        let scalar = must_scan("on.yml", "on: pull_request\n");
        assert_eq!(scalar.on_line, Some(1));
        assert!(scalar.has_pull_request);

        let mapping = must_scan(
            "on.yml",
            "on: {push: null, pull_request: {types: [opened]}}\nconcurrency: {group: ci, cancel-in-progress: true}\n",
        );
        assert_eq!(mapping.on_line, Some(1));
        assert!(mapping.has_pull_request);
        assert!(mapping.cancel_in_progress);
    }

    #[test]
    fn flow_jobs_keep_key_lines() {
        let inline = must_scan("flow.yml", "jobs: {lint: {runs-on: macos-latest}}\n");
        assert_eq!(inline.jobs.len(), 1);
        assert_eq!(inline.jobs[0].name, "lint");
        assert_eq!(inline.jobs[0].line, 1);
        let runs_on = inline.jobs[0].runs_on.as_ref().unwrap();
        assert_eq!(runs_on.line, 1);
        assert_eq!(runs_on.value, "macos-latest");
        assert_eq!(runs_on.labels, vec!["macos-latest".to_string()]);

        let split = must_scan(
            "flow.yml",
            "jobs: {\n  lint: {\n    runs-on: macos-latest\n  }\n}\n",
        );
        assert_eq!(split.jobs[0].line, 2);
        assert_eq!(split.jobs[0].runs_on.as_ref().unwrap().line, 3);
        assert_eq!(
            split.jobs[0].runs_on.as_ref().unwrap().value,
            "macos-latest"
        );
    }

    #[test]
    fn trigger_forms_keep_key_lines() {
        let sequence = must_scan("on.yml", "on: [push, pull_request]\n");
        assert_eq!(sequence.on_line, Some(1));
        assert!(sequence.has_pull_request);

        let quoted = must_scan("on.yml", "on: [\"push\", \"pull_request\"]\n");
        assert_eq!(quoted.on_line, Some(1));
        assert!(quoted.has_pull_request);

        let scalar = must_scan("on.yml", "\u{feff}on: pull_request\n");
        assert_eq!(scalar.on_line, Some(1));
        assert!(scalar.has_pull_request);

        let mapping = must_scan("on.yml", "on: {pull_request: {}}\n");
        assert_eq!(mapping.on_line, Some(1));
        assert!(mapping.has_pull_request);
        assert!(!mapping.cancel_in_progress);

        let disabled = must_scan(
            "on.yml",
            "on: {pull_request: {}}\nconcurrency: {cancel-in-progress: false}\n",
        );
        assert!(disabled.has_pull_request);
        assert!(!disabled.cancel_in_progress);

        let quoted_true = must_scan(
            "on.yml",
            "on: pull_request\nconcurrency:\n  cancel-in-progress: \"true\"\n",
        );
        assert_eq!(quoted_true.on_line, Some(1));
        assert!(quoted_true.cancel_in_progress);
    }

    #[test]
    fn quoted_keys_and_values_keep_lines() {
        let workflow = must_scan(
            "quoted.yml",
            "on: \"pull_request\"\njobs:\n  \"lint\":\n    \"runs-on\": \"macos-latest\"\n    steps:\n      - run: \"cargo fmt --check\"\n",
        );
        assert_eq!(workflow.on_line, Some(1));
        assert!(workflow.has_pull_request);
        assert_eq!(workflow.jobs[0].name, "lint");
        assert_eq!(workflow.jobs[0].line, 3);
        let runs_on = workflow.jobs[0].runs_on.as_ref().unwrap();
        assert_eq!(runs_on.line, 4);
        assert_eq!(runs_on.value, "macos-latest");
        assert_eq!(runs_on.labels, vec!["macos-latest".to_string()]);
        assert_eq!(workflow.jobs[0].steps_text, "cargo fmt --check");

        let single = must_scan(
            "quoted.yml",
            "jobs:\n  'lint':\n    runs-on: 'windows-latest'\n    steps:\n      - 'run': 'cargo test'\n",
        );
        assert_eq!(single.jobs[0].line, 2);
        assert_eq!(single.jobs[0].runs_on.as_ref().unwrap().line, 3);
        assert_eq!(
            single.jobs[0].runs_on.as_ref().unwrap().value,
            "windows-latest"
        );
        assert_eq!(single.jobs[0].steps_text, "cargo test");
    }

    #[test]
    fn runs_on_scalar_list_and_map_keep_lines() {
        let scalar = must_scan("runs.yml", "jobs:\n  test:\n    runs-on: ubuntu-latest\n");
        let scalar_runs = scalar.jobs[0].runs_on.as_ref().unwrap();
        assert_eq!(scalar.jobs[0].line, 2);
        assert_eq!(scalar_runs.line, 3);
        assert_eq!(scalar_runs.value, "ubuntu-latest");
        assert_eq!(scalar_runs.labels, vec!["ubuntu-latest".to_string()]);

        let flow_list = must_scan(
            "runs.yml",
            "jobs:\n  lint:\n    runs-on: [self-hosted, macos]\n",
        );
        let flow_runs = flow_list.jobs[0].runs_on.as_ref().unwrap();
        assert_eq!(flow_runs.line, 3);
        assert_eq!(flow_runs.value, "[self-hosted, macos]");
        assert_eq!(
            flow_runs.labels,
            vec!["self-hosted".to_string(), "macos".to_string()]
        );

        let flow_map = must_scan(
            "runs.yml",
            "jobs:\n  lint:\n    runs-on: {group: large, labels: [self-hosted, macos]}\n",
        );
        let map_runs = flow_map.jobs[0].runs_on.as_ref().unwrap();
        assert_eq!(flow_map.jobs[0].line, 2);
        assert_eq!(map_runs.line, 3);
        assert_eq!(
            map_runs.value,
            "{group: large, labels: [self-hosted, macos]}"
        );
        assert_eq!(
            map_runs.labels,
            vec!["self-hosted".to_string(), "macos".to_string()]
        );

        let block_map = must_scan(
            "runs.yml",
            "jobs:\n  lint:\n    runs-on:\n      group: large\n      labels:\n        - self-hosted\n        - windows\n",
        );
        let block_runs = block_map.jobs[0].runs_on.as_ref().unwrap();
        assert_eq!(block_runs.line, 3);
        assert_eq!(
            block_runs.value,
            "{group: large, labels: [self-hosted, windows]}"
        );
        assert_eq!(
            block_runs.labels,
            vec!["self-hosted".to_string(), "windows".to_string()]
        );

        let label_scalar = must_scan(
            "runs.yml",
            "jobs:\n  lint:\n    runs-on: {group: large, labels: macos-latest}\n",
        );
        let label_runs = label_scalar.jobs[0].runs_on.as_ref().unwrap();
        assert_eq!(label_runs.line, 3);
        assert_eq!(label_runs.value, "{group: large, labels: macos-latest}");
        assert_eq!(label_runs.labels, vec!["macos-latest".to_string()]);

        let group_only = must_scan("runs.yml", "jobs:\n  test:\n    runs-on: {group: large}\n");
        let group_runs = group_only.jobs[0].runs_on.as_ref().unwrap();
        assert_eq!(group_runs.line, 3);
        assert_eq!(group_runs.value, "{group: large}");
        assert!(group_runs.labels.is_empty());
    }

    #[test]
    fn multiline_run_keeps_script_lines() {
        let workflow = must_scan(
            "run.yml",
            "jobs:\n  lint:\n    steps:\n      - run: |\n          echo one\n          echo two\n      - run: cargo fmt --check\n",
        );
        assert_eq!(workflow.jobs[0].line, 2);
        assert_eq!(
            workflow.jobs[0].steps_text,
            "echo one\necho two\ncargo fmt --check"
        );
    }

    #[test]
    fn anchors_and_merge_keys_keep_source_lines() {
        let overridden = must_scan(
            "anchor.yml",
            "defaults: &defaults\n  runs-on: ubuntu-latest\n  timeout-minutes: 10\n  steps:\n    - run: cargo test\non: pull_request\njobs:\n  lint:\n    <<: *defaults\n    runs-on: macos-latest\n    steps:\n      - run: cargo clippy -- -D warnings\n  test:\n    <<: *defaults\n",
        );
        assert_eq!(overridden.on_line, Some(6));
        assert!(overridden.has_pull_request);
        assert!(!overridden.cancel_in_progress);
        assert_eq!(overridden.jobs.len(), 2);

        let lint = &overridden.jobs[0];
        assert_eq!(lint.name, "lint");
        assert_eq!(lint.line, 8);
        assert!(lint.has_timeout_minutes);
        let lint_runs = lint.runs_on.as_ref().unwrap();
        assert_eq!(lint_runs.line, 10);
        assert_eq!(lint_runs.value, "macos-latest");
        assert_eq!(lint_runs.labels, vec!["macos-latest".to_string()]);
        assert_eq!(lint.steps_text, "cargo clippy -- -D warnings");

        let test = &overridden.jobs[1];
        assert_eq!(test.name, "test");
        assert_eq!(test.line, 13);
        assert!(test.has_timeout_minutes);
        let test_runs = test.runs_on.as_ref().unwrap();
        assert_eq!(test_runs.line, 2);
        assert_eq!(test_runs.value, "ubuntu-latest");
        assert_eq!(test.steps_text, "cargo test");

        let inherited = must_scan(
            "anchor.yml",
            "defaults: &defaults\n  runs-on: macos-latest\n  steps:\n    - run: |\n        cargo fmt --check\njobs:\n  lint:\n    <<: *defaults\n",
        );
        assert_eq!(inherited.jobs[0].line, 7);
        let runs_on = inherited.jobs[0].runs_on.as_ref().unwrap();
        assert_eq!(runs_on.line, 2);
        assert_eq!(runs_on.value, "macos-latest");
        assert_eq!(inherited.jobs[0].steps_text, "cargo fmt --check");
        assert!(!inherited.jobs[0].has_timeout_minutes);

        let explicit_first = must_scan(
            "anchor.yml",
            "defaults: &defaults\n  runs-on: ubuntu-latest\njobs:\n  lint:\n    runs-on: windows-latest\n    <<: *defaults\n",
        );
        let runs_on = explicit_first.jobs[0].runs_on.as_ref().unwrap();
        assert_eq!(explicit_first.jobs[0].line, 4);
        assert_eq!(runs_on.line, 5);
        assert_eq!(runs_on.value, "windows-latest");

        let sequence = must_scan(
            "anchor.yml",
            "linux: &linux\n  runs-on: ubuntu-latest\n  timeout-minutes: 5\nextra: &extra\n  steps:\n    - run: cargo test\njobs:\n  test:\n    <<: [*linux, *extra]\n",
        );
        assert_eq!(sequence.jobs[0].line, 8);
        assert!(sequence.jobs[0].has_timeout_minutes);
        assert_eq!(sequence.jobs[0].runs_on.as_ref().unwrap().line, 2);
        assert_eq!(
            sequence.jobs[0].runs_on.as_ref().unwrap().value,
            "ubuntu-latest"
        );
        assert_eq!(sequence.jobs[0].steps_text, "cargo test");

        let earlier_wins = must_scan(
            "anchor.yml",
            "first: &first\n  runs-on: macos-latest\nsecond: &second\n  runs-on: ubuntu-latest\njobs:\n  lint:\n    <<: [*first, *second]\n",
        );
        let runs_on = earlier_wins.jobs[0].runs_on.as_ref().unwrap();
        assert_eq!(earlier_wins.jobs[0].line, 6);
        assert_eq!(runs_on.line, 2);
        assert_eq!(runs_on.value, "macos-latest");

        let named = must_scan(
            "anchor.yml",
            "defaults: &defaults\n  name: clippy\n  runs-on: macos-latest\n  timeout-minutes: 5\njobs:\n  build:\n    <<: *defaults\n",
        );
        assert_eq!(named.jobs[0].name, "build");
        assert_eq!(named.jobs[0].line, 6);
        assert_eq!(named.jobs[0].display_name.as_deref(), Some("clippy"));
        assert_eq!(named.jobs[0].runs_on.as_ref().unwrap().line, 3);
        assert!(named.jobs[0].has_timeout_minutes);
    }

    #[test]
    fn odd_but_valid_yaml_returns_a_workflow() {
        let empty = must_scan("odd.yml", "");
        assert!(empty.jobs.is_empty());
        assert_eq!(empty.on_line, None);

        let listed = must_scan("odd.yml", "jobs:\n  - just-a-list\n");
        assert!(listed.jobs.is_empty());

        let workflow = must_scan(
            "odd.yml",
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
    fn invalid_yaml_reports_line_without_content() {
        let cases = [
            ("broken-colons.yml", "::::\n{{\n", 3),
            ("broken-flow.yml", "jobs: [\n", 2),
            ("broken-tabs.yml", "\tjobs:\n\t\tlint:\n", 2),
            ("broken-braces.yml", "on: {{{{pull_request:\n", 2),
            ("broken-alias.yml", "a: &a\n  b: *a\n", 2),
            (
                "broken-missing.yml",
                "jobs:\n  lint:\n    <<: *missing\n",
                3,
            ),
        ];
        for (name, text, line) in cases {
            let err = scan_yaml(Path::new(name), text).expect_err(name);
            assert_eq!(err.path, Path::new(name), "{name}");
            assert_eq!(
                err.to_string(),
                format!("line {line}: invalid YAML"),
                "{name}"
            );
            assert!(!err.to_string().contains("pull_request"), "{name}: {err}");
            assert!(!err.to_string().contains("jobs"), "{name}: {err}");
            assert!(!err.to_string().contains("lint"), "{name}: {err}");
            assert!(!err.to_string().contains("missing"), "{name}: {err}");
            assert!(!err.to_string().contains('*'), "{name}: {err}");
        }

        let dir = scratch("invalid-yaml");
        let secret = "SECRET_TOKEN_XYZ";
        fs::write(dir.join("ok.yml"), "name: Ok\n").unwrap();
        fs::write(
            dir.join("broken.yml"),
            format!("name: Demo\njobs: [\n  {secret}\n"),
        )
        .unwrap();
        let err = scan_dir(&dir).unwrap_err();
        assert_eq!(err.path, dir.join("broken.yml"));
        assert_eq!(err.to_string(), "line 4: invalid YAML");
        assert!(!err.to_string().contains(secret), "{err}");
    }

    #[test]
    fn arbitrary_text_does_not_panic() {
        let samples = [
            "",
            "jobs:\n  - just-a-list\n",
            "jobs:\n  good:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo ok\n  :::\n",
            "::::\n{{\n",
            "jobs: [\n",
            "\tjobs:\n\t\tlint:\n",
            "on: {{{{pull_request:\n",
            "\0\0\0",
            "\"unterminated",
            "a: &a\n  b: *a\n",
            "jobs:\n  lint:\n    <<: *missing\n",
            &"[".repeat(64),
        ];
        for text in samples {
            let _ = scan_yaml(Path::new("odd.yml"), text);
        }
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

    fn job_matrix(text: &str) -> Matrix {
        let workflow = must_scan("matrix.yml", text);
        assert_eq!(workflow.jobs.len(), 1, "{text}");
        workflow.jobs[0]
            .matrix
            .clone()
            .unwrap_or_else(|| panic!("missing matrix in {text}"))
    }

    #[test]
    fn matrix_two_by_three_flow_and_block_lists_are_six() {
        let flow = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16, 18]\n",
        );
        assert_eq!(flow.line, 4);
        assert_eq!(flow.jobs, 6);
        assert!(!flow.at_least);

        let block = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os:\n          - ubuntu-latest\n          - windows-latest\n        node:\n          - 14\n          - 16\n          - 18\n",
        );
        assert_eq!(block.line, 4);
        assert_eq!(block.jobs, 6);
        assert!(!block.at_least);

        let mixed = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node:\n          - 14\n          - 16\n          - 18\n",
        );
        assert_eq!(mixed.jobs, 6);
        assert!(!mixed.at_least);
    }

    #[test]
    fn matrix_two_by_two_is_four() {
        let matrix = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16]\n",
        );
        assert_eq!(matrix.line, 4);
        assert_eq!(matrix.jobs, 4);
        assert!(!matrix.at_least);
    }

    #[test]
    fn matrix_exclude_removes_matching_combinations_only() {
        let one = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16, 18]\n        exclude:\n          - os: ubuntu-latest\n            node: 14\n",
        );
        assert_eq!(one.line, 4);
        assert_eq!(one.jobs, 5);
        assert!(!one.at_least);

        let flow = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16, 18]\n        exclude: [{os: ubuntu-latest, node: 14}]\n",
        );
        assert_eq!(flow.jobs, 5);

        let duplicate = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16, 18]\n        exclude:\n          - os: ubuntu-latest\n            node: 14\n          - os: ubuntu-latest\n            node: 14\n",
        );
        assert_eq!(duplicate.jobs, 5);

        let absent = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16]\n        exclude:\n          - os: macos-latest\n            node: 14\n",
        );
        assert_eq!(absent.jobs, 4);

        let partial = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [macos-latest, windows-latest]\n        version: [12, 14, 16]\n        environment: [staging, production]\n        exclude:\n          - os: macos-latest\n            version: 12\n            environment: production\n          - os: windows-latest\n            version: 16\n",
        );
        assert_eq!(partial.jobs, 9);
        assert!(!partial.at_least);
    }

    #[test]
    fn matrix_include_adds_only_new_combinations() {
        let added = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16]\n        include:\n          - os: macos-latest\n            node: 18\n          - os: macos-latest\n            node: 20\n",
        );
        assert_eq!(added.line, 4);
        assert_eq!(added.jobs, 6);
        assert!(!added.at_least);

        let flow = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16]\n        include: [{os: macos-latest, node: 18}, {os: macos-latest, node: 20}]\n",
        );
        assert_eq!(flow.jobs, 6);

        let expanded = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16, 18]\n        include:\n          - os: ubuntu-latest\n            node: 14\n            npm: 6\n          - color: green\n",
        );
        assert_eq!(expanded.jobs, 6);

        let small_expanded = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16]\n        include:\n          - os: ubuntu-latest\n            node: 14\n            npm: 6\n",
        );
        assert_eq!(small_expanded.jobs, 4);

        let fruit = job_matrix(
            "jobs:\n  test:\n    strategy:\n      matrix:\n        fruit: [apple, pear]\n        animal: [cat, dog]\n        include:\n          - color: green\n          - color: pink\n            animal: cat\n          - fruit: apple\n            shape: circle\n          - fruit: banana\n          - fruit: banana\n            animal: cat\n",
        );
        assert_eq!(fruit.jobs, 6);

        let include_only = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        include:\n          - site: production\n          - site: staging\n          - site: dev\n          - site: qa\n          - site: eu\n          - site: us\n",
        );
        assert_eq!(include_only.jobs, 6);
        assert!(!include_only.at_least);

        let five = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        include:\n          - site: production\n          - site: staging\n          - site: dev\n          - site: qa\n          - site: eu\n",
        );
        assert_eq!(five.jobs, 5);

        let added_back = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16]\n        exclude:\n          - os: ubuntu-latest\n            node: 14\n        include:\n          - os: ubuntu-latest\n            node: 14\n            npm: 6\n",
        );
        assert_eq!(added_back.jobs, 4);
    }

    #[test]
    fn matrix_expression_axis_is_at_least_the_rest() {
        let wide = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16, 18]\n        extra: ${{ fromJSON(needs.setup.outputs.extra) }}\n",
        );
        assert_eq!(wide.line, 4);
        assert_eq!(wide.jobs, 6);
        assert!(wide.at_least);

        let rest = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: ${{ fromJSON(needs.setup.outputs.node) }}\n",
        );
        assert_eq!(rest.line, 4);
        assert_eq!(rest.jobs, 2);
        assert!(rest.at_least);

        let quoted = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: \"${{ fromJSON(needs.setup.outputs.node) }}\"\n",
        );
        assert_eq!(quoted.jobs, 2);
        assert!(quoted.at_least);

        let only = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        node: ${{ fromJSON(needs.setup.outputs.node) }}\n",
        );
        assert_eq!(only.jobs, 1);
        assert!(only.at_least);

        let embedded = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: ubuntu-${{ matrix.extra }}\n        node: [14, 16, 18, 20, 22, 24]\n",
        );
        assert_eq!(embedded.jobs, 6);
        assert!(!embedded.at_least);

        let list_item = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, \"${{ matrix.extra }}\"]\n        node: [14, 16, 18]\n",
        );
        assert_eq!(list_item.jobs, 6);
        assert!(!list_item.at_least);

        let whole = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix: ${{ fromJSON(needs.setup.outputs.matrix) }}\n",
        );
        assert_eq!(whole.line, 4);
        assert_eq!(whole.jobs, 1);
        assert!(whole.at_least);

        let dynamic_include = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16, 18]\n        include: ${{ fromJSON(needs.setup.outputs.include) }}\n",
        );
        assert_eq!(dynamic_include.jobs, 6);
        assert!(dynamic_include.at_least);
    }

    #[test]
    fn matrix_flow_style_keeps_the_matrix_key_line() {
        let inline = job_matrix(
            "jobs: {build: {strategy: {matrix: {os: [ubuntu-latest, windows-latest], node: [14, 16, 18]}}}}\n",
        );
        assert_eq!(inline.line, 1);
        assert_eq!(inline.jobs, 6);
        assert!(!inline.at_least);

        let split = job_matrix(
            "jobs:\n  build:\n    timeout-minutes: 5\n    strategy:\n      matrix: {os: [ubuntu-latest, windows-latest], node: [14, 16, 18]}\n",
        );
        assert_eq!(split.line, 5);
        assert_eq!(split.jobs, 6);

        let commented = job_matrix(
            "jobs:\n  build:\n    timeout-minutes: 5\n    strategy:\n      # keep going\n      fail-fast: false\n\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16, 18]\n",
        );
        assert_eq!(commented.line, 8);
        assert_eq!(commented.jobs, 6);
    }

    #[test]
    fn matrix_anchor_alias_and_object_values() {
        let anchored = job_matrix(
            "defaults: &defaults\n  strategy:\n    matrix:\n      os: [ubuntu-latest, windows-latest]\n      node: [14, 16, 18]\njobs:\n  build:\n    <<: *defaults\n    timeout-minutes: 5\n",
        );
        assert_eq!(anchored.line, 3);
        assert_eq!(anchored.jobs, 6);

        let alias = job_matrix(
            "oses: &oses\n  - ubuntu-latest\n  - windows-latest\njobs:\n  build:\n    strategy:\n      matrix:\n        os: *oses\n        node: [14, 16, 18]\n",
        );
        assert_eq!(alias.line, 7);
        assert_eq!(alias.jobs, 6);

        let objects = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os:\n          - ubuntu-latest\n          - macos-latest\n        node:\n          - version: 14\n          - version: 20\n            env: legacy\n",
        );
        assert_eq!(objects.jobs, 4);
        assert!(!objects.at_least);

        let scalar = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: ubuntu-latest\n        node: [14, 16, 18, 20, 22, 24]\n",
        );
        assert_eq!(scalar.jobs, 6);

        let empty = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: []\n        node: [14, 16, 18, 20, 22, 24]\n",
        );
        assert_eq!(empty.jobs, 0);
        assert!(!empty.at_least);

        let workflow = must_scan(
            "matrix.yml",
            "jobs:\n  build:\n    strategy:\n      fail-fast: false\n      max-parallel: 2\n",
        );
        assert!(workflow.jobs[0].matrix.is_none());
    }

    fn crlf(text: &str) -> String {
        text.replace('\n', "\r\n")
    }

    #[test]
    fn duplicate_runs_on_keeps_the_later_value() {
        let cases = [
            (
                "block",
                "jobs:\n  lint:\n    runs-on: ubuntu-latest\n    runs-on: macos-latest\n",
                "macos-latest",
                3,
                4,
            ),
            (
                "crlf",
                "jobs:\r\n  lint:\r\n    runs-on: ubuntu-latest\r\n    runs-on: macos-latest\r\n",
                "macos-latest",
                3,
                4,
            ),
            (
                "reversed",
                "jobs:\n  lint:\n    runs-on: macos-latest\n    runs-on: ubuntu-latest\n",
                "ubuntu-latest",
                3,
                4,
            ),
            (
                "quoted",
                "jobs:\n  lint:\n    runs-on: \"ubuntu-latest\"\n    runs-on: \"macos-latest\"\n",
                "macos-latest",
                3,
                4,
            ),
            (
                "capitals",
                "jobs:\n  lint:\n    runs-on: ubuntu-latest\n    runs-on: MacOS-latest\n",
                "MacOS-latest",
                3,
                4,
            ),
            (
                "flow",
                "jobs:\n  lint: {\n    runs-on: ubuntu-latest,\n    runs-on: macos-latest\n  }\n",
                "macos-latest",
                3,
                4,
            ),
            (
                "empty-then-value",
                "jobs:\n  lint:\n    runs-on: \"\"\n    runs-on: macos-latest\n",
                "macos-latest",
                3,
                4,
            ),
            (
                "value-then-empty",
                "jobs:\n  lint:\n    runs-on: ubuntu-latest\n    runs-on: \"\"\n",
                "",
                3,
                4,
            ),
            (
                "list-wins",
                "jobs:\n  lint:\n    runs-on: ubuntu-latest\n    runs-on: [macos-latest]\n",
                "[macos-latest]",
                3,
                4,
            ),
        ];
        for (name, text, value, first, second) in cases {
            let workflow = must_scan(name, text);
            assert_eq!(workflow.jobs.len(), 1, "{name}");
            let runs_on = workflow.jobs[0].runs_on.as_ref().unwrap();
            assert_eq!(runs_on.value, value, "{name}");
            assert_eq!(runs_on.line, second, "{name}");
            assert_eq!(workflow.duplicate_keys.len(), 1, "{name}");
            assert_eq!(workflow.duplicate_keys[0].key, "runs-on");
            assert_eq!(workflow.duplicate_keys[0].first, first, "{name}");
            assert_eq!(workflow.duplicate_keys[0].second, second, "{name}");
        }

        let third = must_scan(
            "three.yml",
            "jobs:\n  lint:\n    runs-on: ubuntu-latest\n    runs-on: windows-latest\n    runs-on: macos-latest\n",
        );
        assert_eq!(
            third.jobs[0].runs_on.as_ref().unwrap().value,
            "macos-latest"
        );
        assert_eq!(third.duplicate_keys.len(), 2);
        assert_eq!(third.duplicate_keys[0].first, 3);
        assert_eq!(third.duplicate_keys[0].second, 4);
        assert_eq!(third.duplicate_keys[1].first, 4);
        assert_eq!(third.duplicate_keys[1].second, 5);

        let same_line = must_scan(
            "flow.yml",
            "jobs: {lint: {runs-on: ubuntu-latest, runs-on: macos-latest}}\n",
        );
        assert_eq!(
            same_line.jobs[0].runs_on.as_ref().unwrap().value,
            "macos-latest"
        );
        assert_eq!(same_line.duplicate_keys[0].first, 1);
        assert_eq!(same_line.duplicate_keys[0].second, 1);

        let distinct = must_scan(
            "case.yml",
            "jobs:\n  lint:\n    runs-on: ubuntu-latest\n    Runs-on: macos-latest\n",
        );
        assert_eq!(
            distinct.jobs[0].runs_on.as_ref().unwrap().value,
            "ubuntu-latest"
        );
        assert!(distinct.duplicate_keys.is_empty());

        let missing = must_scan("one.yml", "jobs:\n  lint:\n    runs-on: ubuntu-latest\n");
        assert!(missing.duplicate_keys.is_empty());
    }

    #[test]
    fn merge_key_does_not_override_an_explicit_key() {
        let explicit_last = must_scan(
            "anchor.yml",
            "defaults: &defaults\n  runs-on: ubuntu-latest\njobs:\n  lint:\n    <<: *defaults\n    runs-on: macos-latest\n",
        );
        let runs_on = explicit_last.jobs[0].runs_on.as_ref().unwrap();
        assert_eq!(runs_on.value, "macos-latest");
        assert_eq!(runs_on.line, 6);
        assert!(explicit_last.duplicate_keys.is_empty());

        let explicit_first = must_scan(
            "anchor.yml",
            "defaults: &defaults\n  runs-on: ubuntu-latest\njobs:\n  lint:\n    runs-on: windows-latest\n    <<: *defaults\n",
        );
        let runs_on = explicit_first.jobs[0].runs_on.as_ref().unwrap();
        assert_eq!(runs_on.value, "windows-latest");
        assert_eq!(runs_on.line, 5);
        assert!(explicit_first.duplicate_keys.is_empty());

        let crlf = must_scan(
            "anchor.yml",
            &crlf("defaults: &defaults\n  runs-on: ubuntu-latest\njobs:\n  lint:\n    runs-on: windows-latest\n    <<: *defaults\n"),
        );
        assert_eq!(
            crlf.jobs[0].runs_on.as_ref().unwrap().value,
            "windows-latest"
        );
        assert!(crlf.duplicate_keys.is_empty());

        let inside_anchor = must_scan(
            "anchor.yml",
            "defaults: &defaults\n  runs-on: ubuntu-latest\n  runs-on: macos-latest\njobs:\n  lint:\n    <<: *defaults\n    runs-on: windows-latest\n",
        );
        assert_eq!(
            inside_anchor.jobs[0].runs_on.as_ref().unwrap().value,
            "windows-latest"
        );
        assert_eq!(inside_anchor.duplicate_keys.len(), 1);
        assert_eq!(inside_anchor.duplicate_keys[0].key, "runs-on");
        assert_eq!(inside_anchor.duplicate_keys[0].first, 2);
        assert_eq!(inside_anchor.duplicate_keys[0].second, 3);
    }

    #[test]
    fn include_chain_that_only_adds_keys_is_one_job() {
        let chain = "jobs:\n  build:\n    strategy:\n      matrix:\n        include:\n          - os: macos-latest\n          - os: macos-latest\n            npm: \"6\"\n";
        let matrix = job_matrix(chain);
        assert_eq!(matrix.jobs, 1);
        assert!(matrix.jobs_hi.is_none());
        assert!(!matrix.at_least);

        let crlf_chain = job_matrix(&crlf(chain));
        assert_eq!(crlf_chain.jobs, 1);

        let flow = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        include: [{os: macos-latest}, {npm: \"6\", os: macos-latest}]\n",
        );
        assert_eq!(flow.jobs, 1);

        let missing_os = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        include:\n          - os: macos-latest\n          - npm: \"6\"\n",
        );
        assert_eq!(missing_os.jobs, 1);

        let third = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        include:\n          - os: macos-latest\n          - os: macos-latest\n            npm: \"6\"\n          - os: macos-latest\n            npm: \"6\"\n            cache: true\n",
        );
        assert_eq!(third.jobs, 1);

        let capitals = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        include:\n          - os: MacOS-latest\n          - os: MacOS-latest\n            npm: \"6\"\n",
        );
        assert_eq!(capitals.jobs, 1);

        let other_os = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        include:\n          - os: macos-latest\n          - os: windows-latest\n            npm: \"6\"\n",
        );
        assert_eq!(other_os.jobs, 2);

        let beside_axis = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest]\n        include:\n          - os: macos-latest\n          - os: macos-latest\n            npm: \"6\"\n",
        );
        assert_eq!(beside_axis.jobs, 2);
        assert!(!beside_axis.at_least);

        let empty_extra = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        include:\n          - os: macos-latest\n          - os: macos-latest\n            npm: \"\"\n",
        );
        assert_eq!(empty_extra.jobs, 1);
    }

    #[test]
    fn dynamic_exclude_reports_lower_and_upper_bounds() {
        let plain = "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16, 18]\n        exclude: ${{ inputs.rules }}\n";
        let matrix = job_matrix(plain);
        assert_eq!(matrix.line, 4);
        assert_eq!(matrix.jobs, 0);
        assert_eq!(matrix.jobs_hi, Some(6));
        assert!(!matrix.at_least);

        assert_eq!(job_matrix(&crlf(plain)).jobs_hi, Some(6));

        let flow = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix: {node: [14, 16, 18], os: [ubuntu-latest, windows-latest], exclude: \"${{ inputs.rules }}\"}\n",
        );
        assert_eq!(flow.jobs, 0);
        assert_eq!(flow.jobs_hi, Some(6));

        let capitals = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16, 18]\n        exclude: ${{ INPUTS.RULES }}\n",
        );
        assert_eq!(capitals.jobs, 0);
        assert_eq!(capitals.jobs_hi, Some(6));

        let spaced = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16, 18]\n        exclude: ${{ inputs.rules }}\n",
        );
        assert_eq!(spaced.jobs_hi, Some(6));

        let reordered = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        exclude: ${{ inputs.rules }}\n        node: [14, 16, 18]\n        os: [ubuntu-latest, windows-latest]\n",
        );
        assert_eq!(reordered.jobs, 0);
        assert_eq!(reordered.jobs_hi, Some(6));

        let added = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16, 18]\n        exclude: ${{ inputs.rules }}\n        include:\n          - os: macos-latest\n            node: 20\n",
        );
        assert_eq!(added.jobs, 1);
        assert_eq!(added.jobs_hi, Some(7));

        let chain = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16, 18]\n        exclude: ${{ inputs.rules }}\n        include:\n          - os: macos-latest\n            node: 20\n          - os: macos-latest\n            node: 20\n            npm: \"6\"\n",
        );
        assert_eq!(chain.jobs, 1);
        assert_eq!(chain.jobs_hi, Some(7));

        let expands = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16, 18]\n        exclude: ${{ inputs.rules }}\n        include:\n          - os: ubuntu-latest\n            node: 14\n            npm: \"6\"\n",
        );
        assert_eq!(expands.jobs, 0);
        assert_eq!(expands.jobs_hi, Some(6));

        let narrow = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16]\n        exclude: ${{ inputs.rules }}\n",
        );
        assert_eq!(narrow.jobs, 0);
        assert_eq!(narrow.jobs_hi, Some(4));
        assert!(!narrow.at_least);

        let absent = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16, 18]\n",
        );
        assert_eq!(absent.jobs, 6);
        assert!(absent.jobs_hi.is_none());

        let empty_list = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16, 18]\n        exclude: []\n",
        );
        assert_eq!(empty_list.jobs, 6);
        assert!(empty_list.jobs_hi.is_none());
    }

    fn axis_block(name: &str, prefix: &str, count: usize) -> String {
        let mut text = format!("        {name}:\n");
        for index in 0..count {
            text.push_str(&format!("          - {prefix}{index}\n"));
        }
        text
    }

    #[test]
    fn large_matrix_bounds_use_saturating_arithmetic() {
        let wide = format!(
            "jobs:\n  build:\n    strategy:\n      matrix:\n{}{}",
            axis_block("a", "a", 500),
            axis_block("b", "b", 401),
        );
        let matrix = job_matrix(&wide);
        assert_eq!(matrix.jobs, 200_500);
        assert!(matrix.jobs_hi.is_none());
        assert!(!matrix.at_least);

        let excluded = format!("{wide}        exclude: ${{{{ inputs.rules }}}}\n");
        let matrix = job_matrix(&excluded);
        assert_eq!(matrix.jobs, 0);
        assert_eq!(matrix.jobs_hi, Some(200_500));

        let crlf_excluded = job_matrix(&crlf(&excluded));
        assert_eq!(crlf_excluded.jobs, 0);
        assert_eq!(crlf_excluded.jobs_hi, Some(200_500));

        let added = format!(
            "{wide}        exclude: ${{{{ inputs.rules }}}}\n        include:\n          - a: other\n          - a: other\n            npm: \"6\"\n"
        );
        let matrix = job_matrix(&added);
        assert_eq!(matrix.jobs, 1);
        assert_eq!(matrix.jobs_hi, Some(200_501));

        let chained = format!(
            "{wide}        include:\n          - a: other\n          - a: other\n            npm: \"6\"\n"
        );
        let matrix = job_matrix(&chained);
        assert_eq!(matrix.jobs, 200_501);
        assert!(matrix.jobs_hi.is_none());

        let one = format!("{wide}        exclude:\n          - a: a0\n");
        let matrix = job_matrix(&one);
        assert_eq!(matrix.jobs, 200_500 - 401);
        assert!(matrix.jobs_hi.is_none());

        let two = format!("{wide}        exclude:\n          - a: a0\n          - a: a1\n");
        let matrix = job_matrix(&two);
        assert_eq!(matrix.jobs, 200_500 - 802);
        assert_eq!(matrix.jobs_hi, Some(200_500 - 401));

        let mut saturated = String::from("jobs:\n  build:\n    strategy:\n      matrix:\n");
        for axis in 0..16 {
            saturated.push_str(&axis_block(&format!("a{axis}"), "v", 16));
        }
        let matrix = job_matrix(&saturated);
        assert_eq!(matrix.jobs, u64::MAX);
        assert!(matrix.jobs_hi.is_none());
        assert!(!matrix.at_least);

        saturated.push_str("        exclude: ${{ inputs.rules }}\n");
        let matrix = job_matrix(&saturated);
        assert_eq!(matrix.jobs, 0);
        assert_eq!(matrix.jobs_hi, Some(u64::MAX));
    }

    #[test]
    fn duplicate_axis_keeps_the_later_list() {
        let matrix = job_matrix(
            "jobs:\n  build:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n        node: [14, 16, 18]\n        os: [ubuntu-latest]\n",
        );
        assert_eq!(matrix.jobs, 3);
        assert!(matrix.jobs_hi.is_none());
        assert!(!matrix.at_least);
    }
}
