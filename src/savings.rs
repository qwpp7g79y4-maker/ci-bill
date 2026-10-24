//! Savings on each finding.
//!
//! F1 is the paid cost of those minutes minus the same minutes at the Linux
//! rate. F2 is one stuck run: 360 minutes times the runner rate times the
//! matrix size. F3 uses the overlap amount. F4 uses rounding loss. F5 uses
//! re-run cost. The list is ordered by savings, highest first.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::Path;

use crate::findings::{Finding, FindingKind, SavingsMethod};
use crate::money::{self, Micros};
use crate::prices::{billed_on_public, embedded_table};
use crate::rank::WorkflowTotal;
use crate::reconcile::{self, RunnerPortion};
use crate::workflow::Workflow;

const STUCK_MINUTES: u64 = 360;

/// Price every finding and order the list by savings, highest first.
///
/// Equal savings keep path, line, kind, and message order. `public_repo` is
/// true when `repo.json` says the repository is public. A standard Linux
/// minute then costs nothing. A CSV net still wins for minutes the report
/// already paid.
pub fn apply(
    found: &mut [Finding],
    workflows: &[WorkflowTotal],
    scanned: &[Workflow],
    public_repo: bool,
) {
    let linux = linux_rate(public_repo);
    let portions: Vec<Vec<RunnerPortion>> = workflows
        .iter()
        .map(|workflow| reconcile::runner_portions(workflow, scanned))
        .collect();
    let rates: Vec<HashMap<String, Micros>> = workflows.iter().map(csv_rates).collect();

    for finding in found.iter_mut() {
        let index = workflows
            .iter()
            .position(|workflow| belongs(workflow, finding));
        match finding.kind {
            FindingKind::LintOnRunner => {
                finding.method = SavingsMethod::LinuxRate;
                finding.savings = index
                    .map(|slot| lint_savings(&portions[slot], finding, linux))
                    .unwrap_or(Micros::ZERO);
            }
            FindingKind::MissingTimeout => {
                finding.method = SavingsMethod::StuckRun;
                finding.savings = index
                    .map(|slot| stuck_savings(&portions[slot], finding, &rates[slot], public_repo))
                    .unwrap_or(Micros::ZERO);
            }
            FindingKind::PullRequestConcurrency => {
                finding.method = SavingsMethod::Overlap;
                finding.savings = Micros::ZERO;
            }
            FindingKind::OverlapSavings => finding.method = SavingsMethod::Overlap,
            FindingKind::MatrixJobs => finding.method = SavingsMethod::Rounding,
            FindingKind::RerunWaste => finding.method = SavingsMethod::Rerun,
            _ => {
                finding.method = SavingsMethod::Recorded;
                finding.savings = Micros::ZERO;
            }
        }
    }

    for index in 0..found.len() {
        if found[index].kind != FindingKind::PullRequestConcurrency {
            continue;
        }
        let amount = overlap_amount(&found[index], workflows, found);
        found[index].savings = amount;
    }

    found.sort_by(by_savings);
}

fn lint_savings(portions: &[RunnerPortion], finding: &Finding, linux: Micros) -> Micros {
    let (macos, windows) = os_family(&finding.message);
    if !macos && !windows {
        return Micros::ZERO;
    }
    let mut minutes = Micros::ZERO;
    let mut net = Micros::ZERO;
    for portion in portions {
        if !runner_portion(portion, finding) || !sku_family(&portion.sku, macos, windows) {
            continue;
        }
        minutes = add(minutes, portion.minutes);
        net = add(net, portion.net);
    }
    // A heavily discounted CSV net can be lower than the same minutes priced
    // at the undiscounted Linux list rate; there is nothing left to save then.
    sub(net, money::rounded_product(minutes, linux)).max(Micros::ZERO)
}

fn stuck_savings(
    portions: &[RunnerPortion],
    finding: &Finding,
    rates: &HashMap<String, Micros>,
    public_repo: bool,
) -> Micros {
    let mut total = Micros::ZERO;
    for portion in portions {
        if portion.job_line != finding.line || !same_file(&portion.file, &finding.path) {
            continue;
        }
        let rate = rate_for(rates, &portion.sku, public_repo);
        total = add(total, stuck_cost(portion.cells, rate));
    }
    total
}

