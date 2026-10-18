use std::collections::{BTreeSet, HashMap};

use crate::money::Micros;
use crate::usage::{is_actions_product, UsageError, UsageErrorKind, UsageRow};

/// Cost of one SKU inside a repository workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkuTotal {
    pub sku: String,
    pub net: Micros,
    pub gross: Micros,
    pub discount: Micros,
    pub minutes: Micros,
}

/// Cost of one repository + workflow path, with a total per SKU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowTotal {
    pub repository: String,
    pub workflow_path: String,
    pub net: Micros,
    pub gross: Micros,
    pub discount: Micros,
    pub minutes: Micros,
    pub skus: Vec<SkuTotal>,
}

/// Actions totals plus the rest of the bill that the report still has to show.
///
/// `total_net` and `total_minutes` count product `actions` only. Other products
/// are summed into `other_products_net` and stay outside the Actions total.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bill {
    pub total_net: Micros,
    pub total_minutes: Micros,
    pub other_products_net: Micros,
    pub period_from: Option<String>,
    pub period_to: Option<String>,
    pub repositories: Vec<String>,
}

/// Ranked Actions workflows plus the bill those rows came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ranking {
    pub workflows: Vec<WorkflowTotal>,
    pub bill: Bill,
}

/// Totals per repository and workflow path, highest Actions net first.
///
/// Equal net sorts by repository, then by path, A to Z. SKUs under a workflow
/// use the same money order, then SKU name. `top` keeps that many workflows.
/// The bill totals always cover every Actions row, including ones `top` hides.
///
/// Minutes add `quantity` only when `unit_type` is `minutes`, ignoring case.
/// Other unit types still add to net and gross. Product names other than
/// `actions` (any ASCII case) do not enter the workflow list.
///
/// Every sum is checked: a row that would push a per-workflow, per-SKU, or
/// grand total out of `i64` micros is a clean input error naming that row's
/// line, the same as any other bad amount in the CSV.
pub fn rank(rows: &[UsageRow], top: Option<usize>) -> Result<Ranking, UsageError> {
    let mut totals: HashMap<(String, String), WorkflowAcc> = HashMap::new();
    let mut bill = Bill {
        total_net: Micros::ZERO,
        total_minutes: Micros::ZERO,
        other_products_net: Micros::ZERO,
        period_from: None,
        period_to: None,
        repositories: Vec::new(),
    };
    let mut repositories = BTreeSet::new();

    for row in rows {
        track_date(&mut bill.period_from, &mut bill.period_to, &row.date);
        if !row.repository.is_empty() {
            repositories.insert(row.repository.clone());
        }
        if !is_actions_product(&row.product) {
            bill.other_products_net =
                checked_total(bill.other_products_net, row.net, row.line, "net_amount")?;
            continue;
        }
        let key = (row.repository.clone(), row.workflow_path.clone());
        add(totals.entry(key).or_default(), row)?;
    }

    let mut workflows: Vec<WorkflowTotal> = totals.into_iter().map(finish_workflow).collect();
    workflows.sort_by(|left, right| {
        right
            .net
            .cmp(&left.net)
            .then_with(|| left.repository.cmp(&right.repository))
            .then_with(|| left.workflow_path.cmp(&right.workflow_path))
    });
    for workflow in &workflows {
        // Each `workflow.net`/`workflow.minutes` is already a sum over many rows, so
        // no single line is to blame if adding it to the grand total overflows.
        bill.total_net = checked_total(bill.total_net, workflow.net, 0, "net_amount")?;
        bill.total_minutes = checked_total(bill.total_minutes, workflow.minutes, 0, "quantity")?;
    }
    if let Some(limit) = top {
        workflows.truncate(limit);
    }
    bill.repositories = repositories.into_iter().collect();
    Ok(Ranking { workflows, bill })
}

/// `total + amount`, or an `AmountOverflow` input error naming `line` and `column`.
fn checked_total(
    total: Micros,
    amount: Micros,
    line: u64,
    column: &'static str,
) -> Result<Micros, UsageError> {
    total.checked_add(amount).ok_or(UsageError {
        line,
        kind: UsageErrorKind::AmountOverflow(column),
    })
}

#[derive(Default)]
struct WorkflowAcc {
    net: Micros,
    gross: Micros,
    discount: Micros,
    minutes: Micros,
    skus: HashMap<String, SkuAcc>,
}

#[derive(Default)]
struct SkuAcc {
    net: Micros,
    gross: Micros,
    discount: Micros,
    minutes: Micros,
}

