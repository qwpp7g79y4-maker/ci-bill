//! Fit job costs to the usage-report net.
//!
//! For one repository, workflow, and SKU, billable job costs are scaled so
//! they add up to that SKU's CSV net. The weight is billed minutes. Any
//! leftover micros go to the job with the most minutes.
//! A workflow with no saved jobs uses its YAML jobs instead. The weight is
//! the matrix cell count times the runner's minute rate (`yaml_weight`).
//! A job with no CSV row keeps its list price (`list_price`).
//! On a public repository that invoice is zero for a standard SKU, while the
//! list price stays on the job. A larger runner stays on its list price.
//! A CSV net replaces either amount.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::api::{repository_from_run_path, workflow_paths_match, ApiData, Job as ApiJob};
use crate::job_cost::bill_job;
use crate::money::Micros;
use crate::prices::{billed_on_public, embedded_table, sku_for_labels};
use crate::rank::WorkflowTotal;
use crate::workflow::{ReusableSource, Workflow};

/// How an amount was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostMethod {
    /// Share of the CSV net, weighted by billed minutes.
    BilledMinutes,
    /// Share of the CSV net, weighted by matrix cells times the minute rate.
    YamlWeight,
    /// Minutes times the runner table rate.
    ListPrice,
}

impl CostMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BilledMinutes => "billed_minutes",
            Self::YamlWeight => "yaml_weight",
            Self::ListPrice => "list_price",
        }
    }
}

/// One USD amount and the rule that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Amount {
    pub micros: Micros,
    pub method: CostMethod,
}

/// One job's reconciled cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobCost {
    pub name: String,
    pub file: String,
    pub line: Option<usize>,
    pub job_id: Option<u64>,
    pub sku: Option<String>,
    pub amount: Amount,
}

/// Measured list price, invoice net, and their difference for one workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowCost {
    pub measured: Amount,
    pub invoice: Amount,
    pub difference: Amount,
    pub jobs: Vec<JobCost>,
}

/// Reconciled costs aligned with the ranked workflow slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciliation {
    pub workflows: Vec<WorkflowCost>,
    jobs: HashMap<u64, Amount>,
}

impl Reconciliation {
    pub fn job(&self, id: u64) -> Option<&Amount> {
        self.jobs.get(&id)
    }
}

/// Scale saved jobs onto the CSV, or spread the CSV across YAML jobs.
pub fn reconcile(
    workflows: &[WorkflowTotal],
    api: Option<&ApiData>,
    scanned: &[Workflow],
) -> Reconciliation {
    let linked = link_jobs(workflows, api);
    let public_repo = api.is_some_and(|data| data.public);
    let mut jobs = HashMap::new();
    let mut workflow_costs = Vec::with_capacity(workflows.len());
    for (index, workflow) in workflows.iter().enumerate() {
        workflow_costs.push(reconcile_workflow(
            index,
            workflow,
            &linked,
            scanned,
            api.is_some(),
            public_repo,
            &mut jobs,
        ));
    }
    for job in &linked {
        jobs.entry(job.id).or_insert_with(|| Amount {
            micros: list_price_invoice(public_repo, job.sku.as_deref(), job.list_price),
            method: CostMethod::ListPrice,
        });
    }
    Reconciliation {
        workflows: workflow_costs,
        jobs,
    }
}

/// Text lines for the workflow table: totals, then one line per job.
pub fn text_lines(cost: &WorkflowCost) -> Vec<String> {
    let mut lines = vec![format!(
        "  measured {}  invoice {}  difference {}",
        cost.measured.micros.format_cents(),
        cost.invoice.micros.format_cents(),
        cost.difference.micros.format_cents(),
    )];
    for job in &cost.jobs {
        lines.push(job_text(job));
    }
    lines
}

fn job_text(job: &JobCost) -> String {
    let amount = job.amount.micros.format_cents();
    let name = if job.name.is_empty() {
        "job"
    } else {
        job.name.as_str()
    };
    if !job.file.is_empty() {
        if let Some(line) = job.line {
            return format!("  {name}  {}:{} = {amount}", job.file, line);
        }
    }
    if let Some(id) = job.job_id {
        return format!("  {name}  job {id} = {amount}");
    }
    format!("  {name} = {amount}")
}

struct LinkedJob {
    id: u64,
    name: String,
    sku: Option<String>,
    minutes: u64,
    list_price: Micros,
    workflow: Option<usize>,
    billable: bool,
}

