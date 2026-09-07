//! `mandate` command line. Commands are stubs until the slices in issue #2 land.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Buyer runtime: purchases evidence under a mandate and accounts for every payment.
#[derive(Parser)]
#[command(name = "mandate", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Execute a mandate file and print the transcript.
    Run {
        /// Path to the mandate file, TOML.
        file: PathBuf,
        /// Print the transcript as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Resolve authorizations a previous run left unresolved.
    Reconcile,
    /// HCS receipts topic.
    Topic {
        #[command(subcommand)]
        command: TopicCommand,
    },
}

#[derive(Subcommand)]
enum TopicCommand {
    /// Create the receipts topic and print its id.
    Create,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let what = match cli.command {
        Command::Run { file, json } => format!("run {} json={json}", file.display()),
        Command::Reconcile => "reconcile".to_owned(),
        Command::Topic {
            command: TopicCommand::Create,
        } => "topic create".to_owned(),
    };
    eprintln!("mandate {what}: not implemented");
    ExitCode::from(2)
}
