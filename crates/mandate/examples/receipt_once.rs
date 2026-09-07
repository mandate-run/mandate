//! Issue 9: publishes receipt 0 to the configured topic and reads it back
//! from the mirror node. Exits non-zero unless the bytes match.
//!
//! ```text
//! cargo run -p mandate --example receipt_once
//! ```

use std::time::Duration;

use anyhow::{Context as _, bail};
use mandate::config::Config;
use mandate::hedera::{HederaTopicId, MirrorNode};
use mandate::receipts::Receipt;
use mandate::x402::sha256_hex;
use time::format_description::well_known::Rfc3339;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = Config::load()?;
    let topic_id = cfg
        .hcs_topic_id
        .clone()
        .context("HCS_TOPIC_ID is not set; run `mandate topic create` first")?;
    let topic: HederaTopicId = topic_id.parse()?;
    let consensus = cfg.signer()?.consensus(cfg.network);
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let mirror = MirrorNode::new(http, cfg.mirror_node_url.clone());

    let now = time::OffsetDateTime::now_utc().format(&Rfc3339)?;
    let mandate_id = format!("spike-{}", now.replace([':', '-'], ""));
    let receipt = Receipt::start(
        &mandate_id,
        &sha256_hex(b"spike mandate"),
        &sha256_hex(b"spike manifest"),
        &now,
    );
    let message = receipt.message()?;
    println!(
        "receipt 0, {} bytes:\n{}",
        message.len(),
        String::from_utf8_lossy(&message)
    );

    let submitted = consensus.submit_message(topic, &message, 5_000_000).await?;
    let seq = submitted.sequence;
    println!(
        "submitted to {topic_id} as sequence {seq}, transaction {}",
        submitted.transaction_id
    );
    println!("{}/topic/{topic_id}", cfg.network.hashscan());

    let mut seen = None;
    for _ in 0..18 {
        if let Some(m) = mirror.topic_message(&topic_id, seq).await? {
            seen = Some(m);
            break;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    let Some(seen) = seen else {
        bail!("mirror node did not return sequence {seq} within 90 s");
    };
    let same = seen.bytes()? == message;
    println!(
        "mirror node: sequence {} at {} from {:?}, bytes match: {same}",
        seen.sequence_number, seen.consensus_timestamp, seen.payer_account_id
    );
    if !same {
        bail!("mirror node bytes differ from what was submitted");
    }
    println!("receipt_once: all checks passed");
    Ok(())
}
