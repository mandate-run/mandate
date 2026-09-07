//! `proctor` command line. Commands are stubs until slice D in issue #2 lands.

mod agent;
mod checks;
mod hedera;
mod hooks;
mod journal;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Builds and checks Hedera services against executable task contracts.
#[derive(Parser)]
#[command(name = "proctor", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run one task's checks against the current worktree and report the outcome.
    Check {
        /// Path to the task contract, TOML.
        task: PathBuf,
    },
    /// Drive the agent adapter through attempts until the task passes or attempts run out.
    Run {
        /// Path to the task contract, TOML.
        task: PathBuf,
        /// Maximum attempts.
        #[arg(long, default_value_t = 3)]
        attempts: u32,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let what = match cli.command {
        Command::Check { task } => format!("check {}", task.display()),
        Command::Run { task, attempts } => format!("run {} attempts={attempts}", task.display()),
    };
    eprintln!("proctor {what}: not implemented");
    ExitCode::from(2)
}
