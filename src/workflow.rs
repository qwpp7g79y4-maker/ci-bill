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
    /// Line of the top-level `concurrency:` key. The key match ignores ASCII case.
    pub concurrency_line: Option<usize>,
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
    /// Steps in document order, for later linking.
    pub steps: Vec<Step>,
    /// `strategy.matrix`, when the job has one.
    pub matrix: Option<Matrix>,
    /// Job-level `concurrency.cancel-in-progress` cancels in-progress runs.
    pub cancel_in_progress: bool,
    /// Line of this job's `concurrency:` key. The key match ignores ASCII case.
    pub concurrency_line: Option<usize>,
    /// `uses:` calling a local or remote reusable workflow.
    pub uses: Option<ReusableCall>,
    /// Job-level `uses:` that is not a reusable workflow call.
    pub invalid_uses: Option<InvalidUses>,
}

/// One step under `jobs.<id>.steps`.
///
/// `name` is the `name` scalar when it has text. Otherwise it is the `uses`
/// scalar, and otherwise the first non-empty line of `run`. `line` is the
/// 1-based line of the key that supplied `name`. Those keys match without
/// ASCII case, and the later key wins. Block and flow styles both count.
/// A step with none of those keys keeps an empty name and the line of its
/// first key, or the line of the step itself.
///
/// `local_action` is set when `uses` is a `./` path to an action in this
/// repository. A reusable workflow path and a remote action stay empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub name: String,
    pub line: usize,
    /// `uses: ./path` for a local action directory, when present.
    pub local_action: Option<LocalAction>,
}

/// A step calling `uses: ./path` in this repository.
///
/// The path is relative to the repository root. `./`, `./action.yml`, and
/// `./pad/action.yaml` all name the directory that holds the action file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalAction {
    /// Line of the `uses:` key.
    pub line: usize,
    /// Trimmed scalar, for example `./.github/actions/lint`.
    pub spec: String,
    /// Directory relative to the repository root, `/`-separated.
    /// Empty means the repository root (`uses: ./`).
    pub relative: String,
}

/// `runs` from an action file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedAction {
    pub path: PathBuf,
    /// Trimmed `runs.using` text. Empty when the key is missing.
    pub using: String,
    pub steps: Vec<ActionStep>,
}

/// One step under `runs.steps` of an action file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionStep {
    pub name: String,
    pub name_line: Option<usize>,
    pub run_text: String,
    pub run_line: Option<usize>,
    pub local_action: Option<LocalAction>,
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
        // `Parser::load_node` recurses once per collection, and the state
        // machine calls `parse_node` again for a nested block entry. Past
        // roughly 19_000 levels that aborts the process. `catch_unwind`
        // cannot turn the abort into a `Result`, so reject the text before
        // yaml-rust2's scanner or parser sees it.
        if let Some(line) = nesting_too_deep(text, MAX_NODE_DEPTH) {
            return Err(line);
        }
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

/// 1-based line where block or flow nesting exceeds `limit`, if it does.
///
/// One forward pass over the text. It never calls yaml-rust2. Flow `[` `{`
/// each add a level. A block sequence `-` or a block key at a new column adds
/// a level; another entry at the same column does not. Several `-` on one
/// line are nested, not siblings. Returns `None` when the document is shallow
/// enough to parse, so a real syntax error keeps the parser's line number.
fn nesting_too_deep(text: &str, limit: usize) -> Option<usize> {
    let mut block: Vec<usize> = Vec::new();
    let mut flow: usize = 0;
    // Lowest indent at which a line still belongs to the open block scalar.
    let mut scalar_min_indent: Option<usize> = None;
    // Quote left open at the end of a line: the next line starts inside it.
    let mut open_quote: Option<char> = None;

    for (idx, raw) in text.split('\n').enumerate() {
        let line_no = idx + 1;
        let line = raw.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let indent = line.bytes().take_while(|byte| *byte == b' ').count();
        let content = &line[indent..];
        if content.is_empty() {
            continue;
        }

        if let Some(quote) = open_quote {
            let chars: Vec<char> = content.chars().collect();
            let mut index = 0;
            if !consume_quoted(&chars, &mut index, quote) {
                continue;
            }
            open_quote = None;
            if flow > 0
                && scan_flow(
                    &chars,
                    &mut index,
                    &mut flow,
                    block.len(),
                    limit,
                    &mut open_quote,
                )
            {
                return Some(line_no);
            }
            continue;
        }

        if let Some(min_indent) = scalar_min_indent {
            if flow == 0 && indent >= min_indent {
                continue;
            }
            scalar_min_indent = None;
        }

        if flow > 0 {
            if flow_line_exceeds(content, &mut flow, block.len(), limit, &mut open_quote) {
                return Some(line_no);
            }
            continue;
        }

        if content.starts_with('#') {
            continue;
        }

        while block.last().copied().is_some_and(|col| indent < col) {
            block.pop();
        }

        if block_line_exceeds(
            content,
            indent,
            &mut block,
            &mut flow,
            &mut scalar_min_indent,
            &mut open_quote,
            limit,
        ) {
            return Some(line_no);
        }
    }
    None
}

fn depth_exceeds(block: usize, flow: usize, limit: usize) -> bool {
    block.saturating_add(flow) > limit
}

fn flow_line_exceeds(
    content: &str,
    flow: &mut usize,
    block: usize,
    limit: usize,
    open_quote: &mut Option<char>,
) -> bool {
    let chars: Vec<char> = content.chars().collect();
    let mut index = 0;
    scan_flow(&chars, &mut index, flow, block, limit, open_quote)
}