fn link_jobs(workflows: &[WorkflowTotal], api: Option<&ApiData>) -> Vec<LinkedJob> {
    let Some(api) = api else {
        return Vec::new();
    };
    api.jobs
        .iter()
        .map(|job| link_job(workflows, api, job))
        .collect()
}

fn link_job(workflows: &[WorkflowTotal], api: &ApiData, job: &ApiJob) -> LinkedJob {
    let bill = bill_job(api, job);
    let path = api.workflow_path(job.run_id);
    let repo = api
        .runs
        .iter()
        .find(|run| run.id == job.run_id)
        .and_then(repository_of_run);
    let billable = job.counts() && !api.is_inherited_copy(job);
    LinkedJob {
        id: job.id,
        name: job.name.clone(),
        sku: bill.sku,
        minutes: bill.minutes,
        list_price: bill.list_price,
        workflow: workflow_index(workflows, &path, repo.as_deref()),
        billable,
    }
}

fn repository_of_run(run: &crate::api::WorkflowRun) -> Option<String> {
    run.repository
        .clone()
        .or_else(|| repository_from_run_path(&run.path))
}

fn workflow_index(workflows: &[WorkflowTotal], path: &str, repo: Option<&str>) -> Option<usize> {
    let mut hits = Vec::new();
    for (index, workflow) in workflows.iter().enumerate() {
        if !workflow_paths_match(&workflow.workflow_path, path) {
            continue;
        }
        if let Some(repo) = repo {
            if workflow.repository.eq_ignore_ascii_case(repo) {
                hits.push(index);
            }
        } else {
            hits.push(index);
        }
    }
    if hits.len() == 1 {
        Some(hits[0])
    } else {
        None
    }
}

/// Invoice for a job that has no CSV row.
///
/// A public repository invoices a standard SKU at zero. The list price is
/// unchanged. A larger runner (`billed_on_public`) keeps the list price.
pub(crate) fn list_price_invoice(
    public_repo: bool,
    sku: Option<&str>,
    list_price: Micros,
) -> Micros {
    if public_repo && !sku.is_some_and(billed_on_public) {
        Micros::ZERO
    } else {
        list_price
    }
}

fn reconcile_workflow(
    index: usize,
    workflow: &WorkflowTotal,
    linked: &[LinkedJob],
    scanned: &[Workflow],
    had_api: bool,
    public_repo: bool,
    amounts: &mut HashMap<u64, Amount>,
) -> WorkflowCost {
    let slots = yaml_slots(scanned, &workflow.workflow_path);
    let mut used_jobs = HashSet::new();
    let mut used_slots = HashSet::new();
    let mut costs = Vec::new();

    for sku in &workflow.skus {
        let api_indexes = linked
            .iter()
            .enumerate()
            .filter(|(_, job)| {
                job.workflow == Some(index)
                    && job.billable
                    && !used_jobs.contains(&job.id)
                    && job
                        .sku
                        .as_ref()
                        .is_some_and(|name| name.eq_ignore_ascii_case(&sku.sku))
            })
            .map(|(slot, _)| slot)
            .collect::<Vec<_>>();
        if !api_indexes.is_empty() {
            assign_api(
                sku.net,
                linked,
                &api_indexes,
                &mut used_jobs,
                &mut costs,
                amounts,
            );
            continue;
        }
        let slot_indexes = slots
            .iter()
            .enumerate()
            .filter(|(slot_index, slot)| {
                !used_slots.contains(slot_index) && slot.sku.eq_ignore_ascii_case(&sku.sku)
            })
            .map(|(slot_index, _)| slot_index)
            .collect::<Vec<_>>();
        if slot_indexes.is_empty() {
            continue;
        }
        assign_yaml(sku.net, &slots, &slot_indexes, &mut used_slots, &mut costs);
    }

    for job in linked.iter().filter(|job| job.workflow == Some(index)) {
        if used_jobs.contains(&job.id) {
            continue;
        }
        let amount = Amount {
            micros: list_price_invoice(public_repo, job.sku.as_deref(), job.list_price),
            method: CostMethod::ListPrice,
        };
        amounts.insert(job.id, amount.clone());
        if amount.micros != Micros::ZERO {
            costs.push(api_cost(job, amount));
        }
    }
    for (slot_index, slot) in slots.iter().enumerate() {
        if used_slots.contains(&slot_index) {
            continue;
        }
        costs.push(yaml_list_cost(slot));
    }

    costs.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then(left.line.cmp(&right.line))
            .then(left.name.cmp(&right.name))
            .then(left.sku.cmp(&right.sku))
            .then(left.job_id.cmp(&right.job_id))
    });

    let measured = linked
        .iter()
        .filter(|job| job.workflow == Some(index))
        .fold(Micros::ZERO, |total, job| add_micros(total, job.list_price));
    let invoice = workflow.net;
    let invoice_method = invoice_method(&costs, had_api, !slots.is_empty());
    WorkflowCost {
        measured: Amount {
            micros: measured,
            method: CostMethod::ListPrice,
        },
        invoice: Amount {
            micros: invoice,
            method: invoice_method,
        },
        difference: Amount {
            micros: sub_micros(invoice, measured),
            method: invoice_method,
        },
        jobs: costs,
    }
}

