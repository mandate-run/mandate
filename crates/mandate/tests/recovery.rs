//! Issue 21: the failure behaviors of spec section 6, plus I5, I6 and I8, as
//! deterministic execution tests. A scripted market settles, drops responses,
//! withholds records or answers with a duplicate; the tests assert how many
//! transmissions went out, that every one carried the same signed payload and
//! payment id, what the ledger holds afterwards, and that exactly one receipt
//! exists per terminal transition.

use mandate::ledger::{Ledger, PaymentState};
use mandate::mandate::Mandate;
use mandate::manifest::Manifest;
use mandate::run::{Inputs, Report, Status, execute};
use mandate::testing::{FakeMarket, FakePublisher, MANDATE_TOML, MANIFEST_JSON};

const POOLS: [&str; 2] = [
    "0x1111111111111111111111111111111111111111",
    "0x2222222222222222222222222222222222222222",
];

/// Two pools, one material, so a run buys screen then events then explain.
fn mandate(id: &str) -> Mandate {
    let pools = POOLS
        .iter()
        .map(|p| format!("\"{p}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let text = MANDATE_TOML
        .replace("id = \"demo-1\"", &format!("id = \"{id}\""))
        .replace(
            "pools = [\"0x88E6A0C2DDD26FEEB64F039A2C41296FCB3F5640\"]",
            &format!("pools = [{pools}]"),
        )
        .replace("degrade = false", "degrade = false\nprovenance_samples = 0");
    Mandate::from_toml(&text, time::OffsetDateTime::now_utc()).expect("mandate loads")
}

/// A ledger file of its own per test, so a second run can reopen it.
struct Scratch {
    dir: std::path::PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("mandate-recovery-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }

    fn path(&self) -> std::path::PathBuf {
        self.dir.join("ledger.sqlite")
    }

    fn ledger(&self) -> Ledger {
        Ledger::open(&self.path()).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

async fn run_with(m: &Mandate, market: &FakeMarket, ledger: Ledger, resume: bool) -> Report {
    let manifest = Manifest::from_json(MANIFEST_JSON).unwrap();
    let http = reqwest::Client::new();
    let inputs = Inputs {
        mandate: m,
        manifest,
        facilitator_url: "https://api.testnet.blocky402.com".to_owned(),
        facilitator_note: None,
        topic: "0.0.10410389".parse().unwrap(),
        hashscan: "https://hashscan.io/testnet".to_owned(),
        ledger_hint: "the scratch ledger".to_owned(),
        http: &http,
        quiet: true,
        resume,
    };
    execute(inputs, ledger, market, market, &FakePublisher)
        .await
        .expect("run completes")
}

/// The general requirement: whatever the mirror node's timing, the original
/// payment is reused, submissions never exceed three, and the retrieval
/// happens as soon as settlement is known. Here the record appears at once,
/// so one submission and one retrieval suffice.
#[tokio::test]
async fn a_lost_response_never_pays_twice_whatever_the_timing() {
    let mut market = FakeMarket::new(vec![POOLS[1].to_owned()]);
    market.drop_responses.insert("events".to_owned(), 1);
    let scratch = Scratch::new("lost-fast");
    let m = mandate("lost-fast");
    let r = run_with(&m, &market, scratch.ledger(), false).await;
    assert_eq!(r.status, Status::Delivered, "{:?}", r.transcript);

    let events = r.steps.iter().find(|s| s.listing_id == "events").unwrap();
    assert!(
        events.submissions <= 3,
        "I8's cap holds: {}",
        events.submissions
    );
    assert_eq!(
        events.retrievals, 1,
        "settlement known, so retrieve at once"
    );
    assert_eq!(events.delivery_state, "received");
    let sent: std::collections::BTreeSet<String> = market
        .transmissions
        .lock()
        .unwrap()
        .iter()
        .filter(|(step, _)| step == "events")
        .map(|(_, sig)| sig.clone())
        .collect();
    assert_eq!(sent.len(), 1, "one signed payload throughout");
    assert_eq!(
        scratch
            .ledger()
            .authorizations("lost-fast")
            .unwrap()
            .iter()
            .filter(|a| a.step == "events")
            .count(),
        1,
        "one authorization, I6"
    );
}

/// Scenario 5 with a delayed record: the seller settles and drops the
/// response, and the record lags, so the buyer exhausts I8's three
/// submissions before the retrieval recovers the result.
#[tokio::test]
async fn lost_response_is_recovered_by_one_retrieval() {
    let mut market = FakeMarket::new(vec![POOLS[1].to_owned()]);
    market.drop_responses.insert("events".to_owned(), 3);
    let scratch = Scratch::new("lost");
    let m = mandate("lost");
    let r = run_with(&m, &market, scratch.ledger(), false).await;
    let ledger = scratch.ledger();
    assert_eq!(r.status, Status::Delivered, "{:?}", r.transcript);

    let events = r.steps.iter().find(|s| s.listing_id == "events").unwrap();
    assert_eq!(events.submissions, 3, "three submissions, I8's cap");
    assert_eq!(events.retrievals, 1, "one retrieval recovers the result");
    assert_eq!(events.payment_state, "settled");
    assert_eq!(events.delivery_state, "received");

    // One authorization, one payment id, one distinct signed payload.
    let auths = ledger.authorizations(&r.mandate_id).unwrap();
    let for_events: Vec<_> = auths.iter().filter(|a| a.step == "events").collect();
    assert_eq!(for_events.len(), 1, "no second authorization, I6");
    let sent: Vec<String> = market
        .transmissions
        .lock()
        .unwrap()
        .iter()
        .filter(|(step, _)| step == "events")
        .map(|(_, sig)| sig.clone())
        .collect();
    assert_eq!(sent.len(), 4, "three submissions and one retrieval");
    assert_eq!(
        sent.iter().collect::<std::collections::BTreeSet<_>>().len(),
        1,
        "every transmission carried the same signed payload"
    );

    // The report is what a clean run produces.
    let v = r.validation.as_ref().unwrap();
    assert!(v.passed && v.complete, "{:?}", v.failures);
    assert_eq!(r.totals.as_ref().unwrap().outstanding, "0.000000");
    assert!(r.explanation.is_some());
    // Exactly one receipt per terminal transition: start, three paid.
    assert_eq!(r.receipts.len(), 4);
    assert_eq!(r.receipts.iter().filter(|x| x.outcome == "paid").count(), 3);
}

/// Scenario 6: no record ever appears. The amount stays outstanding, the
/// receipt says unresolved, the report carries the reconcile notice, and the
/// run exits 6. Absence never releases the budget, I5.
#[tokio::test]
async fn an_absent_record_keeps_its_exposure_and_exits_six() {
    let mut market = FakeMarket::new(vec![POOLS[1].to_owned()]);
    market.never_settle.push("events".to_owned());
    let scratch = Scratch::new("absent");
    let m = mandate("absent");
    let r = run_with(&m, &market, scratch.ledger(), false).await;
    let ledger = scratch.ledger();

    assert_eq!(r.status, Status::Unresolved, "{:?}", r.transcript);
    assert_eq!(r.status.exit_code(), 6);
    let events = r.steps.iter().find(|s| s.listing_id == "events").unwrap();
    assert_eq!(events.payment_state, "unresolved");
    assert_eq!(events.records, 0);

    let totals = r.totals.as_ref().unwrap();
    assert_ne!(totals.outstanding, "0.000000", "exposure is kept");
    assert_eq!(totals.settled, "0.000400", "only the screen settled");
    assert!(
        r.reconcile_notice
            .as_deref()
            .is_some_and(|n| n.contains("mandate reconcile")),
        "{:?}",
        r.reconcile_notice
    );
    assert!(r.receipts.iter().any(|x| x.outcome == "unresolved"));
    let auths = ledger.authorizations(&r.mandate_id).unwrap();
    assert_eq!(
        auths
            .iter()
            .filter(|a| a.payment_state == PaymentState::Unresolved)
            .count(),
        1
    );
}

/// Scenario 7: a duplicate record beside the success record. The runtime
/// settles on the success record and never releases exposure early.
#[tokio::test]
async fn a_duplicate_record_is_ignored_and_the_success_settles() {
    let mut market = FakeMarket::new(vec![POOLS[1].to_owned()]);
    market.duplicate_records = true;
    let scratch = Scratch::new("dup");
    let m = mandate("dup");
    let r = run_with(&m, &market, scratch.ledger(), false).await;
    assert_eq!(r.status, Status::Delivered, "{:?}", r.transcript);
    for s in &r.steps {
        assert_eq!(
            s.records, 2,
            "{} saw the duplicate and the success",
            s.listing_id
        );
        assert_eq!(s.duplicates_ignored, 1);
        assert_eq!(s.payment_state, "settled");
    }
    assert_eq!(r.totals.as_ref().unwrap().outstanding, "0.000000");
    let v = r.validation.as_ref().unwrap();
    assert!(v.passed && v.complete, "{:?}", v.failures);
}

// Crash and resume, section 6's recovery rows and I8. The run is stopped by
// dropping it at a point, exactly as a process death would, then a second
// run over the same ledger file resumes it. Nothing is re-signed and no
// authorization is taken twice.

/// Runs until `steps` purchases have been authorized, then abandons the run,
/// leaving the ledger as a dead process would.
async fn crash_after(scratch: &Scratch, m: &Mandate, market: &FakeMarket, steps: usize) {
    market.stop_after(steps);
    let manifest = Manifest::from_json(MANIFEST_JSON).unwrap();
    let http = reqwest::Client::new();
    let inputs = Inputs {
        mandate: m,
        manifest,
        facilitator_url: "https://api.testnet.blocky402.com".to_owned(),
        facilitator_note: None,
        topic: "0.0.10410389".parse().unwrap(),
        hashscan: "https://hashscan.io/testnet".to_owned(),
        ledger_hint: "the scratch ledger".to_owned(),
        http: &http,
        quiet: true,
        resume: false,
    };
    let err = execute(inputs, scratch.ledger(), market, market, &FakePublisher)
        .await
        .expect_err("the run is abandoned at the crash point");
    assert!(err.to_string().contains("crash point"), "{err}");
    market.stop_after(usize::MAX);
}

/// A crash right after the authorization is persisted, before the first
/// send: the resumed run transmits the same bytes as submission 1.
#[tokio::test]
async fn a_crash_after_authorization_resends_the_same_payload() {
    let market = FakeMarket::new(vec![POOLS[1].to_owned()]);
    let scratch = Scratch::new("crash-auth");
    let m = mandate("crash-auth");
    crash_after(&scratch, &m, &market, 1).await;

    // The ledger holds one prepared authorization and nothing was sent.
    let ledger = scratch.ledger();
    let before = ledger.authorizations("crash-auth").unwrap();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].payment_state, PaymentState::Prepared);
    assert_eq!(before[0].submissions, 0, "no send preceded the crash");
    let payload = before[0].signature.clone();
    let payment_id = before[0].payment_id.clone();
    drop(ledger);

    let r = run_with(&m, &market, scratch.ledger(), true).await;
    assert_eq!(r.status, Status::Delivered, "{:?}", r.transcript);
    assert!(
        r.transcript
            .iter()
            .any(|l| l.contains("resuming 1 authorization")),
        "{:?}",
        r.transcript
    );

    let ledger = scratch.ledger();
    let after = ledger.authorizations("crash-auth").unwrap();
    let same: Vec<_> = after
        .iter()
        .filter(|a| a.payment_id == payment_id)
        .collect();
    assert_eq!(
        same.len(),
        1,
        "no second authorization for the purchase, I6"
    );
    assert_eq!(same[0].signature, payload, "the same bytes were resent");
    assert_eq!(same[0].submissions, 1, "the resumed send is submission 1");
    assert_eq!(same[0].payment_state, PaymentState::Settled);
    // Every purchase the mandate needs is settled and the report is whole.
    let v = r.validation.as_ref().unwrap();
    assert!(v.passed && v.complete, "{:?}", v.failures);
    assert_eq!(r.totals.as_ref().unwrap().outstanding, "0.000000");
}

/// A crash after the screen is delivered: the resumed run keeps that
/// evidence, buys only what is still missing, and never re-buys the screen.
#[tokio::test]
async fn a_crash_after_a_delivery_keeps_the_evidence() {
    let market = FakeMarket::new(vec![POOLS[1].to_owned()]);
    let scratch = Scratch::new("crash-delivery");
    let m = mandate("crash-delivery");
    crash_after(&scratch, &m, &market, 2).await;

    let ledger = scratch.ledger();
    let before = ledger.authorizations("crash-delivery").unwrap();
    assert_eq!(before.len(), 2, "screen and events were authorized");
    let ids: Vec<String> = before.iter().map(|a| a.payment_id.clone()).collect();
    drop(ledger);

    let r = run_with(&m, &market, scratch.ledger(), true).await;
    assert_eq!(r.status, Status::Delivered, "{:?}", r.transcript);

    let ledger = scratch.ledger();
    let after = ledger.authorizations("crash-delivery").unwrap();
    // The two recovered purchases keep their ids; only the explanation is new.
    for id in &ids {
        assert_eq!(
            after.iter().filter(|a| &a.payment_id == id).count(),
            1,
            "the recovered purchase kept its payment id"
        );
    }
    assert_eq!(after.len(), 3, "screen, events, explain: nothing re-bought");
    assert_eq!(
        after.iter().filter(|a| a.step == "screen").count(),
        1,
        "the screen was never bought twice"
    );
    let v = r.validation.as_ref().unwrap();
    assert!(v.passed && v.complete, "{:?}", v.failures);
    assert!(r.explanation.is_some());
    assert_eq!(r.totals.as_ref().unwrap().outstanding, "0.000000");
}

// The review's probes, as tests. Each asserts what the ledger holds after a
// second run, because that is where double spending would show.

/// Resuming a run that already finished buys nothing and reports the same
/// thing again. The evidence is restored from the ledger, not repurchased.
#[tokio::test]
async fn resuming_a_finished_run_buys_nothing() {
    let market = FakeMarket::new(vec![POOLS[1].to_owned()]);
    let scratch = Scratch::new("resume-complete");
    let m = mandate("resume-complete");

    let first = run_with(&m, &market, scratch.ledger(), false).await;
    assert_eq!(first.status, Status::Delivered, "{:?}", first.transcript);
    let bought = market.authorized.lock().unwrap().len();
    assert_eq!(bought, 3, "screen, events, explain");
    let receipts_before = first.receipts.len();

    let second = run_with(&m, &market, scratch.ledger(), true).await;
    assert_eq!(second.status, Status::Delivered, "{:?}", second.transcript);
    assert_eq!(
        market.authorized.lock().unwrap().len(),
        bought,
        "a finished run authorizes nothing on resume"
    );
    let ledger = scratch.ledger();
    assert_eq!(
        ledger.authorizations("resume-complete").unwrap().len(),
        3,
        "no purchase was made twice"
    );
    // The report is whole again, from the ledger alone.
    let v = second.validation.as_ref().unwrap();
    assert!(v.passed && v.complete, "{:?}", v.failures);
    assert_eq!(second.outcomes.len(), 2);
    assert!(second.explanation.is_some(), "the explanation was restored");
    assert_eq!(
        second.receipts.len(),
        receipts_before,
        "no receipt was written twice"
    );
    assert_eq!(
        second
            .receipts
            .iter()
            .filter(|r| r.outcome == "start")
            .count(),
        1,
        "one start receipt for the run"
    );
    assert_eq!(
        second.totals.as_ref().unwrap().settled,
        first.totals.as_ref().unwrap().settled
    );
}

/// A crash after the final response is persisted: the resumed run validates
/// what it already paid for and buys no second explanation.
#[tokio::test]
async fn a_crash_after_the_last_response_buys_no_second_explanation() {
    let market = FakeMarket::new(vec![POOLS[1].to_owned()]);
    let scratch = Scratch::new("crash-final");
    let m = mandate("crash-final");
    // Stop once all three purchases exist, with the last body persisted.
    crash_after(&scratch, &m, &market, 3).await;

    let before = scratch.ledger().authorizations("crash-final").unwrap();
    assert_eq!(before.len(), 3);

    let r = run_with(&m, &market, scratch.ledger(), true).await;
    assert_eq!(r.status, Status::Delivered, "{:?}", r.transcript);
    let after = scratch.ledger().authorizations("crash-final").unwrap();
    assert_eq!(after.len(), 3, "no fourth authorization");
    assert_eq!(
        after.iter().filter(|a| a.step == "explain").count(),
        1,
        "the explanation was never bought twice"
    );
    assert!(r.explanation.is_some());
    assert_eq!(r.totals.as_ref().unwrap().outstanding, "0.000000");
}

/// A crash leaves the completion hold in the ledger. The resumed run adopts
/// it rather than holding a second time, and completion leaves nothing held.
#[tokio::test]
async fn a_resumed_run_adopts_the_completion_reserve() {
    let market = FakeMarket::new(vec![POOLS[1].to_owned()]);
    let scratch = Scratch::new("reserve");
    let m = mandate("reserve");
    crash_after(&scratch, &m, &market, 1).await;

    // The interrupted run held the explanation reserve.
    let held = scratch.ledger().accounts("reserve").unwrap().held;
    assert!(held > 0, "the crash left a completion hold");

    let r = run_with(&m, &market, scratch.ledger(), true).await;
    assert_eq!(r.status, Status::Delivered, "{:?}", r.transcript);
    assert!(
        r.transcript
            .iter()
            .any(|l| l.contains("adopting the completion reserve")),
        "{:?}",
        r.transcript
    );
    assert_eq!(
        r.totals.as_ref().unwrap().held,
        "0.000000",
        "completion leaves nothing held"
    );
    let accounts = scratch.ledger().accounts("reserve").unwrap();
    assert_eq!(accounts.held, 0, "the budget is not stranded");
}

/// The window is the task: a resume restores the one the purchases were made
/// for, whatever the clock says now.
#[tokio::test]
async fn a_resume_keeps_the_original_window() {
    let market = FakeMarket::new(vec![POOLS[1].to_owned()]);
    let scratch = Scratch::new("window");
    let m = mandate("window");
    crash_after(&scratch, &m, &market, 1).await;
    let stored = scratch.ledger().mandate("window").unwrap().window;
    assert!(stored.1 > stored.0, "the window was persisted");

    let r = run_with(&m, &market, scratch.ledger(), true).await;
    assert_eq!(
        (r.window.from, r.window.to),
        stored,
        "the resumed run investigates the window it already paid for"
    );
    assert_eq!(r.status, Status::Delivered, "{:?}", r.transcript);
}
