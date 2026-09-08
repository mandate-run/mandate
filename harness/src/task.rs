//! The task contract: an executable acceptance specification, TOML. It names
//! the hooks that bring a fixture up and down, the checks to run, and what
//! each check must show, both in the application's own output (`expect`) and
//! in the measurements Proctor takes independently (`observe`).
//!
//! A task is data. Proctor never lets an agent edit one inside the attempt it
//! would turn green, so the file's hash is recorded at run start.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    #[error("task {path}: {problem}")]
    File { path: String, problem: String },
    #[error("task is not valid TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("task: {0}")]
    Field(String),
}

/// What a check compares against: a JSON scalar, so a contract stays readable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Expected {
    Bool(bool),
    Int(i64),
    Text(String),
}

impl Expected {
    pub fn matches(&self, actual: &serde_json::Value) -> bool {
        match (self, actual) {
            (Self::Bool(b), serde_json::Value::Bool(a)) => b == a,
            (Self::Int(i), serde_json::Value::Number(n)) => n.as_i64() == Some(*i),
            // A number written as a string still compares as one, since the
            // runtime reports amounts as decimal strings.
            (Self::Int(i), serde_json::Value::String(s)) => s.parse::<i64>().ok() == Some(*i),
            (Self::Text(t), serde_json::Value::String(s)) => t == s,
            (Self::Text(t), other) => t == &other.to_string(),
            _ => false,
        }
    }
}

impl std::fmt::Display for Expected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bool(b) => write!(f, "{b}"),
            Self::Int(i) => write!(f, "{i}"),
            Self::Text(t) => write!(f, "{t}"),
        }
    }
}

/// Commands that bring the fixture up and down. Each runs with a timeout and
/// its output is captured; a hook that runs project code and fails is an
/// implementation failure, not an infrastructure error.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hooks {
    pub setup: Option<String>,
    /// Polled until it succeeds or the timeout passes.
    pub ready: Option<String>,
    pub teardown: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout_s: u64,
}

fn default_timeout() -> u64 {
    120
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub name: String,
    /// The command to run. Its stdout is parsed as JSON when `expect` is set.
    pub run: String,
    /// Assertions over the application's own output, by dotted path.
    #[serde(default)]
    pub expect: BTreeMap<String, Expected>,
    /// Assertions over what Proctor measures itself, by name.
    #[serde(default)]
    pub observe: BTreeMap<String, Expected>,
    /// Exit codes that are not a failure. Default: 0 only.
    #[serde(default)]
    pub allow_exit: Vec<u8>,
    #[serde(default = "default_timeout")]
    pub timeout_s: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    pub name: String,
    #[serde(default = "one")]
    pub attempts: u32,
}

fn one() -> u32 {
    1
}

/// A service Proctor probes itself before blaming the code under test.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requires {
    /// URLs that must answer for a check to mean anything. Unreachable is an
    /// infrastructure error, never a finding.
    #[serde(default)]
    pub reachable: Vec<String>,
}

/// Where Proctor reads its own measurements.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    /// The fixture sellers' journal, JSON lines.
    pub journal: Option<String>,
    /// The buyer's ledger, SQLite.
    pub ledger: Option<String>,
    /// The mandate id the checks run under, for ledger measurements.
    pub mandate_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub task: Meta,
    #[serde(default)]
    pub hooks: Hooks,
    #[serde(default)]
    pub requires: Requires,
    #[serde(default)]
    pub evidence: Evidence,
    #[serde(rename = "check")]
    pub checks: Vec<Check>,
    /// SHA-256 of the file, set by the loader. A run records it so a changed
    /// contract aborts the attempt.
    #[serde(skip)]
    pub hash: String,
}

