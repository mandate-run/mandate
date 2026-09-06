#![allow(clippy::cloned_ref_to_slice_refs, clippy::needless_borrow)]
//! Fixture data and the simulated demo runner.
//!
//! The demo (docs/demo.md) needs live x402 sellers, Hedera testnet settlement
//! and The Graph evidence. This module simulates those three transports with
//! deterministic fixtures while exercising the real runtime logic: the ledger,
//! planning, the purchase state machine, brief construction and validation.
//! The numbers match the normative arithmetic in docs/mandate.md section 6.

use chrono::{Duration, Utc};
use thiserror::Error;

use crate::brief::BriefBuilder;
use crate::ledger::{InMemoryLedger, Ledger, LedgerState};
use crate::plan::{input_kb, PlanBuilder, PlanSelection};
use crate::signing;
use crate::types::*;
use crate::validate::validate_report;

pub const SERVICE_DECIMALS: u8 = 6;
pub const AUDIT_DECIMALS: u8 = 8;
/// Fixture HCS message fee in HBAR per receipt (0.0001 HBAR).
pub const RECEIPT_FEE: i128 = 10_000;
pub const FACILITATOR_FEE_PAYER: &str = "0.0.7162784";
pub const RECEIPTS_TOPIC: &str = "0.0.1000";
pub const SPEC_VERSION: &str = "0.6";

/// The five Uniswap v3 pools on Ethereum mainnet used by the reference mission.
pub const POOLS: [&str; 5] = [
    "0xBa9e1006dEb46A96FD07843B8C4C8C2e287D83a",
    "0x3416cF6C708Da44DB2624D63ea0AAef7113527C6",
    "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640",
    "0x8ad599c3A0ff1De0829EFDDc76F38f1F5f5f5F8",
    "0x5777d92f208679DB4b9778590Fa3CAB3aC9E2168",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    /// Scenario 1: normal completion, staged path chosen.
    Normal,
    /// Scenario 2: the bundle seller quotes 0.0030 live, below its ceiling.
    BundleCheaper,
    /// Scenario 3: service budget 0.0030, nothing fits; refuse before spending.
    Refusal,
    /// Scenario 4: after screening, the events seller quotes above its ceiling.
    OffTariff,
}

impl Scenario {
    pub fn as_str(&self) -> &'static str {
        match self {
            Scenario::Normal => "normal",
            Scenario::BundleCheaper => "bundle",
            Scenario::Refusal => "refusal",
            Scenario::OffTariff => "offtariff",
        }
    }

    pub fn parse(s: &str) -> Result<Scenario, String> {
        match s {
            "normal" => Ok(Scenario::Normal),
            "bundle" => Ok(Scenario::BundleCheaper),
            "refusal" => Ok(Scenario::Refusal),
            "offtariff" => Ok(Scenario::OffTariff),
            other => Err(format!(
                "unknown scenario '{other}' (expected normal|bundle|refusal|offtariff)"
            )),
        }
    }
}

