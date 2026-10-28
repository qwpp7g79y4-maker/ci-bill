//! A workflow file with enough nested block sequences must be rejected as
//! a clean parse error, not crash the process (`collect_events` called
//! `yaml_rust2::Parser::load` with no depth limit; past ~19k levels of
//! nesting it overflows the thread stack and aborts with SIGABRT, which
//! `catch_unwind` cannot intercept).

use std::fs;
use std::process::Command;

#[test]
fn deeply_nested_block_sequence_is_a_clean_error_not_a_crash() {
    let dir = std::env::temp_dir().join("ci-bill-depth-regression");
    let workflows = dir.join(".github/workflows");
    fs::create_dir_all(&workflows).expect("scratch dir");
    fs::write(
        workflows.join("ci.yml"),
        format!("on:\n{}1\n", "- ".repeat(20_000)),
    )
    .expect("write workflow");
    let usage = dir.join("usage.csv");
    fs::write(&usage, "date,product,sku,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,net_amount,username,organization,repository,workflow_path,cost_center_name\n")
        .expect("write usage");

    let output = Command::new(env!("CARGO_BIN_EXE_ci-bill"))
        .args(["report", "--usage"])
        .arg(&usage)
        .args(["--workflows"])
        .arg(&workflows)
        .output()
        .expect("run ci-bill");

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            output.status.signal(),
            None,
            "ci-bill was killed by a signal instead of exiting cleanly: {:?}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
        );
    }
    assert!(
        !output.status.success(),
        "a 20k-deep workflow should be rejected, not accepted"
    );

    let _ = fs::remove_dir_all(&dir);
}
