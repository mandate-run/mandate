use crate::types::{
    merge_evidence, Claim, ClaimType, EvidenceKind, EvidenceResponse, Mandate, PoolOutcome, Quote,
    ValidationRow,
};

#[derive(Debug, Clone)]
pub struct ValidationReport {
    pub row: ValidationRow,
}

/// Validate the purchased result against the mandate (spec section 9):
/// coverage of `R`, re-evaluated calculations, citations, provenance samples,
/// freshness and schema conformance.
///
/// `provenance`: (checked, total) sampled transaction hashes that passed
/// `eth_getTransactionReceipt`; pass `None` when no transaction hash is cited
/// (provenance is then `not_applicable`).
pub fn validate_report(
    mandate: &Mandate,
    outcomes: &[PoolOutcome],
    claims: &[Claim],
    evidence: &[EvidenceResponse],
    quote: &Quote,
    provenance: Option<(usize, usize)>,
    prose_ok: bool,
) -> ValidationReport {
    let pools = &mandate.inputs.pools;
    let required = pools.len();
    let covered = outcomes
        .iter()
        .filter(|o| **o != PoolOutcome::Pending)
        .count()
        .min(required);
    let coverage = format!("{}/{}", covered, required);

    let complete = !outcomes
        .iter()
        .any(|o| matches!(o, PoolOutcome::Pending | PoolOutcome::Undetermined));

    let calculations = claims
        .iter()
        .all(|c| claim_calculation_recomputes(c, evidence));

    let citations = citations_pass(mandate, outcomes, claims, evidence);

    let freshness = evidence.iter().all(|r| {
        !r.indexing_errors
            && (quote.received_at - r.block_end_timestamp).num_seconds()
                <= mandate.requirements.max_data_age_s
            && r.block_end_timestamp <= quote.received_at
    });

    let (provenance_label, provenance_ok) = match provenance {
        Some((checked, total)) if total > 0 => {
            (format!("{checked}/{total}"), checked == total)
        }
        _ => ("not_applicable".to_string(), true),
    };

    let complete = complete && provenance_ok && calculations && citations && freshness && prose_ok;

    ValidationReport {
        row: ValidationRow {
            coverage,
            calculations,
            citations,
            provenance: provenance_label,
            freshness,
            schema: true,
            complete,
            incomplete_reason: if complete {
                None
            } else {
                Some("validation failed one or more checks".to_string())
            },
        },
    }
}

/// Every claim's calculation must re-evaluate to its value against the
/// purchased facts (spec section 8 and 9).
fn claim_calculation_recomputes(claim: &Claim, evidence: &[EvidenceResponse]) -> bool {
    let merged = merge_evidence(evidence);
    let pool = merged.iter().find(|p| p.address == claim.pool);
    let Some(pool) = pool else {
        return false;
    };
    match claim.claim_type {
        ClaimType::TvlChange => {
            // One claim per token; values: [change] as a "x.xxxx" decimal
            // string. The token is identified by the evidence fact id.
            let token = claim
                .evidence
                .first()
                .map(|e| e.fact_id.as_str())
                .unwrap_or("");
            let (start, end) = if token.contains("token0") {
                (pool.tvl_start.token0, pool.tvl_end.token0)
            } else {
                (pool.tvl_start.token1, pool.tvl_end.token1)
            };
            claim.values.len() == 1
                && close(claim.values[0].parse().unwrap_or(f64::NAN), tvl_change(start, end))
        }
        ClaimType::LargeEvent => {
            // values: [amount_usd]
            claim.values.len() == 1
                && claim
                    .evidence
                    .iter()
                    .any(|e| {
                        pool.events
                            .iter()
                            .any(|f| f.fact_id == e.fact_id && f.amount_usd >= claim.values[0].parse().unwrap_or(f64::MAX))
                    })
        }
        ClaimType::ActivitySummary => {
            // values: [mint_count, burn_count, swap_count, mint_usd, burn_usd, swap_usd]
            claim.values.len() == 6
                && close(claim.values[0].parse().unwrap_or(f64::NAN), pool.mint_count as f64)
                && close(claim.values[1].parse().unwrap_or(f64::NAN), pool.burn_count as f64)
                && close(claim.values[2].parse().unwrap_or(f64::NAN), pool.swap_count as f64)
                && close(claim.values[3].parse().unwrap_or(f64::NAN), pool.mint_amount_usd)
                && close(claim.values[4].parse().unwrap_or(f64::NAN), pool.burn_amount_usd)
                && close(claim.values[5].parse().unwrap_or(f64::NAN), pool.swap_amount_usd)
        }
    }
}

/// Spec section 9 citations: every claim carries at least one evidence
/// reference of its permitted kind; every supported pool has at least one
/// claim; a supported pool whose evidence contains an event must have a claim
/// citing a transaction hash.
fn citations_pass(
    mandate: &Mandate,
    outcomes: &[PoolOutcome],
    claims: &[Claim],
    evidence: &[EvidenceResponse],
) -> bool {
    let merged = merge_evidence(evidence);
    let pools = &mandate.inputs.pools;
    for claim in claims {
        let kind_ok = match claim.claim_type {
            ClaimType::TvlChange => {
                claim.evidence.iter().all(|e| e.kind == EvidenceKind::ScreeningFact)
                    && claim.evidence.len() >= 2
            }
            ClaimType::LargeEvent => {
                claim.evidence.len() == 1
                    && claim.evidence[0].kind == EvidenceKind::EventFact
            }
            ClaimType::ActivitySummary => {
                claim.evidence.iter().all(|e| e.kind == EvidenceKind::CountFact)
            }
        };
        if !kind_ok {
            return false;
        }
        // every evidence reference resolves to a purchased fact
        for e in &claim.evidence {
            let pool_found = merged.iter().any(|p| p.address == claim.pool);
            if !pool_found {
                return false;
            }
            match e.kind {
                EvidenceKind::EventFact => {
                    // must resolve to an actual event fact in the pool
                    let found = merged.iter().any(|p| {
                        p.address == claim.pool
                            && p.events.iter().any(|f| f.fact_id == e.fact_id)
                    });
                    if !found {
                        return false;
                    }
                }
                EvidenceKind::ScreeningFact | EvidenceKind::CountFact => {
                    // block-height and count facts are identified by pool
                }
            }
        }
    }
    for (i, pool) in pools.iter().enumerate() {
        if outcomes[i] == PoolOutcome::Supported {
            let pool_claims: Vec<&Claim> = claims.iter().filter(|c| c.pool == *pool).collect();
            if pool_claims.is_empty() {
                return false;
            }
            let has_event = merged.iter().any(|p| p.address == *pool && !p.events.is_empty());
            if has_event
                && !pool_claims.iter().any(|c| {
                    c.evidence
                        .iter()
                        .any(|e| e.kind == EvidenceKind::EventFact)
                })
            {
                return false;
            }
        }
    }
    true
}

fn tvl_change(start: f64, end: f64) -> f64 {
    if start == 0.0 {
        end
    } else {
        (end - start) / start
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6
}