use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{error::ErrorKind, Parser, Subcommand};

use ci_bill::{
    api, display, fetch, findings, fix_diff, interrupt, matrix_cost, md,
    money::{DecimalError, Micros},
    overlap, prices, rank, report, rerun, savings, usage, workflow,
};

/// Path of a temporary file next to `path`, on the same filesystem, so the
/// finished report can replace `path` in one rename. SIGINT before that
/// rename deletes the temporary file and leaves `path` as it was.
fn temp_beside(path: &Path) -> PathBuf {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut name = std::ffi::OsString::from(".");
    name.push(
        path.file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("report")),
    );
    name.push(format!(".ci-bill-{}.tmp", std::process::id()));
    parent.join(name)
}

/// `Err` is the process status. Broken pipe stays success (exit 0). SIGINT
/// is 130 and prints nothing here; `main` writes the one stderr line.
fn write_result(result: std::io::Result<()>) -> Result<(), ExitCode> {
    match result {
        Ok(()) if interrupt::interrupted() => Err(ExitCode::from(interrupt::EXIT_INTERRUPTED)),
        Ok(()) => Ok(()),
        Err(_) if interrupt::interrupted() => Err(ExitCode::from(interrupt::EXIT_INTERRUPTED)),
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => Err(ExitCode::SUCCESS),
        Err(err) => {
            note(&format!("ci-bill: could not write to stdout: {err}"));
            Err(ExitCode::from(1))
        }
    }
}

/// Write `text` to stdout. `print!`/`println!` panic when the write fails
/// (broken pipe, full disk); this treats a closed reader (e.g. `| head`) as
/// a quiet success like most Unix tools, and any other write error as a
/// clean exit instead of a panic. SIGINT during the write is held back so
/// the bytes are not cut in half.
fn write_stdout(text: &str) -> Result<(), ExitCode> {
    if interrupt::interrupted() {
        return Err(ExitCode::from(interrupt::EXIT_INTERRUPTED));
    }
    let result = interrupt::with_sigint_blocked(|| {
        if interrupt::interrupted() {
            return None;
        }
        let mut out = std::io::stdout().lock();
        Some(out.write_all(text.as_bytes()).and_then(|()| out.flush()))
    });
    match result {
        None => Err(ExitCode::from(interrupt::EXIT_INTERRUPTED)),
        Some(result) => write_result(result),
    }
}

/// Write the report to `--output`, or to stdout when that flag is absent.
/// A missing parent directory is not created. The failure uses exit 1, the
/// same code as a stdout write error. SIGINT publishes nothing partial: the
/// destination is replaced only after the new bytes are complete.
fn write_report(path: Option<&Path>, text: &str) -> Result<(), ExitCode> {
    if interrupt::interrupted() {
        return Err(ExitCode::from(interrupt::EXIT_INTERRUPTED));
    }
    let Some(path) = path else {
        return write_stdout(text);
    };
    let tmp = temp_beside(path);
    let created = std::fs::File::create(&tmp).and_then(|mut file| {
        file.write_all(text.as_bytes())?;
        file.flush()
    });
    if let Err(err) = created {
        let _ = std::fs::remove_file(&tmp);
        if interrupt::interrupted() {
            return Err(ExitCode::from(interrupt::EXIT_INTERRUPTED));
        }
        return Err(report_file_error(path, err));
    }
    let renamed = interrupt::with_sigint_blocked(|| {
        if interrupt::interrupted() {
            return None;
        }
        Some(std::fs::rename(&tmp, path))
    });
    match renamed {
        None => {
            let _ = std::fs::remove_file(&tmp);
            Err(ExitCode::from(interrupt::EXIT_INTERRUPTED))
        }
        Some(Ok(())) if interrupt::interrupted() => {
            Err(ExitCode::from(interrupt::EXIT_INTERRUPTED))
        }
        Some(Ok(())) => Ok(()),
        Some(Err(_)) if interrupt::interrupted() => {
            let _ = std::fs::remove_file(&tmp);
            Err(ExitCode::from(interrupt::EXIT_INTERRUPTED))
        }
        Some(Err(err)) => {
            let _ = std::fs::remove_file(&tmp);
            Err(report_file_error(path, err))
        }
    }
}

fn report_file_error(path: &Path, err: std::io::Error) -> ExitCode {
    let shown = display::escape_display(&display::display_path(path));
    note(&format!(
        "ci-bill: could not write report to {shown}: {err}"
    ));
    ExitCode::from(1)
}

