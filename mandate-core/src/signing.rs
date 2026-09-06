use std::collections::BTreeMap;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SigningError {
    #[error("payload construction failed: {0}")]
    Build(String),
    #[error("signature stub not implemented for live key: {0}")]
    NotStubbed(String),
}

pub type Result<T> = std::result::Result<T, SigningError>;

/// A deterministic placeholder signer used for development and fixture runs.
/// It does not represent any real Hedera key and must not be used against a live facilitator.
pub struct StubSigner {
    account_id: String,
}

impl StubSigner {
    pub fn new(account_id: String) -> Self {
        Self { account_id }
    }

    pub fn account_id(&self) -> &str {
        &self.account_id
    }
}

/// A constructed but unsigned exact-scheme transfer payload, enough to drive
/// quote binding checks and transcript tx_id formatting before a real signer is wired.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TransferPayload {
    pub debit_account_id: String,
    pub credit_account_id: String,
    pub asset: String,
    pub amount: i128,
    pub fee_payer: String,
    pub valid_start_epoch_seconds: i64,
    pub valid_duration_seconds: i64,
    pub tx_id_account_id: String,
    pub tx_id_valid_start_seconds: i64,
    pub application_timestamp: i64,
}

#[allow(clippy::too_many_arguments)] // domain API: one argument per exact-scheme field
pub fn build_exact_payload(
    debit_account_id: String,
    credit_account_id: String,
    asset: String,
    amount: i128,
    fee_payer: String,
    max_timeout_s: i64,
    now_epoch_seconds: i64,
    application_timestamp: i64,
) -> Result<TransferPayload> {
    if amount <= 0 {
        return Err(SigningError::Build("amount must be positive".into()));
    }

    let valid_start = now_epoch_seconds.saturating_sub(5);
    let valid_duration = max_timeout_s.min(120);

    Ok(TransferPayload {
        debit_account_id,
        credit_account_id,
        asset,
        amount,
        fee_payer: fee_payer.clone(),
        valid_start_epoch_seconds: valid_start,
        valid_duration_seconds: valid_duration,
        tx_id_account_id: fee_payer,
        tx_id_valid_start_seconds: valid_start,
        application_timestamp,
    })
}

pub fn tx_id_from_payload(payload: &TransferPayload) -> String {
    format!(
        "{}@{}.{}",
        payload.tx_id_account_id, payload.tx_id_valid_start_seconds, 0
    )
}

pub fn payload_fingerprint(payload: &TransferPayload) -> String {
    let mut parts: BTreeMap<String, String> = BTreeMap::new();
    parts.insert("debit".into(), payload.debit_account_id.clone());
    parts.insert("credit".into(), payload.credit_account_id.clone());
    parts.insert("asset".into(), payload.asset.clone());
    parts.insert("amount".into(), payload.amount.to_string());
    parts.insert("fee_payer".into(), payload.fee_payer.clone());
    parts.insert("valid_start".into(), payload.valid_start_epoch_seconds.to_string());
    parts.insert("duration".into(), payload.valid_duration_seconds.to_string());

    let mut canon = String::new();
    for (k, v) in parts {
        canon.push_str(&k);
        canon.push('=');
        canon.push_str(&v);
        canon.push('&');
    }
    sha256_hex(&canon)
}

fn sha256_hex(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut d = Sha256::new();
    d.update(input.as_bytes());
    hex::encode(d.finalize())
}