fn block_line_exceeds(
    content: &str,
    line_indent: usize,
    block: &mut Vec<usize>,
    flow: &mut usize,
    scalar_min_indent: &mut Option<usize>,
    open_quote: &mut Option<char>,
    limit: usize,
) -> bool {
    let chars: Vec<char> = content.chars().collect();
    let mut index = 0;
    // Column of the nearest key on this line; an indent indicator counts from it.
    let mut parent_indent = line_indent;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '#' {
            break;
        }
        if ch == ' ' || ch == '\t' {
            index += 1;
            continue;
        }
        if is_seq_dash(&chars, index) {
            let column = line_indent + index;
            if block.last().copied() != Some(column) {
                block.push(column);
                if depth_exceeds(block.len(), *flow, limit) {
                    return true;
                }
            }
            index += 1;
            continue;
        }
        if ch == '[' || ch == '{' {
            *flow += 1;
            if depth_exceeds(block.len(), *flow, limit) {
                return true;
            }
            index += 1;
            if scan_flow(&chars, &mut index, flow, block.len(), limit, open_quote) {
                return true;
            }
            continue;
        }
        if ch == '\'' || ch == '"' {
            let quote = ch;
            index += 1;
            if !consume_quoted(&chars, &mut index, quote) {
                *open_quote = Some(quote);
            }
            break;
        }
        if is_block_header(&chars, index) {
            let floor = line_indent + 1;
            let explicit = parent_indent + block_indent_indicator(&chars, index);
            *scalar_min_indent = Some(floor.max(explicit));
            break;
        }
        if let Some(after_colon) = key_end(&chars, index) {
            let column = line_indent + index;
            parent_indent = column;
            if block.last().copied() != Some(column) {
                block.push(column);
                if depth_exceeds(block.len(), *flow, limit) {
                    return true;
                }
            }
            index = after_colon;
            continue;
        }
        break;
    }
    false
}

fn is_seq_dash(chars: &[char], index: usize) -> bool {
    if chars.get(index) != Some(&'-') {
        return false;
    }
    matches!(chars.get(index + 1).copied(), None | Some(' ' | '\t'))
}

fn is_plain_key_char(ch: char) -> bool {
    !matches!(
        ch,
        ' ' | '\t'
            | ':'
            | '#'
            | '{'
            | '}'
            | '['
            | ']'
            | ','
            | '"'
            | '\''
            | '|'
            | '>'
            | '&'
            | '*'
            | '!'
            | '?'
            | '%'
            | '@'
            | '`'
    )
}

/// Index just after `:` when `chars[start..]` is a block key, else `None`.
fn key_end(chars: &[char], start: usize) -> Option<usize> {
    if !chars.get(start).is_some_and(|ch| is_plain_key_char(*ch)) {
        return None;
    }
    let mut index = start + 1;
    while chars.get(index).is_some_and(|ch| is_plain_key_char(*ch)) {
        index += 1;
    }
    if chars.get(index) != Some(&':') {
        return None;
    }
    let after = index + 1;
    match chars.get(after).copied() {
        None
        | Some(
            ' ' | '\t' | '#' | '\'' | '"' | '{' | '[' | '|' | '>' | '-' | '&' | '*' | '!' | '?',
        ) => Some(after),
        Some(_) => None,
    }
}

fn is_block_header(chars: &[char], index: usize) -> bool {
    if !matches!(chars.get(index), Some('|' | '>')) {
        return false;
    }
    let mut cursor = index + 1;
    // `|+-2`, `|-`, `|2` and `|2-` are all block-scalar headers.
    if matches!(chars.get(cursor), Some('+' | '-')) {
        cursor += 1;
    }
    while chars.get(cursor).is_some_and(|ch| ch.is_ascii_digit()) {
        cursor += 1;
    }
    if matches!(chars.get(cursor), Some('+' | '-')) {
        cursor += 1;
    }
    while matches!(chars.get(cursor), Some(' ' | '\t')) {
        cursor += 1;
    }
    matches!(chars.get(cursor), None | Some('#'))
}

/// Value of the explicit indentation digit in a `|2` / `>-2` header, else 0.
fn block_indent_indicator(chars: &[char], header: usize) -> usize {
    chars[header + 1..]
        .iter()
        .take_while(|ch| matches!(ch, '+' | '-') || ch.is_ascii_digit())
        .find_map(|ch| ch.to_digit(10))
        .map_or(0, |digit| digit as usize)
}

/// True when the quote at `index` starts a scalar rather than sitting inside a
/// plain one (`[it's]`), so an unclosed quote may carry over to the next line.
fn quote_starts_scalar(chars: &[char], index: usize) -> bool {
    let before = &chars[..index];
    let Some(last) = before.iter().rposition(|ch| !matches!(ch, ' ' | '\t')) else {
        return true;
    };
    if matches!(before[last], '[' | '{' | ',' | ':' | '?' | '-') {
        return true;
    }
    // A quote after a separated `&anchor` or `!tag` token still starts the scalar.
    let separated = last + 1 < index;
    let token = before[..=last]
        .iter()
        .rposition(|ch| matches!(ch, ' ' | '\t' | '[' | '{' | ','))
        .map_or(0, |at| at + 1);
    separated && matches!(before.get(token), Some('&' | '!'))
}

/// Returns false when the line ends before the closing quote.
fn consume_quoted(chars: &[char], index: &mut usize, quote: char) -> bool {
    if quote == '\'' {
        while *index < chars.len() {
            if chars[*index] == '\'' {
                if chars.get(*index + 1) == Some(&'\'') {
                    *index += 2;
                    continue;
                }
                *index += 1;
                return true;
            }
            *index += 1;
        }
        return false;
    }
    let mut escape = false;
    while *index < chars.len() {
        let ch = chars[*index];
        *index += 1;
        if escape {
            escape = false;
            continue;
        }
        if ch == '\\' {
            escape = true;
            continue;
        }
        if ch == '"' {
            return true;
        }
    }
    false
}

/// Walk a flow collection that is already open. `index` moves forward.
fn scan_flow(
    chars: &[char],
    index: &mut usize,
    flow: &mut usize,
    block: usize,
    limit: usize,
    open_quote: &mut Option<char>,
) -> bool {
    while *index < chars.len() && *flow > 0 {
        let ch = chars[*index];
        match ch {
            '\'' | '"' => {
                let starts_scalar = quote_starts_scalar(chars, *index);
                *index += 1;
                if !consume_quoted(chars, index, ch) && starts_scalar {
                    *open_quote = Some(ch);
                }
            }
            '[' | '{' => {
                *flow += 1;
                *index += 1;
                if depth_exceeds(block, *flow, limit) {
                    return true;
                }
            }
            ']' | '}' => {
                *flow = flow.saturating_sub(1);
                *index += 1;
            }
            '#' if *index == 0 || chars[*index - 1] == ' ' || chars[*index - 1] == '\t' => {
                *index = chars.len();
            }
            _ => *index += 1,
        }
    }
    false
}

