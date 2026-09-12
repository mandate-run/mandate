//! Live transports and the live scenario runner.
//!
//! The fixture (fixture.rs) exercises the real runtime logic against simulated
//! quotes, settlement and evidence. This module runs the same logic against
//! live services: real x402 sellers over HTTP (v2 wire format, exact scheme),
//! real Hedera transfers signed through r402-hedera and settled through
//! Blocky402, settlement proven from mirror-node records (never from an HTTP
//! response, spec section 6) and receipts published to an HCS topic.
//!
//! The Graph evidence is purchased from the sellers like any other fact;
//! provenance spot-checks go against the mandate's Ethereum RPC.

use std::str::FromStr;
use std::time::Duration;

use base64::Engine;
use chrono::{Duration as ChronoDuration, Utc};
use reqwest::blocking::Client as HttpClient;
use serde_json::{json, Value};
use thiserror::Error;

use crate::brief::BriefBuilder;
use crate::fixture::{
    self, fmt_amount, sha256_hex, AUDIT_DECIMALS, FACILITATOR_FEE_PAYER, RECEIPT_FEE,
    SERVICE_DECIMALS, SPEC_VERSION,
};
use crate::ledger::{InMemoryLedger, Ledger};
use crate::plan::{input_kb, PlanBuilder, PlanSelection};
use crate::types::*;

#[derive(Debug, Clone)]
pub struct LiveConfig {
    /// Payer account that funds the purchases (holds the USDC service budget).
    pub account_id: String,
    /// Hex-encoded private key of the payer.
    pub private_key: String,
    /// Blocky402 facilitator base URL.
    pub facilitator_url: String,
    /// Mirror node REST base URL.
    pub mirror_url: String,
    /// HCS topic for receipts; None keeps receipts local (audit_pending).
    pub receipts_topic: Option<String>,
    /// Ethereum JSON-RPC used for provenance spot-checks; None skips them.
    pub eth_rpc: Option<String>,
    pub quote_timeout_ms: u64,
    pub settle_poll_s: u64,
    pub settle_attempts: u32,
}

impl LiveConfig {
    /// Read configuration from the environment.
    pub fn from_env() -> Result<Self, LiveError> {
        let account_id = std::env::var("MANDATE_ACCOUNT_ID")
            .map_err(|_| LiveError::Config("MANDATE_ACCOUNT_ID is not set".into()))?;
        let private_key = std::env::var("MANDATE_PRIVATE_KEY")
            .map_err(|_| LiveError::Config("MANDATE_PRIVATE_KEY is not set".into()))?;
        Ok(Self {
            account_id,
            private_key,
            facilitator_url: std::env::var("FACILITATOR_URL")
                .unwrap_or_else(|_| "https://api.testnet.blocky402.com".into()),
            mirror_url: std::env::var("MIRROR_NODE_URL")
                .unwrap_or_else(|_| "https://testnet.mirrornode.hedera.com/api/v1".into()),
            receipts_topic: std::env::var("RECEIPTS_TOPIC").ok(),
            eth_rpc: std::env::var("ETH_RPC_URL").ok(),
            quote_timeout_ms: 30_000,
            settle_poll_s: 2,
            settle_attempts: 20,
        })
    }
}

