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
            } => report(&usage, workflows.as_deref(), top, json),
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

fn report(usage_path: &Path, workflows: Option<&Path>, top: Option<usize>, json: bool) -> ExitCode {
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
        report::render_json(shown, total_net, total_minutes, found.as_deref())
    } else {
        report::render(shown, total_net, total_minutes, found.as_deref())
    };
    print!("{text}");
    ExitCode::SUCCESS
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
    },
}
