//! Persists what a run did under `.proctor/runs/<id>/`: the attempts, their
//! checks and findings, the hashes of the task and the verifier, and the
//! measurements. The report is the evidence a reviewer reads, so it is
//! written even when the run fails.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::checks::{Attempt, Outcome};

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub run: String,
    pub task: String,
    pub task_hash: String,
    /// The verifier's own version, so a report says what judged it.
    pub proctor_version: String,
    pub outcome: Outcome,
    pub attempts: Vec<Attempt>,
    pub started_at: String,
    pub finished_at: String,
}

impl Report {
    pub fn new(task: &crate::task::Task, started_at: String) -> Self {
        Self {
            run: run_id(),
            task: task.task.name.clone(),
            task_hash: task.hash.clone(),
            proctor_version: env!("CARGO_PKG_VERSION").to_owned(),
            outcome: Outcome::Pass,
            attempts: Vec::new(),
            started_at,
            finished_at: String::new(),
        }
    }

    pub fn finish(mut self, attempts: Vec<Attempt>) -> Self {
        self.outcome = attempts
            .last()
            .map(|a| a.outcome)
            .unwrap_or(Outcome::InfrastructureError);
        self.attempts = attempts;
        self.finished_at = now();
        self
    }

    /// Writes `report.json` under the run directory and returns its path.
    pub fn write(&self, root: &Path) -> std::io::Result<PathBuf> {
        let dir = root.join(".proctor/runs").join(&self.run);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("report.json");
        std::fs::write(&path, serde_json::to_vec_pretty(self)?)?;
        Ok(path)
    }
}

pub fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// A run id that sorts by time and needs no coordination.
pub fn run_id() -> String {
    let now = time::OffsetDateTime::now_utc();
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        now.year(),
        now.month() as u8,
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checks::{CheckResult, Measurements};
    use crate::task::Task;

    const TASK: &str = r#"
[task]
name = "t"
[[check]]
name = "c"
run = "true"
"#;

    #[test]
    fn a_report_records_what_judged_it_and_survives_a_failure() {
        let task = Task::from_toml(TASK).unwrap();
        let failed = CheckResult {
            name: "c".into(),
            outcome: Outcome::ImplementationFailure,
            exit_code: Some(1),
            ms: 2,
            assertions: vec![],
            findings: vec!["c: exit 1".into()],
        };
        let attempt = Attempt::new(&task, 1, vec![failed], Measurements::new());
        let report = Report::new(&task, now()).finish(vec![attempt]);
        assert_eq!(report.outcome, Outcome::ImplementationFailure);
        assert_eq!(report.task_hash, task.hash);
        assert!(!report.proctor_version.is_empty());

        let dir = std::env::temp_dir().join(format!("proctor-report-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = report.write(&dir).unwrap();
        let back: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(back["outcome"], "IMPLEMENTATION_FAILURE");
        assert_eq!(back["task_hash"], task.hash);
        assert_eq!(back["attempts"][0]["checks"][0]["findings"][0], "c: exit 1");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_run_with_no_attempt_is_an_infrastructure_error() {
        let task = Task::from_toml(TASK).unwrap();
        let report = Report::new(&task, now()).finish(vec![]);
        assert_eq!(report.outcome, Outcome::InfrastructureError);
    }
}