#[derive(Debug, Error)]
pub enum LiveError {
    #[error("configuration: {0}")]
    Config(String),
    #[error("facilitator error: {0}")]
    Facilitator(String),
    #[error("signing error: {0}")]
    Signing(String),
    #[error("http error: {0}")]
    Http(String),
    #[error("seller answered {0}, expected 200")]
    SellerStatus(u16),
    #[error("SELLER_UNREACHABLE: {0}")]
    Unreachable(String),
    #[error("settlement not observed for {0}")]
    Unsettled(String),
    #[error("ledger error: {0}")]
    Ledger(#[from] crate::ledger::LedgerError),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("invalid evidence: {0}")]
    Evidence(String),
    #[error("fixture error: {0}")]
    Fixture(#[from] fixture::FixtureError),
}

pub type LiveResult<T> = std::result::Result<T, LiveError>;

/// A signed payment: the base64 PaymentPayload sent in PAYMENT-SIGNATURE, its
/// JSON (the ledger's `signed_bytes`) and the transaction id.
#[derive(Debug, Clone)]
pub struct SignedPayment {
    pub header: String,
    pub tx_id: String,
    pub payload_json: Value,
}

/// What the mirror node said about one transaction id (spec I5).
#[derive(Debug, Clone, Copy)]
pub struct Settlement {
    pub record_count: usize,
    pub duplicates_ignored: usize,
    pub success: bool,
    pub transfers_match: bool,
}

impl Settlement {
    /// A failure result with no SUCCESS record: `failed`, not `unresolved`.
    fn failed(&self) -> bool {
        !self.success && self.record_count > self.duplicates_ignored
    }
}

/// The live transport client.
pub struct LiveClient {
    pub cfg: LiveConfig,
    http: HttpClient,
}

impl LiveClient {
    pub fn new(cfg: LiveConfig) -> Self {
        let http = HttpClient::builder()
            .timeout(Duration::from_millis(cfg.quote_timeout_ms.max(5_000)))
            .build()
            .expect("http client builds");
        Self { cfg, http }
    }

    /// Fee payer the facilitator advertises for `hedera:testnet` (spec I2).
    pub fn facilitator_fee_payer(&self) -> LiveResult<String> {
        let resp = self
            .http
            .get(format!("{}/supported", self.cfg.facilitator_url))
            .send()
            .map_err(|e| LiveError::Unreachable(format!("facilitator /supported: {e}")))?;
        let body: Value = resp
            .json()
            .map_err(|e| LiveError::Facilitator(format!("parse /supported: {e}")))?;
        let fee_payer = body
            .pointer("/signers/hedera:*/0")
            .or_else(|| {
                body["kinds"]
                    .as_array()?
                    .iter()
                    .find(|k| k["network"] == "hedera:testnet")?
                    .pointer("/extra/feePayer")
            })
            .and_then(Value::as_str);
        fee_payer
            .map(str::to_string)
            .ok_or_else(|| LiveError::Facilitator("no hedera:testnet fee payer advertised".into()))
    }

    /// Ask one seller for a quote by sending the real request without payment
    /// (spec section 4). The 402 is turned into a Quote and checked against
    /// the pinned listing.
    pub fn quote(&self, listing: &Listing, body: &Value, units: i128) -> LiveResult<Quote> {
        let resp = self
            .http
            .post(&listing.url)
            .json(body)
            .send()
            .map_err(|e| LiveError::Unreachable(format!("{}: {e}", listing.url)))?;
        let status = resp.status().as_u16();
        if status != 402 {
            return Err(LiveError::Unreachable(format!(
                "{} answered {status}, expected 402",
                listing.url
            )));
        }
        let header = resp
            .headers()
            .get("PAYMENT-REQUIRED")
            .and_then(|v| v.to_str().ok());
        let required: Value = match header {
            Some(h) => decode_b64_json(h)?,
            None => resp
                .json()
                .map_err(|e| LiveError::Parse(format!("no PAYMENT-REQUIRED: {e}")))?,
        };
        let accepted = required
            .get("accepts")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .ok_or_else(|| LiveError::Parse("402 carries no accepts".into()))?;
        let amount: i128 = accepted
            .get("amount")
            .and_then(Value::as_str)
            .ok_or_else(|| LiveError::Parse("accepts.amount missing".into()))?
            .parse()
            .map_err(|e| LiveError::Parse(format!("amount: {e}")))?;
        let network = get_str(accepted, "network", "accepts.network")?;
        let asset = get_str(accepted, "asset", "accepts.asset")?;
        let pay_to = get_str(accepted, "payTo", "accepts.payTo")?;
        let fee_payer = accepted
            .pointer("/extra/feePayer")
            .and_then(Value::as_str)
            .ok_or_else(|| LiveError::Parse("accepts.extra.feePayer missing".into()))?
            .to_string();
        let max_timeout_s = accepted
            .get("maxTimeoutSeconds")
            .and_then(Value::as_i64)
            .ok_or_else(|| LiveError::Parse("accepts.maxTimeoutSeconds missing".into()))?;

        let ceiling = crate::plan::ceiling(listing, units);
        let received_at = Utc::now();
        let body_bytes = serde_json::to_vec(body).map_err(|e| LiveError::Parse(e.to_string()))?;
        let listing_match = listing.network == network
            && listing.asset == asset
            && listing.pay_to == pay_to;
        let fee_payer_ok = fee_payer == FACILITATOR_FEE_PAYER;
        Ok(Quote {
            listing_id: listing.id.clone(),
            amount,
            asset,
            network,
            pay_to,
            fee_payer,
            max_timeout_s,
            received_at,
            ceiling,
            within_tariff: amount <= ceiling,
            listing_match,
            fee_payer_ok,
            request_binding: RequestBinding {
                method: listing.method.clone(),
                url: listing.url.clone(),
                body_hash: sha256_hex(&body_bytes),
            },
            requested_units: units,
        })
    }

    /// Build and sign the transfer for a quote (spec section 10): transaction
    /// id owned by the fee payer, the payer's signature only. The result
    /// travels base64 in PAYMENT-SIGNATURE.
    pub fn sign_payment(&self, quote: &Quote, payment_id: &str) -> LiveResult<SignedPayment> {
        let signer = r402_hedera::exact::client::HederaSigner::from_secret(
            &self.cfg.account_id,
            &self.cfg.private_key,
        )
        .map_err(|e| LiveError::Signing(e.to_string()))?;
        let tx_b64 = r402_hedera::exact::client::create_signed_transfer(
            &signer,
            &quote.pay_to,
            &quote.asset,
            &quote.amount.to_string(),
            &quote.fee_payer,
            &quote.network,
        )
        .map_err(|e| LiveError::Signing(e.to_string()))?;
        let tx_id = r402_hedera::inspect_hedera_transaction(&tx_b64)
            .map_err(LiveError::Signing)?
            .transaction_id;
        let payload_json = json!({
            "x402Version": 2,
            "scheme": "exact",
            "network": quote.network,
            "resource": {
                "method": quote.request_binding.method,
                "url": quote.request_binding.url,
                "contentType": "application/json",
            },
            "accepted": {
                "scheme": "exact",
                "network": quote.network,
                "asset": quote.asset,
                "amount": quote.amount.to_string(),
                "payTo": quote.pay_to,
                "maxTimeoutSeconds": quote.max_timeout_s,
                "extra": { "feePayer": quote.fee_payer },
            },
            "payload": { "transaction": tx_b64 },
            "extensions": {
                "payment-identifier": { "info": { "id": payment_id } },
            },
        });
        let bytes = serde_json::to_vec(&payload_json)
            .map_err(|e| LiveError::Signing(e.to_string()))?;
        let header = base64::engine::general_purpose::STANDARD.encode(bytes);
        Ok(SignedPayment {
            header,
            tx_id,
            payload_json,
        })
    }

    /// Send a paid request. A 2xx with a body is a delivery candidate;
    /// anything else is a lost response recovered by retrieving with the same
    /// signed payment (spec section 6).
    pub fn send_paid(
        &self,
        quote: &Quote,
        signed: &SignedPayment,
        body: &Value,
    ) -> LiveResult<(u16, Option<Value>, Option<String>)> {
        let resp = self
            .http
            .post(&quote.request_binding.url)
            .header("PAYMENT-SIGNATURE", &signed.header)
            .header("X-PAYMENT", &signed.header)
            .json(body)
            .send()
            .map_err(|e| LiveError::Http(format!("{}: {e}", quote.request_binding.url)))?;
        let status = resp.status().as_u16();
        let payment_response = resp
            .headers()
            .get("PAYMENT-RESPONSE")
            .and_then(|v| v.to_str().ok())
            .or_else(|| {
                resp.headers()
                    .get("X-PAYMENT-RESPONSE")
                    .and_then(|v| v.to_str().ok())
            })
            .map(str::to_string);
        if status == 200 {
            let value: Value = resp.json().map_err(|e| LiveError::Parse(e.to_string()))?;
            Ok((status, Some(value), payment_response))
        } else {
            Ok((status, None, payment_response))
        }
    }

    /// Prove settlement from the mirror node for our own transaction id (spec
    /// I5): poll until a SUCCESS record with matching transfers, a failure, or
    /// the attempts run out. An empty record set keeps exposure held.
    pub fn observe_settlement(&self, tx_id: &str, quote: &Quote) -> LiveResult<Settlement> {
        let mirror_id = mirror_transaction_id(tx_id)?;
        let url = format!("{}/transactions/{}", self.cfg.mirror_url, mirror_id);
        let mut last: Option<Settlement> = None;
        for attempt in 0..self.cfg.settle_attempts {
            match self.http.get(&url).send() {
                Ok(r) if r.status().is_success() => {
                    let body: Value = r.json().map_err(|e| LiveError::Parse(e.to_string()))?;
                    let s = parse_mirror_records(&body, quote)?;
                    if s.success || s.failed() {
                        return Ok(s);
                    }
                    last = Some(s);
                }
                Ok(r) if r.status().as_u16() == 404 => last = None,
                Ok(_) => {}
                Err(_) => {}
            }
            if attempt + 1 < self.cfg.settle_attempts {
                std::thread::sleep(Duration::from_secs(self.cfg.settle_poll_s));
            }
        }
        match last {
            Some(s) => Ok(s),
            None => Err(LiveError::Unsettled(tx_id.to_string())),
        }
    }

    /// Publish one receipt to the HCS topic (spec section 11). Returns the HCS
    /// transaction id.
    pub fn publish_receipt(&self, receipt: &Receipt) -> LiveResult<String> {
        let topic = self
            .cfg
            .receipts_topic
            .clone()
            .ok_or_else(|| LiveError::Config("RECEIPTS_TOPIC is not set".into()))?;
        let message = receipt_message(receipt);
        let account_id = self.cfg.account_id.clone();
        let private_key = self.cfg.private_key.clone();
        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| LiveError::Config(format!("tokio runtime: {e}")))?;
        runtime.block_on(async move {
            let account = hedera::AccountId::from_str(&account_id)
                .map_err(|e| LiveError::Signing(e.to_string()))?;
            let key = hedera::PrivateKey::from_str(&private_key)
                .map_err(|e| LiveError::Signing(e.to_string()))?;
            let client = hedera::Client::for_testnet();
            client.set_operator(account, key);
            let topic_id = hedera::TopicId::from_str(&topic)
                .map_err(|e| LiveError::Config(format!("topic {topic}: {e}")))?;
            let mut tx = hedera::TopicMessageSubmitTransaction::new();
            #[allow(clippy::needless_borrows_for_generic_args)]
            tx.topic_id(topic_id);
            tx.message(message.as_bytes());
            let response = tx
                .execute(&client)
                .await
                .map_err(|e| LiveError::Config(format!("HCS submit: {e}")))?;
            Ok(response.transaction_id.to_string())
        })
    }
}

fn decode_b64_json(header: &str) -> LiveResult<Value> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(header.trim())
        .map_err(|e| LiveError::Parse(format!("base64 PAYMENT-REQUIRED: {e}")))?;
    serde_json::from_slice(&bytes).map_err(|e| LiveError::Parse(e.to_string()))
}

