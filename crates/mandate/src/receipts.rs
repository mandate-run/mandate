//! Spec sections 2.6 and 11: receipts. One HCS message per receipt, compact
//! JSON of at most 1024 bytes, section 2.6 fields only. Never inputs, prompts,
//! evidence, reports or payment ids. The publish queue and I9 arrive with
//! slice E.

use serde::{Deserialize, Serialize};

pub const MAX_BYTES: usize = 1024;
pub const SPEC_VERSION: &str = "0.6";

#[derive(Debug, thiserror::Error)]
pub enum ReceiptError {
    #[error("receipt {seq} is {len} bytes with no reason left to cut; the bound is {MAX_BYTES}")]
    TooLarge { seq: u64, len: usize },
    #[error("receipt does not serialize: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Receipt 0: the run's identity.
    Start,
    Paid,
    Refused,
    Failed,
    Unresolved,
}

/// One receipt, section 2.6. Optional fields are absent from the JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub seq: u64,
    pub mandate_id: String,
    pub outcome: Outcome,
    /// RFC 3339.
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listing_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seller: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tx_id: Option<String>,
    /// SHA-256 of the payment id. The id itself is never published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payment_id_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mandate_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_version: Option<String>,
}

impl Receipt {
    /// Receipt 0: what was run, not what was bought.
    pub fn start(mandate_id: &str, mandate_hash: &str, manifest_hash: &str, at: &str) -> Self {
        Self {
            seq: 0,
            mandate_id: mandate_id.to_owned(),
            outcome: Outcome::Start,
            at: at.to_owned(),
            step: None,
            listing_id: None,
            seller: None,
            amount: None,
            asset: None,
            tx_id: None,
            payment_id_hash: None,
            request_hash: None,
            response_hash: None,
            reason: None,
            latency_ms: None,
            mandate_hash: Some(mandate_hash.to_owned()),
            manifest_hash: Some(manifest_hash.to_owned()),
            spec_version: Some(SPEC_VERSION.to_owned()),
        }
    }

    /// The HCS message: compact JSON within the bound. A `reason` is cut to
    /// fit; when the fixed fields alone exceed the bound, that is an error.
    pub fn message(&self) -> Result<Vec<u8>, ReceiptError> {
        let mut candidate = self.clone();
        loop {
            let bytes = serde_json::to_vec(&candidate)?;
            if bytes.len() <= MAX_BYTES {
                return Ok(bytes);
            }
            let excess = bytes.len() - MAX_BYTES;
            match candidate.reason.as_mut() {
                Some(reason) if !reason.is_empty() => {
                    let keep = reason.len().saturating_sub(excess + 1);
                    let mut cut = keep;
                    while cut > 0 && !reason.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    reason.truncate(cut);
                }
                _ => {
                    return Err(ReceiptError::TooLarge {
                        seq: self.seq,
                        len: bytes.len(),
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paid(reason: Option<String>) -> Receipt {
        Receipt {
            seq: 3,
            mandate_id: "m-2026-09-07-0001".to_owned(),
            outcome: Outcome::Paid,
            at: "2026-09-07T18:00:00Z".to_owned(),
            step: Some(2),
            listing_id: Some("events".to_owned()),
            seller: Some("0.0.10409989".to_owned()),
            amount: Some("1500".to_owned()),
            asset: Some("0.0.429274".to_owned()),
            tx_id: Some("0.0.7162784@1788804389.851433000".to_owned()),
            payment_id_hash: Some("a".repeat(64)),
            request_hash: Some("b".repeat(64)),
            response_hash: Some("c".repeat(64)),
            reason,
            latency_ms: Some(812),
            mandate_hash: None,
            manifest_hash: None,
            spec_version: None,
        }
    }

    #[test]
    fn start_receipt_is_small_and_names_no_payment_id() {
        let r = Receipt::start(
            "m",
            &"d".repeat(64),
            &"e".repeat(64),
            "2026-09-07T18:00:00Z",
        );
        let bytes = r.message().unwrap();
        assert!(bytes.len() < 300, "{}", bytes.len());
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("\"outcome\":\"start\""));
        assert!(text.contains("\"spec_version\":\"0.6\""));
        assert!(!text.contains("\"payment_id\""));
        assert!(!text.contains("\"step\""));
    }

    #[test]
    fn paid_receipt_round_trips() {
        let r = paid(None);
        let bytes = r.message().unwrap();
        assert!(bytes.len() <= MAX_BYTES);
        let back: Receipt = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn long_reason_is_cut_to_the_bound() {
        let r = paid(Some("é".repeat(2000)));
        let bytes = r.message().unwrap();
        assert!(bytes.len() <= MAX_BYTES, "{}", bytes.len());
        assert!(bytes.len() > MAX_BYTES - 8, "{}", bytes.len());
        let back: Receipt = serde_json::from_slice(&bytes).unwrap();
        assert!(back.reason.unwrap().starts_with("é"));
    }

    #[test]
    fn oversized_fixed_fields_are_an_error() {
        let mut r = paid(None);
        r.mandate_id = "x".repeat(2000);
        assert!(matches!(
            r.message(),
            Err(ReceiptError::TooLarge { seq: 3, .. })
        ));
    }
}
