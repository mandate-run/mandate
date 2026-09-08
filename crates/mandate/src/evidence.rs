//! Spec section 7: the evidence contract as the sellers deliver it, and the
//! exact decimals the analysis computes with. Field names follow the JSON on
//! the wire; nothing is rounded.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use num_bigint::BigInt;
use num_traits::{Signed as _, Zero as _};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    pub from: u64,
    pub to: u64,
}

/// The header every evidence response carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Header {
    pub deployment_id: String,
    pub block_start: u64,
    pub block_end: u64,
    pub block_end_timestamp: u64,
    pub indexed_block: u64,
    pub indexed_block_timestamp: Option<u64>,
    pub indexing_errors: bool,
    pub window_requested: Window,
    pub window_covered: Window,
    pub coverage_shortfall: bool,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    #[serde(rename = "totalValueLockedToken0")]
    pub tvl_token0: String,
    #[serde(rename = "totalValueLockedToken1")]
    pub tvl_token1: String,
    #[serde(rename = "totalValueLockedUSD")]
    pub tvl_usd: Option<String>,
    pub liquidity: String,
    #[serde(rename = "token0PriceUSD")]
    pub token0_price_usd: Option<String>,
    #[serde(rename = "token1PriceUSD")]
    pub token1_price_usd: Option<String>,
}

