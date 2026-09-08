//! Spec section 9: result validation. Every check is named by its failure
//! reason: `coverage`, `calculation`, `citation`, `prose`, `provenance`,
//! `freshness`, `schema`. The local checks are pure; provenance needs an
//! Ethereum JSON-RPC and is applied afterwards with [`Validation::with_provenance`].

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

use crate::analysis::{Claim, ClaimType, Outcome, PoolOutcome, outcomes_and_claims};
use crate::evidence::{Dec, EventsResponse, Explanation, Header, ScreenResponse};
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

/// What the run bought, as typed responses.
#[derive(Debug, Clone, Copy)]
pub struct Delivered<'a> {
    pub screen: Timed<'a, ScreenResponse>,
    pub events: Option<Timed<'a, EventsResponse>>,
    pub explanation: Option<&'a Explanation>,
}

/// The mandate's requirements that validation reads.
#[derive(Debug, Clone)]
pub struct Rules<'a> {
    /// `R`, lowercased pool addresses.
    pub required: &'a [String],
    pub evidence: Evidence,
    pub citations: Citations,
    pub max_data_age_s: u64,
    pub provenance_samples: u32,
    pub degrade: bool,
    pub min_event_usd: &'a str,
    /// Numbers a model may state that are inputs rather than facts: window hours and pool count.
    pub input_numbers: Vec<String>,
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

/// Whether a fact id names a purchased fact.
fn resolve_fact(
    id: &str,
    pool: &str,
    screen: &ScreenResponse,
    events: Option<&EventsResponse>,
) -> Result<(), String> {
    let (id_pool, rest) = id
        .split_once(':')
        .ok_or_else(|| format!("{id}: not a fact id"))?;
    let (field, at) = rest
        .split_once('@')
        .ok_or_else(|| format!("{id}: not a fact id"))?;
    if id_pool != pool {
        return Err(format!("{id}: names pool {id_pool}, claim is about {pool}"));
    }
    let facts = screen
        .pools
        .get(pool)
        .ok_or_else(|| format!("{id}: pool not in the screen"))?;
    let h = &screen.header;
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
                    "{id}: block {block} is neither {} nor {}",
                    h.block_start, h.block_end
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
            let ev = events.ok_or_else(|| format!("{id}: no events held"))?;
            let window = format!(
                "{}-{}",
                ev.header.window_requested.from, ev.header.window_requested.to
            );
            if at != window {
                return Err(format!("{id}: events window is {window}"));
            }
            ev.pools
                .contains_key(pool)
                .then_some(())
                .ok_or_else(|| format!("{id}: pool not in the events"))
        }
        other => Err(format!("{id}: unknown field {other}")),
    }
}

fn resolve_tx(
    tx: &str,
    pool: &str,
    screen: &ScreenResponse,
    events: Option<&EventsResponse>,
) -> Result<(), String> {
    if let Some(facts) = screen.pools.get(pool)
        && [
            &facts.large_events.swap,
            &facts.large_events.mint,
            &facts.large_events.burn,
        ]
        .iter()
        .any(|h| h.as_ref().is_some_and(|h| h.transaction.id == tx))
    {
        return Ok(());
    }
    if let Some(ev) = events
        && let Some(held) = ev.pools.get(pool)
        && held.all().any(|(_, e)| e.transaction.id == tx)
    {
        return Ok(());
    }
    Err(format!("{tx}: not among the purchased events of {pool}"))
}

