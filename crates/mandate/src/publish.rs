//! Spec section 11 and I9, I10: the receipt queue. Receipts are durable in
//! the ledger first; this publishes the unpublished ones in `seq` order, each
//! under an audit fee cap reserved before the submit, and reconciles the fee
//! from the mirror node afterwards. A failed publish leaves the receipt in
//! the queue and never touches a purchase.

use time::OffsetDateTime;

use crate::hedera::{self, Consensus, HederaTopicId, MirrorNode};
use crate::ledger::{Ledger, LedgerError};

/// Tinybars reserved per receipt before the submit; the record decides the real fee.
pub const FEE_CAP_TINYBAR: i64 = 5_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    pub seq: u64,
    pub hcs_sequence: u64,
    pub tx_id: String,
}

pub struct Publisher<'a> {
    pub consensus: &'a Consensus,
    pub mirror: &'a MirrorNode,
    pub topic: HederaTopicId,
    pub fee_cap: i64,
}

impl Publisher<'_> {
    /// Publishes every unpublished receipt of the mandate, in order, stopping
    /// at the first failure. Returns what was published; the rest stays
    /// queued as `audit_pending`.
    pub async fn publish_pending(
        &self,
        ledger: &mut Ledger,
        mandate_id: &str,
        say: &mut dyn FnMut(String),
    ) -> Result<Vec<Published>, LedgerError> {
        let mut out = Vec::new();
        let receipts = ledger.receipts(mandate_id)?;
        for seq in ledger.unpublished(mandate_id)? {
            let Some((receipt, _)) = receipts.iter().find(|(r, _)| r.seq == seq) else {
                continue;
            };
            let message = match receipt.message() {
                Ok(m) => m,
                Err(e) => {
                    say(format!("receipt {seq} not publishable: {e}"));
                    break;
                }
            };
            match recover_publication(
                ledger,
                self.mirror,
                &self.topic.to_string(),
                mandate_id,
                seq,
                &message,
            )
            .await
            {
                Ok(Recovery::Published(p)) => {
                    out.push(p);
                    continue;
                }
                Ok(Recovery::Ready) => {}
                Ok(Recovery::Pending) => {
                    say(format!(
                        "receipt {seq} awaits the original HCS transaction; no replacement submitted"
                    ));
                    break;
                }
                Err(e) => {
                    say(format!("receipt {seq} reconciliation failed: {e}"));
                    break;
                }
            }
            let now = OffsetDateTime::now_utc();
            let charge = match ledger.reserve_audit(
                mandate_id,
                &format!("receipt {seq}"),
                self.fee_cap,
                now,
            ) {
                Ok(c) => c,
                Err(LedgerError::AuditOverBudget { cap, free }) => {
                    say(format!(
                        "receipt {seq} stays queued: audit cap {cap} exceeds free audit budget {free}"
                    ));
                    break;
                }
                Err(e) => return Err(e),
            };
            let prepared = match self
                .consensus
                .prepare_message(self.topic, &message, self.fee_cap)
            {
                Ok(p) => p,
                Err(e) => {
                    ledger.release_audit(charge.id, now)?;
                    say(format!("receipt {seq} not prepared: {e}"));
                    break;
                }
            };
            ledger.audit_submitted(
                charge.id,
                &prepared.transaction_id,
                &prepared.mirror_id,
                prepared.valid_until,
                now,
            )?;
            match self.consensus.execute(prepared).await {
                Ok(sub) => {
                    ledger.mark_published(
                        mandate_id,
                        seq,
                        sub.sequence,
                        &sub.transaction_id,
                        OffsetDateTime::now_utc(),
                    )?;
                    say(format!(
                        "receipt {seq} {} published as HCS sequence {} tx {}",
                        receipt.outcome_str(),
                        sub.sequence,
                        sub.transaction_id
                    ));
                    out.push(Published {
                        seq,
                        hcs_sequence: sub.sequence,
                        tx_id: sub.transaction_id,
                    });
                }
                Err(e) => {
                    say(format!(
                        "receipt {seq} submit failed: {e}; the charge waits for the mirror record"
                    ));
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Replaces submitted caps with the fees the records charged. An absent
    /// record never proves a submit was free, even after transaction expiry.
    pub async fn reconcile_audit(
        &self,
        ledger: &mut Ledger,
        mandate_id: &str,
    ) -> Result<(), ReconcileError> {
        let mirror = self.mirror;
        for c in ledger.audit_submitted_charges(mandate_id)? {
            let Some(mirror_id) = c.mirror_id.as_deref() else {
                continue;
            };
            let records = mirror.records(mirror_id).await?;
            let now = OffsetDateTime::now_utc();
            let charged: Option<i64> = records
                .iter()
                .filter(|r| r.nonce == 0 && r.result != hedera::DUPLICATE)
                .filter_map(|r| r.charged_tx_fee)
                .max();
            if let Some(fee) = charged {
                ledger.audit_reconciled(c.id, fee, now)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Recovery {
    Ready,
    Pending,
    Published(Published),
}

/// Reuse a publication's persisted identity instead of signing a replacement
/// when consensus accepted it but its response or local acknowledgment was lost.
async fn recover_publication(
    ledger: &mut Ledger,
    mirror: &MirrorNode,
    topic: &str,
    mandate_id: &str,
    seq: u64,
    message: &[u8],
) -> Result<Recovery, ReconcileError> {
    let Some(c) = ledger.latest_audit_charge(mandate_id, &format!("receipt {seq}"))? else {
        return Ok(Recovery::Ready);
    };
    if c.state == "released" {
        return Ok(Recovery::Ready);
    }
    if c.state == "reserved" {
        // No transaction was handed to the network before audit_submitted.
        ledger.release_audit(c.id, OffsetDateTime::now_utc())?;
        return Ok(Recovery::Ready);
    }
    let (Some(id), Some(tx_id)) = (c.mirror_id.as_deref(), c.tx_id.as_deref()) else {
        return Ok(Recovery::Pending);
    };
    let records = mirror.records(id).await?;
    let records: Vec<_> = records
        .iter()
        .filter(|r| r.nonce == 0 && r.result != hedera::DUPLICATE)
        .collect();
    if records.len() != 1 {
        return Ok(Recovery::Pending);
    }
    let record = records[0];
    let Some(fee) = record.charged_tx_fee.filter(|f| *f >= 0) else {
        return Ok(Recovery::Pending);
    };
    if c.state == "submitted" {
        ledger.audit_reconciled(c.id, fee, OffsetDateTime::now_utc())?;
    }
    if record.result != "SUCCESS" {
        return Ok(Recovery::Ready);
    }
    let Some(found) = mirror
        .topic_message_at(topic, &record.consensus_timestamp)
        .await?
    else {
        return Ok(Recovery::Pending);
    };
    if found.bytes().ok().as_deref() != Some(message) {
        return Ok(Recovery::Pending);
    }
    ledger.mark_published(
        mandate_id,
        seq,
        found.sequence_number,
        tx_id,
        OffsetDateTime::now_utc(),
    )?;
    Ok(Recovery::Published(Published {
        seq,
        hcs_sequence: found.sequence_number,
        tx_id: tx_id.to_owned(),
    }))
}

pub use crate::purchase::ReconcileError;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::Network,
        receipts::Receipt,
        testing::{mandate_row, test_signer},
    };
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use std::io::{BufRead as _, Write as _};

    fn mirror(replies: Vec<serde_json::Value>) -> (MirrorNode, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let thread = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            for reply in replies {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((s, _)) => break s,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                std::time::Instant::now() < deadline,
                                "missing mirror request"
                            );
                            std::thread::sleep(std::time::Duration::from_millis(2));
                        }
                        Err(e) => panic!("{e}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let mut line = String::new();
                std::io::BufReader::new(&stream)
                    .read_line(&mut line)
                    .unwrap();
                if reply.get("messages").is_some() {
                    assert!(
                        line.contains(
                            "/api/v1/topics/0.0.99/messages?timestamp=eq%3A1.000000001&limit=1"
                        ),
                        "{line}"
                    );
                } else {
                    assert!(line.contains("/api/v1/transactions/"), "{line}");
                }
                let body = reply.to_string();
                write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap();
        (MirrorNode::new(client, format!("http://{address}")), thread)
    }

    fn submitted() -> (Ledger, Vec<u8>) {
        let now = OffsetDateTime::now_utc() - time::Duration::days(7);
        let mut ledger = Ledger::in_memory().unwrap();
        ledger.insert_mandate(&mandate_row(), now).unwrap();
        let receipt = Receipt::start("m1", "h", "m", "t");
        ledger.append_receipt("m1", &receipt).unwrap();
        let charge = ledger
            .reserve_audit("m1", "receipt 0", FEE_CAP_TINYBAR, now)
            .unwrap();
        ledger
            .audit_submitted(
                charge.id,
                "0.0.7@1.1",
                "0.0.7-1-000000001",
                now + time::Duration::seconds(120),
                now,
            )
            .unwrap();
        (ledger, receipt.message().unwrap())
    }

    fn record(result: &str) -> serde_json::Value {
        serde_json::json!({"transactions": [{"transaction_id": "0.0.7-1-000000001", "nonce": 0, "result": result,
            "charged_tx_fee": 1000, "consensus_timestamp": "1.000000001"}]})
    }

    #[tokio::test]
    async fn absence_after_expiry_keeps_the_cap_and_blocks_republication() {
        let (mut ledger, message) = submitted();
        let empty = serde_json::json!({"transactions": []});
        let (mirror, thread) = mirror(vec![empty.clone(), empty]);
        let consensus = test_signer().consensus(Network::Testnet);
        let publisher = Publisher {
            consensus: &consensus,
            mirror: &mirror,
            topic: "0.0.99".parse().unwrap(),
            fee_cap: FEE_CAP_TINYBAR,
        };
        publisher.reconcile_audit(&mut ledger, "m1").await.unwrap();
        assert_eq!(
            recover_publication(&mut ledger, &mirror, "0.0.99", "m1", 0, &message)
                .await
                .unwrap(),
            Recovery::Pending
        );
        assert_eq!(
            ledger.audit_accounts("m1").unwrap().reserved,
            FEE_CAP_TINYBAR
        );
        thread.join().unwrap();
    }

    #[tokio::test]
    async fn lost_acknowledgment_recovers_the_original_message_and_fee() {
        let (mut ledger, message) = submitted();
        let (mirror, thread) = mirror(vec![
            record("SUCCESS"),
            serde_json::json!({"messages": [{
            "message": STANDARD.encode(&message), "sequence_number": 17, "consensus_timestamp": "1.000000001"}]}),
        ]);
        let recovered = recover_publication(&mut ledger, &mirror, "0.0.99", "m1", 0, &message)
            .await
            .unwrap();
        assert_eq!(
            recovered,
            Recovery::Published(Published {
                seq: 0,
                hcs_sequence: 17,
                tx_id: "0.0.7@1.1".into()
            })
        );
        assert!(ledger.unpublished("m1").unwrap().is_empty());
        assert_eq!(ledger.audit_accounts("m1").unwrap().spent(), 1000);
        assert_eq!(
            ledger
                .latest_audit_charge("m1", "receipt 0")
                .unwrap()
                .unwrap()
                .id,
            1
        );
        thread.join().unwrap();
    }

    #[tokio::test]
    async fn only_a_positive_failure_allows_a_replacement() {
        for result in ["DUPLICATE_TRANSACTION", "INVALID_TOPIC_ID", "SUCCESS"] {
            let (mut ledger, message) = submitted();
            let mut replies = vec![record(result)];
            if result == "SUCCESS" {
                replies.push(serde_json::json!({"messages": []}));
            }
            let (mirror, thread) = mirror(replies);
            let recovered = recover_publication(&mut ledger, &mirror, "0.0.99", "m1", 0, &message)
                .await
                .unwrap();
            assert_eq!(
                recovered,
                if result == "INVALID_TOPIC_ID" {
                    Recovery::Ready
                } else {
                    Recovery::Pending
                }
            );
            assert_eq!(ledger.unpublished("m1").unwrap(), vec![0]);
            thread.join().unwrap();
        }
    }
}
