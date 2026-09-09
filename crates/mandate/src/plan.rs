//! Spec section 5: the three named plans priced over `W`, feasibility, and
//! the choice. `expected` uses live quotes where they exist and ceilings at
//! expected quantities otherwise; `bound` uses ceilings at `|W|`. Explain is
//! priced at its listing's maximum, 8 KB, in both numbers; the mandatory
//! brief bound decides only feasibility, as `brief_too_large`. Re-planning
//! after a delivery calls the same function over the new `W`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::analysis::mandatory_brief_bound;
use crate::mandate::format_amount;
use crate::manifest::{Capability, KB, Listing, Manifest, TariffError, WINDOW_SECONDS};
use crate::refusal::{Code, Refusal};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanKind {
    Staged,
    Hybrid,
    Bundle,
}

impl PlanKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Hybrid => "hybrid",
            Self::Bundle => "bundle",
        }
    }

    pub const ALL: [PlanKind; 3] = [PlanKind::Staged, PlanKind::Hybrid, PlanKind::Bundle];
}

/// One step of a plan at one quantity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    pub listing_id: String,
    pub capability: Capability,
    pub units: u64,
    pub amount: i64,
    pub source: Source,
}

/// Where a step's amount came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// A live quote covered exactly these units.
    Quote,
    /// The tariff ceiling at these units.
    Ceiling,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Quote => "quote",
            Self::Ceiling => "ceiling",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub kind: PlanKind,
    pub expected: i64,
    pub bound: i64,
    pub expected_steps: Vec<Step>,
    pub bound_steps: Vec<Step>,
    pub authorizations: u32,
    pub feasible: bool,
    pub reasons: Vec<String>,
}

/// Everything planning reads. Amounts are atomic units.
#[derive(Debug, Clone)]
pub struct Situation<'a> {
    pub manifest: &'a Manifest,
    /// `|R|`, fixed at start.
    pub required: u64,
    /// Pools not yet screened: `|R|` before the screen, 0 after.
    pub unscreened: u64,
    /// `|W|`.
    pub pending: u64,
    /// `inputs.expected_material_pools`, the planning assumption.
    pub expected_material: u64,
    pub window_seconds: u64,
    pub free: i64,
    pub max_single_payment: i64,
    pub decimals: u32,
    /// Live quotes by listing id and unit count.
    pub quotes: &'a BTreeMap<(String, u64), i64>,
    /// The brief's size in whole KB, once it exists. Until then the explain
    /// step is estimated at its listing's maximum, section 5.
    pub brief_kb: Option<u64>,
    /// Listings whose quote was refused or unreachable, with the reason; no plan may use them.
    pub unusable: &'a BTreeMap<String, String>,
}

fn listing_for(manifest: &Manifest, capability: Capability) -> Option<&Listing> {
    manifest.with_capability(capability).next()
}

fn windows(window_seconds: u64) -> u64 {
    window_seconds.div_ceil(WINDOW_SECONDS).max(1)
}

fn step(sit: &Situation<'_>, listing: &Listing, units: u64) -> Result<Step, String> {
    if let Some(reason) = sit.unusable.get(&listing.id) {
        return Err(format!("{}: {reason}", listing.id));
    }
    let amount = match sit.quotes.get(&(listing.id.clone(), units)) {
        Some(q) => {
            return Ok(Step {
                listing_id: listing.id.clone(),
                capability: listing.capability,
                units,
                amount: *q,
                source: Source::Quote,
            });
        }
        None => listing
            .ceiling(units)
            .map_err(|e: TariffError| format!("{}: {e}", listing.id))?,
    };
    Ok(Step {
        listing_id: listing.id.clone(),
        capability: listing.capability,
        units,
        amount,
        source: Source::Ceiling,
    })
}

fn units_for(sit: &Situation<'_>, listing: &Listing, pools: u64, window_seconds: u64) -> u64 {
    match listing.tariff.unit {
        crate::manifest::Unit::Pool => pools,
        crate::manifest::Unit::PoolWindow => pools * windows(window_seconds),
        // The brief decides the explain quantity as soon as it exists; before
        // that the maximum is the only honest estimate.
        crate::manifest::Unit::InputKb => sit.brief_kb.map_or(listing.tariff.max_units, |kb| {
            kb.clamp(1, listing.tariff.max_units)
        }),
    }
}

