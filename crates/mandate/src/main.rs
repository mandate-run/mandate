//! `mandate` command line: `run`, `quote`, `reconcile`, `topic create`.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use mandate::config::{Config, PublicConfig};
use mandate::hedera::MirrorNode;
use mandate::ledger::Ledger;
use mandate::mandate::asset_decimals;
use mandate::manifest::Manifest;
use mandate::purchase;
use mandate::quote::{DEFAULT_QUOTE_TIMEOUT, Quoter};
use mandate::transcript::quotes_table;

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
        /// Path to the mandate file, TOML. The manifest path inside it is relative to this file.
        file: PathBuf,
        /// Path to the ledger.
        #[arg(long, default_value = "mandate.sqlite")]
        ledger: PathBuf,
        /// Mandate id for this run, replacing the file's; ids are unique per run.
        #[arg(long)]
        id: Option<String>,
        /// Print the report and transcript as one JSON document instead of streaming lines.
        #[arg(long)]
        json: bool,
    },
    /// Recover a run that stopped mid-purchase, then carry on where it left off.
    Resume {
        /// Path to the mandate file the run used.
        file: PathBuf,
        /// Path to the ledger holding its authorizations.
        #[arg(long, default_value = "mandate.sqlite")]
        ledger: PathBuf,
        /// The mandate id to resume, when it differs from the file's.
        #[arg(long)]
        id: Option<String>,
        /// Print the report and transcript as one JSON document instead of streaming lines.
        #[arg(long)]
        json: bool,
    },
    /// Re-run I5 for every non-terminal authorization of a mandate from the mirror node.
    Reconcile {
        /// The mandate id.
        mandate_id: String,
        /// Path to the ledger.
        #[arg(long, default_value = "mandate.sqlite")]
        ledger: PathBuf,
    },
    /// Quote one listing through a client that cannot pay; prints the quotes table.
    Quote {
        /// Path to the manifest JSON.
        #[arg(long)]
        manifest: PathBuf,
        /// Listing id in the manifest.
        #[arg(long)]
        listing: String,
        /// Request body as JSON text; empty for GET listings. The unit count is read from it.
        #[arg(long, default_value = "")]
        body: String,
    },
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
    match cli.command {
        Command::Run {
            file,
            ledger,
            id,
            json,
        } => run(&file, &ledger, id.as_deref(), json, false).await,
        Command::Resume {
            file,
            ledger,
            id,
            json,
        } => run(&file, &ledger, id.as_deref(), json, true).await,
        Command::Quote {
            manifest,
            listing,
            body,
        } => match quote(&manifest, &listing, body).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("mandate quote: {e:#}");
                ExitCode::from(1)
            }
        },
        Command::Reconcile { mandate_id, ledger } => match reconcile(&mandate_id, &ledger).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("mandate reconcile: {e:#}");
                ExitCode::from(1)
            }
        },
        Command::Topic {
            command: TopicCommand::Create,
        } => match topic_create().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("mandate topic create: {e:#}");
                ExitCode::from(1)
            }
        },
    }
}

/// Sections 4 to 12 for one mandate, or section 6 recovery first when
/// `resume`. Exit 0 delivered, 3 refused, 4 delivered with findings, 5
/// receipts not anchored when the mandate requires it, 6 a payment neither
/// settled nor failed, 1 error, 2 a mandate or manifest that does not load.
async fn run(
    file: &std::path::Path,
    ledger: &std::path::Path,
    id: Option<&str>,
    json: bool,
    resume: bool,
) -> ExitCode {
    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("mandate run: {e}");
            return ExitCode::from(1);
        }
    };
    match mandate::run::run(&cfg, file, ledger, id, json, resume).await {
        Ok(report) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).unwrap_or_default()
                );
            }
            ExitCode::from(report.status.exit_code())
        }
        Err(e @ (mandate::run::RunError::Mandate(_) | mandate::run::RunError::Manifest(_))) => {
            eprintln!("mandate run: {e}");
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("mandate run: {e}");
            ExitCode::from(1)
        }
    }
}

/// Section 4 quoting from a process that holds no key.
async fn quote(
    manifest_path: &std::path::Path,
    listing_id: &str,
    body: String,
) -> anyhow::Result<()> {
    let cfg = PublicConfig::load()?;
    let text = std::fs::read_to_string(manifest_path)?;
    let manifest = Manifest::from_json(&text)?;
    let listing = manifest.listing(listing_id)?;
    let quoter = Quoter::from_config(&cfg, DEFAULT_QUOTE_TIMEOUT).await?;
    println!(
        "facilitator signers for {}: {:?}",
        cfg.network.caip2(),
        quoter.signers()
    );
    let now = time::OffsetDateTime::now_utc();
    let decimals = asset_decimals(&listing.asset).unwrap_or(0);
    match quoter.quote(listing, body.into_bytes(), now).await {
        Ok(q) => {
            print!("{}", quotes_table(std::slice::from_ref(&q), &[], decimals));
            match q.refusal() {
                Some(r) => println!("{r}"),
                None => println!(
                    "quote usable until {} for {} units",
                    q.valid_until(),
                    q.request.units
                ),
            }
            Ok(())
        }
        Err(e) => {
            println!("REFUSED {} {e}", e.code());
            Ok(())
        }
    }
}

/// Section 6 reconcile. Needs the mirror node, never the key.
async fn reconcile(mandate_id: &str, path: &std::path::Path) -> anyhow::Result<()> {
    let cfg = PublicConfig::load()?;
    let mut ledger = Ledger::open(path)?;
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let mirror = MirrorNode::new(http, cfg.mirror_node_url);
    let now = time::OffsetDateTime::now_utc();
    let results = purchase::reconcile(&mut ledger, &mirror, mandate_id, now).await?;
    if results.is_empty() {
        println!("nothing to reconcile: every authorization is terminal");
    }
    for r in &results {
        println!(
            "{} {} -> {} {:?}",
            r.tx_id,
            r.before.as_str(),
            r.after.as_str(),
            r.settlement
        );
    }
    let accounts = ledger.accounts(mandate_id)?;
    println!(
        "settled {} outstanding {} held {} free {}",
        accounts.settled,
        accounts.outstanding,
        accounts.held,
        accounts.free()
    );
    Ok(())
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