fn overlap_amount(finding: &Finding, workflows: &[WorkflowTotal], found: &[Finding]) -> Micros {
    let overlaps = found
        .iter()
        .filter(|item| item.kind == FindingKind::OverlapSavings);
    if let Some(workflow) = workflows.iter().find(|workflow| belongs(workflow, finding)) {
        return overlaps
            .filter(|item| belongs(workflow, item))
            .fold(Micros::ZERO, |total, item| add(total, item.savings));
    }
    let key = workflow_key_of(finding);
    overlaps
        .filter(|item| workflow_key_of(item).eq_ignore_ascii_case(&key))
        .fold(Micros::ZERO, |total, item| add(total, item.savings))
}

fn by_savings(left: &Finding, right: &Finding) -> Ordering {
    right
        .savings
        .cmp(&left.savings)
        .then_with(|| left.path.cmp(&right.path))
        .then_with(|| left.line.cmp(&right.line))
        .then_with(|| left.kind.cmp(&right.kind))
        .then_with(|| left.message.cmp(&right.message))
        .then_with(|| left.repository.cmp(&right.repository))
        .then_with(|| left.workflow_path.cmp(&right.workflow_path))
}

fn os_family(message: &str) -> (bool, bool) {
    let lower = message.to_ascii_lowercase();
    (lower.contains("macos"), lower.contains("windows"))
}

fn sku_family(sku: &str, macos: bool, windows: bool) -> bool {
    let sku = sku.to_ascii_lowercase();
    (macos && sku.starts_with("actions_macos")) || (windows && sku.starts_with("actions_windows"))
}

fn runner_portion(portion: &RunnerPortion, finding: &Finding) -> bool {
    let line = finding.anchor.unwrap_or(finding.line);
    if portion.runs_on_line != line {
        return false;
    }
    if finding.anchor.is_some() {
        if let Some(key) = &finding.workflow_path {
            return file_has_key(&portion.file, key);
        }
    }
    same_file(&portion.file, &finding.path)
}

fn same_file(file: &str, path: &Path) -> bool {
    let shown = path.display().to_string();
    normalize(&shown) == normalize(file)
}

fn file_has_key(file: &str, key: &str) -> bool {
    match workflow_key_from_text(file) {
        Some(found) => path_matches(&found, key) || path_matches(key, &found),
        None => normalize(file).eq_ignore_ascii_case(&normalize(key)),
    }
}

fn workflow_key_of(finding: &Finding) -> String {
    if let Some(path) = &finding.workflow_path {
        if !path.is_empty() {
            return normalize(path);
        }
    }
    workflow_key(&finding.path).unwrap_or_else(|| finding.path.display().to_string())
}

fn workflow_key(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let workflows = path.parent()?;
    if workflows.file_name()?.to_str()? != "workflows" {
        return None;
    }
    if workflows.parent()?.file_name()?.to_str()? != ".github" {
        return None;
    }
    Some(format!(".github/workflows/{name}"))
}

fn workflow_key_from_text(file: &str) -> Option<String> {
    let normalized = normalize(file);
    let name = normalized.rsplit('/').next()?;
    let workflows = normalized.rsplit_once('/')?.0;
    let workflows_name = workflows.rsplit('/').next()?;
    if !workflows_name.eq_ignore_ascii_case("workflows") {
        return None;
    }
    let github = workflows.rsplit_once('/')?.0;
    let github_name = github.rsplit('/').next()?;
    if !github_name.eq_ignore_ascii_case(".github") {
        return None;
    }
    Some(format!(".github/workflows/{name}"))
}

fn belongs(workflow: &WorkflowTotal, finding: &Finding) -> bool {
    if let Some(workflow_path) = &finding.workflow_path {
        let path_ok = path_matches(&workflow.workflow_path, workflow_path);
        return match &finding.repository {
            Some(repository) => path_ok && workflow.repository == *repository,
            None => path_ok,
        };
    }
    let Some(key) = workflow_key(&finding.path) else {
        return false;
    };
    path_matches(&workflow.workflow_path, &key)
}

fn path_matches(workflow_path: &str, key: &str) -> bool {
    let workflow_path = normalize(workflow_path);
    let key = normalize(key);
    workflow_path == key || workflow_path.ends_with(&format!("/{key}"))
}

fn normalize(text: &str) -> String {
    text.replace('\\', "/")
}