/// Prices one plan over the situation. Missing listings make it infeasible.
pub fn price(kind: PlanKind, sit: &Situation<'_>) -> Plan {
    let mut reasons: Vec<String> = Vec::new();
    let mut expected_steps = Vec::new();
    let mut bound_steps = Vec::new();
    let push = |cap: Capability,
                expected_pools: u64,
                bound_pools: u64,
                reasons: &mut Vec<String>,
                expected_steps: &mut Vec<Step>,
                bound_steps: &mut Vec<Step>| {
        let Some(listing) = listing_for(sit.manifest, cap) else {
            reasons.push(format!("no {cap:?} listing").to_lowercase());
            return;
        };
        for (pools, into) in [
            (expected_pools, &mut *expected_steps),
            (bound_pools, &mut *bound_steps),
        ] {
            let units = units_for(sit, listing, pools, sit.window_seconds);
            match step(sit, listing, units) {
                Ok(s) => into.push(s),
                Err(e) => reasons.push(e),
            }
        }
    };
    // The assumption prices events and investigate only while nothing has
    // been screened; once the screen has delivered, `|W|` is observed.
    let expected_w = if sit.unscreened > 0 {
        sit.expected_material
            .min(sit.pending)
            .max(u64::from(sit.pending > 0))
    } else {
        sit.pending
    };
    match kind {
        PlanKind::Staged => {
            if sit.unscreened > 0 {
                push(
                    Capability::Screen,
                    sit.unscreened,
                    sit.unscreened,
                    &mut reasons,
                    &mut expected_steps,
                    &mut bound_steps,
                );
            }
            if sit.pending > 0 {
                push(
                    Capability::Events,
                    expected_w,
                    sit.pending,
                    &mut reasons,
                    &mut expected_steps,
                    &mut bound_steps,
                );
            }
            push(
                Capability::Explain,
                0,
                0,
                &mut reasons,
                &mut expected_steps,
                &mut bound_steps,
            );
            if let Some(explain) = listing_for(sit.manifest, Capability::Explain) {
                let brief_kb = (mandatory_brief_bound(sit.required as usize) as u64).div_ceil(KB);
                if brief_kb > explain.tariff.max_units {
                    reasons.push(format!(
                        "brief_too_large: {brief_kb} KB for {} pools, explain takes {}",
                        sit.required, explain.tariff.max_units
                    ));
                }
            }
        }
        PlanKind::Hybrid => {
            if sit.unscreened > 0 {
                push(
                    Capability::Screen,
                    sit.unscreened,
                    sit.unscreened,
                    &mut reasons,
                    &mut expected_steps,
                    &mut bound_steps,
                );
            }
            if sit.pending > 0 {
                push(
                    Capability::Investigate,
                    expected_w,
                    sit.pending,
                    &mut reasons,
                    &mut expected_steps,
                    &mut bound_steps,
                );
            }
        }
        PlanKind::Bundle => {
            if sit.pending > 0 {
                push(
                    Capability::Investigate,
                    sit.pending,
                    sit.pending,
                    &mut reasons,
                    &mut expected_steps,
                    &mut bound_steps,
                );
            }
        }
    }
    let expected: i64 = expected_steps.iter().map(|s| s.amount).sum();
    let bound: i64 = bound_steps.iter().map(|s| s.amount).sum();
    if bound > sit.free {
        reasons.push(format!(
            "bound {} above available {}",
            format_amount(bound, sit.decimals),
            format_amount(sit.free, sit.decimals)
        ));
    }
    for s in &bound_steps {
        if s.amount > sit.max_single_payment {
            reasons.push(format!(
                "{} at {} units is {}, above max_single_payment {}",
                s.listing_id,
                s.units,
                format_amount(s.amount, sit.decimals),
                format_amount(sit.max_single_payment, sit.decimals)
            ));
        }
    }
    if expected_steps.is_empty() {
        reasons.push("no pending work".to_owned());
    }
    // One step is priced twice, at expected and at bound quantities; a
    // listing that cannot be used says so once.
    reasons.dedup();
    Plan {
        kind,
        expected,
        bound,
        authorizations: expected_steps.len() as u32,
        feasible: reasons.is_empty(),
        expected_steps,
        bound_steps,
        reasons,
    }
}

/// All three plans, priced.
pub fn price_all(sit: &Situation<'_>) -> Vec<Plan> {
    PlanKind::ALL.iter().map(|k| price(*k, sit)).collect()
}

