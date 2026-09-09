//! `proctor` command line. `check` runs a task contract once against the
//! current worktree and reports one of four outcomes; `run` drives an agent
//! adapter through attempts until the task passes.

mod agent;
mod checks;
mod hedera;
mod hooks;
mod journal;
mod observe;
mod task;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};

use checks::{Attempt, CheckResult, Outcome};
use journal::Report;
use task::Task;

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
        /// Directory the hooks and checks run in. Default: the task's parent.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Print the report as JSON instead of a summary.
        #[arg(long)]
        json: bool,
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

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Check { task, dir, json } => check(&task, dir.as_deref(), json).await,
        Command::Run { task, attempts } => {
            eprintln!(
                "proctor run {} attempts={attempts}: the agent loop is not implemented; use `proctor check`",
                task.display()
            );
            ExitCode::from(2)
        }
    }
}

/// An infrastructure error before a task could run: still structured when
/// `--json`, so a caller always gets a document rather than bare text.
fn fail_early(problem: &str, json: bool) -> ExitCode {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "outcome": Outcome::InfrastructureError.as_str(),
                "problem": problem,
            })
        );
    } else {
        eprintln!("proctor: INFRASTRUCTURE_ERROR: {problem}");
    }
    ExitCode::from(Outcome::InfrastructureError.exit_code())
}

/// One attempt: setup, ready, every check, teardown, report. Teardown always
/// runs, so a failing check never leaves a fixture behind.
async fn check(path: &Path, dir: Option<&Path>, json: bool) -> ExitCode {
    let task = match Task::load(path) {
        Ok(t) => t,
        Err(e) => return fail_early(&format!("{e}"), json),
    };
    let dir = dir
        .map(Path::to_path_buf)
        .or_else(|| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    let started = journal::now();
    if !json {
        println!(
            "proctor: {} ({} check(s), up to {} attempt(s))",
            task.task.name,
            task.checks.len(),
            task.task.attempts
        );
        println!("  task {} in {}", &task.hash[..16], dir.display());
    }

    // Proctor's own probe first: a service it cannot reach is its problem,
    // and must never cause a rewrite of working payment code.
    for url in &task.requires.reachable {
        if let Err(e) = hedera::reachable(url, Duration::from_secs(10)).await {
            return fail_early(&format!("{url} is unreachable: {e}"), json);
        }
    }

    let env: BTreeMap<String, String> = BTreeMap::new();
    let hook_timeout = Duration::from_secs(task.hooks.timeout_s);
    let mut results: Vec<CheckResult> = Vec::new();

    let mut infrastructure: Option<String> = None;

    // Setup and ready are the fixture's, not the application's: a failure
    // here stops the loop rather than blaming the code under test.
    if let Some(setup) = &task.hooks.setup {
        match hooks::run(setup, &dir, &env, hook_timeout).await {
            Ok(out) if out.ok() => {}
            Ok(out) => infrastructure = Some(format!("setup failed: {}", out.tail(8))),
            Err(e) => infrastructure = Some(format!("setup: {e}")),
        }
    }
    if infrastructure.is_none()
        && let Some(ready) = &task.hooks.ready
    {
        match hooks::wait_ready(ready, &dir, &env, hook_timeout).await {
            Ok(out) if out.ok() => {}
            Ok(out) => {
                infrastructure = Some(format!("the fixture never became ready: {}", out.tail(8)))
            }
            Err(e) => infrastructure = Some(format!("ready: {e}")),
        }
    }

    if infrastructure.is_none() {
        for c in &task.checks {
            if !json {
                print!("  {} ... ", c.name);
                use std::io::Write as _;
                let _ = std::io::stdout().flush();
            }
            let out = match hooks::run(&c.run, &dir, &env, Duration::from_secs(c.timeout_s)).await {
                Ok(out) => out,
                Err(e) => {
                    infrastructure = Some(format!("{}: {e}", c.name));
                    break;
                }
            };
            // Measure after the check, so the journal and ledger show what it
            // did. Evidence Proctor cannot read in full stops the run: a
            // contract must never be judged against partial evidence.
            let observed = match observe::gather(&task.evidence, &dir) {
                Ok(m) => m,
                Err(e) => {
                    infrastructure = Some(format!("evidence: {e}"));
                    break;
                }
            };
            let result = checks::judge(c, &out, &observed);
            let stop = !result.findings.is_empty() && c.name == "build";
            if !json {
                println!("{} ({} ms)", result.outcome.as_str(), result.ms);
                for f in &result.findings {
                    for line in f.lines() {
                        println!("      {line}");
                    }
                }
            }
            results.push(result);
            // A build that fails makes every later check meaningless: it
            // would test whatever binary happened to be lying around.
            if stop {
                if !json {
                    println!("  stopping: the remaining checks would test a stale build");
                }
                break;
            }
            // Exposure stops the run before anything else is bought. A check
            // that recovers exposure is the exception, and says so.
            let held = hedera::exposure(&observed);
            if !held.is_empty() {
                let next_recovers = task
                    .checks
                    .iter()
                    .skip_while(|x| x.name != c.name)
                    .nth(1)
                    .is_some_and(|x| x.recovers);
                if !next_recovers {
                    if !json {
                        println!(
                            "  stopping: {} authorization(s) are exposure; nothing more is bought",
                            held.iter().map(|(_, n)| n).sum::<i64>()
                        );
                    }
                    break;
                }
            }
        }
    }

    if let Some(teardown) = &task.hooks.teardown {
        let _ = hooks::run(teardown, &dir, &env, hook_timeout).await;
    }

    let observed = match observe::gather(&task.evidence, &dir) {
        Ok(m) => m,
        Err(e) => {
            infrastructure.get_or_insert(format!("evidence: {e}"));
            checks::Measurements::new()
        }
    };
    // Exposure the ledger still holds outranks whatever the checks decided.
    hedera::reclassify(&mut results, &observed);
    let mut attempt = Attempt::new(&task, 1, results, observed);
    if let Some(problem) = &infrastructure {
        attempt.outcome = Outcome::InfrastructureError;
        if !json {
            println!("  INFRASTRUCTURE_ERROR: {problem}");
        }
    }
    let mut outcome = attempt.outcome;
    let mut report = Report::new(&task, started).finish(vec![attempt]);
    let written = report.write(&dir);
    // A run whose evidence was not saved cannot be certified: the report is
    // the record a reviewer reads, so failing to write it is infrastructure.
    if let Err(e) = &written {
        let problem = format!("the report could not be written: {e}");
        eprintln!("proctor: INFRASTRUCTURE_ERROR: {problem}");
        if outcome < Outcome::PaymentUnresolved {
            outcome = Outcome::InfrastructureError;
            report.outcome = outcome;
            if let Some(a) = report.attempts.first_mut() {
                a.outcome = outcome;
            }
        }
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
    } else {
        println!("proctor: {}", outcome.as_str());
        // The findings, gathered once, are what an agent adapter receives.
        let found = checks::findings(&report.attempts[0].checks);
        if !found.is_empty() {
            println!("  {} finding(s)", found.len());
        }
        if let Ok(p) = &written {
            println!("  report {}", p.display());
        }
        if !report.attempts[0].observed.is_empty() {
            let m = &report.attempts[0].observed;
            let mut named: Vec<String> = m.iter().map(|(k, v)| format!("{k} {v}")).collect();
            named.sort();
            println!("  observed {}", named.join(", "));
        }
    }
    ExitCode::from(outcome.exit_code())
}
