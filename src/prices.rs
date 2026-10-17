//! Actions runner prices and the map from a workflow label to a SKU.
//!
//! Rates live in `prices/actions.json` and are loaded with `include_str`.
//! A workflow's own usage rows (`net / quantity`) replace the table rate.
//! `self-hosted` is decided before any OS label. `--runner-map` replaces
//! those built-in decisions for the labels it names.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::OnceLock;

use serde_json::{Map, Value};

use crate::money::{div_half_away, DecimalError, Micros};
use crate::usage::{is_actions_product, UsageError, UsageRow};

const ACTIONS_JSON: &str = include_str!("../prices/actions.json");
const NO_WORKFLOW: &str = "(no workflow)";

/// One SKU from the embedded price table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkuPrice {
    pub sku: String,
    pub usd_per_minute: Micros,
    pub billed_on_public: bool,
}

/// Embedded Actions minute prices.
#[derive(Debug, Clone)]
pub struct PriceTable {
    pub retrieved: String,
    pub effective_from: String,
    pub sources: Vec<String>,
    skus: BTreeMap<String, SkuPrice>,
    by_lower: HashMap<String, String>,
}

impl PriceTable {
    pub fn get(&self, sku: &str) -> Option<&SkuPrice> {
        self.by_lower
            .get(&sku.to_ascii_lowercase())
            .and_then(|name| self.skus.get(name))
    }

    pub fn iter(&self) -> impl Iterator<Item = &SkuPrice> {
        self.skus.values()
    }
}

/// Label keys (lower case, comma-joined) to a canonical SKU.
#[derive(Debug, Clone, Default)]
pub struct RunnerMap {
    entries: HashMap<String, String>,
}

impl RunnerMap {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(String::as_str)
    }
}

/// Why a price request failed before a rate could be chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PriceFail {
    /// Exit 2. `path` is the file that could not be used. Empty when there is no file.
    Input { path: String, message: String },
    /// Exit 3. The runner label is not in the built-in map or `--runner-map`.
    UnknownLabel(String),
    /// Exit 3. The flags disagree or a price flag is missing its partner.
    Usage(String),
}

impl PriceFail {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::UnknownLabel(_) | Self::Usage(_) => 3,
            Self::Input { .. } => 2,
        }
    }
}

impl std::fmt::Display for PriceFail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input { path, message } if path.is_empty() => write!(f, "{message}"),
            Self::Input { path, message } => write!(f, "{path}: {message}"),
            Self::UnknownLabel(label) => write!(f, "unknown runner label: {label}"),
            Self::Usage(message) => f.write_str(message),
        }
    }
}

/// Where `usd_per_minute` came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateSource {
    Table,
    Csv,
}

impl RateSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Table => "table",
            Self::Csv => "csv",
        }
    }
}

/// The minute rate for one resolved runner or SKU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quote {
    pub labels: Vec<String>,
    pub sku: String,
    pub usd_per_minute: Micros,
    pub billed_on_public: bool,
    pub source: RateSource,
    pub repository: Option<String>,
    pub workflow_path: Option<String>,
}

/// Load the embedded table. A broken embed is an input error with no path.
pub fn embedded_table() -> Result<&'static PriceTable, PriceFail> {
    static CELL: OnceLock<Result<PriceTable, String>> = OnceLock::new();
    match CELL.get_or_init(|| parse_table_json(ACTIONS_JSON).map_err(|err| err.to_string())) {
        Ok(table) => Ok(table),
        Err(message) => Err(PriceFail::Input {
            path: "prices/actions.json".to_string(),
            message: message.clone(),
        }),
    }
}

