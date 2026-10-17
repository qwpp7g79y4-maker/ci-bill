use std::fmt::Write;
use std::path::Path;

use crate::findings::Finding;
use crate::rank::WorkflowTotal;

/// Text report. Money is rounded to cents here. Totals are sums of raw amounts.
///
/// `total_net` and `total_minutes` are the whole bill. `workflows` may already
/// be limited by `--top`; each share is still that workflow's part of `total_net`.
/// Findings attach to a workflow when the YAML path ends in
/// `.github/workflows/<file>` and that suffix matches `workflow_path`.
/// Anything left over is listed under "Workflows without usage".
pub fn render(
    workflows: &[WorkflowTotal],
    total_net: f64,
    total_minutes: f64,
    findings: Option<&[Finding]>,
) -> String {
    let attached = attach(workflows, findings);
    let mut out = String::new();
    let _ = writeln!(out, "USD (from net_amount)");
    let _ = writeln!(out);
    let _ = writeln!(out, "Total net: {}", format_amount(total_net));
    let _ = writeln!(out, "Total minutes: {}", format_number(total_minutes));
    let _ = writeln!(out);

    let blocks: Vec<Vec<Row>> = workflows
        .iter()
        .map(|workflow| workflow_rows(workflow, total_net))
        .collect();
    let widths = widths(&blocks);
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
    for (rows, found) in blocks.iter().zip(&attached.by_workflow) {
        for row in rows {
            emit_row(&mut out, row, widths);
        }
        for finding in found {
            let _ = writeln!(out, "  {}", finding_text(finding));
        }
    }

    if !attached.unattached.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "Workflows without usage");
        for finding in &attached.unattached {
            let _ = writeln!(out, "{}", finding_text(finding));
        }
    }
    out
}

/// One JSON object. Numbers are the raw totals, not the rounded text amounts.
pub fn render_json(
    workflows: &[WorkflowTotal],
    total_net: f64,
    total_minutes: f64,
    findings: Option<&[Finding]>,
    budget: Option<f64>,
) -> String {
    let attached = attach(workflows, findings);
    let workflow_json: Vec<serde_json::Value> = workflows
        .iter()
        .zip(&attached.by_workflow)
        .map(|(workflow, found)| workflow_json(workflow, total_net, found))
        .collect();
    let unattached: Vec<serde_json::Value> = attached
        .unattached
        .iter()
        .map(|finding| finding_json(finding))
        .collect();
    let mut value = serde_json::json!({
        "schema_version": 1,
        "currency": "USD",
        "total_net": finite(total_net),
        "total_minutes": finite(total_minutes),
        "workflows": workflow_json,
        "unattached_findings": unattached,
    });
    if let Some(amount) = budget {
        value["budget"] = serde_json::Value::from(amount);
        value["budget_exceeded"] = serde_json::Value::from(total_net > amount);
    }
    let mut text = serde_json::to_string_pretty(&value).expect("finite report json");
    text.push('\n');
    text
}

/// Status line when the net total is above the budget. Amounts use cent rounding.
pub(crate) fn budget_exceeded_line(total_net: f64, budget: f64) -> String {
    format!(
        "Budget exceeded: {} > {}",
        format_amount(total_net),
        format_amount(budget)
    )
}

struct Attached<'a> {
    by_workflow: Vec<Vec<&'a Finding>>,
    unattached: Vec<&'a Finding>,
}

fn attach<'a>(workflows: &[WorkflowTotal], findings: Option<&'a [Finding]>) -> Attached<'a> {
    let mut by_workflow = vec![Vec::new(); workflows.len()];
    let mut unattached = Vec::new();
    if let Some(findings) = findings {
        for finding in findings {
            if let Some(index) = workflows
                .iter()
                .position(|workflow| belongs_to(&workflow.workflow_path, finding))
            {
                by_workflow[index].push(finding);
            } else {
                unattached.push(finding);
            }
        }
    }
    Attached {
        by_workflow,
        unattached,
    }
}

/// `.github/workflows/<file>` taken from the YAML path.
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

fn belongs_to(workflow_path: &str, finding: &Finding) -> bool {
    let Some(key) = workflow_key(&finding.path) else {
        return false;
    };
    workflow_path == key || workflow_path.ends_with(&format!("/{key}"))
}

