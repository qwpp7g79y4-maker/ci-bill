mod findings;
mod rank;
mod report;
mod usage;
mod workflow;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{error::ErrorKind, Parser, Subcommand};

fn main() -> ExitCode {
    match Cli::try_parse() {
        Ok(cli) => match cli.command {
            Command::Report {
                usage,
                workflows,
                top,
                json,
                budget,
            } => report(&usage, workflows.as_deref(), top, json, budget),
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
    top: Option<usize>,
    json: bool,
    budget: Option<f64>,
) -> ExitCode {
    let rows = match usage::read_usage(usage_path) {
        Ok(rows) => rows,
        Err(err) => {
            eprintln!("ci-bill: {}: {err}", usage_path.display());
            return ExitCode::from(2);
        }
    };
    let ranked = rank::rank(&rows, None);
    let total_net: f64 = ranked.iter().map(|item| item.net).sum();
    let total_minutes: f64 = ranked.iter().map(|item| item.minutes).sum();
    let shown = match top {
        Some(limit) => &ranked[..limit.min(ranked.len())],
        None => ranked.as_slice(),
    };
    let found = match workflows {
        Some(dir) => match workflow::scan_dir(dir) {
            Ok(scanned) => Some(findings::findings(&scanned)),
            Err(err) => {
                eprintln!("ci-bill: {}: {err}", err.path.display());
                return ExitCode::from(2);
            }
        },
        None => None,
    };
    let text = if json {
        report::render_json(shown, total_net, total_minutes, found.as_deref(), budget)
    } else {
        report::render(shown, total_net, total_minutes, found.as_deref())
    };
    print!("{text}");
    // The budget is the whole net total, including workflows omitted by --top.
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

/// USD budget for the net total. Negative, non-finite, and non-numeric values are a usage error.
fn parse_budget(raw: &str) -> Result<f64, String> {
    match raw.parse::<f64>() {
        Ok(amount) if amount.is_finite() && !amount.is_sign_negative() => Ok(amount),
        _ => Err("expected a finite USD amount greater than or equal to zero".to_string()),
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
        budget: Option<f64>,
    },
}