fn invoice_method(jobs: &[JobCost], had_api: bool, had_yaml: bool) -> CostMethod {
    if jobs
        .iter()
        .any(|job| job.amount.method == CostMethod::BilledMinutes)
    {
        CostMethod::BilledMinutes
    } else if jobs
        .iter()
        .any(|job| job.amount.method == CostMethod::YamlWeight)
    {
        CostMethod::YamlWeight
    } else if had_api {
        CostMethod::BilledMinutes
    } else if had_yaml {
        CostMethod::YamlWeight
    } else {
        CostMethod::ListPrice
    }
}

fn assign_api(
    net: Micros,
    linked: &[LinkedJob],
    indexes: &[usize],
    used: &mut HashSet<u64>,
    costs: &mut Vec<JobCost>,
    amounts: &mut HashMap<u64, Amount>,
) {
    let parts = indexes
        .iter()
        .map(|index| {
            let job = &linked[*index];
            Part {
                weight: i128::from(job.minutes),
                rank: i128::from(job.list_price.0),
                tie: i64::try_from(job.id).unwrap_or(i64::MAX),
            }
        })
        .collect::<Vec<_>>();
    let shares = allocate(net.0, &parts);
    for (index, share) in indexes.iter().zip(shares) {
        let job = &linked[*index];
        used.insert(job.id);
        let amount = Amount {
            micros: Micros(share),
            method: CostMethod::BilledMinutes,
        };
        amounts.insert(job.id, amount.clone());
        costs.push(api_cost(job, amount));
    }
}

fn assign_yaml(
    net: Micros,
    slots: &[YamlSlot],
    indexes: &[usize],
    used: &mut HashSet<usize>,
    costs: &mut Vec<JobCost>,
) {
    let parts = indexes
        .iter()
        .map(|index| {
            let slot = &slots[*index];
            Part {
                weight: slot.weight,
                rank: slot.weight,
                tie: i64::try_from(slot.line).unwrap_or(i64::MAX),
            }
        })
        .collect::<Vec<_>>();
    let shares = allocate(net.0, &parts);
    for (index, share) in indexes.iter().zip(shares) {
        let slot = &slots[*index];
        used.insert(*index);
        costs.push(JobCost {
            name: slot.name.clone(),
            file: slot.file.clone(),
            line: Some(slot.line),
            job_id: None,
            sku: Some(slot.sku.clone()),
            amount: Amount {
                micros: Micros(share),
                method: CostMethod::YamlWeight,
            },
        });
    }
}

fn api_cost(job: &LinkedJob, amount: Amount) -> JobCost {
    JobCost {
        name: job.name.clone(),
        file: String::new(),
        line: None,
        job_id: Some(job.id),
        sku: job.sku.clone(),
        amount,
    }
}

fn yaml_list_cost(slot: &YamlSlot) -> JobCost {
    JobCost {
        name: slot.name.clone(),
        file: slot.file.clone(),
        line: Some(slot.line),
        job_id: None,
        sku: Some(slot.sku.clone()),
        amount: Amount {
            micros: Micros::ZERO,
            method: CostMethod::ListPrice,
        },
    }
}

struct Part {
    weight: i128,
    rank: i128,
    tie: i64,
}