fn header_fresh(
    what: &str,
    h: &Header,
    quoted_at: OffsetDateTime,
    max_age: u64,
    failures: &mut Vec<Failure>,
) {
    match h.indexed_block_timestamp {
        None => failures.push(Failure {
            reason: Reason::Freshness,
            detail: format!("{what}: indexed_block_timestamp is null"),
        }),
        Some(ts) => {
            let age = quoted_at.unix_timestamp() - ts as i64;
            if age > max_age as i64 {
                failures.push(Failure {
                    reason: Reason::Freshness,
                    detail: format!("{what}: indexed block is {age} s older than the quote, max_data_age_s is {max_age}"),
                });
            }
        }
    }
    if h.indexing_errors {
        failures.push(Failure {
            reason: Reason::Freshness,
            detail: format!("{what}: indexing_errors is true"),
        });
    }
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

/// Every number a report may state: claim values, then every fact field.
fn allowed_numbers(
    claims: &[Claim],
    screen: &ScreenResponse,
    events: Option<&EventsResponse>,
    extra: &[String],
) -> Vec<Dec> {
    let mut raw: Vec<String> = extra.to_vec();
    for c in claims {
        for v in c.values.values() {
            value_numbers(v, &mut raw);
        }
    }
    let h = &screen.header;
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
    for p in screen.pools.values() {
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
            [
                &p.large_events.swap,
                &p.large_events.mint,
                &p.large_events.burn,
            ]
            .into_iter()
            .flatten()
            .map(|h| h.amount_usd.clone()),
        );
    }
    if let Some(ev) = events {
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
            raw.extend(
                [
                    p.counts.swap,
                    p.counts.mint,
                    p.counts.burn,
                    p.amount_usd_nulls,
                ]
                .map(|n| n.to_string()),
            );
            raw.extend([
                p.sum_amount_usd.swap.clone(),
                p.sum_amount_usd.mint.clone(),
                p.sum_amount_usd.burn.clone(),
            ]);
        }
    }
    raw.iter().filter_map(|s| Dec::parse(s).ok()).collect()
}