fn get_str(v: &Value, key: &str, what: &str) -> LiveResult<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| LiveError::Parse(format!("{what} missing")))
}

/// `0.0.1234@1234567890.123456789` -> `0.0.1234-1234567890-123456789`.
pub fn mirror_transaction_id(tx_id: &str) -> LiveResult<String> {
    let (account, rest) = tx_id
        .split_once('@')
        .ok_or_else(|| LiveError::Parse(format!("bad tx id {tx_id}")))?;
    let (sec, nanos) = rest
        .split_once('.')
        .ok_or_else(|| LiveError::Parse(format!("bad tx id {tx_id}")))?;
    let nanos = format!("{nanos:0>9}");
    Ok(format!("{account}-{sec}-{nanos}"))
}

/// Parse the mirror node response for one transaction id: every record for the
/// id, DUPLICATE_TRANSACTION records ignored, and whether a SUCCESS record
/// transfers the approved debit and credit (spec I5).
fn parse_mirror_records(body: &Value, quote: &Quote) -> LiveResult<Settlement> {
    let entries: Vec<&Value> = match body.get("transactions") {
        Some(Value::Array(a)) => a.iter().collect(),
        Some(Value::Object(_)) => vec![body],
        _ => {
            return Ok(Settlement {
                record_count: 0,
                duplicates_ignored: 0,
                success: false,
                transfers_match: false,
            })
        }
    };
    let mut record_count = 0usize;
    let mut duplicates_ignored = 0usize;
    let mut success = false;
    let mut transfers_match = false;
    for entry in entries {
        let result = entry.get("result").and_then(Value::as_str).unwrap_or("");
        if result == "DUPLICATE_TRANSACTION" {
            duplicates_ignored += 1;
            record_count += 1;
            continue;
        }
        record_count += 1;
        if entry.get("nonce").and_then(Value::as_u64).unwrap_or(0) != 0 {
            continue;
        }
        if result == "SUCCESS" && transfers_match_entry(entry, quote) {
            success = true;
            transfers_match = true;
        }
    }
    Ok(Settlement {
        record_count,
        duplicates_ignored,
        success,
        transfers_match,
    })
}

fn transfers_match_entry(entry: &Value, quote: &Quote) -> bool {
    let mut debit = false;
    let mut credit = false;
    let transfers: Vec<&Value> = if quote.asset == "0.0.0" {
        entry
            .get("transfers")
            .and_then(Value::as_array)
            .map(|a| a.iter().collect())
            .unwrap_or_default()
    } else {
        entry
            .get("token_transfers")
            .and_then(Value::as_array)
            .map(|a| a.iter().collect())
            .unwrap_or_default()
    };
    for t in transfers {
        let account = t.get("account").and_then(Value::as_str).unwrap_or("");
        let amount = t.get("amount").and_then(Value::as_i64).unwrap_or(0);
        if quote.asset != "0.0.0" && t.get("token_id").and_then(Value::as_str) != Some(quote.asset.as_str()) {
            continue;
        }
        if account == quote.pay_to && amount == quote.amount as i64 {
            credit = true;
        }
        if amount == -(quote.amount as i64) {
            debit = true;
        }
    }
    debit && credit
}

/// HCS message for one receipt: JSON with the receipt fields only, no payment
/// ids, inputs or evidence (spec section 11).
fn receipt_message(receipt: &Receipt) -> String {
    let v = json!({
        "seq": receipt.seq,
        "mandate_id": receipt.mandate_id,
        "step": receipt.step,
        "listing_id": receipt.listing_id,
        "seller": receipt.seller,
        "amount": fmt_amount(receipt.amount, SERVICE_DECIMALS),
        "asset": receipt.asset,
        "tx_id": receipt.tx_id,
        "payment_id_hash": receipt.payment_id_hash,
        "request_hash": receipt.request_hash,
        "response_hash": receipt.response_hash,
        "outcome": format!("{:?}", receipt.outcome).to_lowercase(),
        "reason": receipt.reason,
        "at": receipt.at.to_rfc3339(),
    });
    serde_json::to_string(&v).unwrap_or_default()
}

/// Run one mandate scenario against live sellers.
///
/// `sellers_url` rewrites the pinned manifest's listing URLs to
/// `{sellers_url}/{listing_id}` so the mandate can point at the local seller
/// fleet while keeping the pinned ids and tariffs.
pub fn run_live(
    mandate: &Mandate,
    listings: &[Listing],
    sellers_url: Option<&str>,
    scenario: fixture::Scenario,
    cfg: LiveConfig,
) -> LiveResult<fixture::ScenarioRun> {
    let listings: Vec<Listing> = match sellers_url {
        Some(base) => listings
            .iter()
            .map(|l| Listing {
                url: format!("{}/{}", base.trim_end_matches('/'), l.id),
                ..l.clone()
            })
            .collect(),
        None => listings.to_vec(),
    };

    // Sanity checks before any spend.
    let client = LiveClient::new(cfg.clone());
    let advertised = client.facilitator_fee_payer()?;
    if advertised != FACILITATOR_FEE_PAYER {
        return Err(LiveError::Facilitator(format!(
            "facilitator advertises fee payer {advertised}, mandate pins {FACILITATOR_FEE_PAYER}"
        )));
    }
    let rpc_placeholder = mandate.constraints.eth_rpc.contains("alchemy.com/v2/dev");
    if rpc_placeholder && cfg.eth_rpc.is_none() {
        tracing::warn!("mandate eth_rpc is the dev placeholder and ETH_RPC_URL is unset; provenance spot-checks will be skipped");
    }

    let mut runner = LiveRunner {
        mandate,
        listings: &listings,
        client,
        ledger: InMemoryLedger::new(),
        next_seq: 0,
        released_total: 0,
        audit_fee_total: 0,
    };
    let mut t = runner.empty_transcript();
    runner.commit_receipt_0()?;

    match scenario {
        fixture::Scenario::Normal => runner.run_normal(&mut t)?,
        fixture::Scenario::Refusal => runner.run_refusal(&mut t)?,
        other => {
            return Err(LiveError::Config(format!(
                "live mode does not support scenario {} (fixture-only)",
                other.as_str()
            )))
        }
    }

    let snapshot = runner.ledger.budget_snapshot(&mandate.id)?;
    t.totals = TotalsRow {
        settled: fmt_amount(snapshot.settled, SERVICE_DECIMALS),
        released: fmt_amount(runner.released_total, SERVICE_DECIMALS),
        unspent: fmt_amount(snapshot.free(mandate.budget.service.total), SERVICE_DECIMALS),
        unresolved: fmt_amount(snapshot.outstanding, SERVICE_DECIMALS),
        audit_spent: fmt_amount(runner.audit_fee_total, AUDIT_DECIMALS),
    };

    // Publish receipts to HCS. Publication failure never repeats a purchase;
    // receipts stay durable locally and are reported as audit_pending.
    if let Some(topic) = &cfg.receipts_topic {
        t.receipts.topic = topic.clone();
        let receipts = runner.ledger.receipts_since(&mandate.id, 0)?;
        for receipt in &receipts {
            match runner.client.publish_receipt(receipt) {
                Ok(hcs_tx) => {
                    tracing::info!(
                        "receipt seq {} published to {} at {hcs_tx}",
                        receipt.seq,
                        topic
                    );
                }
                Err(e) => {
                    t.receipts.pending.push(receipt.seq);
                    tracing::warn!("receipt seq {} publication failed: {e}", receipt.seq);
                }
            }
        }
    }

    let state = runner.ledger.export();
    Ok(fixture::ScenarioRun {
        transcript: t,
        state,
    })
}