fn csv_rates(workflow: &WorkflowTotal) -> HashMap<String, Micros> {
    let mut rates = HashMap::new();
    for sku in &workflow.skus {
        if sku.minutes.0 <= 0 {
            continue;
        }
        let Some(num) = i128::from(sku.net.0).checked_mul(i128::from(Micros::SCALE)) else {
            continue;
        };
        let Some(rate) = i64::try_from(money::div_half_away(num, i128::from(sku.minutes.0))).ok()
        else {
            continue;
        };
        rates.insert(sku.sku.to_ascii_lowercase(), Micros(rate));
    }
    rates
}

fn rate_for(rates: &HashMap<String, Micros>, sku: &str, public_repo: bool) -> Micros {
    if let Some(rate) = rates.get(&sku.to_ascii_lowercase()) {
        return *rate;
    }
    let table = embedded_table()
        .ok()
        .and_then(|table| table.get(sku).map(|price| price.usd_per_minute))
        .unwrap_or(Micros::ZERO);
    if public_repo && !billed_on_public(sku) {
        return Micros::ZERO;
    }
    reconcile::list_price_invoice(public_repo, Some(sku), table)
}

fn linux_rate(public_repo: bool) -> Micros {
    let table = embedded_table()
        .ok()
        .and_then(|table| table.get("actions_linux").map(|price| price.usd_per_minute))
        .unwrap_or(Micros::ZERO);
    reconcile::list_price_invoice(public_repo, Some("actions_linux"), table)
}

fn stuck_cost(cells: u64, rate: Micros) -> Micros {
    let minutes = STUCK_MINUTES.saturating_mul(cells);
    let Ok(minutes) = i64::try_from(minutes) else {
        return Micros(i64::MAX);
    };
    let qty = minutes.saturating_mul(Micros::SCALE);
    from_i128(money::rounded_product(Micros(qty), rate))
}

fn add(left: Micros, right: Micros) -> Micros {
    Micros(left.0.saturating_add(right.0))
}

fn sub(net: Micros, cost: i128) -> Micros {
    from_i128(i128::from(net.0).saturating_sub(cost))
}

