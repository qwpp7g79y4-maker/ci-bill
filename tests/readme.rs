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

#[test]
fn readme_names_both_input_sources() {
    assert!(README.contains("--usage"), "{README}");
    assert!(README.contains("--runs"), "{README}");
    assert!(README.contains("ci-bill fetch"), "{README}");
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
