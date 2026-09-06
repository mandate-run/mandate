use crate::ledger::{InMemoryLedger, Ledger};
use crate::types::*;
use std::path::PathBuf;

#[test]
fn load_mandate_produces_plausible_defaults() {
    let ledger = InMemoryLedger::new();
    let mandate = ledger.load_mandate(&PathBuf::from("manifest.json")).unwrap();

    assert!(!mandate.id.is_empty());
    assert!(!mandate.constraints.manifest.path.is_empty());
    assert!(mandate.budget.service.total > 0);
    assert_eq!(mandate.inputs.pools.len(), 5);
}

#[test]
fn budget_columns_are_derived_from_state() {
    let mut ledger = InMemoryLedger::new();
    let mandate = ledger.load_mandate(&PathBuf::from("manifest.json")).unwrap();

    // A held reservation shows up in `held` (I1 / I7).
    ledger
        .insert_reservation(&Reservation {
            id: "res-1".into(),
            step: "explain".into(),
            amount: 800, // 0.0008 USDC
            source: ReservationSource::CeilingAtMax,
            state: ReservationState::Held,
        })
        .unwrap();
    let snap = ledger.budget_snapshot(&mandate.id).unwrap();
    assert_eq!(snap.held, 800);
    assert_eq!(snap.settled, 0);
    assert_eq!(snap.outstanding, 0);

    // A prepared authorization moves held -> outstanding.
    let mut auth = authorization(&mandate, "screen", 1_000); // 0.0010 USDC
    auth.payment_state = PaymentState::Prepared;
    ledger.insert_authorization(&auth).unwrap();
    let snap = ledger.budget_snapshot(&mandate.id).unwrap();
    assert_eq!(snap.outstanding, 1_000);
    assert_eq!(snap.held, 800);

    // Settlement moves outstanding -> settled.
    auth.payment_state = PaymentState::Settled;
    ledger.update_authorization(&auth).unwrap();
    let snap = ledger.budget_snapshot(&mandate.id).unwrap();
    assert_eq!(snap.settled, 1_000);
    assert_eq!(snap.outstanding, 0);

    // Failure moves outstanding -> free.
    auth.payment_state = PaymentState::Prepared;
    ledger.update_authorization(&auth).unwrap();
    auth.payment_state = PaymentState::Failed;
    ledger.update_authorization(&auth).unwrap();
    let snap = ledger.budget_snapshot(&mandate.id).unwrap();
    assert_eq!(snap.settled, 0);
    assert_eq!(snap.outstanding, 0);

    // I1: an authorization that would exceed the service budget is refused.
    ledger.release_reservation("res-1").unwrap();
    let mut big = authorization(&mandate, "big", 10_000 + 1);
    big.payment_state = PaymentState::Prepared;
    let err = ledger.insert_authorization(&big).unwrap_err();
    assert!(matches!(err, crate::ledger::LedgerError::Invariant(_)));
}

#[test]
fn receipts_are_sequenced_and_queryable() {
    let mut ledger = InMemoryLedger::new();
    let mandate = ledger.load_mandate(&PathBuf::from("manifest.json")).unwrap();
    let receipt = |seq: u64| Receipt {
        seq,
        mandate_id: mandate.id.clone(),
        step: "screen".into(),
        listing_id: Some("screen".into()),
        seller: Some("s".into()),
        amount: 1_000,
        asset: "0.0.429274".into(),
        tx_id: Some("0.0.1@1.0".into()),
        payment_id_hash: None,
        request_hash: None,
        response_hash: None,
        outcome: ReceiptOutcome::Paid,
        reason: None,
        latency_ms: None,
        at: chrono::Utc::now(),
        mandate_hash: None,
        manifest_hash: None,
        spec_version: None,
    };
    ledger.insert_receipt(&receipt(5)).unwrap();
    ledger.insert_receipt(&receipt(3)).unwrap();
    let since = ledger.receipts_since(&mandate.id, 4).unwrap();
    assert_eq!(since.len(), 1);
    assert_eq!(since[0].seq, 5);
}

fn authorization(_mandate: &Mandate, id: &str, amount: i128) -> Authorization {
    let now = chrono::Utc::now();
    Authorization {
        id: id.into(),
        quote_id: id.into(),
        payment_id: format!("pay_{id}"),
        tx_id: "0.0.7162784@100.0".into(),
        amount,
        valid_start: now,
        valid_until: now + chrono::Duration::seconds(30),
        signed_bytes: vec![],
        request: RequestSent {
            method: "POST".into(),
            url: "https://sellers.example/screen".into(),
            headers: vec![],
            body: vec![],
        },
        submissions: 1,
        retrievals: 0,
        payment_state: PaymentState::Sent,
        delivery_state: DeliveryState::None,
        response_body: None,
        response_hash: None,
    }
}