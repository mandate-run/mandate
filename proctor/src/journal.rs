//! Persist attempts under `.proctor/runs/<id>/`: the task's hash (frozen per
//! run — a changed hash aborts the attempt), config, check results and any
//! unresolved exposure.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

pub struct RunJournal {
    pub id: String,
    pub dir: PathBuf,
}

impl RunJournal {
    /// Create a fresh run directory under `root/.proctor/runs/<ts>-<slug>`.
    pub fn create(root: &Path, task_name: &str) -> Result<Self, String> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let slug = task_name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect::<String>();
        let id = format!("{nanos}-{slug}");
        let dir = root.join(".proctor").join("runs").join(&id);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("create {}: {e}", dir.display()))?;
        Ok(Self { id, dir })
    }

    /// Record the frozen hashes and task file bytes. A changed hash aborts the
    /// attempt (the agent cannot alter its own contract).
    pub fn freeze_task(&self, task_path: &Path, task_bytes: &[u8]) -> Result<String, String> {
        let hash = mandate_core::fixture::sha256_hex(task_bytes);
        let checks = json!({
            "task": task_path.display().to_string(),
            "sha256": hash,
        });
        std::fs::write(self.dir.join("task.json"), serde_json::to_vec_pretty(&checks).unwrap())
            .map_err(|e| format!("write task.json: {e}"))?;
        Ok(hash)
    }

    pub fn write_report(
        &self,
        outcome_label: &str,
        results: &[crate::contract::CheckResult],
    ) -> Result<PathBuf, String> {
        let rows: Vec<_> = results
            .iter()
            .map(|r| {
                json!({
                    "name": r.name,
                    "pass": r.ok(),
                    "expect": r.expect_ok,
                    "expect_failures": r.expect_failures,
                    "observe": r.observe_ok,
                    "observe_failures": r.observe_failures,
                    "output": r.output_excerpt,
                })
            })
            .collect();
        let report = json!({
            "outcome": outcome_label,
            "checks": rows,
            "written_at": now_rfc3339(),
        });
        let path = self.dir.join("report.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&report).unwrap())
            .map_err(|e| format!("write report: {e}"))?;
        Ok(path)
    }
}

fn now_rfc3339() -> String {
    // No chrono dep needed: a coarse timestamp is fine for a journal.
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| format!("{}s", d.as_secs()))
        .unwrap_or_else(|_| "unknown".into())
}

