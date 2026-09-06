use chrono::{DateTime, Utc};
use serde::{de, ser, Deserialize, Serializer, Serialize};

#[derive(Debug, Clone, PartialEq)]
pub enum PoolOutcome {
    NonMaterial,
    Pending,
    Supported,
    Undetermined,
}

impl ser::Serialize for PoolOutcome {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            PoolOutcome::NonMaterial => serializer.serialize_str("non_material"),
            PoolOutcome::Pending => serializer.serialize_str("pending"),
            PoolOutcome::Supported => serializer.serialize_str("supported"),
            PoolOutcome::Undetermined => serializer.serialize_str("undetermined"),
        }
    }
}

impl<'de> de::Deserialize<'de> for PoolOutcome {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "non_material" => Ok(PoolOutcome::NonMaterial),
            "pending" => Ok(PoolOutcome::Pending),
            "supported" => Ok(PoolOutcome::Supported),
            "undetermined" => Ok(PoolOutcome::Undetermined),
            _ => Err(de::Error::unknown_variant(&s, &[
                "non_material",
                "pending",
                "supported",
                "undetermined",
            ])),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClaimType {
    TvlChange,
    LargeEvent,
    ActivitySummary,
}

impl ser::Serialize for ClaimType {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            ClaimType::TvlChange => serializer.serialize_str("tvl_change"),
            ClaimType::LargeEvent => serializer.serialize_str("large_event"),
            ClaimType::ActivitySummary => serializer.serialize_str("activity_summary"),
        }
    }
}

