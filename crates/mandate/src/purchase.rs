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

// Section 6 forward path: one purchase from an approved quote to a settled,
// delivered authorization, driven by the ledger at every transition. The
// signer is used once, before anything is persisted; every later send reuses
// the stored header.

/// Why a purchase did not start.
#[derive(Debug, thiserror::Error)]
pub enum PayError {
    #[error("{0}")]
    Refused(crate::refusal::Refusal),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error(transparent)]
    Hedera(#[from] hedera::Error),
    #[error("binding: {0}")]
    Binding(#[from] crate::ledger::BindingError),
    #[error("quote for {0} expired before it was paid")]
    QuoteExpired(String),
}

/// What one purchase ended as. The authorization row is the record; the body
/// is present when the delivery was received.
#[derive(Debug, Clone)]
pub struct Purchase {
    pub authorization: Authorization,
    pub settlement: Settlement,
    pub body: Option<Vec<u8>>,
    /// Milliseconds from the first send to the received delivery.
    pub latency_ms: Option<u64>,
    /// Records in the last mirror read.
    pub records: usize,
}

/// One line of progress, printed by the caller.
pub type Say<'a> = &'a mut dyn FnMut(String);

/// The paying client: the signer, the ledger's mirror node and an HTTP client
/// that never follows redirects.
pub struct Payer<'a> {
    pub signer: &'a hedera::Signer,
    pub mirror: &'a MirrorNode,
    pub http: &'a reqwest::Client,
    /// How often the mirror node is read while waiting, section 6: 5 s.
    pub poll: std::time::Duration,
    /// Most retrievals after settlement within one run.
    pub max_retrievals: u32,
}

