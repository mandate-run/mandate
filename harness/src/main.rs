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
        /// Directory the hooks, checks and agent run in. Default: the task's parent.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Maximum attempts. Bounded by the contract's own `attempts`.
        #[arg(long, default_value_t = 3)]
        attempts: u32,
        /// Print the report as JSON instead of a summary.
        #[arg(long)]
        json: bool,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Check { task, dir, json } => check(&task, dir.as_deref(), json).await,
        Command::Run {
            task,
            dir,
            attempts,
            json,
        } => run(&task, dir.as_deref(), attempts, json).await,
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

/// Takes the worktree for the duration of a command. Held by `check` for one
/// attempt and by `run` across every attempt, so an agent never edits a tree
/// another Proctor is measuring. The returned file must stay alive: dropping
/// it releases the lock.
fn take_worktree(dir: &Path) -> std::io::Result<std::fs::File> {
    std::fs::create_dir_all(dir.join(".proctor"))?;
    let file = std::fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(".proctor/check.lock"))?;
    file.try_lock().map_err(|e| {
        std::io::Error::other(format!("another Proctor run owns this worktree: {e}"))
    })?;
    Ok(file)
}

/// Where a task's checks and hooks run: the directory given, else the task's
/// own parent.
fn worktree(path: &Path, dir: Option<&Path>) -> PathBuf {
    dir.map(Path::to_path_buf)
        .or_else(|| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// One attempt: setup, ready, every check, teardown. Teardown always runs, so
/// a failing check never leaves a fixture behind. Reporting is the caller's,
/// because `run` collects many of these before it writes anything.
async fn attempt(
    task: &Task,
    dir: &Path,
    number: u32,
    lock_problem: Option<&str>,
    json: bool,
) -> Attempt {
    let env: BTreeMap<String, String> = BTreeMap::new();
    let hook_timeout = Duration::from_secs(task.hooks.timeout_s);
    let mut results: Vec<CheckResult> = Vec::new();
    let mut infrastructure = Vec::new();
    let mut observed = checks::Measurements::new();
    if let Some(problem) = lock_problem {
        infrastructure.push(problem.to_owned());
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
            match hooks::run(setup, dir, &env, hook_timeout).await {
                Ok(out) if out.ok() => {}
                Ok(out) => infrastructure.push(format!("setup failed: {}", out.tail(8))),
                Err(e) => infrastructure.push(format!("setup: {e}")),
            }
        }
        if infrastructure.is_empty()
            && let Some(ready) = &task.hooks.ready
        {
            match hooks::wait_ready(ready, dir, &env, hook_timeout).await {
                Ok(out) if out.ok() => {}
                Ok(out) => infrastructure.push(format!("fixture never ready: {}", out.tail(8))),
                Err(e) => infrastructure.push(format!("ready: {e}")),
            }
        }
    }
    if infrastructure.is_empty() {
        for c in &task.checks {
            let before = observe::gather(&task.evidence, dir, false);
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
            let out = match hooks::run(&c.run, dir, &env, Duration::from_secs(c.timeout_s)).await {
                Ok(out) => out,
                Err(e) => {
                    infrastructure.push(format!("{}: {e}", c.name));
                    break;
                }
            };
            let after = observe::gather(&task.evidence, dir, c.name != "build");
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
    let before_cleanup = observe::gather(&task.evidence, dir, false);
    observed.extend(before_cleanup.measurements);
    infrastructure.extend(before_cleanup.errors);
    if setup_started && let Some(teardown) = &task.hooks.teardown {
        match hooks::run(teardown, dir, &env, hook_timeout).await {
            Ok(out) if out.ok() => {}
            Ok(out) => infrastructure.push(format!("teardown failed: {}", out.tail(8))),
            Err(e) => infrastructure.push(format!("teardown: {e}")),
        }
    }
    // Teardown is fixture cleanup, not payment reconciliation. It must not
    // lower the exposure observed after the last check.
    let exposure_outcome = hedera::reclassify(&mut results, &observed);
    let mut attempt = Attempt::new(task, number, results, observed);
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
    attempt
}

/// Writes the report and prints the summary a reviewer reads. Returns the
/// outcome, which a failed write can only make worse.
fn deliver(
    task: &Task,
    dir: &Path,
    started: String,
    attempts: Vec<Attempt>,
    json: bool,
) -> Outcome {
    let last = attempts.len();
    let mut report = Report::new(task, started).finish(attempts);
    let mut outcome = report.outcome;
    let written = report.write(dir);
    // A run whose evidence was not saved cannot be certified: the report is
    // the record a reviewer reads, so failing to write it is infrastructure.
    if let Err(e) = &written {
        let problem = format!("the report could not be written: {e}");
        eprintln!("proctor: INFRASTRUCTURE_ERROR: {problem}");
        if let Some(a) = report.attempts.last_mut() {
            a.infrastructure.push(problem);
        }
        if outcome < Outcome::PaymentUnresolved {
            outcome = Outcome::InfrastructureError;
            report.outcome = outcome;
            if let Some(a) = report.attempts.last_mut() {
                a.outcome = outcome;
            }
        }
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
        return outcome;
    }
    println!("proctor: {}", outcome.as_str());
    if let Some(a) = report.attempts.last() {
        // The findings, gathered once, are what an agent adapter receives.
        let found = checks::findings(&a.checks);
        if !found.is_empty() {
            println!("  {} finding(s)", found.len());
        }
    }
    if last > 1 {
        println!("  {last} attempt(s)");
    }
    if let Ok(p) = &written {
        println!("  report {}", p.display());
    }
    if let Some(a) = report.attempts.last()
        && !a.observed.is_empty()
    {
        let mut named: Vec<String> = a.observed.iter().map(|(k, v)| format!("{k} {v}")).collect();
        named.sort();
        println!("  observed {}", named.join(", "));
    }
    outcome
}

/// `proctor check`: one attempt against the current worktree.
async fn check(path: &Path, dir: Option<&Path>, json: bool) -> ExitCode {
    let task = match Task::load(path) {
        Ok(t) => t,
        Err(e) => return fail_early(&format!("{e}"), json),
    };
    let dir = worktree(path, dir);
    let started = journal::now();
    if !json {
        println!(
            "proctor: {} ({} check(s))",
            task.task.name,
            task.checks.len()
        );
        println!("  task {} in {}", &task.hash[..16], dir.display());
    }
    let held = take_worktree(&dir);
    let problem = held.as_ref().err().map(std::string::ToString::to_string);
    let a = attempt(&task, &dir, 1, problem.as_deref(), json).await;
    ExitCode::from(deliver(&task, &dir, started, vec![a], json).exit_code())
}

/// `proctor run`: attempt, hand the findings to the adapter, attempt again.
///
/// Stops at the first pass, at the attempt limit, and at anything that makes
/// another attempt meaningless or unsafe. Only an implementation failure is
/// handed to an agent: an infrastructure error says nothing about the code,
/// and unresolved exposure means money is neither spent nor free, so editing
/// the code and running again could sign a second payment over the first.
/// That is the failure Proctor exists to catch, so it must not cause it.
async fn run(path: &Path, dir: Option<&Path>, attempts: u32, json: bool) -> ExitCode {
    let task = match Task::load(path) {
        Ok(t) => t,
        Err(e) => return fail_early(&format!("{e}"), json),
    };
    // The adapter reads the contract as written, not a summary of it.
    let task_toml = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => return fail_early(&format!("{}: {e}", path.display()), json),
    };
    let Some(adapter) = task.agent.adapter.clone() else {
        return fail_early(
            &format!(
                "{} names no agent.adapter, so there is nothing to drive; use `proctor check`",
                path.display()
            ),
            json,
        );
    };
    let dir = worktree(path, dir);
    let started = journal::now();
    // The command line bounds the contract, never raises it: a task that
    // allows one attempt is not made to allow five by a flag.
    let allowed = attempts.min(task.task.attempts).max(1);
    if !json {
        println!(
            "proctor: {} ({} check(s), up to {allowed} attempt(s))",
            task.task.name,
            task.checks.len()
        );
        println!("  task {} in {}", &task.hash[..16], dir.display());
    }

    // Held across every attempt, so the agent never edits a worktree another
    // Proctor is measuring.
    let held = take_worktree(&dir);
    let problem = held.as_ref().err().map(std::string::ToString::to_string);

    let mut done: Vec<Attempt> = Vec::new();
    for number in 1..=allowed {
        if !json && number > 1 {
            println!("  attempt {number} of {allowed}");
        }
        let a = attempt(&task, &dir, number, problem.as_deref(), json).await;
        let outcome = a.outcome;
        done.push(a);

        match checks::next_after(outcome, number, allowed) {
            checks::Next::Stop(why) => {
                if !json && outcome != Outcome::Pass {
                    println!("  stopping: {why}");
                }
                break;
            }
            checks::Next::Agent => {}
        }
        // A contract edited mid-run is a different task, and every attempt
        // before it was judged against something else.
        match Task::load(path) {
            Ok(t) if t.hash == task.hash => {}
            Ok(_) => {
                if let Some(last) = done.last_mut() {
                    last.infrastructure
                        .push("the task contract changed during the run".to_owned());
                    last.outcome = last.outcome.max(Outcome::InfrastructureError);
                }
                break;
            }
            Err(e) => {
                if let Some(last) = done.last_mut() {
                    last.infrastructure
                        .push(format!("the task contract became unreadable: {e}"));
                    last.outcome = last.outcome.max(Outcome::InfrastructureError);
                }
                break;
            }
        }

        let brief = agent::brief(
            &task,
            &task_toml,
            done.last().expect("just pushed"),
            number + 1,
        );
        if !json {
            println!("  agent: {} finding(s) to fix", brief.findings.len());
        }
        let timeout = Duration::from_secs(task.agent.timeout_s);
        if let Err(e) = agent::invoke(&adapter, &brief, &dir, timeout).await {
            // An adapter that could not run is infrastructure: it says
            // nothing about the code under test.
            if let Some(last) = done.last_mut() {
                last.infrastructure.push(format!("agent: {e}"));
                last.outcome = last.outcome.max(Outcome::InfrastructureError);
            }
            if !json {
                println!("  INFRASTRUCTURE_ERROR: agent: {e}");
            }
            break;
        }
    }
    ExitCode::from(deliver(&task, &dir, started, done, json).exit_code())
}