/// `ci-bill prices` output.
pub fn render(
    json: bool,
    runner_map: Option<&Path>,
    label: Option<&str>,
    sku: Option<&str>,
    usage: Option<&Path>,
    repository: Option<&str>,
    workflow: Option<&str>,
) -> Result<String, PriceFail> {
    if workflow.is_some() && usage.is_none() {
        return Err(PriceFail::Usage("--workflow requires --usage".to_string()));
    }
    if repository.is_some() && usage.is_none() {
        return Err(PriceFail::Usage(
            "--repository requires --usage".to_string(),
        ));
    }
    if usage.is_some() && label.is_none() && sku.is_none() {
        return Err(PriceFail::Usage(
            "--usage requires --label or --sku".to_string(),
        ));
    }

    let table = embedded_table()?;
    let map = match runner_map {
        Some(path) => load_runner_map(path, table)?,
        None => RunnerMap::default(),
    };

    match label {
        Some(raw) => {
            let labels = parse_runner_labels(raw);
            let resolved = resolve_runner(&labels, &map, table)
                .map_err(|unknown| PriceFail::UnknownLabel(unknown_label_text(raw, &unknown)))?;
            if let Some(requested) = sku {
                let wanted = table.get(requested).ok_or_else(|| PriceFail::Input {
                    path: String::new(),
                    message: format!("unknown sku {requested}"),
                })?;
                if !wanted.sku.eq_ignore_ascii_case(&resolved) {
                    return Err(PriceFail::Usage(format!(
                        "label {raw} resolves to {resolved}, not {}",
                        wanted.sku
                    )));
                }
            }
            let rows = read_rows(usage)?;
            let quote = quote_sku(table, &rows, repository, workflow, &resolved, labels)?;
            Ok(render_quote(json, raw, &quote))
        }
        None => match sku {
            Some(requested) => {
                let price = table.get(requested).ok_or_else(|| PriceFail::Input {
                    path: String::new(),
                    message: format!("unknown sku {requested}"),
                })?;
                let rows = read_rows(usage)?;
                let quote = quote_sku(table, &rows, repository, workflow, &price.sku, Vec::new())?;
                Ok(render_quote(json, requested, &quote))
            }
            None => Ok(if json {
                render_table_json(table)
            } else {
                render_table_text(table)
            }),
        },
    }
}

fn read_rows(usage: Option<&Path>) -> Result<Vec<UsageRow>, PriceFail> {
    match usage {
        Some(path) => crate::usage::read_usage(path).map_err(|err| usage_fail(path, err)),
        None => Ok(Vec::new()),
    }
}

fn usage_fail(path: &Path, err: UsageError) -> PriceFail {
    PriceFail::Input {
        path: path.display().to_string(),
        message: err.to_string(),
    }
}

/// Split a `runs-on` value into labels.
///
/// Accepts `ubuntu-latest`, `[self-hosted, macos]`, `self-hosted, macos`,
/// and quoted pieces. Empty pieces are dropped.
pub fn parse_runner_labels(raw: &str) -> Vec<String> {
    let mut text = raw.trim();
    if let Some(unquoted) = unquote(text) {
        text = unquoted.trim();
    }
    let bracketed = text.starts_with('[') && text.ends_with(']') && text.len() >= 2;
    let inner = if bracketed {
        text[1..text.len() - 1].trim()
    } else {
        text
    };
    if bracketed || inner.contains(',') {
        inner
            .split(',')
            .map(clean_piece)
            .filter(|piece| !piece.is_empty())
            .collect()
    } else {
        let one = clean_piece(inner);
        if one.is_empty() {
            Vec::new()
        } else {
            vec![one]
        }
    }
}

fn clean_piece(raw: &str) -> String {
    let text = raw.trim();
    unquote(text).unwrap_or(text).trim().to_string()
}

/// One matching pair of quotes around the whole string.
fn unquote(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    if text.len() >= 2 {
        let open = bytes[0];
        let close = bytes[text.len() - 1];
        if (open == b'"' && close == b'"') || (open == b'\'' && close == b'\'') {
            return Some(&text[1..text.len() - 1]);
        }
    }
    None
}