fn workflow_json(
    workflow: &WorkflowTotal,
    total_net: f64,
    findings: &[&Finding],
) -> serde_json::Value {
    let skus: Vec<serde_json::Value> = workflow
        .skus
        .iter()
        .map(|sku| {
            serde_json::json!({
                "sku": sku.sku,
                "net": finite(sku.net),
                "minutes": finite(sku.minutes),
            })
        })
        .collect();
    let found: Vec<serde_json::Value> = findings.iter().copied().map(finding_json).collect();
    serde_json::json!({
        "path": workflow.workflow_path,
        "net": finite(workflow.net),
        "gross": finite(workflow.gross),
        "minutes": finite(workflow.minutes),
        "share": finite(share_ratio(workflow.net, total_net)),
        "skus": skus,
        "findings": found,
    })
}

fn finding_json(finding: &Finding) -> serde_json::Value {
    serde_json::json!({
        "id": finding.kind.code(),
        "file": finding.path.display().to_string(),
        "line": finding.line,
        "message": finding.message,
    })
}

fn finding_text(finding: &Finding) -> String {
    format!(
        "{} {}:{}: {}",
        finding.kind.code(),
        finding.path.display(),
        finding.line,
        finding.message
    )
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

fn workflow_rows(workflow: &WorkflowTotal, total_net: f64) -> Vec<Row> {
    let mut rows = vec![Row {
        name: workflow.workflow_path.clone(),
        net: format_amount(workflow.net),
        minutes: format_number(workflow.minutes),
        share: format_share(workflow.net, total_net),
    }];
    for sku in &workflow.skus {
        rows.push(Row {
            name: format!("  {}", sku.sku),
            net: format_amount(sku.net),
            minutes: format_number(sku.minutes),
            share: String::new(),
        });
    }
    rows
}

fn widths(blocks: &[Vec<Row>]) -> Widths {
    let mut widths = Widths {
        name: "Workflow".len(),
        net: "Net".len(),
        minutes: "Minutes".len(),
        share: "Share".len(),
    };
    for row in blocks.iter().flatten() {
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

/// Two decimal places, half up (away from zero). Display only.
fn format_amount(value: f64) -> String {
    if !value.is_finite() {
        return "0.00".to_string();
    }
    let negative = value.is_sign_negative();
    let scaled = value.abs() * 100.0 + 0.5;
    if !scaled.is_finite() {
        return "0.00".to_string();
    }
    let cents = scaled.floor() as u128;
    let sign = if negative && cents != 0 { "-" } else { "" };
    format!("{sign}{}.{:02}", cents / 100, cents % 100)
}

/// Round minutes to 3 decimal places for display, then drop trailing zeros.
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
    format!("{}%", format_amount(share_ratio(net, total)))
}

fn share_ratio(net: f64, total: f64) -> f64 {
    if !net.is_finite() || !total.is_finite() || total == 0.0 {
        0.0
    } else {
        net / total * 100.0
    }
}

fn finite(value: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        0.0
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

    fn finding(path: &str, line: usize, kind: FindingKind, message: &str) -> Finding {
        Finding {
            path: PathBuf::from(path),
            line,
            kind,
            message: message.to_string(),
        }
    }

    #[test]
    fn amounts_round_half_up_to_two_decimals() {
        assert_eq!(format_amount(0.005), "0.01");
        assert_eq!(format_amount(2.112), "2.11");
        assert_eq!(format_amount(0.0), "0.00");
    }

    #[test]
    fn report_lists_total_share_skus_and_findings() {
        let workflows = vec![
            workflow("a.yml", 3.0, 10.0, "actions_linux"),
            workflow("b.yml", 1.0, 0.0, "actions_storage"),
        ];
        let findings = vec![finding(
            "demo.yml",
            4,
            FindingKind::MissingTimeout,
            "default timeout is 360 minutes",
        )];
        let text = render(&workflows, 4.0, 10.0, Some(&findings));
        assert!(text.starts_with("USD (from net_amount)\n"), "{text}");
        assert!(text.contains("Total net: 4.00\n"), "{text}");
        assert!(text.contains("Total minutes: 10\n"), "{text}");
        assert!(text.contains("75.00%"), "{text}");
        assert!(text.contains("25.00%"), "{text}");
        assert!(text.contains("actions_linux"), "{text}");
        assert!(text.contains("actions_storage"), "{text}");
        assert!(
            text.contains(
                "Workflows without usage\nF2 demo.yml:4: default timeout is 360 minutes\n"
            ),
            "{text}"
        );
        assert!(text.find("a.yml").unwrap() < text.find("Workflows without usage").unwrap());
        assert!(!text.contains("Sample"), "{text}");
    }

    #[test]
    fn finding_is_listed_under_its_workflow() {
        let workflows = vec![
            workflow(".github/workflows/lint.yml", 3.0, 10.0, "actions_macos"),
            workflow(".github/workflows/ci.yml", 1.0, 4.0, "actions_linux"),
        ];
        let findings = vec![
            finding(
                "samples/.github/workflows/lint.yml",
                8,
                FindingKind::LintOnRunner,
                "lint on macOS",
            ),
            finding(
                "samples/.github/workflows/nightly.yml",
                2,
                FindingKind::MissingTimeout,
                "default timeout is 360 minutes",
            ),
        ];
        let text = render(&workflows, 4.0, 14.0, Some(&findings));
        let lint = text.find(".github/workflows/lint.yml").expect("lint row");
        let lint_finding = text
            .find("  F1 samples/.github/workflows/lint.yml:8: lint on macOS")
            .expect("lint finding");
        let ci = text.find(".github/workflows/ci.yml").expect("ci row");
        let heading = text
            .find("Workflows without usage")
            .expect("unattached heading");
        let nightly = text
            .find("F2 samples/.github/workflows/nightly.yml:2: default timeout is 360 minutes")
            .expect("nightly finding");
        assert!(lint < lint_finding && lint_finding < ci, "{text}");
        assert!(ci < heading && heading < nightly, "{text}");
        assert_eq!(text.matches("lint on macOS").count(), 1, "{text}");
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
        assert!(text.contains("Total net: 284.21\n"), "{text}");
        assert!(text.contains("80.98%"), "{text}");
        assert!(text.contains("230.14"), "{text}");
        assert!(!text.contains("230.144"), "{text}");
        assert!(!text.contains("284.208"), "{text}");
        assert!(!text.contains("100.00%"), "{text}");
        assert!(!text.contains("Findings"), "{text}");
        assert!(!text.contains("Workflows without usage"), "{text}");
        assert!(!text.contains("Sample"), "{text}");
    }

    #[test]
    fn empty_scan_has_no_finding_section() {
        let text = render(&[], 0.0, 0.0, Some(&[]));
        assert!(text.contains("Total net: 0.00\n"), "{text}");
        assert!(!text.contains("Findings"), "{text}");
        assert!(!text.contains("Workflows without usage"), "{text}");
        assert!(!text.contains("Sample"), "{text}");
    }

    #[test]
    fn json_keeps_unrounded_numbers_and_nests_findings() {
        let workflows = vec![
            workflow(".github/workflows/lint.yml", 2.112, 10.0, "actions_macos"),
            workflow(".github/workflows/ci.yml", 1.0, 0.0, "actions_linux"),
        ];
        let findings = vec![
            finding(
                "app/.github/workflows/lint.yml",
                8,
                FindingKind::LintOnRunner,
                "lint on macOS",
            ),
            finding(
                "app/.github/workflows/nightly.yml",
                3,
                FindingKind::MissingTimeout,
                "default timeout is 360 minutes",
            ),
        ];
        let text = render_json(&workflows, 2.112, 10.0, Some(&findings), None);
        assert!(!text.contains("USD (from net_amount)"), "{text}");
        let value: serde_json::Value = serde_json::from_str(text.trim_end()).expect("json");
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["currency"], "USD");
        assert_eq!(
            value["total_net"].as_f64().unwrap().to_bits(),
            2.112_f64.to_bits()
        );
        assert_eq!(
            value["total_minutes"].as_f64().unwrap().to_bits(),
            10.0_f64.to_bits()
        );
        assert_eq!(value["workflows"][0]["path"], ".github/workflows/lint.yml");
        assert_eq!(
            value["workflows"][0]["net"].as_f64().unwrap().to_bits(),
            2.112_f64.to_bits()
        );
        assert_eq!(
            value["workflows"][0]["gross"].as_f64().unwrap().to_bits(),
            2.112_f64.to_bits()
        );
        assert!(value["workflows"][0]["share"].is_number());
        assert!(value["workflows"][0]["minutes"].is_number());
        assert_eq!(value["workflows"][0]["skus"][0]["sku"], "actions_macos");
        assert_eq!(
            value["workflows"][0]["skus"][0]["net"]
                .as_f64()
                .unwrap()
                .to_bits(),
            2.112_f64.to_bits()
        );
        assert!(value["workflows"][0]["skus"][0].get("gross").is_none());
        assert_eq!(value["workflows"][0]["findings"][0]["id"], "F1");
        assert_eq!(
            value["workflows"][0]["findings"][0]["file"],
            "app/.github/workflows/lint.yml"
        );
        assert_eq!(value["workflows"][0]["findings"][0]["line"], 8);
        assert_eq!(
            value["workflows"][0]["findings"][0]["message"],
            "lint on macOS"
        );
        assert!(value["workflows"][1]["findings"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(value["unattached_findings"][0]["id"], "F2");
        assert_eq!(
            value["unattached_findings"][0]["file"],
            "app/.github/workflows/nightly.yml"
        );
        assert_eq!(value["unattached_findings"][0]["line"], 3);
        assert_eq!(value["unattached_findings"].as_array().unwrap().len(), 1);
        assert!(value.get("budget").is_none(), "{value}");
        assert!(value.get("budget_exceeded").is_none(), "{value}");
    }

    #[test]
    fn budget_exceeded_line_rounds_to_cents() {
        assert_eq!(
            budget_exceeded_line(10.0, 9.5),
            "Budget exceeded: 10.00 > 9.50"
        );
        assert_eq!(
            budget_exceeded_line(0.005, 0.0),
            "Budget exceeded: 0.01 > 0.00"
        );
    }

    #[test]
    fn json_budget_fields_follow_the_net_total() {
        let workflows = vec![workflow("a.yml", 10.0, 1.0, "actions_linux")];

        let under = render_json(&workflows, 10.0, 1.0, None, Some(10.125));
        let under_value: serde_json::Value = serde_json::from_str(under.trim_end()).unwrap();
        assert_eq!(
            under_value["budget"].as_f64().unwrap().to_bits(),
            10.125_f64.to_bits()
        );
        assert_eq!(under_value["budget_exceeded"], false);
        assert!(under_value["budget"].is_number());
        assert_eq!(under_value["schema_version"], 1);
        assert_eq!(under_value["currency"], "USD");

        let equal = render_json(&workflows, 10.0, 1.0, None, Some(10.0));
        let equal_value: serde_json::Value = serde_json::from_str(equal.trim_end()).unwrap();
        assert_eq!(equal_value["budget_exceeded"], false);
        assert_eq!(
            equal_value["budget"].as_f64().unwrap().to_bits(),
            10.0_f64.to_bits()
        );
        assert_eq!(
            equal_value["total_net"].as_f64().unwrap().to_bits(),
            10.0_f64.to_bits()
        );

        let over = render_json(&workflows, 10.0, 1.0, None, Some(9.5));
        let over_value: serde_json::Value = serde_json::from_str(over.trim_end()).unwrap();
        assert_eq!(over_value["budget_exceeded"], true);
        assert_eq!(
            over_value["budget"].as_f64().unwrap().to_bits(),
            9.5_f64.to_bits()
        );
        assert!(over_value["budget_exceeded"].is_boolean());
        assert_eq!(over_value["workflows"][0]["net"].as_f64(), Some(10.0));

        let absent = render_json(&workflows, 10.0, 1.0, None, None);
        let absent_value: serde_json::Value = serde_json::from_str(absent.trim_end()).unwrap();
        assert!(absent_value.get("budget").is_none(), "{absent_value}");
        assert!(
            absent_value.get("budget_exceeded").is_none(),
            "{absent_value}"
        );
    }
}