/// The `what to do` line is extra context.
///
/// A terminal shows it. A pipe or a redirect does not, so captured stderr
/// stays the error line. `--verbose` shows it anyway. `--quiet` hides it
/// even on a terminal. The report on stdout is unchanged either way.
/// Both flags together are a clap usage error (exit 3) and never reach a report.
fn show_extra(quiet: bool, verbose: bool) -> bool {
    if quiet {
        false
    } else if verbose {
        true
    } else {
        std::io::stderr().is_terminal()
    }
}

/// One stderr line. A closed stderr must not panic.
fn note(line: &str) {
    let _ = writeln!(std::io::stderr(), "{line}");
}

/// Input failure, exit 2, unless SIGINT was caught. The SIGINT line is printed
/// once, from `main`, so this stays quiet in that case.
fn input_exit(head: &str, hint: &str, extra: bool) -> ExitCode {
    if interrupt::interrupted() {
        return ExitCode::from(interrupt::EXIT_INTERRUPTED);
    }
    emit_hinted(head, hint, extra);
    ExitCode::from(2)
}

/// Writes the error line. When `extra` is set, adds the existing `what to do` line.
fn emit_hinted(head: &str, hint: &str, extra: bool) {
    if extra {
        note(&format!("{head}\n  what to do: {hint}"));
    } else {
        note(head);
    }
}

fn main() -> ExitCode {
    interrupt::install();
    let code = match Cli::try_parse() {
        Ok(cli) => {
            let extra = show_extra(cli.quiet, cli.verbose);
            let verbose = cli.verbose;
            match cli.command {
                Command::Report {
                    usage,
                    workflows,
                    runs,
                    top,
                    json,
                    budget,
                    fix_diff,
                    md,
                    output,
                } => report(
                    &usage,
                    workflows.as_deref(),
                    runs.as_deref(),
                    top,
                    json,
                    budget,
                    fix_diff,
                    md,
                    output.as_deref(),
                    extra,
                    verbose,
                ),
                Command::Prices {
                    json,
                    runner_map,
                    label,
                    sku,
                    usage,
                    repository,
                    workflow,
                    output,
                } => show_prices(
                    json,
                    runner_map.as_deref(),
                    label.as_deref(),
                    sku.as_deref(),
                    usage.as_deref(),
                    repository.as_deref(),
                    workflow.as_deref(),
                    output.as_deref(),
                    extra,
                    verbose,
                ),
                Command::Fetch {
                    repo,
                    out,
                    since,
                    until,
                } => match fetch::fetch(&repo, &out, since, until) {
                    Ok(text) => match write_stdout(&text) {
                        Ok(()) => {
                            if verbose {
                                let lines = text.lines().filter(|line| !line.is_empty()).count();
                                note(&format!("context: lines {lines}"));
                            }
                            ExitCode::SUCCESS
                        }
                        Err(code) => code,
                    },
                    Err(err) => {
                        if !interrupt::interrupted() {
                            note(&format!("ci-bill: {err}"));
                        }
                        ExitCode::from(if interrupt::interrupted() {
                            interrupt::EXIT_INTERRUPTED
                        } else {
                            err.code()
                        })
                    }
                },
            }
        }
        Err(err) => usage_exit(err),
    };
    if interrupt::interrupted() {
        note("ci-bill: interrupted by SIGINT");
        return ExitCode::from(interrupt::EXIT_INTERRUPTED);
    }
    code
}

fn usage_exit(err: clap::Error) -> ExitCode {
    if interrupt::interrupted() {
        return ExitCode::from(interrupt::EXIT_INTERRUPTED);
    }
    if matches!(
        err.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    ) {
        err.exit();
    }
    let _ = err.print();
    ExitCode::from(3)
}

