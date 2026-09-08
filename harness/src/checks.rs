//! Runs checks and classifies outcomes. A check has two halves: `expect`,
//! assertions over the application's own output, and `observe`, measurements
//! Proctor takes from the fixture journal and the buyer's ledger. A check
//! passes only when both agree, because an application is never its own
//! oracle.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::task::{Check, Task, at};

/// The four outcomes, in the precedence the design fixes: unresolved
/// exposure first, then infrastructure, then implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Outcome {
    Pass,
    ImplementationFailure,
    InfrastructureError,
    PaymentUnresolved,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::ImplementationFailure => "IMPLEMENTATION_FAILURE",
            Self::InfrastructureError => "INFRASTRUCTURE_ERROR",
            Self::PaymentUnresolved => "PAYMENT_UNRESOLVED",
        }
    }

    /// Exit code of a `proctor check`.
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Pass => 0,
            Self::ImplementationFailure => 1,
            Self::InfrastructureError => 2,
            Self::PaymentUnresolved => 3,
        }
    }
}

/// One assertion and what was actually there.
#[derive(Debug, Clone, Serialize)]
pub struct Assertion {
    pub key: String,
    pub want: String,
    pub got: Option<String>,
    pub ok: bool,
    /// `expect` reads the application's output; `observe` reads Proctor's own.
    pub source: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub name: String,
    pub outcome: Outcome,
    pub exit_code: Option<i32>,
    pub ms: u128,
    pub assertions: Vec<Assertion>,
    /// What went wrong, in words an agent can act on.
    pub findings: Vec<String>,
}

/// The measurements Proctor takes itself, by name, so a contract can assert
/// on them without knowing how they were gathered.
pub type Measurements = BTreeMap<String, serde_json::Value>;

/// Compares one check's `expect` and `observe` against the application's
/// output and Proctor's measurements. The command's own result decides the
/// outcome first: a crash or a timeout is a failure whatever the assertions
/// would have said.
pub fn judge(check: &Check, out: &crate::hooks::Output, observed: &Measurements) -> CheckResult {
    let mut assertions = Vec::new();
    let mut findings = Vec::new();
    let mut outcome = Outcome::Pass;

    let allowed = |code: Option<i32>| match code {
        Some(0) => true,
        Some(c) => u8::try_from(c)
            .map(|c| check.allow_exit.contains(&c))
            .unwrap_or(false),
        None => false,
    };
    if out.timed_out {
        outcome = Outcome::ImplementationFailure;
        findings.push(format!("{}: timed out after {} ms", check.name, out.ms));
    } else if !allowed(out.code) {
        outcome = Outcome::ImplementationFailure;
        findings.push(format!(
            "{}: exit {}{}\n{}",
            check.name,
            out.code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_owned()),
            if check.allow_exit.is_empty() {
                String::new()
            } else {
                format!(", allowed {:?}", check.allow_exit)
            },
            out.tail(12)
        ));
    }

    // `expect` reads the application's own output, parsed as JSON.
    if !check.expect.is_empty() {
        match serde_json::from_str::<serde_json::Value>(out.stdout.trim()) {
            Ok(doc) => {
                for (key, want) in &check.expect {
                    let got = at(&doc, key);
                    let ok = got.is_some_and(|g| want.matches(g));
                    if !ok {
                        outcome = outcome.max(Outcome::ImplementationFailure);
                        findings.push(format!(
                            "{}: expected {key} to be {want}, found {}",
                            check.name,
                            got.map(|g| g.to_string())
                                .unwrap_or_else(|| "nothing".to_owned())
                        ));
                    }
                    assertions.push(Assertion {
                        key: key.clone(),
                        want: want.to_string(),
                        got: got.map(|g| g.to_string()),
                        ok,
                        source: "expect",
                    });
                }
            }
            Err(e) => {
                // Malformed application output never becomes a pass.
                outcome = outcome.max(Outcome::ImplementationFailure);
                findings.push(format!("{}: output is not JSON: {e}", check.name));
            }
        }
    }

    // `observe` reads what Proctor measured, independently of the application.
    for (key, want) in &check.observe {
        let got = observed.get(key);
        let ok = got.is_some_and(|g| want.matches(g));
        if !ok {
            outcome = outcome.max(Outcome::ImplementationFailure);
            findings.push(format!(
                "{}: observed {key} is {}, the contract requires {want}",
                check.name,
                got.map(|g| g.to_string())
                    .unwrap_or_else(|| "unmeasured".to_owned())
            ));
        }
        assertions.push(Assertion {
            key: key.clone(),
            want: want.to_string(),
            got: got.map(|g| g.to_string()),
            ok,
            source: "observe",
        });
    }

    CheckResult {
        name: check.name.clone(),
        outcome,
        exit_code: out.code,
        ms: out.ms,
        assertions,
        findings,
    }
}

/// The run's outcome: the worst of its checks, under the fixed precedence.
pub fn overall(results: &[CheckResult]) -> Outcome {
    results
        .iter()
        .map(|r| r.outcome)
        .max()
        .unwrap_or(Outcome::Pass)
}

/// A task's findings, for the agent adapter and the report.
pub fn findings(results: &[CheckResult]) -> Vec<String> {
    results.iter().flat_map(|r| r.findings.clone()).collect()
}

/// Everything one attempt produced.
#[derive(Debug, Clone, Serialize)]
pub struct Attempt {
    pub attempt: u32,
    pub outcome: Outcome,
    pub task: String,
    pub task_hash: String,
    pub checks: Vec<CheckResult>,
    pub observed: Measurements,
    pub at: String,
}

