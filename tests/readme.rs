use std::process::Command;

const README: &str = include_str!("../README.md");

/// The text inside the first fenced block whose info string is `lang`.
fn fenced_block(text: &str, lang: &str) -> String {
    let open = format!("```{lang}\n");
    let start = text.find(&open).unwrap_or_else(|| {
        panic!("README.md: no ```{lang} block");
    });
    let after_open = &text[start + open.len()..];
    let end = after_open.find("```").unwrap_or_else(|| {
        panic!("README.md: ```{lang} block has no closing fence");
    });
    after_open[..end].to_string()
}

/// `cargo run --quiet -- report ...` with everything up to `--` stripped, as
/// the plain argument list the compiled binary takes.
fn example_args(command_line: &str) -> Vec<&str> {
    let marker = "-- ";
    let start = command_line
        .find(marker)
        .unwrap_or_else(|| panic!("README.md example command has no `-- ` marker: {command_line}"))
        + marker.len();
    command_line[start..].split_whitespace().collect()
}

#[test]
fn example_command_output_matches_real_output() {
    let example_section = README
        .split("## Example")
        .nth(1)
        .expect("README.md has an \"## Example\" section");

    let command_block = fenced_block(example_section, "sh");
    let command_line = command_block.trim();
    let args = example_args(command_line);

    let expected = fenced_block(example_section, "text");

    let output = Command::new(env!("CARGO_BIN_EXE_ci-bill"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(&args)
        .output()
        .expect("run ci-bill");
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "command: {command_line}\nstderr:\n{stderr}\nstdout:\n{stdout}"
    );
    assert!(stderr.is_empty(), "{stderr}");
    assert_eq!(stdout, expected, "command: {command_line}");
}

/// The `## Quick start` example must work with nothing but the installed
/// binary: no cloned repository, no `samples/` directory. It is run in a
/// fresh temporary directory, not `CARGO_MANIFEST_DIR`, to prove that.
#[test]
fn quick_start_works_without_cloning_the_repository() {
    let quick_start_section = README
        .split("## Quick start")
        .nth(1)
        .expect("README.md has a \"## Quick start\" section");

    let command_block = fenced_block(quick_start_section, "sh");
    let expected = fenced_block(quick_start_section, "text");

    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("quick-start");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mut lines = command_block.lines();
    let mut command_line = None;
    while let Some(line) = lines.next() {
        if let Some(rest) = line.strip_prefix("cat > ") {
            let (filename, marker) = rest.split_once(" <<'").unwrap_or_else(|| {
                panic!("README.md quick start heredoc has no `cat > FILE <<'DELIM'` line: {line}")
            });
            let delimiter = marker.trim_end_matches('\'');
            let mut content = String::new();
            for body_line in lines.by_ref() {
                if body_line == delimiter {
                    break;
                }
                content.push_str(body_line);
                content.push('\n');
            }
            std::fs::write(dir.join(filename), content).unwrap();
        } else if !line.trim().is_empty() {
            command_line = Some(line);
        }
    }
    let command_line =
        command_line.expect("README.md quick start has no command after the heredoc");
    let args: Vec<&str> = command_line
        .strip_prefix("ci-bill ")
        .unwrap_or_else(|| {
            panic!("README.md quick start command does not start with `ci-bill `: {command_line}")
        })
        .split_whitespace()
        .collect();

    let output = Command::new(env!("CARGO_BIN_EXE_ci-bill"))
        .current_dir(&dir)
        .args(&args)
        .output()
        .expect("run ci-bill");
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "command: {command_line}\nstderr:\n{stderr}\nstdout:\n{stdout}"
    );
    assert!(stderr.is_empty(), "{stderr}");
    assert_eq!(stdout, expected, "command: {command_line}");
}

#[test]
fn readme_names_both_input_sources() {
    assert!(README.contains("--usage"), "{README}");
    assert!(README.contains("--runs"), "{README}");
    assert!(README.contains("ci-bill fetch"), "{README}");
}

#[test]
fn readme_explains_where_the_usage_csv_comes_from() {
    assert!(
        README.contains("detailed usage report") && README.contains("billing settings"),
        "{README}"
    );
}

#[test]
fn readme_names_the_billing_page_alternative() {
    assert!(README.contains("Billing page"), "{README}");
}

#[test]
fn readme_lists_all_findings() {
    for id in ["F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9"] {
        assert!(
            README.contains(&format!("**{id}**")),
            "README.md is missing finding {id}"
        );
    }
}

#[test]
fn readme_explains_measured_vs_invoice() {
    assert!(README.contains("## Measured vs invoice"), "{README}");
}

#[test]
fn readme_documents_prices_command() {
    assert!(README.contains("ci-bill prices"), "{README}");
}

#[test]
fn readme_lists_exit_codes_0_through_3() {
    for code in ["| 0 ", "| 1 ", "| 2 ", "| 3 "] {
        assert!(
            README.contains(code),
            "README.md is missing exit code {code}"
        );
    }
}

#[test]
fn readme_lists_known_limitations() {
    let section = README
        .split("## Known limitations")
        .nth(1)
        .expect("README.md has a \"## Known limitations\" section");
    for id in ["**F4**", "**F5**", "Other products"] {
        assert!(section.contains(id), "Known limitations is missing {id}");
    }
}