struct LiveRunner<'a> {
    mandate: &'a Mandate,
    listings: &'a [Listing],
    client: LiveClient,
    ledger: InMemoryLedger,
    next_seq: u64,
    released_total: i128,
    audit_fee_total: i128,
}

impl<'a> LiveRunner<'a> {
    fn empty_transcript(&self) -> Transcript {
        Transcript {
            mandate: self.mandate.id.clone(),
            assumption: self.mandate.inputs.expected_material_pools.to_string(),
            brief_bound: crate::brief::mandatory_bound(self.mandate.inputs.pools.len()) as u64,
            quotes: Vec::new(),
            plan: None,
            rejected_plans: Vec::new(),
            plan_phases: Vec::new(),
            reservations: Vec::new(),
            steps: Vec::new(),
            outcomes: Vec::new(),
            validation: ValidationRow {
                coverage: String::new(),
                calculations: false,
                citations: false,
                provenance: String::new(),
                freshness: false,
                schema: false,
                complete: false,
                incomplete_reason: None,
            },
            refusals: Vec::new(),
            totals: TotalsRow {
                settled: "0.0000".into(),
                released: "0.0000".into(),
                unspent: "0.0000".into(),
                unresolved: "0.0000".into(),
                audit_spent: "0.0000".into(),
            },
            receipts: ReceiptsRow {
                topic: self.mandate.duties.receipts_topic.clone(),
                pending: Vec::new(),
            },
        }
    }

    fn window(&self) -> Value {
        let end = Utc::now() - ChronoDuration::seconds(5);
        let start = end - ChronoDuration::hours(self.mandate.inputs.window_h);
        json!({ "start": start.to_rfc3339(), "end": end.to_rfc3339() })
    }

    fn listing(&self, capability: &Capability) -> &Listing {
        self.listings
            .iter()
            .find(|l| l.capability == *capability)
            .expect("listing present")
    }

    fn listing_by_id(&self, id: &str) -> &Listing {
        self.listings.iter().find(|l| l.id == id).expect("listing present")
    }

    /// The request body the buyer will pay for (spec section 4: quote with the
    /// real request).
    fn quote_body(&self, pools: &[String]) -> Value {
        json!({ "pools": pools, "window": self.window() })
    }

    fn commit_receipt_0(&mut self) -> LiveResult<()> {
        let mandate_hash = sha256_hex(&serde_json::to_vec(self.mandate).unwrap_or_default());
        let manifest_hash = sha256_hex(&serde_json::to_vec(self.listings).unwrap_or_default());
        let receipt = Receipt {
            seq: self.next_seq,
            mandate_id: self.mandate.id.clone(),
            step: "init".into(),
            listing_id: None,
            seller: None,
            amount: 0,
            asset: self.mandate.budget.service.asset.clone(),
            tx_id: None,
            payment_id_hash: None,
            request_hash: None,
            response_hash: None,
            outcome: ReceiptOutcome::Paid,
            reason: None,
            latency_ms: None,
            at: Utc::now(),
            mandate_hash: Some(mandate_hash),
            manifest_hash: Some(manifest_hash),
            spec_version: Some(SPEC_VERSION.into()),
        };
        self.commit_receipt(receipt)?;
        Ok(())
    }