/// Split `total` by weight. The remainder micros go to the largest part.
fn allocate(total: i64, parts: &[Part]) -> Vec<i64> {
    let mut shares = vec![0; parts.len()];
    if parts.is_empty() {
        return shares;
    }
    let weight_sum: i128 = parts.iter().map(|part| part.weight.max(0)).sum();
    if weight_sum == 0 {
        shares[largest(parts)] = total;
        return shares;
    }
    let mut assigned = 0i128;
    for (index, part) in parts.iter().enumerate() {
        let weight = part.weight.max(0);
        let share = i128::from(total).saturating_mul(weight) / weight_sum;
        shares[index] = clamp_i64(share);
        assigned += share;
    }
    let remainder = i128::from(total) - assigned;
    let index = largest(parts);
    shares[index] = clamp_i64(i128::from(shares[index]) + remainder);
    shares
}

fn largest(parts: &[Part]) -> usize {
    parts
        .iter()
        .enumerate()
        .max_by(|(_, left), (_, right)| {
            left.weight
                .cmp(&right.weight)
                .then(left.rank.cmp(&right.rank))
                .then(right.tie.cmp(&left.tie))
        })
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn clamp_i64(value: i128) -> i64 {
    i64::try_from(value).unwrap_or(if value > 0 { i64::MAX } else { i64::MIN })
}

fn add_micros(left: Micros, right: Micros) -> Micros {
    Micros(left.0.saturating_add(right.0))
}

fn sub_micros(left: Micros, right: Micros) -> Micros {
    match left.0.checked_sub(right.0) {
        Some(value) => Micros(value),
        None if left.0 >= right.0 => Micros(i64::MAX),
        None => Micros(i64::MIN),
    }
}

struct YamlSlot {
    name: String,
    file: String,
    line: usize,
    sku: String,
    weight: i128,
}

fn yaml_slots(scanned: &[Workflow], workflow_path: &str) -> Vec<YamlSlot> {
    let Some(workflow) = scanned.iter().find(|workflow| {
        workflow
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| same_workflow_file(workflow_path, name))
    }) else {
        return Vec::new();
    };
    let mut slots = Vec::new();
    let mut stack = Vec::new();
    collect_slots(workflow, scanned, 1, &mut stack, &mut slots);
    slots
}

fn collect_slots(
    workflow: &Workflow,
    scanned: &[Workflow],
    multiplier: u64,
    stack: &mut Vec<std::path::PathBuf>,
    slots: &mut Vec<YamlSlot>,
) {
    if stack.iter().any(|path| path == &workflow.path) {
        return;
    }
    stack.push(workflow.path.clone());
    for job in &workflow.jobs {
        if let Some(call) = &job.uses {
            if matches!(call.source, ReusableSource::Local) {
                if let Some(callee) = find_callee(scanned, &call.file_name) {
                    let next = multiplier.saturating_mul(cell_count(job)).max(1);
                    collect_slots(callee, scanned, next, stack, slots);
                }
            }
            continue;
        }
        slots.extend(slots_for_job(job, &workflow.path, multiplier));
    }
    stack.pop();
}

fn find_callee<'a>(scanned: &'a [Workflow], file_name: &str) -> Option<&'a Workflow> {
    scanned.iter().find(|workflow| {
        workflow
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(file_name))
    })
}

fn slots_for_job(job: &crate::workflow::Job, path: &Path, multiplier: u64) -> Vec<YamlSlot> {
    let Some(runs_on) = &job.runs_on else {
        return Vec::new();
    };
    let file = path.display().to_string();
    let cells = cell_count(job).saturating_mul(multiplier).max(1);
    let mut names = axis_names(&runs_on.value);
    for label in &runs_on.labels {
        for name in axis_names(label) {
            push_unique(&mut names, name);
        }
    }
    if names.is_empty() {
        let labels = concrete_labels(&runs_on.labels);
        let Some(sku) = sku_for_labels(&labels) else {
            return Vec::new();
        };
        return vec![yaml_slot(job, &file, runs_on.line, &sku, cells)];
    }
    let Some(matrix) = &job.matrix else {
        return Vec::new();
    };
    for name in &names {
        let Some(axis) = matrix
            .axes
            .iter()
            .find(|axis| axis.name.eq_ignore_ascii_case(name))
        else {
            continue;
        };
        let counts = sku_value_counts(axis);
        if counts.is_empty() {
            continue;
        }
        return split_cells(cells, &counts)
            .into_iter()
            .filter(|(_, count)| *count > 0)
            .map(|(sku, count)| yaml_slot(job, &file, runs_on.line, &sku, count))
            .collect();
    }
    Vec::new()
}

