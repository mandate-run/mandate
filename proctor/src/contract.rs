//! The executable acceptance contract (docs/harness.md). A task is one TOML
//! document: hooks to bring the system up and down, one or more checks, each
//! with `expect` (assertions over the application's own output) and `observe`
//! (measurements Proctor takes from the fixture journal and ledgers), and an
//! optional live section. The agent cannot edit the task: its hash is frozen
//! per run.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Task {
    pub task: TaskMeta,
    #[serde(default)]
    pub hooks: Hooks,
    #[serde(default)]
    pub agent: Option<Agent>,
    #[serde(default)]
    pub check: Vec<Check>,
    #[serde(default)]
    pub live: Option<Live>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TaskMeta {
    pub name: String,
    #[serde(default = "default_attempts")]
    pub attempts: u32,
}

fn default_attempts() -> u32 {
    3
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Hooks {
    pub setup: Option<String>,
    pub ready: Option<String>,
    pub teardown: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Agent {
    pub adapter: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Check {
    pub name: String,
    pub run: String,
    /// Assertions over the application's own output: dotted JSON paths to
    /// expected values (e.g. `totals.settled = "0.0000"`).
    #[serde(default)]
    pub expect: BTreeMap<String, serde_json::Value>,
    /// Measurements from the fixture journal or application ledgers.
    #[serde(default)]
    pub observe: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Live {
    pub facilitator: String,
    pub mandate: String,
}

impl Task {
    pub fn load(path: &Path) -> Result<Task, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        let task: Task = toml::from_str(&raw)
            .map_err(|e| format!("parse {}: {e}", path.display()))?;
        if task.task.name.trim().is_empty() {
            return Err("task.name is required".into());
        }
        if task.check.is_empty() {
            return Err(format!("task {} has no [[check]] sections", task.task.name));
        }
        Ok(task)
    }
}

/// Outcome precedence (docs/harness.md): PAYMENT_UNRESOLVED, then
/// INFRASTRUCTURE_ERROR, then IMPLEMENTATION_FAILURE. A pass stops the loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Outcome {
    Pass,
    ImplementationFailure,
    InfrastructureError,
    PaymentUnresolved,
}

impl Outcome {
    pub fn label(&self) -> &'static str {
        match self {
            Outcome::Pass => "PASS",
            Outcome::ImplementationFailure => "IMPLEMENTATION_FAILURE",
            Outcome::InfrastructureError => "INFRASTRUCTURE_ERROR",
            Outcome::PaymentUnresolved => "PAYMENT_UNRESOLVED",
        }
    }
}

/// One check's result: which `expect` and `observe` rows held.
#[derive(Debug, Clone)]
pub struct CheckResult {
    pub name: String,
    pub expect_ok: bool,
    pub expect_failures: Vec<String>,
    pub observe_ok: bool,
    pub observe_failures: Vec<String>,
    pub output_excerpt: String,
}

impl CheckResult {
    pub fn ok(&self) -> bool {
        self.expect_ok && self.observe_ok
    }
}
