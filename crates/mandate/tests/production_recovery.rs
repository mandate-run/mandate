//! The production payment path under recovery, not a fixture's imitation of
//! it. `Payer` is driven against a local HTTP server standing in for the
//! mirror node and the seller, so `Payer::resume_one` and `Payer::settle`
//! are the code under test. A fixture that reimplements recovery can pass
//! while these fail; that is the point of running them separately.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use mandate::hedera::{MirrorNode, Settlement};
use mandate::ledger::{DeliveryState, Ledger, PaymentState};
use mandate::purchase::Payer;
use mandate::testing::{mandate_row, request, signed_payment, test_signer};
use time::OffsetDateTime;

/// A one-route HTTP server: the mirror node's transactions endpoint, and the
/// seller's paid route. It counts what it served so a test can assert how
/// many times the runtime transmitted.
struct Stub {
    port: u16,
    pub mirror_calls: Arc<AtomicUsize>,
    pub seller_calls: Arc<AtomicUsize>,
}

fn record_json(result: &str, payer: &str, pay_to: &str, amount: i64) -> String {
    format!(
        r#"{{"transactions":[{{"transaction_id":"x","result":"{result}","nonce":0,"charged_tx_fee":1,"consensus_timestamp":"1.000000001","transfers":[],"token_transfers":[{{"token_id":"0.0.429274","account":"{payer}","amount":-{amount}}},{{"token_id":"0.0.429274","account":"{pay_to}","amount":{amount}}}]}}]}}"#
    )
}

impl Stub {
    /// `records` is what the mirror endpoint answers; `body` what the seller
    /// serves. A `None` body drops the response, as a lost delivery does.
    fn start(records: String, body: Option<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let mirror_calls = Arc::new(AtomicUsize::new(0));
        let seller_calls = Arc::new(AtomicUsize::new(0));
        let (m, s) = (mirror_calls.clone(), seller_calls.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut line = String::new();
                if BufReader::new(&stream).read_line(&mut line).is_err() {
                    continue;
                }
                let is_mirror = line.contains("/api/v1/transactions/");
                if is_mirror {
                    m.fetch_add(1, Ordering::SeqCst);
                    respond(&mut stream, 200, &records);
                } else {
                    s.fetch_add(1, Ordering::SeqCst);
                    match &body {
                        Some(b) => respond(&mut stream, 200, b),
                        // A dropped response: close without writing.
                        None => drop(stream),
                    }
                }
            }
        });
        Self {
            port,
            mirror_calls,
            seller_calls,
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    let head = format!(
        "HTTP/1.1 {status} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

fn payer<'a>(
    signer: &'a mandate::hedera::Signer,
    mirror: &'a MirrorNode,
    http: &'a reqwest::Client,
) -> Payer<'a> {
    Payer {
        signer,
        mirror,
        http,
        poll: std::time::Duration::from_millis(10),
        max_retrievals: 2,
    }
}

/// The review's SUCCESS/Absent probe: recovery observes a settled record for
/// a row that is already complete, so `settle` transmits nothing. The
/// settlement it observed must survive, or the receipt calls a paid purchase
/// unresolved.
#[tokio::test]
async fn recovery_reports_the_settlement_it_observed() {
    let now = OffsetDateTime::now_utc();
    let mut ledger = Ledger::in_memory().unwrap();
    ledger.insert_mandate(&mandate_row(), now).unwrap();
    let payment = signed_payment(1, 1_500, "0.0.429274", now);
    let id = ledger
        .prepare("m1", "events", None, &payment, &request(), now)
        .unwrap()
        .id;
    // The row is already settled and delivered: nothing left to transmit.
    ledger.commit_submission(id, now).unwrap();
    ledger
        .record_delivery(id, b"{\"ok\":true}", None, now)
        .unwrap();
    let stub = Stub::start(
        record_json("SUCCESS", "0.0.10399984", "0.0.10409989", 1_500),
        Some("{}".to_owned()),
    );
    let http = reqwest::Client::new();
    let mirror = MirrorNode::new(http.clone(), stub.url());
    let signer = test_signer();
    let p = payer(&signer, &mirror, &http);

    let purchase = p
        .resume_one(&mut ledger, id, now + time::Duration::hours(1), &mut |_| {})
        .await
        .expect("recovery completes");

    assert!(
        matches!(purchase.settlement, Settlement::Settled { .. }),
        "the observed settlement survives: {:?}",
        purchase.settlement
    );
    assert_eq!(purchase.records, 1, "the record it observed is reported");
    assert!(
        stub.mirror_calls.load(Ordering::SeqCst) >= 1,
        "settlement came from the mirror node, not the seller"
    );
    assert_eq!(purchase.authorization.payment_state, PaymentState::Settled);
    assert_eq!(
        stub.seller_calls.load(Ordering::SeqCst),
        0,
        "nothing was transmitted"
    );
}