#[derive(Debug, Error)]
pub enum FixtureError {
    #[error("ledger error: {0}")]
    Ledger(#[from] crate::ledger::LedgerError),
    #[error("signing error: {0}")]
    Signing(#[from] signing::SigningError),
    #[error("fixture error: {0}")]
    Other(String),
}

pub type FixtureResult<T> = std::result::Result<T, FixtureError>;

/// The dev mandate: five pools, 0.0100 USDC service budget, transaction-level
/// citations required, one expected material pool (docs/mandate.md section 6).
pub fn dev_mandate() -> Mandate {
    Mandate {
        id: "dev-mandate".into(),
        principal: "0.0.123456".into(),
        purpose: "Explain any material liquidity change in the listed pools over the last 24 hours."
            .into(),
        budget: Budget {
            // 0.0100 USDC, atomic units (6 decimals).
            service: ServiceBudget {
                total: 10_000,
                asset: "0.0.429274".into(),
            },
            // 0.5 HBAR, atomic units (8 decimals).
            audit: AuditBudget {
                total: 50_000_000,
                asset: "0.0.0".into(),
            },
            reserve_completion: true,
        },
        coverage: Coverage::AllMaterial,
        constraints: Constraints {
            networks: vec!["hedera:testnet".into()],
            facilitator: "https://api.testnet.blocky402.com".into(),
            manifest: ManifestRef {
                path: "manifest.json".into(),
                hash: "sha256:dev".into(),
            },
            sellers: SellerConstraint::Allowlist,
            allowlist: Some(
                vec!["screen", "events", "investigate", "explain"]
                    .into_iter()
                    .map(String::from)
                    .collect(),
            ),
            max_single_payment: 9_000, // 0.0090 USDC
            deadline: Utc::now() + Duration::hours(24),
            eth_rpc: "https://eth-mainnet.g.alchemy.com/v2/dev".into(),
        },
        requirements: Requirements {
            evidence: EvidenceRequirement::Transaction,
            citations: CitationRequirement::Required,
            max_data_age_s: 3600,
            provenance_samples: 3,
            brief_events: 5,
            degrade: false,
        },
        duties: Duties {
            receipts_topic: RECEIPTS_TOPIC.into(),
            anchor_before_delivery: false,
            report_refusals: true,
        },
        inputs: Inputs {
            pools: POOLS.iter().map(|p| p.to_string()).collect(),
            window_h: 24,
            materiality: 0.05,
            min_event_usd: 10_000.0,
            expected_material_pools: 1,
        },
    }
}

/// The four reference listings with the tariffs of docs/mandate.md section 6.
pub fn dev_listings() -> Vec<Listing> {
    let asset = "0.0.429274".to_string();
    let network = "hedera:testnet".to_string();
    let pay_to = "0.0.7777777".to_string();
    vec![
        Listing {
            id: "screen".into(),
            seller: "mandate-screen".into(),
            url: "https://sellers.example/screen".into(),
            method: "POST".into(),
            capability: Capability::Screen,
            produces: vec![Produce::Screening],
            tariff: Tariff {
                version: "1.0.0".into(),
                base: 0,
                unit: TariffUnit::Pool,
                unit_price: 200, // 0.0002 per pool
                max_units: 100,
                rounding: Rounding::InputKb,
            },
            network: network.clone(),
            asset: asset.clone(),
            pay_to: pay_to.clone(),
            discovery: None,
        },
        Listing {
            id: "events".into(),
            seller: "mandate-events".into(),
            url: "https://sellers.example/events".into(),
            method: "POST".into(),
            capability: Capability::Events,
            produces: vec![Produce::Transaction],
            tariff: Tariff {
                version: "1.0.0".into(),
                base: 0,
                unit: TariffUnit::PoolWindow,
                unit_price: 1500, // 0.0015 per pool-window
                max_units: 100,
                rounding: Rounding::InputKb,
            },
            network: network.clone(),
            asset: asset.clone(),
            pay_to: pay_to.clone(),
            discovery: None,
        },
        Listing {
            id: "investigate".into(),
            seller: "mandate-investigate".into(),
            url: "https://sellers.example/investigate".into(),
            method: "POST".into(),
            capability: Capability::Investigate,
            produces: vec![Produce::Report],
            tariff: Tariff {
                version: "1.0.0".into(),
                base: 2000,   // 0.0020
                unit: TariffUnit::Pool,
                unit_price: 1200, // 0.0012 per pool
                max_units: 100,
                rounding: Rounding::InputKb,
            },
            network: network.clone(),
            asset: asset.clone(),
            pay_to: pay_to.clone(),
            discovery: None,
        },
        Listing {
            id: "explain".into(),
            seller: "mandate-explain".into(),
            url: "https://sellers.example/explain".into(),
            method: "POST".into(),
            capability: Capability::Explain,
            produces: vec![Produce::Report],
            tariff: Tariff {
                version: "1.0.0".into(),
                base: 0,
                unit: TariffUnit::InputKb,
                unit_price: 100, // 0.0001 per KB of brief
                max_units: 8,
                rounding: Rounding::InputKb,
            },
            network,
            asset,
            pay_to,
            discovery: None,
        },
    ]
}

/// The result of one simulated run: the transcript plus the final ledger state
/// for persistence (reconcile / ledger / receipts commands).
pub struct ScenarioRun {
    pub transcript: Transcript,
    pub state: LedgerState,
}

pub fn run_scenario(
    mandate: &Mandate,
    listings: &[Listing],
    scenario: Scenario,
) -> FixtureResult<ScenarioRun> {
    let ledger = InMemoryLedger::new();
    let mut transcript = Transcript {
        mandate: mandate.id.clone(),
        assumption: mandate.inputs.expected_material_pools.to_string(),
        brief_bound: crate::brief::mandatory_bound(mandate.inputs.pools.len()) as u64,
        quotes: Vec::new(),
        plan: None,
        rejected_plans: Vec::new(),
        plan_phases: Vec::new(),
        reservations: Vec::new(),
        steps: Vec::new(),
        outcomes: Vec::new(),
        validation: ValidationRow {
            coverage: String::new(),
            calculations: false,
            citations: false,
            provenance: String::new(),
            freshness: false,
            schema: false,
            complete: false,
            incomplete_reason: None,
        },
        refusals: Vec::new(),
        totals: TotalsRow {
            settled: "0.0000".into(),
            released: "0.0000".into(),
            unspent: "0.0000".into(),
            unresolved: "0.0000".into(),
            audit_spent: "0.0000".into(),
        },
        receipts: ReceiptsRow {
            topic: mandate.duties.receipts_topic.clone(),
            pending: Vec::new(),
        },
    };

    let mut ledger = ledger;
    let mut runner = Runner {
        mandate,
        listings,
        ledger: &mut ledger,
        next_seq: 0,
        released_total: 0,
        audit_fee_total: 0,
    };
    runner.insert_receipt_0()?;

    match scenario {
        Scenario::Normal => runner.run_normal(&mut transcript)?,
        Scenario::BundleCheaper => runner.run_bundle_cheaper(&mut transcript)?,
        Scenario::Refusal => runner.run_refusal(&mut transcript)?,
        Scenario::OffTariff => runner.run_off_tariff(&mut transcript)?,
    }

    let snapshot = runner.ledger.budget_snapshot(&mandate.id)?;
    transcript.totals = TotalsRow {
        settled: fmt_amount(snapshot.settled, SERVICE_DECIMALS),
        released: fmt_amount(runner.released_total, SERVICE_DECIMALS),
        unspent: fmt_amount(
            mandate.budget.service.total - snapshot.settled - snapshot.outstanding - snapshot.held,
            SERVICE_DECIMALS,
        ),
        unresolved: fmt_amount(snapshot.outstanding, SERVICE_DECIMALS),
        audit_spent: fmt_amount(runner.audit_fee_total, AUDIT_DECIMALS),
    };

    let state = ledger.export();
    Ok(ScenarioRun { transcript, state })
}

struct Runner<'a> {
    mandate: &'a Mandate,
    listings: &'a [Listing],
    ledger: &'a mut InMemoryLedger,
    next_seq: u64,
    released_total: i128,
    audit_fee_total: i128,
}

impl<'a> Runner<'a> {
    fn insert_receipt_0(&mut self) -> FixtureResult<()> {
        let mandate_hash = sha256_hex(
            &serde_json::to_vec(self.mandate).unwrap_or_default(),
        );
        let manifest_hash = sha256_hex(&serde_json::to_vec(self.listings).unwrap_or_default());
        let receipt = Receipt {
            seq: self.next_seq,
            mandate_id: self.mandate.id.clone(),
            step: "init".into(),
            listing_id: None,
            seller: None,
            amount: 0,
            asset: self.mandate.budget.service.asset.clone(),
            tx_id: None,
            payment_id_hash: None,
            request_hash: None,
            response_hash: None,
            outcome: ReceiptOutcome::Paid,
            reason: None,
            latency_ms: None,
            at: Utc::now(),
            mandate_hash: Some(mandate_hash),
            manifest_hash: Some(manifest_hash),
            spec_version: Some(SPEC_VERSION.into()),
        };
        self.commit_receipt(receipt)?;
        Ok(())
    }

