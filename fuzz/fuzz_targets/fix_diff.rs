//! `--fix-diff`: a diff that `git apply --check` accepts must leave the
//! patched workflow file as valid YAML (CB-3: flow-style jobs used to pass
//! that check while the patch corrupted the file).
//!
//! Applies the diff with the real `git apply` in a scratch directory, the
//! same tool the CLI's own invariant relies on, then re-parses the result.

#![no_main]

use std::path::Path;
use std::process::Command;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let path = Path::new(".github/workflows/ci.yml");
    let Ok(workflow) = ci_bill::workflow::scan_yaml(path, text) else {
        return;
    };
    let Ok(found) = ci_bill::findings::findings(std::slice::from_ref(&workflow)) else {
        return;
    };

    let dir = std::env::temp_dir().join(format!("ci-bill-fuzz-fix-diff-{}", std::process::id()));
    let workflows_dir = dir.join(".github/workflows");
    std::fs::create_dir_all(&workflows_dir).expect("scratch dir");
    let file_path = dir.join(path);
    std::fs::write(&file_path, text).expect("write workflow");

    let Some(diff) = ci_bill::fix_diff::render(&found, Some(&workflows_dir)) else {
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };
    let diff_path = dir.join("fix.diff");
    std::fs::write(&diff_path, &diff).expect("write diff");

    let check = Command::new("git")
        .current_dir(&dir)
        .args(["apply", "--check", "fix.diff"])
        .output()
        .expect("run git apply --check");
    if check.status.success() {
        let apply = Command::new("git")
            .current_dir(&dir)
            .args(["apply", "fix.diff"])
            .output()
            .expect("run git apply");
        assert!(
            apply.status.success(),
            "git apply --check passed but apply failed"
        );
        let patched = std::fs::read_to_string(&file_path).expect("read patched workflow");
        assert!(
            ci_bill::workflow::scan_yaml(path, &patched).is_ok(),
            "git apply --check accepted a diff that left invalid YAML:\n\
             original:\n{text}\n\ndiff:\n{diff}\n\npatched:\n{patched}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
});
