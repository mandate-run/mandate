//! Spec section 9: result validation over the purchases a run holds. Every
//! check is named by its failure reason: `coverage`, `calculation`,
//! `citation`, `prose`, `provenance`, `freshness`, `schema`.
//!
//! Evidence keeps its provenance: each purchase carries its own response
//! header and quote time, a pool's screen facts come from the latest purchase
//! whose screen covers it, its events from the latest purchase whose events
//! cover it, and every fact id carries that purchase's blocks.
//! [`validate_deliveries`] holds the checks the deliveries can fail on their
//! own and runs before any of them authorizes another purchase;
//! [`validate`] adds the report-wide checks. The local checks are pure;
//! provenance needs an Ethereum JSON-RPC and is applied afterwards with
//! [`Validation::with_provenance`].

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

use crate::analysis::{
    Claim, ClaimType, Outcome, PoolOutcome, Thresholds, aggregates, assess, outcomes_and_claims,
};
use crate::evidence::{
    Dec, EVENT_KINDS, EventsResponse, Explanation, Header, ScreenResponse, Window,
};
use crate::mandate::{Citations, Evidence};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Coverage,
    Calculation,
    Citation,
    Prose,
    Provenance,
    Freshness,
    Schema,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Coverage => "coverage",
            Self::Calculation => "calculation",
            Self::Citation => "citation",
            Self::Prose => "prose",
            Self::Provenance => "provenance",
            Self::Freshness => "freshness",
            Self::Schema => "schema",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub reason: Reason,
    pub detail: String,
}

fn fail(failures: &mut Vec<Failure>, reason: Reason, detail: impl Into<String>) {
    failures.push(Failure {
        reason,
        detail: detail.into(),
    });
}

/// One delivered response and when its quote was received, for freshness.
#[derive(Debug)]
pub struct Timed<'a, T> {
    pub body: &'a T,
    pub quoted_at: OffsetDateTime,
}

impl<T> Clone for Timed<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Timed<'_, T> {}

/// One purchase's evidence: a screen, events, or both for a bundle.
#[derive(Debug, Clone)]
pub struct Purchased<'a> {
    /// The listing id, for messages.
    pub label: String,
    pub screen: Option<Timed<'a, ScreenResponse>>,
    pub events: Option<Timed<'a, EventsResponse>>,
}

/// What the run bought, in purchase order.
#[derive(Debug, Clone, Default)]
pub struct Delivered<'a> {
    pub purchases: Vec<Purchased<'a>>,
    pub explanation: Option<&'a Explanation>,
}

/// One pool's facts, each from the purchase that is authoritative for it.
#[derive(Debug, Clone)]
pub struct PoolView {
    pub pool: String,
    pub screen_label: String,
    /// The covering screen, restricted to this pool, with its own header.
    pub screen: ScreenResponse,
    pub screen_at: OffsetDateTime,
    pub events_label: Option<String>,
    /// The covering events, restricted to this pool, with their own header.
    pub events: Option<EventsResponse>,
    pub events_at: Option<OffsetDateTime>,
}

impl<'a> Delivered<'a> {
    /// The latest purchase whose screen has `pool`.
    pub fn screen_for(&self, pool: &str) -> Option<(&Purchased<'a>, Timed<'a, ScreenResponse>)> {
        self.purchases.iter().rev().find_map(|p| {
            p.screen
                .filter(|s| s.body.pools.contains_key(pool))
                .map(|s| (p, s))
        })
    }

    /// The latest purchase whose events have `pool`.
    pub fn events_for(&self, pool: &str) -> Option<(&Purchased<'a>, Timed<'a, EventsResponse>)> {
        self.purchases.iter().rev().find_map(|p| {
            p.events
                .filter(|e| e.body.pools.contains_key(pool))
                .map(|e| (p, e))
        })
    }

    /// Every pool any screen covers.
    pub fn pools(&self) -> BTreeSet<String> {
        self.purchases
            .iter()
            .filter_map(|p| p.screen)
            .flat_map(|s| s.body.pools.keys().cloned())
            .collect()
    }

    pub fn view(&self, pool: &str) -> Option<PoolView> {
        let (sp, screen) = self.screen_for(pool)?;
        let mut only = screen.body.clone();
        only.pools.retain(|k, _| k == pool);
        let events = self.events_for(pool).map(|(ep, e)| {
            let mut only = e.body.clone();
            only.pools.retain(|k, _| k == pool);
            (ep.label.clone(), only, e.quoted_at)
        });
        Some(PoolView {
            pool: pool.to_owned(),
            screen_label: sp.label.clone(),
            screen: only,
            screen_at: screen.quoted_at,
            events_label: events.as_ref().map(|e| e.0.clone()),
            events: events.as_ref().map(|e| e.1.clone()),
            events_at: events.map(|e| e.2),
        })
    }

    pub fn views(&self) -> Vec<PoolView> {
        self.pools().iter().filter_map(|p| self.view(p)).collect()
    }

    /// Observation blocks per pool, from each pool's covering screen.
    pub fn blocks(&self) -> BTreeMap<String, (u64, u64)> {
        self.views()
            .into_iter()
            .map(|v| {
                (
                    v.pool,
                    (v.screen.header.block_start, v.screen.header.block_end),
                )
            })
            .collect()
    }
}

/// Section 8 over every pool the deliveries cover, each from its covering
/// purchases. This is what the report states and what validation recomputes.
pub fn compute(
    delivered: &Delivered<'_>,
    evidence: Evidence,
    t: Thresholds<'_>,
) -> (Vec<PoolOutcome>, Vec<Claim>) {
    let mut outcomes = Vec::new();
    let mut claims = Vec::new();
    for v in delivered.views() {
        let (o, c) = outcomes_and_claims(&v.screen, v.events.as_ref(), evidence, t);
        outcomes.extend(o);
        claims.extend(c);
    }
    (outcomes, claims)
}

