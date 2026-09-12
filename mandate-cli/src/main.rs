use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use mandate_core::cli::exec::{self, DEFAULT_LEDGER, DEFAULT_MANIFEST, DEFAULT_TRANSCRIPT};
use mandate_core::cli::{InitCommand, LedgerCommand, ReceiptsCommand, ReconcileCommand, RunCommand};
use mandate_core::fixture::Scenario;
use mandate_core::types::parse_amount;

#[derive(Parser)]
#[command(
    name = "mandate",
    version,
    about = "Buyer runtime: give your agent a mandate, not a credit card"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Write a mandate, the pinned manifest and an empty ledger into a directory
    Init {
        #[arg(long, default_value = "mandate-demo")]
        out_dir: PathBuf,
        #[arg(long, default_value = "0.0.123456")]
        principal: String,
        #[arg(long, default_value = "Explain any material liquidity change in the listed pools over the last 24 hours.")]
        purpose: String,
        #[arg(long, default_value = "0.0100", help = "USDC service budget")]
        service_total: String,
        #[arg(long, default_value = "0.5", help = "HBAR audit budget")]
        audit_total: String,
    },
    /// Run a demo scenario; by default against simulated sellers, ledger and
    /// settlement, or against live x402 sellers and Hedera testnet with --live
    Run {
        #[arg(long, default_value = "mandate-demo/mandate.json")]
        mandate_path: PathBuf,
        #[arg(long, default_value = "mandate-demo/manifest.json")]
        manifest_path: PathBuf,
        #[arg(long, default_value = "mandate-demo/ledger.json")]
        ledger_path: PathBuf,
        #[arg(long, default_value = "mandate-demo/transcript.json")]
        transcript_path: PathBuf,
        #[arg(long, default_value = "normal", value_parser = parse_scenario)]
        scenario: Scenario,
        #[arg(long, help = "print the transcript as JSON")]
        json: bool,
        #[arg(long, help = "run live: real x402 sellers, Hedera testnet settlement through Blocky402, HCS receipts (env: MANDATE_ACCOUNT_ID, MANDATE_PRIVATE_KEY, FACILITATOR_URL, MIRROR_NODE_URL, RECEIPTS_TOPIC, ETH_RPC_URL)")]
        live: bool,
        #[arg(long, help = "base URL of the live seller fleet; listing URLs are rewritten to {base}/{listing_id}")]
        sellers_url: Option<String>,
    },
    /// Re-check settlement for every non-terminal authorization
    Reconcile {
        mandate_id: String,
        #[arg(long, default_value = "mandate-demo/mandate.json")]
        mandate_path: PathBuf,
        #[arg(long, default_value = "mandate-demo/ledger.json")]
        ledger_path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Budget exposure and authorizations
    Ledger {
        mandate_id: String,
        #[arg(long, default_value = "mandate-demo/ledger.json")]
        ledger_path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Receipts in sequence
    Receipts {
        mandate_id: String,
        #[arg(long, default_value = "mandate-demo/ledger.json")]
        ledger_path: PathBuf,
        #[arg(long)]
        json: bool,
    },
}

fn parse_scenario(s: &str) -> Result<Scenario, String> {
    Scenario::parse(s)
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .init();
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mandate: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Command::Init {
            out_dir,
            principal,
            purpose,
            service_total,
            audit_total,
        } => {
            let cmd = InitCommand {
                out_dir,
                principal,
                purpose,
                service_total: parse_amount(&service_total, 6)?,
                audit_total: parse_amount(&audit_total, 8)?,
            };
            exec::execute_init(&cmd)?;
            println!(
                "init: mandate, manifest and empty ledger written to {}",
                cmd.out_dir.display()
            );
        }
        Command::Run {
            mandate_path,
            manifest_path,
            ledger_path,
            transcript_path,
            scenario,
            json,
            live,
            sellers_url,
        } => {
            let cmd = RunCommand {
                mandate_path,
                manifest_path,
                ledger_path,
                transcript_path,
                scenario,
                json,
                live,
                sellers_url,
            };
            let transcript = if cmd.live {
                exec::execute_run_live(&cmd)?
            } else {
                exec::execute_run(&cmd)?
            };
            if cmd.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&transcript)
                        .map_err(|e| format!("serialize transcript: {e}"))?
                );
            } else {
                print!("{}", exec::render_text(&transcript));
            }
        }
        Command::Reconcile {
            mandate_id,
            mandate_path,
            ledger_path,
            json,
        } => {
            let cmd = ReconcileCommand {
                mandate_id,
                mandate_path,
                ledger_path,
                json,
            };
            let text = exec::execute_reconcile(&cmd)?;
            print_output(json, &text);
        }
        Command::Ledger {
            mandate_id,
            ledger_path,
            json,
        } => {
            let cmd = LedgerCommand {
                mandate_id,
                ledger_path,
                json,
            };
            let text = exec::execute_ledger(&cmd)?;
            print_output(json, &text);
        }
        Command::Receipts {
            mandate_id,
            ledger_path,
            json,
        } => {
            let cmd = ReceiptsCommand {
                mandate_id,
                ledger_path,
                json,
            };
            let text = exec::execute_receipts(&cmd)?;
            print_output(json, &text);
        }
    }
    Ok(())
}

fn print_output(json: bool, text: &str) {
    if json {
        println!("{}", serde_json::json!({ "output": text }));
    } else {
        print!("{text}");
    }
}

#[allow(dead_code)]
fn _defaults() -> (&'static str, &'static str, &'static str) {
    (DEFAULT_LEDGER, DEFAULT_MANIFEST, DEFAULT_TRANSCRIPT)
}