impl Payer<'_> {
    /// Signs and persists the authorization for `quote`, section 6's first
    /// row. Nothing is sent. The quote must be usable and unrefused.
    pub async fn prepare(
        &self,
        ledger: &mut Ledger,
        mandate_id: &str,
        step: &str,
        reservation_id: Option<i64>,
        quote: &crate::quote::Quote,
        now: OffsetDateTime,
    ) -> Result<Authorization, PayError> {
        if let Some(r) = quote.refusal() {
            return Err(PayError::Refused(r));
        }
        if !quote.usable_at(now) {
            return Err(PayError::QuoteExpired(quote.listing_id.clone()));
        }
        let fee_payer = quote.fee_payer.as_deref().ok_or_else(|| {
            PayError::Refused(crate::refusal::Refusal {
                code: crate::refusal::Code::OutsideConstraints,
                detail: format!("{}: quote names no fee payer", quote.listing_id),
            })
        })?;
        let nodes = self.mirror.node_account_ids(5).await?;
        let transfer = hedera::Transfer {
            fee_payer: fee_payer.parse().map_err(hedera::Error::from)?,
            pay_to: quote.pay_to.parse().map_err(hedera::Error::from)?,
            asset: Asset::parse(&quote.asset)?,
            amount: quote.amount,
            node_account_ids: &nodes,
            valid_start: now - hedera::VALID_START_SKEW,
            valid_duration: hedera::valid_duration(quote.max_timeout_s),
        };
        let signed = hedera::sign_transfer(self.signer, &transfer)?;
        let payment_id = crate::x402::new_payment_id();
        let payload = crate::x402::PaymentPayload::new(
            &quote.required,
            &quote.accepted,
            signed.base64(),
            &payment_id,
        );
        let header = crate::x402::encode_header(&payload);
        let prepared = crate::ledger::PreparedPayment::from_signature(&header, &payment_id)?
            .with_quote(serde_json::to_string(quote).unwrap_or_default());
        let mut headers = vec![(crate::x402::HEADER_SIGNATURE.to_owned(), header)];
        if !quote.request.body.is_empty() {
            headers.insert(
                0,
                ("content-type".to_owned(), "application/json".to_owned()),
            );
        }
        let request = crate::ledger::Request {
            method: quote.request.method.clone(),
            url: quote.request.url.clone(),
            headers,
            body: quote.request.body.clone(),
        };
        let a = ledger.prepare(mandate_id, step, reservation_id, &prepared, &request, now)?;
        Ok(a)
    }

    /// Sends a persisted authorization and drives it to a terminal payment
    /// state with a delivery, or to `unresolved`, applying section 6's rules:
    /// counters before sends, mirror records as the only settlement evidence,
    /// resend within validity, retrieval after settlement with the same header.
    pub async fn settle(
        &self,
        ledger: &mut Ledger,
        id: i64,
        deadline: OffsetDateTime,
        say: Say<'_>,
    ) -> Result<Purchase, PayError> {
        let a = ledger.authorization(id)?;
        let expected = Expected {
            asset: Asset::parse(&a.asset)?,
            from: a.payer.parse().map_err(hedera::Error::from)?,
            to: a.pay_to.parse().map_err(hedera::Error::from)?,
            amount: a.amount,
        };
        let started = std::time::Instant::now();
        let mut latency_ms = None;
        let mut settlement = Settlement::Absent {
            duplicates_ignored: 0,
        };
        let mut records = 0;
        let mut a = a;
        let mut retrievals_left = self.max_retrievals;
        loop {
            let now = OffsetDateTime::now_utc();
            // 1. Decide the next transmission from the ledger alone.
            let action =
                if a.payment_state.is_terminal() && a.payment_state != PaymentState::Settled {
                    break;
                } else if a.payment_state == PaymentState::Settled {
                    match a.delivery_state {
                        DeliveryState::None if retrievals_left > 0 && now < deadline => {
                            Some("retrieve")
                        }
                        DeliveryState::None => break,
                        _ => break,
                    }
                } else if a.delivery_state == DeliveryState::None
                    && now < a.valid_until
                    && a.submissions < MAX_SUBMISSIONS
                {
                    Some("send")
                } else {
                    None
                };
            // 2. Commit the counter, then transmit the stored bytes.
            if let Some(kind) = action {
                let committed = if kind == "send" {
                    ledger.commit_submission(a.id, now)
                } else {
                    ledger.commit_retrieval(a.id, now)
                };
                match committed {
                    Ok(row) => {
                        a = row;
                        if kind == "retrieve" {
                            retrievals_left -= 1;
                        }
                        say(format!(
                            "{} {}: {} {} (submissions {}, retrievals {})",
                            a.step, a.payment_id, kind, a.request.url, a.submissions, a.retrievals
                        ));
                        match self.transmit(&a).await {
                            Ok((status, body, payment_response))
                                if (200..300).contains(&status) =>
                            {
                                a = ledger.record_delivery(
                                    a.id,
                                    &body,
                                    payment_response.as_deref(),
                                    OffsetDateTime::now_utc(),
                                )?;
                                latency_ms.get_or_insert(started.elapsed().as_millis() as u64);
                                say(format!(
                                    "{} delivery received, {} bytes, response hash {}",
                                    a.step,
                                    body.len(),
                                    a.response_hash.as_deref().unwrap_or("?")
                                ));
                            }
                            Ok((status, body, _)) => {
                                say(format!(
                                    "{} seller answered {status}: {}",
                                    a.step,
                                    String::from_utf8_lossy(&body[..body.len().min(300)])
                                ));
                            }
                            Err(e) => say(format!("{} no response: {e}", a.step)),
                        }
                    }
                    Err(LedgerError::TooSoon { next_at, .. }) => {
                        say(format!("{} next {kind} allowed at {next_at}", a.step));
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            // 3. Reconcile from the mirror node.
            let found = self.mirror.records(&a.mirror_id).await?;
            records = found.len();
            settlement = hedera::settlement(&found, &expected);
            let now = OffsetDateTime::now_utc();
            a = ledger.record_settlement(a.id, &settlement, now)?;
            match (&settlement, a.delivery_state) {
                (
                    Settlement::Settled { .. },
                    DeliveryState::Received | DeliveryState::Validated | DeliveryState::Rejected,
                ) => {
                    say(format!(
                        "{} payment settled: record matches; records {}, duplicates ignored {}",
                        a.step, records, a.duplicates_ignored
                    ));
                    break;
                }
                (Settlement::Settled { .. }, DeliveryState::None) => {
                    say(format!(
                        "{} payment settled, delivery none: retrieval with the original payment",
                        a.step
                    ));
                }
                (Settlement::Failed { results, .. }, _) => {
                    say(format!("{} payment failed: {}", a.step, results.join(",")));
                    break;
                }
                (Settlement::Anomaly { results, .. }, _) => {
                    say(format!(
                        "{} payment anomaly: {}; reconcile by hand",
                        a.step,
                        results.join(",")
                    ));
                    break;
                }
                (Settlement::Absent { .. }, _) => {}
            }
            if a.payment_state == PaymentState::Unresolved {
                say(format!(
                    "{} unresolved: no record by {}; exposure kept",
                    a.step, a.valid_until
                ));
                break;
            }
            if action.is_none() && now >= deadline {
                break;
            }
            tokio::time::sleep(self.poll).await;
        }
        Ok(Purchase {
            body: a.response_body.clone(),
            authorization: a,
            settlement,
            latency_ms,
            records,
        })
    }

    /// One transmission of the stored request. Never re-signs, never follows redirects.
    async fn transmit(&self, a: &Authorization) -> Result<(u16, Vec<u8>, Option<String>), String> {
        let method =
            reqwest::Method::from_bytes(a.request.method.as_bytes()).map_err(|e| e.to_string())?;
        let mut req = self.http.request(method, &a.request.url);
        for (k, v) in &a.request.headers {
            req = req.header(k, v);
        }
        if !a.request.body.is_empty() {
            req = req.body(a.request.body.clone());
        }
        let resp = req.send().await.map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let payment_response = resp
            .headers()
            .get(crate::x402::HEADER_RESPONSE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let body = resp.bytes().await.map_err(|e| e.to_string())?.to_vec();
        Ok((status, body, payment_response))
    }
}
