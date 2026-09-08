//! The protocol side of the oracle: Proctor's own probes, independent of the
//! application under test. A facilitator or mirror node that Proctor cannot
//! reach is an infrastructure error, never a finding against the code; a test
//! purchase that is neither settled nor failed is unresolved exposure, which
//! outranks every other outcome.

use std::time::Duration;

use crate::checks::{CheckResult, Outcome};

/// Whether Proctor itself can reach the services a live check needs. The
/// application's failure to reach them is a finding; Proctor's own failure
/// is not, because a facilitator outage must never rewrite working payment
/// code.
pub async fn reachable(url: &str, timeout: Duration) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("{url} answered {}", resp.status()))
    }
}

/// Reclassifies a run's results once the buyer's ledger is read: exposure
/// that no record has decided outranks every other outcome, so a check that
/// merely failed its assertions becomes `PAYMENT_UNRESOLVED` when money is
/// still in the air. Nothing goes to an agent while that is true.
pub fn reclassify(results: &mut [CheckResult], observed: &crate::checks::Measurements) -> Outcome {
    let unresolved = observed
        .get("ledger_unresolved")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    if unresolved > 0 {
        for r in results.iter_mut() {
            r.outcome = Outcome::PaymentUnresolved;
            r.findings.push(format!(
                "{unresolved} authorization(s) neither settled nor failed: exposure is kept until `mandate reconcile` decides"
            ));
        }
    }
    crate::checks::overall(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checks::Measurements;

    fn result(outcome: Outcome) -> CheckResult {
        CheckResult {
            name: "c".into(),
            outcome,
            exit_code: Some(0),
            ms: 1,
            assertions: vec![],
            findings: vec![],
        }
    }

    #[test]
    fn unresolved_exposure_outranks_every_other_outcome() {
        let mut observed = Measurements::new();
        observed.insert("ledger_unresolved".into(), serde_json::json!(1));
        let mut results = vec![result(Outcome::Pass)];
        assert_eq!(
            reclassify(&mut results, &observed),
            Outcome::PaymentUnresolved
        );
        assert!(results[0].findings[0].contains("mandate reconcile"));

        // A failing check is also outranked: the money question comes first.
        let mut results = vec![result(Outcome::ImplementationFailure)];
        assert_eq!(
            reclassify(&mut results, &observed),
            Outcome::PaymentUnresolved
        );
    }

    #[test]
    fn a_settled_run_keeps_the_outcome_its_checks_earned() {
        let mut observed = Measurements::new();
        observed.insert("ledger_unresolved".into(), serde_json::json!(0));
        let mut results = vec![result(Outcome::Pass)];
        assert_eq!(reclassify(&mut results, &observed), Outcome::Pass);
        let mut results = vec![result(Outcome::ImplementationFailure)];
        assert_eq!(
            reclassify(&mut results, &observed),
            Outcome::ImplementationFailure
        );
        // A run with no ledger measurement at all is untouched.
        let mut results = vec![result(Outcome::Pass)];
        assert_eq!(
            reclassify(&mut results, &Measurements::new()),
            Outcome::Pass
        );
    }

    #[tokio::test]
    async fn an_unreachable_service_is_proctors_own_problem() {
        // Nothing listens here, so the probe fails rather than blaming code.
        let e = reachable("http://127.0.0.1:9/supported", Duration::from_millis(300))
            .await
            .unwrap_err();
        assert!(!e.is_empty());
    }
}
