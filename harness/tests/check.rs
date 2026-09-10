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
