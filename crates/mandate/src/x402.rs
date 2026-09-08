//! Spec sections 10 and 13: the x402 v2 wire objects the buyer reads and writes.
//!
//! Own types rather than r402-protocol: its structs deny unknown fields, and the
//! buyer must accept whatever a conforming seller or facilitator adds. Every
//! header is base64 of JSON.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

pub const HEADER_REQUIRED: &str = "payment-required";
pub const HEADER_SIGNATURE: &str = "payment-signature";
pub const HEADER_RESPONSE: &str = "payment-response";
pub const PAYMENT_IDENTIFIER: &str = "payment-identifier";
pub const X402_VERSION: u32 = 2;

#[derive(Debug, thiserror::Error)]
pub enum HeaderError {
    #[error("header is not base64: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("header is not the expected JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// One entry of `accepts`: what the seller will take.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Requirement {
    pub scheme: String,
    pub network: String,
    pub amount: String,
    pub pay_to: String,
    pub max_timeout_seconds: u64,
    pub asset: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
}

impl Requirement {
    /// The facilitator account that pays fees, from `extra.feePayer`.
    pub fn fee_payer(&self) -> Option<&str> {
        self.extra.as_ref()?.get("feePayer")?.as_str()
    }
}

/// Decoded `PAYMENT-REQUIRED`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentRequired {
    pub x402_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<Value>,
    #[serde(default)]
    pub accepts: Vec<Requirement>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub extensions: Map<String, Value>,
}

impl PaymentRequired {
    /// The first `exact` requirement on `network`.
    pub fn exact_on(&self, network: &str) -> Option<&Requirement> {
        self.accepts
            .iter()
            .find(|r| r.scheme == "exact" && r.network == network)
    }

    /// Whether the seller declared the payment-identifier extension.
    pub fn accepts_payment_identifier(&self) -> bool {
        self.extensions.contains_key(PAYMENT_IDENTIFIER)
    }
}

/// Scheme payload for Hedera `exact`: the partially signed transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExactHederaPayload {
    pub transaction: String,
}

/// Decoded `PAYMENT-SIGNATURE`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentPayload {
    pub x402_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<Value>,
    pub accepted: Requirement,
    pub payload: ExactHederaPayload,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub extensions: Map<String, Value>,
}

impl PaymentPayload {
    /// Builds the payload for `accepted`, copying `resource` from the 402 and
    /// carrying `payment_id` in the payment-identifier extension.
    pub fn new(
        required: &PaymentRequired,
        accepted: &Requirement,
        transaction_base64: String,
        payment_id: &str,
    ) -> Self {
        // Echo the seller's declaration and set `info.id`, as the reference
        // client does; a bare `{ "info": { "id" } }` when nothing was declared.
        let mut declared = required
            .extensions
            .get(PAYMENT_IDENTIFIER)
            .cloned()
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({ "info": {} }));
        let info = declared
            .as_object_mut()
            .expect("object")
            .entry("info")
            .or_insert_with(|| json!({}));
        if !info.is_object() {
            *info = json!({});
        }
        info["id"] = Value::String(payment_id.to_owned());
        let mut extensions = Map::new();
        extensions.insert(PAYMENT_IDENTIFIER.to_owned(), declared);
        Self {
            x402_version: X402_VERSION,
            resource: required.resource.clone(),
            accepted: accepted.clone(),
            payload: ExactHederaPayload {
                transaction: transaction_base64,
            },
            extensions,
        }
    }

    /// The payment id carried in the extension, if any.
    pub fn payment_id(&self) -> Option<&str> {
        self.extensions
            .get(PAYMENT_IDENTIFIER)?
            .get("info")?
            .get("id")?
            .as_str()
    }
}

/// Decoded `PAYMENT-RESPONSE`. Recorded, never trusted.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentResponse {
    #[serde(default)]
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_reason: Option<String>,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

