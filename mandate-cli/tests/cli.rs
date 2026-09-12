use std::path::PathBuf;

use mandate_core::cli::exec::{self, DEFAULT_LEDGER, DEFAULT_MANIFEST};
use mandate_core::cli::{InitCommand, LedgerCommand, ReceiptsCommand, ReconcileCommand, RunCommand};
use mandate_core::fixture::Scenario;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "mandate-cli-test-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn init_then_run_then_read_back() {
    let dir = temp_dir("roundtrip");
    let init = InitCommand {
        out_dir: dir.clone(),
        principal: "0.0.123456".into(),
        purpose: "Explain any material liquidity change in the listed pools over the last 24 hours."
            .into(),
        service_total: 10_000, // 0.0100 USDC
        audit_total: 50_000_000, // 0.5 HBAR
    };
    exec::execute_init(&init).unwrap();
    assert!(dir.join("mandate.json").exists());
    assert!(dir.join(DEFAULT_MANIFEST).exists());
    assert!(dir.join(DEFAULT_LEDGER).exists());

    let run = RunCommand {
        mandate_path: dir.join("mandate.json"),
        manifest_path: dir.join(DEFAULT_MANIFEST),
        ledger_path: dir.join(DEFAULT_LEDGER),
        transcript_path: dir.join("transcript.json"),
        scenario: Scenario::Normal,
        json: false,
        live: false,
        sellers_url: None,
    };
    let transcript = exec::execute_run(&run).unwrap();
    assert_eq!(transcript.totals.settled, "0.0028");
    assert_eq!(transcript.totals.unspent, "0.0072");
    assert!(transcript.totals.audit_spent.starts_with("0.000"));

    // The ledger file persisted the authorizations and receipts.
    let ledger = LedgerCommand {
        mandate_id: "dev-mandate".into(),
        ledger_path: dir.join(DEFAULT_LEDGER),
        json: false,
    };
    let text = exec::execute_ledger(&ledger).unwrap();
    assert!(text.contains("settled 0.0028"), "ledger: {text}");
    assert!(text.contains("payment Settled"), "ledger: {text}");

    let receipts = ReceiptsCommand {
        mandate_id: "dev-mandate".into(),
        ledger_path: dir.join(DEFAULT_LEDGER),
        json: false,
    };
    let text = exec::execute_receipts(&receipts).unwrap();
    assert!(text.contains("seq 0 init"));
    assert!(text.contains("seq 1 screen"));

    let reconcile = ReconcileCommand {
        mandate_id: "dev-mandate".into(),
        mandate_path: dir.join("mandate.json"),
        ledger_path: dir.join(DEFAULT_LEDGER),
        json: false,
    };
    let text = exec::execute_reconcile(&reconcile).unwrap();
    assert!(text.contains("0 unresolved"), "reconcile: {text}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn run_refusal_persists_no_authorizations() {
    let dir = temp_dir("refusal");
    let mut init = InitCommand {
        out_dir: dir.clone(),
        principal: "0.0.123456".into(),
        purpose: "purpose".into(),
        service_total: 10_000,
        audit_total: 50_000_000,
    };
    exec::execute_init(&init).unwrap();

    init.service_total = 3_000; // 0.0030: nothing fits
    // Re-write the mandate with the small budget by re-initing the service total.
    let mandate = {
        let m: mandate_core::types::Mandate =
            exec::read_json(&dir.join("mandate.json")).unwrap();
        m
    };
    let _ = mandate;
    // Simpler: patch the mandate file directly.
    let mut m: mandate_core::types::Mandate =
        exec::read_json(&dir.join("mandate.json")).unwrap();
    m.budget.service.total = 3_000;
    exec::write_json(&dir.join("mandate.json"), &m).unwrap();

    let run = RunCommand {
        mandate_path: dir.join("mandate.json"),
        manifest_path: dir.join(DEFAULT_MANIFEST),
        ledger_path: dir.join(DEFAULT_LEDGER),
        transcript_path: dir.join("transcript.json"),
        scenario: Scenario::Refusal,
        json: false,
        live: false,
        sellers_url: None,
    };
    let transcript = exec::execute_run(&run).unwrap();
    assert!(transcript.steps.is_empty());
    assert_eq!(transcript.refusals.len(), 1);
    assert_eq!(transcript.refusals[0].code, "REQUIREMENT_UNMEETABLE");
    assert_eq!(transcript.totals.settled, "0.0000");

    let ledger = LedgerCommand {
        mandate_id: "dev-mandate".into(),
        ledger_path: dir.join(DEFAULT_LEDGER),
        json: false,
    };
    let text = exec::execute_ledger(&ledger).unwrap();
    assert!(text.contains("settled 0.0000"), "ledger: {text}");

    let _ = std::fs::remove_dir_all(&dir);
}