impl<'de> de::Deserialize<'de> for ClaimType {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "tvl_change" => Ok(ClaimType::TvlChange),
            "large_event" => Ok(ClaimType::LargeEvent),
            "activity_summary" => Ok(ClaimType::ActivitySummary),
            _ => Err(de::Error::unknown_variant(&s, &[
                "tvl_change",
                "large_event",
                "activity_summary",
            ])),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claim {
    pub claim_type: ClaimType,
    pub pool: String,
    pub values: Vec<String>,
    pub calculation: String,
    pub evidence: Vec<EvidenceRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EvidenceKind {
    ScreeningFact,
    EventFact,
    CountFact,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub kind: EvidenceKind,
    pub fact_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mandate {
    pub id: String,
    pub principal: String,
    pub purpose: String,
    pub budget: Budget,
    pub coverage: Coverage,
    pub constraints: Constraints,
    pub requirements: Requirements,
    pub duties: Duties,
    pub inputs: Inputs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Budget {
    pub service: ServiceBudget,
    pub audit: AuditBudget,
    pub reserve_completion: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceBudget {
    pub total: i128,
    pub asset: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditBudget {
    pub total: i128,
    pub asset: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Coverage {
    AllMaterial,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Constraints {
    pub networks: Vec<String>,
    pub facilitator: String,
    pub manifest: ManifestRef,
    pub sellers: SellerConstraint,
    pub allowlist: Option<Vec<String>>,
    pub max_single_payment: i128,
    pub deadline: DateTime<Utc>,
    pub eth_rpc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SellerConstraint {
    Allowlist,
    Tariffed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestRef {
    pub path: String,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Requirements {
    pub evidence: EvidenceRequirement,
    pub citations: CitationRequirement,
    pub max_data_age_s: i64,
    pub provenance_samples: usize,
    pub brief_events: usize,
    pub degrade: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum EvidenceRequirement {
    Screening,
    Transaction,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CitationRequirement {
    Required,
    Optional,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Duties {
    pub receipts_topic: String,
    pub anchor_before_delivery: bool,
    pub report_refusals: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Inputs {
    pub pools: Vec<String>,
    pub window_h: i64,
    pub materiality: f64,
    pub min_event_usd: f64,
    pub expected_material_pools: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Listing {
    pub id: String,
    pub seller: String,
    pub url: String,
    pub method: String,
    pub capability: Capability,
    pub produces: Vec<Produce>,
    pub tariff: Tariff,
    pub network: String,
    pub asset: String,
    pub pay_to: String,
    pub discovery: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Capability {
    Screen,
    Events,
    Investigate,
    Explain,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Produce {
    Screening,
    Transaction,
    Report,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tariff {
    pub version: String,
    pub base: i128,
    pub unit: TariffUnit,
    pub unit_price: i128,
    pub max_units: i128,
    pub rounding: Rounding,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TariffUnit {
    Pool,
    PoolWindow,
    InputKb,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Rounding {
    InputKb,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Quote {
    pub listing_id: String,
    pub amount: i128,
    pub asset: String,
    pub network: String,
    pub pay_to: String,
    pub fee_payer: String,
    pub max_timeout_s: i64,
    pub received_at: DateTime<Utc>,
    pub ceiling: i128,
    pub within_tariff: bool,
    pub listing_match: bool,
    pub fee_payer_ok: bool,
    pub request_binding: RequestBinding,
    /// Local planning metadata, not part of x402 ResourceInfo: the number of
    /// tariff units the quoted request carried. The planner only applies a live
    /// quote to a plan step whose unit count matches the quoted request.
    pub requested_units: i128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestBinding {
    pub method: String,
    pub url: String,
    pub body_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Reservation {
    pub id: String,
    pub step: String,
    pub amount: i128,
    pub source: ReservationSource,
    pub state: ReservationState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ReservationSource {
    CeilingAtMax,
    Quote,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ReservationState {
    Held,
    Consumed,
    Released,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Authorization {
    pub id: String,
    pub quote_id: String,
    pub payment_id: String,
    pub tx_id: String,
    pub amount: i128,
    pub valid_start: DateTime<Utc>,
    pub valid_until: DateTime<Utc>,
    pub signed_bytes: Vec<u8>,
    pub request: RequestSent,
    pub submissions: u32,
    pub retrievals: u32,
    pub payment_state: PaymentState,
    pub delivery_state: DeliveryState,
    pub response_body: Option<Vec<u8>>,
    pub response_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PaymentState {
    Prepared,
    Sent,
    Settled,
    Failed,
    Unresolved,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DeliveryState {
    None,
    Received,
    Validated,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestSent {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub seq: u64,
    pub mandate_id: String,
    pub step: String,
    pub listing_id: Option<String>,
    pub seller: Option<String>,
    pub amount: i128,
    pub asset: String,
    pub tx_id: Option<String>,
    pub payment_id_hash: Option<String>,
    pub request_hash: Option<String>,
    pub response_hash: Option<String>,
    pub outcome: ReceiptOutcome,
    pub reason: Option<String>,
    pub latency_ms: Option<u64>,
    pub at: DateTime<Utc>,
    /// Receipt 0 fields: the run's identity instead of a purchase.
    pub mandate_hash: Option<String>,
    pub manifest_hash: Option<String>,
    pub spec_version: Option<String>,
}

/// Format an atomic amount as a decimal string with trailing zeros trimmed to
/// at least four places, e.g. USDC 10_000_000 -> "0.0100". Matches the
/// transcript conventions in docs/demo.md.
pub fn fmt_amount(amount: i128, decimals: u8) -> String {
    let sign = if amount < 0 { "-".to_string() } else { String::new() };
    let a = amount.unsigned_abs();
    let d = decimals as usize;
    let s = a.to_string();
    let (int, frac) = if s.len() <= d {
        ("0".to_string(), format!("{:0>width$}", s, width = d))
    } else {
        let (i, f) = s.split_at(s.len() - d);
        (i.to_string(), f.to_string())
    };
    let frac = frac.trim_end_matches('0');
    let frac = format!("{:0<width$}", frac, width = 4);
    format!("{}{}.{}", sign, int, frac)
}

/// Parse a decimal string (e.g. "0.0100") into atomic units for `decimals`.
pub fn parse_amount(s: &str, decimals: u8) -> Result<i128, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty amount".into());
    }
    let (neg, s) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let (int, frac) = match s.split_once('.') {
        Some((i, f)) => (i, f),
        None => (s, ""),
    };
    if int.is_empty() && frac.is_empty() {
        return Err(format!("invalid amount: {s}"));
    }
    let int = if int.is_empty() { "0" } else { int };
    if !int.chars().all(|c| c.is_ascii_digit()) || !frac.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!("invalid amount: {s}"));
    }
    if frac.len() > decimals as usize {
        return Err(format!(
            "amount {s} has more than {decimals} decimal places"
        ));
    }
    let frac = format!("{:0<width$}", frac, width = decimals as usize);
    let combined = format!("{int}{frac}");
    let value: i128 = combined
        .parse()
        .map_err(|_| format!("invalid amount: {s}"))?;
    Ok(if neg { -value } else { value })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ReceiptOutcome {
    Paid,
    Refused,
    Failed,
    Unresolved,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceResponse {
    pub deployment_id: String,
    pub block_start: i64,
    pub block_end: i64,
    pub block_end_timestamp: DateTime<Utc>,
    pub indexing_errors: bool,
    pub window_requested: WindowBounds,
    pub window_covered: WindowBounds,
    pub truncated: bool,
    pub pools: Vec<PoolEvidence>,
}

/// Merge evidence responses by pool address. A later response for the same
/// pool replaces the earlier one (the events seller's facts supersede the
/// screening seller's for the pools it covers). This is what the runtime
/// analyses against (spec section 8: facts accumulate per pool).
pub fn merge_evidence(evidence: &[EvidenceResponse]) -> Vec<PoolEvidence> {
    let mut pools: Vec<PoolEvidence> = Vec::new();
    for response in evidence {
        for pool in &response.pools {
            match pools.iter_mut().find(|p| p.address == pool.address) {
                Some(existing) => *existing = pool.clone(),
                None => pools.push(pool.clone()),
            }
        }
    }
    pools
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowBounds {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolEvidence {
    pub address: String,
    pub tvl_start: TokenTvl,
    pub tvl_end: TokenTvl,
    pub events: Vec<EventFact>,
    pub mint_count: u64,
    pub burn_count: u64,
    pub swap_count: u64,
    pub mint_amount_usd: f64,
    pub burn_amount_usd: f64,
    pub swap_amount_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenTvl {
    pub token0: f64,
    pub token1: f64,
    pub usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventFact {
    pub transaction_id: String,
    pub log_index: u64,
    pub timestamp: DateTime<Utc>,
    pub amount0: f64,
    pub amount1: f64,
    pub amount_usd: f64,
    pub origin: String,
    pub owner: String,
    pub tick_lower: u64,
    pub tick_upper: u64,
    pub fact_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcript {
    pub mandate: String,
    pub assumption: String,
    pub brief_bound: u64,
    pub quotes: Vec<QuoteRow>,
    pub plan: Option<PlanRow>,
    pub rejected_plans: Vec<RejectedPlanRow>,
    /// Every planning decision in order (spec section 12 prints each phase's
    /// chosen plan and rejections).
    pub plan_phases: Vec<PlanPhaseRow>,
    /// Budget held for future steps, with final state after the run.
    pub reservations: Vec<ReservationRow>,
    pub steps: Vec<StepRow>,
    pub outcomes: Vec<OutcomeRow>,
    pub validation: ValidationRow,
    pub refusals: Vec<RefusalRow>,
    pub totals: TotalsRow,
    pub receipts: ReceiptsRow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteRow {
    pub listing_id: String,
    pub amount: String,
    pub ceiling: String,
    pub within_tariff: bool,
    pub listing_match: bool,
    pub fee_payer_ok: bool,
    pub latency_ms: u64,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanRow {
    pub name: String,
    pub expected: String,
    pub bound: String,
    pub assumption: usize,
    pub authorizations: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectedPlanRow {
    pub name: String,
    pub reason: String,
    pub bound: String,
    pub expected: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanPhaseRow {
    pub chosen: PlanRow,
    pub rejected: Vec<RejectedPlanRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReservationRow {
    pub step: String,
    pub amount: String,
    pub source: String,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepRow {
    pub step: String,
    pub payment_id: String,
    pub tx_id: String,
    pub submissions: u32,
    pub retrievals: u32,
    pub transitions: Vec<TransitionRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionRow {
    pub from: String,
    pub to: String,
    pub at: DateTime<Utc>,
    pub record_count: Option<u32>,
    pub duplicates_ignored: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutcomeRow {
    pub pool: String,
    pub outcome: String,
    pub claim_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationRow {
    pub coverage: String,
    pub calculations: bool,
    pub citations: bool,
    pub provenance: String,
    pub freshness: bool,
    pub schema: bool,
    pub complete: bool,
    pub incomplete_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefusalRow {
    pub code: String,
    pub needed_bound: String,
    pub needed_expected: String,
    pub available: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TotalsRow {
    pub settled: String,
    pub released: String,
    pub unspent: String,
    pub unresolved: String,
    pub audit_spent: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiptsRow {
    pub topic: String,
    pub pending: Vec<u64>,
}