/// Map key for a label list: lower case, commas, no spaces.
pub fn label_key(labels: &[String]) -> String {
    labels
        .iter()
        .map(|label| label.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(",")
}

/// Resolve labels to a canonical SKU.
///
/// The exact `--runner-map` key wins, including a multi-label key. Otherwise
/// a set that contains `self-hosted` is `self_hosted` (or that label's
/// override). Every other label must be known. The first label supplies the SKU.
pub fn resolve_runner(
    labels: &[String],
    map: &RunnerMap,
    table: &PriceTable,
) -> Result<String, String> {
    if labels.is_empty() {
        return Err(String::new());
    }
    let key = label_key(labels);
    if let Some(sku) = map.get(&key) {
        return Ok(sku.to_string());
    }
    if labels
        .iter()
        .any(|label| label.eq_ignore_ascii_case("self-hosted"))
    {
        let sku = map.get("self-hosted").unwrap_or("self_hosted");
        return table
            .get(sku)
            .map(|price| price.sku.clone())
            .ok_or_else(|| "self-hosted".to_string());
    }
    let mut chosen: Option<String> = None;
    for label in labels {
        let lower = label.to_ascii_lowercase();
        let sku = if let Some(overridden) = map.get(&lower) {
            overridden.to_string()
        } else if let Some(builtin) = builtin_sku(&lower, table) {
            builtin
        } else {
            return Err(label.clone());
        };
        if chosen.is_none() {
            chosen = Some(sku);
        }
    }
    chosen.ok_or_else(|| labels[0].clone())
}

/// SKU for the labels on a finished or running job.
///
/// A label list that contains `self-hosted` is `self_hosted`. Otherwise each
/// label is resolved on its own and labels the map does not name are ignored.
/// When several labels resolve, the larger runner wins: a core size beats an
/// arm, slim, large, or xlarge label, and those beat a standard runner. A tie
/// keeps the earlier label. No known label yields `None`.
pub fn sku_for_labels(labels: &[String]) -> Option<String> {
    let table = embedded_table().ok()?;
    let map = RunnerMap::default();
    let cleaned: Vec<String> = labels
        .iter()
        .map(|label| label.trim().to_string())
        .filter(|label| !label.is_empty())
        .collect();
    if cleaned
        .iter()
        .any(|label| label.eq_ignore_ascii_case("self-hosted"))
    {
        return resolve_runner(&["self-hosted".to_string()], &map, table).ok();
    }
    let mut best: Option<(u8, String)> = None;
    for label in &cleaned {
        let one = [label.clone()];
        let Ok(sku) = resolve_runner(&one, &map, table) else {
            continue;
        };
        let score = label_specificity(&sku);
        if best.as_ref().is_none_or(|pair| score > pair.0) {
            best = Some((score, sku));
        }
    }
    best.map(|(_, sku)| sku)
}

/// Larger runners outrank the standard image they also advertise.
fn label_specificity(sku: &str) -> u8 {
    if sku.contains("_core") {
        3
    } else if sku.ends_with("_arm")
        || sku.ends_with("_slim")
        || sku.ends_with("_xl")
        || sku.ends_with("_l")
    {
        2
    } else {
        1
    }
}

fn unknown_label_text(raw: &str, unknown: &str) -> String {
    if !unknown.is_empty() {
        return unknown.to_string();
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        "\"\"".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Minute rate for `sku` on one repository workflow.
///
/// Rows are this workflow's own Actions rows for that SKU. `net / quantity`
/// wins when the quantity sum is not zero. Otherwise the table rate is used.
/// With no repository filter, two repositories that both have quantity is an
/// input error.
pub fn quote_sku(
    table: &PriceTable,
    rows: &[UsageRow],
    repository: Option<&str>,
    workflow: Option<&str>,
    sku: &str,
    labels: Vec<String>,
) -> Result<Quote, PriceFail> {
    let price = table.get(sku).ok_or_else(|| PriceFail::Input {
        path: String::new(),
        message: format!("unknown sku {sku}"),
    })?;
    let mut quote = Quote {
        labels,
        sku: price.sku.clone(),
        usd_per_minute: price.usd_per_minute,
        billed_on_public: price.billed_on_public,
        source: RateSource::Table,
        repository: None,
        workflow_path: None,
    };
    let Some(workflow) = workflow else {
        return Ok(quote);
    };
    let wanted_workflow = if workflow.is_empty() {
        NO_WORKFLOW
    } else {
        workflow
    };
    let mut totals: BTreeMap<String, RepoTotal> = BTreeMap::new();
    for row in rows {
        if !row_matches(row, &price.sku, repository, wanted_workflow) {
            continue;
        }
        let key = row.repository.to_ascii_lowercase();
        let entry = totals.entry(key).or_insert_with(|| RepoTotal {
            display: row.repository.clone(),
            net: Micros::ZERO,
            quantity: Micros::ZERO,
        });
        entry.net += row.net;
        entry.quantity += row.quantity;
    }
    let mut active: Vec<&RepoTotal> = totals
        .values()
        .filter(|total| total.quantity.0 != 0)
        .collect();
    if repository.is_none() && active.len() > 1 {
        active.sort_by(|left, right| left.display.cmp(&right.display));
        let names = active
            .iter()
            .map(|total| total.display.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(PriceFail::Input {
            path: String::new(),
            message: format!(
                "workflow {wanted_workflow} matches more than one repository: {names}"
            ),
        });
    }
    if let Some(total) = active.first() {
        let rate = per_minute_rate(total.net, total.quantity).ok_or_else(|| PriceFail::Input {
            path: String::new(),
            message: format!("price for {} does not fit", price.sku),
        })?;
        quote.usd_per_minute = rate;
        quote.source = RateSource::Csv;
        quote.repository = Some(total.display.clone());
        quote.workflow_path = Some(wanted_workflow.to_string());
    }
    Ok(quote)
}

struct RepoTotal {
    display: String,
    net: Micros,
    quantity: Micros,
}

fn row_matches(row: &UsageRow, sku: &str, repository: Option<&str>, workflow: &str) -> bool {
    if !is_actions_product(&row.product) || !row.sku.eq_ignore_ascii_case(sku) {
        return false;
    }
    if let Some(repository) = repository {
        if !row.repository.eq_ignore_ascii_case(repository) {
            return false;
        }
    }
    row.workflow_path.eq_ignore_ascii_case(workflow)
}

/// `net / quantity` in USD per minute, half away from zero at the micro.
fn per_minute_rate(net: Micros, quantity: Micros) -> Option<Micros> {
    if quantity.0 == 0 {
        return None;
    }
    let num = i128::from(net.0).checked_mul(i128::from(Micros::SCALE))?;
    let den = i128::from(quantity.0);
    i64::try_from(div_half_away(num, den)).ok().map(Micros)
}

fn builtin_sku(label: &str, table: &PriceTable) -> Option<String> {
    if label == "ubuntu-slim" {
        return canonical(table, "actions_linux_slim");
    }
    if let Some(sku) = larger_core_sku(label) {
        // A core-sized label that is not in the table stays unknown.
        return canonical(table, &sku);
    }
    if label.starts_with("ubuntu-") && label.ends_with("-arm") {
        return canonical(table, "actions_linux_arm");
    }
    if label.starts_with("windows-") && label.ends_with("-arm") {
        return canonical(table, "actions_windows_arm");
    }
    if (label.starts_with("macos-") || label.starts_with("xcode-")) && label.ends_with("-xlarge") {
        return canonical(table, "actions_macos_xl");
    }
    if label.starts_with("macos-") && label.ends_with("-large") {
        return canonical(table, "actions_macos_l");
    }
    if label.starts_with("ubuntu-") {
        return canonical(table, "actions_linux");
    }
    if label.starts_with("macos-") || label.starts_with("xcode-") {
        return canonical(table, "actions_macos");
    }
    if label.starts_with("windows-") {
        return canonical(table, "actions_windows");
    }
    None
}

fn canonical(table: &PriceTable, sku: &str) -> Option<String> {
    table.get(sku).map(|price| price.sku.clone())
}

/// `ubuntu-24.04-16core` and the same shape for Windows, ARM, GPU, and advanced.
fn larger_core_sku(label: &str) -> Option<String> {
    let (family, rest) = if let Some(rest) = label.strip_prefix("ubuntu-") {
        ("linux", rest)
    } else {
        let rest = label.strip_prefix("windows-")?;
        ("windows", rest)
    };
    let (body, kind) = strip_machine_kind(rest);
    let body = strip_core_word(body)?;
    let (image, digits) = body.rsplit_once('-')?;
    if image.is_empty()
        || digits.is_empty()
        || digits.starts_with('0')
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let cores: u32 = digits.parse().ok()?;
    let suffix = match kind {
        MachineKind::Plain => "",
        MachineKind::Arm => "_arm",
        MachineKind::Gpu => "_gpu",
        MachineKind::Advanced => "_advanced",
    };
    Some(format!("actions_{family}_{cores}_core{suffix}"))
}

#[derive(Clone, Copy)]
enum MachineKind {
    Plain,
    Arm,
    Gpu,
    Advanced,
}

fn strip_machine_kind(rest: &str) -> (&str, MachineKind) {
    if let Some(body) = rest.strip_suffix("-arm") {
        (body, MachineKind::Arm)
    } else if let Some(body) = rest.strip_suffix("-gpu") {
        (body, MachineKind::Gpu)
    } else if let Some(body) = rest.strip_suffix("-advanced") {
        (body, MachineKind::Advanced)
    } else {
        (rest, MachineKind::Plain)
    }
}

fn strip_core_word(body: &str) -> Option<&str> {
    let stripped = if let Some(prefix) = body.strip_suffix("cores") {
        prefix
    } else {
        body.strip_suffix("core")?
    };
    Some(stripped.strip_suffix('-').unwrap_or(stripped))
}

/// CSV with header names `label` and `sku`, in any order.
pub fn load_runner_map(path: &Path, table: &PriceTable) -> Result<RunnerMap, PriceFail> {
    let bytes = std::fs::read(path).map_err(|_| PriceFail::Input {
        path: path.display().to_string(),
        message: "line 0: cannot read file".to_string(),
    })?;
    parse_runner_map(&bytes, table).map_err(|message| PriceFail::Input {
        path: path.display().to_string(),
        message,
    })
}

fn parse_runner_map(bytes: &[u8], table: &PriceTable) -> Result<RunnerMap, String> {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .trim(csv::Trim::All)
        .from_reader(bytes);
    let headers = reader
        .headers()
        .map_err(|err| csv_message(&err, bytes))?
        .clone();
    let header_line = record_line(&headers, bytes);
    let label_index = column_index(&headers, "label", header_line)?;
    let sku_index = column_index(&headers, "sku", header_line)?;
    let mut entries = HashMap::new();
    for result in reader.records() {
        let record = result.map_err(|err| csv_message(&err, bytes))?;
        let line = record_line(&record, bytes);
        let label_cell = record.get(label_index).unwrap_or("");
        let sku_cell = record.get(sku_index).unwrap_or("");
        if label_cell.is_empty() {
            return Err(format!("line {line}: empty label"));
        }
        if sku_cell.is_empty() {
            return Err(format!("line {line}: empty sku"));
        }
        let price = table
            .get(sku_cell)
            .ok_or_else(|| format!("line {line}: unknown sku {sku_cell}"))?;
        let labels = parse_runner_labels(label_cell);
        if labels.is_empty() {
            return Err(format!("line {line}: empty label"));
        }
        entries.insert(label_key(&labels), price.sku.clone());
    }
    Ok(RunnerMap { entries })
}

fn column_index(headers: &csv::StringRecord, name: &str, line: u64) -> Result<usize, String> {
    headers
        .iter()
        .position(|header| header.eq_ignore_ascii_case(name))
        .ok_or_else(|| format!("line {line}: missing column {name}"))
}

fn record_line(record: &csv::StringRecord, bytes: &[u8]) -> u64 {
    record
        .position()
        .map(|position| physical_line(position.line(), position.byte(), bytes))
        .unwrap_or(1)
}

fn physical_line(line: u64, offset: u64, bytes: &[u8]) -> u64 {
    let Some(index) = usize::try_from(offset).ok() else {
        return line;
    };
    if index > 0 && bytes.get(index) == Some(&b'\n') && bytes.get(index - 1) == Some(&b'\r') {
        line.saturating_add(1)
    } else {
        line
    }
}

fn csv_message(err: &csv::Error, bytes: &[u8]) -> String {
    let line = err
        .position()
        .map(|position| physical_line(position.line(), position.byte(), bytes))
        .unwrap_or(1);
    format!("line {line}: invalid CSV")
}

fn parse_table_json(text: &str) -> Result<PriceTable, String> {
    let value: Value = serde_json::from_str(text).map_err(|_| "invalid JSON".to_string())?;
    let object = value.as_object().ok_or("expected an object")?;
    let retrieved = required_string(object, "retrieved")?;
    let effective_from = required_string(object, "effective_from")?;
    let sources_value = object.get("sources").ok_or("missing sources")?;
    let sources_array = sources_value.as_array().ok_or("sources must be an array")?;
    if sources_array.is_empty() {
        return Err("sources is empty".to_string());
    }
    let mut sources = Vec::with_capacity(sources_array.len());
    for source in sources_array {
        let text = source.as_str().ok_or("sources must be strings")?;
        if text.is_empty() {
            return Err("sources contains an empty URL".to_string());
        }
        sources.push(text.to_string());
    }
    let skus_value = object.get("skus").ok_or("missing skus")?;
    let skus_array = skus_value.as_array().ok_or("skus must be an array")?;
    if skus_array.is_empty() {
        return Err("skus is empty".to_string());
    }
    let mut skus = BTreeMap::new();
    let mut by_lower = HashMap::new();
    for entry in skus_array {
        let sku_object = entry.as_object().ok_or("sku entry must be an object")?;
        let sku = required_string(sku_object, "sku")?;
        if sku.is_empty() {
            return Err("sku name is empty".to_string());
        }
        let rate_text = sku_object
            .get("usd_per_minute")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("sku {sku}: usd_per_minute must be a decimal string"))?;
        let usd_per_minute = Micros::parse(rate_text).map_err(|err| match err {
            DecimalError::TooManyDecimals => {
                format!("sku {sku}: usd_per_minute has more than 6 decimal places")
            }
            DecimalError::NotANumber => format!("sku {sku}: usd_per_minute is not a number"),
        })?;
        if usd_per_minute.0 < 0 {
            return Err(format!("sku {sku}: usd_per_minute is negative"));
        }
        let billed_on_public = sku_object
            .get("billed_on_public")
            .and_then(Value::as_bool)
            .ok_or_else(|| format!("sku {sku}: billed_on_public must be a boolean"))?;
        let lower = sku.to_ascii_lowercase();
        if by_lower.contains_key(&lower) {
            return Err(format!("duplicate sku {sku}"));
        }
        by_lower.insert(lower, sku.clone());
        skus.insert(
            sku.clone(),
            SkuPrice {
                sku,
                usd_per_minute,
                billed_on_public,
            },
        );
    }
    Ok(PriceTable {
        retrieved,
        effective_from,
        sources,
        skus,
        by_lower,
    })
}

fn required_string(object: &Map<String, Value>, key: &str) -> Result<String, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("missing {key}"))
}

fn render_table_text(table: &PriceTable) -> String {
    let width = table
        .iter()
        .map(|sku| sku.sku.len())
        .max()
        .unwrap_or(3)
        .max(3);
    let mut out = String::new();
    out.push_str("Actions runner prices\n");
    out.push_str(&format!("Retrieved: {}\n", table.retrieved));
    out.push_str(&format!("Effective from: {}\n", table.effective_from));
    out.push_str("Currency: USD per minute\n");
    out.push_str("Sources:\n");
    for source in &table.sources {
        out.push_str(&format!("  {source}\n"));
    }
    out.push('\n');
    out.push_str(&format!(
        "{:width$}  {:>10}  Billed on public\n",
        "SKU",
        "USD/min",
        width = width
    ));
    for sku in table.iter() {
        let billed = if sku.billed_on_public { "yes" } else { "no" };
        out.push_str(&format!(
            "{:width$}  {:>10}  {billed}\n",
            sku.sku,
            sku.usd_per_minute.to_decimal_string(),
            width = width
        ));
    }
    out
}

fn render_table_json(table: &PriceTable) -> String {
    let skus: Vec<Value> = table
        .iter()
        .map(|sku| {
            serde_json::json!({
                "sku": sku.sku,
                "usd_per_minute": sku.usd_per_minute.to_decimal_string(),
                "billed_on_public": sku.billed_on_public,
            })
        })
        .collect();
    let value = serde_json::json!({
        "schema_version": 1,
        "retrieved": table.retrieved,
        "effective_from": table.effective_from,
        "currency": "USD",
        "unit": "minute",
        "sources": table.sources,
        "skus": skus,
    });
    pretty_json(&value)
}

fn render_quote(json: bool, raw_label: &str, quote: &Quote) -> String {
    if json {
        let mut object = Map::new();
        object.insert(
            "label".to_string(),
            Value::String(raw_label.trim().to_string()),
        );
        object.insert(
            "labels".to_string(),
            Value::Array(quote.labels.iter().cloned().map(Value::String).collect()),
        );
        object.insert("sku".to_string(), Value::String(quote.sku.clone()));
        object.insert(
            "usd_per_minute".to_string(),
            Value::String(quote.usd_per_minute.to_decimal_string()),
        );
        object.insert(
            "billed_on_public".to_string(),
            Value::Bool(quote.billed_on_public),
        );
        object.insert(
            "source".to_string(),
            Value::String(quote.source.as_str().to_string()),
        );
        if let Some(repository) = &quote.repository {
            object.insert("repository".to_string(), Value::String(repository.clone()));
        }
        if let Some(workflow) = &quote.workflow_path {
            object.insert("workflow_path".to_string(), Value::String(workflow.clone()));
        }
        pretty_json(&Value::Object(object))
    } else {
        let mut out = format!(
            "label: {}\nlabels: {}\nsku: {}\nusd_per_minute: {}\nbilled_on_public: {}\nsource: {}\n",
            raw_label.trim(),
            quote.labels.join(", "),
            quote.sku,
            quote.usd_per_minute.to_decimal_string(),
            if quote.billed_on_public { "yes" } else { "no" },
            quote.source.as_str()
        );
        if let Some(repository) = &quote.repository {
            out.push_str(&format!("repository: {repository}\n"));
        }
        if let Some(workflow) = &quote.workflow_path {
            out.push_str(&format!("workflow_path: {workflow}\n"));
        }
        out
    }
}

fn pretty_json(value: &Value) -> String {
    match serde_json::to_string_pretty(value) {
        Ok(text) => format!("{text}\n"),
        Err(_) => "{}\n".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        embedded_table, parse_runner_labels, parse_runner_map, parse_table_json, quote_sku,
        resolve_runner, PriceFail, PriceTable, RateSource, RunnerMap,
    };
    use crate::money::Micros;
    use crate::usage::UsageRow;

    fn table() -> &'static PriceTable {
        embedded_table().unwrap_or_else(|err| panic!("embedded table: {err:?}"))
    }

    fn sku_of(raw: &str) -> String {
        let labels = parse_runner_labels(raw);
        resolve_runner(&labels, &RunnerMap::default(), table())
            .unwrap_or_else(|label| panic!("label {raw} should resolve, unknown {label}"))
    }

    #[test]
    fn self_hosted_beats_an_os_label_and_comes_first() {
        assert_eq!(sku_of("self-hosted"), "self_hosted");
        assert_eq!(sku_of("[self-hosted, macos]"), "self_hosted");
        assert_eq!(sku_of("[macos, self-hosted]"), "self_hosted");
        assert_eq!(sku_of("self-hosted, macos"), "self_hosted");
        assert_eq!(sku_of("[\"self-hosted\", \"macos\"]"), "self_hosted");
        assert_eq!(sku_of("[SELF-HOSTED, MACOS]"), "self_hosted");
        assert!(resolve_runner(
            &parse_runner_labels("macos"),
            &RunnerMap::default(),
            table()
        )
        .is_err());
    }

    #[test]
    fn specific_shapes_win_over_the_os_prefix() {
        assert_eq!(sku_of("ubuntu-slim"), "actions_linux_slim");
        assert_eq!(sku_of("UBUNTU-SLIM"), "actions_linux_slim");
        assert_eq!(sku_of("ubuntu-latest"), "actions_linux");
        assert_eq!(sku_of("ubuntu-22.04-arm"), "actions_linux_arm");
        assert_eq!(sku_of("ubuntu-24.04-arm"), "actions_linux_arm");
        assert_eq!(sku_of("ubuntu-26.04-arm"), "actions_linux_arm");
        assert_eq!(sku_of("windows-11-arm"), "actions_windows_arm");
        assert_eq!(sku_of("macos-14-xlarge"), "actions_macos_xl");
        assert_eq!(sku_of("macos-14-large"), "actions_macos_l");
        assert_eq!(sku_of("xcode-27-xlarge"), "actions_macos_xl");
        assert_eq!(sku_of("xcode-27"), "actions_macos");
        assert_eq!(sku_of("ubuntu-24.04-16core"), "actions_linux_16_core");
        assert_eq!(sku_of("UBUNTU-24.04-16CORE"), "actions_linux_16_core");
        assert_eq!(sku_of("ubuntu-24.04-4core-arm"), "actions_linux_4_core_arm");
        assert_eq!(
            sku_of("ubuntu-24.04-2core-advanced"),
            "actions_linux_2_core_advanced"
        );
        assert_eq!(
            sku_of("windows-2022-4core-gpu"),
            "actions_windows_4_core_gpu"
        );
        let unknown = resolve_runner(
            &parse_runner_labels("ubuntu-24.04-3core"),
            &RunnerMap::default(),
            table(),
        )
        .unwrap_err();
        assert_eq!(unknown, "ubuntu-24.04-3core");
    }

    #[test]
    fn runner_map_overrides_a_built_in_label_and_keeps_self_hosted_first() {
        let bytes = b"sku,label\r\nactions_macos,ubuntu-latest\r\nactions_linux,macos\r\n";
        let map = parse_runner_map(bytes, table()).unwrap();
        let overridden =
            resolve_runner(&parse_runner_labels("UBUNTU-LATEST"), &map, table()).unwrap();
        assert_eq!(overridden, "actions_macos");
        let hosted =
            resolve_runner(&parse_runner_labels("[self-hosted, macos]"), &map, table()).unwrap();
        assert_eq!(hosted, "self_hosted");
    }

    #[test]
    fn csv_quantity_rate_replaces_the_table_rate() {
        let price = table().get("actions_linux").unwrap();
        let quantity = Micros(10 * Micros::SCALE);
        let net = Micros(price.usd_per_minute.0 * 20);
        let row = UsageRow {
            line: 2,
            date: "2026-09-01".to_string(),
            product: "Actions".to_string(),
            sku: "ACTIONS_LINUX".to_string(),
            quantity,
            unit_type: "Minutes".to_string(),
            applied_cost_per_quantity: price.usd_per_minute,
            gross: net,
            discount: Micros::ZERO,
            net,
            username: String::new(),
            organization: "Example-Org".to_string(),
            repository: "Example-Org/Sample-App".to_string(),
            workflow_path: ".GitHub/Workflows/CI.yml".to_string(),
        };
        let other = UsageRow {
            repository: "other/app".to_string(),
            workflow_path: ".github/workflows/ci.yml".to_string(),
            sku: "actions_linux".to_string(),
            product: "actions".to_string(),
            net: price.usd_per_minute,
            quantity: Micros(Micros::SCALE),
            ..row.clone()
        };
        let quote = quote_sku(
            table(),
            &[row, other],
            Some("example-org/sample-app"),
            Some(".github/workflows/ci.yml"),
            "actions_linux",
            vec!["ubuntu-latest".to_string()],
        )
        .unwrap();
        assert_eq!(quote.source, RateSource::Csv);
        assert_eq!(quote.usd_per_minute, Micros(price.usd_per_minute.0 * 2));
        assert_ne!(quote.usd_per_minute, price.usd_per_minute);
        assert_eq!(quote.repository.as_deref(), Some("Example-Org/Sample-App"));

        let untouched = quote_sku(
            table(),
            &[],
            Some("example-org/sample-app"),
            Some(".github/workflows/ci.yml"),
            "actions_linux",
            Vec::new(),
        )
        .unwrap();
        assert_eq!(untouched.source, RateSource::Table);
        assert_eq!(untouched.usd_per_minute, price.usd_per_minute);
    }

    #[test]
    fn zero_quantity_keeps_the_table_and_two_repos_are_an_error() {
        let price = table().get("actions_macos").unwrap();
        let base = UsageRow {
            line: 2,
            date: "2026-09-01".to_string(),
            product: "actions".to_string(),
            sku: "actions_macos".to_string(),
            quantity: Micros::ZERO,
            unit_type: "minutes".to_string(),
            applied_cost_per_quantity: price.usd_per_minute,
            gross: Micros::ZERO,
            discount: Micros::ZERO,
            net: Micros::ZERO,
            username: String::new(),
            organization: "org".to_string(),
            repository: "org/one".to_string(),
            workflow_path: ".github/workflows/lint.yml".to_string(),
        };
        let quote = quote_sku(
            table(),
            std::slice::from_ref(&base),
            None,
            Some(".github/workflows/lint.yml"),
            "actions_macos",
            Vec::new(),
        )
        .unwrap();
        assert_eq!(quote.source, RateSource::Table);

        let mut second = base;
        second.repository = "org/two".to_string();
        second.quantity = Micros(Micros::SCALE);
        second.net = price.usd_per_minute;
        let mut first = second.clone();
        first.repository = "org/one".to_string();
        first.net = Micros(price.usd_per_minute.0 * 3);
        let err = quote_sku(
            table(),
            &[first, second],
            None,
            Some(".github/workflows/lint.yml"),
            "actions_macos",
            Vec::new(),
        )
        .unwrap_err();
        match err {
            PriceFail::Input { message, .. } => {
                assert!(message.contains("org/one"), "{message}");
                assert!(message.contains("org/two"), "{message}");
                assert!(message.contains(".github/workflows/lint.yml"), "{message}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn runner_map_rejects_empty_fields_missing_columns_and_unknown_skus() {
        let err = parse_runner_map(b"label,sku\r\n,actions_linux\r\n", table()).unwrap_err();
        assert!(err.contains("line 2"), "{err}");
        assert!(err.contains("empty label"), "{err}");

        let err = parse_runner_map(b"sku,label\n,my-box\n", table()).unwrap_err();
        assert!(err.contains("line 2"), "{err}");
        assert!(err.contains("empty sku"), "{err}");
        assert!(!err.contains("my-box"), "{err}");

        let err = parse_runner_map(b"label,sku\nmy-box,\n", table()).unwrap_err();
        assert!(err.contains("empty sku"), "{err}");
        assert!(!err.contains("my-box"), "{err}");

        let err = parse_runner_map(b"label,name\nmy-box,actions_linux\n", table()).unwrap_err();
        assert!(err.contains("missing column sku"), "{err}");

        let err = parse_runner_map(b"label,sku\nmy-box,actions_not_real\n", table()).unwrap_err();
        assert!(err.contains("line 2"), "{err}");
        assert!(err.contains("actions_not_real"), "{err}");

        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend(b"LABEL,SKU\r\nUBUNTU-LATEST,ACTIONS_MACOS\r\n");
        let map = parse_runner_map(&bytes, table()).unwrap();
        assert_eq!(map.get("ubuntu-latest"), Some("actions_macos"));
    }

    #[test]
    fn table_json_rejects_a_seventh_decimal_and_a_duplicate() {
        let err = parse_table_json(
            r#"{"retrieved":"2026-10-17","effective_from":"2026-01-01","sources":["https://example.test"],"skus":[{"sku":"actions_linux","usd_per_minute":"0.0000001","billed_on_public":false}]}"#,
        )
        .unwrap_err();
        assert!(err.contains("more than 6 decimal places"), "{err}");

        let err = parse_table_json(
            r#"{"retrieved":"2026-10-17","effective_from":"2026-01-01","sources":["https://example.test"],"skus":[{"sku":"actions_linux","usd_per_minute":"0","billed_on_public":false},{"sku":"Actions_Linux","usd_per_minute":"0","billed_on_public":true}]}"#,
        )
        .unwrap_err();
        assert!(err.contains("duplicate sku"), "{err}");
    }
}