    fn run_normal(&mut self, t: &mut Transcript) -> FixtureResult<()> {
        self.run_screened_path(t, false)
    }

    fn run_off_tariff(&mut self, t: &mut Transcript) -> FixtureResult<()> {
        self.run_screened_path(t, true)
    }

    /// Scenarios 1 and 4: screen first, then either buy events (1) or refuse
    /// the off-tariff events quote and buy the one-pool bundle instead (4).
    fn run_screened_path(&mut self, t: &mut Transcript, off_tariff_events: bool) -> FixtureResult<()> {
        // Phase 1: quote screen and investigate live; estimate events and explain.
        let quotes = self.initial_quotes()?;
        let selection = PlanBuilder::new(self.mandate, self.listings, &quotes).evaluate();
        self.record_quotes(t, &quotes, &selection)?;
        self.record_plan(t, &selection);
        self.hold_reservations(&selection, t)?;

        // Buy screen for all five pools.
        let screen_quote = self.quote_for(&quotes, Capability::Screen, 5)?;
        let screening = self.screening_evidence();
        self.buy(&screen_quote, "screen", 5, screening.clone(), t)?;

        let mut outcomes = outcomes_from(&self.mandate, &[screening.clone()]);
        t.outcomes = outcome_rows(&self.mandate, &outcomes, &[]);

        // Phase 2: re-plan over W (the pending pool).
        let pending = outcomes
            .iter()
            .filter(|o| **o == PoolOutcome::Pending)
            .count();
        let free = self.free_budget()?;
        let quotes2 = self.post_screen_quotes(pending, off_tariff_events)?;
        let selection2 = PlanBuilder::new(self.mandate, self.listings, &quotes2)
            .with_state(0, pending, free)
            .evaluate();
        // Phase-2 prices live in the plan phases; the quotes table holds the
        // initial live quotes, estimates and the explain purchase quote.
        self.record_plan(t, &selection2);

        let events_quote = self.quote_for(&quotes2, Capability::Events, pending as i128)?;
        if !events_quote.within_tariff {
            // Scenario 4: OFF_TARIFF refusal of the events quote; the staged
            // plan is rejected and the one-pool bundle becomes the cheapest
            // path to a cited answer.
            t.refusals.push(RefusalRow {
                code: "OFF_TARIFF".into(),
                needed_bound: fmt_amount(events_quote.ceiling, SERVICE_DECIMALS),
                needed_expected: fmt_amount(events_quote.ceiling, SERVICE_DECIMALS),
                available: fmt_amount(events_quote.amount, SERVICE_DECIMALS),
                reason: Some(format!(
                    "events ceiling {} quoted {}",
                    fmt_amount(events_quote.ceiling, SERVICE_DECIMALS),
                    fmt_amount(events_quote.amount, SERVICE_DECIMALS)
                )),
            });
            self.commit_refusal_receipt("events", "OFF_TARIFF")?;
            // The explain reserve is no longer needed: the hybrid plan's final
            // step is the investigate purchase itself.
            self.release_reservation("res-final-staged")?;
            if let Some(row) = t.reservations.iter_mut().find(|r| r.step == "explain") {
                row.state = "released".into();
            }
            let investigate = self.quote_for(&quotes2, Capability::Investigate, pending as i128)?;
            let evidence = self.events_evidence();
            self.buy(&investigate, "investigate", pending as i128, evidence.clone(), t)?;
            outcomes = outcomes_from(&self.mandate, &[screening, evidence.clone()]);
            let claims = compute_claims(&self.mandate, &[evidence.clone()]);
            let brief = BriefBuilder::new().build_brief(
                &self.mandate,
                &outcomes,
                &claims,
                &[evidence.clone()],
                self.mandate.requirements.brief_events,
            );
            let explain_quote = self.explain_quote(&brief)?;
            let report = validate_report(
                &self.mandate,
                &outcomes,
                &claims,
                &[evidence.clone()],
                &explain_quote,
                Some((3, 3)),
                true,
            );
            t.outcomes = outcome_rows(&self.mandate, &outcomes, &claims);
            t.validation = report.row;
            return Ok(());
        }

        // Scenario 1: events for the pending pool (staged path is cheapest).
        let events = self.events_evidence();
        self.buy(&events_quote, "events", pending as i128, events.clone(), t)?;

        outcomes = outcomes_from(&self.mandate, &[screening.clone(), events.clone()]);
        let claims = compute_claims(&self.mandate, &[events.clone()]);

        // Build the brief, quote explain, consume the reserve and buy.
        let brief = BriefBuilder::new().build_brief(
            &self.mandate,
            &outcomes,
            &claims,
            &[screening, events],
            self.mandate.requirements.brief_events,
        );
        let explain_quote = self.explain_quote(&brief)?;
        self.push_quote_row(t, &explain_quote, "live", 1);
        let explain_response = serde_json::json!({
            "prose": "One material liquidity change was identified in the listed pools over the window.",
        });
        self.buy_json(&explain_quote, "explain", explain_quote.requested_units, explain_response, t)?;

        t.outcomes = outcome_rows(&self.mandate, &outcomes, &claims);
        let provenance = if claims
            .iter()
            .any(|c| c.evidence.iter().any(|e| e.kind == EvidenceKind::EventFact))
        {
            Some((3, 3))
        } else {
            None
        };
        let report = validate_report(
            &self.mandate,
            &outcomes,
            &claims,
            &[self.screening_evidence(), self.events_evidence()],
            &explain_quote,
            provenance,
            true,
        );
        t.validation = report.row;
        Ok(())
    }