fn add(acc: &mut WorkflowAcc, row: &UsageRow) -> Result<(), UsageError> {
    let minutes = if row.unit_type.eq_ignore_ascii_case("minutes") {
        row.quantity
    } else {
        Micros::ZERO
    };
    acc.net = checked_total(acc.net, row.net, row.line, "net_amount")?;
    acc.gross = checked_total(acc.gross, row.gross, row.line, "gross_amount")?;
    acc.discount = checked_total(acc.discount, row.discount, row.line, "discount_amount")?;
    acc.minutes = checked_total(acc.minutes, minutes, row.line, "quantity")?;
    let sku = acc.skus.entry(row.sku.clone()).or_default();
    sku.net = checked_total(sku.net, row.net, row.line, "net_amount")?;
    sku.gross = checked_total(sku.gross, row.gross, row.line, "gross_amount")?;
    sku.discount = checked_total(sku.discount, row.discount, row.line, "discount_amount")?;
    sku.minutes = checked_total(sku.minutes, minutes, row.line, "quantity")?;
    Ok(())
}

fn finish_workflow(
    ((repository, workflow_path), acc): ((String, String), WorkflowAcc),
) -> WorkflowTotal {
    let mut skus: Vec<SkuTotal> = acc
        .skus
        .into_iter()
        .map(|(sku, sku_acc)| SkuTotal {
            sku,
            net: sku_acc.net,
            gross: sku_acc.gross,
            discount: sku_acc.discount,
            minutes: sku_acc.minutes,
        })
        .collect();
    skus.sort_by(|left, right| {
        right
            .net
            .cmp(&left.net)
            .then_with(|| left.sku.cmp(&right.sku))
    });
    WorkflowTotal {
        repository,
        workflow_path,
        net: acc.net,
        gross: acc.gross,
        discount: acc.discount,
        minutes: acc.minutes,
        skus,
    }
}

