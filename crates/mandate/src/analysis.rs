//! Spec section 8: outcomes, claims and the bounded brief, computed by the
//! runtime from purchased facts, deterministically, before any explanation
//! is bought. The computation mirrors the sellers' `claims.ts` exactly, so a
//! bundled report can be checked for equality. Completeness is decided
//! before materiality: incomplete facts are `undetermined`, never `supported`.
//!
//! Fact ids: `<pool>:<field>@<block>` for a snapshot field,
//! `<pool>:hours@<from>-<to>` for the hourly aggregates, and
//! `<pool>:events@<from>-<to>` for the delivered event lists.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::evidence::{
    Dec, EVENT_KINDS, EventKind, EventsResponse, PoolEvents, PoolScreen, ScreenResponse, Verdict,
};
use crate::mandate::Evidence;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    NonMaterial,
    Pending,
    Supported,
    Undetermined,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NonMaterial => "non_material",
            Self::Pending => "pending",
            Self::Supported => "supported",
            Self::Undetermined => "undetermined",
        }
    }

    /// Still in `W`, section 5.
    pub fn pending_work(self) -> bool {
        matches!(self, Self::Pending | Self::Undetermined)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolOutcome {
    pub pool: String,
    pub outcome: Outcome,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimType {
    TvlChange,
    LargeEvent,
    LargestEvent,
    ActivitySummary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    #[serde(rename = "type")]
    pub kind: ClaimType,
    pub pool: String,
    pub values: BTreeMap<String, Value>,
    pub calculation: String,
    pub evidence: Vec<String>,
}

impl Claim {
    /// The transaction hashes this claim cites, section 9.
    pub fn transaction_hashes(&self) -> Vec<&str> {
        match self.kind {
            ClaimType::LargeEvent | ClaimType::LargestEvent => {
                self.evidence.iter().map(String::as_str).collect()
            }
            _ => Vec::new(),
        }
    }
}

pub fn fact_id(pool: &str, field: &str, at: impl std::fmt::Display) -> String {
    format!("{pool}:{field}@{at}")
}

/// Why a pool's facts cannot support a verdict, section 8's undetermined row.
pub fn incompleteness(
    screen: &ScreenResponse,
    facts: &PoolScreen,
    held: Option<&PoolEvents>,
) -> Vec<String> {
    let mut reasons: Vec<String> = Vec::new();
    let mut push = |r: String| {
        if !reasons.contains(&r) {
            reasons.push(r);
        }
    };
    if screen.header.coverage_shortfall {
        push("coverage_shortfall".to_owned());
    }
    if facts.truncated {
        push("truncated".to_owned());
    }
    if facts.start.is_none() || facts.end.is_none() {
        push("facts_missing".to_owned());
    }
    if facts.unvalued_events.mint {
        push("unvalued_event:mint".to_owned());
    }
    if facts.unvalued_events.burn {
        push("unvalued_event:burn".to_owned());
    }
    for r in &facts.reasons {
        if let Some(rest) = r.strip_prefix("undetermined:") {
            push(rest.to_owned());
        }
    }
    if let Some(h) = held {
        if h.truncated {
            push("events_truncated".to_owned());
        }
        if h.amount_usd_nulls > 0 {
            push("events_unvalued".to_owned());
        }
    }
    reasons
}

/// Outcomes and claims for every pool in the screen. `events` holds the
/// transaction-level evidence bought so far, by pool.
pub fn outcomes_and_claims(
    screen: &ScreenResponse,
    events: Option<&EventsResponse>,
    evidence: Evidence,
    min_event_usd: &str,
) -> (Vec<PoolOutcome>, Vec<Claim>) {
    let mut outcomes = Vec::new();
    let mut claims = Vec::new();
    for (pool, facts) in &screen.pools {
        let held = events.and_then(|e| e.pools.get(pool));
        let incomplete = incompleteness(screen, facts, held);
        let (outcome, reasons) = if !incomplete.is_empty() {
            (Outcome::Undetermined, incomplete)
        } else if facts.verdict == Verdict::Material {
            let o = if evidence == Evidence::Screening || held.is_some() {
                Outcome::Supported
            } else {
                Outcome::Pending
            };
            (o, facts.reasons.clone())
        } else {
            (Outcome::NonMaterial, facts.reasons.clone())
        };
        outcomes.push(PoolOutcome {
            pool: pool.clone(),
            outcome,
            reasons,
        });

        if let (Some(start), Some(end)) = (&facts.start, &facts.end) {
            for token in [0u8, 1] {
                let field = if token == 0 {
                    "totalValueLockedToken0"
                } else {
                    "totalValueLockedToken1"
                };
                let s = Dec::parse(start.tvl(token)).unwrap_or_else(|_| Dec::zero());
                let e = Dec::parse(end.tvl(token)).unwrap_or_else(|_| Dec::zero());
                let change = e.sub(&s);
                let zero = s.is_zero();
                let mut values = BTreeMap::new();
                values.insert("token".to_owned(), json!(token));
                values.insert("tvl_start".to_owned(), json!(start.tvl(token)));
                values.insert("tvl_end".to_owned(), json!(end.tvl(token)));
                values.insert(
                    "change".to_owned(),
                    json!(if zero {
                        change.to_string()
                    } else {
                        change.div18(&s).to_string()
                    }),
                );
                values.insert("relative".to_owned(), json!(!zero));
                claims.push(Claim {
                    kind: ClaimType::TvlChange,
                    pool: pool.clone(),
                    values,
                    calculation: if zero {
                        "tvl_end - tvl_start"
                    } else {
                        "(tvl_end - tvl_start) / tvl_start"
                    }
                    .to_owned(),
                    evidence: vec![
                        fact_id(pool, field, screen.header.block_start),
                        fact_id(pool, field, screen.header.block_end),
                    ],
                });
            }
        }

        let mut largest: Option<(EventKind, &str, &str)> = None;
        for kind in EVENT_KINDS {
            if let Some(hit) = facts.large_events.get(kind) {
                let bigger = match largest {
                    None => true,
                    Some((_, amount, _)) => Dec::parse(&hit.amount_usd)
                        .and_then(|h| {
                            Dec::parse(amount).map(|a| h.cmp(&a) == std::cmp::Ordering::Greater)
                        })
                        .unwrap_or(false),
                };
                if bigger {
                    largest = Some((kind, &hit.amount_usd, &hit.transaction.id));
                }
            }
        }
        if let Some((kind, amount, tx)) = largest {
            let mut values = BTreeMap::new();
            values.insert("kind".to_owned(), json!(kind.as_str()));
            values.insert("amountUSD".to_owned(), json!(amount));
            values.insert("min_event_usd".to_owned(), json!(min_event_usd));
            claims.push(Claim {
                kind: ClaimType::LargeEvent,
                pool: pool.clone(),
                values,
                calculation: "amountUSD >= min_event_usd".to_owned(),
                evidence: vec![tx.to_owned()],
            });
        } else if let Some(h) = held
            && let Some((kind, amount, tx)) = largest_held(h)
        {
            let mut values = BTreeMap::new();
            values.insert("kind".to_owned(), json!(kind.as_str()));
            values.insert("amountUSD".to_owned(), json!(amount));
            values.insert("min_event_usd".to_owned(), json!(min_event_usd));
            values.insert("reaches_threshold".to_owned(), json!(false));
            claims.push(Claim {
                kind: ClaimType::LargestEvent,
                pool: pool.clone(),
                values,
                calculation: "max(amountUSD) over held events, below min_event_usd".to_owned(),
                evidence: vec![tx.to_owned()],
            });
        }

        let mut volume = Dec::zero();
        let mut tx_count: u64 = 0;
        for h in &facts.hours {
            if let Ok(v) = Dec::parse(&h.volume_usd) {
                volume = volume.add(&v);
            }
            tx_count += h.tx_count.parse::<u64>().unwrap_or(0);
        }
        let mut values = BTreeMap::new();
        values.insert("volumeUSD".to_owned(), json!(volume.to_string()));
        values.insert("txCount".to_owned(), json!(tx_count));
        values.insert(
            "largest_swap_usd".to_owned(),
            json!(
                facts
                    .large_events
                    .swap
                    .as_ref()
                    .map(|h| h.amount_usd.clone())
            ),
        );
        values.insert(
            "largest_mint_usd".to_owned(),
            json!(
                facts
                    .large_events
                    .mint
                    .as_ref()
                    .map(|h| h.amount_usd.clone())
            ),
        );
        values.insert(
            "largest_burn_usd".to_owned(),
            json!(
                facts
                    .large_events
                    .burn
                    .as_ref()
                    .map(|h| h.amount_usd.clone())
            ),
        );
        let window = format!(
            "{}-{}",
            screen.header.window_requested.from, screen.header.window_requested.to
        );
        let mut evidence_ids = vec![fact_id(pool, "hours", &window)];
        if let Some(h) = held {
            for kind in EVENT_KINDS {
                values.insert(format!("{}_count", kind.as_str()), json!(h.count(kind)));
                values.insert(format!("{}_amount_usd", kind.as_str()), json!(h.sum(kind)));
            }
            values.insert("amount_usd_nulls".to_owned(), json!(h.amount_usd_nulls));
            evidence_ids.push(fact_id(pool, "events", &window));
        }
        claims.push(Claim {
            kind: ClaimType::ActivitySummary,
            pool: pool.clone(),
            values,
            calculation: if held.is_some() {
                "sum(hours.volumeUSD), sum(hours.txCount), count and sum(amountUSD) per event type"
            } else {
                "sum(hours.volumeUSD), sum(hours.txCount)"
            }
            .to_owned(),
            evidence: evidence_ids,
        });
    }
    (outcomes, claims)
}

fn largest_held(held: &PoolEvents) -> Option<(EventKind, &str, &str)> {
    let mut top: Option<(EventKind, &str, &str)> = None;
    for (kind, e) in held.all() {
        let Some(amount) = e.amount_usd.as_deref() else {
            continue;
        };
        let bigger = match top {
            None => true,
            Some((_, current, _)) => Dec::parse(amount)
                .and_then(|a| Dec::parse(current).map(|c| a.cmp(&c) == std::cmp::Ordering::Greater))
                .unwrap_or(false),
        };
        if bigger {
            top = Some((kind, amount, &e.transaction.id));
        }
    }
    top
}

/// One supporting event in the brief.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SupportingEvent {
    pub kind: EventKind,
    pub tx: String,
    #[serde(rename = "amountUSD")]
    pub amount_usd: Option<String>,
    pub fact: String,
}

/// The brief: the only input the explain step receives, section 8.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Brief {
    pub mandate_id: String,
    pub window: crate::evidence::Window,
    pub outcomes: Vec<PoolOutcome>,
    pub claims: Vec<Claim>,
    pub events: BTreeMap<String, Vec<SupportingEvent>>,
}