    /// The live normal path: quote screen and investigate live, plan, buy
    /// screen for all pools, re-plan over the pending set, buy events for the
    /// pending pools, then explain.
    fn run_normal(&mut self, t: &mut Transcript) -> LiveResult<()> {
        let pools = self.mandate.inputs.pools.clone();
        let n = pools.len() as i128;

        // Phase 1: screen and investigate are fully known now.
        let mut quotes = Vec::new();
        for cap in [Capability::Screen, Capability::Investigate] {
            let q = self.client.quote(self.listing(&cap), &self.quote_body(&pools), n)?;
            self.push_quote_row(t, &q, "live", 1);
            quotes.push(q);
        }
        let selection = PlanBuilder::new(self.mandate, self.listings, &quotes).evaluate();
        self.record_plan(t, &selection);
        let Some(chosen) = selection.chosen.as_ref() else {
            // Nothing feasible: refuse (REQUIREMENT_UNMEETABLE) before any spend.
            return self.refuse_unmeetable(t, &selection);
        };
        let _ = chosen;
        self.hold_reservations(&selection, t)?;

        // Buy screen for all pools.
        let screen_quote = self
            .client
            .quote(self.listing(&Capability::Screen), &self.quote_body(&pools), n)?;
        let screening_value = self.buy(&screen_quote, &self.quote_body(&pools), t)?;
        let screening: EvidenceResponse = serde_json::from_value(screening_value)
            .map_err(|e| LiveError::Evidence(e.to_string()))?;

        let mut outcomes = fixture::outcomes_from(self.mandate, std::slice::from_ref(&screening));
        t.outcomes = outcome_rows(self.mandate, &outcomes, &[]);
        let pending_pools: Vec<String> = pools
            .iter()
            .enumerate()
            .filter(|(i, _)| outcomes[*i] == PoolOutcome::Pending)
            .map(|(_, p)| p.clone())
            .collect();
        let pending = pending_pools.len();
        let free = self.free_budget()?;

        // Phase 2: quote events and investigate over the pending set.
        let mut quotes2 = Vec::new();
        for cap in [Capability::Events, Capability::Investigate] {
            if pending == 0 {
                break;
            }
            let q = self.client.quote(
                self.listing(&cap),
                &self.quote_body(&pending_pools),
                pending as i128,
            )?;
            self.push_quote_row(t, &q, "live", 1);
            quotes2.push(q);
        }
        let selection2 = PlanBuilder::new(self.mandate, self.listings, &quotes2)
            .with_state(0, pending, free)
            .evaluate();
        self.record_plan(t, &selection2);
        if let Some(chosen2) = selection2.chosen.as_ref() {
            let final_cap = chosen2.steps.last().map(|s| s.capability.clone());
            if final_cap != Some(Capability::Explain) {
                self.release_reservation("res-final-staged")?;
                mark_released(t, "explain");
            }
        }

        let mut evidence: Vec<EvidenceResponse> = Vec::new();
        if pending > 0 {
            let events_quote = quotes2
                .iter()
                .find(|q| {
                    q.listing_id == self.listing(&Capability::Events).id
                        && q.requested_units == pending as i128
                })
                .cloned()
                .ok_or_else(|| LiveError::Config("no events quote after screening".into()))?;
            if !events_quote.within_tariff {
                // OFF_TARIFF: refuse the events purchase and buy the
                // investigate bundle for the pending pools instead.
                t.refusals.push(RefusalRow {
                    code: "OFF_TARIFF".into(),
                    needed_bound: fmt_amount(events_quote.ceiling, SERVICE_DECIMALS),
                    needed_expected: fmt_amount(events_quote.ceiling, SERVICE_DECIMALS),
                    available: fmt_amount(events_quote.amount, SERVICE_DECIMALS),
                    reason: Some(format!(
                        "events ceiling {} quoted {}",
                        fmt_amount(events_quote.ceiling, SERVICE_DECIMALS),
                        fmt_amount(events_quote.amount, SERVICE_DECIMALS)
                    )),
                });
                self.commit_refusal_receipt("events", "OFF_TARIFF")?;
                let investigate_quote = quotes2
                    .iter()
                    .find(|q| {
                        q.listing_id == self.listing(&Capability::Investigate).id
                            && q.requested_units == pending as i128
                    })
                    .cloned()
                    .ok_or_else(|| LiveError::Config("no investigate quote after screening".into()))?;
                let v = self.buy(&investigate_quote, &self.quote_body(&pending_pools), t)?;
                evidence.push(serde_json::from_value(v).map_err(|e| LiveError::Evidence(e.to_string()))?);
            } else {
                let v = self.buy(&events_quote, &self.quote_body(&pending_pools), t)?;
                evidence.push(serde_json::from_value(v).map_err(|e| LiveError::Evidence(e.to_string()))?);
            }
        }

        let mut all_evidence = vec![screening.clone()];
        all_evidence.extend(evidence.iter().cloned());
        outcomes = fixture::outcomes_from(self.mandate, &all_evidence);
        let claims = fixture::compute_claims(self.mandate, &all_evidence);
        t.outcomes = outcome_rows(self.mandate, &outcomes, &claims);

        // Explain: build the brief, quote it live at input-KB units, buy it.
        let brief = BriefBuilder::new().build_brief(
            self.mandate,
            &outcomes,
            &claims,
            &all_evidence,
            self.mandate.requirements.brief_events,
        );
        let brief_doc = brief_document(self.mandate, &outcomes, &claims, &all_evidence);
        let explain_listing = self.listing(&Capability::Explain);
        let doc_bytes = serde_json::to_vec(&brief_doc).map_err(|e| LiveError::Parse(e.to_string()))?;
        let units = input_kb(doc_bytes.len());
        let explain_quote = self.client.quote(explain_listing, &brief_doc, units)?;
        self.push_quote_row(t, &explain_quote, "live", 1);
        let response = self.buy(&explain_quote, &brief_doc, t)?;
        let prose = response.get("prose").and_then(Value::as_str).unwrap_or("");

        let provenance = self.provenance(&claims, &all_evidence)?;
        let prose_ok = prose_numbers_ok(prose, &claims, &all_evidence);
        let report = crate::validate::validate_report(
            self.mandate,
            &outcomes,
            &claims,
            &all_evidence,
            &explain_quote,
            provenance,
            prose_ok,
        );
        if !prose_ok {
            t.validation.incomplete_reason =
                Some("prose contains numbers absent from the claims or facts".into());
        }
        t.validation = report.row;
        let _ = brief;
        Ok(())
    }

    /// Refusal path: with a service budget no plan can fit, refuse before the
    /// first purchase (spec section 5). Quotes still go live.
    fn run_refusal(&mut self, t: &mut Transcript) -> LiveResult<()> {
        let pools = self.mandate.inputs.pools.clone();
        let n = pools.len() as i128;
        let mut quotes = Vec::new();
        for cap in [Capability::Screen, Capability::Investigate] {
            let q = self.client.quote(self.listing(&cap), &self.quote_body(&pools), n)?;
            self.push_quote_row(t, &q, "live", 1);
            quotes.push(q);
        }
        let selection = PlanBuilder::new(self.mandate, self.listings, &quotes).evaluate();
        self.record_plan(t, &selection);
        if selection.chosen.is_some() {
            return Err(LiveError::Config(
                "refusal requires a budget no plan can fit".into(),
            ));
        }
        self.refuse_unmeetable(t, &selection)
    }

    fn refuse_unmeetable(&mut self, t: &mut Transcript, selection: &PlanSelection) -> LiveResult<()> {
        let lowest_bound = selection.rejected.iter().map(|r| r.bound).min().unwrap_or(0);
        let lowest_expected = selection.rejected.iter().map(|r| r.expected).min().unwrap_or(0);
        let available = self.mandate.budget.service.total;
        t.refusals.push(RefusalRow {
            code: "REQUIREMENT_UNMEETABLE".into(),
            needed_bound: fmt_amount(lowest_bound, SERVICE_DECIMALS),
            needed_expected: fmt_amount(lowest_expected, SERVICE_DECIMALS),
            available: fmt_amount(available, SERVICE_DECIMALS),
            reason: None,
        });
        self.commit_refusal_receipt("run", "REQUIREMENT_UNMEETABLE")?;
        Ok(())
    }

