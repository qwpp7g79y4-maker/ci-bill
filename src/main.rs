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
            } => report(&usage, workflows.as_deref(), top),
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

fn report(usage: &Path, workflows: Option<&Path>, top: Option<usize>) -> ExitCode {
    let _ = (usage, workflows, top);
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
    },
}
