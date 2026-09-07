//! `mandate` command line. `topic create` is real; the rest are stubs until
//! the slices in issue #2 land.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use mandate::config::Config;

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

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let what = match cli.command {
        Command::Run { file, json } => format!("run {} json={json}", file.display()),
        Command::Reconcile => "reconcile".to_owned(),
        Command::Topic {
            command: TopicCommand::Create,
        } => {
            return match topic_create().await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("mandate topic create: {e:#}");
                    ExitCode::from(1)
                }
            };
        }
    };
    eprintln!("mandate {what}: not implemented");
    ExitCode::from(2)
}

/// Creates the receipts topic, section 11, and prints the line for `.env`.
async fn topic_create() -> anyhow::Result<()> {
    let cfg = Config::load()?;
    let consensus = cfg.signer()?.consensus(cfg.network);
    let topic = consensus.create_topic("mandate receipts").await?;
    println!("HCS_TOPIC_ID={topic}");
    println!("{}/topic/{topic}", cfg.network.hashscan());
    Ok(())
}