    fn run_bundle_cheaper(&mut self, t: &mut Transcript) -> FixtureResult<()> {
        let quotes = self.initial_quotes_bundle()?;
        let selection = PlanBuilder::new(self.mandate, self.listings, &quotes).evaluate();
        self.record_quotes(t, &quotes, &selection)?;
        self.record_plan(t, &selection);
        self.hold_reservations(&selection, t)?;

        let investigate = self.quote_for(&quotes, Capability::Investigate, 5)?;
        let evidence = self.investigate_evidence();
        self.buy(&investigate, "investigate", 5, evidence.clone(), t)?;

        let outcomes = outcomes_from(&self.mandate, &[evidence.clone()]);
        let claims = compute_claims(&self.mandate, &[evidence.clone()]);
        t.outcomes = outcome_rows(&self.mandate, &outcomes, &claims);
        let brief = BriefBuilder::new().build_brief(
            &self.mandate,
            &outcomes,
            &claims,
            &[evidence.clone()],
            self.mandate.requirements.brief_events,
        );
        let explain_quote = self.explain_quote(&brief)?;
        let report = validate_report(
            &self.mandate,
            &outcomes,
            &claims,
            &[evidence.clone()],
            &explain_quote,
            Some((3, 3)),
            true,
        );
        t.validation = report.row;
        Ok(())
    }

    fn run_refusal(&mut self, t: &mut Transcript) -> FixtureResult<()> {
        let quotes = self.initial_quotes()?;
        let selection = PlanBuilder::new(self.mandate, self.listings, &quotes).evaluate();
        self.record_quotes(t, &quotes, &selection)?;
        self.record_plan(t, &selection);
        if selection.chosen.is_some() {
            return Err(FixtureError::Other(
                "refusal scenario requires a service budget no plan can fit".into(),
            ));
        }

        let lowest_bound = selection
            .rejected
            .iter()
            .map(|r| r.bound)
            .min()
            .unwrap_or(0);
        let lowest_expected = selection
            .rejected
            .iter()
            .map(|r| r.expected)
            .min()
            .unwrap_or(0);
        let available = self.mandate.budget.service.total;
        t.refusals.push(RefusalRow {
            code: "REQUIREMENT_UNMEETABLE".into(),
            needed_bound: fmt_amount(lowest_bound, SERVICE_DECIMALS),
            needed_expected: fmt_amount(lowest_expected, SERVICE_DECIMALS),
            available: fmt_amount(available, SERVICE_DECIMALS),
            reason: None,
        });
        self.commit_refusal_receipt("run", "REQUIREMENT_UNMEETABLE")?;
        Ok(())
    }

    // ---- quoting ----

    fn initial_quotes(&self) -> FixtureResult<Vec<Quote>> {
        let pools = self.mandate.inputs.pools.len() as i128;
        let now = Utc::now();
        Ok(vec![
            self.quote(Capability::Screen, pools, 0.0010, now)?,
            self.quote(Capability::Investigate, pools, 0.0080, now)?,
        ])
    }

    fn initial_quotes_bundle(&self) -> FixtureResult<Vec<Quote>> {
        let pools = self.mandate.inputs.pools.len() as i128;
        let now = Utc::now();
        Ok(vec![
            self.quote(Capability::Screen, pools, 0.0010, now)?,
            self.quote(Capability::Investigate, pools, 0.0030, now)?,
        ])
    }

    fn post_screen_quotes(&self, pending: usize, off_tariff_events: bool) -> FixtureResult<Vec<Quote>> {
        let now = Utc::now();
        let pending = pending as i128;
        let events_amount = if off_tariff_events { 0.0020 } else { 0.0015 };
        Ok(vec![
            self.quote(Capability::Events, pending, events_amount, now)?,
            self.quote(Capability::Investigate, pending, 0.0032, now)?,
        ])
    }

    /// Build a quote from the listing's tariff and a simulated 402 amount.
    /// `off_tariff` forces the amount above the ceiling for the OffTariff path.
    fn quote(
        &self,
        capability: Capability,
        units: i128,
        amount_usdc: f64,
        now: chrono::DateTime<Utc>,
    ) -> FixtureResult<Quote> {
        let listing = self
            .listings
            .iter()
            .find(|l| l.capability == capability)
            .ok_or_else(|| FixtureError::Other(format!("no listing for {capability:?}")))?;
        let amount = (amount_usdc * 1_000_000.0) as i128;
        let ceiling = crate::plan::ceiling(listing, units);
        Ok(Quote {
            listing_id: listing.id.clone(),
            amount,
            asset: listing.asset.clone(),
            network: listing.network.clone(),
            pay_to: listing.pay_to.clone(),
            fee_payer: FACILITATOR_FEE_PAYER.into(),
            max_timeout_s: 30,
            received_at: now,
            ceiling,
            within_tariff: amount <= ceiling,
            listing_match: true,
            fee_payer_ok: true,
            request_binding: RequestBinding {
                method: listing.method.clone(),
                url: listing.url.clone(),
                body_hash: String::new(),
            },
            requested_units: units,
        })
    }

    fn explain_quote(&self, brief: &crate::brief::BuiltBrief) -> FixtureResult<Quote> {
        let listing = self
            .listings
            .iter()
            .find(|l| l.capability == Capability::Explain)
            .unwrap();
        let kb = input_kb(brief.total_bytes);
        let amount = listing.tariff.unit_price * kb; // 0.0001 per KB
        let ceiling = crate::plan::ceiling(listing, kb);
        Ok(Quote {
            listing_id: listing.id.clone(),
            amount,
            asset: listing.asset.clone(),
            network: listing.network.clone(),
            pay_to: listing.pay_to.clone(),
            fee_payer: FACILITATOR_FEE_PAYER.into(),
            max_timeout_s: 30,
            received_at: Utc::now(),
            ceiling,
            within_tariff: amount <= ceiling,
            listing_match: true,
            fee_payer_ok: true,
            request_binding: RequestBinding {
                method: listing.method.clone(),
                url: listing.url.clone(),
                body_hash: String::new(),
            },
            requested_units: kb,
        })
    }

