//! Issue 10 durability: the state machine is run in a child process that is
//! killed at a failure point; the parent reopens the ledger file and checks
//! that recovery resumes exactly where section 6 says, with the same bytes and
//! the same payment id. The child is this test binary re-run with
//! `MANDATE_CRASH_POINT` set.

use std::path::PathBuf;
use std::process::Command;

use mandate::hedera::Settlement;
use mandate::ledger::{
    DeliveryState, Ledger, MandateRow, PaymentState, PreparedPayment, Request, ReservationSource,
};
use mandate::purchase::{Resume, plan_recovery};
use time::macros::datetime;
use time::{Duration, OffsetDateTime};

const NOW: OffsetDateTime = datetime!(2026-09-08 09:00 UTC);

fn row() -> MandateRow {
    MandateRow {
        id: "m1".to_owned(),
        mandate_hash: "a".repeat(64),
        manifest_hash: "b".repeat(64),
        service_total: 10_000,
        service_asset: "0.0.429274".to_owned(),
        audit_total: 50_000_000,
        max_single_payment: 9_000,
        deadline: datetime!(2026-09-13 16:00 UTC),
    }
}

fn payment() -> PreparedPayment {
    PreparedPayment {
        payment_id: "pay_00000000000000000000000000000001".to_owned(),
        tx_id: "0.0.7162784@1788800000.000000001".to_owned(),
        mirror_id: "0.0.7162784-1788800000-000000001".to_owned(),
        payer: "0.0.10399984".to_owned(),
        amount: 1_500,
        asset: "0.0.429274".to_owned(),
        pay_to: "0.0.10409989".to_owned(),
        fee_payer: "0.0.7162784".to_owned(),
        valid_start: NOW - Duration::seconds(5),
        valid_until: NOW + Duration::seconds(115),
        signature: "the-exact-header".to_owned(),
        quote_json: "{}".to_owned(),
    }
}

fn request() -> Request {
    Request {
        method: "POST".to_owned(),
        url: "http://127.0.0.1:4021/events".to_owned(),
        headers: vec![("content-type".to_owned(), "application/json".to_owned())],
        body: br#"{"pools":["0xabc"]}"#.to_vec(),
    }
}

/// The child: runs up to the failure point, then dies without cleanup.
#[test]
fn crash_child() {
    let Ok(point) = std::env::var("MANDATE_CRASH_POINT") else {
        return;
    };
    let path = PathBuf::from(std::env::var("MANDATE_CRASH_LEDGER").unwrap());
    let mut ledger = Ledger::open(&path).unwrap();
    ledger.insert_mandate(&row(), NOW).unwrap();
    ledger
        .hold("m1", "explain", 800, ReservationSource::CeilingAtMax, NOW)
        .unwrap();
    let a = ledger
        .prepare("m1", "events", None, &payment(), &request(), NOW)
        .unwrap();
    if point == "after_authorization" {
        std::process::abort();
    }
    ledger.commit_submission(a.id, NOW).unwrap();
    if point == "after_counter" {
        std::process::abort();
    }
    ledger
        .record_delivery(a.id, b"{\"events\":[]}", Some("pr"), NOW)
        .unwrap();
    if point == "after_response" {
        std::process::abort();
    }
    panic!("unknown crash point {point}");
}

fn run_child(point: &str) -> (PathBuf, Ledger) {
    let dir = std::env::temp_dir().join(format!("mandate-crash-{}-{point}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ledger.sqlite");
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "crash_child", "--nocapture", "--test-threads=1"])
        .env("MANDATE_CRASH_POINT", point)
        .env("MANDATE_CRASH_LEDGER", &path)
        .status()
        .unwrap();
    assert!(!status.success(), "the child must die at {point}");
    let ledger = Ledger::open(&path).unwrap();
    (dir, ledger)
}

#[test]
fn crash_after_authorization_resends_the_same_bytes_with_a_first_submission() {
    let (dir, ledger) = run_child("after_authorization");
    let plan = plan_recovery(&ledger, "m1", NOW + Duration::seconds(10)).unwrap();
    assert_eq!(plan.len(), 1);
    let Resume::Resend(a) = &plan[0] else {
        panic!("expected resend, got {plan:?}");
    };
    assert_eq!(
        (a.payment_state, a.submissions),
        (PaymentState::Prepared, 0)
    );
    assert_eq!(a.signature, "the-exact-header");
    assert_eq!(a.payment_id, payment().payment_id);
    assert_eq!(a.request, request());
    assert_eq!(ledger.accounts("m1").unwrap().outstanding, 1_500);
    assert_eq!(ledger.accounts("m1").unwrap().held, 800);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn crash_after_counter_costs_one_attempt_and_never_bypasses_the_cap() {
    let (dir, mut ledger) = run_child("after_counter");
    let plan = plan_recovery(&ledger, "m1", NOW + Duration::seconds(10)).unwrap();
    let Resume::Resend(a) = &plan[0] else {
        panic!("expected resend, got {plan:?}");
    };
    assert_eq!(
        (a.payment_state, a.submissions),
        (PaymentState::Sent, 1),
        "the lost send still counts"
    );
    let second = ledger
        .commit_submission(a.id, NOW + Duration::seconds(10))
        .unwrap();
    assert_eq!(
        (second.submissions, second.signature.as_str()),
        (2, "the-exact-header")
    );
    ledger
        .commit_submission(a.id, NOW + Duration::seconds(20))
        .unwrap();
    assert!(
        ledger
            .commit_submission(a.id, NOW + Duration::seconds(30))
            .is_err(),
        "three is the cap"
    );
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn crash_after_response_resumes_at_reconcile_then_validation() {
    let (dir, mut ledger) = run_child("after_response");
    let a = ledger.authorizations("m1").unwrap().remove(0);
    assert_eq!(
        (a.payment_state, a.delivery_state),
        (PaymentState::Sent, DeliveryState::Received)
    );
    assert_eq!(a.response_body.as_deref(), Some(&b"{\"events\":[]}"[..]));
    let before = plan_recovery(&ledger, "m1", NOW + Duration::seconds(10)).unwrap();
    assert!(
        matches!(before[0], Resume::AwaitRecord(_)),
        "a received delivery is never resent"
    );
    ledger
        .record_settlement(
            a.id,
            &Settlement::Settled {
                consensus_timestamp: "1788800010.1".to_owned(),
                duplicates_ignored: 0,
            },
            NOW + Duration::seconds(12),
        )
        .unwrap();
    let after = plan_recovery(&ledger, "m1", NOW + Duration::seconds(12)).unwrap();
    assert!(matches!(after[0], Resume::Validate(_)));
    ledger
        .mark_validated(a.id, NOW + Duration::seconds(13))
        .unwrap();
    assert!(
        plan_recovery(&ledger, "m1", NOW + Duration::seconds(14))
            .unwrap()
            .is_empty()
    );
    std::fs::remove_dir_all(dir).ok();
}
