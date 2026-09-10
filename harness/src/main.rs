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

    let env: BTreeMap<String, String> = BTreeMap::new();
    let hook_timeout = Duration::from_secs(task.hooks.timeout_s);
    let mut results: Vec<CheckResult> = Vec::new();
    let mut infrastructure = Vec::new();
    let mut observed = checks::Measurements::new();
    // A second check must not replace the first check's fixture or journal.
    let run_lock = (|| -> std::io::Result<std::fs::File> {
        std::fs::create_dir_all(dir.join(".proctor"))?;
        let file = std::fs::File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(".proctor/check.lock"))?;
        file.try_lock().map_err(|e| {
            std::io::Error::other(format!("another Proctor check owns this worktree: {e}"))
        })?;
        Ok(file)
    })();
    if let Err(e) = &run_lock {
        infrastructure.push(e.to_string());
    }

    // All loaded tasks go through report finalization, even if preflight fails.
    for url in &task.requires.reachable {
        if let Err(e) = hedera::reachable(url, Duration::from_secs(10)).await {
            infrastructure.push(format!("{url} is unreachable: {e}"));
            break;
        }
    }
    let mut setup_started = false;
    if infrastructure.is_empty() {
        setup_started = true;
        if let Some(setup) = &task.hooks.setup {
            match hooks::run(setup, &dir, &env, hook_timeout).await {
                Ok(out) if out.ok() => {}
                Ok(out) => infrastructure.push(format!("setup failed: {}", out.tail(8))),
                Err(e) => infrastructure.push(format!("setup: {e}")),
            }
        }
        if infrastructure.is_empty()
            && let Some(ready) = &task.hooks.ready
        {
            match hooks::wait_ready(ready, &dir, &env, hook_timeout).await {
                Ok(out) if out.ok() => {}
                Ok(out) => infrastructure.push(format!("fixture never ready: {}", out.tail(8))),
                Err(e) => infrastructure.push(format!("ready: {e}")),
            }
        }
    }
    if infrastructure.is_empty() {
        for c in &task.checks {
            let before = observe::gather(&task.evidence, &dir, false);
            observed.extend(before.measurements);
            infrastructure.extend(before.errors);
            if !infrastructure.is_empty()
                || (!hedera::exposure(&observed).is_empty() && !c.recovers)
            {
                break;
            }
            if !json {
                print!("  {} ... ", c.name);
                use std::io::Write as _;
                let _ = std::io::stdout().flush();
            }
            let out = match hooks::run(&c.run, &dir, &env, Duration::from_secs(c.timeout_s)).await {
                Ok(out) => out,
                Err(e) => {
                    infrastructure.push(format!("{}: {e}", c.name));
                    break;
                }
            };
            let after = observe::gather(&task.evidence, &dir, c.name != "build");
            // Retain the last known exposure if another source becomes unreadable.
            observed.extend(after.measurements.clone());
            infrastructure.extend(after.errors);
            let result = checks::judge(c, &out, &after.measurements);
            let failed = result.outcome != Outcome::Pass;
            if !json {
                println!("{} ({} ms)", result.outcome.as_str(), result.ms);
                for f in &result.findings {
                    println!("      {f}");
                }
            }
            results.push(result);
            // A failed prerequisite must not invoke later paid commands. A
            // deliberate crash is expressed with allow_exit in its contract.
            if failed || !infrastructure.is_empty() {
                break;
            }
        }
    }
    // Capture evidence before teardown as well: cleanup cannot erase exposure.
    let before_cleanup = observe::gather(&task.evidence, &dir, false);
    observed.extend(before_cleanup.measurements);
    infrastructure.extend(before_cleanup.errors);
    if setup_started && let Some(teardown) = &task.hooks.teardown {
        match hooks::run(teardown, &dir, &env, hook_timeout).await {
            Ok(out) if out.ok() => {}
            Ok(out) => infrastructure.push(format!("teardown failed: {}", out.tail(8))),
            Err(e) => infrastructure.push(format!("teardown: {e}")),
        }
    }
    // Teardown is fixture cleanup, not payment reconciliation. It must not
    // lower the exposure observed after the last check.
    let exposure_outcome = hedera::reclassify(&mut results, &observed);
    let mut attempt = Attempt::new(&task, 1, results, observed);
    attempt.outcome = attempt.outcome.max(exposure_outcome);
    if !infrastructure.is_empty() {
        attempt.outcome = attempt.outcome.max(Outcome::InfrastructureError);
        if !json {
            for problem in &infrastructure {
                println!("  INFRASTRUCTURE_ERROR: {problem}");
            }
        }
    }
    attempt.infrastructure = infrastructure;
    let mut outcome = attempt.outcome;
    let mut report = Report::new(&task, started).finish(vec![attempt]);
    let written = report.write(&dir);
    // A run whose evidence was not saved cannot be certified: the report is
    // the record a reviewer reads, so failing to write it is infrastructure.
    if let Err(e) = &written {
        let problem = format!("the report could not be written: {e}");
        eprintln!("proctor: INFRASTRUCTURE_ERROR: {problem}");
        report.attempts[0].infrastructure.push(problem);
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
