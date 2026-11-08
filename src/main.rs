use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{error::ErrorKind, Parser, Subcommand};

use ci_bill::{
    api, display, fetch, findings, fix_diff, matrix_cost, md,
    money::{DecimalError, Micros},
    overlap, prices, rank, report, rerun, savings, usage, workflow,
};

/// Write `text` to stdout. `print!`/`println!` panic when the write fails
/// (broken pipe, full disk); this treats a closed reader (e.g. `| head`) as
/// a quiet success like most Unix tools, and any other write error as a
/// clean exit instead of a panic.
fn write_stdout(text: &str) -> Result<(), ExitCode> {
    match std::io::stdout().write_all(text.as_bytes()) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => Err(ExitCode::SUCCESS),
        Err(err) => {
            eprintln!("ci-bill: could not write to stdout: {err}");
            Err(ExitCode::from(1))
        }
    }
}

/// Write the report to `--output`, or to stdout when that flag is absent.
/// A missing parent directory is not created. The failure uses exit 1, the
/// same code as a stdout write error.
fn write_report(path: Option<&Path>, text: &str) -> Result<(), ExitCode> {
    let Some(path) = path else {
        return write_stdout(text);
    };
    match std::fs::File::create(path) {
        Ok(mut file) => {
            if let Err(err) = file.write_all(text.as_bytes()) {
                return Err(report_file_error(path, err));
            }
            if let Err(err) = file.flush() {
                return Err(report_file_error(path, err));
            }
            Ok(())
        }
        Err(err) => Err(report_file_error(path, err)),
    }
}

fn report_file_error(path: &Path, err: std::io::Error) -> ExitCode {
    let shown = display::escape_display(&display::display_path(path));
    eprintln!("ci-bill: could not write report to {shown}: {err}");
    ExitCode::from(1)
}

fn main() -> ExitCode {
    match Cli::try_parse() {
        Ok(cli) => match cli.command {
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
            ),
            Command::Fetch {
                repo,
                out,
                since,
                until,
            } => match fetch::fetch(&repo, &out, since, until) {
                Ok(text) => match write_stdout(&text) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(code) => code,
                },
                Err(err) => {
                    eprintln!("ci-bill: {err}");
                    ExitCode::from(err.code())
                }
            },
        },
        Err(err) => usage_exit(err),
    }
}

fn usage_exit(err: clap::Error) -> ExitCode {
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
) -> ExitCode {
    let rows = match usage::read_usage(usage_path) {
        Ok(rows) => rows,
        Err(err) => {
            eprintln!(
                "ci-bill: {}: {err}\n  what to do: {}",
                usage_path.display(),
                err.hint()
            );
            return ExitCode::from(2);
        }
    };
    let ranking = match rank::rank(&rows, None) {
        Ok(ranking) => ranking,
        Err(err) => {
            eprintln!(
                "ci-bill: {}: {err}\n  what to do: {}",
                usage_path.display(),
                err.hint()
            );
            return ExitCode::from(2);
        }
    };
    let scanned = match workflows {
        Some(dir) => match workflow::scan_dir(dir) {
            Ok(scanned) => scanned,
            Err(err) => {
                eprintln!(
                    "ci-bill: {}: {err}\n  what to do: {}",
                    err.path.display(),
                    err.hint()
                );
                return ExitCode::from(2);
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
                eprintln!(
                    "ci-bill: {}: {err}\n  what to do: {}",
                    err.path.display(),
                    err.hint()
                );
                return ExitCode::from(2);
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
                eprintln!(
                    "ci-bill: {}: {err}\n  what to do: {}",
                    err.path.display(),
                    err.hint()
                );
                return ExitCode::from(2);
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
            eprintln!("{}", report::budget_exceeded_line(total_net, amount));
        }
        return ExitCode::from(1);
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
) -> ExitCode {
    match prices::render(json, runner_map, label, sku, usage, repository, workflow) {
        Ok(text) => match write_report(output, &text) {
            Ok(()) => ExitCode::SUCCESS,
            Err(code) => code,
        },
        Err(err) => {
            let code = err.exit_code();
            match err.hint() {
                Some(hint) => eprintln!("ci-bill: {err}\n  what to do: {hint}"),
                None => eprintln!("ci-bill: {err}"),
            }
            ExitCode::from(code)
        }
    }
}

/// Rank workflow costs from a GitHub detailed usage report.
#[derive(Debug, Parser)]
#[command(name = "ci-bill", version, about)]
pub(crate) struct Cli {
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