/// The mandate's requirements that validation reads.
#[derive(Debug, Clone)]
pub struct Rules<'a> {
    /// `R`, lowercased pool addresses.
    pub required: &'a [String],
    /// The window the run requested; every delivery must state and cover it.
    pub window: Window,
    pub evidence: Evidence,
    pub citations: Citations,
    pub max_data_age_s: u64,
    pub provenance_samples: u32,
    pub degrade: bool,
    pub thresholds: Thresholds<'a>,
    /// Numbers a model may state that are inputs rather than facts: window hours and pool count.
    pub input_numbers: Vec<String>,
    /// The number forms the brief showed the model, section 8.
    pub brief_numbers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageResult {
    pub required: usize,
    pub resolved: usize,
    pub pending: Vec<String>,
    pub undetermined: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sample {
    pub pool: String,
    pub tx: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Provenance {
    /// No transaction hash is cited.
    NotApplicable,
    /// Hashes are cited and `provenance_samples` is 0.
    NotRun {
        cited: usize,
    },
    /// Chosen deterministically, not yet checked.
    Pending {
        samples: Vec<(String, String)>,
    },
    Checked {
        samples: Vec<Sample>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Validation {
    pub passed: bool,
    /// No pool in `R` is pending or undetermined.
    pub complete: bool,
    pub failures: Vec<Failure>,
    pub coverage: CoverageResult,
    /// Claims whose calculation re-evaluated to its value.
    pub calculations: usize,
    /// Evidence references that resolved.
    pub references: usize,
    /// Claims citing a transaction hash.
    pub transaction_citations: usize,
    /// Numbers found in the prose and matched.
    pub prose_numbers: usize,
    pub provenance: Provenance,
}

fn is_tx_hash(s: &str) -> bool {
    s.len() == 66 && s.starts_with("0x") && s[2..].chars().all(|c| c.is_ascii_hexdigit())
}

fn is_fact_id(s: &str) -> bool {
    !is_tx_hash(s) && s.contains(':') && s.contains('@')
}

/// Whether a fact id names a purchased fact of the pool's covering purchases.
fn resolve_fact(id: &str, view: &PoolView) -> Result<(), String> {
    let (id_pool, rest) = id
        .split_once(':')
        .ok_or_else(|| format!("{id}: not a fact id"))?;
    let (field, at) = rest
        .split_once('@')
        .ok_or_else(|| format!("{id}: not a fact id"))?;
    if id_pool != view.pool {
        return Err(format!(
            "{id}: names pool {id_pool}, claim is about {}",
            view.pool
        ));
    }
    let facts = view
        .screen
        .pools
        .get(&view.pool)
        .ok_or_else(|| format!("{id}: pool not in the screen"))?;
    let h = &view.screen.header;
    match field {
        "totalValueLockedToken0"
        | "totalValueLockedToken1"
        | "totalValueLockedUSD"
        | "liquidity" => {
            let block: u64 = at
                .parse()
                .map_err(|_| format!("{id}: block is not a number"))?;
            let snapshot = if block == h.block_start {
                facts.start.as_ref()
            } else if block == h.block_end {
                facts.end.as_ref()
            } else {
                return Err(format!(
                    "{id}: block {block} is neither {} nor {} of {}",
                    h.block_start, h.block_end, view.screen_label
                ));
            };
            snapshot
                .map(|_| ())
                .ok_or_else(|| format!("{id}: no snapshot at block {block}"))
        }
        "hours" => {
            let window = format!("{}-{}", h.window_requested.from, h.window_requested.to);
            (at == window)
                .then_some(())
                .ok_or_else(|| format!("{id}: window is {window}"))
        }
        "events" => {
            let ev = view
                .events
                .as_ref()
                .ok_or_else(|| format!("{id}: no events held"))?;
            let window = format!(
                "{}-{}",
                ev.header.window_requested.from, ev.header.window_requested.to
            );
            if at != window {
                return Err(format!("{id}: events window is {window}"));
            }
            ev.pools
                .contains_key(&view.pool)
                .then_some(())
                .ok_or_else(|| format!("{id}: pool not in the events"))
        }
        other => Err(format!("{id}: unknown field {other}")),
    }
}

fn resolve_tx(tx: &str, view: &PoolView) -> Result<(), String> {
    if let Some(facts) = view.screen.pools.get(&view.pool)
        && EVENT_KINDS.iter().any(|k| {
            facts
                .large_events
                .get(*k)
                .is_some_and(|h| h.transaction.id == tx)
        })
    {
        return Ok(());
    }
    if let Some(ev) = &view.events
        && let Some(held) = ev.pools.get(&view.pool)
        && held.all().any(|(_, e)| e.transaction.id == tx)
    {
        return Ok(());
    }
    Err(format!(
        "{tx}: not among the purchased events of {}",
        view.pool
    ))
}

/// Schema and freshness of one response header against the requested window.
fn check_header(
    what: &str,
    h: &Header,
    quoted_at: OffsetDateTime,
    rules: &Rules<'_>,
    failures: &mut Vec<Failure>,
) {
    if h.deployment_id.is_empty() {
        fail(
            failures,
            Reason::Schema,
            format!("{what}: deployment_id is empty"),
        );
    }
    if h.block_end < h.block_start {
        fail(
            failures,
            Reason::Schema,
            format!("{what}: block_end before block_start"),
        );
    }
    if h.indexed_block < h.block_end {
        fail(
            failures,
            Reason::Schema,
            format!("{what}: indexed_block before block_end"),
        );
    }
    let w = rules.window;
    if h.window_requested != w {
        fail(
            failures,
            Reason::Schema,
            format!(
                "{what}: window_requested {}..{} is not the requested {}..{}",
                h.window_requested.from, h.window_requested.to, w.from, w.to
            ),
        );
    }
    if h.window_covered.from != w.from || h.window_covered.to > w.to || h.window_covered.to < w.from
    {
        fail(
            failures,
            Reason::Schema,
            format!(
                "{what}: window_covered {}..{} is outside the requested {}..{}",
                h.window_covered.from, h.window_covered.to, w.from, w.to
            ),
        );
    }
    if h.block_end_timestamp > w.to {
        fail(
            failures,
            Reason::Schema,
            format!(
                "{what}: block_end_timestamp {} is after the window end {}",
                h.block_end_timestamp, w.to
            ),
        );
    }
    match h.indexed_block_timestamp {
        None => fail(
            failures,
            Reason::Freshness,
            format!("{what}: indexed_block_timestamp is null"),
        ),
        Some(ts) => {
            let age = quoted_at.unix_timestamp() - ts as i64;
            if age > rules.max_data_age_s as i64 {
                fail(
                    failures,
                    Reason::Freshness,
                    format!(
                        "{what}: indexed block is {age} s older than the quote, max_data_age_s is {}",
                        rules.max_data_age_s
                    ),
                );
            }
            if ts < w.to && !h.coverage_shortfall {
                fail(
                    failures,
                    Reason::Freshness,
                    format!(
                        "{what}: indexed at {ts}, before the window end {}, without coverage_shortfall",
                        w.to
                    ),
                );
            }
            if ts < w.to && h.window_covered.to == w.to {
                fail(
                    failures,
                    Reason::Freshness,
                    format!(
                        "{what}: window_covered reaches the window end while the head is at {ts}"
                    ),
                );
            }
        }
    }
    if h.indexing_errors {
        fail(
            failures,
            Reason::Freshness,
            format!("{what}: indexing_errors is true"),
        );
    }
}

fn number_ok(text: &str) -> bool {
    Dec::parse(text).is_ok()
}

fn check_screen(
    what: &str,
    screen: Timed<'_, ScreenResponse>,
    rules: &Rules<'_>,
    failures: &mut Vec<Failure>,
) {
    let w = rules.window;
    check_header(what, &screen.body.header, screen.quoted_at, rules, failures);
    for (pool, p) in &screen.body.pools {
        if !rules.required.contains(pool) {
            fail(
                failures,
                Reason::Schema,
                format!("{what}: pool {pool} is not in R"),
            );
        }
        for (name, s) in [("start", &p.start), ("end", &p.end)] {
            let Some(s) = s else { continue };
            for (field, value) in [
                ("totalValueLockedToken0", Some(s.tvl_token0.as_str())),
                ("totalValueLockedToken1", Some(s.tvl_token1.as_str())),
                ("totalValueLockedUSD", s.tvl_usd.as_deref()),
                ("liquidity", Some(s.liquidity.as_str())),
                ("token0PriceUSD", s.token0_price_usd.as_deref()),
                ("token1PriceUSD", s.token1_price_usd.as_deref()),
            ] {
                if let Some(v) = value
                    && !number_ok(v)
                {
                    fail(
                        failures,
                        Reason::Schema,
                        format!("{what} {pool}: {name}.{field} {v:?} is not a number"),
                    );
                }
            }
        }
        for h in &p.hours {
            if h.period_start_unix < w.from || h.period_start_unix >= w.to {
                fail(
                    failures,
                    Reason::Schema,
                    format!(
                        "{what} {pool}: hour row at {} is outside {}..{}",
                        h.period_start_unix, w.from, w.to
                    ),
                );
            }
            if !number_ok(&h.volume_usd)
                || !number_ok(&h.tvl_usd)
                || h.tx_count.parse::<u64>().is_err()
            {
                fail(
                    failures,
                    Reason::Schema,
                    format!(
                        "{what} {pool}: hour row at {} has a malformed number",
                        h.period_start_unix
                    ),
                );
            }
        }
        for kind in EVENT_KINDS {
            if let Some(hit) = p.large_events.get(kind) {
                if !is_tx_hash(&hit.transaction.id) {
                    fail(
                        failures,
                        Reason::Schema,
                        format!(
                            "{what} {pool}: large_events.{} transaction {:?} is not a hash",
                            kind.as_str(),
                            hit.transaction.id
                        ),
                    );
                }
                if !number_ok(&hit.amount_usd) {
                    fail(
                        failures,
                        Reason::Schema,
                        format!(
                            "{what} {pool}: large_events.{} amountUSD {:?} is not a number",
                            kind.as_str(),
                            hit.amount_usd
                        ),
                    );
                }
            }
        }
        if p.absent_at_start != (p.start.is_none() && p.end.is_some()) {
            fail(
                failures,
                Reason::Schema,
                format!("{what} {pool}: absent_at_start disagrees with the snapshots"),
            );
        }
        // The seller's verdict must be what its own facts give.
        let mine = assess(p, rules.thresholds);
        if !mine
            .reasons
            .iter()
            .any(|r| r.starts_with("undetermined:bad_number:"))
            && (mine.verdict != p.verdict || mine.reasons != p.reasons)
        {
            fail(
                failures,
                Reason::Calculation,
                format!(
                    "{what} {pool}: seller verdict {:?} {:?} differs from the facts: {:?} {:?}",
                    p.verdict, p.reasons, mine.verdict, mine.reasons
                ),
            );
        }
    }
}

fn check_events(
    what: &str,
    events: Timed<'_, EventsResponse>,
    rules: &Rules<'_>,
    failures: &mut Vec<Failure>,
) {
    let w = rules.window;
    check_header(what, &events.body.header, events.quoted_at, rules, failures);
    for (pool, held) in &events.body.pools {
        if !rules.required.contains(pool) {
            fail(
                failures,
                Reason::Schema,
                format!("{what}: pool {pool} is not in R"),
            );
        }
        for (kind, x) in held.all() {
            if x.timestamp < w.from || x.timestamp >= w.to {
                fail(
                    failures,
                    Reason::Schema,
                    format!(
                        "{what} {pool}: {} {} at {} is outside {}..{}",
                        kind.as_str(),
                        x.id,
                        x.timestamp,
                        w.from,
                        w.to
                    ),
                );
            }
            if !is_tx_hash(&x.transaction.id) {
                fail(
                    failures,
                    Reason::Schema,
                    format!(
                        "{what} {pool}: {} {} transaction {:?} is not a hash",
                        kind.as_str(),
                        x.id,
                        x.transaction.id
                    ),
                );
            }
            if !number_ok(&x.amount0)
                || !number_ok(&x.amount1)
                || x.amount_usd.as_deref().is_some_and(|a| !number_ok(a))
            {
                fail(
                    failures,
                    Reason::Schema,
                    format!(
                        "{what} {pool}: {} {} has a malformed amount",
                        kind.as_str(),
                        x.id
                    ),
                );
            }
        }
        // The seller's aggregates must be what its own events give.
        let agg = aggregates(held);
        if agg.bad.is_empty() {
            for kind in EVENT_KINDS {
                let stated_sum = Dec::parse(held.sum(kind)).ok();
                if held.count(kind) != agg.counts[&kind]
                    || stated_sum.as_ref() != Some(&agg.sums[&kind])
                {
                    fail(
                        failures,
                        Reason::Calculation,
                        format!(
                            "{what} {pool}: stated {} count {} sum {} differ from the events: {} {}",
                            kind.as_str(),
                            held.count(kind),
                            held.sum(kind),
                            agg.counts[&kind],
                            agg.sums[&kind]
                        ),
                    );
                }
            }
            if held.amount_usd_nulls != agg.nulls {
                fail(
                    failures,
                    Reason::Calculation,
                    format!(
                        "{what} {pool}: stated amount_usd_nulls {} differs from the events: {}",
                        held.amount_usd_nulls, agg.nulls
                    ),
                );
            }
        }
    }
}

/// A screen hit must be among the held events with the same amount, no
/// held event may reach the threshold the screen missed, and the two
/// purchases must read one deployment.
fn check_view(view: &PoolView, rules: &Rules<'_>, failures: &mut Vec<Failure>) {
    let Some(ev) = &view.events else { return };
    let pool = &view.pool;
    if ev.header.deployment_id != view.screen.header.deployment_id {
        fail(
            failures,
            Reason::Schema,
            format!(
                "{pool}: events from {} read deployment {}, the screen from {} read {}",
                view.events_label.as_deref().unwrap_or("?"),
                ev.header.deployment_id,
                view.screen_label,
                view.screen.header.deployment_id
            ),
        );
    }
    let (Some(facts), Some(held)) = (view.screen.pools.get(pool), ev.pools.get(pool)) else {
        return;
    };
    for kind in EVENT_KINDS {
        if let Some(hit) = facts.large_events.get(kind)
            && !held.all().any(|(k, x)| {
                k == kind
                    && x.transaction.id == hit.transaction.id
                    && x.amount_usd.as_deref() == Some(hit.amount_usd.as_str())
            })
        {
            fail(
                failures,
                Reason::Calculation,
                format!(
                    "{pool}: the screen's {} hit {} is not among the held events with that amount",
                    kind.as_str(),
                    hit.transaction.id
                ),
            );
        }
    }
    if let Ok(min) = Dec::parse(rules.thresholds.min_event_usd) {
        for kind in EVENT_KINDS {
            let top = held
                .all()
                .filter(|(k, _)| *k == kind)
                .filter_map(|(_, x)| x.amount_usd.as_deref().and_then(|a| Dec::parse(a).ok()))
                .max();
            if let Some(top) = top
                && top >= min
                && facts.large_events.get(kind).is_none()
            {
                fail(
                    failures,
                    Reason::Calculation,
                    format!(
                        "{pool}: a held {} of {} reaches min_event_usd but the screen reports no hit",
                        kind.as_str(),
                        top
                    ),
                );
            }
        }
    }
}

/// Section 9 schema and freshness for whatever has been delivered so far:
/// the typed shape, the binding to the requested window, numeric fields that
/// parse, rows inside the window, the seller's own verdicts and aggregates
/// agreeing with its facts, and the covering purchases of each pool
/// agreeing with each other.
pub fn validate_deliveries(rules: &Rules<'_>, delivered: &Delivered<'_>) -> Vec<Failure> {
    let mut failures = Vec::new();
    for p in &delivered.purchases {
        if let Some(s) = p.screen {
            check_screen(&p.label, s, rules, &mut failures);
        }
        if let Some(e) = p.events {
            check_events(&p.label, e, rules, &mut failures);
        }
    }
    for v in delivered.views() {
        check_view(&v, rules, &mut failures);
    }
    failures
}

/// Numbers in prose: maximal digit runs with optional thousands commas and a
/// fraction, delimited by non-alphanumerics, with a `%` flag. Runs glued to
/// letters, such as `token0` or a hex hash, are not numbers.
pub fn prose_numbers(prose: &str) -> Vec<(String, bool)> {
    let chars: Vec<char> = prose.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_digit()
            || (i > 0
                && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '.' || chars[i - 1] == '_'))
        {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len()
            && (chars[i].is_ascii_digit()
                || chars[i] == ','
                || (chars[i] == '.' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit()))
        {
            i += 1;
        }
        if i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            continue;
        }
        let text: String = chars[start..i].iter().filter(|c| **c != ',').collect();
        let percent = i < chars.len() && chars[i] == '%';
        out.push((text, percent));
    }
    out
}

fn value_numbers(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Number(n) => out.push(n.to_string()),
        Value::String(s) => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|v| value_numbers(v, out)),
        Value::Object(o) => o.values().for_each(|v| value_numbers(v, out)),
        _ => {}
    }
}

