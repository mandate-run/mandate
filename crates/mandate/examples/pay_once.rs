//! Issue 3 spike: one exact payment through the facilitator named in the 402,
//! persisted before it is sent, then resent unchanged.
//!
//! ```text
//! cargo run -p mandate --example pay_once -- <url>            quote, sign, persist, send, reconcile, resend
//! cargo run -p mandate --example pay_once -- <url> --sign-only  quote and sign, print the payload, send nothing
//! cargo run -p mandate --example pay_once -- --resend           resend what pay_once.json holds
//! ```
//!
//! Configuration: `MANDATE_ACCOUNT_ID`, `MANDATE_PRIVATE_KEY`, `MIRROR_NODE_URL`
//! and `HEDERA_NETWORK` from the process environment, else from
//! `crates/mandate/.env` or `./.env`. The key is read once and never printed.

use std::time::Duration as StdDuration;

use anyhow::{Context as _, bail, ensure};
use mandate::config::Config;
use mandate::hedera::{self, Asset, Expected, MirrorNode, Settlement, Transfer};
use mandate::x402::{self, PaymentPayload, PaymentRequired, PaymentResponse};
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;

const SAVED: &str = "pay_once.json";

/// The authorization row, written before the first send.
#[derive(Debug, Serialize, Deserialize)]
struct Saved {
    url: String,
    payment_id: String,
    transaction_id: String,
    mirror_id: String,
    payer: String,
    pay_to: String,
    asset: String,
    amount: i64,
    valid_until: String,
    payload_hash: String,
    payment_signature: String,
    created_at: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let sign_only = args.iter().any(|a| a == "--sign-only");
    let resend_only = args.iter().any(|a| a == "--resend");
    let url = args.iter().find(|a| !a.starts_with("--")).cloned();

    let cfg = Config::load()?;
    let http = reqwest::Client::builder()
        .timeout(StdDuration::from_secs(30))
        .build()?;
    let mirror = MirrorNode::new(http.clone(), cfg.mirror_node_url.clone());

    if resend_only {
        let saved: Saved = serde_json::from_slice(
            &std::fs::read(SAVED).with_context(|| format!("{SAVED} not found"))?,
        )?;
        let body = send(&http, &saved).await?;
        let expected = expected(&saved)?;
        let settlement = report_records(&mirror, &saved.mirror_id, &expected).await?;
        let mut checks = Checks::default();
        checks.require("resend answered 2xx", body.is_some());
        checks.require(
            "settlement is a matching SUCCESS record",
            matches!(settlement, Settlement::Settled { .. }),
        );
        return checks.finish();
    }

    let url = url.context("usage: pay_once <url> [--sign-only] | pay_once --resend")?;

    // 1. Quote: the 402 for the actual request.
    let resp = http.get(&url).send().await?;
    ensure!(
        resp.status().as_u16() == 402,
        "expected 402, got {}",
        resp.status()
    );
    let header = resp
        .headers()
        .get(x402::HEADER_REQUIRED)
        .context("402 without PAYMENT-REQUIRED")?
        .to_str()?
        .to_owned();
    let required: PaymentRequired = x402::decode_header(&header)?;
    println!(
        "PAYMENT-REQUIRED\n{}",
        serde_json::to_string_pretty(&required)?
    );
    let req = required
        .exact_on(cfg.network.caip2())
        .with_context(|| format!("no exact requirement on {}", cfg.network.caip2()))?;
    let fee_payer = req
        .fee_payer()
        .context("requirement carries no extra.feePayer")?;

