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
    /// Not settled, still valid, submissions left: commit and send the same bytes.
    Resend(Authorization),
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
            (PaymentState::Settled, DeliveryState::None) => Resume::Retrieve(a),
            (PaymentState::Settled, _) => Resume::Validate(a),
            (PaymentState::Unresolved, _) => Resume::Unresolved(a),
            _ if a.delivery_state == DeliveryState::None
                && now < a.valid_until
                && a.submissions < MAX_SUBMISSIONS =>
            {
                Resume::Resend(a)
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
    use crate::ledger::{MandateRow, PreparedPayment, Request, ReservationSource};
    use time::Duration;
    use time::macros::datetime;

    const NOW: OffsetDateTime = datetime!(2026-09-08 09:00 UTC);

    fn ledger() -> Ledger {
        let mut l = Ledger::in_memory().unwrap();
        l.insert_mandate(
            &MandateRow {
                id: "m1".to_owned(),
                mandate_hash: "a".repeat(64),
                manifest_hash: "b".repeat(64),
                service_total: 10_000,
                service_asset: "0.0.429274".to_owned(),
                audit_total: 50_000_000,
                max_single_payment: 9_000,
                deadline: datetime!(2026-09-13 16:00 UTC),
            },
            NOW,
        )
        .unwrap();
        l
    }

    fn payment(n: u32) -> PreparedPayment {
        PreparedPayment {
            payment_id: format!("pay_{n:0>32}"),
            tx_id: format!("0.0.7162784@1788800000.{n:0>9}"),
            mirror_id: format!("0.0.7162784-1788800000-{n:0>9}"),
            payer: "0.0.10399984".to_owned(),
            amount: 1_000,
            asset: "0.0.429274".to_owned(),
            pay_to: "0.0.10409989".to_owned(),
            fee_payer: "0.0.7162784".to_owned(),
            valid_start: NOW - Duration::seconds(5),
            valid_until: NOW + Duration::seconds(115),
            signature: format!("sig-{n}"),
            quote_json: "{}".to_owned(),
        }
    }

    fn request() -> Request {
        Request {
            method: "GET".to_owned(),
            url: "http://127.0.0.1:4021/spike".to_owned(),
            headers: vec![],
            body: vec![],
        }
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
        for _ in 0..3 {
            l.commit_submission(exhausted.id, NOW).unwrap();
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
        let validate = l
            .prepare("m1", "f", None, &payment(6), &request(), NOW)
            .unwrap();
        l.record_settlement(validate.id, &settled(), NOW).unwrap();
        l.record_delivery(validate.id, b"x", None, NOW).unwrap();

        let plan = plan_recovery(&l, "m1", NOW + Duration::seconds(10)).unwrap();
        let kinds: Vec<(&str, i64)> = plan
            .iter()
            .map(|r| {
                let k = match r {
                    Resume::Resend(_) => "resend",
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
                ("validate", validate.id)
            ]
        );
        if let Resume::Resend(a) = &plan[1] {
            assert_eq!(
                (a.submissions, a.signature.as_str(), a.payment_id.as_str()),
                (1, "sig-2", payment(2).payment_id.as_str())
            );
        }
        let expired = plan_recovery(&l, "m1", NOW + Duration::seconds(120)).unwrap();
        assert!(
            matches!(expired[0], Resume::AwaitRecord(_)),
            "past valid_until nothing is resent"
        );
    }
}
