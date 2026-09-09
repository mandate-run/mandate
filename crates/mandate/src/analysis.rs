//! Spec section 8: outcomes, claims and the bounded brief, computed by the
//! runtime from purchased facts, deterministically, before any explanation
//! is bought. Nothing here reads a seller's verdict, reasons or aggregates:
//! materiality is decided from the facts against the mandate's thresholds
//! and event aggregates are derived from the delivered events. The rules
//! mirror the sellers' `graph.ts` and `claims.ts` exactly, so a bundled
//! report can be checked for equality. Completeness is decided before
//! materiality: incomplete or malformed facts are `undetermined`, never
//! `supported`.
//!
//! Fact ids: `<pool>:<field>@<block>` for a snapshot field,
//! `<pool>:hours@<from>-<to>` for the hourly aggregates, and
//! `<pool>:events@<from>-<to>` for the delivered event lists.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::evidence::{
    Dec, EVENT_KINDS, EventKind, EventsResponse, PoolEvents, PoolScreen, ScreenResponse, Snapshot,
    Verdict, Window,
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

/// The mandate's thresholds, section 7.
#[derive(Debug, Clone, Copy)]
pub struct Thresholds<'a> {
    pub materiality: &'a str,
    pub min_event_usd: &'a str,
}

/// A verdict computed from the facts, the sellers' `materiality()` rule for
/// rule: material reasons win; undetermined reasons only block non_material.
/// A number that does not parse makes the pool undetermined whatever else
/// the facts say, with the field named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Materiality {
    pub verdict: Verdict,
    pub reasons: Vec<String>,
}

fn tvl_field(token: u8) -> &'static str {
    if token == 0 {
        "totalValueLockedToken0"
    } else {
        "totalValueLockedToken1"
    }
}

fn price_of(snapshot: &Snapshot, token: u8) -> Option<&str> {
    if token == 0 {
        snapshot.token0_price_usd.as_deref()
    } else {
        snapshot.token1_price_usd.as_deref()
    }
}

/// `|end - start| >= threshold * start`.
fn relative_change_at_least(start: &Dec, end: &Dec, threshold: &Dec) -> bool {
    end.sub(start).abs().cmp(&threshold.mul(start)) != Ordering::Less
}

pub fn assess(facts: &PoolScreen, t: Thresholds<'_>) -> Materiality {
    let mut reasons: Vec<String> = Vec::new();
    let mut bad: Vec<String> = Vec::new();
    let threshold = Dec::parse(t.materiality);
    let min_usd = Dec::parse(t.min_event_usd);
    if facts.truncated {
        reasons.push("undetermined:truncated".to_owned());
    }
    if facts.coverage_shortfall {
        reasons.push("undetermined:coverage_shortfall".to_owned());
    }
    for kind in EVENT_KINDS {
        if let Some(hit) = facts.large_events.get(kind) {
            match (Dec::parse(&hit.amount_usd), &min_usd) {
                (Ok(a), Ok(m)) if a.cmp(m) != Ordering::Less => {
                    reasons.push(format!("large_event:{}", kind.as_str()));
                }
                (Ok(_), Ok(_)) => bad.push(format!(
                    "large_events.{}.amountUSD below min_event_usd",
                    kind.as_str()
                )),
                _ => bad.push(format!("large_events.{}.amountUSD", kind.as_str())),
            }
        }
    }
    if facts.unvalued_events.mint {
        reasons.push("undetermined:unvalued_event:mint".to_owned());
    }
    if facts.unvalued_events.burn {
        reasons.push("undetermined:unvalued_event:burn".to_owned());
    }
    match (&facts.start, &facts.end) {
        (_, None) => reasons.push("undetermined:pool_absent".to_owned()),
        (None, Some(_)) => reasons.push("undetermined:absent_at_start".to_owned()),
        (Some(start), Some(end)) => {
            for token in [0u8, 1] {
                let (Ok(s), Ok(e)) = (Dec::parse(start.tvl(token)), Dec::parse(end.tvl(token)))
                else {
                    bad.push(tvl_field(token).to_owned());
                    continue;
                };
                if s.is_zero() {
                    if e.is_zero() {
                        continue;
                    }
                    match price_of(end, token) {
                        None => reasons.push(format!("undetermined:null_usd:token{token}")),
                        Some(p) => match (Dec::parse(p), &min_usd) {
                            (Ok(price), Ok(m)) => {
                                if e.mul(&price).cmp(m) != Ordering::Less {
                                    reasons.push(format!("tvl_change:token{token}"));
                                }
                            }
                            _ => bad.push(format!("token{token}PriceUSD")),
                        },
                    }
                } else if let Ok(th) = &threshold {
                    if relative_change_at_least(&s, &e, th) {
                        reasons.push(format!("tvl_change:token{token}"));
                    }
                } else {
                    bad.push("inputs.materiality".to_owned());
                }
            }
        }
    }
    for b in bad {
        reasons.push(format!("undetermined:bad_number:{b}"));
    }
    let malformed = reasons
        .iter()
        .any(|r| r.starts_with("undetermined:bad_number:"));
    let material = reasons
        .iter()
        .any(|r| r.starts_with("tvl_change:") || r.starts_with("large_event:"));
    let verdict = if malformed {
        Verdict::Undetermined
    } else if material {
        Verdict::Material
    } else if reasons.is_empty() {
        Verdict::NonMaterial
    } else {
        Verdict::Undetermined
    };
    Materiality { verdict, reasons }
}

