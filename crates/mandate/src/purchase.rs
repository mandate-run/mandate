//! Spec section 6: recovery and reconciliation over the ledger. Recovery is
//! a plan the runtime executes with its HTTP client; reconciliation reads the
//! mirror node and applies I5. Nothing here signs anything.

use time::OffsetDateTime;

use crate::hedera::{self, Asset, Expected, MirrorNode, Settlement};
use crate::ledger::{
    Authorization, DeliveryState, Ledger, LedgerError, MAX_SUBMISSIONS, PaymentState,
};

/// What a resumed authorization needs next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resume {
    /// Not settled, still valid, submissions left, spacing elapsed: commit and send the same bytes.
    Resend(Authorization),
    /// The next send or retrieval is allowed at `at`, section 6's 30 s spacing.
    Backoff(Authorization, OffsetDateTime),
    /// Not settled and no send is allowed: wait for the record set.
    AwaitRecord(Authorization),
    /// Past the grace with no record: exposure kept until `mandate reconcile` finds one.
    Unresolved(Authorization),
    /// Settled without a delivery: commit a retrieval and send the same bytes.
    Retrieve(Authorization),
    /// Settled and received: validate.
    Validate(Authorization),
}

impl Resume {
    pub fn authorization(&self) -> &Authorization {
        match self {
            Self::Resend(a)
            | Self::Backoff(a, _)
            | Self::AwaitRecord(a)
            | Self::Unresolved(a)
            | Self::Retrieve(a)
            | Self::Validate(a) => a,
        }
    }
}

/// Section 6 recovery, decided from the ledger alone. Reconcile first so
/// settlements observed before any HTTP response are already applied.
pub fn plan_recovery(
    ledger: &Ledger,
    mandate_id: &str,
    now: OffsetDateTime,
) -> Result<Vec<Resume>, LedgerError> {
    Ok(ledger
        .resumable(mandate_id)?
        .into_iter()
        .map(|a| match (a.payment_state, a.delivery_state) {
            (PaymentState::Settled, DeliveryState::None) => match a.next_retrieval_at() {
                Some(at) if now < at => Resume::Backoff(a, at),
                _ => Resume::Retrieve(a),
            },
            (PaymentState::Settled, _) => Resume::Validate(a),
            (PaymentState::Unresolved, _) => Resume::Unresolved(a),
            _ if a.delivery_state == DeliveryState::None
                && now < a.valid_until
                && a.submissions < MAX_SUBMISSIONS =>
            {
                match a.next_submission_at() {
                    Some(at) if now < at => Resume::Backoff(a, at),
                    _ => Resume::Resend(a),
                }
            }
            _ => Resume::AwaitRecord(a),
        })
        .collect())
}

#[derive(Debug, thiserror::Error)]
pub enum ReconcileError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error(transparent)]
    Hedera(#[from] hedera::Error),
}

/// One authorization's reconciliation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciled {
    pub id: i64,
    pub tx_id: String,
    pub before: PaymentState,
    pub after: PaymentState,
    pub settlement: Settlement,
}

