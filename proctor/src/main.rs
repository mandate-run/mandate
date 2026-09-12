//! Proctor CLI: `proctor check <task.toml>` runs a contract once (hooks,
//! checks, journal) with no agent; `proctor run <task.toml>` runs the bounded
//! loop with a coding agent. Design: docs/harness.md.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use proctor::check;
use proctor::contract::{Outcome, Task};
use proctor::exec;
use proctor::journal::RunJournal;

#[derive(Parser)]
#[command(name = "proctor", about = "Build Hedera services from an acceptance contract and test them under payment failure")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the contract once against the current implementation. No agent.
    Check {
        task: PathBuf,
        #[arg(long, default_value = "false")]
        json: bool,
    },
    /// Run the bounded loop: check, hand failures to the coding agent, recheck.
    /// The agent adapter is not wired yet; this fails loudly.
    Run {
        task: PathBuf,
    },
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("proctor: {e}");
        std::process::exit(2);
    }
}

fn run(cli: Cli) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cwd: {e}"))?;
    match cli.command {
        Command::Run { .. } => Err(
            "proctor run needs the agent adapter (docs/harness.md); proctor check works today".into(),
        ),
        Command::Check { task, json } => {
            let task_path = task.clone();
            let contract = Task::load(&task_path)?;
            let journal = RunJournal::create(&cwd, &contract.task.name)?;

            let raw = std::fs::read(&task_path).map_err(|e| format!("read task: {e}"))?;
            let hash = journal.freeze_task(&task_path, &raw)?;

            // Hooks: setup brings the system up; a failing setup is an
            // infrastructure error unless it is project code (the hook runs
            // project code by definition, so any failure here is reported and
            // the run stops).
            if let Some(setup) = &contract.hooks.setup {
                println!("> setup: {setup}");
                let out = exec::run_shell(setup, std::time::Duration::from_secs(180), &cwd);
                if !out.exit_ok {
                    return Err(format!(
                        "setup failed ({:?}): {}",
                        out.code,
                        out.combined().lines().next().unwrap_or("")
                    ));
                }
            }
            if let Some(ready) = &contract.hooks.ready {
                let out = exec::run_shell(ready, std::time::Duration::from_secs(180), &cwd);
                if !out.exit_ok {
                    return Err(format!(
                        "ready probe failed ({:?}): {}",
                        out.code,
                        out.combined().lines().next().unwrap_or("")
                    ));
                }
            }

            println!("> checks for task {} (sha256 {hash})", contract.task.name);
            let (results, pass) = check::run_checks(&contract.check, &cwd);
            let outcome = if pass {
                Outcome::Pass
            } else {
                Outcome::ImplementationFailure
            };

            for r in &results {
                let mark = if r.ok() { "ok " } else { "FAIL" };
                println!("  [{mark}] {}", r.name);
                for f in &r.expect_failures {
                    println!("        {f}");
                }
                for f in &r.observe_failures {
                    println!("        {f}");
                }
                if !r.ok() {
                    println!("        {}", r.output_excerpt.lines().next().unwrap_or(""));
                }
            }

            if let Some(teardown) = &contract.hooks.teardown {
                let _ = exec::run_shell(teardown, std::time::Duration::from_secs(60), &cwd);
            }

            let report = journal.write_report(outcome.label(), &results)?;
            println!("outcome {}; report at {}", outcome.label(), report.display());

            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "outcome": outcome.label(),
                        "task_sha256": hash,
                        "checks": results.iter().map(|r| serde_json::json!({
                            "name": r.name,
                            "pass": r.ok(),
                            "expect": r.expect_ok,
                            "observe": r.observe_ok,
                        })).collect::<Vec<_>>(),
                        "report": report.display().to_string(),
                    }))
                    .unwrap()
                );
            }
            Ok(())
        }
    }
}