/// Per-type counts, summed `amountUSD` and null count, derived from the
/// delivered event lists and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aggregates {
    pub counts: BTreeMap<EventKind, u64>,
    pub sums: BTreeMap<EventKind, Dec>,
    pub nulls: u64,
    /// Fields whose `amountUSD` did not parse.
    pub bad: Vec<String>,
}

pub fn aggregates(held: &PoolEvents) -> Aggregates {
    let mut counts = BTreeMap::new();
    let mut sums = BTreeMap::new();
    let mut nulls = 0;
    let mut bad = Vec::new();
    for kind in EVENT_KINDS {
        counts.insert(kind, 0);
        sums.insert(kind, Dec::zero());
    }
    for (kind, e) in held.all() {
        *counts.get_mut(&kind).expect("kind") += 1;
        match e.amount_usd.as_deref() {
            None => nulls += 1,
            Some(a) => match Dec::parse(a) {
                Ok(v) => {
                    let s = sums.get_mut(&kind).expect("kind");
                    *s = s.add(&v);
                }
                Err(_) => bad.push(format!("{}.{}.amountUSD", kind.as_str(), e.id)),
            },
        }
    }
    Aggregates {
        counts,
        sums,
        nulls,
        bad,
    }
}

/// Why a pool's facts cannot support a verdict, section 8's undetermined row.
pub fn incompleteness(
    screen: &ScreenResponse,
    facts: &PoolScreen,
    assessed: &Materiality,
    held: Option<&Aggregates>,
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
    for r in &assessed.reasons {
        if let Some(rest) = r.strip_prefix("undetermined:") {
            push(rest.to_owned());
        }
    }
    if let Some(h) = held {
        if h.nulls > 0 {
            push("events_unvalued".to_owned());
        }
        for b in &h.bad {
            push(format!("bad_number:{b}"));
        }
    }
    reasons
}

fn parse_or_bad(text: &str, field: &str, bad: &mut Vec<String>) -> Option<Dec> {
    match Dec::parse(text) {
        Ok(d) => Some(d),
        Err(_) => {
            bad.push(field.to_owned());
            None
        }
    }
}

