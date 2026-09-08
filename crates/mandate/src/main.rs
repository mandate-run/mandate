//! `mandate` command line. `topic create` is real; the rest are stubs until
//! the slices in issue #2 land.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use mandate::config::{Config, PublicConfig};
use mandate::hedera::MirrorNode;
use mandate::ledger::Ledger;
use mandate::mandate::asset_decimals;
use mandate::manifest::{Manifest, RequestShape, units_for};
use mandate::purchase;
use mandate::quote::{DEFAULT_QUOTE_TIMEOUT, QuoteRequest, Quoter};
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
        /// Path to the mandate file, TOML.
        file: PathBuf,
        /// Print the transcript as JSON.
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
        /// Request body as JSON text; empty for GET listings.
        #[arg(long, default_value = "")]
        body: String,
        /// Pools in the request, for unit counting.
        #[arg(long, default_value_t = 1)]
        pools: u64,
        /// Window in hours, for unit counting.
        #[arg(long, default_value_t = 24)]
        window_h: u64,
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
    let what = match cli.command {
        Command::Run { file, json } => format!("run {} json={json}", file.display()),
        Command::Quote {
            manifest,
            listing,
            body,
            pools,
            window_h,
        } => {
            return match quote(&manifest, &listing, body, pools, window_h).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("mandate quote: {e:#}");
                    ExitCode::from(1)
                }
            };
        }
        Command::Reconcile { mandate_id, ledger } => {
            return match reconcile(&mandate_id, &ledger).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("mandate reconcile: {e:#}");
                    ExitCode::from(1)
                }
            };
        }
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

/// Section 4 quoting from a process that holds no key.
async fn quote(
    manifest_path: &std::path::Path,
    listing_id: &str,
    body: String,
    pools: u64,
    window_h: u64,
) -> anyhow::Result<()> {
    let cfg = PublicConfig::load()?;
    let text = std::fs::read_to_string(manifest_path)?;
    let manifest = Manifest::from_json(&text)?;
    let listing = manifest.listing(listing_id)?;
    let shape = RequestShape {
        pools,
        window_seconds: window_h * 3600,
        body_bytes: body.len() as u64,
    };
    let units = units_for(listing.tariff.unit, &shape);
    let request = QuoteRequest::new(listing, body.into_bytes(), units);
    let quoter = Quoter::from_config(&cfg, DEFAULT_QUOTE_TIMEOUT).await?;
    println!(
        "facilitator signers for {}: {:?}",
        cfg.network.caip2(),
        quoter.signers()
    );
    let now = time::OffsetDateTime::now_utc();
    let decimals = asset_decimals(&listing.asset).unwrap_or(0);
    match quoter.quote(listing, &request, now).await {
        Ok(q) => {
            print!("{}", quotes_table(std::slice::from_ref(&q), &[], decimals));
            match q.refusal() {
                Some(r) => println!("{r}"),
                None => println!(
                    "quote usable until {} for {} units",
                    q.valid_until(),
                    request.units
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