fn from_i128(value: i128) -> Micros {
    Micros(i64::try_from(value).unwrap_or(if value >= 0 { i64::MAX } else { i64::MIN }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::findings::findings;
    use crate::rank::rank;
    use crate::report::{render_json, render_text};
    use crate::usage::read_usage;
    use crate::workflow::scan_dir;
    use std::fs;
    use std::path::PathBuf;

    const HEAD: &str = "date,product,sku,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,net_amount,username,organization,repository,workflow_path,cost_center_name";

    fn usd(text: &str) -> Micros {
        Micros::parse(text).unwrap()
    }

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join("ci-bill-s1").join(name);
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(path.join(".github/workflows")).unwrap();
        path
    }

    fn row(sku: &str, qty: &str, price: &str, net: &str, path: &str) -> String {
        format!("2026-09-01,actions,{sku},{qty},minutes,{price},{net},0,{net},dev,example-org,example-org/app,{path},")
    }

    fn price(kind: FindingKind, found: &[Finding]) -> &Finding {
        found
            .iter()
            .find(|item| item.kind == kind)
            .unwrap_or_else(|| panic!("missing {kind:?} in {found:?}"))
    }

    fn load(name: &str, csv: &str, files: &[(&str, &str)]) -> Vec<Finding> {
        load_public(name, csv, files, false)
    }

    fn load_public(
        name: &str,
        csv: &str,
        files: &[(&str, &str)],
        public_repo: bool,
    ) -> Vec<Finding> {
        let root = scratch(name);
        let usage_path = root.join("usage.csv");
        fs::write(&usage_path, csv).unwrap();
        let dir = root.join(".github/workflows");
        for (file, body) in files {
            fs::write(dir.join(file), body).unwrap();
        }
        let rows = read_usage(&usage_path).unwrap();
        let ranking = rank(&rows, None).unwrap();
        let scanned = scan_dir(&dir).unwrap();
        let mut found = findings(&scanned).unwrap();
        apply(&mut found, &ranking.workflows, &scanned, public_repo);
        found
    }

    #[test]
    fn sample_f1_is_the_macos_minutes_times_the_linux_gap() {
        let rows = read_usage(Path::new("samples/usage-sample.csv")).unwrap();
        let ranking = rank(&rows, None).unwrap();
        let scanned = scan_dir(Path::new("samples/.github/workflows")).unwrap();
        let mut found = findings(&scanned).unwrap();
        apply(&mut found, &ranking.workflows, &scanned, false);

        let lint: Vec<_> = found
            .iter()
            .filter(|item| item.path.ends_with("lint.yml"))
            .collect();
        assert_eq!(lint.len(), 3, "{found:?}");
        assert_eq!(lint[0].kind, FindingKind::LintOnRunner);
        assert_eq!(lint[0].method, SavingsMethod::LinuxRate);
        assert_eq!(lint[0].savings, usd("207.872"));
        assert_eq!(lint[0].line, 8);
        let gap = Micros(usd("0.062").0 - usd("0.006").0);
        assert_eq!(
            lint[0].savings,
            from_i128(money::rounded_product(usd("3712"), gap))
        );

        assert_eq!(lint[1].kind, FindingKind::MissingTimeout);
        assert_eq!(lint[1].method, SavingsMethod::StuckRun);
        assert_eq!(lint[1].savings, usd("22.32"));
        assert_eq!(lint[1].line, 7);

        assert_eq!(lint[2].kind, FindingKind::PullRequestConcurrency);
        assert_eq!(lint[2].method, SavingsMethod::Overlap);
        assert_eq!(lint[2].savings, Micros::ZERO);
        assert_eq!(lint[2].line, 2);

        let docs = found
            .iter()
            .find(|item| item.path.ends_with("docs.yml"))
            .unwrap();
        assert_eq!(docs.kind, FindingKind::MissingTimeout);
        assert_eq!(docs.savings, usd("2.16"));
        assert_eq!(docs.method, SavingsMethod::StuckRun);

        let json = render_json(
            &ranking.workflows,
            &ranking.bill,
            Some(&found),
            None,
            None,
            &scanned,
        );
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let findings_json = value["workflows"][0]["findings"].as_array().unwrap();
        assert_eq!(findings_json[0]["id"], "F1");
        assert_eq!(findings_json[0]["savings"], "207.872000");
        assert_eq!(findings_json[0]["method"], "linux_rate");
        assert_eq!(findings_json[1]["id"], "F2");
        assert_eq!(findings_json[1]["savings"], "22.320000");
        assert_eq!(findings_json[1]["method"], "stuck_run");
        assert_eq!(findings_json[2]["id"], "F3");
        assert_eq!(findings_json[2]["savings"], "0.000000");
        assert_eq!(findings_json[2]["method"], "overlap");
        assert!(json.contains("207.872000"));
        for item in findings_json {
            assert!(item["savings"].is_string());
            assert!(item["method"].is_string());
        }

        let text = render_text(
            &ranking.workflows,
            &ranking.bill,
            Some(&found),
            None,
            None,
            &scanned,
        );
        let f1 = text.find("F1 ").unwrap();
        let f2 = text.find("F2 ").unwrap();
        let f3 = text.find("F3 ").unwrap();
        assert!(f1 < f2 && f2 < f3, "{text}");
        assert!(text.contains("savings 207.87 USD (linux_rate)"), "{text}");
        assert!(text.contains("savings 22.32 USD (stuck_run)"), "{text}");
        assert!(text.contains("savings 0.00 USD (overlap)"), "{text}");
        assert!(text.contains("savings 2.16 USD (stuck_run)"), "{text}");
        let lower = text.to_ascii_lowercase();
        assert!(!lower.contains("not supported"));
        assert!(!lower.contains("limitation"));
    }

    #[test]
    fn column_order_crlf_case_and_blank_path_keep_the_same_f1() {
        let yaml = "name: Lint\non: [pull_request]\njobs:\n  lint:\n    runs-on: macos-latest\n    steps:\n      - run: cargo clippy -- -D warnings\n";
        let base = format!(
            "{HEAD}\n{}",
            row(
                "actions_macos",
                "10",
                "0.062",
                "0.620",
                ".github/workflows/lint.yml"
            )
        );
        let expected = usd("0.560");
        assert_eq!(
            price(
                FindingKind::LintOnRunner,
                &load("base", &base, &[("lint.yml", yaml)])
            )
            .savings,
            expected
        );

        let reversed = format!(
            "cost_center_name,workflow_path,repository,organization,username,net_amount,discount_amount,gross_amount,applied_cost_per_quantity,unit_type,quantity,sku,product,date\n\
             ,{path},{repo},{org},dev,{net},0,{net},{price},{unit},{qty},{sku},{product},{date}\n",
            path = ".github/workflows/lint.yml",
            repo = "example-org/app",
            org = "example-org",
            net = "0.620",
            price = "0.062",
            unit = "minutes",
            qty = "10",
            sku = "actions_macos",
            product = "actions",
            date = "2026-09-01",
        );
        assert_eq!(
            price(
                FindingKind::LintOnRunner,
                &load("reorder", &reversed, &[("lint.yml", yaml)])
            )
            .savings,
            expected
        );

        let crlf_csv = base.replace('\n', "\r\n");
        let crlf_yaml = yaml.replace('\n', "\r\n");
        assert_eq!(
            price(
                FindingKind::LintOnRunner,
                &load("crlf", &crlf_csv, &[("lint.yml", &crlf_yaml)])
            )
            .savings,
            expected
        );

        let loud = format!(
            "{HEAD}\n{}",
            row(
                "ACTIONS_MACOS",
                "10.000000",
                "0.062000",
                "0.620000",
                ".github/workflows/lint.yml"
            )
            .replace(",actions,", ",ACTIONS,")
            .replace(",minutes,", ",MINUTES,")
        );
        let loud_yaml = yaml.replace("macos-latest", "MACOS-LATEST");
        assert_eq!(
            price(
                FindingKind::LintOnRunner,
                &load("case", &loud, &[("lint.yml", &loud_yaml)])
            )
            .savings,
            expected
        );

        let with_blank = format!(
            "{base}\n{}",
            row("actions_macos", "100", "0.062", "6.200", "")
        );
        assert_eq!(
            price(
                FindingKind::LintOnRunner,
                &load("blank-path", &with_blank, &[("lint.yml", yaml)])
            )
            .savings,
            expected
        );

        let empty_repo = format!(
            "{base}\n{}",
            "2026-09-02,actions,actions_macos,50,minutes,0.062,3.100,0,3.100,dev,example-org,,.github/workflows/lint.yml,"
        );
        let split = load("empty-repo", &empty_repo, &[("lint.yml", yaml)]);
        let lint = price(FindingKind::LintOnRunner, &split);
        assert_eq!(lint.savings, usd("2.800"));
    }

    #[test]
    fn windows_large_and_arm_use_their_own_rates() {
        let yaml = "on: push\njobs:\n  lint:\n    runs-on: windows-latest\n    steps:\n      - run: npm run prettier\n";
        let csv = format!(
            "{HEAD}\n{}",
            row(
                "actions_windows",
                "10",
                "0.010",
                "0.100",
                ".github/workflows/lint.yml"
            )
        );
        let found = load("windows", &csv, &[("lint.yml", yaml)]);
        assert_eq!(
            price(FindingKind::LintOnRunner, &found).savings,
            usd("0.040")
        );

        let arm = yaml.replace("windows-latest", "windows-11-arm");
        let arm_csv = format!(
            "{HEAD}\n{}",
            row(
                "actions_windows_arm",
                "10",
                "0.010",
                "0.100",
                ".github/workflows/lint.yml"
            )
        );
        assert_eq!(
            price(
                FindingKind::LintOnRunner,
                &load("arm", &arm_csv, &[("lint.yml", &arm)])
            )
            .savings,
            usd("0.040")
        );

        let large = yaml.replace("windows-latest", "macos-latest-large");
        let large_csv = format!(
            "{HEAD}\n{}",
            row(
                "actions_macos_l",
                "10",
                "0.077",
                "0.770",
                ".github/workflows/lint.yml"
            )
        );
        assert_eq!(
            price(
                FindingKind::LintOnRunner,
                &load("large", &large_csv, &[("lint.yml", &large)])
            )
            .savings,
            usd("0.710")
        );

        let xl = yaml.replace("windows-latest", "macos-14-xlarge");
        let xl_csv = format!(
            "{HEAD}\n{}",
            row(
                "actions_macos_xl",
                "2",
                "0.102",
                "0.204",
                ".github/workflows/lint.yml"
            )
        );
        assert_eq!(
            price(
                FindingKind::LintOnRunner,
                &load("xlarge", &xl_csv, &[("lint.yml", &xl)])
            )
            .savings,
            usd("0.192")
        );
    }

    #[test]
    fn a_discounted_csv_net_wins_over_the_table_rate() {
        let yaml = "on: push\njobs:\n  lint:\n    runs-on: macos-latest\n    steps:\n      - run: cargo fmt --check\n";
        let csv = format!(
            "{HEAD}\n{}",
            row(
                "actions_macos",
                "10",
                "0.050",
                "0.500",
                ".github/workflows/lint.yml"
            )
        );
        let found = load("discount", &csv, &[("lint.yml", yaml)]);
        assert_eq!(
            price(FindingKind::LintOnRunner, &found).savings,
            usd("0.440")
        );
    }

    #[test]
    fn the_linux_tariff_stays_the_table_rate() {
        let yaml = "\
on: push
jobs:
  lint:
    runs-on: macos-latest
    steps:
      - run: cargo clippy -- -D warnings
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - run: cargo test
";
        let csv = format!(
            "{HEAD}\n{}\n{}",
            row(
                "actions_macos",
                "10",
                "0.062",
                "0.620",
                ".github/workflows/lint.yml"
            ),
            row(
                "actions_linux",
                "10",
                "0.001",
                "0.010",
                ".github/workflows/lint.yml"
            )
        );
        let found = load("linux-csv", &csv, &[("lint.yml", yaml)]);
        assert_eq!(
            price(FindingKind::LintOnRunner, &found).savings,
            usd("0.560")
        );
    }

    #[test]
    fn two_macos_jobs_split_the_minutes_by_matrix_size() {
        let yaml = "\
on: push
jobs:
  lint:
    runs-on: macos-latest
    strategy:
      matrix:
        node: ['14', '16']
    steps:
      - run: cargo clippy -- -D warnings
  extra:
    runs-on: macos-latest
    timeout-minutes: 5
    steps:
      - run: echo hi
";
        let csv = format!(
            "{HEAD}\n{}",
            row(
                "actions_macos",
                "90",
                "0.062",
                "5.580",
                ".github/workflows/lint.yml"
            )
        );
        let found = load("split", &csv, &[("lint.yml", yaml)]);
        let lint = price(FindingKind::LintOnRunner, &found);
        assert_eq!(lint.savings, usd("3.360"));
        let stuck = found
            .iter()
            .find(|item| item.kind == FindingKind::MissingTimeout)
            .unwrap();
        assert_eq!(stuck.savings, usd("44.64"));
    }

    #[test]
    fn a_matrix_without_a_timeout_prices_every_cell() {
        let yaml = "\
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    strategy:
      matrix:
        os: [ubuntu-latest, ubuntu-22.04]
        node: ['14', '16', '18']
    steps:
      - run: echo build
";
        let csv = format!(
            "{HEAD}\n{}",
            row(
                "actions_linux",
                "6",
                "0.006",
                "0.036",
                ".github/workflows/ci.yml"
            )
        );
        let found = load("matrix", &csv, &[("ci.yml", yaml)]);
        let stuck = price(FindingKind::MissingTimeout, &found);
        assert_eq!(stuck.savings, usd("12.96"));
        assert_eq!(stuck.method, SavingsMethod::StuckRun);
        let matrix = price(FindingKind::MatrixJobs, &found);
        assert_eq!(matrix.method, SavingsMethod::Rounding);
        assert_eq!(matrix.savings, Micros::ZERO);
        assert!(stuck.savings > matrix.savings);
    }

    #[test]
    fn a_mixed_matrix_adds_each_runner_rate() {
        let yaml = "\
on: push
jobs:
  build:
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        os: [ubuntu-latest, windows-latest, macos-latest]
    steps:
      - run: echo build
";
        let csv = format!(
            "{HEAD}\n{}\n{}\n{}",
            row(
                "actions_linux",
                "1",
                "0.006",
                "0.006",
                ".github/workflows/ci.yml"
            ),
            row(
                "actions_windows",
                "1",
                "0.010",
                "0.010",
                ".github/workflows/ci.yml"
            ),
            row(
                "actions_macos",
                "1",
                "0.062",
                "0.062",
                ".github/workflows/ci.yml"
            )
        );
        let found = load("mixed", &csv, &[("ci.yml", yaml)]);
        assert_eq!(
            price(FindingKind::MissingTimeout, &found).savings,
            usd("28.08")
        );
    }

    #[test]
    fn a_job_without_a_runner_still_has_a_zero_stuck_run() {
        let yaml = "on: push\njobs:\n  build:\n    steps:\n      - run: echo build\n";
        let csv = format!(
            "{HEAD}\n{}",
            row(
                "actions_linux",
                "1",
                "0.006",
                "0.006",
                ".github/workflows/ci.yml"
            )
        );
        let found = load("no-runner", &csv, &[("ci.yml", yaml)]);
        let stuck = price(FindingKind::MissingTimeout, &found);
        assert_eq!(stuck.savings, Micros::ZERO);
        assert_eq!(stuck.method, SavingsMethod::StuckRun);
    }

    #[test]
    fn flow_style_and_quotes_price_the_lint_job() {
        let flow = "on: [pull_request]\njobs: {lint: {runs-on: \"macos-latest\", steps: [{run: cargo clippy -- -D warnings}]}}\n";
        let csv = format!(
            "{HEAD}\n{}",
            row(
                "actions_macos",
                "4",
                "0.062",
                "0.248",
                ".github/workflows/lint.yml"
            )
        );
        let found = load("flow", &csv, &[("lint.yml", flow)]);
        assert_eq!(
            price(FindingKind::LintOnRunner, &found).savings,
            usd("0.224")
        );
        assert_eq!(
            price(FindingKind::MissingTimeout, &found).savings,
            usd("22.32")
        );
        assert_eq!(
            price(FindingKind::PullRequestConcurrency, &found).savings,
            Micros::ZERO
        );
    }

    #[test]
    fn a_composite_action_uses_the_caller_job_minutes() {
        let caller = "\
on: push
jobs:
  lint:
    runs-on: macos-latest
    steps:
      - uses: ./.github/actions/lint
";
        let action = "\
runs:
  using: composite
  steps:
    - run: cargo clippy -- -D warnings
";
        let root = scratch("composite");
        fs::create_dir_all(root.join(".github/actions/lint")).unwrap();
        fs::write(root.join(".github/workflows/lint.yml"), caller).unwrap();
        fs::write(root.join(".github/actions/lint/action.yml"), action).unwrap();
        let csv = format!(
            "{HEAD}\n{}",
            row(
                "actions_macos",
                "8",
                "0.062",
                "0.496",
                ".github/workflows/lint.yml"
            )
        );
        fs::write(root.join("usage.csv"), csv).unwrap();
        let rows = read_usage(&root.join("usage.csv")).unwrap();
        let ranking = rank(&rows, None).unwrap();
        let scanned = scan_dir(&root.join(".github/workflows")).unwrap();
        let mut found = findings(&scanned).unwrap();
        apply(&mut found, &ranking.workflows, &scanned, false);
        let lint = found
            .iter()
            .find(|item| item.kind == FindingKind::LintOnRunner)
            .unwrap();
        assert!(lint.path.ends_with("action.yml"), "{}", lint.path.display());
        assert_eq!(lint.savings, usd("0.448"));
        assert_eq!(lint.method, SavingsMethod::LinuxRate);
    }

    #[test]
    fn f3_takes_the_overlap_amount_and_sorts_with_it() {
        let mut found = vec![
            Finding::record(
                "repo/.github/workflows/ci.yml",
                2,
                FindingKind::PullRequestConcurrency,
                "pull_request without concurrency cancel-in-progress",
            ),
            Finding::record(
                "repo/.github/workflows/ci.yml",
                4,
                FindingKind::MissingTimeout,
                "default timeout is 360 minutes",
            ),
        ];
        found[0].workflow_path = Some(".github/workflows/ci.yml".to_string());
        let mut overlap = Finding::placed(
            "repo/.github/workflows/ci.yml",
            3,
            FindingKind::OverlapSavings,
            "cancel-in-progress would save 0.012 USD (2 minutes)",
            Some("example-org/app".to_string()),
            Some(".github/workflows/ci.yml".to_string()),
        );
        overlap.savings = usd("0.012");
        overlap.method = SavingsMethod::Overlap;
        found.push(overlap);
        let mut rounding = Finding::record(
            "repo/.github/workflows/ci.yml",
            9,
            FindingKind::MatrixJobs,
            "matrix runs 6 jobs",
        );
        rounding.savings = usd("0.030");
        rounding.workflow_path = Some(".github/workflows/ci.yml".to_string());
        found.push(rounding);
        let mut rerun = Finding::record(
            "repo/.github/workflows/ci.yml",
            1,
            FindingKind::RerunWaste,
            "re-runs cost 0.06 USD (1 extra executions)",
        );
        rerun.savings = usd("0.060");
        rerun.workflow_path = Some(".github/workflows/ci.yml".to_string());
        found.push(rerun);

        apply(&mut found, &[], &[], false);
        assert_eq!(found[0].kind, FindingKind::RerunWaste);
        assert_eq!(found[0].savings, usd("0.060"));
        assert_eq!(found[0].method, SavingsMethod::Rerun);
        assert_eq!(found[1].kind, FindingKind::MatrixJobs);
        assert_eq!(found[1].savings, usd("0.030"));
        assert_eq!(found[1].method, SavingsMethod::Rounding);
        assert_eq!(found[2].kind, FindingKind::PullRequestConcurrency);
        assert_eq!(found[2].savings, usd("0.012"));
        assert_eq!(found[2].method, SavingsMethod::Overlap);
        assert_eq!(found[3].kind, FindingKind::OverlapSavings);
        assert_eq!(found[3].savings, usd("0.012"));
        assert_eq!(found[4].kind, FindingKind::MissingTimeout);
        assert_eq!(found[4].savings, Micros::ZERO);
        assert_eq!(found[4].method, SavingsMethod::StuckRun);
    }

    #[test]
    fn a_public_repository_prices_standard_linux_at_zero_and_keeps_larger_runners() {
        let yaml = "\
on: push
jobs:
  lint:
    runs-on: macos-latest
    steps:
      - run: cargo clippy -- -D warnings
  wide:
    runs-on: ubuntu-24.04-16core
    steps:
      - run: echo wide
";
        let csv = format!(
            "{HEAD}\n{}",
            row(
                "actions_macos",
                "10",
                "0.062",
                "0.620",
                ".github/workflows/lint.yml"
            )
        );
        let found = load_public("public", &csv, &[("lint.yml", yaml)], true);
        assert_eq!(
            price(FindingKind::LintOnRunner, &found).savings,
            usd("0.620")
        );
        let stuck: Vec<_> = found
            .iter()
            .filter(|item| item.kind == FindingKind::MissingTimeout)
            .collect();
        assert_eq!(stuck.len(), 2, "{found:?}");
        let large = stuck
            .iter()
            .find(|item| item.savings != usd("22.32"))
            .unwrap();
        assert_eq!(large.savings, usd("15.12"));
    }

    #[test]
    fn equal_savings_keep_path_and_line_order() {
        let mut found = vec![
            Finding::record(
                "b.yml",
                2,
                FindingKind::DuplicateKey,
                "duplicate key runs-on (line 1 and 2)",
            ),
            Finding::record(
                "a.yml",
                4,
                FindingKind::InvoiceMismatch,
                "invoice row mismatch",
            ),
            Finding::record(
                "a.yml",
                3,
                FindingKind::DuplicateKey,
                "duplicate key on (line 1 and 3)",
            ),
        ];
        apply(&mut found, &[], &[], false);
        assert!(found.iter().all(|item| item.savings == Micros::ZERO));
        assert!(found
            .iter()
            .all(|item| item.method == SavingsMethod::Recorded));
        assert_eq!(found[0].path, Path::new("a.yml"));
        assert_eq!(found[0].line, 3);
        assert_eq!(found[1].path, Path::new("a.yml"));
        assert_eq!(found[1].line, 4);
        assert_eq!(found[2].path, Path::new("b.yml"));
    }

    #[test]
    fn f1_savings_floors_at_zero_when_the_discounted_net_beats_the_linux_rate() {
        // 1000 macOS minutes billed at a steep negotiated discount (net
        // 0.006 total) cost less than those same 1000 minutes would at the
        // undiscounted Linux list rate (0.008/minute = 8.00), so there is
        // nothing left to save by moving to Linux: savings must be 0, not
        // negative.
        let csv = format!(
            "{HEAD}\n2026-09-01,actions,actions_macos,1000,minutes,0.062,62.000000,61.994000,0.006000,dev,example-org,example-org/app,.github/workflows/lint.yml,\n"
        );
        let found = load(
            "f1-floor",
            &csv,
            &[(
                "lint.yml",
                "name: CI\non: push\njobs:\n  lint:\n    runs-on: macos-latest\n    steps:\n      - run: cargo clippy -- -D warnings\n",
            )],
        );
        let lint = price(FindingKind::LintOnRunner, &found);
        assert_eq!(lint.savings, Micros::ZERO, "{found:?}");
    }
}