/// Outcomes and claims for every pool in the screen. `events` holds the
/// transaction-level evidence bought so far, by pool.
pub fn outcomes_and_claims(
    screen: &ScreenResponse,
    events: Option<&EventsResponse>,
    evidence: Evidence,
    t: Thresholds<'_>,
) -> (Vec<PoolOutcome>, Vec<Claim>) {
    let mut outcomes = Vec::new();
    let mut claims = Vec::new();
    let window = format!(
        "{}-{}",
        screen.header.window_requested.from, screen.header.window_requested.to
    );
    for (pool, facts) in &screen.pools {
        let held_events = events.and_then(|e| e.pools.get(pool));
        let held = held_events.map(aggregates);
        let assessed = assess(facts, t);
        let mut incomplete = incompleteness(screen, facts, &assessed, held.as_ref());
        if held_events.is_some_and(|h| h.truncated) {
            incomplete.push("events_truncated".to_owned());
        }
        let (outcome, reasons) = if !incomplete.is_empty() {
            (Outcome::Undetermined, incomplete)
        } else if assessed.verdict == Verdict::Material {
            let o = if evidence == Evidence::Screening || held.is_some() {
                Outcome::Supported
            } else {
                Outcome::Pending
            };
            (o, assessed.reasons.clone())
        } else {
            (Outcome::NonMaterial, assessed.reasons.clone())
        };
        outcomes.push(PoolOutcome {
            pool: pool.clone(),
            outcome,
            reasons,
        });

        let mut bad = Vec::new();
        if let (Some(start), Some(end)) = (&facts.start, &facts.end) {
            for token in [0u8, 1] {
                let field = tvl_field(token);
                let (Some(s), Some(e)) = (
                    parse_or_bad(start.tvl(token), field, &mut bad),
                    parse_or_bad(end.tvl(token), field, &mut bad),
                ) else {
                    continue;
                };
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
                        .and_then(|h| Dec::parse(amount).map(|a| h.cmp(&a) == Ordering::Greater))
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
            values.insert("min_event_usd".to_owned(), json!(t.min_event_usd));
            claims.push(Claim {
                kind: ClaimType::LargeEvent,
                pool: pool.clone(),
                values,
                calculation: "amountUSD >= min_event_usd".to_owned(),
                evidence: vec![tx.to_owned()],
            });
        } else if let Some(h) = held_events
            && let Some((kind, amount, tx)) = largest_held(h)
        {
            let mut values = BTreeMap::new();
            values.insert("kind".to_owned(), json!(kind.as_str()));
            values.insert("amountUSD".to_owned(), json!(amount));
            values.insert("min_event_usd".to_owned(), json!(t.min_event_usd));
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
        let mut hours_ok = true;
        for h in &facts.hours {
            match Dec::parse(&h.volume_usd) {
                Ok(v) => volume = volume.add(&v),
                Err(_) => hours_ok = false,
            }
            match h.tx_count.parse::<u64>() {
                Ok(n) => tx_count += n,
                Err(_) => hours_ok = false,
            }
        }
        if hours_ok {
            let mut values = BTreeMap::new();
            values.insert("volumeUSD".to_owned(), json!(volume.to_string()));
            values.insert("txCount".to_owned(), json!(tx_count));
            for kind in EVENT_KINDS {
                values.insert(
                    format!("largest_{}_usd", kind.as_str()),
                    json!(facts.large_events.get(kind).map(|h| h.amount_usd.clone())),
                );
            }
            let mut evidence_ids = vec![fact_id(pool, "hours", &window)];
            if let Some(h) = &held {
                for kind in EVENT_KINDS {
                    values.insert(format!("{}_count", kind.as_str()), json!(h.counts[&kind]));
                    values.insert(
                        format!("{}_amount_usd", kind.as_str()),
                        json!(h.sums[&kind].to_string()),
                    );
                }
                values.insert("amount_usd_nulls".to_owned(), json!(h.nulls));
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
                .and_then(|a| Dec::parse(current).map(|c| a.cmp(&c) == Ordering::Greater))
                .unwrap_or(false),
        };
        if bigger {
            top = Some((kind, amount, &e.transaction.id));
        }
    }
    top
}

/// `512 + 1024 * |R|`, the mandatory brief bound of section 8, on the whole
/// explain request body.
pub fn mandatory_brief_bound(pools: usize) -> usize {
    512 + 1024 * pools
}

#[derive(Debug, thiserror::Error)]
#[error("brief is {size} bytes with no supporting events; the bound is {bound}")]
pub struct BriefTooLarge {
    pub size: usize,
    pub bound: usize,
}

/// The brief as the explain step receives it: compact keys, numbers in
/// [`Dec::brief_form`], claims grouped by pool, blocks in the header so
/// every fact id is reconstructible.
#[derive(Debug, Clone, PartialEq)]
pub struct Brief {
    pub json: Value,
    /// The explain request body, `{"brief": ...}`, compact.
    pub body: Vec<u8>,
    /// Supporting events per pool that fit.
    pub events_used: u32,
}

fn brief_number(text: &str) -> Value {
    match Dec::parse(text) {
        Ok(d) => Value::String(d.brief_form()),
        Err(_) => Value::String(text.to_owned()),
    }
}

fn number_value(v: Option<&Value>) -> Value {
    match v {
        Some(Value::String(s)) => brief_number(s),
        Some(other) => other.clone(),
        None => Value::Null,
    }
}

fn compact_claim(c: &Claim) -> Value {
    let v = &c.values;
    match c.kind {
        ClaimType::TvlChange => json!({
            "t": "tvl",
            "i": v.get("token").cloned().unwrap_or(Value::Null),
            "s": number_value(v.get("tvl_start")),
            "e": number_value(v.get("tvl_end")),
            "c": number_value(v.get("change")),
            "rel": v.get("relative").cloned().unwrap_or(Value::Null),
        }),
        ClaimType::LargeEvent | ClaimType::LargestEvent => json!({
            "t": if c.kind == ClaimType::LargeEvent { "large" } else { "largest" },
            "k": v.get("kind").cloned().unwrap_or(Value::Null),
            "a": number_value(v.get("amountUSD")),
            "min": number_value(v.get("min_event_usd")),
            "tx": c.evidence.first().cloned().unwrap_or_default(),
        }),
        ClaimType::ActivitySummary => {
            let mut out = json!({
                "t": "act",
                "v": number_value(v.get("volumeUSD")),
                "n": v.get("txCount").cloned().unwrap_or(Value::Null),
                "lg": {
                    "s": number_value(v.get("largest_swap_usd")),
                    "m": number_value(v.get("largest_mint_usd")),
                    "b": number_value(v.get("largest_burn_usd")),
                },
            });
            if v.contains_key("swap_count") {
                out["ev"] = json!({
                    "s": [v.get("swap_count").cloned().unwrap_or(Value::Null), number_value(v.get("swap_amount_usd"))],
                    "m": [v.get("mint_count").cloned().unwrap_or(Value::Null), number_value(v.get("mint_amount_usd"))],
                    "b": [v.get("burn_count").cloned().unwrap_or(Value::Null), number_value(v.get("burn_amount_usd"))],
                    "n": v.get("amount_usd_nulls").cloned().unwrap_or(Value::Null),
                });
            }
            out
        }
    }
}

/// Builds the brief with up to `brief_events` supporting events per
/// supported pool, ordered by `amountUSD`, reducing that number one at a time
/// until the body fits `bound`. Ends at zero events, which is valid.
/// What the brief is built from.
#[derive(Debug, Clone, Copy)]
pub struct BriefInput<'a> {
    pub mandate_id: &'a str,
    pub window: Window,
    /// `block_start` and `block_end` per pool, from the purchase that covers
    /// it, so every fact id is reconstructible.
    pub blocks: &'a BTreeMap<String, (u64, u64)>,
    pub outcomes: &'a [PoolOutcome],
    pub claims: &'a [Claim],
    pub events: Option<&'a EventsResponse>,
    /// Most supporting events per supported pool, `requirements.brief_events`.
    pub brief_events: u32,
}

pub fn build_brief(input: BriefInput<'_>, bound: usize) -> Result<Brief, BriefTooLarge> {
    let BriefInput {
        mandate_id,
        window,
        blocks,
        outcomes,
        claims,
        events,
        brief_events,
    } = input;
    let mut n = brief_events;
    loop {
        let mut by_pool: serde_json::Map<String, Value> = serde_json::Map::new();
        for c in claims {
            by_pool
                .entry(c.pool.clone())
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .expect("array")
                .push(compact_claim(c));
        }
        let mut per_pool = serde_json::Map::new();
        if let Some(ev) = events {
            for o in outcomes.iter().filter(|o| o.outcome == Outcome::Supported) {
                let Some(held) = ev.pools.get(&o.pool) else {
                    continue;
                };
                let mut list: Vec<(Dec, Value)> = held
                    .all()
                    .map(|(kind, e)| {
                        (
                            e.amount_usd
                                .as_deref()
                                .and_then(|a| Dec::parse(a).ok())
                                .unwrap_or_else(Dec::zero),
                            json!({
                                "k": kind.as_str(),
                                "tx": e.transaction.id,
                                "a": e.amount_usd.as_deref().map_or(Value::Null, brief_number),
                            }),
                        )
                    })
                    .collect();
                list.sort_by(|a, b| b.0.cmp(&a.0));
                per_pool.insert(
                    o.pool.clone(),
                    Value::Array(list.into_iter().take(n as usize).map(|(_, e)| e).collect()),
                );
            }
        }
        let brief = json!({
            "m": mandate_id,
            "w": [window.from, window.to],
            "o": outcomes
                .iter()
                .map(|o| {
                    let b = blocks.get(&o.pool).map_or(Value::Null, |b| json!([b.0, b.1]));
                    json!({ "p": o.pool, "o": o.outcome.as_str(), "r": o.reasons, "b": b })
                })
                .collect::<Vec<_>>(),
            "c": Value::Object(by_pool),
            "e": Value::Object(per_pool),
        });
        let body = serde_json::to_vec(&json!({ "brief": brief })).expect("brief serializes");
        if body.len() <= bound {
            return Ok(Brief {
                json: brief,
                body,
                events_used: n,
            });
        }
        if n == 0 {
            return Err(BriefTooLarge {
                size: body.len(),
                bound,
            });
        }
        n -= 1;
    }
}

/// Every number string in the brief, for the prose check: the model sees
/// these forms, which may be shorter than the exact claim values.
pub fn brief_numbers(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Number(n) => out.push(n.to_string()),
        Value::String(s) => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|v| brief_numbers(v, out)),
        Value::Object(o) => o.values().for_each(|v| brief_numbers(v, out)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::{Counts, Event, EventHit, Header, PoolEvents, Sums, TxRef, Verdict};
    use crate::testing::{FROM, POOL, TO, events_fixture as events, screen_fixture as screen};

    const T: Thresholds<'static> = Thresholds {
        materiality: "0.05",
        min_event_usd: "100000",
    };

    #[test]
    fn materiality_follows_the_sellers_rules() {
        let base = screen(false, false);
        let facts = &base.pools[POOL];
        assert_eq!(assess(facts, T).verdict, Verdict::NonMaterial);
        let mut at = facts.clone();
        at.end.as_mut().unwrap().tvl_token0 = "1050".to_owned();
        assert_eq!(
            assess(&at, T),
            Materiality {
                verdict: Verdict::Material,
                reasons: vec!["tvl_change:token0".to_owned()]
            }
        );
        let mut below = facts.clone();
        below.end.as_mut().unwrap().tvl_token0 = "1049.999999999999999999".to_owned();
        assert_eq!(assess(&below, T).verdict, Verdict::NonMaterial);
        let mut zero_start = facts.clone();
        zero_start.start.as_mut().unwrap().tvl_token1 = "0".to_owned();
        zero_start.end.as_mut().unwrap().tvl_token1 = "60".to_owned();
        assert_eq!(
            assess(&zero_start, T).reasons,
            vec!["tvl_change:token1".to_owned()],
            "60 * 2000 >= 100000"
        );
        zero_start.end.as_mut().unwrap().tvl_token1 = "40".to_owned();
        assert_eq!(assess(&zero_start, T).verdict, Verdict::NonMaterial);
        zero_start.end.as_mut().unwrap().token1_price_usd = None;
        assert_eq!(
            assess(&zero_start, T),
            Materiality {
                verdict: Verdict::Undetermined,
                reasons: vec!["undetermined:null_usd:token1".to_owned()]
            }
        );
        let mut absent = facts.clone();
        absent.start = None;
        assert_eq!(
            assess(&absent, T).reasons,
            vec!["undetermined:absent_at_start".to_owned()]
        );
        absent.large_events.mint = Some(EventHit {
            transaction: TxRef {
                id: "0xmint".into(),
            },
            amount_usd: "500000".into(),
        });
        assert_eq!(
            assess(&absent, T).verdict,
            Verdict::Material,
            "a material reason wins"
        );
        let mut unvalued = facts.clone();
        unvalued.unvalued_events.mint = true;
        assert_eq!(
            assess(&unvalued, T).reasons,
            vec!["undetermined:unvalued_event:mint".to_owned()]
        );
        let mut short = facts.clone();
        short.coverage_shortfall = true;
        assert_eq!(assess(&short, T).verdict, Verdict::Undetermined);
    }

    #[test]
    fn the_sellers_verdict_is_never_trusted() {
        // Probe: a 10% change the seller labels non_material.
        let mut s = screen(true, false);
        let p = s.pools.get_mut(POOL).unwrap();
        p.verdict = Verdict::NonMaterial;
        p.reasons.clear();
        let (o, _) = outcomes_and_claims(&s, None, Evidence::Transaction, T);
        assert_eq!(
            (o[0].outcome, o[0].reasons.clone()),
            (Outcome::Pending, vec!["tvl_change:token0".to_owned()])
        );
        // And a quiet pool the seller labels material stays non_material.
        let mut q = screen(false, false);
        q.pools.get_mut(POOL).unwrap().verdict = Verdict::Material;
        let (o, _) = outcomes_and_claims(&q, None, Evidence::Transaction, T);
        assert_eq!(o[0].outcome, Outcome::NonMaterial);
    }

    #[test]
    fn event_aggregates_come_from_the_events() {
        // Probe: one swap reported as 999 swaps with an inflated sum.
        let mut e = events(Some("250000"), false);
        let held = e.pools.get_mut(POOL).unwrap();
        held.counts = Counts {
            swap: 999,
            mint: 0,
            burn: 0,
        };
        held.sum_amount_usd = Sums {
            swap: "999000000".into(),
            mint: "0".into(),
            burn: "0".into(),
        };
        held.amount_usd_nulls = 5;
        let (o, c) = outcomes_and_claims(&screen(true, true), Some(&e), Evidence::Transaction, T);
        assert_eq!(o[0].outcome, Outcome::Supported);
        let act = c
            .iter()
            .find(|c| c.kind == ClaimType::ActivitySummary)
            .unwrap();
        assert_eq!(act.values["swap_count"], 1);
        assert_eq!(act.values["swap_amount_usd"], "250000");
        assert_eq!(act.values["amount_usd_nulls"], 0);
        let agg = aggregates(&e.pools[POOL]);
        assert_eq!((agg.counts[&EventKind::Swap], agg.nulls), (1, 0));
    }

    #[test]
    fn malformed_numbers_make_a_pool_undetermined() {
        // Probe: a malformed TVL string must not become zero.
        let mut s = screen(true, true);
        s.pools
            .get_mut(POOL)
            .unwrap()
            .end
            .as_mut()
            .unwrap()
            .tvl_token0 = "1,100".to_owned();
        let (o, c) = outcomes_and_claims(&s, None, Evidence::Transaction, T);
        assert_eq!(o[0].outcome, Outcome::Undetermined);
        assert!(
            o[0].reasons
                .contains(&"bad_number:totalValueLockedToken0".to_owned()),
            "{:?}",
            o[0].reasons
        );
        assert_eq!(
            c.iter().filter(|c| c.kind == ClaimType::TvlChange).count(),
            1,
            "the malformed token has no claim"
        );
        let mut e = events(Some("abc"), false);
        e.pools.get_mut(POOL).unwrap().swaps.push(Event {
            id: "s2".into(),
            transaction: TxRef {
                id: "0xother".into(),
            },
            log_index: None,
            timestamp: FROM + 2,
            amount0: "1".into(),
            amount1: "1".into(),
            amount_usd: Some("5".into()),
            origin: "0xo".into(),
            owner: None,
            tick_lower: None,
            tick_upper: None,
        });
        let (o, _) = outcomes_and_claims(&screen(true, true), Some(&e), Evidence::Transaction, T);
        assert_eq!(o[0].outcome, Outcome::Undetermined);
        assert!(
            o[0].reasons
                .iter()
                .any(|r| r.starts_with("bad_number:swap.s1")),
            "{:?}",
            o[0].reasons
        );
    }

    #[test]
    fn outcomes_follow_section_8() {
        let (o, _) = outcomes_and_claims(&screen(true, true), None, Evidence::Transaction, T);
        assert_eq!(o[0].outcome, Outcome::Pending);
        let (o, _) = outcomes_and_claims(&screen(true, true), None, Evidence::Screening, T);
        assert_eq!(o[0].outcome, Outcome::Supported);
        let (o, _) = outcomes_and_claims(
            &screen(true, true),
            Some(&events(Some("250000"), false)),
            Evidence::Transaction,
            T,
        );
        assert_eq!(o[0].outcome, Outcome::Supported);
        let (o, c) = outcomes_and_claims(&screen(false, false), None, Evidence::Transaction, T);
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
            T,
        );
        assert_eq!(
            (o[0].outcome, o[0].reasons.clone()),
            (Outcome::Undetermined, vec!["events_truncated".to_owned()])
        );
        let (o, _) = outcomes_and_claims(
            &screen(true, true),
            Some(&events(None, false)),
            Evidence::Transaction,
            T,
        );
        assert_eq!(o[0].reasons, vec!["events_unvalued".to_owned()]);
        let mut s = screen(true, true);
        s.header.coverage_shortfall = true;
        let (o, _) = outcomes_and_claims(&s, None, Evidence::Transaction, T);
        assert_eq!(
            (o[0].outcome, o[0].reasons.clone()),
            (Outcome::Undetermined, vec!["coverage_shortfall".to_owned()])
        );
        let mut s = screen(true, true);
        s.pools.get_mut(POOL).unwrap().unvalued_events.mint = true;
        let (o, _) = outcomes_and_claims(&s, None, Evidence::Transaction, T);
        assert_eq!(o[0].reasons, vec!["unvalued_event:mint".to_owned()]);
    }

    #[test]
    fn claims_match_the_sellers_values_and_fact_ids() {
        let (_, c) = outcomes_and_claims(
            &screen(true, true),
            Some(&events(Some("250000"), false)),
            Evidence::Transaction,
            T,
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
            T,
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
        let ev = events(Some("250000"), false);
        let (o, c) = outcomes_and_claims(&screen(true, true), Some(&ev), Evidence::Transaction, T);
        let w = Window { from: FROM, to: TO };
        let blocks = BTreeMap::from([(POOL.to_owned(), (100u64, 200u64))]);
        let input = BriefInput {
            mandate_id: "m1",
            window: w,
            blocks: &blocks,
            outcomes: &o,
            claims: &c,
            events: Some(&ev),
            brief_events: 5,
        };
        let b = build_brief(input, 8192).unwrap();
        assert_eq!(b.events_used, 5);
        assert_eq!(b.json["e"][POOL].as_array().unwrap().len(), 1);
        assert_eq!(b.json["o"][0]["b"], json!([100, 200]));
        assert!(
            b.body.starts_with(br#"{"brief":{"c":{"#),
            "{}",
            String::from_utf8_lossy(&b.body)
        );
        let tight = build_brief(input, b.body.len() - 1).unwrap();
        assert_eq!(tight.events_used, 0);
        assert!(tight.json["e"][POOL].as_array().unwrap().is_empty());
        let none = build_brief(
            BriefInput {
                brief_events: 0,
                ..input
            },
            10,
        )
        .unwrap_err();
        assert!(none.size > none.bound);
        let mut numbers = Vec::new();
        brief_numbers(&b.json, &mut numbers);
        assert!(numbers.contains(&"250000".to_owned()) && numbers.contains(&"0.1".to_owned()));
    }

    /// Worst case per section 8: 18 significant digits everywhere, all three
    /// large hits, every claim, held events, a 64 byte mandate id.
    fn worst_case(pools: usize) -> (ScreenResponse, EventsResponse) {
        let big = "123456789012345678901234567890.123456789012345678";
        let hash = format!("0x{}", "f".repeat(64));
        let mut screen = screen(true, true);
        let mut ev = events(Some(big), false);
        let (facts, held) = (
            screen.pools.remove(POOL).unwrap(),
            ev.pools.remove(POOL).unwrap(),
        );
        for i in 0..pools {
            let pool = format!("0x{:040x}", i + 1);
            let mut f = facts.clone();
            for s in [f.start.as_mut().unwrap(), f.end.as_mut().unwrap()] {
                s.tvl_token0 = big.to_owned();
                s.tvl_token1 = big.to_owned();
            }
            f.end.as_mut().unwrap().tvl_token0 =
                "223456789012345678901234567890.123456789012345678".to_owned();
            for kind in EVENT_KINDS {
                *f.large_events.get_mut(kind) = Some(EventHit {
                    transaction: TxRef { id: hash.clone() },
                    amount_usd: big.to_owned(),
                });
            }
            f.hours.iter_mut().for_each(|h| {
                h.volume_usd = big.to_owned();
                h.tx_count = "4294967295".to_owned();
            });
            screen.pools.insert(pool.clone(), f);
            let mut h = held.clone();
            h.swaps[0].transaction.id = hash.clone();
            h.counts.swap = 99_999_999;
            ev.pools.insert(pool, h);
        }
        (screen, ev)
    }

    #[test]
    fn the_mandatory_brief_fits_the_bound_for_every_size() {
        let id = "m".repeat(crate::mandate::MAX_ID_LEN);
        let w = Window {
            from: 1_788_793_200,
            to: 1_788_879_600,
        };
        for pools in 1..=20 {
            let (screen, ev) = worst_case(pools);
            let (o, c) = outcomes_and_claims(&screen, Some(&ev), Evidence::Transaction, T);
            assert_eq!(c.len(), 4 * pools);
            let bound = mandatory_brief_bound(pools);
            let blocks: BTreeMap<String, (u64, u64)> = screen
                .pools
                .keys()
                .map(|p| (p.clone(), (23_500_000u64, 23_507_000u64)))
                .collect();
            let input = BriefInput {
                mandate_id: &id,
                window: w,
                blocks: &blocks,
                outcomes: &o,
                claims: &c,
                events: Some(&ev),
                brief_events: 0,
            };
            let b = build_brief(input, bound).unwrap_or_else(|e| panic!("{pools} pools: {e}"));
            assert!(
                b.body.len() <= bound,
                "{pools} pools: {} > {bound}",
                b.body.len()
            );
        }
        let (screen, ev) = worst_case(5);
        let (o, c) = outcomes_and_claims(&screen, Some(&ev), Evidence::Transaction, T);
        let blocks: BTreeMap<String, (u64, u64)> = screen
            .pools
            .keys()
            .map(|p| (p.clone(), (1u64, 2u64)))
            .collect();
        let input = BriefInput {
            mandate_id: &id,
            window: w,
            blocks: &blocks,
            outcomes: &o,
            claims: &c,
            events: Some(&ev),
            brief_events: 0,
        };
        let b = build_brief(input, usize::MAX).unwrap();
        assert!(
            b.body.len() <= 8 * 1024,
            "five pools fit the explain listing: {}",
            b.body.len()
        );
        let _ = (Header::clone, PoolEvents::clone);
    }
}