    // 2. Sign with the runtime key only.
    let nodes = mirror.node_account_ids(5).await?;
    ensure!(!nodes.is_empty(), "mirror node listed no consensus nodes");
    let signer = cfg.signer()?;
    let asset = Asset::parse(&req.asset)?;
    let amount: i64 = req.amount.parse().context("amount is not an integer")?;
    let pay_to: hedera::HederaAccountId = req.pay_to.parse()?;
    let transfer = Transfer {
        fee_payer: fee_payer.parse()?,
        pay_to,
        asset,
        amount,
        node_account_ids: &nodes,
        valid_start: hedera::valid_start_now(),
        valid_duration: hedera::valid_duration(req.max_timeout_seconds),
    };
    let signed = hedera::sign_transfer(&signer, &transfer)?;
    let seen = hedera::inspect(&signed.bytes)?;
    println!(
        "signed: id {} payer {} nodes {:?} duration {:?}\n  hbar {:?}\n  tokens {:?}",
        seen.transaction_id,
        seen.transaction_id.account_id,
        seen.node_account_ids,
        seen.valid_duration,
        seen.hbar,
        seen.tokens
    );
    let payment_id = x402::new_payment_id();
    let payload = PaymentPayload::new(&required, req, signed.base64(), &payment_id);
    let signature = x402::encode_header(&payload);
    if sign_only {
        println!("PAYMENT-SIGNATURE\n{signature}");
        println!("decoded\n{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    // 3. Persist before send: the row the ledger will hold.
    let saved = Saved {
        url: url.clone(),
        payment_id,
        transaction_id: signed.transaction_id.to_string(),
        mirror_id: signed.mirror_id(),
        payer: signer.account_id.to_string(),
        pay_to: req.pay_to.clone(),
        asset: req.asset.clone(),
        amount,
        valid_until: signed.valid_until.format(&Rfc3339)?,
        payload_hash: x402::sha256_hex(signature.as_bytes()),
        payment_signature: signature,
        created_at: time::OffsetDateTime::now_utc().format(&Rfc3339)?,
    };
    std::fs::write(SAVED, serde_json::to_vec_pretty(&saved)?)?;
    println!(
        "persisted {SAVED} before send: payment_id {} mirror_id {} valid_until {}",
        saved.payment_id, saved.mirror_id, saved.valid_until
    );

    // 4. Send.
    let first = send(&http, &saved).await?;

    // 5. Reconcile from the mirror node record set.
    let expected = expected(&saved)?;
    let mut settlement = Settlement::Absent {
        duplicates_ignored: 0,
    };
    for _ in 0..18 {
        settlement = report_records(&mirror, &saved.mirror_id, &expected).await?;
        if !matches!(settlement, Settlement::Absent { .. }) {
            break;
        }
        tokio::time::sleep(StdDuration::from_secs(5)).await;
    }
    println!("settlement: {settlement:?}");

    // 6. Resend the identical authorization.
    let second = send(&http, &saved).await?;
    let after = report_records(&mirror, &saved.mirror_id, &expected).await?;
    println!("settlement after resend: {after:?}");

    // 7. The gate passes only when every check holds.
    let mut checks = Checks::default();
    checks.require("first send answered 2xx", first.is_some());
    checks.require(
        "settlement is a matching SUCCESS record",
        matches!(settlement, Settlement::Settled { .. }),
    );
    checks.require("resend answered 2xx", second.is_some());
    checks.require(
        "resend served the identical body",
        first.is_some() && first == second,
    );
    checks.require("record set unchanged by the resend", after == settlement);
    checks.finish()
}

/// Gate checks. Every failure is printed; any failure is a non-zero exit.
#[derive(Default)]
struct Checks {
    failures: Vec<&'static str>,
}

impl Checks {
    fn require(&mut self, name: &'static str, ok: bool) {
        println!("check {}: {name}", if ok { "pass" } else { "FAIL" });
        if !ok {
            self.failures.push(name);
        }
    }

    fn finish(self) -> anyhow::Result<()> {
        if self.failures.is_empty() {
            println!("pay_once: all checks passed");
            Ok(())
        } else {
            bail!("pay_once: {} check(s) failed", self.failures.len())
        }
    }
}

fn expected(saved: &Saved) -> anyhow::Result<Expected> {
    Ok(Expected {
        asset: Asset::parse(&saved.asset)?,
        from: saved.payer.parse()?,
        to: saved.pay_to.parse()?,
        amount: saved.amount,
    })
}

/// Sends the saved authorization. Returns the body hash on 2xx.
async fn send(http: &reqwest::Client, saved: &Saved) -> anyhow::Result<Option<String>> {
    let resp = http
        .get(&saved.url)
        .header(x402::HEADER_SIGNATURE, &saved.payment_signature)
        .send()
        .await?;
    let status = resp.status();
    let payment_response = resp
        .headers()
        .get(x402::HEADER_RESPONSE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let body = resp.bytes().await?;
    println!("send: status {status}");
    match payment_response {
        Some(raw) => match x402::decode_header::<PaymentResponse>(&raw) {
            Ok(decoded) => println!(
                "PAYMENT-RESPONSE\n{}",
                serde_json::to_string_pretty(&decoded)?
            ),
            Err(e) => println!("PAYMENT-RESPONSE undecodable ({e}): {raw}"),
        },
        None => println!("no PAYMENT-RESPONSE header"),
    }
    let hash = x402::sha256_hex(&body);
    let shown = body.len().min(400);
    println!(
        "body {} bytes sha256 {hash}\n{}",
        body.len(),
        String::from_utf8_lossy(&body[..shown])
    );
    if status.as_u16() == 402
        && let Some(raw) = resp_header_required(&body)
    {
        println!("seller answered 402 again: {raw}");
    }
    Ok(status.is_success().then_some(hash))
}

fn resp_header_required(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").map(ToString::to_string))
}

async fn report_records(
    mirror: &MirrorNode,
    mirror_id: &str,
    expected: &Expected,
) -> anyhow::Result<Settlement> {
    let records = mirror.records(mirror_id).await?;
    let results: Vec<&str> = records.iter().map(|r| r.result.as_str()).collect();
    println!(
        "mirror records for {mirror_id}: {} {results:?}",
        records.len()
    );
    Ok(hedera::settlement(&records, expected))
}
