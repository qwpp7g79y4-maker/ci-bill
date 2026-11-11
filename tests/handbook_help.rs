//! The handbook matches the built binary: `--help` against `man/ci-bill.1`,
//! and the command documented in `examples/README.md`.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::{Command, Output};

/// Cargo exposes the built binary as `CARGO_BIN_EXE_ci_bill` or, when the
/// target name keeps its hyphen, as `CARGO_BIN_EXE_ci-bill`.
fn ci_bill() -> Command {
    let path = option_env!("CARGO_BIN_EXE_ci_bill").unwrap_or(env!("CARGO_BIN_EXE_ci-bill"));
    Command::new(path)
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("utf-8 stdout")
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Long options are tokens that start with `--` and continue with letters,
/// digits, or hyphens. A preceding letter or digit sticks to the token, so a
/// font escape does not hide an option and a word does not invent one.
fn long_options(text: &str) -> BTreeSet<String> {
    let bytes = text.as_bytes();
    let mut found = BTreeSet::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            let start = i;
            let preceded_by_word = start > 0 && bytes[start - 1].is_ascii_alphanumeric();
            i += 2;
            let name = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-') {
                i += 1;
            }
            if !preceded_by_word && i > name {
                found.insert(text[start..i].to_string());
            }
        } else {
            i += 1;
        }
    }
    found
}

fn subcommand_names(help: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_commands = false;
    for line in help.lines() {
        if !in_commands {
            if line == "Commands:" {
                in_commands = true;
            }
            continue;
        }
        if line.trim().is_empty() {
            break;
        }
        // Clap indents a command name by two spaces. A wrapped description
        // is indented further and is not another command.
        let Some(rest) = line.strip_prefix("  ") else {
            continue;
        };
        if rest.starts_with(' ') {
            continue;
        }
        let name = rest.split_whitespace().next().expect("command name");
        names.push(name.to_string());
    }
    assert!(
        !names.is_empty(),
        "top-level help listed no commands:\n{help}"
    );
    names
}

/// Top-level `--help`, plus `--help` of every command that text names.
/// `help` has no flags; `ci-bill help help` is the text that prints.
fn help_corpus() -> String {
    let top = ci_bill().arg("--help").output().expect("run --help");
    assert!(
        top.status.success(),
        "ci-bill --help failed\n{}",
        stderr_of(&top)
    );
    let top_text = stdout_of(&top);
    let mut corpus = top_text.clone();
    for name in subcommand_names(&top_text) {
        let direct = ci_bill()
            .args([&name, "--help"])
            .output()
            .expect("subcommand --help");
        if direct.status.success() {
            corpus.push_str(&stdout_of(&direct));
            continue;
        }
        let via = ci_bill()
            .args(["help", &name])
            .output()
            .expect("help subcommand");
        assert!(
            via.status.success(),
            "ci-bill {name} --help and ci-bill help {name} both failed\n{}",
            stderr_of(&via)
        );
        corpus.push_str(&stdout_of(&via));
    }
    corpus
}

fn manifest_file(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

#[test]
fn help_options_match_man_page() {
    let from_help = long_options(&help_corpus());
    // Roff may spell a long option as \-\-option. Treat that as --option.
    let man = manifest_file("man/ci-bill.1").replace("\\-\\-", "--");
    let from_man = long_options(&man);

    let missing: Vec<_> = from_help.difference(&from_man).cloned().collect();
    let extra: Vec<_> = from_man.difference(&from_help).cloned().collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "man page long options differ from --help\nmissing from man: {missing:?}\nonly in man: {extra:?}"
    );

    for section in [
        "NAME",
        "SYNOPSIS",
        "DESCRIPTION",
        "OPTIONS",
        "EXIT STATUS",
        "EXAMPLES",
    ] {
        assert!(
            man.contains(&format!(".SH {section}")),
            "man/ci-bill.1 is missing .SH {section}"
        );
    }
}

#[test]
fn unknown_option_is_not_a_panic() {
    let output = ci_bill()
        .arg("--no-such-option")
        .output()
        .expect("run unknown option");
    assert_ne!(output.status.code(), Some(0), "{}", stderr_of(&output));
    assert!(
        !stderr_of(&output).contains("panicked"),
        "{}",
        stderr_of(&output)
    );
}

fn documented_examples(text: &str) -> Vec<(Vec<String>, i32)> {
    let mut examples = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("```sh\n") {
        rest = &rest[start + "```sh\n".len()..];
        let end = rest
            .find("```")
            .expect("examples/README.md: unclosed sh fence");
        let command = rest[..end].trim();
        rest = &rest[end + "```".len()..];
        assert!(
            command.starts_with("ci-bill "),
            "example command does not start with `ci-bill `: {command}"
        );
        assert!(
            !command.contains('\n'),
            "example command must be one line: {command}"
        );
        let marker = "Expected exit code: ";
        let pos = rest
            .find(marker)
            .expect("examples/README.md: missing expected exit code");
        let after = &rest[pos + marker.len()..];
        let digits_end = after
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(after.len());
        let code: i32 = after[..digits_end]
            .parse()
            .expect("examples/README.md: exit code is not a number");
        let args = command["ci-bill ".len()..]
            .split_whitespace()
            .map(str::to_string)
            .collect();
        examples.push((args, code));
    }
    assert!(
        !examples.is_empty(),
        "examples/README.md has no ```sh command"
    );
    examples
}

#[test]
fn example_command_exits_as_documented() {
    let text = manifest_file("examples/README.md");
    for (args, code) in documented_examples(&text) {
        let output = ci_bill()
            .args(&args)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .expect("run documented example");
        assert_eq!(
            output.status.code(),
            Some(code),
            "command: ci-bill {}\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            stdout_of(&output),
            stderr_of(&output)
        );
    }
}

/// Every long option and short flag in any `--help` appears in both READMEs.
#[test]
fn every_help_option_is_in_both_readmes() {
    let mut options = BTreeSet::new();
    for args in [
        vec!["--help"],
        vec!["report", "--help"],
        vec!["prices", "--help"],
        vec!["fetch", "--help"],
    ] {
        let output = ci_bill().args(&args).output().expect("run ci-bill");
        assert!(output.status.success(), "{}", stderr_of(&output));
        options.extend(long_options(&stdout_of(&output)));
    }
    assert!(options.contains("--usage") && options.contains("--until"));
    for file in ["README.md", "README.nl.md"] {
        let text = manifest_file(file);
        for option in &options {
            assert!(
                text.contains(&format!("`{option}")),
                "{file} does not mention {option}"
            );
        }
        assert!(text.contains("`-V`"), "{file} does not mention -V");
        assert!(text.contains("`-h`"), "{file} does not mention -h");
    }
}