impl Task {
    pub fn from_toml(text: &str) -> Result<Self, TaskError> {
        let mut task: Task = toml::from_str(text)?;
        task.hash = sha256_hex(text.as_bytes());
        if task.checks.is_empty() {
            return Err(TaskError::Field(
                "a task needs at least one check".to_owned(),
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for c in &task.checks {
            if !seen.insert(&c.name) {
                return Err(TaskError::Field(format!("two checks are named {}", c.name)));
            }
            if c.run.trim().is_empty() {
                return Err(TaskError::Field(format!("check {} has no command", c.name)));
            }
        }
        Ok(task)
    }

    pub fn load(path: &Path) -> Result<Self, TaskError> {
        let text = std::fs::read_to_string(path).map_err(|e| TaskError::File {
            path: path.display().to_string(),
            problem: e.to_string(),
        })?;
        Self::from_toml(&text)
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Reads a dotted path out of a JSON document: `totals.settled`, `steps.0.tx_id`.
pub fn at<'a>(doc: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut here = doc;
    for part in path.split('.') {
        here = match here {
            serde_json::Value::Array(a) => a.get(part.parse::<usize>().ok()?)?,
            other => other.get(part)?,
        };
    }
    Some(here)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TASK: &str = r#"
[task]
name = "recover a paid request after a lost response"
attempts = 3

[hooks]
setup = "scripts/sellers up"
ready = "scripts/sellers ready"
teardown = "scripts/sellers down"

[evidence]
journal = ".journal/sellers-journal.jsonl"
ledger = "run.sqlite"
mandate_id = "lost"

[[check]]
name = "build"
run = "cargo build --workspace"

[[check]]
name = "recovery"
run = "mandate run fixtures/lost.toml --json"
allow_exit = [0]
expect = { status = "delivered", "totals.outstanding" = "0.000000" }
observe = { fixture_settlements = 1, fixture_distinct_payloads = 1 }
"#;

    #[test]
    fn a_contract_loads_with_its_hash() {
        let t = Task::from_toml(TASK).unwrap();
        assert_eq!(t.task.name, "recover a paid request after a lost response");
        assert_eq!(t.task.attempts, 3);
        assert_eq!(t.checks.len(), 2);
        assert_eq!(t.hooks.timeout_s, 120);
        assert_eq!(t.evidence.mandate_id.as_deref(), Some("lost"));
        assert_eq!(t.hash.len(), 64);
        // The hash covers the file, so an edited contract is a different task.
        let other = Task::from_toml(&TASK.replace("attempts = 3", "attempts = 9")).unwrap();
        assert_ne!(t.hash, other.hash);
    }

    #[test]
    fn a_contract_must_name_distinct_checks_that_run_something() {
        assert!(
            Task::from_toml("[task]\nname = \"x\"\n").is_err(),
            "no checks"
        );
        let dup = TASK.replace("name = \"recovery\"", "name = \"build\"");
        assert!(
            Task::from_toml(&dup)
                .unwrap_err()
                .to_string()
                .contains("two checks are named build")
        );
        let empty = TASK.replace("run = \"cargo build --workspace\"", "run = \"\"");
        assert!(
            Task::from_toml(&empty)
                .unwrap_err()
                .to_string()
                .contains("has no command")
        );
    }

    #[test]
    fn expectations_compare_across_the_shapes_a_report_uses() {
        let doc = serde_json::json!({
            "status": "delivered",
            "totals": { "outstanding": "0.000000", "audit_spent_tinybar": 2037339 },
            "steps": [{ "tx_id": "0.0.7162784@1.2", "submissions": 3 }],
        });
        assert_eq!(at(&doc, "status").unwrap(), "delivered");
        assert_eq!(at(&doc, "steps.0.submissions").unwrap(), 3);
        assert!(at(&doc, "steps.9.tx_id").is_none());
        assert!(at(&doc, "totals.nothing").is_none());
        assert!(Expected::Text("delivered".into()).matches(at(&doc, "status").unwrap()));
        assert!(Expected::Int(3).matches(at(&doc, "steps.0.submissions").unwrap()));
        assert!(Expected::Int(2_037_339).matches(at(&doc, "totals.audit_spent_tinybar").unwrap()));
        assert!(!Expected::Int(2).matches(at(&doc, "steps.0.submissions").unwrap()));
        assert!(Expected::Text("0.000000".into()).matches(at(&doc, "totals.outstanding").unwrap()));
    }
}