fn yaml_slot(
    job: &crate::workflow::Job,
    file: &str,
    line: usize,
    sku: &str,
    cells: u64,
) -> YamlSlot {
    let rate = table_rate(sku);
    YamlSlot {
        name: job.name.clone(),
        file: file.to_string(),
        line,
        sku: sku.to_string(),
        weight: i128::from(cells).saturating_mul(i128::from(rate.0)).max(0),
    }
}

fn cell_count(job: &crate::workflow::Job) -> u64 {
    let Some(matrix) = &job.matrix else {
        return 1;
    };
    let upper = matrix.jobs_hi.unwrap_or(matrix.jobs);
    matrix.jobs.max(upper).max(1)
}

fn concrete_labels(labels: &[String]) -> Vec<String> {
    labels
        .iter()
        .filter(|label| !label.contains("${{"))
        .cloned()
        .collect()
}

struct SkuCount {
    sku: String,
    count: u64,
    rate: Micros,
}

fn sku_value_counts(axis: &crate::workflow::MatrixAxis) -> Vec<SkuCount> {
    let mut seen = HashSet::new();
    let mut counts: Vec<SkuCount> = Vec::new();
    for labels in &axis.values {
        let key = labels
            .iter()
            .map(|label| label.to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join("\u{0}");
        if !seen.insert(key) {
            continue;
        }
        let Some(sku) = sku_for_labels(labels) else {
            continue;
        };
        if let Some(entry) = counts
            .iter_mut()
            .find(|entry| entry.sku.eq_ignore_ascii_case(&sku))
        {
            entry.count += 1;
        } else {
            let rate = table_rate(&sku);
            counts.push(SkuCount {
                sku,
                count: 1,
                rate,
            });
        }
    }
    counts
}

fn split_cells(total: u64, counts: &[SkuCount]) -> Vec<(String, u64)> {
    let value_sum: u64 = counts.iter().map(|entry| entry.count).sum();
    if value_sum == 0 {
        return Vec::new();
    }
    let mut shares = Vec::with_capacity(counts.len());
    let mut assigned = 0u64;
    for entry in counts {
        let share =
            u64::try_from(u128::from(total) * u128::from(entry.count) / u128::from(value_sum))
                .unwrap_or(u64::MAX);
        assigned = assigned.saturating_add(share);
        shares.push((entry.sku.clone(), share));
    }
    let remainder = total.saturating_sub(assigned);
    if remainder > 0 {
        let index = counts
            .iter()
            .enumerate()
            .max_by(|(_, left), (_, right)| {
                left.count
                    .cmp(&right.count)
                    .then(left.rate.cmp(&right.rate))
                    .then(right.sku.cmp(&left.sku))
            })
            .map(|(index, _)| index)
            .unwrap_or(0);
        shares[index].1 = shares[index].1.saturating_add(remainder);
    }
    shares
}

fn table_rate(sku: &str) -> Micros {
    embedded_table()
        .ok()
        .and_then(|table| table.get(sku).map(|price| price.usd_per_minute))
        .unwrap_or(Micros::ZERO)
}

fn same_workflow_file(workflow_path: &str, file_name: &str) -> bool {
    let key = format!(".github/workflows/{file_name}");
    let normalized = workflow_path.replace('\\', "/");
    if normalized.eq_ignore_ascii_case(&key) {
        return true;
    }
    normalized.char_indices().any(|(index, separator)| {
        separator == '/' && normalized[index + 1..].eq_ignore_ascii_case(&key)
    })
}

fn axis_names(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut names = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if !starts_with_matrix(&chars, index) {
            index += 1;
            continue;
        }
        let boundary = index == 0 || !is_name_char(chars[index - 1]);
        if !boundary {
            index += 1;
            continue;
        }
        let after = index + MATRIX.len();
        if let Some(end) = dotted_axis_end(&chars, after) {
            push_unique(&mut names, chars[after + 1..end].iter().collect());
            index = end;
            continue;
        }
        if let Some((name, end)) = bracket_axis(&chars, after) {
            push_unique(&mut names, name);
            index = end;
            continue;
        }
        index += 1;
    }
    names
}

const MATRIX: &[char] = &['m', 'a', 't', 'r', 'i', 'x'];

