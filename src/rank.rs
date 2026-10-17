use std::cmp::Ordering;
use std::collections::HashMap;

use crate::usage::UsageRow;

/// Cost of one SKU inside a workflow.
#[derive(Debug, Clone, PartialEq)]
pub struct SkuTotal {
    pub sku: String,
    pub net: f64,
    pub gross: f64,
    pub minutes: f64,
}

/// Cost of one workflow, with a total per SKU.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowTotal {
    pub workflow_path: String,
    pub net: f64,
    pub gross: f64,
    pub minutes: f64,
    pub skus: Vec<SkuTotal>,
}

/// Totals per workflow, highest net first. Equal net sorts by path, A to Z.
///
/// SKUs under a workflow use the same order, by SKU name when net is equal.
/// `top` keeps that many workflows; `None` keeps all.
/// Minutes add `quantity` only when `unit_type` is `minutes`, ignoring case.
/// Other unit types still add to net and gross.
pub fn rank(rows: &[UsageRow], top: Option<usize>) -> Vec<WorkflowTotal> {
    let mut totals: HashMap<String, WorkflowAcc> = HashMap::new();
    for row in rows {
        add(totals.entry(row.workflow_path.clone()).or_default(), row);
    }

    let mut workflows: Vec<WorkflowTotal> = totals.into_iter().map(finish_workflow).collect();
    workflows.sort_by(|left, right| {
        cmp_net_then_name(
            left.net,
            &left.workflow_path,
            right.net,
            &right.workflow_path,
        )
    });
    if let Some(limit) = top {
        workflows.truncate(limit);
    }
    workflows
}

#[derive(Default)]
struct WorkflowAcc {
    net: f64,
    gross: f64,
    minutes: f64,
    skus: HashMap<String, SkuAcc>,
}

#[derive(Default)]
struct SkuAcc {
    net: f64,
    gross: f64,
    minutes: f64,
}

fn add(acc: &mut WorkflowAcc, row: &UsageRow) {
    let minutes = if row.unit_type.eq_ignore_ascii_case("minutes") {
        row.quantity
    } else {
        0.0
    };
    acc.net += row.net_amount;
    acc.gross += row.gross_amount;
    acc.minutes += minutes;
    let sku = acc.skus.entry(row.sku.clone()).or_default();
    sku.net += row.net_amount;
    sku.gross += row.gross_amount;
    sku.minutes += minutes;
}

fn finish_workflow((workflow_path, acc): (String, WorkflowAcc)) -> WorkflowTotal {
    let mut skus: Vec<SkuTotal> = acc
        .skus
        .into_iter()
        .map(|(sku, sku_acc)| SkuTotal {
            sku,
            net: sku_acc.net,
            gross: sku_acc.gross,
            minutes: sku_acc.minutes,
        })
        .collect();
    skus.sort_by(|left, right| cmp_net_then_name(left.net, &left.sku, right.net, &right.sku));
    WorkflowTotal {
        workflow_path,
        net: acc.net,
        gross: acc.gross,
        minutes: acc.minutes,
        skus,
    }
}