#[derive(Clone)]
enum Node {
    Scalar {
        value: String,
        line: usize,
    },
    Sequence {
        /// 1-based line of the sequence start.
        line: usize,
        items: Vec<Node>,
    },
    Mapping {
        /// 1-based line of the mapping start.
        line: usize,
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

/// Upper bound on the total number of nodes a document may expand to, counting
/// every node reached through an alias as many times as it is aliased. This
/// caps "billion laughs" style anchor/alias chains: expanding past the budget
/// is a normal parse error rather than unbounded cloning.
const MAX_EXPANDED_NODES: usize = 100_000;

struct Cursor<'a> {
    events: &'a [(Event, Marker)],
    index: usize,
    expanded: usize,
}

fn node_size(node: &Node) -> usize {
    match node {
        Node::Scalar { .. } => 1,
        Node::Sequence { items, .. } => 1 + items.iter().map(node_size).sum::<usize>(),
        Node::Mapping { entries, .. } => {
            1 + entries
                .iter()
                .map(|entry| node_size(&entry.value))
                .sum::<usize>()
        }
    }
}

fn document_root(events: &[(Event, Marker)]) -> Result<Option<Node>, usize> {
    let mut cursor = Cursor {
        events,
        index: 0,
        expanded: 0,
    };
    let mut chosen = None;
    let mut document_seen = false;
    while cursor.index < events.len() {
        let is_document = matches!(events[cursor.index].0, Event::DocumentStart);
        if !is_document {
            cursor.index += 1;
            continue;
        }
        let line = events[cursor.index].1.line().max(1);
        if document_seen {
            return Err(line);
        }
        document_seen = true;
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
                self.charge(1, line)?;
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
                let node = anchors.get(&id).ok_or(line)?;
                self.charge(node_size(node), line)?;
                Ok(node.clone())
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
        self.charge(1, line)?;
        let node = Node::Sequence { line, items };
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
        self.charge(1, line)?;
        let node = Node::Mapping {
            line,
            entries,
            duplicates,
        };
        self.finish_anchor(anchors, building, anchor, &node);
        Ok(node)
    }

    fn charge(&mut self, amount: usize, line: usize) -> Result<(), usize> {
        self.expanded = self.expanded.saturating_add(amount);
        if self.expanded > MAX_EXPANDED_NODES {
            Err(line)
        } else {
            Ok(())
        }
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
            ..
        } => {
            out.extend(duplicates.iter().cloned());
            for item in entries {
                collect_duplicates(&item.value, out);
            }
        }
        Node::Sequence { items, .. } => {
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

/// Later key wins. `steps`, `name`, `uses`, and `run` match without ASCII case.
fn entry_last<'a>(entries: &'a [Entry], key: &str) -> Option<&'a Entry> {
    entries
        .iter()
        .rev()
        .find(|item| item.key.eq_ignore_ascii_case(key))
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
        concurrency_line: None,
        jobs: Vec::new(),
        duplicate_keys: dedupe_duplicates(duplicate_keys),
    };
    let Some(Node::Mapping { entries, .. }) = root else {
        return workflow;
    };
    for item in entries {
        if item.key == "on" {
            note_on(&mut workflow, item);
        } else if item.key.eq_ignore_ascii_case("concurrency") {
            note_concurrency(&mut workflow, item.line, &item.value);
        } else if item.key == "jobs" {
            note_jobs(&mut workflow, &item.value);
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

fn note_concurrency(workflow: &mut Workflow, line: usize, node: &Node) {
    workflow.concurrency_line = Some(line);
    workflow.cancel_in_progress = concurrency_enabled(node);
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
            steps: Vec::new(),
            matrix: None,
            cancel_in_progress: false,
            concurrency_line: None,
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
    if let Some(item) = entry_last(entries, "steps") {
        job.steps_text = step_runs(&item.value);
        job.steps = steps_in(&item.value);
    }
    if let Some(item) = entry(entries, "strategy") {
        job.matrix = matrix_from(&item.value);
    }
    if let Some(item) = entries
        .iter()
        .rev()
        .find(|item| item.key.eq_ignore_ascii_case("concurrency"))
    {
        job.concurrency_line = Some(item.line);
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
        let Some(run) = entry_last(entries, "run") else {
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

fn steps_in(node: &Node) -> Vec<Step> {
    match node {
        Node::Scalar { value, .. } if scalar_is_empty(value) => Vec::new(),
        other => {
            let mut steps = Vec::new();
            collect_steps(other, &mut steps, 0);
            steps
        }
    }
}

fn scalar_is_empty(value: &str) -> bool {
    let value = value.trim();
    value.is_empty() || value == "~" || value.eq_ignore_ascii_case("null")
}

fn collect_steps(node: &Node, out: &mut Vec<Step>, depth: usize) {
    if depth > MAX_NODE_DEPTH {
        out.push(Step {
            name: String::new(),
            line: node_line(node),
            local_action: None,
        });
        return;
    }
    match node {
        Node::Sequence { items, .. } => {
            for item in items {
                collect_steps(item, out, depth + 1);
            }
        }
        Node::Mapping { entries, line, .. } => out.push(step_from_entries(entries, *line)),
        Node::Scalar { value, line } => {
            let name = if scalar_is_empty(value) {
                String::new()
            } else {
                first_content_line(value)
            };
            out.push(Step {
                name,
                line: *line,
                local_action: None,
            });
        }
    }
}

fn step_from_entries(entries: &[Entry], line: usize) -> Step {
    let local_action = local_action_of(entries);
    let mut step = if let Some(step) = step_from_key(entries, "name") {
        step
    } else if let Some(step) = step_from_key(entries, "uses") {
        step
    } else if let Some(step) = step_from_key(entries, "run") {
        step
    } else {
        Step {
            name: String::new(),
            line: entries.first().map_or(line, |item| item.line),
            local_action: None,
        }
    };
    step.local_action = local_action;
    step
}

fn step_from_key(entries: &[Entry], key: &str) -> Option<Step> {
    let item = entry_last(entries, key)?;
    let text = scalar_text(&item.value)?;
    if scalar_is_empty(text) {
        return None;
    }
    let name = first_content_line(text);
    if name.is_empty() {
        return None;
    }
    Some(Step {
        name,
        line: item.line,
        local_action: None,
    })
}

fn local_action_of(entries: &[Entry]) -> Option<LocalAction> {
    let item = entry_last(entries, "uses")?;
    let text = scalar_text(&item.value)?;
    local_action_spec(item.line, text)
}

/// `./path` to a local action. Reusable workflow calls and remote actions are not.
fn local_action_spec(line: usize, raw: &str) -> Option<LocalAction> {
    let spec = trim_breaks(raw);
    let spec = spec.trim();
    if spec.is_empty()
        || spec.contains('@')
        || spec.contains("://")
        || spec.chars().any(|character| character.is_control())
    {
        return None;
    }
    if local_reusable_call(line, spec).is_some() {
        return None;
    }
    let relative = normalize_action_dir(spec)?;
    if is_workflow_relative(&relative) {
        return None;
    }
    Some(LocalAction {
        line,
        spec: spec.to_string(),
        relative,
    })
}

/// Directory relative to the repository root.
///
/// `./` and `./action.yml` are the repository root. `..` that leaves the
/// root is rejected. An empty string means the root.
fn normalize_action_dir(spec: &str) -> Option<String> {
    let rest = spec
        .strip_prefix("./")
        .or_else(|| spec.strip_prefix(".\\"))?;
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
    if parts.last().is_some_and(|part| is_action_file_name(part)) {
        parts.pop();
    }
    Some(parts.join("/"))
}

fn is_action_file_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("action.yml") || name.eq_ignore_ascii_case("action.yaml")
}

/// `.github/workflows/<file>.yml` is a reusable workflow, not an action directory.
fn is_workflow_relative(relative: &str) -> bool {
    let mut parts = relative.split('/');
    let Some(github) = parts.next() else {
        return false;
    };
    let Some(workflows) = parts.next() else {
        return false;
    };
    let Some(file) = parts.next() else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }
    github.eq_ignore_ascii_case(".github")
        && workflows.eq_ignore_ascii_case("workflows")
        && valid_workflow_file(file)
}

/// Repository root for a file at `<root>/.github/workflows/<file>`.
pub fn repository_root(workflow_file: &Path) -> Option<PathBuf> {
    let workflows = workflow_file.parent()?;
    if !file_name_eq(workflows, "workflows") {
        return None;
    }
    let github = workflows.parent()?;
    if !file_name_eq(github, ".github") {
        return None;
    }
    github.parent().map(Path::to_path_buf)
}

fn file_name_eq(path: &Path, expected: &str) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(expected))
}

/// `action.yml` or `action.yaml` inside `root/relative`.
///
/// An empty `relative` is the repository root. Exact `action.yml` wins, then
/// exact `action.yaml`, then a case-insensitive match. `Ok(None)` when the
/// directory or the action file is absent.
pub fn locate_action(root: &Path, relative: &str) -> Result<Option<PathBuf>, ScanError> {
    let dir = join_relative(root, relative);
    if dir.is_file() && is_action_file_name_os(dir.file_name()) {
        return Ok(Some(dir));
    }
    if !dir.is_dir() {
        return Ok(None);
    }
    let entries = fs::read_dir(&dir).map_err(|_| ScanError::directory(&dir))?;
    let mut exact_yml = None;
    let mut exact_yaml = None;
    let mut folded_yml = None;
    let mut folded_yaml = None;
    for entry in entries {
        let entry = entry.map_err(|_| ScanError::directory(&dir))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(text) = name.to_str() else {
            continue;
        };
        if text == "action.yml" {
            exact_yml = Some(path);
        } else if text == "action.yaml" {
            exact_yaml = Some(path);
        } else if folded_yml.is_none() && text.eq_ignore_ascii_case("action.yml") {
            folded_yml = Some(path);
        } else if folded_yaml.is_none() && text.eq_ignore_ascii_case("action.yaml") {
            folded_yaml = Some(path);
        }
    }
    Ok(exact_yml.or(exact_yaml).or(folded_yml).or(folded_yaml))
}

fn is_action_file_name_os(name: Option<&std::ffi::OsStr>) -> bool {
    name.and_then(|name| name.to_str())
        .is_some_and(is_action_file_name)
}

fn join_relative(root: &Path, relative: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    for part in relative.split('/') {
        if !part.is_empty() && part != "." && part != ".." {
            path.push(part);
        }
    }
    path
}

/// Read one action file. Invalid YAML names the file and the line.
pub fn load_action(path: &Path) -> Result<ParsedAction, ScanError> {
    let text = fs::read_to_string(path).map_err(|_| ScanError::file(path))?;
    parse_action(path, &text)
}

/// Parse action YAML. Invalid YAML names the file and the line.
pub fn parse_action(path: &Path, text: &str) -> Result<ParsedAction, ScanError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let events = collect_events(text).map_err(|line| ScanError::yaml(path, line))?;
    let root = document_root(&events).map_err(|line| ScanError::yaml(path, line))?;
    Ok(action_from(path, root.as_ref()))
}

fn action_from(path: &Path, root: Option<&Node>) -> ParsedAction {
    let empty = ParsedAction {
        path: path.to_path_buf(),
        using: String::new(),
        steps: Vec::new(),
    };
    let Some(Node::Mapping { entries, .. }) = root else {
        return empty;
    };
    let Some(runs) = entry_last(entries, "runs") else {
        return empty;
    };
    let Node::Mapping { entries, .. } = &runs.value else {
        return ParsedAction {
            path: path.to_path_buf(),
            using: String::new(),
            steps: Vec::new(),
        };
    };
    let using = entry_last(entries, "using")
        .and_then(|item| scalar_text(&item.value))
        .map(|text| trim_breaks(text).trim().to_string())
        .unwrap_or_default();
    let steps = entry_last(entries, "steps")
        .map(|item| action_steps(&item.value))
        .unwrap_or_default();
    ParsedAction {
        path: path.to_path_buf(),
        using,
        steps,
    }
}

fn action_steps(node: &Node) -> Vec<ActionStep> {
    match node {
        Node::Sequence { items, .. } => items
            .iter()
            .filter_map(|item| match item {
                Node::Mapping { entries, .. } => Some(action_step_from(entries)),
                _ => None,
            })
            .collect(),
        Node::Mapping { entries, .. } => vec![action_step_from(entries)],
        _ => Vec::new(),
    }
}

fn action_step_from(entries: &[Entry]) -> ActionStep {
    let name = entry_last(entries, "name").and_then(|item| {
        let text = scalar_text(&item.value)?;
        let text = first_content_line(text);
        if text.is_empty() || scalar_is_empty(&text) {
            None
        } else {
            Some((text, item.line))
        }
    });
    let run = entry_last(entries, "run").and_then(|item| {
        let text = scalar_text(&item.value)?;
        let text = trim_breaks(text);
        if text.trim().is_empty() {
            None
        } else {
            Some((text, item.line))
        }
    });
    let (name, name_line) = match name {
        Some((text, line)) => (text, Some(line)),
        None => (String::new(), None),
    };
    let (run_text, run_line) = match run {
        Some((text, line)) => (text, Some(line)),
        None => (String::new(), None),
    };
    ActionStep {
        name,
        name_line,
        run_text,
        run_line,
        local_action: local_action_of(entries),
    }
}

fn first_content_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_string()
}

