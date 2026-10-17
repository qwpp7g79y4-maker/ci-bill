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
    /// Runner text, for example `ubuntu-latest` or `[self-hosted, macos]`.
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
    Scalar { value: String, line: usize },
    Sequence { items: Vec<Node> },
    Mapping { entries: Vec<Entry> },
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
        let node = Node::Mapping {
            entries: resolve_merge(raw),
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

fn resolve_merge(entries: Vec<Entry>) -> Vec<Entry> {
    let mut explicit = Vec::new();
    let mut merges = Vec::new();
    let mut seen = HashSet::new();
    for item in entries {
        if item.key == "<<" {
            merges.push(item.value);
            continue;
        }
        if seen.insert(item.key.clone()) {
            explicit.push(item);
        }
    }
    for value in merges {
        merge_node(&mut explicit, &mut seen, &value);
    }
    explicit
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
    let mut workflow = Workflow {
        path: path.to_path_buf(),
        on_line: None,
        has_pull_request: false,
        cancel_in_progress: false,
        jobs: Vec::new(),
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
    if has_pull_request(&item.value) {
        workflow.has_pull_request = true;
    }
}

fn has_pull_request(node: &Node) -> bool {
    match node {
        Node::Scalar { value, .. } => value == "pull_request",
        Node::Sequence { items, .. } => items
            .iter()
            .any(|item| matches!(item, Node::Scalar { value, .. } if value == "pull_request")),
        Node::Mapping { entries, .. } => entries.iter().any(|item| item.key == "pull_request"),
    }
}

fn note_concurrency(workflow: &mut Workflow, node: &Node) {
    let Node::Mapping { entries, .. } = node else {
        return;
    };
    let enabled = entry(entries, "cancel-in-progress").is_some_and(|item| is_true(&item.value));
    if enabled {
        workflow.cancel_in_progress = true;
    }
}

fn is_true(node: &Node) -> bool {
    match node {
        Node::Scalar { value, .. } => value.eq_ignore_ascii_case("true"),
        _ => false,
    }
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
}
