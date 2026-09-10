//! CLI tests use inert commands and synthetic local ledgers, never a wallet.
use mandate::{
    ledger::Ledger,
    testing::{mandate_row, request, signed_payment},
};
use std::{path::PathBuf, process::Command};

struct Work(PathBuf);
impl Work {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "proctor-cli-{}-{}-{}",
            std::process::id(),
            time::OffsetDateTime::now_utc().unix_timestamp_nanos(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn check(&self, text: &str) -> (std::process::Output, serde_json::Value) {
        let task = self.0.join("task.toml");
        std::fs::write(&task, text).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_proctor"))
            .arg("check")
            .arg(task)
            .arg("--dir")
            .arg(&self.0)
            .arg("--json")
            .output()
            .unwrap();
        let report = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&output.stdout)));
        (output, report)
    }
    fn run(&self, text: &str, attempts: u32) -> (std::process::Output, serde_json::Value) {
        let task = self.0.join("task.toml");
        std::fs::write(&task, text).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_proctor"))
            .arg("run")
            .arg(task)
            .arg("--dir")
            .arg(&self.0)
            .arg("--attempts")
            .arg(attempts.to_string())
            .arg("--json")
            .output()
            .unwrap();
        let report = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&output.stdout)));
        (output, report)
    }
    /// An adapter that records every invocation and repairs the fault.
    fn adapter(&self, body: &str) -> String {
        let path = self.0.join("adapter.sh");
        std::fs::write(&path, format!("#!/bin/sh\ncat >> briefs.jsonl\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        "sh ./adapter.sh".to_owned()
    }
    fn briefs(&self) -> Vec<serde_json::Value> {
        let path = self.0.join("briefs.jsonl");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Vec::new();
        };
        // Each brief is pretty-printed, so the file is a JSON stream.
        let mut out = Vec::new();
        let mut de = serde_json::Deserializer::from_str(&text).into_iter();
        while let Some(Ok(v)) = de.next() {
            out.push(v);
        }
        out
    }
    fn pending(&self) {
        let now = time::OffsetDateTime::now_utc();
        let mut ledger = Ledger::open(&self.0.join("ledger.sqlite")).unwrap();
        ledger.insert_mandate(&mandate_row(), now).unwrap();
        ledger
            .prepare(
                "m1",
                "events",
                None,
                &signed_payment(1, 1500, "0.0.429274", now),
                &request(),
                now,
            )
            .unwrap();
    }
}
impl Drop for Work {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn preexisting_payment_outranks_errors_and_stops_new_work() {
    let work = Work::new();
    work.pending();
    std::fs::write(work.0.join("journal.jsonl"), "malformed payment\n").unwrap();
    let (out, report) = work.check(
        r#"
[task]
name = "existing exposure"
[hooks]
teardown = "exit 2"
[evidence]
ledger = "ledger.sqlite"
mandate_id = "m1"
journal = "journal.jsonl"
[[check]]
name = "must not run"
run = "touch paid-again"
"#,
    );
    assert!(!out.status.success());
    assert_eq!(report["outcome"], "PAYMENT_UNRESOLVED");
    assert_eq!(report["attempts"][0]["observed"]["ledger_prepared"], 1);
    assert!(
        !report["attempts"][0]["infrastructure"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(!work.0.join("paid-again").exists());
    let saved = work
        .0
        .join(".proctor/runs")
        .join(report["run"].as_str().unwrap())
        .join("report.json");
    assert!(saved.exists());
}

#[test]
fn failed_prerequisite_stops_later_commands_but_runs_teardown() {
    let work = Work::new();
    let (_, report) = work.check(
        r#"
[task]
name = "failed build"
[hooks]
teardown = "touch cleaned"
[[check]]
name = "compile"
run = "exit 1"
[[check]]
name = "paid"
run = "touch paid"
"#,
    );
    assert_eq!(report["outcome"], "IMPLEMENTATION_FAILURE");
    assert!(!work.0.join("paid").exists());
    assert!(work.0.join("cleaned").exists());
}

#[test]
fn missing_post_run_evidence_cannot_certify_success() {
    let work = Work::new();
    let (_, report) = work.check(
        r#"
[task]
name = "missing evidence"
[evidence]
ledger = "ledger.sqlite"
mandate_id = "m1"
[[check]]
name = "run"
run = "true"
"#,
    );
    assert_eq!(report["outcome"], "INFRASTRUCTURE_ERROR");
    assert!(
        !report["attempts"][0]["infrastructure"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_second_check_cannot_run_setup_or_teardown() {
    let work = Work::new();
    std::fs::create_dir_all(work.0.join(".proctor")).unwrap();
    let lock = std::fs::File::create(work.0.join(".proctor/check.lock")).unwrap();
    lock.try_lock().unwrap();
    let (_, report) = work.check(
        r#"
[task]
name = "concurrent check"
[hooks]
setup = "touch replaced"
teardown = "touch stopped"
[[check]]
name = "run"
run = "true"
"#,
    );
    assert_eq!(report["outcome"], "INFRASTRUCTURE_ERROR");
    assert!(!work.0.join("replaced").exists());
    assert!(!work.0.join("stopped").exists());
}

#[test]
fn the_loop_drives_the_agent_until_the_contract_passes() {
    let work = Work::new();
    std::fs::write(work.0.join("answer"), "1").unwrap();
    // The adapter is the only thing that edits the worktree.
    let adapter = work.adapter("expr $(cat answer) + 1 > next && mv next answer");
    let (out, report) = work.run(
        &format!(
            r#"
[task]
name = "the answer must be three"
attempts = 5
[agent]
adapter = "{adapter}"
[[check]]
name = "answer"
run = "printf '{{\"value\": %s}}' $(cat answer)"
expect = {{ value = 3 }}
"#
        ),
        5,
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(report["outcome"], "PASS");
    assert_eq!(report["attempts"].as_array().unwrap().len(), 3);
    // Two failures, two agent invocations, then a pass that calls nobody.
    let briefs = work.briefs();
    assert_eq!(
        briefs.len(),
        2,
        "the passing attempt must not call an agent"
    );
    assert_eq!(
        briefs[0]["attempt"], 2,
        "a brief names the attempt it is for"
    );
    assert_eq!(briefs[0]["outcome"], "IMPLEMENTATION_FAILURE");
    assert!(
        briefs[0]["findings"][0]
            .as_str()
            .unwrap()
            .contains("found 1"),
        "the agent is told what was actually there: {:?}",
        briefs[0]["findings"]
    );
    // The contract itself reaches the agent, not a summary of it.
    assert!(briefs[0]["task_toml"].as_str().unwrap().contains("[task]"));
    assert_eq!(briefs[0]["task_hash"], report["task_hash"]);
}

#[test]
fn an_exposed_ledger_never_reaches_the_agent() {
    // The rule the whole harness exists for: with money neither spent nor
    // free, editing the code and running again could sign a second payment
    // against the first. No adapter call, whatever attempts allow.
    let work = Work::new();
    work.pending();
    let adapter = work.adapter("echo edited > edited-the-code");
    let (out, report) = work.run(
        &format!(
            r#"
[task]
name = "exposure stops the loop"
attempts = 5
[agent]
adapter = "{adapter}"
[evidence]
ledger = "ledger.sqlite"
mandate_id = "m1"
[[check]]
name = "must not run"
run = "touch paid-again"
"#
        ),
        5,
    );
    assert!(!out.status.success());
    assert_eq!(report["outcome"], "PAYMENT_UNRESOLVED");
    assert_eq!(report["attempts"].as_array().unwrap().len(), 1, "no retry");
    assert!(work.briefs().is_empty(), "an agent was handed exposure");
    assert!(!work.0.join("edited-the-code").exists());
    assert!(!work.0.join("paid-again").exists());
}

#[test]
fn an_adapter_that_cannot_run_is_infrastructure_not_a_verdict() {
    // A missing model or a crashed adapter says nothing about the code, so
    // it must not be reported as the code failing.
    let work = Work::new();
    let (out, report) = work.run(
        r#"
[task]
name = "the adapter is broken"
attempts = 4
[agent]
adapter = "echo no model >&2; exit 7"
[[check]]
name = "always fails"
run = "printf '{\"value\": 1}'"
expect = { value = 3 }
"#,
        4,
    );
    assert_eq!(out.status.code(), Some(2), "INFRASTRUCTURE_ERROR");
    assert_eq!(report["outcome"], "INFRASTRUCTURE_ERROR");
    assert_eq!(report["attempts"].as_array().unwrap().len(), 1);
    let problems = report["attempts"][0]["infrastructure"].as_array().unwrap();
    assert!(
        problems
            .iter()
            .any(|p| p.as_str().unwrap().contains("no model")),
        "the adapter's own words reach the report: {problems:?}"
    );
}

#[test]
fn a_task_naming_no_adapter_is_told_so_rather_than_looping() {
    let work = Work::new();
    let (out, _) = work.run(
        r#"
[task]
name = "nothing to drive"
[[check]]
name = "fails"
run = "printf '{\"value\": 1}'"
expect = { value = 3 }
"#,
        3,
    );
    assert_eq!(out.status.code(), Some(2));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("agent.adapter"), "{text}");
}

#[test]
fn the_contract_bounds_the_attempts_a_flag_asks_for() {
    // A task that allows two attempts is not made to allow nine.
    let work = Work::new();
    let adapter = work.adapter("true");
    let (_, report) = work.run(
        &format!(
            r#"
[task]
name = "two attempts only"
attempts = 2
[agent]
adapter = "{adapter}"
[[check]]
name = "always fails"
run = "printf '{{\"value\": 1}}'"
expect = {{ value = 3 }}
"#
        ),
        9,
    );
    assert_eq!(report["outcome"], "IMPLEMENTATION_FAILURE");
    assert_eq!(report["attempts"].as_array().unwrap().len(), 2);
    assert_eq!(
        work.briefs().len(),
        1,
        "only the non-final attempt calls out"
    );
}

#[test]
fn a_contract_edited_mid_run_aborts_rather_than_judging_two_tasks() {
    // Every attempt before the edit was judged against a different contract,
    // so a report covering both would be evidence for neither.
    let work = Work::new();
    let task_path = work.0.join("task.toml");
    let adapter = work.adapter(&format!(
        "sed 's/value = 3/value = 4/' {} > t && mv t {}",
        task_path.display(),
        task_path.display()
    ));
    let (out, report) = work.run(
        &format!(
            r#"
[task]
name = "the contract must not move"
attempts = 4
[agent]
adapter = "{adapter}"
[[check]]
name = "always fails"
run = "printf '{{\"value\": 1}}'"
expect = {{ value = 3 }}
"#
        ),
        4,
    );
    assert_eq!(out.status.code(), Some(2), "INFRASTRUCTURE_ERROR");
    assert_eq!(report["attempts"].as_array().unwrap().len(), 2);
    let problems = report["attempts"][1]["infrastructure"].as_array().unwrap();
    assert!(
        problems
            .iter()
            .any(|p| p.as_str().unwrap().contains("contract changed")),
        "{problems:?}"
    );
}
