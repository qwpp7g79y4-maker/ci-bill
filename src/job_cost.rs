//! Billed minutes and list price for one Actions job.
//!
//! Minutes are the completion interval rounded up to a whole minute.
//! A finished job whose interval is zero still bills one minute.
//! A job that did not run, and a job with no completion time, bill zero.
//! A missing completion time is in progress. A cancelled job bills the
//! minutes it actually ran. The SKU comes from the runner labels and the
//! list price is that SKU's table rate times the billed minutes.
//! An inherited copy adds no minutes and no list price.

use crate::api::{parse_github_time, ApiData, Job};
use crate::money::Micros;
use crate::prices::{embedded_table, sku_for_labels};

/// What one job adds at list price.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobBill {
    pub minutes: u64,
    pub sku: Option<String>,
    pub usd_per_minute: Option<Micros>,
    pub list_price: Micros,
    pub in_progress: bool,
}

/// Minutes and list price for `job`.
///
/// Inherited copies are zero even when the timestamps span a billable interval.
pub fn bill_job(api: &ApiData, job: &Job) -> JobBill {
    let timing = job_timing(job);
    let sku = sku_for_labels(&job.labels);
    let usd_per_minute = sku.as_deref().and_then(table_rate);
    let inherited = api.is_inherited_copy(job);
    let minutes = if inherited { 0 } else { timing.minutes };
    let list_price = if inherited || timing.in_progress {
        Micros::ZERO
    } else {
        sku.as_deref()
            .and_then(|name| price_minutes(name, minutes))
            .unwrap_or(Micros::ZERO)
    };
    JobBill {
        minutes,
        sku,
        usd_per_minute,
        list_price,
        in_progress: timing.in_progress,
    }
}

/// `minutes` times the table rate for `sku`.
fn price_minutes(sku: &str, minutes: u64) -> Option<Micros> {
    table_rate(sku).map(|rate| multiply_minutes(minutes, rate))
}

fn table_rate(sku: &str) -> Option<Micros> {
    embedded_table()
        .ok()
        .and_then(|table| table.get(sku).map(|price| price.usd_per_minute))
}

fn multiply_minutes(minutes: u64, rate: Micros) -> Micros {
    let minutes = i64::try_from(minutes).unwrap_or(i64::MAX);
    match rate.0.checked_mul(minutes) {
        Some(product) => Micros(product),
        None => Micros(i64::MAX),
    }
}

struct Timing {
    minutes: u64,
    in_progress: bool,
}

fn job_timing(job: &Job) -> Timing {
    if !job.counts() {
        return Timing {
            minutes: 0,
            in_progress: true,
        };
    }
    if conclusion_is(job, "skipped") {
        return Timing {
            minutes: 0,
            in_progress: false,
        };
    }
    let elapsed = elapsed_nanos(job);
    if conclusion_is(job, "cancelled") {
        return Timing {
            minutes: elapsed.map(ceil_minutes).unwrap_or(0),
            in_progress: false,
        };
    }
    let minutes = match elapsed {
        Some(nanos) if nanos > 0 => ceil_minutes(nanos),
        _ => 1,
    };
    Timing {
        minutes,
        in_progress: false,
    }
}

fn conclusion_is(job: &Job, word: &str) -> bool {
    job.conclusion
        .as_deref()
        .is_some_and(|raw| raw.trim().eq_ignore_ascii_case(word))
}

fn elapsed_nanos(job: &Job) -> Option<i128> {
    let start = job.started_at.as_deref().and_then(parse_github_time)?;
    let end = job.completed_at.as_deref().and_then(parse_github_time)?;
    Some(end.saturating_sub(start))
}