    fn quote_for(
        &self,
        quotes: &[Quote],
        capability: Capability,
        units: i128,
    ) -> FixtureResult<Quote> {
        quotes
            .iter()
            .find(|q| {
                q.listing_id
                    == self
                        .listings
                        .iter()
                        .find(|l| l.capability == capability)
                        .unwrap()
                        .id
                    && q.requested_units == units
            })
            .cloned()
            .ok_or_else(|| FixtureError::Other(format!("no quote for {capability:?} @ {units}")))
    }

    // ---- plan and ledger plumbing ----

    fn free_budget(&self) -> FixtureResult<i128> {
        let snap = self.ledger.budget_snapshot(&self.mandate.id)?;
        Ok(self.mandate.budget.service.total - snap.settled - snap.outstanding - snap.held)
    }

    fn hold_reservations(&mut self, selection: &PlanSelection, t: &mut Transcript) -> FixtureResult<()> {
        for res in &selection.reservations {
            self.ledger.insert_reservation(res)?;
            t.reservations.push(ReservationRow {
                step: res.step.clone(),
                amount: fmt_amount(res.amount, SERVICE_DECIMALS),
                source: match res.source {
                    ReservationSource::CeilingAtMax => "ceiling_at_max".into(),
                    ReservationSource::Quote => "quote".into(),
                },
                state: "held".into(),
            });
        }
        Ok(())
    }

    fn release_reservation(&mut self, id: &str) -> FixtureResult<()> {
        let reservations = self.ledger.get_reservations(&self.mandate.id)?;
        if let Some(res) = reservations.iter().find(|r| r.id == id) {
            self.released_total += res.amount;
            self.ledger.release_reservation(id)?;
        }
        Ok(())
    }

    fn commit_receipt(&mut self, mut receipt: Receipt) -> FixtureResult<u64> {
        receipt.seq = self.next_seq;
        self.next_seq += 1;
        let seq = self.ledger.insert_receipt(&receipt)?;
        self.audit_fee_total += RECEIPT_FEE;
        self.ledger.add_audit_spend(&self.mandate.id, RECEIPT_FEE);
        Ok(seq)
    }

    fn commit_refusal_receipt(&mut self, step: &str, code: &str) -> FixtureResult<()> {
        let receipt = Receipt {
            seq: self.next_seq,
            mandate_id: self.mandate.id.clone(),
            step: step.into(),
            listing_id: None,
            seller: None,
            amount: 0,
            asset: self.mandate.budget.service.asset.clone(),
            tx_id: None,
            payment_id_hash: None,
            request_hash: None,
            response_hash: None,
            outcome: ReceiptOutcome::Refused,
            reason: Some(code.into()),
            latency_ms: None,
            at: Utc::now(),
            mandate_hash: None,
            manifest_hash: None,
            spec_version: None,
        };
        self.commit_receipt(receipt)?;
        Ok(())
    }

    // ---- purchases ----

    fn buy(
        &mut self,
        quote: &Quote,
        step: &str,
        units: i128,
        response: EvidenceResponse,
        t: &mut Transcript,
    ) -> FixtureResult<()> {
        self.buy_json(quote, step, units, serde_json::to_value(response).unwrap(), t)
    }

    /// Full purchase lifecycle for one authorization: build and sign, persist
    /// while moving held -> outstanding, commit the submission, send, observe
    /// the settlement record, receive and validate the delivery, write the
    /// receipt (spec sections 6 and 10).
    fn buy_json(
        &mut self,
        quote: &Quote,
        step: &str,
        _units: i128,
        response: serde_json::Value,
        t: &mut Transcript,
    ) -> FixtureResult<()> {
        let listing = self
            .listings
            .iter()
            .find(|l| l.id == quote.listing_id)
            .unwrap();
        let now = Utc::now();
        let payload = signing::build_exact_payload(
            self.mandate.principal.clone(),
            listing.pay_to.clone(),
            quote.asset.clone(),
            quote.amount,
            quote.fee_payer.clone(),
            quote.max_timeout_s,
            now.timestamp(),
            now.timestamp_millis(),
        )?;
        let tx_id = signing::tx_id_from_payload(&payload);
        let payment_id = format!("pay_{}", uuid4(&format!("{}-{}", self.mandate.id, step)));
        let body = serde_json::to_vec(&response).unwrap();
        // Consume a held reservation for this step (I7: held -> outstanding).
        // The unspent part of a ceiling-at-max reservation is released.
        let reservations = self.ledger.get_reservations(&self.mandate.id)?;
        for res in reservations
            .iter()
            .filter(|r| r.step == step && r.state == ReservationState::Held)
        {
            self.released_total += (res.amount - quote.amount).max(0);
            self.ledger.release_reservation(&res.id)?;
            if let Some(row) = t
                .reservations
                .iter_mut()
                .find(|r| r.step == res.step && r.state == "held")
            {
                row.state = "consumed".into();
            }
        }

        let mut auth = Authorization {
            id: format!("auth-{}", t.steps.len() + 1),
            quote_id: quote.listing_id.clone(),
            payment_id: payment_id.clone(),
            tx_id: tx_id.clone(),
            amount: quote.amount,
            valid_start: now,
            valid_until: now + Duration::seconds(quote.max_timeout_s),
            signed_bytes: serde_json::to_vec(&payload).unwrap(),
            request: RequestSent {
                method: listing.method.clone(),
                url: listing.url.clone(),
                headers: vec![
                    ("PAYMENT-SIGNATURE".into(), String::new()),
                    ("X-PAYMENT-ID".into(), payment_id.clone()),
                ],
                body: Vec::new(),
            },
            submissions: 1,
            retrievals: 0,
            payment_state: PaymentState::Prepared,
            delivery_state: DeliveryState::None,
            response_body: None,
            response_hash: None,
        };
        self.ledger.insert_authorization(&auth)?;
        // prepared -> sent: submission committed (1), then transmitted
        auth.payment_state = PaymentState::Sent;
        self.ledger.update_authorization(&auth)?;
        // settlement record observed: SUCCESS with matching transfers
        auth.payment_state = PaymentState::Settled;
        self.ledger.update_authorization(&auth)?;
        // delivery: 2xx with body
        auth.delivery_state = DeliveryState::Received;
        auth.response_body = Some(body.clone());
        auth.response_hash = Some(sha256_hex(&body));
        self.ledger.update_authorization(&auth)?;
        auth.delivery_state = DeliveryState::Validated;
        self.ledger.update_authorization(&auth)?;

        t.steps.push(StepRow {
            step: step.to_string(),
            payment_id: payment_id.clone(),
            tx_id,
            submissions: auth.submissions,
            retrievals: auth.retrievals,
            transitions: vec![
                TransitionRow {
                    from: "prepared".into(),
                    to: "sent".into(),
                    at: now,
                    record_count: None,
                    duplicates_ignored: None,
                },
                TransitionRow {
                    from: "sent".into(),
                    to: "settled".into(),
                    at: now,
                    record_count: Some(1),
                    duplicates_ignored: Some(0),
                },
                TransitionRow {
                    from: "none".into(),
                    to: "validated".into(),
                    at: now,
                    record_count: None,
                    duplicates_ignored: None,
                },
            ],
        });

        let receipt = Receipt {
            seq: self.next_seq,
            mandate_id: self.mandate.id.clone(),
            step: step.to_string(),
            listing_id: Some(quote.listing_id.clone()),
            seller: Some(listing.seller.clone()),
            amount: quote.amount,
            asset: quote.asset.clone(),
            tx_id: Some(auth.tx_id.clone()),
            payment_id_hash: Some(sha256_hex(payment_id.clone().as_bytes())),
            request_hash: None,
            response_hash: auth.response_hash.clone(),
            outcome: ReceiptOutcome::Paid,
            reason: None,
            latency_ms: Some(1),
            at: Utc::now(),
            mandate_hash: None,
            manifest_hash: None,
            spec_version: None,
        };
        self.commit_receipt(receipt)?;
        Ok(())
    }