/// `512 + 1024 * |R|`, the mandatory brief bound of section 8.
pub fn mandatory_brief_bound(pools: usize) -> usize {
    512 + 1024 * pools
}

#[derive(Debug, thiserror::Error)]
#[error("brief is {size} bytes with no supporting events; the bound is {bound}")]
pub struct BriefTooLarge {
    pub size: usize,
    pub bound: usize,
}

/// Builds the brief with up to `brief_events` supporting events per
/// supported pool, ordered by `amountUSD`, reducing that number one at a time
/// until the JSON fits `bound`. Ends at zero events, which is valid.
pub fn build_brief(
    mandate_id: &str,
    window: crate::evidence::Window,
    outcomes: &[PoolOutcome],
    claims: &[Claim],
    events: Option<&EventsResponse>,
    brief_events: u32,
    bound: usize,
) -> Result<(Brief, Vec<u8>, u32), BriefTooLarge> {
    let mut n = brief_events;
    loop {
        let mut per_pool = BTreeMap::new();
        if let Some(ev) = events {
            for o in outcomes.iter().filter(|o| o.outcome == Outcome::Supported) {
                let Some(held) = ev.pools.get(&o.pool) else {
                    continue;
                };
                let window_id = format!(
                    "{}-{}",
                    ev.header.window_requested.from, ev.header.window_requested.to
                );
                let mut list: Vec<(Dec, SupportingEvent)> = held
                    .all()
                    .map(|(kind, e)| {
                        (
                            e.amount_usd
                                .as_deref()
                                .and_then(|a| Dec::parse(a).ok())
                                .unwrap_or_else(Dec::zero),
                            SupportingEvent {
                                kind,
                                tx: e.transaction.id.clone(),
                                amount_usd: e.amount_usd.clone(),
                                fact: fact_id(&o.pool, "events", &window_id),
                            },
                        )
                    })
                    .collect();
                list.sort_by(|a, b| b.0.cmp(&a.0));
                per_pool.insert(
                    o.pool.clone(),
                    list.into_iter()
                        .take(n as usize)
                        .map(|(_, e)| e)
                        .collect::<Vec<_>>(),
                );
            }
        }
        let brief = Brief {
            mandate_id: mandate_id.to_owned(),
            window,
            outcomes: outcomes.to_vec(),
            claims: claims.to_vec(),
            events: per_pool,
        };
        let bytes = serde_json::to_vec(&brief).expect("brief serializes");
        if bytes.len() <= bound {
            return Ok((brief, bytes, n));
        }
        if n == 0 {
            return Err(BriefTooLarge {
                size: bytes.len(),
                bound,
            });
        }
        n -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::Window;
    use crate::testing::{FROM, POOL, TO, events_fixture as events, screen_fixture as screen};

    #[test]
    fn outcomes_follow_section_8() {
        let (o, _) =
            outcomes_and_claims(&screen(true, true), None, Evidence::Transaction, "100000");
        assert_eq!(o[0].outcome, Outcome::Pending);
        let (o, _) = outcomes_and_claims(&screen(true, true), None, Evidence::Screening, "100000");
        assert_eq!(o[0].outcome, Outcome::Supported);
        let (o, _) = outcomes_and_claims(
            &screen(true, true),
            Some(&events(Some("250000"), false)),
            Evidence::Transaction,
            "100000",
        );
        assert_eq!(o[0].outcome, Outcome::Supported);
        let (o, c) =
            outcomes_and_claims(&screen(false, false), None, Evidence::Transaction, "100000");
        assert_eq!(o[0].outcome, Outcome::NonMaterial);
        assert!(
            !c.iter()
                .any(|c| matches!(c.kind, ClaimType::LargeEvent | ClaimType::LargestEvent))
        );
    }

    #[test]
    fn incomplete_facts_are_undetermined_first() {
        let (o, _) = outcomes_and_claims(
            &screen(true, true),
            Some(&events(Some("1"), true)),
            Evidence::Transaction,
            "100000",
        );
        assert_eq!(
            (o[0].outcome, o[0].reasons.clone()),
            (Outcome::Undetermined, vec!["events_truncated".to_owned()])
        );
        let (o, _) = outcomes_and_claims(
            &screen(true, true),
            Some(&events(None, false)),
            Evidence::Transaction,
            "100000",
        );
        assert_eq!(o[0].reasons, vec!["events_unvalued".to_owned()]);
        let mut s = screen(true, true);
        s.header.coverage_shortfall = true;
        let (o, _) = outcomes_and_claims(&s, None, Evidence::Transaction, "100000");
        assert_eq!(
            (o[0].outcome, o[0].reasons.clone()),
            (Outcome::Undetermined, vec!["coverage_shortfall".to_owned()])
        );
        let mut s = screen(true, true);
        s.pools.get_mut(POOL).unwrap().unvalued_events.mint = true;
        let (o, _) = outcomes_and_claims(&s, None, Evidence::Transaction, "100000");
        assert_eq!(o[0].reasons, vec!["unvalued_event:mint".to_owned()]);
    }

    #[test]
    fn claims_match_the_sellers_values_and_fact_ids() {
        let (_, c) = outcomes_and_claims(
            &screen(true, true),
            Some(&events(Some("250000"), false)),
            Evidence::Transaction,
            "100000",
        );
        let kinds: Vec<ClaimType> = c.iter().map(|c| c.kind).collect();
        assert_eq!(
            kinds,
            vec![
                ClaimType::TvlChange,
                ClaimType::TvlChange,
                ClaimType::LargeEvent,
                ClaimType::ActivitySummary
            ]
        );
        assert_eq!(c[0].values["change"], "0.1");
        assert_eq!(c[0].values["relative"], true);
        assert_eq!(c[0].calculation, "(tvl_end - tvl_start) / tvl_start");
        assert_eq!(
            c[0].evidence,
            vec![
                format!("{POOL}:totalValueLockedToken0@100"),
                format!("{POOL}:totalValueLockedToken0@200")
            ]
        );
        assert_eq!(c[1].values["change"], "0");
        assert_eq!(c[2].evidence, vec!["0xswap".to_owned()]);
        assert_eq!(c[2].transaction_hashes(), vec!["0xswap"]);
        assert_eq!(c[3].values["volumeUSD"], "300.5");
        assert_eq!(c[3].values["txCount"], 7);
        assert_eq!(c[3].values["swap_count"], 1);
        assert_eq!(
            c[3].evidence,
            vec![
                format!("{POOL}:hours@{FROM}-{TO}"),
                format!("{POOL}:events@{FROM}-{TO}")
            ]
        );
    }

    #[test]
    fn small_held_events_still_yield_a_transaction_claim() {
        let (o, c) = outcomes_and_claims(
            &screen(true, false),
            Some(&events(Some("1500"), false)),
            Evidence::Transaction,
            "100000",
        );
        assert_eq!(o[0].outcome, Outcome::Supported);
        let largest = c
            .iter()
            .find(|c| c.kind == ClaimType::LargestEvent)
            .unwrap();
        assert_eq!(largest.evidence, vec!["0xswap".to_owned()]);
        assert_eq!(largest.values["reaches_threshold"], false);
        assert!(!c.iter().any(|c| c.kind == ClaimType::LargeEvent));
    }

    #[test]
    fn the_brief_fits_its_bound_by_shedding_events() {
        let (o, c) = outcomes_and_claims(
            &screen(true, true),
            Some(&events(Some("250000"), false)),
            Evidence::Transaction,
            "100000",
        );
        let bound = mandatory_brief_bound(1);
        assert_eq!(bound, 1536);
        let (brief, bytes, used) = build_brief(
            "m1",
            Window { from: FROM, to: TO },
            &o,
            &c,
            Some(&events(Some("250000"), false)),
            5,
            8192,
        )
        .unwrap();
        assert_eq!(used, 5);
        assert_eq!(brief.events[POOL].len(), 1);
        assert!(bytes.len() < 8192);
        let tight = build_brief(
            "m1",
            Window { from: FROM, to: TO },
            &o,
            &c,
            Some(&events(Some("250000"), false)),
            5,
            bytes.len() - 1,
        );
        match tight {
            Ok((b, _, used)) => {
                assert_eq!(used, 0);
                assert!(b.events[POOL].is_empty());
            }
            Err(e) => assert!(e.size > e.bound),
        }
    }
}
