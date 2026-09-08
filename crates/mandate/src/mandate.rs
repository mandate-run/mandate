//! Spec section 2.1: the mandate object, loaded from TOML and validated at
//! load. A file that violates a field rule is rejected with the field named;
//! nothing is quoted or spent. Amounts in the file are decimal strings and
//! become integers in the asset's atomic unit here.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

pub const HBAR: &str = "0.0.0";
pub const USDC_TESTNET: &str = "0.0.429274";
pub const USDC_MAINNET: &str = "0.0.456858";
pub const HBAR_DECIMALS: u32 = 8;

#[derive(Debug, thiserror::Error)]
pub enum MandateError {
    #[error("mandate file is not valid TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("mandate.{field}: {problem}")]
    Field {
        field: &'static str,
        problem: String,
    },
}

fn bad(field: &'static str, problem: impl Into<String>) -> MandateError {
    MandateError::Field {
        field,
        problem: problem.into(),
    }
}

/// Decimals of the assets the runtime knows how to price.
pub fn asset_decimals(asset: &str) -> Option<u32> {
    match asset {
        HBAR => Some(HBAR_DECIMALS),
        USDC_TESTNET | USDC_MAINNET => Some(6),
        _ => None,
    }
}

/// `"0.0100"` with 6 decimals is 10000. Rejects signs, exponents and more
/// fractional digits than the asset has.
pub fn parse_amount(text: &str, decimals: u32) -> Result<i64, String> {
    let text = text.trim();
    let (int, frac) = text.split_once('.').unwrap_or((text, ""));
    if int.is_empty() && frac.is_empty()
        || !int.chars().all(|c| c.is_ascii_digit())
        || !frac.chars().all(|c| c.is_ascii_digit())
    {
        return Err(format!("{text:?} is not a non-negative decimal"));
    }
    if frac.len() > decimals as usize {
        return Err(format!("{text:?} has more than {decimals} decimals"));
    }
    let scaled = format!("{int}{frac:0<width$}", width = decimals as usize);
    let scaled = scaled.trim_start_matches('0');
    if scaled.is_empty() {
        return Ok(0);
    }
    scaled
        .parse::<i64>()
        .map_err(|_| format!("{text:?} overflows"))
}

/// The inverse of [`parse_amount`], for documents and transcripts.
pub fn format_amount(atomic: i64, decimals: u32) -> String {
    if decimals == 0 {
        return atomic.to_string();
    }
    let sign = if atomic < 0 { "-" } else { "" };
    let digits = format!(
        "{:0>width$}",
        atomic.unsigned_abs(),
        width = decimals as usize + 1
    );
    let cut = digits.len() - decimals as usize;
    format!("{sign}{}.{}", &digits[..cut], &digits[cut..])
}