fn starts_with_matrix(chars: &[char], index: usize) -> bool {
    if index + MATRIX.len() > chars.len() {
        return false;
    }
    chars[index..index + MATRIX.len()]
        .iter()
        .zip(MATRIX)
        .all(|(char, expected)| char.to_ascii_lowercase() == *expected)
}

fn is_name_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

fn dotted_axis_end(chars: &[char], after_matrix: usize) -> Option<usize> {
    if after_matrix >= chars.len() || chars[after_matrix] != '.' {
        return None;
    }
    let start = after_matrix + 1;
    if start >= chars.len() || !(chars[start].is_ascii_alphabetic() || chars[start] == '_') {
        return None;
    }
    let mut end = start + 1;
    while end < chars.len() && (is_name_char(chars[end]) || chars[end] == '-') {
        end += 1;
    }
    while end > start && chars[end - 1] == '-' {
        end -= 1;
    }
    if end == start {
        None
    } else {
        Some(end)
    }
}

fn bracket_axis(chars: &[char], after_matrix: usize) -> Option<(String, usize)> {
    let mut index = skip_ws(chars, after_matrix);
    if index >= chars.len() || chars[index] != '[' {
        return None;
    }
    index = skip_ws(chars, index + 1);
    if index >= chars.len() {
        return None;
    }
    let quote = chars[index];
    if quote != '\'' && quote != '"' {
        return None;
    }
    index += 1;
    let start = index;
    while index < chars.len() && chars[index] != quote {
        index += 1;
    }
    if index >= chars.len() {
        return None;
    }
    let name: String = chars[start..index].iter().collect();
    index = skip_ws(chars, index + 1);
    if name.is_empty() || index >= chars.len() || chars[index] != ']' {
        return None;
    }
    Some((name, index + 1))
}

fn skip_ws(chars: &[char], mut index: usize) -> usize {
    while index < chars.len() && chars[index].is_whitespace() {
        index += 1;
    }
    index
}