/// The local checks of section 9 over the report's outcomes and claims.
pub fn validate(
    rules: &Rules<'_>,
    delivered: &Delivered<'_>,
    outcomes: &[PoolOutcome],
    claims: &[Claim],
) -> Validation {
    let mut failures = Vec::new();
    let screen = delivered.screen.body;
    let events = delivered.events.map(|e| e.body);

    // Schema: the typed parse already happened; the shape must cover the request.
    if screen.header.deployment_id.is_empty() {
        failures.push(Failure {
            reason: Reason::Schema,
            detail: "screen: deployment_id is empty".to_owned(),
        });
    }
    if screen.header.block_end < screen.header.block_start {
        failures.push(Failure {
            reason: Reason::Schema,
            detail: "screen: block_end before block_start".to_owned(),
        });
    }
    for pool in rules.required {
        if !screen.pools.contains_key(pool) {
            failures.push(Failure {
                reason: Reason::Schema,
                detail: format!("screen: pool {pool} missing from the response"),
            });
        }
    }
    if let Some(ev) = events {
        if ev.header.deployment_id != screen.header.deployment_id {
            failures.push(Failure {
                reason: Reason::Schema,
                detail: "events: deployment differs from the screen".to_owned(),
            });
        }
        if ev.header.window_requested != screen.header.window_requested {
            failures.push(Failure {
                reason: Reason::Schema,
                detail: "events: window differs from the screen".to_owned(),
            });
        }
    }

    // Coverage over R.
    let mut pending = Vec::new();
    let mut undetermined = Vec::new();
    let mut resolved = 0;
    for pool in rules.required {
        match outcomes.iter().find(|o| &o.pool == pool) {
            None => failures.push(Failure {
                reason: Reason::Coverage,
                detail: format!("{pool}: no outcome"),
            }),
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
        failures.push(Failure {
            reason: Reason::Coverage,
            detail: format!(
                "{resolved}/{} resolved; pending {:?}; undetermined {:?}",
                rules.required.len(),
                pending,
                undetermined
            ),
        });
    }

    // Calculations: recompute everything from the facts and require equality.
    let (again_outcomes, again_claims) =
        outcomes_and_claims(screen, events, rules.evidence, rules.min_event_usd);
    let mut calculations = 0;
    for c in claims {
        if again_claims.contains(c) {
            calculations += 1;
        } else {
            failures.push(Failure {
                reason: Reason::Calculation,
                detail: format!(
                    "{} {:?} does not re-evaluate to its values {}",
                    c.pool,
                    c.kind,
                    serde_json::to_string(&c.values).unwrap_or_default()
                ),
            });
        }
    }
    for o in outcomes {
        if !again_outcomes.contains(o) {
            failures.push(Failure {
                reason: Reason::Calculation,
                detail: format!(
                    "{}: outcome {:?} is not what the facts give",
                    o.pool, o.outcome
                ),
            });
        }
    }
    if again_claims.len() != claims.len() {
        failures.push(Failure {
            reason: Reason::Calculation,
            detail: format!(
                "{} claims reported, the facts give {}",
                claims.len(),
                again_claims.len()
            ),
        });
    }

    // Evidence references and permitted kinds.
    let mut references = 0;
    let mut transaction_citations = 0;
    for c in claims {
        if !rules.required.contains(&c.pool) {
            failures.push(Failure {
                reason: Reason::Citation,
                detail: format!("{}: claim about a pool outside R", c.pool),
            });
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
            failures.push(Failure {
                reason: Reason::Citation,
                detail: format!(
                    "{} {:?}: evidence {:?} is not of the permitted kind",
                    c.pool, c.kind, c.evidence
                ),
            });
        }
        if c.evidence.is_empty() && rules.citations == Citations::Required {
            failures.push(Failure {
                reason: Reason::Citation,
                detail: format!("{} {:?}: no evidence reference", c.pool, c.kind),
            });
        }
        for e in &c.evidence {
            let r = if is_tx_hash(e) {
                resolve_tx(e, &c.pool, screen, events)
            } else {
                resolve_fact(e, &c.pool, screen, events)
            };
            match r {
                Ok(()) => references += 1,
                Err(detail) => failures.push(Failure {
                    reason: Reason::Citation,
                    detail,
                }),
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
                failures.push(Failure {
                    reason: Reason::Citation,
                    detail: format!("{}: supported without a claim", o.pool),
                });
            }
            let held_events = events
                .and_then(|e| e.pools.get(&o.pool))
                .is_some_and(|p| p.all().next().is_some());
            if held_events && !mine.iter().any(|c| !c.transaction_hashes().is_empty()) {
                failures.push(Failure {
                    reason: Reason::Citation,
                    detail: format!(
                        "{}: events held but no claim cites a transaction hash",
                        o.pool
                    ),
                });
            }
        }
    }

    // Prose.
    let mut prose_count = 0;
    if let Some(x) = delivered.explanation {
        let allowed = allowed_numbers(claims, screen, events, &rules.input_numbers);
        for (text, percent) in prose_numbers(&x.prose) {
            let Ok(n) = Dec::parse(&text) else { continue };
            let hundred = Dec::parse("100").expect("literal");
            let found = allowed.iter().any(|a| {
                a.cmp(&n) == std::cmp::Ordering::Equal
                    || (percent && a.mul(&hundred).cmp(&n) == std::cmp::Ordering::Equal)
            });
            if found {
                prose_count += 1;
            } else {
                failures.push(Failure {
                    reason: Reason::Prose,
                    detail: format!(
                        "{text}{} is not among claim or fact values",
                        if percent { "%" } else { "" }
                    ),
                });
            }
        }
    }

    // Freshness.
    header_fresh(
        "screen",
        &screen.header,
        delivered.screen.quoted_at,
        rules.max_data_age_s,
        &mut failures,
    );
    if let Some(ev) = delivered.events {
        header_fresh(
            "events",
            &ev.body.header,
            ev.quoted_at,
            rules.max_data_age_s,
            &mut failures,
        );
    }
    for o in outcomes
        .iter()
        .filter(|o| o.outcome == Outcome::NonMaterial)
    {
        let pool_flags = screen
            .pools
            .get(&o.pool)
            .is_some_and(|p| p.truncated || p.coverage_shortfall);
        if screen.header.coverage_shortfall || screen.header.truncated || pool_flags {
            failures.push(Failure {
                reason: Reason::Freshness,
                detail: format!(
                    "{}: non_material while coverage_shortfall or truncated",
                    o.pool
                ),
            });
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
            self.failures.push(Failure {
                reason: Reason::Provenance,
                detail: format!("{}: {}", s.tx, s.detail),
            });
        }
        self.provenance = Provenance::Checked { samples };
        self.passed = self.failures.is_empty();
        self
    }

    /// The `rejected` reasons, distinct, in section 2.7 order.
    pub fn reasons(&self) -> Vec<Reason> {
        let mut out: Vec<Reason> = Vec::new();
        for f in &self.failures {
            if !out.contains(&f.reason) {
                out.push(f.reason);
            }
        }
        out
    }
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
    use crate::testing::{POOL, TO, events_fixture, screen_fixture};
    use time::Duration;

    const TX: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";

    fn quoted_at() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(TO as i64 + 120).unwrap()
    }

    fn rules(required: &[String]) -> Rules<'_> {
        Rules {
            required,
            evidence: Evidence::Transaction,
            citations: Citations::Required,
            max_data_age_s: 3600,
            provenance_samples: 3,
            degrade: false,
            min_event_usd: "100000",
            input_numbers: vec!["24".to_owned(), "1".to_owned()],
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

    #[test]
    fn a_supported_pool_with_events_passes_every_local_check() {
        let (screen, events) = with_real_hash(
            screen_fixture(true, true),
            events_fixture(Some("250000"), false),
        );
        let required = vec![POOL.to_owned()];
        let (o, c) = outcomes_and_claims(&screen, Some(&events), Evidence::Transaction, "100000");
        let prose = Explanation {
            prose: format!(
                "Pool {POOL} is supported: token0 TVL moved from 1,000 to 1100, a 10% change, on 250000 USD of swaps over 24 hours."
            ),
            model: "template".into(),
            input_bytes: 1,
        };
        let d = Delivered {
            screen: Timed {
                body: &screen,
                quoted_at: quoted_at(),
            },
            events: Some(Timed {
                body: &events,
                quoted_at: quoted_at(),
            }),
            explanation: Some(&prose),
        };
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
        let (o, c) = outcomes_and_claims(&screen, None, Evidence::Transaction, "100000");
        let d = Delivered {
            screen: Timed {
                body: &screen,
                quoted_at: quoted_at(),
            },
            events: None,
            explanation: None,
        };
        let v = validate(&rules(&required), &d, &o, &c);
        assert!(v.passed, "{:?}", v.failures);
        assert_eq!(v.provenance, Provenance::NotApplicable);
        assert_eq!(v.calculations, 3);
        let mut zero = rules(&required);
        zero.provenance_samples = 0;
        let (screen2, events2) = with_real_hash(
            screen_fixture(true, true),
            events_fixture(Some("250000"), false),
        );
        let (o2, c2) =
            outcomes_and_claims(&screen2, Some(&events2), Evidence::Transaction, "100000");
        let d2 = Delivered {
            screen: Timed {
                body: &screen2,
                quoted_at: quoted_at(),
            },
            events: Some(Timed {
                body: &events2,
                quoted_at: quoted_at(),
            }),
            explanation: None,
        };
        assert_eq!(
            validate(&zero, &d2, &o2, &c2).provenance,
            Provenance::NotRun { cited: 1 }
        );
    }

    #[test]
    fn each_failure_is_named() {
        let (screen, events) = with_real_hash(
            screen_fixture(true, true),
            events_fixture(Some("250000"), false),
        );
        let required = vec![POOL.to_owned()];
        let (o, c) = outcomes_and_claims(&screen, Some(&events), Evidence::Transaction, "100000");
        let d = Delivered {
            screen: Timed {
                body: &screen,
                quoted_at: quoted_at(),
            },
            events: Some(Timed {
                body: &events,
                quoted_at: quoted_at(),
            }),
            explanation: None,
        };

        // coverage: a pending pool without degrade.
        let (po, pc) = outcomes_and_claims(&screen, None, Evidence::Transaction, "100000");
        let d_pending = Delivered { events: None, ..d };
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
        let d_prose = Delivered {
            explanation: Some(&prose),
            ..d
        };
        let v = validate(&rules(&required), &d_prose, &o, &c);
        assert_eq!(v.reasons(), vec![Reason::Prose]);
        assert!(v.failures[0].detail.starts_with("999 "));

        // freshness: stale head, and non_material under a shortfall.
        let d_stale = Delivered {
            screen: Timed {
                body: &screen,
                quoted_at: quoted_at() + Duration::hours(2),
            },
            ..d
        };
        assert_eq!(
            validate(&rules(&required), &d_stale, &o, &c).reasons(),
            vec![Reason::Freshness]
        );
        let mut short = screen_fixture(false, false);
        short.header.coverage_shortfall = true;
        let (so, sc) = outcomes_and_claims(&short, None, Evidence::Transaction, "100000");
        let mut forced = so.clone();
        forced[0].outcome = Outcome::NonMaterial;
        let d_short = Delivered {
            screen: Timed {
                body: &short,
                quoted_at: quoted_at(),
            },
            events: None,
            explanation: None,
        };
        let v = validate(&rules(&required), &d_short, &forced, &sc);
        assert!(
            v.reasons().contains(&Reason::Freshness) && v.reasons().contains(&Reason::Calculation),
            "{:?}",
            v.reasons()
        );

        // schema: a required pool the seller did not return.
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
