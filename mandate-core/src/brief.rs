use serde::Serialize;

use crate::types::{Claim, EvidenceResponse, Mandate, PoolOutcome};

/// Mandatory brief bound (spec section 8): header at most 512 bytes, each
/// pool's outcome and mandatory claims at most 1024 bytes, for every pool in
/// `|R|`. It is a feasibility condition of the staged plan, not of the mandate.
pub fn mandatory_bound(pool_count: usize) -> usize {
    512 + 1024 * pool_count
}

#[derive(Default)]
pub struct BriefBuilder;

#[derive(Debug, Clone)]
pub struct BuiltBrief {
    pub header_bytes: usize,
    pub pool_rows: usize,
    pub total_bytes: usize,
    pub claims: Vec<Claim>,
    pub supporting_events_per_pool: usize,
}

impl BriefBuilder {
    pub fn new() -> Self {
        Self
    }

    /// Build the bounded brief from the per-pool outcomes, the computed claims
    /// and the purchased evidence. Supporting events per supported pool start
    /// at `brief_events` and are reduced one at a time until the whole brief
    /// fits the mandatory bound; zero events is valid (spec section 8).
    pub fn build_brief(
        &self,
        mandate: &Mandate,
        outcomes: &[PoolOutcome],
        claims: &[Claim],
        evidence: &[EvidenceResponse],
        brief_events: usize,
    ) -> BuiltBrief {
        let pools = &mandate.inputs.pools;
        debug_assert_eq!(pools.len(), outcomes.len());

        let header = Header {
            mandate_id: &mandate.id,
            purpose: &mandate.purpose,
            coverage: "all_material",
            window_h: mandate.inputs.window_h,
            pools: pools.len(),
        };
        let header_bytes = json_bytes(&header);
        let header_bytes = header_bytes.min(512);

        let mut supporting = brief_events;
        let mut rows = Vec::new();
        let mut claims_in_brief = Vec::new();
        loop {
            rows.clear();
            claims_in_brief.clear();
            let mut total = header_bytes;
            for (i, pool) in pools.iter().enumerate() {
                let outcome = &outcomes[i];
                let pool_claims: Vec<&Claim> = claims.iter().filter(|c| c.pool == *pool).collect();
                let events = if *outcome == PoolOutcome::Supported {
                    supporting_events(pool, evidence, supporting)
                } else {
                    Vec::new()
                };
                let facts = pool_facts(pool, evidence);
                let row = PoolRow {
                    pool,
                    outcome: outcome_label(outcome),
                    claims: pool_claims.iter().map(|c| (*c).clone()).collect(),
                    events,
                    facts,
                };
                // A pool row may not exceed 1024 bytes: drop supporting events
                // until it fits, down to zero.
                let mut row_bytes = json_bytes(&row);
                let mut row = row;
                while row_bytes > 1024 && !row.events.is_empty() {
                    row.events.pop();
                    row_bytes = json_bytes(&row);
                }
                total += row_bytes;
                rows.push(row);
                claims_in_brief.extend(pool_claims.into_iter().cloned());
            }
            if total <= mandatory_bound(pools.len()) || supporting == 0 {
                return BuiltBrief {
                    header_bytes,
                    pool_rows: rows.len(),
                    total_bytes: total,
                    claims: claims_in_brief,
                    supporting_events_per_pool: supporting,
                };
            }
            supporting -= 1;
        }
    }
}

fn supporting_events(
    pool: &str,
    evidence: &[EvidenceResponse],
    limit: usize,
) -> Vec<crate::types::EventFact> {
    let mut events: Vec<crate::types::EventFact> = evidence
        .iter()
        .flat_map(|r| r.pools.iter())
        .filter(|p| p.address == pool)
        .flat_map(|p| p.events.iter().cloned())
        .collect();
    events.sort_by(|a, b| {
        b.amount_usd
            .partial_cmp(&a.amount_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    events.truncate(limit);
    events
}

fn outcome_label(outcome: &PoolOutcome) -> &'static str {
    match outcome {
        PoolOutcome::NonMaterial => "non_material",
        PoolOutcome::Pending => "pending",
        PoolOutcome::Supported => "supported",
        PoolOutcome::Undetermined => "undetermined",
    }
}

#[derive(Serialize)]
struct Header<'a> {
    mandate_id: &'a str,
    purpose: &'a str,
    coverage: &'a str,
    window_h: i64,
    pools: usize,
}

#[derive(Serialize)]
struct PoolRow<'a> {
    pool: &'a str,
    outcome: &'a str,
    claims: Vec<Claim>,
    events: Vec<crate::types::EventFact>,
    /// The screening facts that justify the verdict, so the explain step can
    /// describe why a pool was or was not material without buying again.
    facts: Option<PoolFacts>,
}

#[derive(Serialize)]
struct PoolFacts {
    tvl_start_usd: f64,
    tvl_end_usd: f64,
    token0_change: f64,
    token1_change: f64,
    mints: u64,
    burns: u64,
    swaps: u64,
}

fn pool_facts(pool: &str, evidence: &[EvidenceResponse]) -> Option<PoolFacts> {
    let p = evidence
        .iter()
        .flat_map(|r| r.pools.iter())
        .find(|p| p.address == pool)?;
    Some(PoolFacts {
        tvl_start_usd: p.tvl_start.usd,
        tvl_end_usd: p.tvl_end.usd,
        token0_change: change(p.tvl_start.token0, p.tvl_end.token0),
        token1_change: change(p.tvl_start.token1, p.tvl_end.token1),
        mints: p.mint_count,
        burns: p.burn_count,
        swaps: p.swap_count,
    })
}

fn change(start: f64, end: f64) -> f64 {
    if start == 0.0 {
        end
    } else {
        (end - start) / start
    }
}

fn json_bytes<T: Serialize>(value: &T) -> usize {
    serde_json::to_vec(value).map(|v| v.len()).unwrap_or(0)
}

pub fn summarize_transcript(_transcript: &crate::types::Transcript) -> String {
    // Placeholder: full transcript summarization implemented in a later step.
    String::new()
}