//! Writes the bash and zsh completions from the real CLI definition, so they
//! cannot drift from `--help`. The manual page `man/ci-bill.1` is kept by
//! hand and checked by `tests/handbook_help.rs`; this does not touch it.
//!
//! Run from the repository root:
//!
//! ```text
//! cargo run --example gen_completions -- target/completions
//! ```
//!
//! It writes `DIR/ci-bill.bash` and `DIR/_ci-bill`.
#![allow(dead_code)]

#[path = "../src/main.rs"]
mod app;

use std::path::Path;

use clap::CommandFactory;
use clap_complete::{generate_to, Shell};

fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let (Some(dir), None) = (args.next(), args.next()) else {
        eprintln!("usage: gen_completions DIR");
        std::process::exit(2);
    };
    std::fs::create_dir_all(&dir)?;
    let mut cmd = app::Cli::command();
    for shell in [Shell::Bash, Shell::Zsh] {
        generate_to(shell, &mut cmd, "ci-bill", Path::new(&dir))?;
    }
    Ok(())
}
