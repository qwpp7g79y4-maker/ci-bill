//! Schema test for `report --json` (`schema_version` 2): the exact key set
//! and value type at each level. Money is a 6-decimal string, counts are
//! numbers. The snapshot pins the values; this pins the shape.

use serde_json::Value;
use std::process::Command;

fn keys(v: &Value) -> Vec<&str> {
    v.as_object()
        .expect("object")
        .keys()
        .map(|s| s.as_str())
        .collect()
}

fn money(v: &Value) {
    let t = v.as_str().unwrap_or_else(|| panic!("money string: {v}"));
    let (_, frac) = t.trim_start_matches('-').split_once('.').expect("decimal");
    assert_eq!(frac.len(), 6, "{t}");
}

fn amount_method(v: &Value) {
    assert_eq!(keys(v), ["amount", "method"]);
    money(&v["amount"]);
    assert!(v["method"].is_string());
}

#[test]
fn report_json_has_the_documented_keys_and_types() {
    let out = Command::new(env!("CARGO_BIN_EXE_ci-bill"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "report",
            "--usage",
            "samples/usage-sample.csv",
            "--workflows",
            "samples/.github/workflows",
            "--json",
        ])
        .output()
        .expect("run ci-bill");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("one json object");

    assert_eq!(
        keys(&v),
        [
            "currency",
            "money_unit",
            "other_products_net",
            "period",
            "repositories",
            "schema_version",
            "total_minutes",
            "total_net",
            "unattached_findings",
            "workflows"
        ]
    );
    assert_eq!(v["schema_version"], 2);
    assert!(v["currency"].is_string() && v["money_unit"].is_string());
    money(&v["other_products_net"]);
    money(&v["total_net"]);
    assert!(v["total_minutes"].is_u64());
    assert_eq!(keys(&v["period"]), ["from", "to"]);
    assert!(v["repositories"]
        .as_array()
        .unwrap()
        .iter()
        .all(Value::is_string));
    assert!(v["unattached_findings"].is_array());

    let workflows = v["workflows"].as_array().expect("workflows");
    assert!(!workflows.is_empty());
    for w in workflows {
        assert_eq!(
            keys(w),
            [
                "difference",
                "discount",
                "findings",
                "gross",
                "invoice",
                "job_costs",
                "measured",
                "minutes",
                "net",
                "path",
                "repository",
                "share",
                "skus",
                "steps"
            ]
        );
        for k in ["discount", "gross", "net", "share"] {
            money(&w[k]);
        }
        for k in ["difference", "invoice", "measured"] {
            amount_method(&w[k]);
        }
        assert!(w["minutes"].is_u64());
        assert!(w["path"].is_string() && w["repository"].is_string());
        for f in w["findings"].as_array().unwrap() {
            assert_eq!(
                keys(f),
                ["file", "id", "line", "message", "method", "savings"]
            );
            assert!(f["line"].is_u64());
            money(&f["savings"]);
        }
        for j in w["job_costs"].as_array().unwrap() {
            assert_eq!(
                keys(j),
                ["amount", "file", "job_id", "line", "method", "name", "sku"]
            );
            money(&j["amount"]);
        }
        for s in w["skus"].as_array().unwrap() {
            assert_eq!(keys(s), ["minutes", "net", "sku"]);
            money(&s["net"]);
        }
        for s in w["steps"].as_array().unwrap() {
            assert_eq!(keys(s), ["line", "name"]);
        }
    }
}