impl Attempt {
    pub fn new(
        task: &Task,
        attempt: u32,
        checks: Vec<CheckResult>,
        observed: Measurements,
    ) -> Self {
        Self {
            attempt,
            outcome: overall(&checks),
            task: task.task.name.clone(),
            task_hash: task.hash.clone(),
            checks,
            observed,
            at: time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::Output;
    use crate::task::Expected;

    fn out(code: i32, stdout: &str) -> Output {
        Output {
            code: Some(code),
            stdout: stdout.to_owned(),
            stderr: String::new(),
            ms: 1,
            timed_out: false,
        }
    }

    fn check(expect: &[(&str, Expected)], observe: &[(&str, Expected)]) -> Check {
        Check {
            name: "recovery".to_owned(),
            run: "x".to_owned(),
            expect: expect
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
            observe: observe
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
            allow_exit: Vec::new(),
            timeout_s: 10,
        }
    }

    #[test]
    fn both_halves_must_agree() {
        let c = check(
            &[("status", Expected::Text("delivered".into()))],
            &[("fixture_settlements", Expected::Int(1))],
        );
        let mut observed = Measurements::new();
        observed.insert("fixture_settlements".to_owned(), serde_json::json!(1));
        let r = judge(&c, &out(0, r#"{"status":"delivered"}"#), &observed);
        assert_eq!(r.outcome, Outcome::Pass);
        assert_eq!(r.assertions.len(), 2);
        assert!(r.assertions.iter().all(|a| a.ok));

        // The application says it worked; Proctor measured otherwise.
        let mut lying = Measurements::new();
        lying.insert("fixture_settlements".to_owned(), serde_json::json!(2));
        let r = judge(&c, &out(0, r#"{"status":"delivered"}"#), &lying);
        assert_eq!(r.outcome, Outcome::ImplementationFailure);
        assert!(
            r.findings[0].contains("observed fixture_settlements is 2"),
            "{:?}",
            r.findings
        );

        // A measurement the contract names but Proctor never took is a failure.
        let r = judge(
            &c,
            &out(0, r#"{"status":"delivered"}"#),
            &Measurements::new(),
        );
        assert!(r.findings[0].contains("unmeasured"), "{:?}", r.findings);
    }

    #[test]
    fn a_crash_a_timeout_or_malformed_output_never_passes() {
        let c = check(&[("status", Expected::Text("delivered".into()))], &[]);
        let observed = Measurements::new();
        let crashed = judge(&c, &out(101, ""), &observed);
        assert_eq!(crashed.outcome, Outcome::ImplementationFailure);
        assert!(crashed.findings[0].contains("exit 101"));

        let timed = judge(
            &c,
            &Output {
                code: None,
                stdout: String::new(),
                stderr: "x".into(),
                ms: 5,
                timed_out: true,
            },
            &observed,
        );
        assert_eq!(timed.outcome, Outcome::ImplementationFailure);
        assert!(timed.findings[0].contains("timed out"));

        let garbage = judge(&c, &out(0, "not json"), &observed);
        assert_eq!(garbage.outcome, Outcome::ImplementationFailure);
        assert!(garbage.findings[0].contains("not JSON"));
    }

    #[test]
    fn a_refusal_exit_code_can_be_the_contract() {
        // A task may require a refusal: exit 3 is then the passing result.
        let mut c = check(&[("status", Expected::Text("refused".into()))], &[]);
        c.allow_exit = vec![3];
        let r = judge(
            &c,
            &Output {
                code: Some(3),
                ..out(3, r#"{"status":"refused"}"#)
            },
            &Measurements::new(),
        );
        assert_eq!(r.outcome, Outcome::Pass);
        // Any other code is still a failure.
        let r = judge(&c, &out(4, r#"{"status":"refused"}"#), &Measurements::new());
        assert_eq!(r.outcome, Outcome::ImplementationFailure);
    }

    #[test]
    fn the_worst_outcome_decides_the_run() {
        let pass = CheckResult {
            name: "a".into(),
            outcome: Outcome::Pass,
            exit_code: Some(0),
            ms: 1,
            assertions: vec![],
            findings: vec![],
        };
        let failed = CheckResult {
            outcome: Outcome::ImplementationFailure,
            ..pass.clone()
        };
        let infra = CheckResult {
            outcome: Outcome::InfrastructureError,
            ..pass.clone()
        };
        let unresolved = CheckResult {
            outcome: Outcome::PaymentUnresolved,
            ..pass.clone()
        };
        assert_eq!(overall(std::slice::from_ref(&pass)), Outcome::Pass);
        assert_eq!(
            overall(&[pass.clone(), failed.clone()]),
            Outcome::ImplementationFailure
        );
        assert_eq!(
            overall(&[failed.clone(), infra.clone()]),
            Outcome::InfrastructureError
        );
        assert_eq!(
            overall(&[infra, unresolved, failed]),
            Outcome::PaymentUnresolved
        );
        assert_eq!(Outcome::PaymentUnresolved.exit_code(), 3);
        assert_eq!(Outcome::Pass.exit_code(), 0);
        // Every outcome has a distinct code, so a caller can tell them apart.
        let codes: std::collections::BTreeSet<u8> = [
            Outcome::Pass,
            Outcome::ImplementationFailure,
            Outcome::InfrastructureError,
            Outcome::PaymentUnresolved,
        ]
        .iter()
        .map(|o| o.exit_code())
        .collect();
        assert_eq!(codes.len(), 4);
    }
}