fn track_date(min_date: &mut Option<String>, max_date: &mut Option<String>, date: &str) {
    if date.is_empty() {
        return;
    }
    match min_date {
        Some(current) if date.as_bytes() < current.as_bytes() => *current = date.to_string(),
        None => *min_date = Some(date.to_string()),
        Some(_) => {}
    }
    match max_date {
        Some(current) if date.as_bytes() > current.as_bytes() => *current = date.to_string(),
        None => *max_date = Some(date.to_string()),
        Some(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usd(text: &str) -> Micros {
        Micros::parse(text).unwrap()
    }

    fn row(
        repository: &str,
        workflow_path: &str,
        sku: &str,
        quantity: &str,
        unit_type: &str,
        net_amount: &str,
        gross_amount: &str,
    ) -> UsageRow {
        UsageRow {
            line: 2,
            date: "2026-09-01".to_string(),
            product: "actions".to_string(),
            sku: sku.to_string(),
            quantity: usd(quantity),
            unit_type: unit_type.to_string(),
            applied_cost_per_quantity: Micros::ZERO,
            gross: usd(gross_amount),
            discount: Micros::ZERO,
            net: usd(net_amount),
            username: String::new(),
            organization: String::new(),
            repository: repository.to_string(),
            workflow_path: workflow_path.to_string(),
        }
    }

    fn sku(name: &str, net: &str, gross: &str, minutes: &str) -> SkuTotal {
        SkuTotal {
            sku: name.to_string(),
            net: usd(net),
            gross: usd(gross),
            discount: Micros::ZERO,
            minutes: usd(minutes),
        }
    }

    fn workflow(
        path: &str,
        net: &str,
        gross: &str,
        minutes: &str,
        skus: Vec<SkuTotal>,
    ) -> WorkflowTotal {
        WorkflowTotal {
            repository: String::new(),
            workflow_path: path.to_string(),
            net: usd(net),
            gross: usd(gross),
            discount: Micros::ZERO,
            minutes: usd(minutes),
            skus,
        }
    }

    /// Three workflows, two SKUs. Storage money counts; storage quantity does not.
    ///
    /// ci.yml
    ///   actions_linux    20 minutes          net 1.00  gross 1.00
    ///   actions_storage   2 gigabyte-hours   net 4.00  gross 4.00
    ///   actions_linux     5 Minutes          net 0.25  gross 0.25
    ///   → net 5.25, gross 5.25, minutes 25
    ///     actions_storage net 4.00 gross 4.00 minutes 0
    ///     actions_linux   net 1.25 gross 1.25 minutes 25
    ///
    /// lint.yml
    ///   actions_linux   100 minutes          net 2.00  gross 3.00
    ///   actions_linux    50 minutes          net 1.00  gross 1.00
    ///   actions_storage   8 gigabyte-hours   net 0.50  gross 0.50
    ///   → net 3.50, gross 4.50, minutes 150
    ///     actions_linux   net 3.00 gross 4.00 minutes 150
    ///     actions_storage net 0.50 gross 0.50 minutes 0
    ///
    /// release.yml
    ///   actions_storage 100 gigabyte-hours   net 0.25  gross 1.00
    ///   actions_linux     1 MINUTES          net 0.25  gross 0.75
    ///   → net 0.50, gross 1.75, minutes 1
    ///     actions_linux   net 0.25 gross 0.75 minutes 1
    ///     actions_storage net 0.25 gross 1.00 minutes 0
    fn three_workflows() -> Vec<UsageRow> {
        vec![
            row(
                "",
                ".github/workflows/release.yml",
                "actions_storage",
                "100",
                "gigabyte-hours",
                "0.25",
                "1",
            ),
            row(
                "",
                ".github/workflows/release.yml",
                "actions_linux",
                "1",
                "MINUTES",
                "0.25",
                "0.75",
            ),
            row(
                "",
                ".github/workflows/lint.yml",
                "actions_linux",
                "100",
                "minutes",
                "2",
                "3",
            ),
            row(
                "",
                ".github/workflows/lint.yml",
                "actions_linux",
                "50",
                "minutes",
                "1",
                "1",
            ),
            row(
                "",
                ".github/workflows/lint.yml",
                "actions_storage",
                "8",
                "gigabyte-hours",
                "0.5",
                "0.5",
            ),
            row(
                "",
                ".github/workflows/ci.yml",
                "actions_linux",
                "20",
                "minutes",
                "1",
                "1",
            ),
            row(
                "",
                ".github/workflows/ci.yml",
                "actions_storage",
                "2",
                "gigabyte-hours",
                "4",
                "4",
            ),
            row(
                "",
                ".github/workflows/ci.yml",
                "actions_linux",
                "5",
                "Minutes",
                "0.25",
                "0.25",
            ),
        ]
    }

    fn expected_three() -> Vec<WorkflowTotal> {
        vec![
            workflow(
                ".github/workflows/ci.yml",
                "5.25",
                "5.25",
                "25",
                vec![
                    sku("actions_storage", "4", "4", "0"),
                    sku("actions_linux", "1.25", "1.25", "25"),
                ],
            ),
            workflow(
                ".github/workflows/lint.yml",
                "3.5",
                "4.5",
                "150",
                vec![
                    sku("actions_linux", "3", "4", "150"),
                    sku("actions_storage", "0.5", "0.5", "0"),
                ],
            ),
            workflow(
                ".github/workflows/release.yml",
                "0.5",
                "1.75",
                "1",
                vec![
                    sku("actions_linux", "0.25", "0.75", "1"),
                    sku("actions_storage", "0.25", "1", "0"),
                ],
            ),
        ]
    }

    #[test]
    fn ranks_three_workflows_and_two_skus() {
        let ranking = rank(&three_workflows(), None).unwrap();
        assert_eq!(ranking.workflows, expected_three());
        assert_eq!(ranking.bill.total_net, usd("9.25"));
        assert_eq!(ranking.bill.total_minutes, usd("176"));
        assert_eq!(ranking.bill.other_products_net, Micros::ZERO);
        assert_eq!(ranking.bill.period_from.as_deref(), Some("2026-09-01"));
        assert_eq!(ranking.bill.period_to.as_deref(), Some("2026-09-01"));
    }

    #[test]
    fn equal_net_sorts_by_repository_then_path() {
        // b.yml has the higher gross, so path order is not gross order.
        let rows = vec![
            row("", "b.yml", "actions_linux", "1", "minutes", "5", "9"),
            row("", "a.yml", "actions_linux", "2", "minutes", "5", "1"),
            row(
                "",
                "c.yml",
                "actions_storage",
                "3",
                "gigabyte-hours",
                "1",
                "1",
            ),
        ];
        let ranked = rank(&rows, None).unwrap();
        assert_eq!(
            ranked
                .workflows
                .iter()
                .map(|item| item.workflow_path.as_str())
                .collect::<Vec<_>>(),
            vec!["a.yml", "b.yml", "c.yml"]
        );
        assert_eq!(ranked.workflows[0].net, usd("5"));
        assert_eq!(ranked.workflows[1].net, usd("5"));
        assert_eq!(ranked.workflows[0].minutes, usd("2"));
        assert_eq!(ranked.workflows[2].minutes, Micros::ZERO);
        assert_eq!(ranked.workflows[2].net, usd("1"));
    }

    #[test]
    fn same_workflow_path_in_two_repositories_stays_two_rows() {
        let mut first = row(
            "example-org/alpha",
            ".github/workflows/ci.yml",
            "actions_linux",
            "10",
            "minutes",
            "4",
            "4",
        );
        first.date = "2026-09-30".to_string();
        first.product = "ACTIONS".to_string();
        let mut second = row(
            "example-org/beta",
            ".github/workflows/ci.yml",
            "actions_linux",
            "3",
            "MINUTES",
            "4",
            "9",
        );
        second.date = "2026-09-01".to_string();
        second.product = "Actions".to_string();
        let mut copilot = row(
            "example-org/beta",
            ".github/workflows/ci.yml",
            "copilot_premium_request",
            "2",
            "requests",
            "7",
            "7",
        );
        copilot.product = "Copilot".to_string();
        copilot.date = String::new();

        let ranking = rank(&[first, second, copilot], None).unwrap();
        assert_eq!(ranking.workflows.len(), 2);
        assert_eq!(ranking.workflows[0].repository, "example-org/alpha");
        assert_eq!(ranking.workflows[1].repository, "example-org/beta");
        assert!(ranking
            .workflows
            .iter()
            .all(|item| item.workflow_path == ".github/workflows/ci.yml"));
        assert_eq!(ranking.workflows[0].net, usd("4"));
        assert_eq!(ranking.workflows[1].net, usd("4"));
        assert_eq!(ranking.workflows[0].minutes, usd("10"));
        assert_eq!(ranking.workflows[1].minutes, usd("3"));
        assert!(ranking.workflows.iter().all(|item| item.skus.len() == 1));
        assert_eq!(ranking.bill.total_net, usd("8"));
        assert_eq!(ranking.bill.total_minutes, usd("13"));
        assert_eq!(ranking.bill.other_products_net, usd("7"));
        assert_eq!(ranking.bill.period_from.as_deref(), Some("2026-09-01"));
        assert_eq!(ranking.bill.period_to.as_deref(), Some("2026-09-30"));
        assert_eq!(
            ranking.bill.repositories,
            vec![
                "example-org/alpha".to_string(),
                "example-org/beta".to_string()
            ]
        );
    }

    #[test]
    fn top_n_keeps_highest_net_and_full_totals() {
        let rows = three_workflows();
        let expected = expected_three();
        assert_eq!(rank(&rows, Some(2)).unwrap().workflows, expected[..2]);
        assert_eq!(rank(&rows, Some(1)).unwrap().workflows, expected[..1]);
        assert_eq!(rank(&rows, Some(0)).unwrap().workflows, []);
        assert_eq!(rank(&rows, Some(10)).unwrap().workflows, expected);
        assert_eq!(rank(&rows, None).unwrap().workflows, expected);
        assert_eq!(rank(&rows, Some(1)).unwrap().bill.total_net, usd("9.25"));
        assert_eq!(rank(&rows, Some(0)).unwrap().bill.total_minutes, usd("176"));
    }

    fn huge(
        line: u64,
        workflow_path: &str,
        quantity: &str,
        unit_type: &str,
        net: &str,
        gross: &str,
        discount: &str,
    ) -> UsageRow {
        UsageRow {
            line,
            date: "2026-09-01".to_string(),
            product: "actions".to_string(),
            sku: "actions_linux".to_string(),
            quantity: usd(quantity),
            unit_type: unit_type.to_string(),
            applied_cost_per_quantity: Micros::ZERO,
            gross: usd(gross),
            discount: usd(discount),
            net: usd(net),
            username: String::new(),
            organization: String::new(),
            repository: String::new(),
            workflow_path: workflow_path.to_string(),
        }
    }

    /// Each amount on its own fits in `i64` micros; summed twice it does not.
    const HUGE: &str = "9000000000000";

    #[test]
    fn net_amount_overflow_in_one_workflow_is_a_clean_error() {
        let rows = vec![
            huge(2, "a.yml", "1", "minutes", HUGE, "0", "0"),
            huge(3, "a.yml", "1", "minutes", HUGE, "0", "0"),
        ];
        let err = rank(&rows, None).unwrap_err();
        assert_eq!(err.line, 3);
        assert_eq!(err.kind, UsageErrorKind::AmountOverflow("net_amount"));
        assert_eq!(err.to_string(), "line 3: net_amount total is out of range");
    }

    #[test]
    fn gross_amount_overflow_in_one_workflow_is_a_clean_error() {
        let rows = vec![
            huge(2, "a.yml", "1", "minutes", "0", HUGE, "0"),
            huge(3, "a.yml", "1", "minutes", "0", HUGE, "0"),
        ];
        let err = rank(&rows, None).unwrap_err();
        assert_eq!(err.line, 3);
        assert_eq!(err.kind, UsageErrorKind::AmountOverflow("gross_amount"));
    }

    #[test]
    fn discount_amount_overflow_in_one_workflow_is_a_clean_error() {
        let rows = vec![
            huge(2, "a.yml", "1", "minutes", "0", "0", HUGE),
            huge(3, "a.yml", "1", "minutes", "0", "0", HUGE),
        ];
        let err = rank(&rows, None).unwrap_err();
        assert_eq!(err.line, 3);
        assert_eq!(err.kind, UsageErrorKind::AmountOverflow("discount_amount"));
    }

    #[test]
    fn quantity_overflow_in_one_workflow_is_a_clean_error() {
        let rows = vec![
            huge(2, "a.yml", HUGE, "minutes", "0", "0", "0"),
            huge(3, "a.yml", HUGE, "minutes", "0", "0", "0"),
        ];
        let err = rank(&rows, None).unwrap_err();
        assert_eq!(err.line, 3);
        assert_eq!(err.kind, UsageErrorKind::AmountOverflow("quantity"));
        assert_eq!(err.to_string(), "line 3: quantity total is out of range");
    }

    #[test]
    fn grand_total_net_overflow_across_workflows_is_a_clean_error() {
        let rows = vec![
            huge(2, "a.yml", "1", "minutes", HUGE, "0", "0"),
            huge(3, "b.yml", "1", "minutes", HUGE, "0", "0"),
        ];
        let err = rank(&rows, None).unwrap_err();
        assert_eq!(err.line, 0);
        assert_eq!(err.kind, UsageErrorKind::AmountOverflow("net_amount"));
        assert_eq!(
            err.to_string(),
            "net_amount total is out of range (sum of all rows)"
        );
    }

    #[test]
    fn grand_total_quantity_overflow_across_workflows_is_a_clean_error() {
        let rows = vec![
            huge(2, "a.yml", HUGE, "minutes", "0", "0", "0"),
            huge(3, "b.yml", HUGE, "minutes", "0", "0", "0"),
        ];
        let err = rank(&rows, None).unwrap_err();
        assert_eq!(err.line, 0);
        assert_eq!(err.kind, UsageErrorKind::AmountOverflow("quantity"));
        assert_eq!(
            err.to_string(),
            "quantity total is out of range (sum of all rows)"
        );
    }

    #[test]
    fn no_overflow_error_message_prints_line_zero() {
        let rows = vec![
            huge(2, "a.yml", "1", "minutes", HUGE, "0", "0"),
            huge(3, "b.yml", "1", "minutes", HUGE, "0", "0"),
        ];
        let err = rank(&rows, None).unwrap_err();
        assert!(!err.to_string().contains("line 0"), "{err}");
    }

    #[test]
    fn other_products_net_overflow_is_a_clean_error() {
        let mut first = huge(2, "", "0", "requests", HUGE, "0", "0");
        first.product = "copilot".to_string();
        let mut second = huge(3, "", "0", "requests", HUGE, "0", "0");
        second.product = "copilot".to_string();
        let err = rank(&[first, second], None).unwrap_err();
        assert_eq!(err.line, 3);
        assert_eq!(err.kind, UsageErrorKind::AmountOverflow("net_amount"));
    }

    #[test]
    fn negative_net_overflow_in_one_workflow_is_a_clean_error() {
        let rows = vec![
            huge(2, "a.yml", "1", "minutes", "-9000000000000", "0", "0"),
            huge(3, "a.yml", "1", "minutes", "-9000000000000", "0", "0"),
        ];
        let err = rank(&rows, None).unwrap_err();
        assert_eq!(err.line, 3);
        assert_eq!(err.kind, UsageErrorKind::AmountOverflow("net_amount"));
    }

    #[test]
    fn negative_grand_total_overflow_across_workflows_is_a_clean_error() {
        let rows = vec![
            huge(2, "a.yml", "1", "minutes", "-9000000000000", "0", "0"),
            huge(3, "b.yml", "1", "minutes", "-9000000000000", "0", "0"),
        ];
        let err = rank(&rows, None).unwrap_err();
        assert_eq!(err.line, 0);
        assert_eq!(err.kind, UsageErrorKind::AmountOverflow("net_amount"));
    }
}
