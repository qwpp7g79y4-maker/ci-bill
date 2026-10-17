mod api;
mod findings;
mod job_cost;
mod money;
mod prices;
mod rank;
mod reconcile;
mod report;
mod rerun;
mod usage;
mod workflow;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{error::ErrorKind, Parser, Subcommand};

use crate::money::{DecimalError, Micros};

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
            } => report(
                &usage,
                workflows.as_deref(),
                runs.as_deref(),
                top,
                json,
                budget,
            ),
            Command::Prices {
                json,
                runner_map,
                label,
                sku,
                usage,
                repository,
                workflow,
            } => show_prices(
                json,
                runner_map.as_deref(),
                label.as_deref(),
                sku.as_deref(),
                usage.as_deref(),
                repository.as_deref(),
                workflow.as_deref(),
            ),
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

fn report(
    usage_path: &Path,
    workflows: Option<&Path>,
    runs: Option<&Path>,
    top: Option<usize>,
    json: bool,
    budget: Option<Micros>,
) -> ExitCode {
    let rows = match usage::read_usage(usage_path) {
        Ok(rows) => rows,
        Err(err) => {
            eprintln!("ci-bill: {}: {err}", usage_path.display());
            return ExitCode::from(2);
        }
    };
    let ranking = rank::rank(&rows, None);
    let scanned = match workflows {
        Some(dir) => match workflow::scan_dir(dir) {
            Ok(scanned) => scanned,
            Err(err) => {
                eprintln!("ci-bill: {}: {err}", err.path.display());
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
                eprintln!("ci-bill: {}: {err}", err.path.display());
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
                eprintln!("ci-bill: {}: {err}", err.path.display());
                return ExitCode::from(2);
            }
        },
        None => None,
    };
    let ranked = ranking.workflows.as_slice();
    let found = findings::append(
        found,
        api.as_ref()
            .map(|data| rerun::findings(data, ranked, &scanned))
            .unwrap_or_default(),
    );
    let text = if json {
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
    print!("{text}");
    // The budget is the Actions net total, including workflows omitted by --top.
    let total_net = ranking.bill.total_net;
    if let Some(amount) = budget.filter(|amount| total_net > *amount) {
        let line = report::budget_exceeded_line(total_net, amount);
        if json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

const BUDGET_ERROR: &str = "expected a finite USD amount greater than or equal to zero";

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

fn show_prices(
    json: bool,
    runner_map: Option<&Path>,
    label: Option<&str>,
    sku: Option<&str>,
    usage: Option<&Path>,
    repository: Option<&str>,
    workflow: Option<&str>,
) -> ExitCode {
    match prices::render(json, runner_map, label, sku, usage, repository, workflow) {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            let code = err.exit_code();
            eprintln!("ci-bill: {err}");
            ExitCode::from(code)
        }
    }
}

/// Rank workflow costs from a GitHub detailed usage report.
#[derive(Debug, Parser)]
#[command(name = "ci-bill", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Rank workflow costs from a detailed usage report.
    Report {
        /// Detailed usage report CSV.
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
    },
    /// Show Actions runner prices and the minute rate for a label or SKU.
    Prices {
        /// Print one JSON object instead of the text table.
        #[arg(long)]
        json: bool,
        /// CSV of `label,sku` rows. Matching labels use this SKU instead of the built-in map.
        #[arg(long, value_name = "CSV")]
        runner_map: Option<PathBuf>,
        /// Runner label or list, for example `ubuntu-latest` or `[self-hosted, macos]`.
        #[arg(long, value_name = "LABEL")]
        label: Option<String>,
        /// Actions SKU. With `--label`, it must be the SKU that label resolves to.
        #[arg(long, value_name = "SKU")]
        sku: Option<String>,
        /// Detailed usage report. Own rows for the workflow and SKU replace the table rate.
        #[arg(long, value_name = "CSV")]
        usage: Option<PathBuf>,
        /// Repository (`owner/repo`) that owns the usage rows.
        #[arg(long, value_name = "OWNER/REPO")]
        repository: Option<String>,
        /// Workflow path inside the usage report.
        #[arg(long, value_name = "PATH")]
        workflow: Option<String>,
    },
}