/// Whole minutes, rounding any partial minute up. A non-positive interval is 0.
fn ceil_minutes(elapsed_ns: i128) -> u64 {
    if elapsed_ns <= 0 {
        return 0;
    }
    let minute = 60 * 1_000_000_000_i128;
    let minutes = (elapsed_ns + minute - 1) / minute;
    u64::try_from(minutes).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{ApiData, Job, WorkflowRun};
    use crate::money::Micros;

    fn job(
        name: &str,
        labels: &[&str],
        started: Option<&str>,
        completed: Option<&str>,
        conclusion: Option<&str>,
    ) -> Job {
        Job {
            id: 1,
            run_id: 1,
            run_attempt: 1,
            name: name.to_string(),
            labels: labels.iter().map(|label| (*label).to_string()).collect(),
            started_at: started.map(str::to_string),
            completed_at: completed.map(str::to_string),
            conclusion: conclusion.map(str::to_string),
            runner_name: None,
        }
    }

    fn alone(job: Job) -> (ApiData, Job) {
        let api = ApiData {
            runs: vec![WorkflowRun {
                id: job.run_id,
                path: ".github/workflows/ci.yml".to_string(),
                repository: None,
                run_attempt: job.run_attempt,
                run_number: 1,
                event: "push".to_string(),
                conclusion: Some("success".to_string()),
                run_started_at: Some("2026-01-01T00:00:00Z".to_string()),
            }],
            jobs: vec![job.clone()],
        };
        (api, job)
    }

    fn bill(
        labels: &[&str],
        started: Option<&str>,
        completed: Option<&str>,
        conclusion: Option<&str>,
    ) -> JobBill {
        let (api, job) = alone(job("build", labels, started, completed, conclusion));
        bill_job(&api, &job)
    }

    fn assert_minutes(got: &JobBill, minutes: u64, in_progress: bool) {
        assert_eq!(got.minutes, minutes, "{got:?}");
        assert_eq!(got.in_progress, in_progress, "{got:?}");
    }

    #[test]
    fn sixty_one_seconds_bill_two_minutes_and_sixty_bill_one() {
        let start = "2026-04-01T00:00:00Z";
        let cases = [
            ("2026-04-01T00:01:01Z", "success", 2),
            ("2026-04-01T00:01:01Z", "failure", 2),
            ("2026-04-01T00:01:01Z", "SUCCESS", 2),
            ("2026-04-01T00:01:00.000000001Z", "success", 2),
            ("2026-04-01T00:01:00Z", "success", 1),
            ("2026-04-01T00:01:00Z", "failure", 1),
            ("2026-04-01T00:00:59Z", "success", 1),
            ("2026-04-01T00:02:00Z", "success", 2),
            ("2026-04-01T00:01:59Z", "success", 2),
        ];
        for (end, conclusion, minutes) in cases {
            let got = bill(&["ubuntu-latest"], Some(start), Some(end), Some(conclusion));
            assert_minutes(&got, minutes, false);
        }
        let zoned = bill(
            &["ubuntu-latest"],
            Some("2026-04-01T02:00:00+02:00"),
            Some("2026-04-01T00:01:01Z"),
            Some("success"),
        );
        assert_minutes(&zoned, 2, false);
        let zoned_minute = bill(
            &["ubuntu-latest"],
            Some("2026-04-01T02:00:00+02:00\r"),
            Some("2026-04-01T00:01:00Z"),
            Some("success"),
        );
        assert_minutes(&zoned_minute, 1, false);
    }

    #[test]
    fn a_finished_job_of_zero_seconds_bills_one_minute() {
        let instant = "2026-06-01T12:00:00Z";
        for conclusion in ["success", "failure", "neutral", "timed_out", "SUCCESS", ""] {
            let conclusion = if conclusion.is_empty() {
                None
            } else {
                Some(conclusion)
            };
            let got = bill(&["ubuntu-latest"], Some(instant), Some(instant), conclusion);
            assert_minutes(&got, 1, false);
            assert_eq!(got.list_price.to_decimal_string(), "0.006000");
        }
        let backwards = bill(
            &["ubuntu-latest"],
            Some("2026-06-01T12:00:02Z"),
            Some("2026-06-01T12:00:00Z"),
            Some("success"),
        );
        assert_minutes(&backwards, 1, false);
        let missing_start = bill(&["ubuntu-latest"], None, Some(instant), Some("success"));
        assert_minutes(&missing_start, 1, false);
        let blank_start = bill(
            &["ubuntu-latest"],
            Some("  \r"),
            Some(instant),
            Some("failure"),
        );
        assert_minutes(&blank_start, 1, false);
        let unreadable = bill(
            &["ubuntu-latest"],
            Some("yesterday"),
            Some("later"),
            Some("success"),
        );
        assert_minutes(&unreadable, 1, false);
        let one_ns = bill(
            &["ubuntu-latest"],
            Some("2026-06-01T12:00:00.000000000Z"),
            Some("2026-06-01T12:00:00.000000001Z"),
            Some("success"),
        );
        assert_minutes(&one_ns, 1, false);
    }

    #[test]
    fn a_job_that_did_not_run_bills_zero() {
        let start = "2026-04-01T00:00:00Z";
        let end = "2026-04-01T00:10:00Z";
        for conclusion in ["skipped", "Skipped", "SKIPPED", " skipped ", "skipped\r"] {
            let got = bill(&["macos-latest"], Some(start), Some(end), Some(conclusion));
            assert_minutes(&got, 0, false);
            assert_eq!(got.sku.as_deref(), Some("actions_macos"));
            assert_eq!(got.list_price, Micros::ZERO);
            assert_eq!(got.usd_per_minute.unwrap().to_decimal_string(), "0.062000");
        }
    }

    #[test]
    fn a_missing_completion_time_bills_zero_and_is_in_progress() {
        let start = Some("2026-04-01T00:00:00Z");
        let cases = [
            (start, None, None),
            (start, Some(""), None),
            (start, Some("   "), None),
            (start, Some("\r"), Some("success")),
            (None, None, None),
            (start, None, Some("skipped")),
            (start, None, Some("cancelled")),
            (start, Some(""), Some("SKIPPED")),
        ];
        for (started, completed, conclusion) in cases {
            let got = bill(&["macos-latest"], started, completed, conclusion);
            assert_minutes(&got, 0, true);
            assert_eq!(got.list_price, Micros::ZERO);
        }
    }

    #[test]
    fn a_cancelled_job_bills_the_elapsed_minutes() {
        let start = "2026-05-01T08:00:00Z";
        let cases = [
            ("2026-05-01T08:01:30Z", "cancelled", 2),
            ("2026-05-01T08:01:30Z", "Cancelled", 2),
            ("2026-05-01T08:01:30Z", "CANCELLED", 2),
            ("2026-05-01T08:01:30Z", " cancelled\r", 2),
            ("2026-05-01T08:01:01Z", "cancelled", 2),
            ("2026-05-01T08:01:00Z", "cancelled", 1),
            ("2026-05-01T08:00:00Z", "cancelled", 0),
            ("2026-05-01T07:59:00Z", "cancelled", 0),
        ];
        for (end, conclusion, minutes) in cases {
            let got = bill(
                &["windows-latest"],
                Some(start),
                Some(end),
                Some(conclusion),
            );
            assert_minutes(&got, minutes, false);
        }
        let missing = bill(&["windows-latest"], None, Some(start), Some("cancelled"));
        assert_minutes(&missing, 0, false);
        let blank = bill(
            &["windows-latest"],
            Some(""),
            Some(start),
            Some("cancelled"),
        );
        assert_minutes(&blank, 0, false);
        let ninety = bill(
            &["windows-latest"],
            Some(start),
            Some("2026-05-01T08:01:30Z"),
            Some("cancelled"),
        );
        assert_eq!(ninety.minutes, 2);
        assert_eq!(ninety.sku.as_deref(), Some("actions_windows"));
        assert_eq!(ninety.list_price.to_decimal_string(), "0.020000");
    }

    #[test]
    fn list_price_uses_billed_minutes_and_the_table_rate() {
        let linux_61 = bill(
            &["ubuntu-latest"],
            Some("2026-04-01T00:00:00Z"),
            Some("2026-04-01T00:01:01Z"),
            Some("success"),
        );
        assert_eq!(linux_61.minutes, 2);
        assert_eq!(linux_61.sku.as_deref(), Some("actions_linux"));
        assert_eq!(
            linux_61.usd_per_minute.unwrap().to_decimal_string(),
            "0.006000"
        );
        assert_eq!(linux_61.list_price.to_decimal_string(), "0.012000");

        let linux_60 = bill(
            &["ubuntu-latest"],
            Some("2026-04-01T00:00:00Z"),
            Some("2026-04-01T00:01:00Z"),
            Some("success"),
        );
        assert_eq!(linux_60.minutes, 1);
        assert_eq!(linux_60.list_price.to_decimal_string(), "0.006000");

        let rows = [
            (
                vec!["ubuntu-latest"],
                "actions_linux",
                "0.006000",
                "0.012000",
            ),
            (
                vec!["UBUNTU-LATEST"],
                "actions_linux",
                "0.006000",
                "0.012000",
            ),
            (
                vec![" ubuntu-22.04 "],
                "actions_linux",
                "0.006000",
                "0.012000",
            ),
            (
                vec!["ubuntu-slim"],
                "actions_linux_slim",
                "0.002000",
                "0.004000",
            ),
            (
                vec!["UBUNTU-SLIM"],
                "actions_linux_slim",
                "0.002000",
                "0.004000",
            ),
            (
                vec!["ubuntu-22.04-arm"],
                "actions_linux_arm",
                "0.005000",
                "0.010000",
            ),
            (
                vec!["ubuntu-24.04-arm"],
                "actions_linux_arm",
                "0.005000",
                "0.010000",
            ),
            (
                vec!["ubuntu-26.04-arm"],
                "actions_linux_arm",
                "0.005000",
                "0.010000",
            ),
            (
                vec!["windows-latest"],
                "actions_windows",
                "0.010000",
                "0.020000",
            ),
            (
                vec!["Windows-Latest"],
                "actions_windows",
                "0.010000",
                "0.020000",
            ),
            (
                vec!["windows-11-arm"],
                "actions_windows_arm",
                "0.010000",
                "0.020000",
            ),
            (
                vec!["macos-latest"],
                "actions_macos",
                "0.062000",
                "0.124000",
            ),
            (vec!["MACOS-14"], "actions_macos", "0.062000", "0.124000"),
            (
                vec!["macos-14-large"],
                "actions_macos_l",
                "0.077000",
                "0.154000",
            ),
            (
                vec!["macos-15-large"],
                "actions_macos_l",
                "0.077000",
                "0.154000",
            ),
            (
                vec!["macos-latest-large"],
                "actions_macos_l",
                "0.077000",
                "0.154000",
            ),
            (
                vec!["macos-14-xlarge"],
                "actions_macos_xl",
                "0.102000",
                "0.204000",
            ),
            (
                vec!["macos-15-xlarge"],
                "actions_macos_xl",
                "0.102000",
                "0.204000",
            ),
            (
                vec!["xcode-27-xlarge"],
                "actions_macos_xl",
                "0.102000",
                "0.204000",
            ),
            (
                vec!["ubuntu-24.04-16core"],
                "actions_linux_16_core",
                "0.042000",
                "0.084000",
            ),
            (
                vec!["UBUNTU-24.04-16CORE"],
                "actions_linux_16_core",
                "0.042000",
                "0.084000",
            ),
            (
                vec!["ubuntu-24.04-4core"],
                "actions_linux_4_core",
                "0.012000",
                "0.024000",
            ),
            (
                vec!["ubuntu-24.04-4core-arm"],
                "actions_linux_4_core_arm",
                "0.008000",
                "0.016000",
            ),
            (vec!["self-hosted"], "self_hosted", "0.000000", "0.000000"),
            (vec!["SELF-HOSTED"], "self_hosted", "0.000000", "0.000000"),
            (
                vec!["self-hosted", "macos-latest"],
                "self_hosted",
                "0.000000",
                "0.000000",
            ),
            (
                vec!["macos-latest", "SELF-HOSTED"],
                "self_hosted",
                "0.000000",
                "0.000000",
            ),
            (
                vec!["ubuntu-latest", "linux", "X64"],
                "actions_linux",
                "0.006000",
                "0.012000",
            ),
            (
                vec!["X64", "macos-latest"],
                "actions_macos",
                "0.062000",
                "0.124000",
            ),
            (
                vec!["ubuntu-latest", "ubuntu-24.04-16core"],
                "actions_linux_16_core",
                "0.042000",
                "0.084000",
            ),
            (
                vec!["ubuntu-24.04-16core", "ubuntu-latest"],
                "actions_linux_16_core",
                "0.042000",
                "0.084000",
            ),
            (
                vec!["macos-latest", "macos-14-xlarge"],
                "actions_macos_xl",
                "0.102000",
                "0.204000",
            ),
            (
                vec!["self-hosted", "ubuntu-24.04-16core"],
                "self_hosted",
                "0.000000",
                "0.000000",
            ),
        ];
        for (labels, sku, rate, list) in rows {
            let got = bill(
                &labels,
                Some("2026-04-01T00:00:00Z"),
                Some("2026-04-01T00:02:00Z"),
                Some("success"),
            );
            assert_eq!(got.minutes, 2, "{labels:?}");
            assert_eq!(got.sku.as_deref(), Some(sku), "{labels:?}");
            assert_eq!(
                got.usd_per_minute.unwrap().to_decimal_string(),
                rate,
                "{labels:?}"
            );
            assert_eq!(got.list_price.to_decimal_string(), list, "{labels:?}");
        }
    }

    #[test]
    fn empty_and_unknown_labels_have_minutes_and_a_zero_list_price() {
        for labels in [Vec::new(), vec!["custom-box"], vec!["linux", "X64"]] {
            let got = bill(
                &labels,
                Some("2026-04-01T00:00:00Z"),
                Some("2026-04-01T00:01:01Z"),
                Some("success"),
            );
            assert_minutes(&got, 2, false);
            assert_eq!(got.sku, None, "{labels:?}");
            assert_eq!(got.usd_per_minute, None, "{labels:?}");
            assert_eq!(got.list_price, Micros::ZERO, "{labels:?}");
        }
    }

    #[test]
    fn every_sku_in_the_price_file_bills_at_that_rate() {
        let file: serde_json::Value =
            serde_json::from_str(include_str!("../prices/actions.json")).unwrap();
        let skus = file["skus"].as_array().unwrap();
        assert!(skus.len() > 20, "{}", skus.len());
        for entry in skus {
            let name = entry["sku"].as_str().unwrap();
            let rate = Micros::parse(entry["usd_per_minute"].as_str().unwrap()).unwrap();
            let one = price_minutes(name, 1).unwrap_or_else(|| panic!("missing {name}"));
            let two = price_minutes(name, 2).unwrap();
            assert_eq!(one, rate, "{name}");
            assert_eq!(two.0, rate.0.checked_mul(2).unwrap(), "{name}");
            assert_eq!(price_minutes(&name.to_ascii_uppercase(), 1), Some(rate));
        }
        assert_eq!(price_minutes("actions_not_a_runner", 2), None);
    }

    #[test]
    fn an_inherited_copy_bills_zero_minutes_and_zero_list_price() {
        let mut copied = job(
            "build",
            &["ubuntu-latest"],
            Some("2026-04-01T09:00:00Z"),
            Some("2026-04-01T09:01:01Z"),
            Some("success"),
        );
        copied.id = 2;
        copied.run_attempt = 2;
        let earlier = job(
            "build",
            &["ubuntu-latest"],
            Some("2026-04-01T09:00:00Z"),
            Some("2026-04-01T09:01:01Z"),
            Some("success"),
        );
        let api = ApiData {
            runs: vec![WorkflowRun {
                id: 1,
                path: ".github/workflows/ci.yml".to_string(),
                repository: None,
                run_attempt: 2,
                run_number: 4,
                event: "push".to_string(),
                conclusion: Some("success".to_string()),
                run_started_at: Some("2026-04-01T10:00:00Z".to_string()),
            }],
            jobs: vec![earlier.clone(), copied.clone()],
        };
        let fresh = bill_job(&api, &earlier);
        assert_eq!(fresh.minutes, 2);
        assert_eq!(fresh.list_price.to_decimal_string(), "0.012000");
        let copy = bill_job(&api, &copied);
        assert_eq!(copy.minutes, 0);
        assert_eq!(copy.list_price, Micros::ZERO);
        assert_eq!(copy.sku.as_deref(), Some("actions_linux"));
        assert!(!copy.in_progress);
    }
}