/// Decodes a base64 JSON header.
pub fn decode_header<T: DeserializeOwned>(value: &str) -> Result<T, HeaderError> {
    let bytes = STANDARD.decode(value.trim())?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Encodes a value as a base64 JSON header.
pub fn encode_header<T: Serialize>(value: &T) -> String {
    let json = serde_json::to_vec(value).expect("header types serialize");
    STANDARD.encode(json)
}

/// A fresh payment id: `pay_` and a UUID v4.
pub fn new_payment_id() -> String {
    format!("pay_{}", uuid::Uuid::new_v4())
}

/// Lowercase hex SHA-256.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQUIRED: &str = r#"{"x402Version":2,"error":"PAYMENT-SIGNATURE header is required","resource":{"url":"http://localhost:4021/spike","description":"spike","mimeType":"application/json"},"accepts":[{"scheme":"exact","network":"hedera:testnet","amount":"1000","asset":"0.0.429274","payTo":"0.0.111","maxTimeoutSeconds":120,"extra":{"feePayer":"0.0.7162784","unknownLater":true}}],"extensions":{"payment-identifier":{"info":{"required":true},"schema":{"type":"object"}}},"someFutureField":1}"#;

    #[test]
    fn decodes_required_and_tolerates_unknown_fields() {
        let header = STANDARD.encode(REQUIRED);
        let required: PaymentRequired = decode_header(&header).unwrap();
        let req = required.exact_on("hedera:testnet").unwrap();
        assert_eq!(req.amount, "1000");
        assert_eq!(req.asset, "0.0.429274");
        assert_eq!(req.pay_to, "0.0.111");
        assert_eq!(req.max_timeout_seconds, 120);
        assert_eq!(req.fee_payer(), Some("0.0.7162784"));
        assert!(required.accepts_payment_identifier());
        assert!(required.exact_on("hedera:mainnet").is_none());
    }

    #[test]
    fn payload_carries_resource_accepted_and_payment_id() {
        let required: PaymentRequired = serde_json::from_str(REQUIRED).unwrap();
        let req = required.exact_on("hedera:testnet").unwrap();
        let payload = PaymentPayload::new(&required, req, "AAEC".to_owned(), "pay_x");
        let header = encode_header(&payload);
        let back: PaymentPayload = decode_header(&header).unwrap();
        assert_eq!(back.x402_version, 2);
        assert_eq!(back.accepted, *req);
        assert_eq!(back.payload.transaction, "AAEC");
        assert_eq!(back.payment_id(), Some("pay_x"));
        assert_eq!(back.resource.unwrap()["url"], "http://localhost:4021/spike");
        let json: Value = serde_json::from_slice(&STANDARD.decode(header).unwrap()).unwrap();
        let ext = &json["extensions"]["payment-identifier"];
        assert_eq!(ext["info"]["id"], "pay_x");
        assert_eq!(ext["info"]["required"], true);
        assert_eq!(ext["schema"]["type"], "object");
        assert!(json.get("error").is_none());
    }

    #[test]
    fn payload_without_declaration_still_carries_id() {
        let required = PaymentRequired {
            x402_version: 2,
            error: None,
            resource: None,
            accepts: vec![],
            extensions: Map::new(),
        };
        let req: Requirement = serde_json::from_str(
            r#"{"scheme":"exact","network":"hedera:testnet","amount":"1","payTo":"0.0.1","maxTimeoutSeconds":60,"asset":"HBAR"}"#,
        )
        .unwrap();
        let payload = PaymentPayload::new(&required, &req, "AA==".to_owned(), "pay_y");
        assert_eq!(payload.payment_id(), Some("pay_y"));
        assert!(payload.resource.is_none());
    }

    #[test]
    fn payment_id_shape() {
        let id = new_payment_id();
        assert!(id.starts_with("pay_"));
        assert_eq!(id.len(), 40);
        assert!(
            id.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
    }

    #[test]
    fn response_tolerates_any_shape() {
        let header = STANDARD.encode(r#"{"success":true,"transaction":"0.0.7162784@1.2","network":"hedera:testnet","payer":"0.0.5","extra":{}}"#);
        let r: PaymentResponse = decode_header(&header).unwrap();
        assert!(r.success);
        assert_eq!(r.transaction.as_deref(), Some("0.0.7162784@1.2"));
        assert!(r.rest.contains_key("extra"));
    }

    #[test]
    fn sha256_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