/// Recovery of a prepared row whose payment already settled before any
/// response was seen, the `prepared -> settled` row of section 6: the
/// production payer retrieves with the stored bytes, never re-signs, and
/// never authorizes a second time.
#[tokio::test]
async fn recovery_of_a_prepared_row_uses_the_stored_bytes() {
    let now = OffsetDateTime::now_utc();
    let stub = Stub::start(
        record_json("SUCCESS", "0.0.10399984", "0.0.10409989", 1_500),
        Some(r#"{"served":true}"#.to_owned()),
    );
    let mut ledger = Ledger::in_memory().unwrap();
    ledger.insert_mandate(&mandate_row(), now).unwrap();
    let payment = signed_payment(1, 1_500, "0.0.429274", now);
    let mut req = request();
    req.url = format!("{}/events", stub.url());
    let a = ledger
        .prepare("m1", "events", None, &payment, &req, now)
        .unwrap();
    assert_eq!(a.submissions, 0, "the crash preceded any send");
    let signature = a.signature.clone();

    let http = reqwest::Client::new();
    let mirror = MirrorNode::new(http.clone(), stub.url());
    let signer = test_signer();
    let p = payer(&signer, &mirror, &http);
    let purchase = p
        .resume_one(
            &mut ledger,
            a.id,
            now + time::Duration::hours(1),
            &mut |_| {},
        )
        .await
        .expect("recovery completes");

    assert_eq!(
        purchase.authorization.payment_id, payment.payment_id,
        "the same payment id"
    );
    assert_eq!(
        purchase.authorization.signature, signature,
        "the same bytes, never re-signed"
    );
    assert_eq!(purchase.authorization.payment_state, PaymentState::Settled);
    assert_eq!(
        purchase.authorization.delivery_state,
        DeliveryState::Received
    );
    assert_eq!(
        purchase.body.as_deref(),
        Some(br#"{"served":true}"#.as_slice())
    );
    assert!(matches!(purchase.settlement, Settlement::Settled { .. }));
    assert_eq!(
        purchase.authorization.submissions + purchase.authorization.retrievals,
        1,
        "exactly one transmission recovered the result"
    );
    assert_eq!(stub.seller_calls.load(Ordering::SeqCst), 1);
    assert_eq!(ledger.authorizations("m1").unwrap().len(), 1);
}

/// A settled payment whose response was lost: the production payer retrieves
/// with the original payment rather than paying again.
#[tokio::test]
async fn a_settled_row_without_a_delivery_is_retrieved() {
    let now = OffsetDateTime::now_utc();
    let stub = Stub::start(
        record_json("SUCCESS", "0.0.10399984", "0.0.10409989", 1_500),
        Some(r#"{"recovered":true}"#.to_owned()),
    );
    let mut ledger = Ledger::in_memory().unwrap();
    ledger.insert_mandate(&mandate_row(), now).unwrap();
    let payment = signed_payment(2, 1_500, "0.0.429274", now);
    let mut req = request();
    req.url = format!("{}/events", stub.url());
    let a = ledger
        .prepare("m1", "events", None, &payment, &req, now)
        .unwrap();
    // The payment settled; the response never arrived.
    ledger.commit_submission(a.id, now).unwrap();
    ledger
        .record_settlement(
            a.id,
            &Settlement::Settled {
                consensus_timestamp: "1.000000001".to_owned(),
                duplicates_ignored: 0,
            },
            now,
        )
        .unwrap();

    let http = reqwest::Client::new();
    let mirror = MirrorNode::new(http.clone(), stub.url());
    let signer = test_signer();
    let p = payer(&signer, &mirror, &http);
    let purchase = p
        .resume_one(
            &mut ledger,
            a.id,
            now + time::Duration::hours(1),
            &mut |_| {},
        )
        .await
        .expect("recovery completes");

    assert_eq!(
        purchase.authorization.submissions, 1,
        "no second submission"
    );
    assert_eq!(purchase.authorization.retrievals, 1, "one retrieval");
    assert_eq!(
        purchase.authorization.delivery_state,
        DeliveryState::Received
    );
    assert_eq!(
        purchase.body.as_deref(),
        Some(br#"{"recovered":true}"#.as_slice())
    );
    assert_eq!(
        ledger.authorizations("m1").unwrap().len(),
        1,
        "the purchase was never authorized twice"
    );
}

/// Issue 21's audit follow-through: a late reconciliation resolves the
/// exposure and records that it did, without erasing the `unresolved`
/// receipt that stated what was known at the time.
#[tokio::test]
async fn a_late_reconciliation_appends_a_resolution_receipt() {
    let now = OffsetDateTime::now_utc();
    let mut ledger = Ledger::in_memory().unwrap();
    ledger.insert_mandate(&mandate_row(), now).unwrap();
    let payment = signed_payment(3, 1_500, "0.0.429274", now);
    let a = ledger
        .prepare("m1", "events", None, &payment, &request(), now)
        .unwrap();
    ledger.commit_submission(a.id, now).unwrap();
    // No record by the grace: the exposure is unresolved and says so.
    ledger
        .record_settlement(
            a.id,
            &Settlement::Absent {
                duplicates_ignored: 0,
            },
            now + time::Duration::seconds(200),
        )
        .unwrap();
    assert_eq!(
        ledger.authorization(a.id).unwrap().payment_state,
        PaymentState::Unresolved
    );
    let unresolved = mandate::receipts::Receipt {
        seq: 0,
        mandate_id: "m1".to_owned(),
        outcome: mandate::receipts::Outcome::Unresolved,
        at: "2026-09-08T00:00:00Z".to_owned(),
        step: Some(1),
        listing_id: Some("events".to_owned()),
        seller: None,
        amount: Some("1500".to_owned()),
        asset: Some("0.0.429274".to_owned()),
        tx_id: Some(a.tx_id.clone()),
        payment_id_hash: None,
        request_hash: None,
        response_hash: None,
        reason: Some("no record".to_owned()),
        latency_ms: None,
        mandate_hash: None,
        manifest_hash: None,
        spec_version: None,
    };
    ledger.append_receipt("m1", &unresolved).unwrap();
    let before = ledger.receipts("m1").unwrap().len();

    // The record arrives later.
    let stub = Stub::start(
        record_json("SUCCESS", "0.0.10399984", "0.0.10409989", 1_500),
        Some("{}".to_owned()),
    );
    let http = reqwest::Client::new();
    let mirror = MirrorNode::new(http.clone(), stub.url());
    let done = mandate::purchase::reconcile(&mut ledger, &mirror, "m1", OffsetDateTime::now_utc())
        .await
        .expect("reconcile runs");

    assert_eq!(done.len(), 1);
    assert_eq!(done[0].before, PaymentState::Unresolved);
    assert_eq!(done[0].after, PaymentState::Settled);
    let receipts = ledger.receipts("m1").unwrap();
    assert_eq!(receipts.len(), before + 1, "the resolution is recorded");
    let last = &receipts.last().unwrap().0;
    assert_eq!(last.outcome, mandate::receipts::Outcome::Paid);
    assert!(
        last.reason
            .as_deref()
            .is_some_and(|r| r.contains("reconciled unresolved to settled")),
        "{:?}",
        last.reason
    );
    assert!(
        receipts
            .iter()
            .any(|(r, _)| r.outcome == mandate::receipts::Outcome::Unresolved),
        "the original unresolved receipt still stands"
    );

    // Reconciling again is a no-op: the transition was already recorded.
    let again = mandate::purchase::reconcile(&mut ledger, &mirror, "m1", OffsetDateTime::now_utc())
        .await
        .unwrap();
    assert!(again.is_empty(), "a terminal row is not reconciled again");
    assert_eq!(ledger.receipts("m1").unwrap().len(), before + 1);
}