/// Longest mandate id; the brief header of section 8 budgets for it.
pub const MAX_ID_LEN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mandate {
    pub id: String,
    /// SHA-256 of the file bytes.
    pub hash: String,
    pub principal: String,
    pub purpose: String,
    pub budget: Budget,
    pub coverage: Coverage,
    pub constraints: Constraints,
    pub requirements: Requirements,
    pub duties: Duties,
    pub inputs: Inputs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Budget {
    pub service_total: i64,
    pub service_asset: String,
    pub service_decimals: u32,
    /// Tinybars.
    pub audit_total: i64,
    pub reserve_completion: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    AllMaterial,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SellerPolicy {
    Allowlist(Vec<String>),
    Tariffed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Constraints {
    pub networks: Vec<String>,
    pub facilitator: String,
    pub manifest_path: String,
    pub manifest_hash: String,
    pub sellers: SellerPolicy,
    pub max_single_payment: i64,
    pub deadline: OffsetDateTime,
    pub eth_rpc: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    Screening,
    Transaction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Citations {
    Required,
    Optional,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirements {
    pub evidence: Evidence,
    pub citations: Citations,
    pub max_data_age_s: u64,
    pub provenance_samples: u32,
    pub brief_events: u32,
    pub degrade: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Duties {
    pub receipts_topic: String,
    pub anchor_before_delivery: bool,
    pub report_refusals: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inputs {
    pub pools: Vec<String>,
    pub window_h: u32,
    pub materiality: String,
    pub min_event_usd: String,
    pub expected_material_pools: u32,
}

#[derive(Deserialize)]
struct File {
    mandate: Raw,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    id: Option<String>,
    principal: String,
    purpose: String,
    budget: RawBudget,
    coverage: String,
    constraints: RawConstraints,
    #[serde(default)]
    requirements: RawRequirements,
    duties: RawDuties,
    inputs: RawInputs,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBudget {
    service: RawMoney,
    audit: RawMoney,
    #[serde(default = "yes")]
    reserve_completion: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMoney {
    total: String,
    asset: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum When {
    Date(toml::value::Datetime),
    Text(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    path: String,
    hash: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConstraints {
    networks: Vec<String>,
    facilitator: String,
    manifest: RawManifest,
    sellers: String,
    #[serde(default)]
    allowlist: Vec<String>,
    max_single_payment: String,
    deadline: When,
    eth_rpc: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawRequirements {
    evidence: Option<String>,
    citations: Option<String>,
    max_data_age_s: Option<u64>,
    provenance_samples: Option<u32>,
    brief_events: Option<u32>,
    degrade: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDuties {
    receipts_topic: String,
    #[serde(default)]
    anchor_before_delivery: bool,
    #[serde(default = "yes")]
    report_refusals: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInputs {
    pools: Vec<String>,
    window_h: u32,
    materiality: String,
    min_event_usd: String,
    expected_material_pools: Option<u32>,
}

fn yes() -> bool {
    true
}

fn is_entity_id(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// An absolute http(s) URL with a host.
fn is_url(s: &str) -> bool {
    url::Url::parse(s)
        .map(|u| {
            matches!(u.scheme(), "http" | "https") && u.host_str().is_some_and(|h| !h.is_empty())
        })
        .unwrap_or(false)
}

fn is_decimal(s: &str) -> bool {
    let (int, frac) = s.split_once('.').unwrap_or((s, "0"));
    !int.is_empty()
        && !frac.is_empty()
        && int.chars().all(|c| c.is_ascii_digit())
        && frac.chars().all(|c| c.is_ascii_digit())
}

impl Mandate {
    pub fn from_path(path: &std::path::Path, now: OffsetDateTime) -> Result<Self, MandateError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| bad("file", format!("{}: {e}", path.display())))?;
        Self::from_toml(&text, now)
    }

    /// Parses and validates. `now` decides whether the deadline has passed.
    pub fn from_toml(text: &str, now: OffsetDateTime) -> Result<Self, MandateError> {
        let hash = hex(&Sha256::digest(text.as_bytes()));
        let raw = toml::from_str::<File>(text)?.mandate;

        let service_asset = raw.budget.service.asset.trim().to_owned();
        let service_decimals = asset_decimals(&service_asset).ok_or_else(|| {
            bad(
                "budget.service.asset",
                format!("{service_asset} has unknown decimals"),
            )
        })?;
        let service_total = parse_amount(&raw.budget.service.total, service_decimals)
            .map_err(|e| bad("budget.service.total", e))?;
        if service_total <= 0 {
            return Err(bad("budget.service.total", "must be positive"));
        }
        if raw.budget.audit.asset.trim() != "HBAR" && raw.budget.audit.asset.trim() != HBAR {
            return Err(bad("budget.audit.asset", "must be HBAR"));
        }
        let audit_total = parse_amount(&raw.budget.audit.total, HBAR_DECIMALS)
            .map_err(|e| bad("budget.audit.total", e))?;
        if audit_total <= 0 {
            return Err(bad("budget.audit.total", "must be positive"));
        }

        let coverage = match raw.coverage.as_str() {
            "all_material" => Coverage::AllMaterial,
            other => {
                return Err(bad(
                    "coverage",
                    format!("{other:?}; only all_material is supported"),
                ));
            }
        };

        let c = raw.constraints;
        if c.networks.is_empty() {
            return Err(bad(
                "constraints.networks",
                "must list at least one network",
            ));
        }
        for n in &c.networks {
            if n != "hedera:testnet" && n != "hedera:mainnet" {
                return Err(bad(
                    "constraints.networks",
                    format!("{n:?} is not a Hedera CAIP-2 id"),
                ));
            }
        }
        if !is_url(&c.facilitator) {
            return Err(bad("constraints.facilitator", "must be an http(s) url"));
        }
        if c.manifest.path.trim().is_empty() {
            return Err(bad("constraints.manifest.path", "must be set"));
        }
        if !is_hex(&c.manifest.hash, 64) {
            return Err(bad(
                "constraints.manifest.hash",
                "must be 64 hex characters",
            ));
        }
        let sellers = match c.sellers.as_str() {
            "allowlist" if c.allowlist.is_empty() => {
                return Err(bad(
                    "constraints.allowlist",
                    "must list sellers when sellers is allowlist",
                ));
            }
            "allowlist" => SellerPolicy::Allowlist(c.allowlist.clone()),
            "tariffed" => SellerPolicy::Tariffed,
            other => {
                return Err(bad(
                    "constraints.sellers",
                    format!("{other:?}; allowlist or tariffed"),
                ));
            }
        };
        let max_single_payment = parse_amount(&c.max_single_payment, service_decimals)
            .map_err(|e| bad("constraints.max_single_payment", e))?;
        if max_single_payment <= 0 {
            return Err(bad("constraints.max_single_payment", "must be positive"));
        }
        let deadline_text = match &c.deadline {
            When::Date(d) => d.to_string(),
            When::Text(t) => t.clone(),
        };
        let deadline = OffsetDateTime::parse(&deadline_text, &Rfc3339).map_err(|_| {
            bad(
                "constraints.deadline",
                format!("{deadline_text:?} is not RFC 3339 with an offset"),
            )
        })?;
        if deadline <= now {
            return Err(bad("constraints.deadline", "has passed"));
        }
        if let Some(rpc) = &c.eth_rpc
            && !is_url(rpc)
        {
            return Err(bad("constraints.eth_rpc", "must be an http(s) url"));
        }

        let r = raw.requirements;
        let evidence = match r.evidence.as_deref().unwrap_or("transaction") {
            "screening" => Evidence::Screening,
            "transaction" => Evidence::Transaction,
            other => {
                return Err(bad(
                    "requirements.evidence",
                    format!("{other:?}; screening or transaction"),
                ));
            }
        };
        let citations = match r.citations.as_deref().unwrap_or("required") {
            "required" => Citations::Required,
            "optional" => Citations::Optional,
            other => {
                return Err(bad(
                    "requirements.citations",
                    format!("{other:?}; required or optional"),
                ));
            }
        };
        let max_data_age_s = r.max_data_age_s.unwrap_or(3600);
        if max_data_age_s == 0 {
            return Err(bad("requirements.max_data_age_s", "must be positive"));
        }
        let provenance_samples = r.provenance_samples.unwrap_or(3);
        if provenance_samples > 0 && c.eth_rpc.is_none() {
            return Err(bad(
                "constraints.eth_rpc",
                format!(
                    "required when provenance_samples is {provenance_samples}; set provenance_samples = 0 to disable sampling"
                ),
            ));
        }
        let brief_events = r.brief_events.unwrap_or(5);

        let d = raw.duties;
        if !is_entity_id(&d.receipts_topic) {
            return Err(bad(
                "duties.receipts_topic",
                "must be a topic id like 0.0.123",
            ));
        }

        let i = raw.inputs;
        if i.pools.is_empty() {
            return Err(bad("inputs.pools", "must list at least one pool"));
        }
        let mut pools = Vec::with_capacity(i.pools.len());
        for p in &i.pools {
            let p = p.trim().to_lowercase();
            if !(p.len() == 42
                && p.starts_with("0x")
                && p[2..].chars().all(|c| c.is_ascii_hexdigit()))
            {
                return Err(bad("inputs.pools", format!("{p:?} is not a pool address")));
            }
            if pools.contains(&p) {
                return Err(bad("inputs.pools", format!("{p} is listed twice")));
            }
            pools.push(p);
        }
        if i.window_h == 0 {
            return Err(bad("inputs.window_h", "must be positive"));
        }
        if !is_decimal(&i.materiality) {
            return Err(bad("inputs.materiality", "must be a decimal like 0.05"));
        }
        if !is_decimal(&i.min_event_usd) {
            return Err(bad("inputs.min_event_usd", "must be a decimal like 100000"));
        }
        let expected_material_pools = i.expected_material_pools.unwrap_or(1);
        if expected_material_pools as usize > pools.len() {
            return Err(bad(
                "inputs.expected_material_pools",
                "exceeds the number of pools",
            ));
        }

        let id = match raw.id {
            Some(id) if !id.trim().is_empty() => id.trim().to_owned(),
            _ => format!("m-{}", &hash[..12]),
        };
        if !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(bad("id", "letters, digits, hyphen and underscore only"));
        }
        if id.len() > MAX_ID_LEN {
            return Err(bad("id", format!("at most {MAX_ID_LEN} bytes")));
        }

        Ok(Self {
            id,
            hash,
            principal: raw.principal,
            purpose: raw.purpose.trim().to_owned(),
            budget: Budget {
                service_total,
                service_asset,
                service_decimals,
                audit_total,
                reserve_completion: raw.budget.reserve_completion,
            },
            coverage,
            constraints: Constraints {
                networks: c.networks,
                facilitator: c.facilitator.trim_end_matches('/').to_owned(),
                manifest_path: c.manifest.path,
                manifest_hash: c.manifest.hash.to_lowercase(),
                sellers,
                max_single_payment,
                deadline,
                eth_rpc: c.eth_rpc,
            },
            requirements: Requirements {
                evidence,
                citations,
                max_data_age_s,
                provenance_samples,
                brief_events,
                degrade: r.degrade.unwrap_or(false),
            },
            duties: Duties {
                receipts_topic: d.receipts_topic,
                anchor_before_delivery: d.anchor_before_delivery,
                report_refusals: d.report_refusals,
            },
            inputs: Inputs {
                pools,
                window_h: i.window_h,
                materiality: i.materiality,
                min_event_usd: i.min_event_usd,
                expected_material_pools,
            },
        })
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    pub const EXAMPLE: &str = r#"
[mandate]
id = "demo-1"
principal = "0.0.10399984"
purpose = "Explain any material liquidity change in the listed pools over the last 24 hours."
coverage = "all_material"

[mandate.budget]
service = { total = "0.0100", asset = "0.0.429274" }
audit = { total = "0.5", asset = "HBAR" }
reserve_completion = true

[mandate.constraints]
networks = ["hedera:testnet"]
facilitator = "https://api.testnet.blocky402.com/"
manifest = { path = "manifest.json", hash = "8d804a9bc82dd7ec447a446e2379f13719b9a69dceff82487e87051d469e645c" }
sellers = "allowlist"
allowlist = ["mandate-sellers"]
max_single_payment = "0.0090"
deadline = 2026-09-13T16:00:00Z
eth_rpc = "https://ethereum-rpc.publicnode.com"

[mandate.requirements]
evidence = "transaction"
citations = "required"
max_data_age_s = 3600
degrade = false

[mandate.duties]
receipts_topic = "0.0.10410389"
report_refusals = true

[mandate.inputs]
pools = ["0x88E6A0C2DDD26FEEB64F039A2C41296FCB3F5640"]
window_h = 24
materiality = "0.05"
min_event_usd = "100000"
"#;

    const NOW: OffsetDateTime = datetime!(2026-09-07 18:00 UTC);

    #[test]
    fn amounts_parse_and_format() {
        assert_eq!(parse_amount("0.0100", 6), Ok(10_000));
        assert_eq!(parse_amount("1", 6), Ok(1_000_000));
        assert_eq!(parse_amount("0.5", 8), Ok(50_000_000));
        assert_eq!(parse_amount("0", 6), Ok(0));
        assert!(parse_amount("0.0000001", 6).is_err());
        assert!(parse_amount("-1", 6).is_err());
        assert!(parse_amount("1e3", 6).is_err());
        assert!(parse_amount("99999999999999999999", 6).is_err());
        assert_eq!(format_amount(10_000, 6), "0.010000");
        assert_eq!(format_amount(1_500, 6), "0.001500");
        assert_eq!(format_amount(50_000_000, 8), "0.50000000");
    }

    #[test]
    fn example_loads_with_atomic_amounts_and_defaults() {
        let m = Mandate::from_toml(EXAMPLE, NOW).unwrap();
        assert_eq!(m.id, "demo-1");
        assert_eq!(m.hash.len(), 64);
        assert_eq!(m.budget.service_total, 10_000);
        assert_eq!(m.budget.service_decimals, 6);
        assert_eq!(m.budget.audit_total, 50_000_000);
        assert_eq!(m.constraints.max_single_payment, 9_000);
        assert_eq!(
            m.constraints.facilitator,
            "https://api.testnet.blocky402.com"
        );
        assert_eq!(m.constraints.deadline, datetime!(2026-09-13 16:00 UTC));
        assert_eq!(m.requirements.provenance_samples, 3);
        assert_eq!(m.requirements.brief_events, 5);
        assert_eq!(
            m.inputs.pools,
            vec!["0x88e6a0c2ddd26feeb64f039a2c41296fcb3f5640"]
        );
        assert_eq!(m.inputs.expected_material_pools, 1);
        assert!(
            matches!(m.constraints.sellers, SellerPolicy::Allowlist(ref l) if l == &["mandate-sellers"])
        );
    }

    fn with(replace: &str, by: &str) -> Result<Mandate, MandateError> {
        assert!(EXAMPLE.contains(replace), "{replace}");
        Mandate::from_toml(&EXAMPLE.replacen(replace, by, 1), NOW)
    }

    fn field_of(r: Result<Mandate, MandateError>) -> String {
        match r {
            Err(MandateError::Field { field, .. }) => field.to_owned(),
            other => panic!("expected a field error, got {other:?}"),
        }
    }

    #[test]
    fn positive_provenance_samples_need_an_rpc() {
        assert_eq!(
            field_of(with(
                "eth_rpc = \"https://ethereum-rpc.publicnode.com\"\n",
                ""
            )),
            "constraints.eth_rpc"
        );
        let m = Mandate::from_toml(
            &EXAMPLE
                .replacen("eth_rpc = \"https://ethereum-rpc.publicnode.com\"\n", "", 1)
                .replacen(
                    "degrade = false",
                    "degrade = false\nprovenance_samples = 0",
                    1,
                ),
            NOW,
        )
        .unwrap();
        assert_eq!(m.requirements.provenance_samples, 0);
        assert!(m.constraints.eth_rpc.is_none());
    }

    #[test]
    fn each_rule_names_its_field() {
        assert_eq!(
            field_of(with("coverage = \"all_material\"", "coverage = \"top_3\"")),
            "coverage"
        );
        assert_eq!(
            field_of(with(
                "max_single_payment = \"0.0090\"",
                "max_single_payment = \"0\""
            )),
            "constraints.max_single_payment"
        );
        assert_eq!(
            field_of(with(
                "eth_rpc = \"https://ethereum-rpc.publicnode.com\"",
                "eth_rpc = \"https://\""
            )),
            "constraints.eth_rpc"
        );
        assert_eq!(
            field_of(with(
                "facilitator = \"https://api.testnet.blocky402.com/\"",
                "facilitator = \"ftp://api.testnet.blocky402.com\""
            )),
            "constraints.facilitator"
        );
        assert_eq!(
            field_of(with(
                "deadline = 2026-09-13T16:00:00Z",
                "deadline = 2026-09-01T16:00:00Z"
            )),
            "constraints.deadline"
        );
        assert_eq!(
            field_of(with(
                "service = { total = \"0.0100\"",
                "service = { total = \"0.00001234\""
            )),
            "budget.service.total"
        );
        assert_eq!(
            field_of(with("asset = \"0.0.429274\"", "asset = \"0.0.1\"")),
            "budget.service.asset"
        );
        assert_eq!(
            field_of(with(
                "networks = [\"hedera:testnet\"]",
                "networks = [\"eip155:1\"]"
            )),
            "constraints.networks"
        );
        assert_eq!(
            field_of(with("allowlist = [\"mandate-sellers\"]\n", "")),
            "constraints.allowlist"
        );
        assert_eq!(
            field_of(with(
                "pools = [\"0x88E6A0C2DDD26FEEB64F039A2C41296FCB3F5640\"]",
                "pools = [\"88e6\"]"
            )),
            "inputs.pools"
        );
        assert_eq!(
            field_of(with("window_h = 24", "window_h = 0")),
            "inputs.window_h"
        );
        assert_eq!(
            field_of(with(
                "receipts_topic = \"0.0.10410389\"",
                "receipts_topic = \"topic\""
            )),
            "duties.receipts_topic"
        );
        assert_eq!(
            field_of(with(
                "hash = \"8d804a9bc82dd7ec447a446e2379f13719b9a69dceff82487e87051d469e645c\"",
                "hash = \"abc\""
            )),
            "constraints.manifest.hash"
        );
    }

    #[test]
    fn unknown_fields_and_bad_toml_are_rejected() {
        assert!(matches!(
            with(
                "report_refusals = true",
                "report_refusals = true\nextra = 1"
            ),
            Err(MandateError::Toml(_))
        ));
        assert!(matches!(
            Mandate::from_toml("not toml", NOW),
            Err(MandateError::Toml(_))
        ));
    }

    #[test]
    fn a_payment_cap_above_the_budget_is_allowed() {
        // Scenario 3: service 0.0030 with a 0.0090 cap must reach planning and
        // be refused there, not at load.
        let m = with(
            "service = { total = \"0.0100\"",
            "service = { total = \"0.0030\"",
        )
        .unwrap();
        assert_eq!(
            (m.budget.service_total, m.constraints.max_single_payment),
            (3_000, 9_000)
        );
    }

    #[test]
    fn id_defaults_to_the_hash_prefix() {
        let m = with("id = \"demo-1\"\n", "").unwrap();
        assert!(m.id.starts_with("m-"));
        assert_eq!(m.id.len(), 14);
    }
}