#[allow(clippy::too_many_arguments)]
fn report(
    usage_path: &Path,
    workflows: Option<&Path>,
    runs: Option<&Path>,
    top: Option<usize>,
    json: bool,
    budget: Option<Micros>,
    fix_diff: bool,
    md: bool,
    output: Option<&Path>,
    extra: bool,
    verbose: bool,
) -> ExitCode {
    let rows = match usage::read_usage(usage_path) {
        Ok(rows) => rows,
        Err(err) => {
            return input_exit(
                &format!("ci-bill: {}: {err}", usage_path.display()),
                err.hint(),
                extra,
            );
        }
    };
    let ranking = match rank::rank(&rows, None) {
        Ok(ranking) => ranking,
        Err(err) => {
            return input_exit(
                &format!("ci-bill: {}: {err}", usage_path.display()),
                err.hint(),
                extra,
            );
        }
    };
    let scanned = match workflows {
        Some(dir) => match workflow::scan_dir(dir) {
            Ok(scanned) => scanned,
            Err(err) => {
                return input_exit(
                    &format!("ci-bill: {}: {err}", err.path.display()),
                    err.hint(),
                    extra,
                );
            }
        },
        None => Vec::new(),
    };
    let yaml = if workflows.is_some() {
        let result = match runs {
            Some(cache) => findings::findings_with_cache(&scanned, Some(cache)),
            None => findings::findings(&scanned),
        };
        match result {
            Ok(items) => Some(items),
            Err(err) => {
                return input_exit(
                    &format!("ci-bill: {}: {err}", err.path.display()),
                    err.hint(),
                    extra,
                );
            }
        }
    } else {
        None
    };
    // CSV net stays the total. A mismatched unit price is reported, not applied.
    let found = findings::combine(yaml, findings::invoice_findings(usage_path, &rows));
    let api = match runs {
        Some(dir) => match api::read_dir(dir) {
            Ok(data) => Some(data),
            Err(err) => {
                return input_exit(
                    &format!("ci-bill: {}: {err}", err.path.display()),
                    err.hint(),
                    extra,
                );
            }
        },
        None => None,
    };
    let ranked = ranking.workflows.as_slice();
    let mut found = findings::append(
        found,
        api.as_ref()
            .map(|data| rerun::findings(data, ranked, &scanned))
            .unwrap_or_default(),
    );
    let measured = api
        .as_ref()
        .map(|data| matrix_cost::findings(data, ranked, &scanned))
        .unwrap_or_default();
    if let Some(items) = found.as_mut() {
        matrix_cost::supersede(items, &measured);
    }
    let found = findings::append(found, measured);
    let mut found = findings::append(
        found,
        api.as_ref()
            .map(|data| overlap::findings(data, ranked, &scanned))
            .unwrap_or_default(),
    );
    if let Some(items) = found.as_mut() {
        let public_repo = api.as_ref().is_some_and(|data| data.public);
        savings::apply(items, ranked, &scanned, public_repo);
    }
    let mut text = if md {
        md::render(
            ranked,
            &ranking.bill,
            found.as_deref(),
            api.as_ref(),
            &scanned,
            workflows,
        )
    } else if fix_diff {
        found
            .as_deref()
            .and_then(|items| fix_diff::render(items, workflows))
            .unwrap_or_default()
    } else if json {
        match api.as_ref() {
            Some(data) => report::render_json_with_runs(
                ranked,
                &ranking.bill,
                found.as_deref(),
                budget,
                data,
                top,
                &scanned,
            ),
            None => report::render_json(
                ranked,
                &ranking.bill,
                found.as_deref(),
                budget,
                top,
                &scanned,
            ),
        }
    } else {
        report::render_text(
            ranked,
            &ranking.bill,
            found.as_deref(),
            api.as_ref(),
            top,
            &scanned,
        )
    };
    // The budget is the Actions net total, including workflows omitted by --top.
    let total_net = ranking.bill.total_net;
    let over_budget = budget.filter(|amount| total_net > *amount);
    if let Some(amount) = over_budget {
        if !json {
            text.push_str(&report::budget_exceeded_line(total_net, amount));
            text.push('\n');
        }
    }
    if let Err(code) = write_report(output, &text) {
        return code;
    }
    if let Some(amount) = over_budget {
        if json {
            note(&report::budget_exceeded_line(total_net, amount));
        }
        return ExitCode::from(1);
    }
    if verbose {
        let findings = found.as_ref().map(Vec::len).unwrap_or(0);
        note(&format!(
            "context: workflows {}, findings {findings}",
            ranked.len()
        ));
    }
    ExitCode::SUCCESS
}

const BUDGET_ERROR: &str =
    "expected a finite USD amount greater than or equal to zero, for example 25.50";