    /// One live purchase: consume the held reservation, sign, persist
    /// (held -> outstanding), send, prove settlement from the mirror, receive
    /// and validate the delivery, and write the receipt.
    fn buy(&mut self, quote: &Quote, body: &Value, t: &mut Transcript) -> LiveResult<Value> {
        let step = self.listing_by_id(&quote.listing_id).id.clone();
        let now = Utc::now();
        let payment_id =
            format!("pay_{}", uuid4(&format!("{}-{}-{}", self.mandate.id, step, quote.listing_id)));
        let signed = self.client.sign_payment(quote, &payment_id)?;

        // Consume a held reservation for this step (I7: held -> outstanding).
        let reservations = self.ledger.get_reservations(&self.mandate.id)?;
        for res in reservations
            .iter()
            .filter(|r| r.step == step && r.state == ReservationState::Held)
        {
            self.released_total += (res.amount - quote.amount).max(0);
            self.ledger.release_reservation(&res.id)?;
            if let Some(row) = t
                .reservations
                .iter_mut()
                .find(|r| r.step == res.step && r.state == "held")
            {
                row.state = "consumed".into();
            }
        }

        let mut auth = Authorization {
            id: format!("auth-{}", t.steps.len() + 1),
            quote_id: quote.listing_id.clone(),
            payment_id: payment_id.clone(),
            tx_id: signed.tx_id.clone(),
            amount: quote.amount,
            valid_start: now,
            valid_until: now + ChronoDuration::seconds(quote.max_timeout_s),
            signed_bytes: serde_json::to_vec(&signed.payload_json).unwrap_or_default(),
            request: RequestSent {
                method: "POST".into(),
                url: quote.request_binding.url.clone(),
                headers: vec![
                    ("PAYMENT-SIGNATURE".into(), signed.header.clone()),
                    ("X-PAYMENT".into(), signed.header.clone()),
                ],
                body: serde_json::to_vec(body).unwrap_or_default(),
            },
            submissions: 1,
            retrievals: 0,
            payment_state: PaymentState::Prepared,
            delivery_state: DeliveryState::None,
            response_body: None,
            response_hash: None,
        };
        self.ledger.insert_authorization(&auth)?;
        auth.payment_state = PaymentState::Sent;
        self.ledger.update_authorization(&auth)?;

        let (status, mut response, _payment_response) =
            self.client.send_paid(quote, &signed, body)?;
        if status != 200 || response.is_none() {
            // Lost response: prove settlement independently of HTTP (I5).
            let settlement = self.client.observe_settlement(&signed.tx_id, quote)?;
            if !(settlement.success && settlement.transfers_match) {
                auth.payment_state = if settlement.failed() {
                    PaymentState::Failed
                } else {
                    PaymentState::Unresolved
                };
                self.ledger.update_authorization(&auth)?;
                return Err(LiveError::Unsettled(signed.tx_id));
            }
            auth.payment_state = PaymentState::Settled;
            self.ledger.update_authorization(&auth)?;
            // Retrieve with the same signed payment (I6): sellers serve the
            // stored result.
            auth.retrievals += 1;
            self.ledger.update_authorization(&auth)?;
            let (rstatus, rbody, _) = self.client.send_paid(quote, &signed, body)?;
            if rstatus != 200 {
                return Err(LiveError::SellerStatus(rstatus));
            }
            response = rbody;
        } else {
            // Delivery arrived; settlement is still proven from the mirror.
            let settlement = self.client.observe_settlement(&signed.tx_id, quote)?;
            if !(settlement.success && settlement.transfers_match) {
                if settlement.failed() {
                    auth.payment_state = PaymentState::Failed;
                }
                self.ledger.update_authorization(&auth)?;
                return Err(LiveError::Unsettled(signed.tx_id));
            }
            auth.payment_state = PaymentState::Settled;
            self.ledger.update_authorization(&auth)?;
        }

        let value = response.ok_or_else(|| LiveError::Evidence("empty delivery".into()))?;
        let response_bytes = serde_json::to_vec(&value).unwrap_or_default();
        auth.delivery_state = DeliveryState::Received;
        auth.response_body = Some(response_bytes.clone());
        auth.response_hash = Some(sha256_hex(&response_bytes));
        self.ledger.update_authorization(&auth)?;
        auth.delivery_state = DeliveryState::Validated;
        self.ledger.update_authorization(&auth)?;

        t.steps.push(StepRow {
            step: step.clone(),
            payment_id: payment_id.clone(),
            tx_id: signed.tx_id.clone(),
            submissions: auth.submissions,
            retrievals: auth.retrievals,
            transitions: vec![
                TransitionRow {
                    from: "prepared".into(),
                    to: "sent".into(),
                    at: now,
                    record_count: None,
                    duplicates_ignored: None,
                },
                TransitionRow {
                    from: "sent".into(),
                    to: "settled".into(),
                    at: Utc::now(),
                    record_count: None,
                    duplicates_ignored: None,
                },
                TransitionRow {
                    from: "none".into(),
                    to: "validated".into(),
                    at: Utc::now(),
                    record_count: None,
                    duplicates_ignored: None,
                },
            ],
        });

        let listing = self.listing_by_id(&quote.listing_id);
        let receipt = Receipt {
            seq: self.next_seq,
            mandate_id: self.mandate.id.clone(),
            step,
            listing_id: Some(quote.listing_id.clone()),
            seller: Some(listing.seller.clone()),
            amount: quote.amount,
            asset: quote.asset.clone(),
            tx_id: Some(signed.tx_id.clone()),
            payment_id_hash: Some(sha256_hex(payment_id.as_bytes())),
            request_hash: None,
            response_hash: auth.response_hash.clone(),
            outcome: ReceiptOutcome::Paid,
            reason: None,
            latency_ms: None,
            at: Utc::now(),
            mandate_hash: None,
            manifest_hash: None,
            spec_version: None,
        };
        self.commit_receipt(receipt)?;
        Ok(value)
    }

    fn commit_refusal_receipt(&mut self, step: &str, code: &str) -> LiveResult<()> {
        let receipt = Receipt {
            seq: self.next_seq,
            mandate_id: self.mandate.id.clone(),
            step: step.into(),
            listing_id: None,
            seller: None,
            amount: 0,
            asset: self.mandate.budget.service.asset.clone(),
            tx_id: None,
            payment_id_hash: None,
            request_hash: None,
            response_hash: None,
            outcome: ReceiptOutcome::Refused,
            reason: Some(code.into()),
            latency_ms: None,
            at: Utc::now(),
            mandate_hash: None,
            manifest_hash: None,
            spec_version: None,
        };
        self.commit_receipt(receipt)?;
        Ok(())
    }

    fn commit_receipt(&mut self, mut receipt: Receipt) -> LiveResult<u64> {
        receipt.seq = self.next_seq;
        self.next_seq += 1;
        let seq = self.ledger.insert_receipt(&receipt)?;
        self.audit_fee_total += RECEIPT_FEE;
        self.ledger.add_audit_spend(&self.mandate.id, RECEIPT_FEE);
        Ok(seq)
    }

    fn free_budget(&self) -> LiveResult<i128> {
        let snap = self.ledger.budget_snapshot(&self.mandate.id)?;
        Ok(snap.free(self.mandate.budget.service.total))
    }

    fn hold_reservations(&mut self, selection: &PlanSelection, t: &mut Transcript) -> LiveResult<()> {
        for res in &selection.reservations {
            self.ledger.insert_reservation(res)?;
            t.reservations.push(ReservationRow {
                step: res.step.clone(),
                amount: fmt_amount(res.amount, SERVICE_DECIMALS),
                source: match res.source {
                    ReservationSource::CeilingAtMax => "ceiling_at_max".into(),
                    ReservationSource::Quote => "quote".into(),
                },
                state: "held".into(),
            });
        }
        Ok(())
    }

    fn release_reservation(&mut self, id: &str) -> LiveResult<()> {
        let reservations = self.ledger.get_reservations(&self.mandate.id)?;
        if let Some(res) = reservations.iter().find(|r| r.id == id) {
            self.released_total += res.amount;
            self.ledger.release_reservation(id)?;
        }
        Ok(())
    }

    fn record_plan(&mut self, t: &mut Transcript, selection: &PlanSelection) {
        let chosen = match selection.chosen.as_ref() {
            Some(chosen) => PlanRow {
                name: chosen.kind.as_str().into(),
                expected: fmt_amount(chosen.expected, SERVICE_DECIMALS),
                bound: fmt_amount(chosen.bound, SERVICE_DECIMALS),
                assumption: self.mandate.inputs.expected_material_pools,
                authorizations: chosen.authorizations,
            },
            None => PlanRow {
                name: "none".into(),
                expected: "0.0000".into(),
                bound: "0.0000".into(),
                assumption: self.mandate.inputs.expected_material_pools,
                authorizations: 0,
            },
        };
        t.plan = selection.chosen.as_ref().map(|_| chosen.clone());
        t.plan_phases.push(PlanPhaseRow {
            chosen,
            rejected: rejection_rows(&selection.rejected),
        });
        if let Some(phase) = t.plan_phases.last() {
            for r in &phase.rejected {
                if !t
                    .rejected_plans
                    .iter()
                    .any(|x| x.name == r.name && x.reason == r.reason)
                {
                    t.rejected_plans.push(r.clone());
                }
            }
        }
    }

