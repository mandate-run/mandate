use crate::types::{Capability, Listing, Mandate, Quote, Reservation, ReservationSource, ReservationState};

/// A computed plan step: which listing, at how many units for the expected and
/// worst-case quantities, and what each costs.
#[derive(Debug, Clone)]
pub struct PlanStep {
    pub capability: Capability,
    pub listing_id: String,
    pub units_expected: i128,
    pub units_bound: i128,
    pub expected: i128,
    pub bound: i128,
    pub source_expected: CostSource,
    pub source_bound: CostSource,
}

impl PlanStep {
    pub fn capability_label(&self) -> &'static str {
        match self.capability {
            Capability::Screen => "screen",
            Capability::Events => "events",
            Capability::Investigate => "investigate",
            Capability::Explain => "explain",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostSource {
    LiveQuote,
    Ceiling,
    OffTariff,
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub kind: PlanKind,
    pub expected: i128,
    pub bound: i128,
    pub authorizations: usize,
    pub steps: Vec<PlanStep>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanKind {
    Staged,
    Hybrid,
    Bundle,
}

impl PlanKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            PlanKind::Staged => "staged",
            PlanKind::Hybrid => "hybrid",
            PlanKind::Bundle => "bundle",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PlanRejection {
    pub name: &'static str,
    pub reason: String,
    pub bound: i128,
    pub expected: i128,
}

/// One planning decision, recorded for the transcript (spec section 12).
#[derive(Debug, Clone)]
pub struct PlanSelection {
    pub chosen: Option<Plan>,
    pub rejected: Vec<PlanRejection>,
    /// Reservation for the final step of the chosen plan, priced at the
    /// ceiling at maximum quantity (spec I4), when `reserve_completion`.
    pub reservations: Vec<Reservation>,
}

/// Builds the staged, hybrid and bundle plans (spec section 5) and chooses the
/// feasible plan with the lowest expected cost.
///
/// Quantities: `unscreened` pools still need screening; `pending` (`|W|`) pools
/// need deep evidence; `required` (`|R|`) is the full coverage set used for the
/// staged plan's mandatory brief bound. Expected quantities use
/// `expected_material_pools` for post-screen steps; bound quantities use `|W|`.
pub struct PlanBuilder<'a> {
    mandate: &'a Mandate,
    listings: &'a [Listing],
    quotes: &'a [Quote],
    unscreened: usize,
    pending: usize,
    required: usize,
    free_budget: i128,
}

impl<'a> PlanBuilder<'a> {
    pub fn new(mandate: &'a Mandate, listings: &'a [Listing], quotes: &'a [Quote]) -> Self {
        let pools = mandate.inputs.pools.len();
        Self {
            mandate,
            listings,
            quotes,
            unscreened: pools,
            pending: pools,
            required: pools,
            free_budget: mandate.budget.service.total,
        }
    }

    /// Reflect post-screening state: `unscreened` pools remain, `pending` is
    /// the current `|W|`, and `free_budget` is what a plan may bind.
    pub fn with_state(mut self, unscreened: usize, pending: usize, free_budget: i128) -> Self {
        self.unscreened = unscreened;
        self.pending = pending;
        self.free_budget = free_budget;
        self
    }

    fn listing_for(&self, capability: Capability) -> Option<&Listing> {
        self.listings.iter().find(|l| l.capability == capability)
    }

    /// Cost of `units` on `listing`: a live quote when one matches the exact
    /// request (same listing and unit count), else the tariff ceiling.
    fn cost(&self, listing: &Listing, units: i128) -> (i128, CostSource) {
        let ceiling = ceiling(listing, units);
        match self
            .quotes
            .iter()
            .find(|q| q.listing_id == listing.id && q.requested_units == units)
        {
            Some(q) if q.within_tariff => (q.amount, CostSource::LiveQuote),
            Some(q) => (q.amount, CostSource::OffTariff),
            None => (ceiling, CostSource::Ceiling),
        }
    }

    fn step(&self, capability: Capability, units_e: i128, units_b: i128) -> Option<PlanStep> {
        let listing = self.listing_for(capability.clone())?;
        let (expected, source_expected) = self.cost(listing, units_e);
        let (bound, source_bound) = self.cost(listing, units_b);
        Some(PlanStep {
            capability,
            listing_id: listing.id.clone(),
            units_expected: units_e,
            units_bound: units_b,
            expected,
            bound,
            source_expected,
            source_bound,
        })
    }

    fn build(&self, kind: PlanKind) -> Option<Plan> {
        let expected_material = self.mandate.inputs.expected_material_pools as i128;
        let unscreened = self.unscreened as i128;
        let pending = self.pending as i128;
        let steps = match kind {
            PlanKind::Staged => {
                // Explain is priced at its listing's max units (the whole brief
                // budget) until a live quote for the built brief exists.
                let explain_max = self
                    .listing_for(Capability::Explain)?
                    .tariff
                    .max_units;
                vec![
                    self.step(Capability::Screen, unscreened, unscreened)?,
                    self.step(Capability::Events, expected_material, pending)?,
                    self.step(Capability::Explain, explain_max, explain_max)?,
                ]
            }
            PlanKind::Hybrid => vec![
                self.step(Capability::Screen, unscreened, unscreened)?,
                self.step(Capability::Investigate, expected_material, pending)?,
            ],
            PlanKind::Bundle => vec![self.step(Capability::Investigate, pending, pending)?],
        };
        let expected = steps.iter().map(|s| s.expected).sum();
        let bound = steps.iter().map(|s| s.bound).sum();
        // A step at zero units (e.g. screening when nothing is unscreened) is
        // not an authorization; ties break on actual purchases.
        let authorizations = steps.iter().filter(|s| s.units_bound > 0).count();
        Some(Plan {
            kind,
            expected,
            bound,
            authorizations,
            steps,
        })
    }

    /// Feasibility per spec section 5: bound within the free budget, no step
    /// above `max_single_payment` at its maximum quantity, every step within
    /// its listing's `max_units`, no off-tariff quote, and for the staged plan
    /// the mandatory brief bound for `|R|` pools fits the explain listing.
    fn feasible(&self, plan: &Plan) -> Option<String> {
        if plan.bound > self.free_budget {
            return Some(format!(
                "bound {} exceeds free budget {}",
                plan.bound, self.free_budget
            ));
        }
        for step in &plan.steps {
            let listing = self.listing_for(step.capability.clone()).unwrap();
            if step.units_bound > listing.tariff.max_units {
                return Some(format!(
                    "{} step exceeds max_units {}",
                    step.capability_label(),
                    listing.tariff.max_units
                ));
            }
            if step.bound > self.mandate.constraints.max_single_payment {
                return Some(format!(
                    "{} step {} exceeds max_single_payment {}",
                    step.capability_label(),
                    step.bound,
                    self.mandate.constraints.max_single_payment
                ));
            }
            if step.source_expected == CostSource::OffTariff
                || step.source_bound == CostSource::OffTariff
            {
                let q = self
                    .quotes
                    .iter()
                    .find(|q| q.listing_id == listing.id)
                    .unwrap();
                return Some(format!(
                    "OFF_TARIFF ceiling {} quoted {}",
                    q.ceiling, q.amount
                ));
            }
        }
        if plan.kind == PlanKind::Staged {
            let brief_bound = crate::brief::mandatory_bound(self.required);
            let kb = brief_bound.div_ceil(1024);
            let explain = self.listing_for(Capability::Explain).unwrap();
            if kb as i128 > explain.tariff.max_units {
                return Some(format!(
                    "brief_too_large {} bytes needs {} KB, explain max {} KB",
                    brief_bound, kb, explain.tariff.max_units
                ));
            }
        }
        None
    }

    pub fn evaluate(&self) -> PlanSelection {
        let mut plans: Vec<Plan> = [PlanKind::Staged, PlanKind::Hybrid, PlanKind::Bundle]
            .iter()
            .filter_map(|k| self.build(*k))
            .collect();

        let mut rejected = Vec::new();
        let mut feasible: Vec<Plan> = Vec::new();
        for plan in plans.drain(..) {
            match self.feasible(&plan) {
                Some(reason) => rejected.push(PlanRejection {
                    name: plan.kind.as_str(),
                    reason,
                    bound: plan.bound,
                    expected: plan.expected,
                }),
                None => feasible.push(plan),
            }
        }

        feasible.sort_by(|a, b| {
            a.expected
                .cmp(&b.expected)
                .then(a.authorizations.cmp(&b.authorizations))
                .then(kind_rank(a.kind).cmp(&kind_rank(b.kind)))
        });

        let chosen = feasible.first().cloned();
        if let Some(best) = &chosen {
            for other in feasible.iter().skip(1) {
                let reason = if other.expected == best.expected
                    && other.authorizations == best.authorizations
                {
                    "tie-break".to_string()
                } else {
                    "higher expected".to_string()
                };
                rejected.push(PlanRejection {
                    name: other.kind.as_str(),
                    reason,
                    bound: other.bound,
                    expected: other.expected,
                });
            }
        }

        let mut reservations = Vec::new();
        if let Some(chosen) = &chosen {
            if self.mandate.budget.reserve_completion {
                if let Some(final_step) = chosen.steps.last() {
                    if let Some(listing) = self.listing_for(final_step.capability.clone()) {
                        let amount = ceiling(listing, final_step.units_bound);
                        reservations.push(Reservation {
                            id: format!("res-final-{}", chosen.kind.as_str()),
                            step: final_step.capability_label().to_string(),
                            amount,
                            source: ReservationSource::CeilingAtMax,
                            state: ReservationState::Held,
                        });
                    }
                }
            }
        }

        PlanSelection {
            chosen,
            rejected,
            reservations,
        }
    }
}

fn kind_rank(kind: PlanKind) -> u8 {
    match kind {
        PlanKind::Staged => 0,
        PlanKind::Hybrid => 1,
        PlanKind::Bundle => 2,
    }
}

/// Tariff ceiling for `n` units: `base + unit_price * n` (spec section 2.2).
/// The tariff is a ceiling; sellers may compete below it.
pub fn ceiling(listing: &Listing, units: i128) -> i128 {
    listing.tariff.base + listing.tariff.unit_price * units
}

/// Unit count for a request body of `bytes` under an `input_kb` tariff: whole
/// KB, rounding up.
pub fn input_kb(bytes: usize) -> i128 {
    bytes.div_ceil(1024) as i128
}