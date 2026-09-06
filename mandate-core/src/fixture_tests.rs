use crate::fixture::*;
use crate::types::*;

fn run(scenario: Scenario) -> ScenarioRun {
    run_scenario(&dev_mandate(), &dev_listings(), scenario).expect("scenario runs")
}

#[test]
fn normal_completion_matches_demo_numbers() {
    let run = run(Scenario::Normal);
    let t = &run.transcript;

    // Quotes: screen and investigate live, events and explain as ceiling
    // estimates, plus the explain purchase quote.
    assert_eq!(t.quotes.len(), 5);
    let live: Vec<&QuoteRow> = t.quotes.iter().filter(|q| q.source == "live").collect();
    assert_eq!(live.len(), 3);
    assert!(live.iter().all(|q| q.within_tariff && q.listing_match && q.fee_payer_ok));
    assert_eq!(
        live[0].amount, "0.0010",
        "screen quotes at its 0.0002/pool ceiling for five pools"
    );
    assert_eq!(live[1].amount, "0.0080");
    assert_eq!(live[2].amount, "0.0003");

    // First planning phase: staged chosen with the demo's expected/bound.
    let phase1 = &t.plan_phases[0];
    assert_eq!(phase1.chosen.name, "staged");
    assert_eq!(phase1.chosen.expected, "0.0033");
    assert_eq!(phase1.chosen.bound, "0.0093");
    let hybrid = phase1
        .rejected
        .iter()
        .find(|r| r.name == "hybrid")
        .expect("hybrid rejected");
    assert_eq!(hybrid.expected, "0.0042");
    assert_eq!(hybrid.bound, "0.0090");
    let bundle = phase1
        .rejected
        .iter()
        .find(|r| r.name == "bundle")
        .expect("bundle rejected");
    assert_eq!(bundle.expected, "0.0080");

    // Reserve for the explanation at its ceiling (I4), consumed by the purchase.
    let res = t
        .reservations
        .iter()
        .find(|r| r.step == "explain")
        .expect("explain reservation");
    assert_eq!(res.amount, "0.0008");
    assert_eq!(res.source, "ceiling_at_max");
    assert_eq!(res.state, "consumed");

    // Screen bought, then outcomes: 1 pending, 4 non_material.
    assert_eq!(t.steps.len(), 3, "screen, events, explain");
    assert_eq!(t.steps[0].step, "screen");
    assert_eq!(t.steps[1].step, "events");
    assert_eq!(t.steps[2].step, "explain");
    assert!(t.steps[0].payment_id.starts_with("pay_"));
    assert!(t.steps[0].tx_id.starts_with("0.0.7162784@"));

    // Second planning phase: events + explain at 0.0023 vs bundle 0.0032.
    let phase2 = &t.plan_phases[1];
    assert_eq!(phase2.chosen.name, "staged");
    assert_eq!(phase2.chosen.expected, "0.0023");
    assert_eq!(phase2.chosen.bound, "0.0023");

    // Outcomes after events: 1 supported, 4 non_material, 4 claims.
    let supported = t.outcomes.iter().filter(|o| o.outcome == "supported").count();
    let non_material = t.outcomes.iter().filter(|o| o.outcome == "non_material").count();
    assert_eq!(supported, 1);
    assert_eq!(non_material, 4);
    let claims: usize = t.outcomes.iter().map(|o| o.claim_count).sum();
    assert_eq!(claims, 4);

    // Totals: settled 0.0028 (screen 0.0010 + events 0.0015 + explain 0.0003),
    // released 0.0005 (reserve 0.0008 minus the 0.0003 quote), unspent 0.0072.
    assert_eq!(t.totals.settled, "0.0028");
    assert_eq!(t.totals.released, "0.0005");
    assert_eq!(t.totals.unspent, "0.0072");
    assert_eq!(t.totals.unresolved, "0.0000");

    // The explain purchase quote: 0.0001 per KB of the built brief.
    let explain = t
        .quotes
        .iter()
        .find(|q| q.listing_id == "explain" && q.source == "live")
        .expect("live explain quote");
    assert_eq!(explain.amount, "0.0003");
    assert!(explain.amount <= explain.ceiling, "within the ceiling");
    assert!(
        explain.amount < "0.0008".to_string(),
        "quoted below the ceiling-at-max reservation"
    );

    // Validation: coverage 5/5, calculations, citations, provenance 3/3.
    assert_eq!(t.validation.coverage, "5/5");
    assert!(t.validation.calculations);
    assert!(t.validation.citations);
    assert_eq!(t.validation.provenance, "3/3");
    assert!(t.validation.complete);

    // Receipts: receipt 0 plus one per purchase, all published.
    assert_eq!(run.state.receipts.len(), 4);
    assert_eq!(run.state.receipts[0].seq, 0);
    assert!(run.state.receipts[0].mandate_hash.is_some());
    assert_eq!(t.receipts.pending.len(), 0);
}