impl Snapshot {
    pub fn tvl(&self, token: u8) -> &str {
        if token == 0 {
            &self.tvl_token0
        } else {
            &self.tvl_token1
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HourRow {
    #[serde(rename = "periodStartUnix")]
    pub period_start_unix: u64,
    #[serde(rename = "tvlUSD")]
    pub tvl_usd: String,
    #[serde(rename = "volumeUSD")]
    pub volume_usd: String,
    #[serde(rename = "txCount")]
    pub tx_count: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TxRef {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventHit {
    pub transaction: TxRef,
    #[serde(rename = "amountUSD")]
    pub amount_usd: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LargeEvents {
    pub swap: Option<EventHit>,
    pub mint: Option<EventHit>,
    pub burn: Option<EventHit>,
}

impl LargeEvents {
    pub fn get(&self, kind: EventKind) -> Option<&EventHit> {
        match kind {
            EventKind::Swap => self.swap.as_ref(),
            EventKind::Mint => self.mint.as_ref(),
            EventKind::Burn => self.burn.as_ref(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UnvaluedEvents {
    pub mint: bool,
    pub burn: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Material,
    NonMaterial,
    Undetermined,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PoolScreen {
    pub start: Option<Snapshot>,
    pub end: Option<Snapshot>,
    #[serde(default)]
    pub absent_at_start: bool,
    #[serde(default)]
    pub hours: Vec<HourRow>,
    #[serde(default)]
    pub large_events: LargeEvents,
    #[serde(default)]
    pub unvalued_events: UnvaluedEvents,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub coverage_shortfall: bool,
    pub verdict: Verdict,
    #[serde(default)]
    pub reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScreenResponse {
    #[serde(flatten)]
    pub header: Header,
    pub pools: BTreeMap<String, PoolScreen>,
    #[serde(default)]
    pub requests: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Swap,
    Mint,
    Burn,
}

pub const EVENT_KINDS: [EventKind; 3] = [EventKind::Swap, EventKind::Mint, EventKind::Burn];

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Swap => "swap",
            Self::Mint => "mint",
            Self::Burn => "burn",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub transaction: TxRef,
    #[serde(rename = "logIndex")]
    pub log_index: Option<u64>,
    pub timestamp: u64,
    pub amount0: String,
    pub amount1: String,
    #[serde(rename = "amountUSD")]
    pub amount_usd: Option<String>,
    pub origin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(rename = "tickLower", default, skip_serializing_if = "Option::is_none")]
    pub tick_lower: Option<i64>,
    #[serde(rename = "tickUpper", default, skip_serializing_if = "Option::is_none")]
    pub tick_upper: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Counts {
    pub swap: u64,
    pub mint: u64,
    pub burn: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sums {
    pub swap: String,
    pub mint: String,
    pub burn: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PoolEvents {
    pub swaps: Vec<Event>,
    pub mints: Vec<Event>,
    pub burns: Vec<Event>,
    pub counts: Counts,
    pub sum_amount_usd: Sums,
    pub amount_usd_nulls: u64,
    pub truncated: bool,
}

impl PoolEvents {
    pub fn count(&self, kind: EventKind) -> u64 {
        match kind {
            EventKind::Swap => self.counts.swap,
            EventKind::Mint => self.counts.mint,
            EventKind::Burn => self.counts.burn,
        }
    }

    pub fn sum(&self, kind: EventKind) -> &str {
        match kind {
            EventKind::Swap => &self.sum_amount_usd.swap,
            EventKind::Mint => &self.sum_amount_usd.mint,
            EventKind::Burn => &self.sum_amount_usd.burn,
        }
    }

    pub fn all(&self) -> impl Iterator<Item = (EventKind, &Event)> {
        self.swaps
            .iter()
            .map(|e| (EventKind::Swap, e))
            .chain(self.mints.iter().map(|e| (EventKind::Mint, e)))
            .chain(self.burns.iter().map(|e| (EventKind::Burn, e)))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventsResponse {
    #[serde(flatten)]
    pub header: Header,
    pub cap: u64,
    pub pools: BTreeMap<String, PoolEvents>,
    #[serde(default)]
    pub requests: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Explanation {
    pub prose: String,
    pub model: String,
    #[serde(default)]
    pub input_bytes: u64,
}

// Exact decimals: value = m / 10^e.

#[derive(Debug, Clone)]
pub struct Dec {
    pub m: BigInt,
    pub e: u32,
}

impl PartialEq for Dec {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Dec {}

impl PartialOrd for Dec {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Dec {
    /// Numeric order: `0.1` equals `0.10`.
    fn cmp(&self, other: &Self) -> Ordering {
        let (a, b, _) = self.aligned(other);
        a.cmp(&b)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("not a decimal: {0:?}")]
pub struct DecError(pub String);

impl Dec {
    /// Parses `123`, `-1.50`, `1.5e-7`, `2E3`.
    pub fn parse(text: &str) -> Result<Self, DecError> {
        let t = text.trim();
        let (mantissa, exp) = match t.find(['e', 'E']) {
            Some(i) => (
                &t[..i],
                t[i + 1..]
                    .parse::<i32>()
                    .map_err(|_| DecError(text.to_owned()))?,
            ),
            None => (t, 0),
        };
        let (sign, digits) = match mantissa.strip_prefix('-') {
            Some(rest) => (-1, rest),
            None => (1, mantissa.strip_prefix('+').unwrap_or(mantissa)),
        };
        let (int, frac) = digits.split_once('.').unwrap_or((digits, ""));
        if int.is_empty() && frac.is_empty()
            || !int.chars().all(|c| c.is_ascii_digit())
            || !frac.chars().all(|c| c.is_ascii_digit())
        {
            return Err(DecError(text.to_owned()));
        }
        let mut m: BigInt = format!("{int}{frac}")
            .parse()
            .unwrap_or_else(|_| BigInt::zero());
        if sign < 0 {
            m = -m;
        }
        let mut e = frac.len() as i64 - exp as i64;
        if e < 0 {
            m *= BigInt::from(10u32).pow((-e) as u32);
            e = 0;
        }
        Ok(Self { m, e: e as u32 })
    }

    fn aligned(&self, other: &Self) -> (BigInt, BigInt, u32) {
        let e = self.e.max(other.e);
        let a = &self.m * BigInt::from(10u32).pow(e - self.e);
        let b = &other.m * BigInt::from(10u32).pow(e - other.e);
        (a, b, e)
    }

    pub fn sub(&self, other: &Self) -> Self {
        let (a, b, e) = self.aligned(other);
        Self { m: a - b, e }
    }

    pub fn add(&self, other: &Self) -> Self {
        let (a, b, e) = self.aligned(other);
        Self { m: a + b, e }
    }

    pub fn mul(&self, other: &Self) -> Self {
        Self {
            m: &self.m * &other.m,
            e: self.e + other.e,
        }
    }

    /// `self / other` to 18 decimals, truncated toward zero, as the sellers compute it.
    pub fn div18(&self, other: &Self) -> Self {
        let (a, b, _) = self.aligned(other);
        Self {
            m: (a * BigInt::from(10u32).pow(18)) / b,
            e: 18,
        }
    }

    pub fn is_zero(&self) -> bool {
        self.m.is_zero()
    }

    pub fn abs(&self) -> Self {
        Self {
            m: self.m.abs(),
            e: self.e,
        }
    }

    pub fn zero() -> Self {
        Self {
            m: BigInt::zero(),
            e: 0,
        }
    }
}

impl std::fmt::Display for Dec {
    /// The sellers' `decToString`: no exponent, trailing zeros stripped.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let neg = self.m.is_negative();
        let mut digits = self.m.abs().to_string();
        if self.e == 0 {
            return write!(f, "{}{digits}", if neg { "-" } else { "" });
        }
        let e = self.e as usize;
        if digits.len() <= e {
            digits = format!("{}{digits}", "0".repeat(e - digits.len() + 1));
        }
        let cut = digits.len() - e;
        let mut out = format!("{}.{}", &digits[..cut], &digits[cut..]);
        while out.ends_with('0') {
            out.pop();
        }
        if out.ends_with('.') {
            out.pop();
        }
        write!(f, "{}{out}", if neg { "-" } else { "" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimals_match_the_sellers() {
        assert_eq!(Dec::parse("1.50").unwrap().to_string(), "1.5");
        assert_eq!(Dec::parse("1.5e-7").unwrap().to_string(), "0.00000015");
        assert_eq!(Dec::parse("2E3").unwrap().to_string(), "2000");
        assert_eq!(Dec::parse("-0.25").unwrap().to_string(), "-0.25");
        assert_eq!(
            Dec::parse("0.1").unwrap().cmp(&Dec::parse("0.10").unwrap()),
            Ordering::Equal
        );
        let change = Dec::parse("1100")
            .unwrap()
            .sub(&Dec::parse("1000").unwrap());
        assert_eq!(
            change.div18(&Dec::parse("1000").unwrap()).to_string(),
            "0.1"
        );
        assert_eq!(
            Dec::parse("100.5")
                .unwrap()
                .add(&Dec::parse("200").unwrap())
                .to_string(),
            "300.5"
        );
        assert!(Dec::parse("1e3x").is_err());
        assert!(Dec::parse("").is_err());
    }

    #[test]
    fn screen_and_events_parse_from_the_wire_shape() {
        let screen = r#"{"deployment_id":"Qm","block_start":100,"block_end":200,"block_end_timestamp":5,"indexed_block":210,"indexed_block_timestamp":60,"indexing_errors":false,"window_requested":{"from":0,"to":86400},"window_covered":{"from":0,"to":86400},"coverage_shortfall":false,"truncated":false,"requests":4,
          "pools":{"0xabc":{"start":{"totalValueLockedToken0":"1000","totalValueLockedToken1":"500","totalValueLockedUSD":"1","liquidity":"1","token0PriceUSD":"1","token1PriceUSD":null},"end":null,"absent_at_start":false,"hours":[{"periodStartUnix":0,"tvlUSD":"1","volumeUSD":"100.5","txCount":"3"}],"large_events":{"swap":{"transaction":{"id":"0xswap"},"amountUSD":"250000"},"mint":null,"burn":null},"unvalued_events":{"mint":false,"burn":false},"truncated":false,"coverage_shortfall":false,"verdict":"material","reasons":["tvl_change:token0"]}}}"#;
        let s: ScreenResponse = serde_json::from_str(screen).unwrap();
        assert_eq!(s.header.indexed_block, 210);
        let p = &s.pools["0xabc"];
        assert_eq!(p.start.as_ref().unwrap().tvl(1), "500");
        assert!(p.end.is_none());
        assert_eq!(p.verdict, Verdict::Material);
        assert_eq!(
            p.large_events.get(EventKind::Swap).unwrap().amount_usd,
            "250000"
        );
        let events = r#"{"deployment_id":"Qm","block_start":100,"block_end":200,"block_end_timestamp":5,"indexed_block":210,"indexed_block_timestamp":null,"indexing_errors":false,"window_requested":{"from":0,"to":86400},"window_covered":{"from":0,"to":86400},"coverage_shortfall":false,"truncated":false,"cap":5000,"requests":8,
          "pools":{"0xabc":{"swaps":[{"id":"s1","transaction":{"id":"0xswap"},"logIndex":null,"timestamp":1,"amount0":"1","amount1":"-1","amountUSD":"250000","origin":"0xo"}],"mints":[{"id":"m1","transaction":{"id":"0xmint"},"logIndex":7,"timestamp":2,"amount0":"3","amount1":"4","amountUSD":null,"origin":"0xo","owner":null,"tickLower":-10,"tickUpper":10}],"burns":[],"counts":{"swap":1,"mint":1,"burn":0},"sum_amount_usd":{"swap":"250000","mint":"0","burn":"0"},"amount_usd_nulls":1,"truncated":false}}}"#;
        let e: EventsResponse = serde_json::from_str(events).unwrap();
        let p = &e.pools["0xabc"];
        assert_eq!(p.swaps[0].log_index, None);
        assert_eq!(p.mints[0].tick_lower, Some(-10));
        assert_eq!(p.all().count(), 2);
        assert!(e.header.indexed_block_timestamp.is_none());
    }
}