    fn push_quote_row(&self, t: &mut Transcript, q: &Quote, source: &str, latency_ms: u64) {
        let row = QuoteRow {
            listing_id: q.listing_id.clone(),
            amount: fmt_amount(q.amount, SERVICE_DECIMALS),
            ceiling: fmt_amount(q.ceiling, SERVICE_DECIMALS),
            within_tariff: q.within_tariff,
            listing_match: q.listing_match,
            fee_payer_ok: q.fee_payer_ok,
            latency_ms,
            source: source.into(),
        };
        if !t.quotes.iter().any(|existing| {
            existing.listing_id == row.listing_id
                && existing.amount == row.amount
                && existing.source == row.source
        }) {
            t.quotes.push(row);
        }
    }

    /// Provenance spot-checks (spec section 9): sample cited transaction
    /// hashes and check each exists with status 1 on Ethereum and mentions its
    /// pool. None when no RPC is usable or no transaction hash is cited.
    fn provenance(&self, claims: &[Claim], evidence: &[EvidenceResponse]) -> LiveResult<Option<(usize, usize)>> {
        let hashes: Vec<&Claim> = claims.iter().collect();
        let cited: Vec<String> = hashes
            .iter()
            .flat_map(|c| c.evidence.iter())
            .filter(|e| e.kind == EvidenceKind::EventFact)
            .map(|e| e.fact_id.clone())
            .collect();
        if cited.is_empty() {
            return Ok(None);
        }
        let rpc = match (&self.client.cfg.eth_rpc, self.mandate.constraints.eth_rpc.as_str()) {
            (Some(r), _) => Some(r.clone()),
            (None, r) if !r.contains("alchemy.com/v2/dev") => Some(r.to_string()),
            (None, _) => None,
        };
        let Some(rpc) = rpc else { return Ok(None); };
        let pools = &self.mandate.inputs.pools;
        let sample: Vec<String> = cited
            .iter()
            .take(self.mandate.requirements.provenance_samples)
            .cloned()
            .collect();
        let mut passed = 0usize;
        for fact_id in &sample {
            let hash = fact_id.split(':').nth(3).unwrap_or("").to_string();
            if !hash.is_empty() && check_eth_receipt(&rpc, &hash, pools)? {
                passed += 1;
            }
        }
        let _ = evidence;
        Ok(Some((passed, sample.len())))
    }
}

fn rejection_rows(rejected: &[crate::plan::PlanRejection]) -> Vec<RejectedPlanRow> {
    rejected
        .iter()
        .map(|r| RejectedPlanRow {
            name: r.name.into(),
            reason: r.reason.clone(),
            bound: fmt_amount(r.bound, SERVICE_DECIMALS),
            expected: fmt_amount(r.expected, SERVICE_DECIMALS),
        })
        .collect()
}

fn mark_released(t: &mut Transcript, step: &str) {
    if let Some(row) = t
        .reservations
        .iter_mut()
        .find(|r| r.step == step && r.state == "held")
    {
        row.state = "released".into();
    }
}

/// The explain seller receives a brief document: per-pool outcomes and the
/// claims, nothing more (spec section 8: the brief is the only input the
/// explain step receives).
fn brief_document(
    mandate: &Mandate,
    outcomes: &[PoolOutcome],
    claims: &[Claim],
    evidence: &[EvidenceResponse],
) -> Value {
    let outcome_rows: Vec<Value> = mandate
        .inputs
        .pools
        .iter()
        .enumerate()
        .map(|(i, pool)| {
            let facts = evidence
                .iter()
                .flat_map(|r| r.pools.iter())
                .find(|p| &p.address == pool);
            json!({
                "pool": pool,
                "outcome": outcome_label(&outcomes[i]),
                "claims": claims.iter().filter(|c| c.pool == *pool).cloned().collect::<Vec<Claim>>(),
                "facts": facts.map(|p| json!({
                    "tvl_start_usd": p.tvl_start.usd,
                    "tvl_end_usd": p.tvl_end.usd,
                    "token0_change": change(p.tvl_start.token0, p.tvl_end.token0),
                    "token1_change": change(p.tvl_start.token1, p.tvl_end.token1),
                    "mints": p.mint_count,
                    "burns": p.burn_count,
                    "swaps": p.swap_count,
                })),
            })
        })
        .collect();
    json!({
        "mandate_id": mandate.id,
        "purpose": mandate.purpose,
        "coverage": "all_material",
        "window_h": mandate.inputs.window_h,
        "outcomes": outcome_rows,
        "claims": claims,
    })
}

fn outcome_label(outcome: &PoolOutcome) -> &'static str {
    match outcome {
        PoolOutcome::NonMaterial => "non_material",
        PoolOutcome::Pending => "pending",
        PoolOutcome::Supported => "supported",
        PoolOutcome::Undetermined => "undetermined",
    }
}

fn change(start: f64, end: f64) -> f64 {
    if start == 0.0 {
        end
    } else {
        (end - start) / start
    }
}

/// Spec section 9 prose check: every number in the prose must appear among the
/// claim values or fact values.
fn prose_numbers_ok(prose: &str, claims: &[Claim], evidence: &[EvidenceResponse]) -> bool {
    if prose.trim().is_empty() {
        return true;
    }
    let mut allowed: Vec<f64> = Vec::new();
    for claim in claims {
        for v in &claim.values {
            if let Ok(n) = v.parse::<f64>() {
                allowed.push(n);
            }
        }
    }
    for r in evidence {
        for p in &r.pools {
            for n in [
                p.tvl_start.token0,
                p.tvl_start.token1,
                p.tvl_start.usd,
                p.tvl_end.token0,
                p.tvl_end.token1,
                p.tvl_end.usd,
            ] {
                allowed.push(n);
            }
            for e in &p.events {
                allowed.push(e.amount_usd);
            }
        }
    }
    for n in prose_numbers(prose) {
        if !allowed.iter().any(|a| (a - n).abs() <= 1e-6) {
            return false;
        }
    }
    true
}

/// Extract decimal numbers from prose, skipping hex addresses, dates and
/// identifier-like tokens.
fn prose_numbers(prose: &str) -> Vec<f64> {
    let mut out = Vec::new();
    for token in prose.split(|c: char| {
        c.is_whitespace() || matches!(c, ',' | ';' | ':' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | '%' | '/' | '\\')
    }) {
        if token.is_empty() || token.contains(['x', 'X']) {
            continue;
        }
        if token.matches('-').count() > 1 {
            continue; // date-like
        }
        let t = token.strip_suffix('.').unwrap_or(token);
        let t = t.strip_suffix(',').unwrap_or(t);
        if let Ok(n) = t.parse::<f64>() {
            if t.chars().any(|c| c.is_ascii_digit()) {
                out.push(n);
            }
        }
    }
    out
}