/// Every number a report may state: claim values, brief forms, inputs, then every fact field.
fn allowed_numbers(claims: &[Claim], views: &[PoolView], extra: &[String]) -> Vec<Dec> {
    let mut raw: Vec<String> = extra.to_vec();
    for c in claims {
        for v in c.values.values() {
            value_numbers(v, &mut raw);
        }
    }
    for view in views {
        let h = &view.screen.header;
        raw.extend(
            [
                h.block_start,
                h.block_end,
                h.block_end_timestamp,
                h.indexed_block,
                h.window_requested.from,
                h.window_requested.to,
            ]
            .map(|n| n.to_string()),
        );
        for p in view.screen.pools.values() {
            for s in [&p.start, &p.end].into_iter().flatten() {
                raw.extend([
                    s.tvl_token0.clone(),
                    s.tvl_token1.clone(),
                    s.liquidity.clone(),
                ]);
                raw.extend(
                    [&s.tvl_usd, &s.token0_price_usd, &s.token1_price_usd]
                        .into_iter()
                        .flatten()
                        .cloned(),
                );
            }
            for r in &p.hours {
                raw.extend([
                    r.period_start_unix.to_string(),
                    r.tvl_usd.clone(),
                    r.volume_usd.clone(),
                    r.tx_count.clone(),
                ]);
            }
            raw.extend(
                EVENT_KINDS
                    .iter()
                    .filter_map(|k| p.large_events.get(*k))
                    .map(|h| h.amount_usd.clone()),
            );
        }
        if let Some(ev) = &view.events {
            for p in ev.pools.values() {
                for (_, e) in p.all() {
                    raw.extend([
                        e.amount0.clone(),
                        e.amount1.clone(),
                        e.timestamp.to_string(),
                    ]);
                    raw.extend(e.amount_usd.clone());
                    raw.extend(e.log_index.map(|v| v.to_string()));
                    raw.extend(e.tick_lower.map(|v| v.to_string()));
                    raw.extend(e.tick_upper.map(|v| v.to_string()));
                }
            }
        }
    }
    raw.iter().filter_map(|s| Dec::parse(s).ok()).collect()
}