fn push_unique(names: &mut Vec<String>, name: String) {
    if !names.iter().any(|item| item.eq_ignore_ascii_case(&name)) {
        names.push(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rank::SkuTotal;
    use crate::workflow::scan_yaml;
    use std::path::Path;
    fn usd(text: &str) -> Micros {
        Micros::parse(text).unwrap()
    }

    fn part(weight: i128, rank: i128, tie: i64) -> Part {
        Part { weight, rank, tie }
    }

    fn workflow(path: &str, repo: &str, net: &str, sku: &str) -> WorkflowTotal {
        let net = usd(net);
        WorkflowTotal {
            repository: repo.to_string(),
            workflow_path: path.to_string(),
            net,
            gross: net,
            discount: Micros::ZERO,
            minutes: usd("1"),
            skus: vec![SkuTotal {
                sku: sku.to_string(),
                net,
                gross: net,
                discount: Micros::ZERO,
                minutes: usd("1"),
            }],
        }
    }

    #[test]
    fn a_public_repository_invoices_standard_skus_at_zero() {
        let macos = usd("0.062");
        assert_eq!(
            list_price_invoice(true, Some("actions_macos"), macos),
            Micros::ZERO
        );
        assert_eq!(
            list_price_invoice(true, Some("ACTIONS_LINUX"), usd("0.006")),
            Micros::ZERO
        );
        assert_eq!(
            list_price_invoice(true, Some("actions_windows"), usd("0.010")),
            Micros::ZERO
        );
        assert_eq!(
            list_price_invoice(true, Some("actions_linux_arm"), usd("0.005")),
            Micros::ZERO
        );
        assert_eq!(
            list_price_invoice(true, Some("actions_windows_arm"), usd("0.010")),
            Micros::ZERO
        );
        assert_eq!(
            list_price_invoice(true, Some("actions_linux_slim"), usd("0.002")),
            Micros::ZERO
        );
        assert_eq!(list_price_invoice(true, None, usd("1")), Micros::ZERO);
        assert_eq!(
            list_price_invoice(true, Some("actions_linux_4_core"), usd("0.012")),
            usd("0.012")
        );
        assert_eq!(
            list_price_invoice(true, Some("Actions_Macos_L"), usd("0.077")),
            usd("0.077")
        );
        assert_eq!(
            list_price_invoice(true, Some("actions_macos_xl"), usd("0.102")),
            usd("0.102")
        );
        assert_eq!(
            list_price_invoice(false, Some("actions_macos"), macos),
            macos
        );
        assert_eq!(
            list_price_invoice(false, Some("actions_linux_4_core"), usd("0.012")),
            usd("0.012")
        );
    }

    #[test]
    fn remainder_micros_go_to_the_largest_weight() {
        let parts = vec![part(1, 1, 2), part(1, 1, 1), part(3, 3, 3)];
        let shares = allocate(100_001, &parts);
        assert_eq!(shares, vec![20_000, 20_000, 60_001]);
        assert_eq!(shares.iter().sum::<i64>(), 100_001);

        let exact = allocate(100_000, &parts);
        assert_eq!(exact, vec![20_000, 20_000, 60_000]);
        assert_eq!(exact.iter().sum::<i64>(), 100_000);

        let negative = allocate(-8, &parts);
        assert_eq!(negative, vec![-1, -1, -6]);
        assert_eq!(negative.iter().sum::<i64>(), -8);
    }

    #[test]
    fn zero_weights_give_the_whole_net_to_the_largest_tie_break() {
        let parts = vec![part(0, 0, 5), part(0, 4, 9), part(0, 4, 2)];
        let shares = allocate(10, &parts);
        assert_eq!(shares, vec![0, 0, 10]);
        assert_eq!(shares.iter().sum::<i64>(), 10);

        let mixed = allocate(10, &[part(0, 0, 1), part(5, 5, 2)]);
        assert_eq!(mixed, vec![0, 10]);
    }

    #[test]
    fn yaml_weight_gives_macos_lint_and_linux_test_their_own_sku_net() {
        let text = "\
name: CI
on: push
jobs:
  lint:
    runs-on: macos-latest
    steps:
      - run: cargo clippy -- -D warnings
  test:
    runs-on: ubuntu-latest
    steps:
      - run: cargo test
";
        let scanned = vec![scan_yaml(Path::new("repo/.github/workflows/ci.yml"), text).unwrap()];
        assert_eq!(scanned[0].jobs[0].runs_on.as_ref().unwrap().line, 5);
        assert_eq!(scanned[0].jobs[1].runs_on.as_ref().unwrap().line, 9);
        let workflows = vec![WorkflowTotal {
            repository: "acme/app".to_string(),
            workflow_path: ".github/workflows/ci.yml".to_string(),
            net: usd("1.620001"),
            gross: usd("1.620001"),
            discount: Micros::ZERO,
            minutes: usd("11"),
            skus: vec![
                SkuTotal {
                    sku: "Actions_MacOS".to_string(),
                    net: usd("0.620001"),
                    gross: usd("0.620001"),
                    discount: Micros::ZERO,
                    minutes: usd("10"),
                },
                SkuTotal {
                    sku: "actions_linux".to_string(),
                    net: usd("1.000000"),
                    gross: usd("1.000000"),
                    discount: Micros::ZERO,
                    minutes: usd("1"),
                },
            ],
        }];
        let got = reconcile(&workflows, None, &scanned);
        let jobs = &got.workflows[0].jobs;
        assert_eq!(jobs.len(), 2, "{jobs:?}");
        let lint = jobs.iter().find(|job| job.name == "lint").unwrap();
        let test = jobs.iter().find(|job| job.name == "test").unwrap();
        assert_eq!(lint.line, Some(5));
        assert_eq!(lint.amount.method, CostMethod::YamlWeight);
        assert_eq!(lint.amount.micros, usd("0.620001"));
        assert_eq!(lint.sku.as_deref(), Some("actions_macos"));
        assert_eq!(test.line, Some(9));
        assert_eq!(test.amount.method, CostMethod::YamlWeight);
        assert_eq!(test.amount.micros, usd("1.000000"));
        assert_eq!(test.sku.as_deref(), Some("actions_linux"));
        let sum = lint.amount.micros.0 + test.amount.micros.0;
        assert_eq!(sum, workflows[0].net.0);
        assert_eq!(got.workflows[0].invoice.method, CostMethod::YamlWeight);
        assert_eq!(got.workflows[0].measured.method, CostMethod::ListPrice);
        assert_eq!(got.workflows[0].measured.micros, Micros::ZERO);
        assert_eq!(got.workflows[0].difference.micros, workflows[0].net);
    }

    #[test]
    fn yaml_weight_scales_matrix_cells_and_puts_remainder_on_the_wider_job() {
        let text = "\
name: CI
on: push
jobs:
  one:
    runs-on: ubuntu-latest
    steps:
      - run: echo one
  many:
    runs-on: ubuntu-latest
    strategy:
      matrix:
        node: [18, 20]
    steps:
      - run: echo many
";
        let scanned = vec![scan_yaml(Path::new("app/.github/workflows/ci.yml"), text).unwrap()];
        let workflows = vec![workflow(
            ".github/workflows/ci.yml",
            "acme/app",
            "0.000010",
            "actions_linux",
        )];
        let got = reconcile(&workflows, None, &scanned);
        let jobs = &got.workflows[0].jobs;
        let one = jobs.iter().find(|job| job.name == "one").unwrap();
        let many = jobs.iter().find(|job| job.name == "many").unwrap();
        assert_eq!(one.amount.method, CostMethod::YamlWeight);
        assert_eq!(many.amount.method, CostMethod::YamlWeight);
        assert_eq!(one.amount.micros.0, 3);
        assert_eq!(many.amount.micros.0, 7);
        assert_eq!(one.amount.micros.0 + many.amount.micros.0, 10);
    }

    #[test]
    fn a_job_without_a_csv_sku_keeps_a_zero_list_price_until_minutes_exist() {
        let text = "\
jobs:
  lint:
    runs-on: macos-latest
    steps:
      - run: cargo clippy
  extra:
    runs-on: ubuntu-latest
    steps:
      - run: echo extra
";
        let scanned = vec![scan_yaml(Path::new(".github/workflows/lint.yml"), text).unwrap()];
        let workflows = vec![workflow(
            ".github/workflows/lint.yml",
            "acme/app",
            "0.062",
            "actions_macos",
        )];
        let got = reconcile(&workflows, None, &scanned);
        let extra = got.workflows[0]
            .jobs
            .iter()
            .find(|job| job.name == "extra")
            .unwrap();
        assert_eq!(extra.amount.method, CostMethod::ListPrice);
        assert_eq!(extra.amount.micros, Micros::ZERO);
        let lint = got.workflows[0]
            .jobs
            .iter()
            .find(|job| job.name == "lint")
            .unwrap();
        assert_eq!(lint.amount.micros, usd("0.062"));
        assert_eq!(lint.amount.method, CostMethod::YamlWeight);
        assert!(lint.file.ends_with("lint.yml"));
    }

    #[test]
    fn uppercase_crlf_and_flow_style_still_split_by_sku() {
        let text = "name: CI\r\non: push\r\njobs: {lint: {runs-on: MACOS-LATEST, steps: [{run: cargo clippy}]}, test: {runs-on: UBUNTU-LATEST, steps: [{run: cargo test}]}}\r\n";
        let scanned = vec![scan_yaml(Path::new("wf/.github/workflows/ci.yml"), text).unwrap()];
        let workflows = vec![WorkflowTotal {
            repository: "ACME/APP".to_string(),
            workflow_path: ".GitHub/Workflows/CI.yml".to_string(),
            net: usd("0.068"),
            gross: usd("0.068"),
            discount: Micros::ZERO,
            minutes: usd("2"),
            skus: vec![
                SkuTotal {
                    sku: "ACTIONS_MACOS".to_string(),
                    net: usd("0.062"),
                    gross: usd("0.062"),
                    discount: Micros::ZERO,
                    minutes: usd("1"),
                },
                SkuTotal {
                    sku: "ACTIONS_LINUX".to_string(),
                    net: usd("0.006"),
                    gross: usd("0.006"),
                    discount: Micros::ZERO,
                    minutes: usd("1"),
                },
            ],
        }];
        let got = reconcile(&workflows, None, &scanned);
        let jobs = &got.workflows[0].jobs;
        assert_eq!(jobs.len(), 2, "{jobs:?}");
        let sum = jobs.iter().map(|job| job.amount.micros.0).sum::<i64>();
        assert_eq!(sum, usd("0.068").0);
        assert!(jobs
            .iter()
            .all(|job| job.amount.method == CostMethod::YamlWeight));
        assert_eq!(
            jobs.iter()
                .find(|job| job.name == "lint")
                .unwrap()
                .amount
                .micros,
            usd("0.062")
        );
        assert_eq!(
            jobs.iter()
                .find(|job| job.name == "test")
                .unwrap()
                .amount
                .micros,
            usd("0.006")
        );
    }
}