    // ---- transcript recording ----

    fn record_quotes(
        &self,
        t: &mut Transcript,
        quotes: &[Quote],
        selection: &PlanSelection,
    ) -> FixtureResult<()> {
        for q in quotes {
            self.push_quote_row(t, q, "live", 1);
        }
        // Ceiling estimates for the chosen plan's steps without a live quote.
        if let Some(plan) = &selection.chosen {
            for step in &plan.steps {
                let already = t
                    .quotes
                    .iter()
                    .any(|q| q.listing_id == step.listing_id);
                if already {
                    continue;
                }
                let listing = self
                    .listings
                    .iter()
                    .find(|l| l.capability == step.capability)
                    .unwrap();
                let ceiling = crate::plan::ceiling(listing, step.units_expected);
                let est = Quote {
                    listing_id: listing.id.clone(),
                    amount: ceiling,
                    asset: listing.asset.clone(),
                    network: listing.network.clone(),
                    pay_to: listing.pay_to.clone(),
                    fee_payer: FACILITATOR_FEE_PAYER.into(),
                    max_timeout_s: 0,
                    received_at: Utc::now(),
                    ceiling,
                    within_tariff: true,
                    listing_match: true,
                    fee_payer_ok: true,
                    request_binding: RequestBinding {
                        method: listing.method.clone(),
                        url: listing.url.clone(),
                        body_hash: String::new(),
                    },
                    requested_units: step.units_expected,
                };
                self.push_quote_row(t, &est, "estimate", 0);
            }
        }
        Ok(())
    }

    /// Append a quote row, skipping an identical row already recorded.
    fn push_quote_row(&self, t: &mut Transcript, q: &Quote, source: &str, latency_ms: u64) {
        let row = QuoteRow {
            listing_id: q.listing_id.clone(),
            amount: fmt_amount(q.amount, SERVICE_DECIMALS),
            ceiling: fmt_amount(q.ceiling, SERVICE_DECIMALS),
            within_tariff: q.within_tariff,
            listing_match: q.listing_match,
            fee_payer_ok: q.fee_payer_ok,
            latency_ms,
            source: source.into(),
        };
        if !t.quotes.iter().any(|existing| {
            existing.listing_id == row.listing_id
                && existing.amount == row.amount
                && existing.source == row.source
        }) {
            t.quotes.push(row);
        }
    }

    fn record_plan(&mut self, t: &mut Transcript, selection: &PlanSelection) {
        if let Some(chosen) = &selection.chosen {
            let row = PlanRow {
                name: chosen.kind.as_str().into(),
                expected: fmt_amount(chosen.expected, SERVICE_DECIMALS),
                bound: fmt_amount(chosen.bound, SERVICE_DECIMALS),
                assumption: self.mandate.inputs.expected_material_pools,
                authorizations: chosen.authorizations,
            };
            t.plan = Some(row.clone());
            t.plan_phases.push(PlanPhaseRow {
                chosen: row,
                rejected: selection
                    .rejected
                    .iter()
                    .map(|r| RejectedPlanRow {
                        name: r.name.into(),
                        reason: r.reason.clone(),
                        bound: fmt_amount(r.bound, SERVICE_DECIMALS),
                        expected: fmt_amount(r.expected, SERVICE_DECIMALS),
                    })
                    .collect(),
            });
        } else {
            t.plan_phases.push(PlanPhaseRow {
                chosen: PlanRow {
                    name: "none".into(),
                    expected: "0.0000".into(),
                    bound: "0.0000".into(),
                    assumption: self.mandate.inputs.expected_material_pools,
                    authorizations: 0,
                },
                rejected: selection
                    .rejected
                    .iter()
                    .map(|r| RejectedPlanRow {
                        name: r.name.into(),
                        reason: r.reason.clone(),
                        bound: fmt_amount(r.bound, SERVICE_DECIMALS),
                        expected: fmt_amount(r.expected, SERVICE_DECIMALS),
                    })
                    .collect(),
            });
        }
        // Merge per-phase rejections into the transcript's flat list.
        if let Some(phase) = t.plan_phases.last() {
            for r in &phase.rejected {
                if !t
                    .rejected_plans
                    .iter()
                    .any(|x| x.name == r.name && x.reason == r.reason)
                {
                    t.rejected_plans.push(r.clone());
                }
            }
        }
    }