fn sorted(mut claims: Vec<Claim>) -> Vec<Claim> {
    claims.sort_by(|a, b| (a.pool.as_str(), a.kind as u8).cmp(&(b.pool.as_str(), b.kind as u8)));
    claims
}

/// The report-wide checks of section 9 over the report's outcomes and claims,
/// the delivery checks included.
pub fn validate(
    rules: &Rules<'_>,
    delivered: &Delivered<'_>,
    outcomes: &[PoolOutcome],
    claims: &[Claim],
) -> Validation {
    let mut failures = validate_deliveries(rules, delivered);
    let views = delivered.views();
    let view_of = |pool: &str| views.iter().find(|v| v.pool == pool);

    // Coverage over R.
    let mut pending = Vec::new();
    let mut undetermined = Vec::new();
    let mut resolved = 0;
    for pool in rules.required {
        if view_of(pool).is_none() {
            fail(
                &mut failures,
                Reason::Schema,
                format!("{pool}: no purchase covers this pool"),
            );
        }
        match outcomes.iter().find(|o| &o.pool == pool) {
            None => fail(
                &mut failures,
                Reason::Coverage,
                format!("{pool}: no outcome"),
            ),
            Some(o) => match o.outcome {
                Outcome::Pending => pending.push(pool.clone()),
                Outcome::Undetermined => {
                    undetermined.push(format!("{pool} ({})", o.reasons.join(", ")))
                }
                Outcome::Supported | Outcome::NonMaterial => resolved += 1,
            },
        }
    }
    let complete =
        pending.is_empty() && undetermined.is_empty() && resolved == rules.required.len();
    if !complete && !rules.degrade {
        fail(
            &mut failures,
            Reason::Coverage,
            format!(
                "{resolved}/{} resolved; pending {:?}; undetermined {:?}",
                rules.required.len(),
                pending,
                undetermined
            ),
        );
    }

    // Calculations: recompute everything from the facts and require equality.
    let (again_outcomes, again_claims) = compute(delivered, rules.evidence, rules.thresholds);
    let mut calculations = 0;
    for c in claims {
        if again_claims.contains(c) {
            calculations += 1;
        } else {
            fail(
                &mut failures,
                Reason::Calculation,
                format!(
                    "{} {:?} does not re-evaluate to its values {}",
                    c.pool,
                    c.kind,
                    serde_json::to_string(&c.values).unwrap_or_default()
                ),
            );
        }
    }
    for o in outcomes {
        if !again_outcomes.contains(o) {
            fail(
                &mut failures,
                Reason::Calculation,
                format!(
                    "{}: outcome {:?} is not what the facts give",
                    o.pool, o.outcome
                ),
            );
        }
    }
    if sorted(again_claims.clone()).len() != claims.len() {
        fail(
            &mut failures,
            Reason::Calculation,
            format!(
                "{} claims reported, the facts give {}",
                claims.len(),
                again_claims.len()
            ),
        );
    }

    // Evidence references and permitted kinds.
    let mut references = 0;
    let mut transaction_citations = 0;
    for c in claims {
        if !rules.required.contains(&c.pool) {
            fail(
                &mut failures,
                Reason::Citation,
                format!("{}: claim about a pool outside R", c.pool),
            );
        }
        let kind_ok = match c.kind {
            ClaimType::TvlChange => {
                c.evidence.len() == 2 && c.evidence.iter().all(|e| is_fact_id(e))
            }
            ClaimType::LargeEvent | ClaimType::LargestEvent => {
                c.evidence.len() == 1 && is_tx_hash(&c.evidence[0])
            }
            ClaimType::ActivitySummary => {
                !c.evidence.is_empty() && c.evidence.iter().all(|e| is_fact_id(e))
            }
        };
        if !kind_ok {
            fail(
                &mut failures,
                Reason::Citation,
                format!(
                    "{} {:?}: evidence {:?} is not of the permitted kind",
                    c.pool, c.kind, c.evidence
                ),
            );
        }
        if c.evidence.is_empty() && rules.citations == Citations::Required {
            fail(
                &mut failures,
                Reason::Citation,
                format!("{} {:?}: no evidence reference", c.pool, c.kind),
            );
        }
        let Some(view) = view_of(&c.pool) else {
            fail(
                &mut failures,
                Reason::Citation,
                format!("{}: no purchase covers this claim's pool", c.pool),
            );
            continue;
        };
        for e in &c.evidence {
            let r = if is_tx_hash(e) {
                resolve_tx(e, view)
            } else {
                resolve_fact(e, view)
            };
            match r {
                Ok(()) => references += 1,
                Err(detail) => fail(&mut failures, Reason::Citation, detail),
            }
        }
        if !c.transaction_hashes().is_empty() {
            transaction_citations += 1;
        }
    }
    if rules.citations == Citations::Required {
        for o in outcomes.iter().filter(|o| o.outcome == Outcome::Supported) {
            let mine: Vec<&Claim> = claims.iter().filter(|c| c.pool == o.pool).collect();
            if mine.is_empty() {
                fail(
                    &mut failures,
                    Reason::Citation,
                    format!("{}: supported without a claim", o.pool),
                );
            }
            let held_events = view_of(&o.pool)
                .and_then(|v| v.events.as_ref())
                .and_then(|e| e.pools.get(&o.pool))
                .is_some_and(|p| p.all().next().is_some());
            if held_events && !mine.iter().any(|c| !c.transaction_hashes().is_empty()) {
                fail(
                    &mut failures,
                    Reason::Citation,
                    format!(
                        "{}: events held but no claim cites a transaction hash",
                        o.pool
                    ),
                );
            }
        }
    }

    // Prose.
    let mut prose_count = 0;
    if let Some(x) = delivered.explanation {
        let mut extra = rules.input_numbers.clone();
        extra.extend(rules.brief_numbers.iter().cloned());
        let allowed = allowed_numbers(claims, &views, &extra);
        let hundred = Dec::parse("100").expect("literal");
        for (text, percent) in prose_numbers(&x.prose) {
            let Ok(n) = Dec::parse(&text) else { continue };
            let found = allowed
                .iter()
                .any(|a| *a == n || (percent && a.mul(&hundred) == n));
            if found {
                prose_count += 1;
            } else {
                fail(
                    &mut failures,
                    Reason::Prose,
                    format!(
                        "{text}{} is not among claim or fact values",
                        if percent { "%" } else { "" }
                    ),
                );
            }
        }
    }

    // A non_material pool needs complete coverage of its facts.
    for o in outcomes
        .iter()
        .filter(|o| o.outcome == Outcome::NonMaterial)
    {
        if let Some(v) = view_of(&o.pool) {
            let pool_flags = v
                .screen
                .pools
                .get(&o.pool)
                .is_some_and(|p| p.truncated || p.coverage_shortfall);
            if v.screen.header.coverage_shortfall || v.screen.header.truncated || pool_flags {
                fail(
                    &mut failures,
                    Reason::Freshness,
                    format!(
                        "{}: non_material while coverage_shortfall or truncated",
                        o.pool
                    ),
                );
            }
        }
    }

    // Provenance: the choice is deterministic; the check comes later.
    let mut hashes: BTreeSet<(String, String)> = BTreeSet::new();
    for c in claims {
        for h in c.transaction_hashes() {
            hashes.insert((c.pool.clone(), h.to_owned()));
        }
    }
    let provenance = if hashes.is_empty() {
        Provenance::NotApplicable
    } else if rules.provenance_samples == 0 {
        Provenance::NotRun {
            cited: hashes.len(),
        }
    } else {
        Provenance::Pending {
            samples: hashes
                .into_iter()
                .take(rules.provenance_samples as usize)
                .collect(),
        }
    };

    Validation {
        passed: failures.is_empty(),
        complete,
        failures,
        coverage: CoverageResult {
            required: rules.required.len(),
            resolved,
            pending,
            undetermined,
        },
        calculations,
        references,
        transaction_citations,
        prose_numbers: prose_count,
        provenance,
    }
}