#[test]
fn bundle_cheaper_chooses_bundle() {
    let t = &run(Scenario::BundleCheaper).transcript;
    let phase = &t.plan_phases[0];
    assert_eq!(phase.chosen.name, "bundle");
    assert_eq!(phase.chosen.expected, "0.0030");
    assert_eq!(phase.chosen.bound, "0.0030");
    assert_eq!(t.steps.len(), 1);
    assert_eq!(t.steps[0].step, "investigate");
    assert_eq!(t.totals.settled, "0.0030");
    assert_eq!(t.totals.unspent, "0.0070");
    assert_eq!(t.validation.coverage, "5/5");
}

#[test]
fn refusal_spends_nothing_and_reports_requirement_unmeetable() {
    let mandate = dev_mandate();
    let mut small = mandate.clone();
    small.budget.service.total = 3_000; // 0.0030 USDC
    let run = run_scenario(&small, &dev_listings(), Scenario::Refusal).expect("refusal runs");
    let t = &run.transcript;

    assert!(t.plan.is_none());
    assert_eq!(t.steps.len(), 0);
    assert_eq!(t.refusals.len(), 1);
    let refusal = &t.refusals[0];
    assert_eq!(refusal.code, "REQUIREMENT_UNMEETABLE");
    assert_eq!(refusal.needed_bound, "0.0080");
    assert_eq!(refusal.needed_expected, "0.0033");
    assert_eq!(refusal.available, "0.0030");

    assert_eq!(t.totals.settled, "0.0000");
    assert_eq!(t.totals.unspent, "0.0030");
    // Receipt 0 plus the refusal receipt, both costing audit budget.
    assert_eq!(run.state.receipts.len(), 2);
    assert!(run.state.receipts.iter().any(|r| r.outcome == ReceiptOutcome::Refused));
    let audit = run.state.audit_spent;
    assert!(audit > 0, "audit budget spent on refusal receipts");
    assert!(audit <= mandate.budget.audit.total, "I10");
}

#[test]
fn off_tariff_quote_is_refused_and_hybrid_wins() {
    let t = &run(Scenario::OffTariff).transcript;

    let refusal = t
        .refusals
        .iter()
        .find(|r| r.code == "OFF_TARIFF")
        .expect("OFF_TARIFF refusal");
    assert_eq!(refusal.needed_bound, "0.0015");
    assert_eq!(refusal.available, "0.0020");

    let phase2 = &t.plan_phases[1];
    assert_eq!(phase2.chosen.name, "hybrid");
    assert_eq!(phase2.chosen.expected, "0.0032");

    assert_eq!(t.steps.len(), 2, "screen, investigate");
    assert_eq!(t.steps[1].step, "investigate");
    assert_eq!(t.totals.settled, "0.0042");
    assert_eq!(t.totals.unspent, "0.0058");
}

#[test]
fn ledger_invariant_holds_after_run() {
    for scenario in [
        Scenario::Normal,
        Scenario::BundleCheaper,
        Scenario::Refusal,
        Scenario::OffTariff,
    ] {
        let run = if scenario == Scenario::Refusal {
            let mut small = dev_mandate();
            small.budget.service.total = 3_000; // 0.0030 USDC
            run_scenario(&small, &dev_listings(), scenario).expect("scenario runs")
        } else {
            run(scenario)
        };
        let mandate = dev_mandate();
        let mut settled = 0i128;
        let mut outstanding = 0i128;
        for auth in &run.state.authorizations {
            match auth.payment_state {
                PaymentState::Settled => settled += auth.amount,
                PaymentState::Prepared | PaymentState::Sent | PaymentState::Unresolved => {
                    outstanding += auth.amount
                }
                PaymentState::Failed => {}
            }
        }
        let held: i128 = run
            .state
            .reservations
            .iter()
            .filter(|r| r.state == ReservationState::Held)
            .map(|r| r.amount)
            .sum();
        assert!(
            settled + outstanding + held <= mandate.budget.service.total,
            "I1 violated in {scenario:?}: {} + {} + {} > {}",
            settled,
            outstanding,
            held,
            mandate.budget.service.total
        );
        // Every authorization settled with a validated delivery.
        for auth in &run.state.authorizations {
            assert_eq!(auth.payment_state, PaymentState::Settled);
            assert_eq!(auth.delivery_state, DeliveryState::Validated);
            assert!(auth.submissions <= 3, "I8");
            assert!(auth.amount <= mandate.constraints.max_single_payment, "I2");
        }
    }
}

#[test]
fn money_formatting_round_trips() {
    assert_eq!(fmt_amount(10_000, 6), "0.0100");
    assert_eq!(fmt_amount(800, 6), "0.0008");
    assert_eq!(fmt_amount(3_300, 6), "0.0033");
    assert_eq!(fmt_amount(1_000, 6), "0.0010");
    assert_eq!(fmt_amount(10_000_000, 6), "10.0000");
    assert_eq!(parse_amount("0.0100", 6).unwrap(), 10_000);
    assert_eq!(parse_amount("0.0033", 6).unwrap(), 3_300);
    assert_eq!(parse_amount("1.5", 6).unwrap(), 1_500_000);
    assert!(parse_amount("0.0000001", 6).is_err());
}