fn cmp_net_then_name(left_net: f64, left_name: &str, right_net: f64, right_name: &str) -> Ordering {
    right_net
        .partial_cmp(&left_net)
        .unwrap_or(Ordering::Equal)
        .then_with(|| left_name.cmp(right_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        workflow_path: &str,
        sku: &str,
        quantity: f64,
        unit_type: &str,
        net_amount: f64,
        gross_amount: f64,
    ) -> UsageRow {
        UsageRow {
            sku: sku.to_string(),
            quantity,
            unit_type: unit_type.to_string(),
            net_amount,
            gross_amount,
            workflow_path: workflow_path.to_string(),
        }
    }

    fn sku(name: &str, net: f64, gross: f64, minutes: f64) -> SkuTotal {
        SkuTotal {
            sku: name.to_string(),
            net,
            gross,
            minutes,
        }
    }

    fn workflow(
        path: &str,
        net: f64,
        gross: f64,
        minutes: f64,
        skus: Vec<SkuTotal>,
    ) -> WorkflowTotal {
        WorkflowTotal {
            workflow_path: path.to_string(),
            net,
            gross,
            minutes,
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
                ".github/workflows/release.yml",
                "actions_storage",
                100.0,
                "gigabyte-hours",
                0.25,
                1.0,
            ),
            row(
                ".github/workflows/release.yml",
                "actions_linux",
                1.0,
                "MINUTES",
                0.25,
                0.75,
            ),
            row(
                ".github/workflows/lint.yml",
                "actions_linux",
                100.0,
                "minutes",
                2.0,
                3.0,
            ),
            row(
                ".github/workflows/lint.yml",
                "actions_linux",
                50.0,
                "minutes",
                1.0,
                1.0,
            ),
            row(
                ".github/workflows/lint.yml",
                "actions_storage",
                8.0,
                "gigabyte-hours",
                0.5,
                0.5,
            ),
            row(
                ".github/workflows/ci.yml",
                "actions_linux",
                20.0,
                "minutes",
                1.0,
                1.0,
            ),
            row(
                ".github/workflows/ci.yml",
                "actions_storage",
                2.0,
                "gigabyte-hours",
                4.0,
                4.0,
            ),
            row(
                ".github/workflows/ci.yml",
                "actions_linux",
                5.0,
                "Minutes",
                0.25,
                0.25,
            ),
        ]
    }

    fn expected_three() -> Vec<WorkflowTotal> {
        vec![
            workflow(
                ".github/workflows/ci.yml",
                5.25,
                5.25,
                25.0,
                vec![
                    sku("actions_storage", 4.0, 4.0, 0.0),
                    sku("actions_linux", 1.25, 1.25, 25.0),
                ],
            ),
            workflow(
                ".github/workflows/lint.yml",
                3.5,
                4.5,
                150.0,
                vec![
                    sku("actions_linux", 3.0, 4.0, 150.0),
                    sku("actions_storage", 0.5, 0.5, 0.0),
                ],
            ),
            workflow(
                ".github/workflows/release.yml",
                0.5,
                1.75,
                1.0,
                vec![
                    sku("actions_linux", 0.25, 0.75, 1.0),
                    sku("actions_storage", 0.25, 1.0, 0.0),
                ],
            ),
        ]
    }

    #[test]
    fn ranks_three_workflows_and_two_skus() {
        assert_eq!(rank(&three_workflows(), None), expected_three());
    }

    #[test]
    fn equal_net_sorts_by_path() {
        // b.yml has the higher gross, so path order is not gross order.
        let rows = vec![
            row("b.yml", "actions_linux", 1.0, "minutes", 5.0, 9.0),
            row("a.yml", "actions_linux", 2.0, "minutes", 5.0, 1.0),
            row("c.yml", "actions_storage", 3.0, "gigabyte-hours", 1.0, 1.0),
        ];
        let ranked = rank(&rows, None);
        assert_eq!(
            ranked
                .iter()
                .map(|item| item.workflow_path.as_str())
                .collect::<Vec<_>>(),
            vec!["a.yml", "b.yml", "c.yml"]
        );
        assert_eq!(ranked[0].net, 5.0);
        assert_eq!(ranked[1].net, 5.0);
        assert_eq!(ranked[0].minutes, 2.0);
        assert_eq!(ranked[2].minutes, 0.0);
        assert_eq!(ranked[2].net, 1.0);
    }

    #[test]
    fn top_n_keeps_highest_net() {
        let rows = three_workflows();
        let expected = expected_three();
        assert_eq!(rank(&rows, Some(2)), expected[..2]);
        assert_eq!(rank(&rows, Some(1)), expected[..1]);
        assert_eq!(rank(&rows, Some(0)), []);
        assert_eq!(rank(&rows, Some(10)), expected);
        assert_eq!(rank(&rows, None), expected);
    }
}
