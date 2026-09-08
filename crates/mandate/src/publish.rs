//! Spec section 11 and I9, I10: the receipt queue. Receipts are durable in
//! the ledger first; this publishes the unpublished ones in `seq` order, each
//! under an audit fee cap reserved before the submit, and reconciles the fee
//! from the mirror node afterwards. A failed publish leaves the receipt in
//! the queue and never touches a purchase.

use time::OffsetDateTime;

use crate::hedera::{self, Consensus, HederaTopicId, MirrorNode, RECORD_GRACE};
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

    /// Replaces submitted caps with the fees the records charged; a charge
    /// with no record after validity plus grace is released.
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
            match charged {
                Some(fee) => {
                    ledger.audit_reconciled(c.id, fee, now)?;
                }
                None => {
                    if let Some(until) = c.valid_until
                        && now > until + RECORD_GRACE
                    {
                        ledger.audit_not_recorded(c.id, now)?;
                    }
                }
            }
        }
        Ok(())
    }
}

pub use crate::purchase::ReconcileError;