/// I5 for every non-terminal authorization of a mandate, from the mirror
/// node. Usable at any time, including after the deadline; the only way an
/// `unresolved` row changes.
pub async fn reconcile(
    ledger: &mut Ledger,
    mirror: &MirrorNode,
    mandate_id: &str,
    now: OffsetDateTime,
) -> Result<Vec<Reconciled>, ReconcileError> {
    ledger.mandate(mandate_id)?;
    let mut out = Vec::new();
    for a in ledger.authorizations(mandate_id)? {
        if a.payment_state.is_terminal() {
            continue;
        }
        let expected = Expected {
            asset: Asset::parse(&a.asset)?,
            from: a.payer.parse().map_err(hedera::Error::from)?,
            to: a.pay_to.parse().map_err(hedera::Error::from)?,
            amount: a.amount,
        };
        let records = mirror.records(&a.mirror_id).await?;
        let settlement = hedera::settlement(&records, &expected);
        let after = ledger.record_settlement(a.id, &settlement, now)?;
        out.push(Reconciled {
            id: a.id,
            tx_id: a.tx_id.clone(),
            before: a.payment_state,
            after: after.payment_state,
            settlement,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{PreparedPayment, ReservationSource};
    use crate::testing::{mandate_row, request, signed_payment};
    use time::Duration;
    use time::macros::datetime;

    const NOW: OffsetDateTime = datetime!(2026-09-08 09:00 UTC);

    fn ledger() -> Ledger {
        let mut l = Ledger::in_memory().unwrap();
        l.insert_mandate(&mandate_row(), NOW).unwrap();
        l
    }

    fn payment(n: u32) -> PreparedPayment {
        signed_payment(n, 1_000, "0.0.429274", NOW)
    }

    fn settled() -> Settlement {
        Settlement::Settled {
            consensus_timestamp: "1".to_owned(),
            duplicates_ignored: 0,
        }
    }

    #[test]
    fn recovery_names_the_next_step_for_every_resumable_row() {
        let mut l = ledger();
        let _ = l
            .hold("m1", "explain", 800, ReservationSource::CeilingAtMax, NOW)
            .unwrap();
        let prepared = l
            .prepare("m1", "a", None, &payment(1), &request(), NOW)
            .unwrap();
        let sent = l
            .prepare("m1", "b", None, &payment(2), &request(), NOW)
            .unwrap();
        l.commit_submission(sent.id, NOW).unwrap();
        let exhausted = l
            .prepare("m1", "c", None, &payment(3), &request(), NOW)
            .unwrap();
        for i in 0..3 {
            l.commit_submission(exhausted.id, NOW + Duration::seconds(30 * i))
                .unwrap();
        }
        let unresolved = l
            .prepare("m1", "d", None, &payment(4), &request(), NOW)
            .unwrap();
        l.record_settlement(
            unresolved.id,
            &Settlement::Absent {
                duplicates_ignored: 0,
            },
            NOW + Duration::seconds(200),
        )
        .unwrap();
        let retrieve = l
            .prepare("m1", "e", None, &payment(5), &request(), NOW)
            .unwrap();
        l.record_settlement(retrieve.id, &settled(), NOW).unwrap();
        let retrieved_recently = l
            .prepare("m1", "g", None, &payment(7), &request(), NOW)
            .unwrap();
        l.record_settlement(retrieved_recently.id, &settled(), NOW)
            .unwrap();
        l.commit_retrieval(retrieved_recently.id, NOW + Duration::seconds(65))
            .unwrap();
        let validate = l
            .prepare("m1", "f", None, &payment(6), &request(), NOW)
            .unwrap();
        l.record_settlement(validate.id, &settled(), NOW).unwrap();
        l.record_delivery(validate.id, b"x", None, NOW).unwrap();

        let at = NOW + Duration::seconds(70);
        let plan = plan_recovery(&l, "m1", at).unwrap();
        let kinds: Vec<(&str, i64)> = plan
            .iter()
            .map(|r| {
                let k = match r {
                    Resume::Resend(_) => "resend",
                    Resume::Backoff(..) => "backoff",
                    Resume::AwaitRecord(_) => "await",
                    Resume::Unresolved(_) => "unresolved",
                    Resume::Retrieve(_) => "retrieve",
                    Resume::Validate(_) => "validate",
                };
                (k, r.authorization().id)
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("resend", prepared.id),
                ("resend", sent.id),
                ("await", exhausted.id),
                ("unresolved", unresolved.id),
                ("retrieve", retrieve.id),
                ("backoff", retrieved_recently.id),
                ("validate", validate.id)
            ]
        );
        if let Resume::Resend(a) = &plan[1] {
            assert_eq!(
                (a.submissions, a.signature.as_str(), a.payment_id.as_str()),
                (
                    1,
                    payment(2).signature.as_str(),
                    payment(2).payment_id.as_str()
                )
            );
        }
        if let Resume::Backoff(_, until) = &plan[5] {
            assert_eq!(*until, NOW + Duration::seconds(95));
        }
        let soon = plan_recovery(&l, "m1", NOW + Duration::seconds(10)).unwrap();
        assert!(
            matches!(soon[1], Resume::Backoff(_, _)),
            "sent 10 s ago: wait for the spacing"
        );
        let expired = plan_recovery(&l, "m1", NOW + Duration::seconds(120)).unwrap();
        assert!(
            matches!(expired[0], Resume::AwaitRecord(_)),
            "past valid_until nothing is resent"
        );
    }
}