    // ---- fixture evidence ----

    fn screening_evidence(&self) -> EvidenceResponse {
        let mut pools = Vec::new();
        for (i, pool) in self.mandate.inputs.pools.iter().enumerate() {
            pools.push(screening_pool(pool, i == 0, false));
        }
        evidence_response(self.mandate, pools)
    }

    fn events_evidence(&self) -> EvidenceResponse {
        let pool = self.mandate.inputs.pools[0].clone();
        let mut p = screening_pool(&pool, true, false);
        p.events = material_events(&pool);
        evidence_response(self.mandate, vec![p])
    }

    fn investigate_evidence(&self) -> EvidenceResponse {
        let mut pools = Vec::new();
        for (i, pool) in self.mandate.inputs.pools.iter().enumerate() {
            let mut p = screening_pool(pool, i == 0, false);
            if i == 0 {
                p.events = material_events(pool);
            }
            pools.push(p);
        }
        evidence_response(self.mandate, pools)
    }
}

fn evidence_response(mandate: &Mandate, pools: Vec<PoolEvidence>) -> EvidenceResponse {
    let now = Utc::now();
    let end = now - Duration::seconds(5);
    let start = end - Duration::hours(mandate.inputs.window_h);
    EvidenceResponse {
        deployment_id: "fixture-subgraph".into(),
        block_start: 19_000_000,
        block_end: 19_000_600,
        block_end_timestamp: end,
        indexing_errors: false,
        window_requested: WindowBounds {
            start,
            end,
        },
        window_covered: WindowBounds {
            start,
            end,
        },
        truncated: false,
        pools,
    }
}

/// Screening facts for one pool. Pool 0 is material (+10% token0, +8% token1,
/// one large event); the rest move by less than the 5% materiality.
fn screening_pool(pool: &str, material: bool, _with_events: bool) -> PoolEvidence {
    if material {
        PoolEvidence {
            address: pool.to_string(),
            tvl_start: TokenTvl {
                token0: 10_000_000.0,
                token1: 8_000_000.0,
                usd: 18_000_000.0,
            },
            tvl_end: TokenTvl {
                token0: 11_000_000.0,
                token1: 8_640_000.0,
                usd: 19_640_000.0,
            },
            events: Vec::new(),
            mint_count: 2,
            burn_count: 1,
            swap_count: 2,
            mint_amount_usd: 62_000.0,
            burn_amount_usd: 12_500.0,
            swap_amount_usd: 75_000.0,
        }
    } else {
        PoolEvidence {
            address: pool.to_string(),
            tvl_start: TokenTvl {
                token0: 5_000_000.0,
                token1: 4_000_000.0,
                usd: 9_000_000.0,
            },
            tvl_end: TokenTvl {
                token0: 5_003_000.0,
                token1: 4_002_400.0,
                usd: 9_005_400.0,
            },
            events: Vec::new(),
            mint_count: 1,
            burn_count: 0,
            swap_count: 1,
            mint_amount_usd: 1_200.0,
            burn_amount_usd: 0.0,
            swap_amount_usd: 800.0,
        }
    }
}

fn material_events(pool: &str) -> Vec<EventFact> {
    let base = Utc::now() - Duration::hours(12);
    let tx = |i: usize| format!("0x{:064x}", i + 1);
    [
        (50_000.0, 0.0, 1.0),
        (25_000.0, 1.0, 0.0),
        (12_000.0, 0.0, 1.0),
        (8_000.0, 1.0, 0.0),
        (5_000.0, 0.0, 1.0),
    ]
    .iter()
    .enumerate()
    .map(|(i, (usd, a0, a1))| EventFact {
        transaction_id: tx(i),
        log_index: i as u64,
        timestamp: base + Duration::hours(i as i64),
        amount0: a0 * 12_000.0,
        amount1: a1 * 40_000.0,
        amount_usd: *usd,
        origin: format!("0x{:040x}", i + 1),
        owner: format!("0x{:040x}", i + 100),
        tick_lower: 100 + i as u64,
        tick_upper: 300 + i as u64,
        fact_id: format!("evt:{}:{}", pool, i),
    })
    .collect()
}

// ---- analysis (spec section 8) ----

/// Screening facts for all pools, as a seller would return them.
pub fn screening_evidence(mandate: &Mandate) -> EvidenceResponse {
    let pools = mandate
        .inputs
        .pools
        .iter()
        .enumerate()
        .map(|(i, pool)| screening_pool(pool, i == 0, false))
        .collect();
    evidence_response(mandate, pools)
}

/// Event facts for the material pool, as the events seller would return them.
pub fn events_evidence(mandate: &Mandate) -> EvidenceResponse {
    let pool = mandate.inputs.pools[0].clone();
    let mut p = screening_pool(&pool, true, false);
    p.events = material_events(&pool);
    evidence_response(mandate, vec![p])
}

pub fn outcomes_from(mandate: &Mandate, evidence: &[EvidenceResponse]) -> Vec<PoolOutcome> {
    let merged = merge_evidence(evidence);
    mandate
        .inputs
        .pools
        .iter()
        .map(|pool| {
            let facts: Option<&PoolEvidence> = merged.iter().find(|p| &p.address == pool);
            let Some(p) = facts else {
                return PoolOutcome::Undetermined;
            };
            if p.mint_count == 0 && p.swap_count == 0 && p.burn_count == 0 && p.tvl_start.token0 == 0.0 {
                return PoolOutcome::Undetermined;
            }
            let material = is_material(p, mandate);
            if !material {
                return PoolOutcome::NonMaterial;
            }
            match mandate.requirements.evidence {
                EvidenceRequirement::Screening => PoolOutcome::Supported,
                EvidenceRequirement::Transaction => {
                    if p.events.is_empty() {
                        PoolOutcome::Pending
                    } else {
                        PoolOutcome::Supported
                    }
                }
            }
        })
        .collect()
}

pub fn is_material(p: &PoolEvidence, mandate: &Mandate) -> bool {
    let tok0 = tvl_change(p.tvl_start.token0, p.tvl_end.token0);
    let tok1 = tvl_change(p.tvl_start.token1, p.tvl_end.token1);
    tok0.abs() >= mandate.inputs.materiality
        || tok1.abs() >= mandate.inputs.materiality
        || p.events
            .iter()
            .any(|e| e.amount_usd >= mandate.inputs.min_event_usd)
}

fn tvl_change(start: f64, end: f64) -> f64 {
    if start == 0.0 {
        if end == 0.0 {
            0.0
        } else {
            f64::INFINITY
        }
    } else {
        (end - start) / start
    }
}

/// Compute the mandatory claims for every pool (spec section 8). For a
/// supported pool: one tvl_change per token (at most two), one large_event,
/// one activity_summary.
pub fn compute_claims(mandate: &Mandate, evidence: &[EvidenceResponse]) -> Vec<Claim> {
    let merged = merge_evidence(evidence);
    let mut claims = Vec::new();
    for pool in &mandate.inputs.pools {
        let Some(p) = merged.iter().find(|p| &p.address == pool) else {
            continue;
        };
        if !is_material(p, mandate) {
            continue;
        }
        // tvl_change per token: at most two claims, each with the two
        // block-height fact ids (start and end) as evidence.
        let tokens = [
            ("token0", p.tvl_start.token0, p.tvl_end.token0),
            ("token1", p.tvl_start.token1, p.tvl_end.token1),
        ];
        for (token, start, end) in tokens {
            let change = tvl_change(start, end);
            if change.is_finite() {
                claims.push(Claim {
                    claim_type: ClaimType::TvlChange,
                    pool: pool.clone(),
                    values: vec![format!("{:.4}", change)],
                    calculation: format!(
                        "({end} - {start}) / {start} = {change:.4}"
                    ),
                    evidence: vec![
                        EvidenceRef {
                            kind: EvidenceKind::ScreeningFact,
                            fact_id: format!("screen:{}:{}:start", pool, token),
                        },
                        EvidenceRef {
                            kind: EvidenceKind::ScreeningFact,
                            fact_id: format!("screen:{}:{}:end", pool, token),
                        },
                    ],
                });
            }
        }
        // large_event: the largest event at or above min_event_usd
        if let Some(top) = p
            .events
            .iter()
            .max_by(|a, b| a.amount_usd.partial_cmp(&b.amount_usd).unwrap())
        {
            if top.amount_usd >= mandate.inputs.min_event_usd {
                claims.push(Claim {
                    claim_type: ClaimType::LargeEvent,
                    pool: pool.clone(),
                    values: vec![format!("{:.2}", top.amount_usd)],
                    calculation: format!(
                        "event {} amountUSD {:.2} >= min_event_usd {}",
                        top.transaction_id, top.amount_usd, mandate.inputs.min_event_usd
                    ),
                    evidence: vec![EvidenceRef {
                        kind: EvidenceKind::EventFact,
                        fact_id: top.fact_id.clone(),
                    }],
                });
            }
        }
        // activity_summary
        claims.push(Claim {
            claim_type: ClaimType::ActivitySummary,
            pool: pool.clone(),
            values: vec![
                format!("{}", p.mint_count),
                format!("{}", p.burn_count),
                format!("{}", p.swap_count),
                format!("{:.2}", p.mint_amount_usd),
                format!("{:.2}", p.burn_amount_usd),
                format!("{:.2}", p.swap_amount_usd),
            ],
            calculation: format!(
                "counts mints {} burns {} swaps {}; sums {:.2} {:.2} {:.2}",
                p.mint_count, p.burn_count, p.swap_count, p.mint_amount_usd, p.burn_amount_usd, p.swap_amount_usd
            ),
            evidence: vec![EvidenceRef {
                kind: EvidenceKind::CountFact,
                fact_id: format!("count:{}", pool),
            }],
        });
    }
    claims
}

fn outcome_rows(mandate: &Mandate, outcomes: &[PoolOutcome], claims: &[Claim]) -> Vec<OutcomeRow> {
    mandate
        .inputs
        .pools
        .iter()
        .enumerate()
        .map(|(i, pool)| OutcomeRow {
            pool: pool.clone(),
            outcome: match outcomes[i] {
                PoolOutcome::NonMaterial => "non_material".into(),
                PoolOutcome::Pending => "pending".into(),
                PoolOutcome::Supported => "supported".into(),
                PoolOutcome::Undetermined => "undetermined".into(),
            },
            claim_count: claims.iter().filter(|c| c.pool == *pool).count(),
        })
        .collect()
}

pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut d = Sha256::new();
    d.update(data);
    hex::encode(d.finalize())
}

/// Deterministic v4-shaped uuid for fixture payment ids.
fn uuid4(seed: &str) -> String {
    let h = sha256_hex(seed.as_bytes());
    let mut s: Vec<char> = h.chars().take(32).collect();
    s[12] = '4';
    s[16] = '8';
    let s: String = s.into_iter().collect();
    format!(
        "{}-{}-{}-{}-{}",
        &s[0..8],
        &s[8..12],
        &s[12..16],
        &s[16..20],
        &s[20..32]
    )
}

pub use crate::types::fmt_amount;