//! Fixtures shared by unit and integration tests: real signed transfers from
//! a fixed throwaway key, so every accounting field a test sees came out of
//! bytes and the same inputs always give the same bytes. Not part of the
//! runtime's behavior.

use std::str::FromStr;

use serde_json::{Map, json};
use time::{Duration, OffsetDateTime};

use hedera::PrivateKey;

use crate::hedera::{Asset, HederaAccountId, Signer, Transfer, sign_transfer, valid_duration};
use crate::ledger::{BindingError, MandateRow, PreparedPayment, Request};
use crate::x402::{PaymentPayload, PaymentRequired, Requirement, encode_header};

pub const PAYER: &str = "0.0.10399984";
pub const PAY_TO: &str = "0.0.10409989";
pub const FEE_PAYER: &str = "0.0.7162784";
/// A throwaway ED25519 key with a fixed seed. Never funded.
const SEED_DER: &str = "302e020100300506032b6570042204200202020202020202020202020202020202020202020202020202020202020202";

/// The mandate row the tests share: 0.0100 USDC service, 0.5 HBAR audit.
pub fn mandate_row() -> MandateRow {
    MandateRow {
        id: "m1".to_owned(),
        mandate_hash: "a".repeat(64),
        manifest_hash: "b".repeat(64),
        service_total: 10_000,
        service_asset: "0.0.429274".to_owned(),
        audit_total: 50_000_000,
        max_single_payment: 9_000,
        deadline: time::macros::datetime!(2026-09-13 16:00 UTC),
    }
}

pub fn request() -> Request {
    Request {
        method: "POST".to_owned(),
        url: "http://127.0.0.1:4021/events".to_owned(),
        headers: vec![("content-type".to_owned(), "application/json".to_owned())],
        body: br#"{"pools":["0xabc"]}"#.to_vec(),
    }
}

/// A real signed transfer of `amount` in `asset`, distinct per `n`, wrapped
/// in a `PAYMENT-SIGNATURE` header whose accepted terms match the bytes.
pub fn signed_payment(n: u32, amount: i64, asset: &str, now: OffsetDateTime) -> PreparedPayment {
    signed_payment_for(n, amount, asset, now, |_| {}).expect("terms match the bytes")
}

/// Like [`signed_payment`], with a hook that edits the accepted terms before
/// the header is built, for tests that need the terms to disagree with the bytes.
pub fn signed_payment_for(
    n: u32,
    amount: i64,
    asset: &str,
    now: OffsetDateTime,
    edit: impl FnOnce(&mut Requirement),
) -> Result<PreparedPayment, BindingError> {
    let signer = Signer::new(
        HederaAccountId::from_str(PAYER).unwrap(),
        PrivateKey::from_str(SEED_DER).expect("fixed key"),
    );
    let nodes = [HederaAccountId::from_str("0.0.3").unwrap()];
    let transfer = Transfer {
        fee_payer: HederaAccountId::from_str(FEE_PAYER).unwrap(),
        pay_to: HederaAccountId::from_str(PAY_TO).unwrap(),
        asset: Asset::parse(asset).unwrap(),
        amount,
        node_account_ids: &nodes,
        valid_start: now - Duration::seconds(5) + Duration::nanoseconds(i64::from(n)),
        valid_duration: valid_duration(120),
    };
    let signed = sign_transfer(&signer, &transfer).expect("signs");
    let mut accepted = Requirement {
        scheme: "exact".to_owned(),
        network: "hedera:testnet".to_owned(),
        amount: amount.to_string(),
        pay_to: PAY_TO.to_owned(),
        max_timeout_seconds: 120,
        asset: asset.to_owned(),
        extra: Some(json!({ "feePayer": FEE_PAYER })),
    };
    edit(&mut accepted);
    let required = PaymentRequired {
        x402_version: 2,
        error: None,
        resource: None,
        accepts: vec![accepted.clone()],
        extensions: Map::new(),
    };
    let payment_id = format!("pay_{n:0>32}");
    let header = encode_header(&PaymentPayload::new(
        &required,
        &accepted,
        signed.base64(),
        &payment_id,
    ));
    PreparedPayment::from_signature(&header, &payment_id)
}
