use std::fmt::Write;

use crate::findings::Finding;
use crate::rank::WorkflowTotal;

/// Text report. Amounts stay exact until this formats them.
///
/// `total_net` and `total_minutes` are the whole bill. `workflows` may already
/// be limited by `--top`; each share is still that workflow's part of `total_net`.
pub fn render(
    workflows: &[WorkflowTotal],
    total_net: f64,
    total_minutes: f64,
    findings: Option<&[Finding]>,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "USD (from net_amount)");
    let _ = writeln!(out);
    let _ = writeln!(out, "Total net: {}", format_number(total_net));
    let _ = writeln!(out, "Total minutes: {}", format_number(total_minutes));
    let _ = writeln!(out);

    let rows = table_rows(workflows, total_net);
    let widths = widths(&rows);
    emit_row(
        &mut out,
        &Row {
            name: "Workflow".to_string(),
            net: "Net".to_string(),
            minutes: "Minutes".to_string(),
            share: "Share".to_string(),
        },
        widths,
    );
    for row in &rows {
        emit_row(&mut out, row, widths);
    }

    if let Some(findings) = findings {
        let _ = writeln!(out);
        let _ = writeln!(out, "Findings");
        if findings.is_empty() {
            let _ = writeln!(out, "(none)");
        } else {
            for finding in findings {
                let _ = writeln!(
                    out,
                    "{} {}:{}: {}",
                    finding.kind.code(),
                    finding.path.display(),
                    finding.line,
                    finding.message
                );
            }
        }
    }
    out
}

struct Row {
    name: String,
    net: String,
    minutes: String,
    share: String,
}

#[derive(Clone, Copy)]
struct Widths {
    name: usize,
    net: usize,
    minutes: usize,
    share: usize,
}

fn table_rows(workflows: &[WorkflowTotal], total_net: f64) -> Vec<Row> {
    let mut rows = Vec::new();
    for workflow in workflows {
        rows.push(Row {
            name: workflow.workflow_path.clone(),
            net: format_number(workflow.net),
            minutes: format_number(workflow.minutes),
            share: format_share(workflow.net, total_net),
        });
        for sku in &workflow.skus {
            rows.push(Row {
                name: format!("  {}", sku.sku),
                net: format_number(sku.net),
                minutes: format_number(sku.minutes),
                share: String::new(),
            });
        }
    }
    rows
}

fn widths(rows: &[Row]) -> Widths {
    let mut widths = Widths {
        name: "Workflow".len(),
        net: "Net".len(),
        minutes: "Minutes".len(),
        share: "Share".len(),
    };
    for row in rows {
        widths.name = widths.name.max(row.name.len());
        widths.net = widths.net.max(row.net.len());
        widths.minutes = widths.minutes.max(row.minutes.len());
        widths.share = widths.share.max(row.share.len());
    }
    widths
}

fn emit_row(out: &mut String, row: &Row, widths: Widths) {
    let _ = writeln!(
        out,
        "{:<name_w$}  {:>net_w$}  {:>min_w$}  {:>share_w$}",
        row.name,
        row.net,
        row.minutes,
        row.share,
        name_w = widths.name,
        net_w = widths.net,
        min_w = widths.minutes,
        share_w = widths.share,
    );
}

/// Round to 3 decimal places for display, then drop trailing zeros.
fn format_number(value: f64) -> String {
    if !value.is_finite() {
        return "0".to_string();
    }
    let text = format!("{value:.3}");
    let trimmed = text.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-" {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

fn format_share(net: f64, total: f64) -> String {
    if !net.is_finite() || !total.is_finite() || total == 0.0 {
        "0.00%".to_string()
    } else {
        format!("{:.2}%", net / total * 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::findings::{Finding, FindingKind};
    use crate::rank::{SkuTotal, WorkflowTotal};
    use std::path::PathBuf;

    fn workflow(path: &str, net: f64, minutes: f64, sku: &str) -> WorkflowTotal {
        WorkflowTotal {
            workflow_path: path.to_string(),
            net,
            gross: net,
            minutes,
            skus: vec![SkuTotal {
                sku: sku.to_string(),
                net,
                gross: net,
                minutes,
            }],
        }
    }

    #[test]
    fn report_lists_total_share_skus_and_findings() {
        let workflows = vec![
            workflow("a.yml", 3.0, 10.0, "actions_linux"),
            workflow("b.yml", 1.0, 0.0, "actions_storage"),
        ];
        let findings = vec![Finding {
            path: PathBuf::from("demo.yml"),
            line: 4,
            kind: FindingKind::MissingTimeout,
            message: "default timeout is 360 minutes".to_string(),
        }];
        let text = render(&workflows, 4.0, 10.0, Some(&findings));
        assert!(text.starts_with("USD (from net_amount)\n"), "{text}");
        assert!(text.contains("Total net: 4\n"), "{text}");
        assert!(text.contains("Total minutes: 10\n"), "{text}");
        assert!(text.contains("75.00%"), "{text}");
        assert!(text.contains("25.00%"), "{text}");
        assert!(text.contains("actions_linux"), "{text}");
        assert!(text.contains("actions_storage"), "{text}");
        assert!(
            text.contains("F2 demo.yml:4: default timeout is 360 minutes"),
            "{text}"
        );
        assert!(text.find("Findings").unwrap() > text.find("a.yml").unwrap());
        assert!(!text.contains("Sample"), "{text}");
    }

    #[test]
    fn share_uses_the_whole_bill() {
        let workflows = vec![workflow(
            ".github/workflows/lint.yml",
            230.144,
            3712.0,
            "actions_macos",
        )];
        let text = render(&workflows, 284.208, 12208.0, None);
        assert!(text.contains("Total net: 284.208"), "{text}");
        assert!(text.contains("80.98%"), "{text}");
        assert!(!text.contains("100.00%"), "{text}");
        assert!(!text.contains("Findings"), "{text}");
        assert!(!text.contains("Sample"), "{text}");
    }

    #[test]
    fn empty_scan_says_there_are_no_findings() {
        let text = render(&[], 0.0, 0.0, Some(&[]));
        assert!(text.contains("Findings\n(none)\n"), "{text}");
        assert!(!text.contains("Sample"), "{text}");
    }
}