/// USD budget for the Actions net total.
///
/// Parsed as micro-dollars. A leading minus is rejected, including negative zero.
/// NaN, infinity, text, and values that need more than 6 decimal places are a usage error.
fn parse_budget(raw: &str) -> Result<Micros, String> {
    let text = raw.trim();
    if text.is_empty() || text.starts_with('-') {
        return Err(BUDGET_ERROR.to_string());
    }
    match Micros::parse(text) {
        Ok(amount) => Ok(amount),
        Err(DecimalError::NotANumber | DecimalError::TooManyDecimals) => {
            Err(BUDGET_ERROR.to_string())
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn show_prices(
    json: bool,
    runner_map: Option<&Path>,
    label: Option<&str>,
    sku: Option<&str>,
    usage: Option<&Path>,
    repository: Option<&str>,
    workflow: Option<&str>,
    output: Option<&Path>,
    extra: bool,
    verbose: bool,
) -> ExitCode {
    match prices::render(json, runner_map, label, sku, usage, repository, workflow) {
        Ok(text) => match write_report(output, &text) {
            Ok(()) => {
                if verbose {
                    let lines = text.lines().filter(|line| !line.is_empty()).count();
                    note(&format!("context: lines {lines}"));
                }
                ExitCode::SUCCESS
            }
            Err(code) => code,
        },
        Err(err) => {
            if interrupt::interrupted() {
                return ExitCode::from(interrupt::EXIT_INTERRUPTED);
            }
            let code = err.exit_code();
            match err.hint() {
                Some(hint) => emit_hinted(&format!("ci-bill: {err}"), hint, extra),
                None => note(&format!("ci-bill: {err}")),
            }
            ExitCode::from(code)
        }
    }
}

/// Rank workflow costs from a GitHub detailed usage report.
#[derive(Debug, Parser)]
#[command(name = "ci-bill", version, about)]
pub(crate) struct Cli {
    /// Hide the `what to do` line. No context line.
    #[arg(long, global = true, conflicts_with = "verbose")]
    quiet: bool,
    /// Print the `what to do` line, and on success one context line on stderr.
    #[arg(long, global = true, conflicts_with = "quiet")]
    verbose: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Rank workflow costs from a detailed usage report.
    Report {
        /// Detailed usage report CSV. `-` reads standard input.
        #[arg(long, value_name = "CSV")]
        usage: PathBuf,
        /// Directory of workflow YAML files.
        #[arg(long, value_name = "DIR")]
        workflows: Option<PathBuf>,
        /// Saved Actions API JSON (`runs-*.json` and `jobs-<run_id>*.json`)
        /// and cached remote workflows at `<owner>/<repo>/<path>@<ref>`.
        #[arg(long, value_name = "DIR")]
        runs: Option<PathBuf>,
        /// Show the N workflows with the highest net amount.
        #[arg(long, value_name = "N")]
        top: Option<usize>,
        /// Print one JSON object instead of the text report.
        #[arg(long)]
        json: bool,
        /// Net total budget in USD. Exit 1 when the total is above this.
        #[arg(
            long,
            value_name = "AMOUNT",
            value_parser = parse_budget,
            allow_hyphen_values = true
        )]
        budget: Option<Micros>,
        /// Print a unified diff fixing the highest-savings finding instead of the report.
        #[arg(long)]
        fix_diff: bool,
        /// Print a one-page Markdown teardown instead of the report.
        #[arg(long)]
        md: bool,
        /// Write the report to this file instead of standard output.
        /// An existing file is replaced. A missing parent directory is an error.
        #[arg(long, value_name = "PATH")]
        output: Option<PathBuf>,
    },
    /// Show Actions runner prices and the minute rate for a label or SKU.
    Prices {
        /// Print one JSON object instead of the text table.
        #[arg(long)]
        json: bool,
        /// CSV of `label,sku` rows. Matching labels use this SKU instead of the built-in map.
        /// `-` reads standard input (not if `--usage` is also `-`).
        #[arg(long, value_name = "CSV")]
        runner_map: Option<PathBuf>,
        /// Runner label or list, for example `ubuntu-latest` or `[self-hosted, macos]`.
        #[arg(long, value_name = "LABEL")]
        label: Option<String>,
        /// Actions SKU. With `--label`, it must be the SKU that label resolves to.
        #[arg(long, value_name = "SKU")]
        sku: Option<String>,
        /// Detailed usage report. Own rows for the workflow and SKU replace the table rate.
        /// `-` reads standard input (not if `--runner-map` is also `-`).
        #[arg(long, value_name = "CSV")]
        usage: Option<PathBuf>,
        /// Repository (`owner/repo`) that owns the usage rows.
        #[arg(long, value_name = "OWNER/REPO")]
        repository: Option<String>,
        /// Workflow path inside the usage report.
        #[arg(long, value_name = "PATH")]
        workflow: Option<String>,
        /// Write the report to this file instead of standard output.
        /// An existing file is replaced. A missing parent directory is an error.
        #[arg(long, value_name = "PATH")]
        output: Option<PathBuf>,
    },
    /// Save Actions runs, jobs, and referenced reusable workflows.
    Fetch {
        /// Repository to read, as `OWNER/REPO`.
        #[arg(long, value_name = "OWNER/REPO", value_parser = fetch::parse_repo_arg)]
        repo: String,
        /// Directory for `repo.json`, `runs-*.json`, `jobs-<run_id>*.json`, and callee files.
        #[arg(long, value_name = "DIR")]
        out: PathBuf,
        /// First day to fetch, as `YYYY-MM-DD` in UTC.
        #[arg(long, value_name = "DATE", value_parser = fetch::parse_utc_date)]
        since: fetch::UtcDate,
        /// Last day to fetch, as `YYYY-MM-DD` in UTC. Defaults to today.
        #[arg(long, value_name = "DATE", value_parser = fetch::parse_utc_date)]
        until: Option<fetch::UtcDate>,
    },
}