fn node_line(node: &Node) -> usize {
    match node {
        Node::Scalar { line, .. } | Node::Sequence { line, .. } | Node::Mapping { line, .. } => {
            *line
        }
    }
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
        Node::Sequence { items, .. } => {
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
        Node::Sequence { items, .. } => items.iter().filter_map(runner_label_set).collect(),
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
        Node::Sequence { items, .. } => items
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
    fn billion_laughs_sequence_aliases_are_a_parse_error() {
        let text = "a0: &a0 [\"x\",\"x\",\"x\",\"x\",\"x\",\"x\",\"x\",\"x\",\"x\",\"x\"]\n\
a1: &a1 [*a0,*a0,*a0,*a0,*a0,*a0,*a0,*a0,*a0,*a0]\n\
a2: &a2 [*a1,*a1,*a1,*a1,*a1,*a1,*a1,*a1,*a1,*a1]\n\
a3: &a3 [*a2,*a2,*a2,*a2,*a2,*a2,*a2,*a2,*a2,*a2]\n\
a4: &a4 [*a3,*a3,*a3,*a3,*a3,*a3,*a3,*a3,*a3,*a3]\n\
a5: &a5 [*a4,*a4,*a4,*a4,*a4,*a4,*a4,*a4,*a4,*a4]\n\
a6: &a6 [*a5,*a5,*a5,*a5,*a5,*a5,*a5,*a5,*a5,*a5]\n\
a7: &a7 [*a6,*a6,*a6,*a6,*a6,*a6,*a6,*a6,*a6,*a6]\n\
a8: &a8 [*a7,*a7,*a7,*a7,*a7,*a7,*a7,*a7,*a7,*a7]\n\
jobs:\n  job1:\n    runs-on: ubuntu-latest\n    env: *a8\n";
        let err = scan_yaml(Path::new("laughs.yml"), text).expect_err("laughs.yml");
        assert_eq!(err.path, Path::new("laughs.yml"));
        assert!(matches!(err.kind, ScanErrorKind::Yaml { .. }), "{err}");
    }

    #[test]
    fn billion_laughs_merge_key_aliases_are_a_parse_error() {
        let mut text =
            String::from("a0: &a0 [\"x\",\"x\",\"x\",\"x\",\"x\",\"x\",\"x\",\"x\",\"x\",\"x\"]\n");
        for level in 1..=8 {
            let previous = level - 1;
            text.push_str(&format!("a{level}: &a{level} ["));
            for i in 0..10 {
                if i > 0 {
                    text.push(',');
                }
                text.push_str(&format!("*a{previous}"));
            }
            text.push_str("]\n");
        }
        text.push_str("jobs:\n  job1:\n    runs-on: ubuntu-latest\n    <<: *a8\n");
        let err = scan_yaml(Path::new("laughs-merge.yml"), &text).expect_err("laughs-merge.yml");
        assert_eq!(err.path, Path::new("laughs-merge.yml"));
        assert!(matches!(err.kind, ScanErrorKind::Yaml { .. }), "{err}");
    }

    #[test]
    fn repeated_non_multiplying_alias_reuse_stays_within_budget() {
        let mut text =
            String::from("defaults: &defaults\n  runs-on: ubuntu-latest\n  timeout-minutes: 5\n");
        text.push_str("jobs:\n");
        for i in 0..500 {
            text.push_str(&format!("  job{i}:\n    <<: *defaults\n"));
        }
        let workflow = must_scan("many-jobs.yml", &text);
        assert_eq!(workflow.jobs.len(), 500);
        for job in &workflow.jobs {
            assert_eq!(job.runs_on.as_ref().unwrap().value, "ubuntu-latest");
            assert!(job.has_timeout_minutes);
        }
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
    fn nesting_past_the_node_limit_is_invalid_yaml_on_that_line() {
        let shallow =
            "jobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo ok\n";
        assert!(nesting_too_deep(shallow, MAX_NODE_DEPTH).is_none());

        let mut wide = String::from("items:\n");
        for i in 0..200 {
            wide.push_str(&format!("- item{i}\n"));
        }
        assert!(
            nesting_too_deep(&wide, MAX_NODE_DEPTH).is_none(),
            "sibling sequence entries are one collection, not nested depth"
        );

        let compact = format!("on:\n{}1\n", "- ".repeat(200));
        assert_eq!(nesting_too_deep(&compact, MAX_NODE_DEPTH), Some(2));
        let err = scan_yaml(Path::new("deep.yml"), &compact).unwrap_err();
        assert_eq!(err.to_string(), "line 2: invalid YAML");
        assert!(!err.to_string().contains("on:"), "{err}");
    }

    #[test]
    fn multiline_quoted_scalar_in_flow_does_not_hide_depth() {
        // The `]` on each continuation line is scalar text, not a closer.
        let mut deep = String::from("env:\n  X: [\"z\n    ]\"");
        for _ in 0..20_000 {
            deep.push_str(", [\"z\n    ]\"");
        }
        deep.push_str("]\n");
        assert!(nesting_too_deep(&deep, MAX_NODE_DEPTH).is_some());

        let shallow = "env:\n  X: [\"a\n    ]b\", 'c\n    ]d', e]\n  Y: 1\n";
        assert!(nesting_too_deep(shallow, MAX_NODE_DEPTH).is_none());
    }

    #[test]
    fn quote_after_anchor_or_tag_in_flow_does_not_hide_depth() {
        for prefix in ["&a ", "!!str ", "&a !!str "] {
            let mut deep = format!("env:\n  X: [{prefix}\"z\n    ]\"");
            for _ in 0..20_000 {
                deep.push_str(&format!(", [{prefix}\"z\n    ]\""));
            }
            deep.push_str("]\n");
            assert!(
                nesting_too_deep(&deep, MAX_NODE_DEPTH).is_some(),
                "prefix {prefix:?}"
            );
        }

        let shallow = [
            "a: [&a \"x\", b]\n",
            "a: [!!str \"x\n  y\", 1]\n",
            "a: [&a \"x\", *a]\n",
            "a: [&a \"x\n  ]y\", *a]\nb: 1\n",
        ];
        for text in shallow {
            assert!(nesting_too_deep(text, MAX_NODE_DEPTH).is_none(), "{text:?}");
        }
    }

    #[test]
    fn explicit_indent_indicator_does_not_hide_depth() {
        // `run:` is at column 8 and `|2` needs content at column 10, so the
        // column-9 line is structure, not scalar text.
        let deep = format!(
            "steps:\n  - name: x\n    run: |2\n     {}leaf\n",
            "- ".repeat(20_000)
        );
        assert!(nesting_too_deep(&deep, MAX_NODE_DEPTH).is_some());

        let shallow =
            "steps:\n  - name: x\n    run: |2\n        - not a list\n      indented\n    next: 1\n";
        assert!(nesting_too_deep(shallow, MAX_NODE_DEPTH).is_none());
    }

    #[test]
    fn a_single_leading_document_marker_or_trailing_end_marker_still_works() {
        let leading = must_scan(
            "leading.yml",
            "---\njobs:\n  a:\n    runs-on: ubuntu-latest\n",
        );
        assert_eq!(leading.jobs.len(), 1);
        assert_eq!(leading.jobs[0].name, "a");

        let trailing_end = must_scan(
            "trailing-end.yml",
            "jobs:\n  a:\n    runs-on: ubuntu-latest\n...\n",
        );
        assert_eq!(trailing_end.jobs.len(), 1);
        assert_eq!(trailing_end.jobs[0].name, "a");

        let both = must_scan(
            "both.yml",
            "---\njobs:\n  a:\n    runs-on: ubuntu-latest\n...\n",
        );
        assert_eq!(both.jobs.len(), 1);
        assert_eq!(both.jobs[0].name, "a");
    }

    #[test]
    fn a_second_yaml_document_is_a_parse_error_instead_of_a_silent_drop() {
        let cases = [
            (
                "two-docs.yml",
                "jobs:\n  a:\n    runs-on: ubuntu-latest\n---\njobs:\n  b:\n    runs-on: macos-latest\n",
                4,
            ),
            (
                "second-empty.yml",
                "jobs:\n  a:\n    runs-on: ubuntu-latest\n---\n",
                4,
            ),
            (
                "crlf-two-docs.yml",
                "jobs:\r\n  a:\r\n    runs-on: ubuntu-latest\r\n---\r\njobs:\r\n  b:\r\n    runs-on: macos-latest\r\n",
                4,
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
            assert!(!err.to_string().contains("macos"), "{name}: {err}");
        }
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

    fn flat_steps(text: &str) -> Vec<(String, usize)> {
        let workflow = must_scan("steps.yml", text);
        workflow
            .jobs
            .iter()
            .flat_map(|job| job.steps.iter().map(|step| (step.name.clone(), step.line)))
            .collect()
    }

    fn pair(name: &str, line: usize) -> (String, usize) {
        (name.to_string(), line)
    }

    #[test]
    fn sample_files_keep_step_names_and_lines() {
        let workflows = scan_dir(&sample_dir()).unwrap();
        let lint = &by_name(&workflows, "lint.yml").jobs[0];
        assert_eq!(
            lint.steps_text,
            "cargo fmt --check\ncargo clippy -- -D warnings"
        );
        assert_eq!(
            lint.steps
                .iter()
                .map(|step| (step.name.as_str(), step.line))
                .collect::<Vec<_>>(),
            vec![
                ("actions/checkout@v4", 10),
                ("cargo fmt --check", 11),
                ("cargo clippy -- -D warnings", 12),
            ]
        );
        let ci = &by_name(&workflows, "ci.yml").jobs[0];
        assert_eq!(
            ci.steps
                .iter()
                .map(|step| (step.name.as_str(), step.line))
                .collect::<Vec<_>>(),
            vec![("actions/checkout@v4", 12), ("cargo test", 13)]
        );
        let docs = &by_name(&workflows, "docs.yml").jobs[0];
        assert_eq!(
            docs.steps
                .iter()
                .map(|step| (step.name.as_str(), step.line))
                .collect::<Vec<_>>(),
            vec![("actions/checkout@v4", 7), ("cargo doc --no-deps", 8)]
        );
        let release = &by_name(&workflows, "release.yml").jobs[0];
        assert_eq!(
            release
                .steps
                .iter()
                .map(|step| (step.name.as_str(), step.line))
                .collect::<Vec<_>>(),
            vec![("actions/checkout@v4", 10), ("cargo build --release", 11)]
        );
    }

    #[test]
    fn block_steps_keep_name_uses_and_run_lines() {
        let text = "\
jobs:
  lint:
    steps:
      - name: Checkout
        uses: actions/checkout@v4
      - name: say \"hi\"
        run: echo hi
      - uses: ./tools/lint
  test:
    steps:
      - run: cargo test
";
        assert_eq!(
            flat_steps(text),
            vec![
                pair("Checkout", 4),
                pair("say \"hi\"", 6),
                pair("./tools/lint", 8),
                pair("cargo test", 11),
            ]
        );
        let workflow = must_scan("steps.yml", text);
        assert_eq!(workflow.jobs[0].steps_text, "echo hi");
        assert_eq!(workflow.jobs[1].steps_text, "cargo test");
    }

    #[test]
    fn multiline_run_keeps_the_first_line_and_the_run_key_line() {
        let text = "\
jobs:
  lint:
    steps:
      - run: |
          echo one
          echo two
      - name: Script
        run: |
          cargo test -- --test-threads=1
          echo done
      - name: |
          Lint the tree
          carefully
        run: cargo fmt --check
      - run: |
          # lint
          cargo fmt --check
      - run: |

          echo one
      - run: >
          echo one
          echo two
";
        assert_eq!(
            flat_steps(text),
            vec![
                pair("echo one", 4),
                pair("Script", 7),
                pair("Lint the tree", 11),
                pair("# lint", 15),
                pair("echo one", 18),
                pair("echo one echo two", 21),
            ]
        );
        let workflow = must_scan("steps.yml", text);
        assert_eq!(
            workflow.jobs[0].steps_text,
            "echo one\necho two\ncargo test -- --test-threads=1\necho done\ncargo fmt --check\n# lint\ncargo fmt --check\n\necho one\necho one echo two"
        );
    }

    #[test]
    fn step_without_a_name_uses_uses_then_the_first_run_line() {
        let text = "\
jobs:
  lint:
    steps:
      - uses: actions/checkout@v4
      - run: cargo test
      - run: cargo test
        uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        run: npm test
";
        assert_eq!(
            flat_steps(text),
            vec![
                pair("actions/checkout@v4", 4),
                pair("cargo test", 5),
                pair("actions/checkout@v4", 7),
                pair("actions/setup-node@v4", 8),
            ]
        );
    }

    #[test]
    fn flow_steps_keep_key_lines() {
        let inline = "jobs: {lint: {steps: [{name: Checkout, uses: actions/checkout@v4}, {run: cargo test}]}}\n";
        assert_eq!(
            flat_steps(inline),
            vec![pair("Checkout", 1), pair("cargo test", 1)]
        );

        let split = "\
jobs:
  lint:
    steps: [{name: Lint, run: cargo fmt --check}, {uses: actions/checkout@v4}]
";
        assert_eq!(
            flat_steps(split),
            vec![pair("Lint", 3), pair("actions/checkout@v4", 3)]
        );

        let mixed = "\
jobs:
  lint:
    steps:
      - {name: Lint, run: cargo fmt --check}
      - run: |
          echo one
          echo two
";
        assert_eq!(
            flat_steps(mixed),
            vec![pair("Lint", 4), pair("echo one", 5)]
        );

        let quoted = "\
jobs:
  lint:
    steps: [{run: \"echo one\\necho two\"}]
";
        assert_eq!(flat_steps(quoted), vec![pair("echo one", 3)]);
        assert_eq!(
            must_scan("steps.yml", quoted).jobs[0].steps_text,
            "echo one\necho two"
        );
    }

    #[test]
    fn crlf_steps_keep_the_same_lines() {
        let text = "\
jobs:
  lint:
    steps:
      - name: Checkout
        uses: actions/checkout@v4
      - run: |
          echo one
          echo two
";
        assert_eq!(flat_steps(&crlf(text)), flat_steps(text));
        assert_eq!(
            flat_steps(&crlf(text)),
            vec![pair("Checkout", 4), pair("echo one", 6)]
        );
    }

    #[test]
    fn later_keys_choose_the_step_name_and_its_line() {
        let ordered = "\
jobs:
  lint:
    steps:
      - run: cargo test
        name: Run the suite
      - run: echo hidden
        uses: actions/checkout@v4
      - name: First
        name: Second
        run: cargo test
";
        assert_eq!(
            flat_steps(ordered),
            vec![
                pair("Run the suite", 5),
                pair("actions/checkout@v4", 7),
                pair("Second", 9),
            ]
        );

        let capitals = "\
jobs:
  lint:
    STEPS:
      - RUN: cargo test
        NAME: Suite
";
        assert_eq!(flat_steps(&crlf(capitals)), vec![pair("Suite", 5)]);
        assert_eq!(
            must_scan("steps.yml", capitals).jobs[0].steps_text,
            "cargo test"
        );

        let replaced = "\
jobs:
  lint:
    steps:
      - run: cargo test
    STEPS:
      - run: echo later
";
        assert_eq!(flat_steps(replaced), vec![pair("echo later", 6)]);
    }

    #[test]
    fn empty_and_missing_step_fields_stay_addressable() {
        let missing = "\
jobs:
  lint:
    runs-on: ubuntu-latest
";
        assert!(flat_steps(missing).is_empty());
        assert!(flat_steps("jobs:\n  lint:\n    steps: []\n").is_empty());
        assert!(flat_steps("jobs:\n  lint:\n    steps:\n").is_empty());
        assert!(flat_steps("jobs:\n  lint:\n    steps: null\n").is_empty());
        assert!(flat_steps("jobs:\n  lint:\n    steps: ~\n").is_empty());

        let blanks = "\
jobs:
  lint:
    steps:
      - name: \"\"
        run: cargo test
      - name: \"  \"
        uses: actions/setup-node@v4
      - name:
        run: echo hi
      - name: null
        run: echo null-name
      - name: ~
        uses: actions/checkout@v4
      - id: setup
      -
      - {}
      - run: echo kept
";
        assert_eq!(
            flat_steps(blanks),
            vec![
                pair("cargo test", 5),
                pair("actions/setup-node@v4", 7),
                pair("echo hi", 9),
                pair("echo null-name", 11),
                pair("actions/checkout@v4", 13),
                pair("", 14),
                pair("", 16),
                pair("", 16),
                pair("echo kept", 17),
            ]
        );

        let scalar = "\
jobs:
  lint:
    steps: cargo test
";
        assert_eq!(flat_steps(scalar), vec![pair("cargo test", 3)]);

        let mapping = "\
jobs:
  lint:
    steps:
      name: Only
      run: cargo test
";
        assert_eq!(flat_steps(mapping), vec![pair("Only", 4)]);

        let nested = "\
jobs:
  lint:
    steps:
      -
        - run: echo inner
";
        assert_eq!(flat_steps(nested), vec![pair("echo inner", 5)]);
    }

    #[test]
    fn capital_step_keys_keep_lines() {
        let text = "\
jobs:
  lint:
    Steps:
      - NAME: Checkout
        USES: actions/checkout@v4
      - RUN: |
          echo one
          echo two
";
        assert_eq!(
            flat_steps(text),
            vec![pair("Checkout", 4), pair("echo one", 6)]
        );
        assert_eq!(
            must_scan("steps.yml", text).jobs[0].steps_text,
            "echo one\necho two"
        );
        assert_eq!(flat_steps(&crlf(text)), flat_steps(text));
    }

    #[test]
    fn anchors_keep_the_step_source_line() {
        let inherited = "\
defaults: &defaults
  steps:
    - run: cargo test
jobs:
  test:
    <<: *defaults
";
        assert_eq!(flat_steps(inherited), vec![pair("cargo test", 3)]);

        let overridden = "\
defaults: &defaults
  steps:
    - run: cargo test
jobs:
  test:
    <<: *defaults
    steps:
      - name: Real
        run: cargo fmt --check
";
        assert_eq!(flat_steps(overridden), vec![pair("Real", 8)]);

        let merged_step = "\
checkout: &checkout
  name: Checkout
  uses: actions/checkout@v4
jobs:
  test:
    steps:
      - <<: *checkout
      - run: cargo test
";
        assert_eq!(
            flat_steps(merged_step),
            vec![pair("Checkout", 2), pair("cargo test", 8)]
        );

        let bom = "\u{feff}jobs:\n  lint:\n    steps:\n      - run: cargo test\n";
        assert_eq!(flat_steps(bom), vec![pair("cargo test", 4)]);
    }

    #[test]
    fn local_action_uses_are_recorded_on_steps() {
        let text = "\
jobs:
  build:
    steps:
      - uses: ./.github/actions/lint
      - name: Checkout
        uses: actions/checkout@v4
      - uses: ./.github/workflows/lint.yml
      - uses: docker://alpine
      - uses: ./.github/actions/../actions/./lint
      - uses: ./.github\\actions\\lint
      - uses: ./.github/actions/lint/action.yml
      - uses: ./
      - uses: ./../outside
      - uses: .\\.github\\workflows\\lint.yml
      - USES: \"./.github/actions/lint\"
";
        let job = must_scan("repo/.github/workflows/ci.yml", text)
            .jobs
            .remove(0);
        let recorded: Vec<Option<(&str, &str, usize)>> = job
            .steps
            .iter()
            .map(|step| {
                step.local_action
                    .as_ref()
                    .map(|action| (action.spec.as_str(), action.relative.as_str(), action.line))
            })
            .collect();
        assert_eq!(
            recorded,
            vec![
                Some(("./.github/actions/lint", ".github/actions/lint", 4)),
                None,
                None,
                None,
                Some((
                    "./.github/actions/../actions/./lint",
                    ".github/actions/lint",
                    9
                )),
                Some(("./.github\\actions\\lint", ".github/actions/lint", 10)),
                Some((
                    "./.github/actions/lint/action.yml",
                    ".github/actions/lint",
                    11
                )),
                Some(("./", "", 12)),
                None,
                None,
                Some(("./.github/actions/lint", ".github/actions/lint", 15)),
            ]
        );
        assert_eq!(job.steps[1].name, "Checkout");
        assert_eq!(job.steps[10].line, 15);
    }

    #[test]
    fn parse_action_reads_composite_steps_and_ignores_other_runners() {
        let composite = parse_action(
            Path::new("action.yml"),
            "name: Lint\ndescription: Format\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: cargo fmt --check\n",
        )
        .unwrap();
        assert_eq!(composite.using, "composite");
        assert_eq!(composite.steps.len(), 1);
        assert_eq!(composite.steps[0].run_text, "cargo fmt --check");
        assert_eq!(composite.steps[0].run_line, Some(7));

        let reordered = parse_action(
            Path::new("action.yml"),
            "RUNS:\n  STEPS:\n    - name: prettier\n      shell: bash\n      run: echo ok\n  USING: \" Composite \"\n",
        )
        .unwrap();
        assert_eq!(reordered.using, "Composite");
        assert_eq!(reordered.steps[0].name, "prettier");
        assert_eq!(reordered.steps[0].name_line, Some(3));
        assert_eq!(reordered.steps[0].run_line, Some(5));

        let flow = parse_action(
            Path::new("action.yaml"),
            "runs: {using: composite, steps: [{run: cargo clippy -- -D warnings, shell: bash}]}\n",
        )
        .unwrap();
        assert_eq!(flow.using, "composite");
        assert_eq!(flow.steps[0].run_line, Some(1));
        assert!(flow.steps[0].run_text.contains("clippy"));

        let crlf = parse_action(
            Path::new("action.yml"),
            "runs:\r\n  using: composite\r\n  steps:\r\n    - run: cargo fmt --check\r\n",
        )
        .unwrap();
        assert_eq!(crlf.steps[0].run_line, Some(4));

        let bom = parse_action(
            Path::new("action.yml"),
            "\u{feff}runs:\n  using: node20\n  main: index.js\n",
        )
        .unwrap();
        assert_eq!(bom.using, "node20");
        assert!(bom.steps.is_empty());

        let docker = parse_action(
            Path::new("action.yml"),
            "runs:\n  using: docker\n  image: Dockerfile\n  steps:\n    - run: cargo fmt --check\n",
        )
        .unwrap();
        assert_eq!(docker.using, "docker");
        assert_eq!(docker.steps.len(), 1);

        let empty = parse_action(Path::new("action.yml"), "").unwrap();
        assert!(empty.using.is_empty());
        assert!(empty.steps.is_empty());

        let broken = parse_action(Path::new("action.yml"), "runs: [\nSECRET\n");
        let err = broken.expect_err("invalid yaml");
        assert!(err.to_string().contains("invalid YAML"));
        assert!(!err.to_string().contains("SECRET"));

        let nested = parse_action(
            Path::new("action.yml"),
            "runs:\n  using: composite\n  steps:\n    - uses: ./.github/actions/inner\n",
        )
        .unwrap();
        let inner = nested.steps[0].local_action.as_ref().unwrap();
        assert_eq!(inner.relative, ".github/actions/inner");
        assert_eq!(inner.line, 4);
    }
}
