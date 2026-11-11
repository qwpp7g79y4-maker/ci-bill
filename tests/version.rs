//! `-V` and `--version` print crate version 1.0.0 and exit 0.
//! A debug build also prints the git commit when git itself provides one.
//! A release build prints the crate version and does not print that commit.
//! The flag names stay `-V` and `--version`. A subcommand still rejects them.

use std::process::Command;

fn git_sha() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let sha = text.trim();
    if sha.len() == 40 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Some(sha.to_string())
    } else {
        None
    }
}

fn run(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_ci-bill"))
        .args(args)
        .output()
        .expect("run ci-bill");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8(output.stdout).expect("utf-8 stdout"),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn one_line(stdout: &str) -> &str {
    stdout.trim_end_matches(['\r', '\n'])
}

#[test]
fn version_flags_print_crate_version() {
    let sha = git_sha();
    let expected = if cfg!(debug_assertions) {
        match &sha {
            Some(sha) => format!("ci-bill 1.0.0 ({sha})"),
            None => "ci-bill 1.0.0".to_string(),
        }
    } else {
        "ci-bill 1.0.0".to_string()
    };
    for flag in ["--version", "-V"] {
        let (code, stdout, stderr) = run(&[flag]);
        assert_eq!(code, 0, "{flag}\n{stderr}");
        assert!(stderr.is_empty(), "{flag}\n{stderr}");
        assert_eq!(one_line(&stdout), expected, "{flag}\n{stdout:?}");
        assert!(
            stdout.ends_with('\n'),
            "{flag} stdout does not end with a newline: {stdout:?}"
        );
        if !cfg!(debug_assertions) {
            if let Some(sha) = &sha {
                assert!(
                    !stdout.contains(sha),
                    "{flag} release build printed the git commit: {stdout}"
                );
            }
        }
    }

    let (help_code, help, help_err) = run(&["--help"]);
    assert_eq!(help_code, 0, "{help_err}");
    assert!(
        help.contains("-V, --version"),
        "version flags were renamed:\n{help}"
    );

    let (sub_code, sub_out, sub_err) = run(&["report", "--version"]);
    assert_eq!(sub_code, 3, "{sub_err}");
    assert!(
        !one_line(&sub_out).starts_with("ci-bill 1.0.0"),
        "report --version printed the version:\n{sub_out}"
    );
}
