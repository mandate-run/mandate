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

/// The authorization states that are exposure: the payment is neither
/// settled nor failed, so the amount is neither spent nor free. Mandate's
/// invariant I1 counts `prepared`, `sent` and `unresolved` as outstanding,
/// and so does Proctor.
pub const OUTSTANDING: [&str; 3] = ["prepared", "sent", "unresolved"];

/// How much exposure the ledger holds, by state. Empty when no ledger was
/// measured, which is not the same as none: a task that names no ledger
/// makes no claim about payments.
pub fn exposure(observed: &crate::checks::Measurements) -> Vec<(&'static str, i64)> {
    OUTSTANDING
        .iter()
        .filter_map(|state| {
            let n = observed
                .get(&format!("ledger_{state}"))
                .and_then(serde_json::Value::as_i64)?;
            (n > 0).then_some((*state, n))
        })
        .collect()
}

/// Reclassifies a run's results once the buyer's ledger is read: exposure
/// that no record has decided outranks every other outcome, so a check that
/// merely failed its assertions, or passed, becomes `PAYMENT_UNRESOLVED`
/// while money is still in the air. Nothing goes to an agent while that is
/// true.
pub fn reclassify(results: &mut [CheckResult], observed: &crate::checks::Measurements) -> Outcome {
    let held = exposure(observed);
    if held.is_empty() {
        return crate::checks::overall(results);
    }
    let detail = held
        .iter()
        .map(|(state, n)| format!("{n} {state}"))
        .collect::<Vec<_>>()
        .join(", ");
    let total: i64 = held.iter().map(|(_, n)| n).sum();
    for r in results.iter_mut() {
        r.outcome = Outcome::PaymentUnresolved;
        r.findings.push(format!(
            "{total} authorization(s) are exposure ({detail}): neither spent nor free until a record decides, so nothing here is a pass"
        ));
    }
    if results.is_empty() {
        return Outcome::PaymentUnresolved;
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

    fn with(state: &str, n: i64) -> Measurements {
        let mut m = Measurements::new();
        for s in OUTSTANDING {
            m.insert(format!("ledger_{s}"), serde_json::json!(0));
        }
        m.insert(format!("ledger_{state}"), serde_json::json!(n));
        m
    }

    #[test]
    fn every_nonterminal_state_is_exposure_not_only_unresolved() {
        // The probes: a prepared and a sent authorization each reported PASS.
        for state in OUTSTANDING {
            let observed = with(state, 1);
            let mut results = vec![result(Outcome::Pass)];
            assert_eq!(
                reclassify(&mut results, &observed),
                Outcome::PaymentUnresolved,
                "a {state} authorization is money in the air"
            );
            assert!(
                results[0].findings[0].contains(state),
                "{:?}",
                results[0].findings
            );
        }
        // A failing check is outranked too: the money question comes first.
        let mut results = vec![result(Outcome::ImplementationFailure)];
        assert_eq!(
            reclassify(&mut results, &with("sent", 2)),
            Outcome::PaymentUnresolved
        );
        assert!(results[0].findings[0].starts_with("2 authorization(s) are exposure"));
        // Several states at once are all named.
        let mut both = with("prepared", 1);
        both.insert("ledger_sent".into(), serde_json::json!(2));
        let mut results = vec![result(Outcome::Pass)];
        reclassify(&mut results, &both);
        let f = &results[0].findings[0];
        assert!(
            f.contains("3 authorization(s)") && f.contains("1 prepared") && f.contains("2 sent"),
            "{f}"
        );
        // Exposure with no checks at all still reports.
        let mut none: Vec<CheckResult> = vec![];
        assert_eq!(
            reclassify(&mut none, &with("sent", 1)),
            Outcome::PaymentUnresolved
        );
    }

    #[test]
    fn a_settled_run_keeps_the_outcome_its_checks_earned() {
        let observed = with("settled", 3);
        let mut results = vec![result(Outcome::Pass)];
        assert_eq!(reclassify(&mut results, &observed), Outcome::Pass);
        let mut results = vec![result(Outcome::ImplementationFailure)];
        assert_eq!(
            reclassify(&mut results, &observed),
            Outcome::ImplementationFailure
        );
        // A run with no ledger measurement makes no claim about payments.
        let mut results = vec![result(Outcome::Pass)];
        assert_eq!(
            reclassify(&mut results, &Measurements::new()),
            Outcome::Pass
        );
        assert!(exposure(&Measurements::new()).is_empty());
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