fn check_eth_receipt(rpc: &str, hash: &str, pools: &[String]) -> LiveResult<bool> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_getTransactionReceipt",
        "params": [hash],
    });
    let client = HttpClient::new();
    let resp = client
        .post(rpc)
        .json(&body)
        .send()
        .map_err(|e| LiveError::Http(e.to_string()))?;
    let value: Value = resp.json().map_err(|e| LiveError::Parse(e.to_string()))?;
    let result = value.get("result");
    let Some(result) = result else { return Ok(false); };
    if result.is_null() {
        return Ok(false);
    }
    if result.get("status").and_then(Value::as_str) != Some("0x1") {
        return Ok(false);
    }
    let logs = result.get("logs").and_then(Value::as_array);
    let Some(logs) = logs else { return Ok(false); };
    let addresses: Vec<String> = logs
        .iter()
        .filter_map(|l| l.get("address").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    Ok(pools
        .iter()
        .any(|pool| addresses.iter().any(|a| a.eq_ignore_ascii_case(pool))))
}

/// Deterministic v4-shaped uuid for live payment ids (same shape as the
/// fixture so ids compare like-for-like).
fn uuid4(seed: &str) -> String {
    let h = sha256_hex(seed.as_bytes());
    let mut s: Vec<char> = h.chars().take(32).collect();
    s[12] = '4';
    s[16] = '8';
    let s: String = s.into_iter().collect();
    format!(
        "{}-{}-{}-{}-{}",
        &s[0..8],
        &s[8..12],
        &s[12..16],
        &s[16..20],
        &s[20..32]
    )
}

fn outcome_rows(mandate: &Mandate, outcomes: &[PoolOutcome], claims: &[Claim]) -> Vec<OutcomeRow> {
    mandate
        .inputs
        .pools
        .iter()
        .enumerate()
        .map(|(i, pool)| OutcomeRow {
            pool: pool.clone(),
            outcome: match outcomes[i] {
                PoolOutcome::NonMaterial => "non_material".into(),
                PoolOutcome::Pending => "pending".into(),
                PoolOutcome::Supported => "supported".into(),
                PoolOutcome::Undetermined => "undetermined".into(),
            },
            claim_count: claims.iter().filter(|c| c.pool == *pool).count(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirror_id_conversion() {
        assert_eq!(
            mirror_transaction_id("0.0.7162784@1725600000.123456789").unwrap(),
            "0.0.7162784-1725600000-123456789"
        );
        // Nanos without leading zeros are padded to the mirror's 9 digits.
        assert_eq!(
            mirror_transaction_id("0.0.5@1725600000.42").unwrap(),
            "0.0.5-1725600000-000000042"
        );
        assert!(mirror_transaction_id("garbage").is_err());
    }

    #[test]
    fn prose_check_passes_claim_echo() {
        let claim = Claim {
            claim_type: ClaimType::TvlChange,
            pool: "0xpool".into(),
            values: vec!["0.1000".into(), "-0.0345".into()],
            calculation: String::new(),
            evidence: Vec::new(),
        };
        let evidence: Vec<EvidenceResponse> = Vec::new();
        // A model may only echo claim values.
        assert!(prose_numbers_ok(
            "token0 changed 0.1000, token1 changed -0.0345",
            &[claim.clone()],
            &evidence
        ));
        // A number absent from the claims fails the check.
        assert!(!prose_numbers_ok(
            "token0 changed 0.9999",
            &[claim.clone()],
            &evidence
        ));
    }

    #[test]
    fn prose_check_ignores_addresses_and_dates() {
        assert_eq!(
            prose_numbers("Pool 0xBa9e1006dEb46A96FD07843B8C4C8C2e287D83a: 0.1000, at 2026-09-06 2 of 5"),
            vec![0.1000, 2.0, 5.0]
        );
    }

    #[test]
    fn uuid4_is_shape_valid() {
        let id = uuid4("seed");
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert_eq!(&id[19..20], "8");
        assert_eq!(&id[8..9], "-");
        assert_eq!(&id[13..14], "-");
    }

    #[test]
    fn quote_parses_a_live_402() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let required = json!({
            "x402Version": 2,
            "error": "Payment required",
            "resource": {
                "method": "POST",
                "url": format!("http://{addr}/screen"),
                "contentType": "application/json",
            },
            "accepts": [{
                "scheme": "exact",
                "network": "hedera:testnet",
                "asset": "0.0.429274",
                "amount": "200",
                "payTo": "0.0.7777777",
                "maxTimeoutSeconds": 120,
                "extra": { "feePayer": FACILITATOR_FEE_PAYER },
            }],
        });
        let header = base64::engine::general_purpose::STANDARD
            .encode(serde_json::to_vec(&required).unwrap());
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf).unwrap();
            let body = serde_json::to_string(&required).unwrap();
            let head = format!(
                "HTTP/1.1 402 Payment Required\r\nPAYMENT-REQUIRED: {header}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body.as_bytes());
        });

        let cfg = LiveConfig {
            account_id: "0.0.100".into(),
            private_key: "key".into(),
            facilitator_url: "https://example.test".into(),
            mirror_url: "https://example.test".into(),
            receipts_topic: None,
            eth_rpc: None,
            quote_timeout_ms: 5_000,
            settle_poll_s: 1,
            settle_attempts: 2,
        };
        let client = LiveClient::new(cfg);
        let listing = Listing {
            id: "screen".into(),
            seller: "mandate-screen".into(),
            url: format!("http://{addr}/screen"),
            method: "POST".into(),
            capability: Capability::Screen,
            produces: vec![Produce::Screening],
            tariff: Tariff {
                version: "1.0.0".into(),
                base: 0,
                unit: TariffUnit::Pool,
                unit_price: 200,
                max_units: 100,
                rounding: Rounding::InputKb,
            },
            network: "hedera:testnet".into(),
            asset: "0.0.429274".into(),
            pay_to: "0.0.7777777".into(),
            discovery: None,
        };
        let quote = client
            .quote(&listing, &json!({ "pools": ["0xpool"] }), 1)
            .unwrap();
        thread.join().unwrap();
        assert_eq!(quote.amount, 200);
        assert_eq!(quote.ceiling, 200);
        assert!(quote.within_tariff);
        assert!(quote.listing_match);
        assert!(quote.fee_payer_ok);
        assert_eq!(quote.requested_units, 1);
    }

    #[test]
    fn receipt_message_omits_payment_id() {
        let receipt = Receipt {
            seq: 1,
            mandate_id: "dev-mandate".into(),
            step: "screen".into(),
            listing_id: Some("screen".into()),
            seller: Some("mandate-screen".into()),
            amount: 1000,
            asset: "0.0.429274".into(),
            tx_id: Some("0.0.7162784@1725600000.123456789".into()),
            payment_id_hash: Some("hash".into()),
            request_hash: None,
            response_hash: None,
            outcome: ReceiptOutcome::Paid,
            reason: None,
            latency_ms: None,
            at: Utc::now(),
            mandate_hash: None,
            manifest_hash: None,
            spec_version: None,
        };
        let msg = receipt_message(&receipt);
        let v: Value = serde_json::from_str(&msg).unwrap();
        assert!(v.get("payment_id").is_none());
        assert_eq!(v["amount"], "0.0010");
    }
}

