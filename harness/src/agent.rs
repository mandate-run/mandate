//! Invokes one adapter executable with the task and previous findings, under
//! an allowlisted environment.
//!
//! The adapter is the only part of Proctor that writes to the worktree, and
//! Proctor never reads what it says about its own work: an agent claiming it
//! fixed the bug is not evidence, so the next attempt re-runs the contract
//! and the journal decides. The adapter's exit code says only whether it ran.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use crate::checks::Attempt;
use crate::hooks::{self, HookError, Output};
use crate::task::Task;

/// What an adapter is told: the contract it must satisfy and what the last
/// attempt actually did. Serialized as one JSON document on stdin.
#[derive(Debug, Serialize)]
pub struct Brief<'a> {
    /// The task contract as written, so the adapter reads the same file
    /// Proctor hashed rather than a summary of it.
    pub task_toml: &'a str,
    pub task_name: &'a str,
    pub task_hash: &'a str,
    /// Which attempt this is about to be, counting from 1.
    pub attempt: u32,
    pub attempts_allowed: u32,
    /// The outcome the previous attempt reached.
    pub outcome: &'a str,
    /// Every assertion that failed, in the order the checks ran.
    pub findings: Vec<String>,
    /// What Proctor measured itself, rather than what the application said.
    pub observed: &'a BTreeMap<String, serde_json::Value>,
}

/// Builds the brief for the attempt that follows `last`.
pub fn brief<'a>(
    task: &'a Task,
    task_toml: &'a str,
    last: &'a Attempt,
    next_attempt: u32,
) -> Brief<'a> {
    Brief {
        task_toml,
        task_name: &task.task.name,
        task_hash: &task.hash,
        attempt: next_attempt,
        attempts_allowed: task.task.attempts,
        outcome: last.outcome.as_str(),
        findings: crate::checks::findings(&last.checks),
        observed: &last.observed,
    }
}

/// Runs the adapter with the brief on stdin. A non-zero exit or a timeout is
/// returned as `Err`: the caller reports it as infrastructure, because an
/// adapter that could not run says nothing about the code under test.
pub async fn invoke(
    adapter: &str,
    brief: &Brief<'_>,
    dir: &Path,
    timeout: Duration,
) -> Result<Output, AgentError> {
    let payload =
        serde_json::to_string_pretty(brief).map_err(|e| AgentError::Brief(e.to_string()))?;
    let env = BTreeMap::new();
    let out = hooks::run_with_stdin(adapter, dir, &env, timeout, Some(&payload))
        .await
        .map_err(AgentError::Spawn)?;
    if out.timed_out {
        return Err(AgentError::TimedOut(timeout.as_secs()));
    }
    if !out.ok() {
        return Err(AgentError::Failed {
            code: out.code,
            tail: out.tail(8),
        });
    }
    Ok(out)
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("the brief could not be built: {0}")]
    Brief(String),
    #[error("the adapter could not start: {0}")]
    Spawn(#[from] HookError),
    #[error("the adapter timed out after {0} s")]
    TimedOut(u64),
    #[error("the adapter exited {code:?}: {tail}")]
    Failed { code: Option<i32>, tail: String },
}