/// The feasible plan with the lowest `expected`, ties to fewer authorizations,
/// or the REQUIREMENT_UNMEETABLE refusal with the numbers section 5 names.
pub fn choose<'p>(plans: &'p [Plan], sit: &Situation<'_>) -> Result<&'p Plan, Refusal> {
    let mut feasible: Vec<&Plan> = plans.iter().filter(|p| p.feasible).collect();
    feasible.sort_by_key(|p| (p.expected, p.authorizations));
    if let Some(p) = feasible.first() {
        return Ok(p);
    }
    let priced: Vec<&Plan> = plans
        .iter()
        .filter(|p| !p.expected_steps.is_empty())
        .collect();
    let lowest_bound = priced.iter().map(|p| p.bound).min().unwrap_or(0);
    let lowest_expected = priced.iter().map(|p| p.expected).min().unwrap_or(0);
    let per_plan: Vec<String> = plans
        .iter()
        .map(|p| format!("{}: {}", p.kind.as_str(), p.reasons.join("; ")))
        .collect();
    Err(Refusal {
        code: Code::RequirementUnmeetable,
        detail: format!(
            "bound {} expected {} available {}; {}",
            format_amount(lowest_bound, sit.decimals),
            format_amount(lowest_expected, sit.decimals),
            format_amount(sit.free, sit.decimals),
            per_plan.join(" | ")
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The concept doc's four listings in USDC atomic units.
    pub const MANIFEST: &str = r#"{"version":"2026-09-07","listings":[
      {"id":"screen","seller":"s","url":"http://127.0.0.1:4021/screen","method":"post","capability":"screen","produces":"screening","tariff":{"version":"2026-09-07","base":0,"unit":"pool","unit_price":200,"max_units":20},"network":"hedera:testnet","asset":"0.0.429274","pay_to":"0.0.10409989"},
      {"id":"events","seller":"s","url":"http://127.0.0.1:4021/events","method":"post","capability":"events","produces":"transaction","tariff":{"version":"2026-09-07","base":0,"unit":"pool_window","unit_price":1500,"max_units":20},"network":"hedera:testnet","asset":"0.0.429274","pay_to":"0.0.10409989"},
      {"id":"investigate","seller":"s","url":"http://127.0.0.1:4021/investigate","method":"post","capability":"investigate","produces":"report","tariff":{"version":"2026-09-07","base":2000,"unit":"pool","unit_price":1200,"max_units":20},"network":"hedera:testnet","asset":"0.0.429274","pay_to":"0.0.10409989"},
      {"id":"explain","seller":"s","url":"http://127.0.0.1:4021/explain","method":"post","capability":"explain","produces":"report","tariff":{"version":"2026-09-07","base":0,"unit":"input_kb","unit_price":100,"max_units":8},"network":"hedera:testnet","asset":"0.0.429274","pay_to":"0.0.10409989"}
    ]}"#;

    static NONE: std::sync::LazyLock<BTreeMap<String, String>> =
        std::sync::LazyLock::new(BTreeMap::new);

    fn situation<'a>(
        manifest: &'a Manifest,
        quotes: &'a BTreeMap<(String, u64), i64>,
        pools: u64,
        free: i64,
    ) -> Situation<'a> {
        Situation {
            unusable: &NONE,
            brief_kb: None,
            manifest,
            required: pools,
            unscreened: pools,
            pending: pools,
            expected_material: 1,
            window_seconds: 86_400,
            free,
            max_single_payment: 9_000,
            decimals: 6,
            quotes,
        }
    }

    fn by_kind(plans: &[Plan], kind: PlanKind) -> &Plan {
        plans.iter().find(|p| p.kind == kind).unwrap()
    }

    #[test]
    fn the_demo_numbers_for_five_pools() {
        let m = Manifest::from_json(MANIFEST).unwrap();
        let quotes = BTreeMap::new();
        let sit = situation(&m, &quotes, 5, 10_000);
        let plans = price_all(&sit);
        let staged = by_kind(&plans, PlanKind::Staged);
        assert_eq!(
            (
                staged.expected,
                staged.bound,
                staged.authorizations,
                staged.feasible
            ),
            (3_300, 9_300, 3, true)
        );
        let hybrid = by_kind(&plans, PlanKind::Hybrid);
        assert_eq!(
            (hybrid.expected, hybrid.bound, hybrid.authorizations),
            (4_200, 9_000, 2)
        );
        let bundle = by_kind(&plans, PlanKind::Bundle);
        assert_eq!(
            (bundle.expected, bundle.bound, bundle.authorizations),
            (8_000, 8_000, 1)
        );
        assert_eq!(choose(&plans, &sit).unwrap().kind, PlanKind::Staged);
    }

    #[test]
    fn one_pool_picks_staged_and_a_cheap_live_bundle_flips_it() {
        let m = Manifest::from_json(MANIFEST).unwrap();
        let quotes = BTreeMap::new();
        let sit = situation(&m, &quotes, 1, 10_000);
        let plans = price_all(&sit);
        assert_eq!(by_kind(&plans, PlanKind::Staged).expected, 2_500);
        assert_eq!(by_kind(&plans, PlanKind::Bundle).expected, 3_200);
        assert_eq!(choose(&plans, &sit).unwrap().kind, PlanKind::Staged);

        let mut live = BTreeMap::new();
        live.insert(("investigate".to_owned(), 5), 3_000);
        let sit = situation(&m, &live, 5, 10_000);
        let plans = price_all(&sit);
        let bundle = by_kind(&plans, PlanKind::Bundle);
        assert_eq!(
            (
                bundle.expected,
                bundle.bound,
                bundle.expected_steps[0].source
            ),
            (3_000, 3_000, Source::Quote)
        );
        assert_eq!(
            choose(&plans, &sit).unwrap().kind,
            PlanKind::Bundle,
            "scenario 2"
        );
    }

    #[test]
    fn scenario_3_refuses_before_any_purchase_with_the_numbers() {
        let m = Manifest::from_json(MANIFEST).unwrap();
        let quotes = BTreeMap::new();
        let sit = situation(&m, &quotes, 5, 3_000);
        let plans = price_all(&sit);
        assert!(plans.iter().all(|p| !p.feasible));
        let r = choose(&plans, &sit).unwrap_err();
        assert_eq!(r.code, Code::RequirementUnmeetable);
        assert!(
            r.detail
                .starts_with("bound 0.008000 expected 0.003300 available 0.003000"),
            "{}",
            r.detail
        );
    }

    #[test]
    fn after_the_screen_the_choice_is_events_plus_explain_against_investigate() {
        let m = Manifest::from_json(MANIFEST).unwrap();
        let quotes = BTreeMap::new();
        let mut sit = situation(&m, &quotes, 5, 9_000);
        sit.unscreened = 0;
        sit.pending = 1;
        let plans = price_all(&sit);
        let staged = by_kind(&plans, PlanKind::Staged);
        assert_eq!(
            staged
                .expected_steps
                .iter()
                .map(|s| s.listing_id.as_str())
                .collect::<Vec<_>>(),
            vec!["events", "explain"]
        );
        assert_eq!(staged.expected, 2_300);
        let hybrid = by_kind(&plans, PlanKind::Hybrid);
        assert_eq!(hybrid.expected, 3_200);
        assert_eq!(choose(&plans, &sit).unwrap().kind, PlanKind::Staged);
        let mut live = BTreeMap::new();
        live.insert(("events".to_owned(), 1), 2_000);
        let sit2 = Situation {
            quotes: &live,
            ..sit.clone()
        };
        let plans = price_all(&sit2);
        assert_eq!(
            by_kind(&plans, PlanKind::Staged).expected,
            2_800,
            "live events quote replaces the ceiling"
        );
    }

    #[test]
    fn observed_work_replaces_the_assumption_after_the_screen() {
        let m = Manifest::from_json(MANIFEST).unwrap();
        let quotes = BTreeMap::new();
        // Scenario 5: four pending pools after the screen.
        let mut sit = situation(&m, &quotes, 5, 10_000);
        sit.unscreened = 0;
        sit.pending = 4;
        let plans = price_all(&sit);
        let staged = by_kind(&plans, PlanKind::Staged);
        assert_eq!(
            (staged.expected, staged.bound, staged.authorizations),
            (6_800, 6_800, 2)
        );
        let hybrid = by_kind(&plans, PlanKind::Hybrid);
        assert_eq!((hybrid.expected, hybrid.authorizations), (6_800, 1));
        assert_eq!(
            choose(&plans, &sit).unwrap().kind,
            PlanKind::Hybrid,
            "ties go to fewer authorizations"
        );
        // Five pending pools.
        sit.pending = 5;
        let plans = price_all(&sit);
        assert_eq!(by_kind(&plans, PlanKind::Staged).expected, 8_300);
        assert_eq!(by_kind(&plans, PlanKind::Hybrid).expected, 8_000);
        assert_eq!(choose(&plans, &sit).unwrap().kind, PlanKind::Hybrid);
        // Before the screen the assumption still applies.
        let before = situation(&m, &quotes, 5, 10_000);
        assert_eq!(
            by_kind(&price_all(&before), PlanKind::Staged).expected,
            3_300
        );
    }

    #[test]
    fn the_explain_step_prices_at_the_brief_size_once_it_exists() {
        let m = Manifest::from_json(MANIFEST).unwrap();
        // A live explain quote for a 3 KB brief, the size the run measured.
        let mut quotes = BTreeMap::new();
        quotes.insert(("explain".to_owned(), 3), 300);
        let mut sit = situation(&m, &quotes, 5, 10_000);
        sit.unscreened = 0;
        sit.pending = 0;
        // Before the brief exists the maximum is the only honest estimate.
        let plans = price_all(&sit);
        let staged = by_kind(&plans, PlanKind::Staged);
        assert_eq!((staged.expected, staged.expected_steps[0].units), (800, 8));
        assert_eq!(staged.expected_steps[0].source, Source::Ceiling);
        // With the brief measured, the live quote at that size is used.
        sit.brief_kb = Some(3);
        let plans = price_all(&sit);
        let staged = by_kind(&plans, PlanKind::Staged);
        assert_eq!((staged.expected, staged.bound), (300, 300));
        assert_eq!(staged.expected_steps[0].units, 3);
        assert_eq!(staged.expected_steps[0].source, Source::Quote);
        // Without a quote it is the ceiling at that size, not at the maximum.
        let none = BTreeMap::new();
        let mut sit = situation(&m, &none, 5, 10_000);
        sit.unscreened = 0;
        sit.pending = 0;
        sit.brief_kb = Some(3);
        let plans = price_all(&sit);
        let staged = by_kind(&plans, PlanKind::Staged);
        assert_eq!(
            (staged.expected, staged.expected_steps[0].source),
            (300, Source::Ceiling)
        );
        // A brief larger than the listing takes is still capped there, so the
        // feasibility check, not the price, refuses it.
        sit.brief_kb = Some(99);
        let plans = price_all(&sit);
        let staged = by_kind(&plans, PlanKind::Staged);
        assert_eq!(staged.expected_steps[0].units, 8);
    }

    #[test]
    fn brief_too_large_rejects_staged_only() {
        let m = Manifest::from_json(MANIFEST).unwrap();
        let quotes = BTreeMap::new();
        let mut sit = situation(&m, &quotes, 8, 100_000);
        sit.max_single_payment = 20_000;
        let plans = price_all(&sit);
        let staged = by_kind(&plans, PlanKind::Staged);
        assert!(!staged.feasible);
        assert!(
            staged
                .reasons
                .iter()
                .any(|r| r.starts_with("brief_too_large: 9 KB")),
            "{:?}",
            staged.reasons
        );
        assert!(by_kind(&plans, PlanKind::Bundle).feasible);
    }

    #[test]
    fn an_unusable_listing_removes_the_plans_that_need_it() {
        let m = Manifest::from_json(MANIFEST).unwrap();
        let quotes = BTreeMap::new();
        let mut unusable = BTreeMap::new();
        unusable.insert(
            "events".to_owned(),
            "REFUSED OFF_TARIFF ceiling 0.001500 quoted 0.002000".to_owned(),
        );
        let mut sit = situation(&m, &quotes, 5, 9_000);
        sit.unusable = &unusable;
        sit.unscreened = 0;
        sit.pending = 1;
        let plans = price_all(&sit);
        let staged = by_kind(&plans, PlanKind::Staged);
        assert!(!staged.feasible);
        assert!(
            staged.reasons[0].starts_with("events: REFUSED OFF_TARIFF"),
            "{:?}",
            staged.reasons
        );
        assert_eq!(
            choose(&plans, &sit).unwrap().kind,
            PlanKind::Hybrid,
            "scenario 4"
        );
    }

    #[test]
    fn a_step_above_the_payment_cap_is_named() {
        let m = Manifest::from_json(MANIFEST).unwrap();
        let quotes = BTreeMap::new();
        let mut sit = situation(&m, &quotes, 5, 100_000);
        sit.max_single_payment = 7_600;
        let plans = price_all(&sit);
        let bundle = by_kind(&plans, PlanKind::Bundle);
        assert!(!bundle.feasible);
        assert!(
            bundle.reasons[0].contains("above max_single_payment 0.007600"),
            "{:?}",
            bundle.reasons
        );
        assert!(by_kind(&plans, PlanKind::Staged).feasible);
    }
}