impl Validation {
    /// Applies checked samples: every sample must have passed.
    pub fn with_provenance(mut self, samples: Vec<Sample>) -> Self {
        for s in samples.iter().filter(|s| !s.ok) {
            fail(
                &mut self.failures,
                Reason::Provenance,
                format!("{}: {}", s.tx, s.detail),
            );
        }
        self.provenance = Provenance::Checked { samples };
        self.passed = self.failures.is_empty();
        self
    }

    /// The `rejected` reasons, distinct, in order of appearance.
    pub fn reasons(&self) -> Vec<Reason> {
        reasons_of(&self.failures)
    }
}

pub fn reasons_of(failures: &[Failure]) -> Vec<Reason> {
    let mut out: Vec<Reason> = Vec::new();
    for f in failures {
        if !out.contains(&f.reason) {
            out.push(f.reason);
        }
    }
    out
}

/// `eth_getTransactionReceipt` for each sample: the receipt exists, has
/// status 1, and lists the pool among its log addresses. Amounts are not
/// checked.
pub async fn check_provenance(
    http: &reqwest::Client,
    rpc: &str,
    samples: &[(String, String)],
) -> Vec<Sample> {
    let mut out = Vec::new();
    for (pool, tx) in samples {
        let body = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "eth_getTransactionReceipt", "params": [tx] });
        let result: Result<Value, String> = async {
            let resp = http
                .post(rpc)
                .json(&body)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let v: Value = resp.json().await.map_err(|e| e.to_string())?;
            if let Some(err) = v.get("error") {
                return Err(err.to_string());
            }
            Ok(v.get("result").cloned().unwrap_or(Value::Null))
        }
        .await;
        let (ok, detail) = match result {
            Err(e) => (false, format!("rpc: {e}")),
            Ok(Value::Null) => (false, "no receipt".to_owned()),
            Ok(receipt) => {
                let status = receipt.get("status").and_then(Value::as_str).unwrap_or("");
                let addresses: Vec<String> = receipt
                    .get("logs")
                    .and_then(Value::as_array)
                    .map(|logs| {
                        logs.iter()
                            .filter_map(|l| l.get("address").and_then(Value::as_str))
                            .map(str::to_lowercase)
                            .collect()
                    })
                    .unwrap_or_default();
                let involved = addresses.iter().any(|a| a == &pool.to_lowercase());
                match (status == "0x1", involved) {
                    (true, true) => (
                        true,
                        format!("status 1, pool among {} log addresses", addresses.len()),
                    ),
                    (false, _) => (false, format!("status {status:?}")),
                    (true, false) => (false, "pool not among the log addresses".to_owned()),
                }
            }
        };
        out.push(Sample {
            pool: pool.clone(),
            tx: tx.clone(),
            ok,
            detail,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FROM, POOL, TO, events_fixture, screen_fixture};
    use time::Duration;

    const TX: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";
    const T: Thresholds<'static> = Thresholds {
        materiality: "0.05",
        min_event_usd: "100000",
    };

    fn quoted_at() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(TO as i64 + 120).unwrap()
    }

    fn rules(required: &[String]) -> Rules<'_> {
        Rules {
            required,
            window: Window { from: FROM, to: TO },
            evidence: Evidence::Transaction,
            citations: Citations::Required,
            max_data_age_s: 3600,
            provenance_samples: 3,
            degrade: false,
            thresholds: T,
            input_numbers: vec!["24".to_owned(), "1".to_owned()],
            brief_numbers: vec!["1100".to_owned()],
        }
    }

    fn with_real_hash(
        mut s: ScreenResponse,
        mut e: EventsResponse,
    ) -> (ScreenResponse, EventsResponse) {
        if let Some(h) = s.pools.get_mut(POOL).unwrap().large_events.swap.as_mut() {
            h.transaction.id = TX.to_owned();
        }
        e.pools.get_mut(POOL).unwrap().swaps[0].transaction.id = TX.to_owned();
        (s, e)
    }

    fn pair() -> (ScreenResponse, EventsResponse) {
        with_real_hash(
            screen_fixture(true, true),
            events_fixture(Some("250000"), false),
        )
    }

    /// A screen purchase then an events purchase, each with its own quote time.
    fn delivered<'a>(
        screen: &'a ScreenResponse,
        events: Option<&'a EventsResponse>,
        explanation: Option<&'a Explanation>,
    ) -> Delivered<'a> {
        let mut d = Delivered {
            purchases: vec![Purchased {
                label: "screen".into(),
                screen: Some(Timed {
                    body: screen,
                    quoted_at: quoted_at(),
                }),
                events: None,
            }],
            explanation,
        };
        if let Some(e) = events {
            d.purchases.push(Purchased {
                label: "events".into(),
                screen: None,
                events: Some(Timed {
                    body: e,
                    quoted_at: quoted_at() + Duration::seconds(30),
                }),
            });
        }
        d
    }

    #[test]
    fn a_supported_pool_with_events_passes_every_local_check() {
        let (screen, events) = pair();
        let required = vec![POOL.to_owned()];
        let d = delivered(&screen, Some(&events), None);
        let (o, c) = compute(&d, Evidence::Transaction, T);
        let prose = Explanation {
            prose: format!(
                "Pool {POOL} is supported: token0 TVL moved from 1,000 to 1100, a 10% change, on 250000 USD of swaps over 24 hours."
            ),
            model: "template".into(),
            input_bytes: 1,
        };
        let d = delivered(&screen, Some(&events), Some(&prose));
        assert!(validate_deliveries(&rules(&required), &d).is_empty());
        let v = validate(&rules(&required), &d, &o, &c);
        assert!(v.passed, "{:?}", v.failures);
        assert!(v.complete);
        assert_eq!(
            (
                v.calculations,
                v.references,
                v.transaction_citations,
                v.prose_numbers
            ),
            (4, 7, 1, 5)
        );
        assert_eq!(
            v.provenance,
            Provenance::Pending {
                samples: vec![(POOL.to_owned(), TX.to_owned())]
            }
        );
        let checked = v.with_provenance(vec![Sample {
            pool: POOL.into(),
            tx: TX.into(),
            ok: true,
            detail: "status 1".into(),
        }]);
        assert!(checked.passed);
        let failed = checked.with_provenance(vec![Sample {
            pool: POOL.into(),
            tx: TX.into(),
            ok: false,
            detail: "no receipt".into(),
        }]);
        assert_eq!(
            (failed.passed, failed.reasons()),
            (false, vec![Reason::Provenance])
        );
    }

    #[test]
    fn a_non_material_pool_is_complete_with_provenance_not_applicable() {
        let screen = screen_fixture(false, false);
        let required = vec![POOL.to_owned()];
        let d = delivered(&screen, None, None);
        let (o, c) = compute(&d, Evidence::Transaction, T);
        let v = validate(&rules(&required), &d, &o, &c);
        assert!(v.passed, "{:?}", v.failures);
        assert_eq!(v.provenance, Provenance::NotApplicable);
        assert_eq!(v.calculations, 3);
        let mut zero = rules(&required);
        zero.provenance_samples = 0;
        let (screen2, events2) = pair();
        let d2 = delivered(&screen2, Some(&events2), None);
        let (o2, c2) = compute(&d2, Evidence::Transaction, T);
        assert_eq!(
            validate(&zero, &d2, &o2, &c2).provenance,
            Provenance::NotRun { cited: 1 }
        );
    }

    #[test]
    fn evidence_is_bound_to_the_requested_window() {
        // Probe: a screen declaring 0..3600 for a run over the last 24 hours.
        let (mut screen, events) = pair();
        screen.header.window_requested = Window { from: 0, to: 3600 };
        screen.header.window_covered = Window { from: 0, to: 3600 };
        let required = vec![POOL.to_owned()];
        let f = validate_deliveries(&rules(&required), &delivered(&screen, Some(&events), None));
        assert!(
            f.iter().any(
                |f| f.reason == Reason::Schema && f.detail.contains("window_requested 0..3600")
            ),
            "{f:?}"
        );
        // Rows outside the window, and a hidden shortfall.
        let (mut screen, mut events) = pair();
        screen.pools.get_mut(POOL).unwrap().hours[0].period_start_unix = FROM - 3600;
        events.pools.get_mut(POOL).unwrap().swaps[0].timestamp = TO + 1;
        let f = validate_deliveries(&rules(&required), &delivered(&screen, Some(&events), None));
        assert!(
            f.iter().any(|f| f.detail.contains("hour row at"))
                && f.iter().any(|f| f.detail.contains("is outside")),
            "{f:?}"
        );
        let (mut screen, _) = pair();
        screen.header.indexed_block_timestamp = Some(TO - 600);
        let f = validate_deliveries(&rules(&required), &delivered(&screen, None, None));
        assert!(
            f.iter().any(|f| f.reason == Reason::Freshness
                && f.detail.contains("without coverage_shortfall")),
            "{f:?}"
        );
    }

    #[test]
    fn seller_assertions_are_checked_against_their_facts() {
        let required = vec![POOL.to_owned()];
        let (mut screen, events) = pair();
        screen.pools.get_mut(POOL).unwrap().verdict = crate::evidence::Verdict::NonMaterial;
        let f = validate_deliveries(&rules(&required), &delivered(&screen, Some(&events), None));
        assert!(
            f.iter()
                .any(|f| f.reason == Reason::Calculation && f.detail.contains("seller verdict")),
            "{f:?}"
        );
        let (screen, mut events) = pair();
        events.pools.get_mut(POOL).unwrap().counts.swap = 999;
        let f = validate_deliveries(&rules(&required), &delivered(&screen, Some(&events), None));
        assert!(
            f.iter()
                .any(|f| f.reason == Reason::Calculation
                    && f.detail.contains("stated swap count 999")),
            "{f:?}"
        );
        let (mut screen, events) = pair();
        screen
            .pools
            .get_mut(POOL)
            .unwrap()
            .end
            .as_mut()
            .unwrap()
            .tvl_token0 = "1,100".to_owned();
        let f = validate_deliveries(&rules(&required), &delivered(&screen, Some(&events), None));
        assert!(
            f.iter()
                .any(|f| f.reason == Reason::Schema && f.detail.contains("is not a number")),
            "{f:?}"
        );
        let (screen, mut events) = pair();
        events.pools.get_mut(POOL).unwrap().swaps[0].amount_usd = Some("250001".to_owned());
        events.pools.get_mut(POOL).unwrap().sum_amount_usd.swap = "250001".to_owned();
        let f = validate_deliveries(&rules(&required), &delivered(&screen, Some(&events), None));
        assert!(
            f.iter().any(|f| f
                .detail
                .contains("is not among the held events with that amount")),
            "{f:?}"
        );
    }

    #[test]
    fn each_pool_keeps_the_header_of_the_purchase_that_covers_it() {
        use crate::testing::{events_for, screen_for};
        let w = Window { from: FROM, to: TO };
        let quiet = "0x1111111111111111111111111111111111111111".to_owned();
        let hot = "0x2222222222222222222222222222222222222222".to_owned();
        let required = vec![quiet.clone(), hot.clone()];
        // Purchase 1: a screen over both pools at head 25_000_000.
        let first = screen_for(
            &[(quiet.clone(), false), (hot.clone(), true)],
            w,
            25_000_000,
        );
        // Purchase 2: a bundle over the hot pool at a later head, different blocks.
        let bundle_screen = screen_for(&[(hot.clone(), true)], w, 25_000_500);
        let bundle_events = events_for(std::slice::from_ref(&hot), w, 25_000_500);
        // The bundle's screen hit is among its own events.
        assert_eq!(
            bundle_screen.pools[&hot]
                .large_events
                .swap
                .as_ref()
                .unwrap()
                .transaction
                .id,
            bundle_events.pools[&hot].swaps[0].transaction.id
        );
        let d = Delivered {
            purchases: vec![
                Purchased {
                    label: "screen".into(),
                    screen: Some(Timed {
                        body: &first,
                        quoted_at: quoted_at(),
                    }),
                    events: None,
                },
                Purchased {
                    label: "investigate".into(),
                    screen: Some(Timed {
                        body: &bundle_screen,
                        quoted_at: quoted_at() + Duration::seconds(40),
                    }),
                    events: Some(Timed {
                        body: &bundle_events,
                        quoted_at: quoted_at() + Duration::seconds(40),
                    }),
                },
            ],
            explanation: None,
        };
        let blocks = d.blocks();
        assert_eq!(blocks[&quiet], (25_000_000 - 7100, 25_000_000 - 100));
        assert_eq!(
            blocks[&hot],
            (25_000_500 - 7100, 25_000_500 - 100),
            "the hot pool carries the bundle's blocks"
        );
        let (o, c) = compute(&d, Evidence::Transaction, T);
        assert_eq!(
            o.iter()
                .map(|o| (o.pool.as_str(), o.outcome))
                .collect::<Vec<_>>(),
            vec![
                (quiet.as_str(), Outcome::NonMaterial),
                (hot.as_str(), Outcome::Supported)
            ]
        );
        let hot_tvl = c
            .iter()
            .find(|c| c.pool == hot && c.kind == ClaimType::TvlChange)
            .unwrap();
        assert!(
            hot_tvl.evidence[0].ends_with(&format!("@{}", 25_000_500 - 7100)),
            "{:?}",
            hot_tvl.evidence
        );
        let v = validate(&rules(&required), &d, &o, &c);
        assert!(v.passed, "{:?}", v.failures);
        assert_eq!(v.coverage.resolved, 2);
        // A claim citing the first screen's block for the hot pool no longer resolves.
        let mut wrong = c.clone();
        let idx = wrong
            .iter()
            .position(|c| c.pool == hot && c.kind == ClaimType::TvlChange)
            .unwrap();
        wrong[idx].evidence[0] = format!("{hot}:totalValueLockedToken0@{}", 25_000_000 - 7100);
        let v = validate(&rules(&required), &d, &o, &wrong);
        assert!(v.reasons().contains(&Reason::Citation), "{:?}", v.failures);
    }

    #[test]
    fn each_failure_is_named() {
        let (screen, events) = pair();
        let required = vec![POOL.to_owned()];
        let d = delivered(&screen, Some(&events), None);
        let (o, c) = compute(&d, Evidence::Transaction, T);

        // coverage: a pending pool without degrade.
        let d_pending = delivered(&screen, None, None);
        let (po, pc) = compute(&d_pending, Evidence::Transaction, T);
        let v = validate(&rules(&required), &d_pending, &po, &pc);
        assert_eq!(v.reasons(), vec![Reason::Coverage]);
        let mut degrade = rules(&required);
        degrade.degrade = true;
        let v = validate(&degrade, &d_pending, &po, &pc);
        assert!(v.passed && !v.complete);

        // calculation: a tampered value.
        let mut tampered = c.clone();
        tampered[0]
            .values
            .insert("change".into(), serde_json::json!("0.2"));
        assert_eq!(
            validate(&rules(&required), &d, &o, &tampered).reasons(),
            vec![Reason::Calculation]
        );

        // citation: a hash not among the purchased events.
        let mut foreign = c.clone();
        let other = "0x2222222222222222222222222222222222222222222222222222222222222222";
        foreign[2].evidence = vec![other.to_owned()];
        assert_eq!(
            validate(&rules(&required), &d, &o, &foreign).reasons(),
            vec![Reason::Calculation, Reason::Citation]
        );

        // prose: a number from nowhere.
        let prose = Explanation {
            prose: "Volume was 999 USD.".into(),
            model: "m".into(),
            input_bytes: 1,
        };
        let d_prose = delivered(&screen, Some(&events), Some(&prose));
        let v = validate(&rules(&required), &d_prose, &o, &c);
        assert_eq!(v.reasons(), vec![Reason::Prose]);
        assert!(v.failures[0].detail.starts_with("999 "));

        // freshness: stale head, and non_material under a shortfall.
        let mut d_stale = delivered(&screen, Some(&events), None);
        d_stale.purchases[0].screen.as_mut().unwrap().quoted_at = quoted_at() + Duration::hours(2);
        assert_eq!(
            validate(&rules(&required), &d_stale, &o, &c).reasons(),
            vec![Reason::Freshness]
        );
        let mut short = screen_fixture(false, false);
        short.header.coverage_shortfall = true;
        short.header.window_covered.to = TO - 3600;
        short.header.indexed_block_timestamp = Some(TO - 3600);
        short.pools.get_mut(POOL).unwrap().coverage_shortfall = true;
        short.pools.get_mut(POOL).unwrap().verdict = crate::evidence::Verdict::Undetermined;
        short.pools.get_mut(POOL).unwrap().reasons =
            vec!["undetermined:coverage_shortfall".to_owned()];
        let d_short = delivered(&short, None, None);
        let (so, sc) = compute(&d_short, Evidence::Transaction, T);
        assert_eq!(so[0].outcome, Outcome::Undetermined);
        let mut forced = so.clone();
        forced[0].outcome = Outcome::NonMaterial;
        let v = validate(&rules(&required), &d_short, &forced, &sc);
        assert!(
            v.reasons().contains(&Reason::Freshness) && v.reasons().contains(&Reason::Calculation),
            "{:?}",
            v.reasons()
        );

        // schema: a required pool no purchase covers.
        let two = vec![
            POOL.to_owned(),
            "0x0000000000000000000000000000000000000001".to_owned(),
        ];
        let v = validate(&rules(&two), &d, &o, &c);
        assert!(v.reasons().contains(&Reason::Schema));
    }

    #[test]
    fn prose_numbers_skip_identifiers_and_keep_percents() {
        let found = prose_numbers(
            "Pool 0x88e6a0c2ddd26feeb64f039a2c41296fcb3f5640 token0 rose 10% from 1,000.5 to 1100 in 24h; tx 0xabc1.",
        );
        assert_eq!(
            found,
            vec![
                ("10".to_owned(), true),
                ("1000.5".to_owned(), false),
                ("1100".to_owned(), false)
            ]
        );
    }
}
