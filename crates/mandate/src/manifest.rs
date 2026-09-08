//! Spec section 2.2: the manifest of listings the principal approved, pinned
//! by hash. A tariff is a price ceiling: `base + unit_price * units` for
//! `units <= max_units`. Reservations and bounds always use the ceiling.

use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("manifest is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("manifest {path}: {problem}")]
    File { path: String, problem: String },
    #[error("manifest hash {actual} differs from the pinned {expected}")]
    HashMismatch { expected: String, actual: String },
    #[error("listing {listing}: {problem}")]
    Listing { listing: String, problem: String },
    #[error("listing {0} is not in the manifest")]
    UnknownListing(String),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TariffError {
    #[error("{units} units exceed max_units {max_units}")]
    AboveMaxUnits { units: u64, max_units: u64 },
    #[error("unit count or ceiling overflows")]
    Overflow,
    #[error("request body has no pools array; the {0:?} tariff counts pools")]
    MissingPools(Unit),
    #[error("request body has no window; the pool_window tariff counts windows")]
    MissingWindow,
    #[error("request body: {0}")]
    Body(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Screen,
    Events,
    Investigate,
    Explain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Produces {
    Screening,
    Transaction,
    Report,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Pool,
    PoolWindow,
    InputKb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    Get,
    Post,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

/// Section 2.2 tariff. Amounts are atomic units of the listing's asset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tariff {
    pub version: String,
    pub base: i64,
    pub unit: Unit,
    pub unit_price: i64,
    pub max_units: u64,
    /// How fractional units count; only `up` is defined.
    #[serde(default = "up")]
    pub rounding: String,
}

fn up() -> String {
    "up".to_owned()
}

impl Tariff {
    /// The ceiling for `units`, or an error above `max_units`.
    pub fn ceiling(&self, units: u64) -> Result<i64, TariffError> {
        if units > self.max_units {
            return Err(TariffError::AboveMaxUnits {
                units,
                max_units: self.max_units,
            });
        }
        let units = i64::try_from(units).map_err(|_| TariffError::Overflow)?;
        self.unit_price
            .checked_mul(units)
            .and_then(|v| v.checked_add(self.base))
            .ok_or(TariffError::Overflow)
    }

    /// The ceiling at `max_units`, what a reservation holds before a quote.
    pub fn ceiling_at_max(&self) -> Result<i64, TariffError> {
        self.ceiling(self.max_units)
    }
}

/// One manifest entry, section 2.2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Listing {
    pub id: String,
    pub seller: String,
    pub url: String,
    pub method: Method,
    pub capability: Capability,
    pub produces: Produces,
    pub tariff: Tariff,
    pub network: String,
    pub asset: String,
    pub pay_to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery: Option<serde_json::Value>,
}

impl Listing {
    pub fn ceiling(&self, units: u64) -> Result<i64, TariffError> {
        self.tariff.ceiling(units)
    }
}

/// The request shape unit counting needs, read from the body that is sent.
/// Shared with the sellers: `pool` counts pools, `pool_window` counts pools
/// times `ceil(window_seconds / 86400)`, `input_kb` counts
/// `ceil(body_bytes / 1024)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RequestShape {
    /// Length of the body's `pools` array, when the body is JSON with one.
    pub pools: Option<u64>,
    /// `window.to - window.from` when the body carries a window.
    pub window_seconds: Option<u64>,
    pub body_bytes: u64,
}

pub const WINDOW_SECONDS: u64 = 86_400;
pub const KB: u64 = 1024;

impl RequestShape {
    /// Reads pools and window from a JSON body; an empty body is a shape with
    /// neither. A body that is not JSON, or whose window is not two integers
    /// with `to` after `from`, is an error, never a guess.
    pub fn from_body(body: &[u8]) -> Result<Self, TariffError> {
        let body_bytes = body.len() as u64;
        if body.is_empty() {
            return Ok(Self {
                pools: None,
                window_seconds: None,
                body_bytes,
            });
        }
        let json: serde_json::Value = serde_json::from_slice(body)
            .map_err(|e| TariffError::Body(format!("not JSON: {e}")))?;
        let pools = match json.get("pools") {
            None => None,
            Some(serde_json::Value::Array(a)) => Some(a.len() as u64),
            Some(_) => return Err(TariffError::Body("pools must be an array".to_owned())),
        };
        let window_seconds = match json.get("window") {
            None => None,
            Some(w) => {
                let from = w.get("from").and_then(serde_json::Value::as_u64);
                let to = w.get("to").and_then(serde_json::Value::as_u64);
                match (from, to) {
                    (Some(from), Some(to)) if to > from => Some(to - from),
                    _ => {
                        return Err(TariffError::Body(
                            "window must have integer from and to with to after from".to_owned(),
                        ));
                    }
                }
            }
        };
        Ok(Self {
            pools,
            window_seconds,
            body_bytes,
        })
    }
}

/// Units under a tariff for a request shape, with checked arithmetic. A
/// tariff that counts pools needs a body with pools; one that counts windows
/// also needs a window.
pub fn units_for(unit: Unit, shape: &RequestShape) -> Result<u64, TariffError> {
    match unit {
        Unit::Pool => shape.pools.ok_or(TariffError::MissingPools(unit)),
        Unit::PoolWindow => {
            let pools = shape.pools.ok_or(TariffError::MissingPools(unit))?;
            let seconds = shape.window_seconds.ok_or(TariffError::MissingWindow)?;
            let windows = seconds.div_ceil(WINDOW_SECONDS).max(1);
            pools.checked_mul(windows).ok_or(TariffError::Overflow)
        }
        Unit::InputKb => Ok(shape.body_bytes.div_ceil(KB)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: String,
    pub listings: Vec<Listing>,
    /// SHA-256 of the document bytes; set by the loader, never by the file.
    #[serde(skip)]
    pub hash: String,
}

fn is_entity_id(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

impl Manifest {
    /// Parses and validates a manifest document; `hash` is the SHA-256 of `text`.
    pub fn from_json(text: &str) -> Result<Self, ManifestError> {
        let mut manifest: Manifest = serde_json::from_str(text)?;
        manifest.hash = Sha256::digest(text.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        manifest.validate()?;
        Ok(manifest)
    }

    /// Loads a file and requires its hash to equal the pinned one.
    pub fn load_pinned(path: &Path, expected_hash: &str) -> Result<Self, ManifestError> {
        let text = std::fs::read_to_string(path).map_err(|e| ManifestError::File {
            path: path.display().to_string(),
            problem: e.to_string(),
        })?;
        let manifest = Self::from_json(&text)?;
        if !manifest.hash.eq_ignore_ascii_case(expected_hash) {
            return Err(ManifestError::HashMismatch {
                expected: expected_hash.to_owned(),
                actual: manifest.hash,
            });
        }
        Ok(manifest)
    }

    fn validate(&self) -> Result<(), ManifestError> {
        let bad = |listing: &str, problem: &str| ManifestError::Listing {
            listing: listing.to_owned(),
            problem: problem.to_owned(),
        };
        let mut ids = std::collections::HashSet::new();
        for l in &self.listings {
            if l.id.is_empty() || !ids.insert(l.id.as_str()) {
                return Err(bad(&l.id, "id must be unique and non-empty"));
            }
            let url = url::Url::parse(&l.url).map_err(|_| bad(&l.id, "url must parse"))?;
            if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
                return Err(bad(&l.id, "url must be http(s) with a host"));
            }
            if l.network != "hedera:testnet" && l.network != "hedera:mainnet" {
                return Err(bad(&l.id, "network must be a Hedera CAIP-2 id"));
            }
            if !is_entity_id(&l.asset) || !is_entity_id(&l.pay_to) {
                return Err(bad(&l.id, "asset and pay_to must be entity ids"));
            }
            if l.tariff.base < 0 || l.tariff.unit_price < 0 || l.tariff.max_units == 0 {
                return Err(bad(
                    &l.id,
                    "tariff needs non-negative prices and max_units above 0",
                ));
            }
            if l.tariff.rounding != "up" {
                return Err(bad(&l.id, "tariff.rounding must be up"));
            }
            if l.tariff.version.is_empty() {
                return Err(bad(&l.id, "tariff.version must be set"));
            }
            l.tariff
                .ceiling_at_max()
                .map_err(|e| bad(&l.id, &e.to_string()))?;
        }
        Ok(())
    }

    pub fn listing(&self, id: &str) -> Result<&Listing, ManifestError> {
        self.listings
            .iter()
            .find(|l| l.id == id)
            .ok_or_else(|| ManifestError::UnknownListing(id.to_owned()))
    }

    pub fn with_capability(&self, capability: Capability) -> impl Iterator<Item = &Listing> {
        self.listings
            .iter()
            .filter(move |l| l.capability == capability)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const EXAMPLE: &str = r#"{
  "version": "1",
  "listings": [
    {"id": "screen", "seller": "mandate-sellers", "url": "http://127.0.0.1:4021/screen", "method": "post",
     "capability": "screen", "produces": "screening",
     "tariff": {"version": "2026-09-07", "base": 0, "unit": "pool", "unit_price": 200, "max_units": 20},
     "network": "hedera:testnet", "asset": "0.0.429274", "pay_to": "0.0.10409989"},
    {"id": "events", "seller": "mandate-sellers", "url": "http://127.0.0.1:4021/events", "method": "post",
     "capability": "events", "produces": "transaction",
     "tariff": {"version": "2026-09-07", "base": 0, "unit": "pool_window", "unit_price": 1500, "max_units": 20},
     "network": "hedera:testnet", "asset": "0.0.429274", "pay_to": "0.0.10409989"},
    {"id": "explain", "seller": "mandate-sellers", "url": "http://127.0.0.1:4021/explain", "method": "post",
     "capability": "explain", "produces": "report",
     "tariff": {"version": "2026-09-07", "base": 0, "unit": "input_kb", "unit_price": 100, "max_units": 8},
     "network": "hedera:testnet", "asset": "0.0.429274", "pay_to": "0.0.10409989"}
  ]
}"#;

    #[test]
    fn parses_and_pins_by_hash() {
        let m = Manifest::from_json(EXAMPLE).unwrap();
        assert_eq!(m.listings.len(), 3);
        assert_eq!(m.hash.len(), 64);
        assert_eq!(m.listing("events").unwrap().tariff.unit, Unit::PoolWindow);
        assert!(matches!(
            m.listing("nope"),
            Err(ManifestError::UnknownListing(_))
        ));
        let dir = std::env::temp_dir().join(format!("mandate-manifest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("manifest.json");
        std::fs::write(&path, EXAMPLE).unwrap();
        assert!(Manifest::load_pinned(&path, &m.hash.to_uppercase()).is_ok());
        assert!(matches!(
            Manifest::load_pinned(&path, &"0".repeat(64)),
            Err(ManifestError::HashMismatch { .. })
        ));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn ceilings_follow_the_tariff_and_stop_at_max_units() {
        let m = Manifest::from_json(EXAMPLE).unwrap();
        let screen = m.listing("screen").unwrap();
        assert_eq!(screen.ceiling(5), Ok(1_000));
        assert_eq!(screen.ceiling(20), Ok(4_000));
        assert_eq!(
            screen.ceiling(21),
            Err(TariffError::AboveMaxUnits {
                units: 21,
                max_units: 20
            })
        );
        let explain = m.listing("explain").unwrap();
        assert_eq!(explain.tariff.ceiling_at_max(), Ok(800));
        let huge = Tariff {
            version: "v".into(),
            base: i64::MAX,
            unit: Unit::Pool,
            unit_price: 1,
            max_units: 2,
            rounding: "up".into(),
        };
        assert_eq!(huge.ceiling(1), Err(TariffError::Overflow));
    }

    #[test]
    fn unit_counting_is_the_shared_definition_and_checked() {
        let body = |pools: usize, from: u64, to: u64| -> Vec<u8> {
            let list: Vec<String> = (0..pools).map(|i| format!("\"0x{i:040x}\"")).collect();
            format!(
                r#"{{"pools":[{}],"window":{{"from":{from},"to":{to}}}}}"#,
                list.join(",")
            )
            .into_bytes()
        };
        let day = RequestShape::from_body(&body(5, 0, 86_400)).unwrap();
        assert_eq!((day.pools, day.window_seconds), (Some(5), Some(86_400)));
        assert_eq!(units_for(Unit::Pool, &day), Ok(5));
        assert_eq!(units_for(Unit::PoolWindow, &day), Ok(5));
        let over = RequestShape::from_body(&body(5, 0, 86_401)).unwrap();
        assert_eq!(units_for(Unit::PoolWindow, &over), Ok(10));
        let hour = RequestShape::from_body(&body(5, 0, 3_600)).unwrap();
        assert_eq!(units_for(Unit::PoolWindow, &hour), Ok(5));
        assert_eq!(
            units_for(
                Unit::InputKb,
                &RequestShape {
                    body_bytes: 1,
                    ..Default::default()
                }
            ),
            Ok(1)
        );
        assert_eq!(
            units_for(
                Unit::InputKb,
                &RequestShape {
                    body_bytes: 1024,
                    ..Default::default()
                }
            ),
            Ok(1)
        );
        assert_eq!(
            units_for(
                Unit::InputKb,
                &RequestShape {
                    body_bytes: 1025,
                    ..Default::default()
                }
            ),
            Ok(2)
        );
        assert_eq!(
            units_for(Unit::InputKb, &RequestShape::from_body(b"").unwrap()),
            Ok(0)
        );

        let empty = RequestShape::from_body(b"").unwrap();
        assert_eq!(
            units_for(Unit::Pool, &empty),
            Err(TariffError::MissingPools(Unit::Pool))
        );
        let no_window = RequestShape::from_body(br#"{"pools":["0xa"]}"#).unwrap();
        assert_eq!(
            units_for(Unit::PoolWindow, &no_window),
            Err(TariffError::MissingWindow)
        );
        assert!(matches!(
            RequestShape::from_body(b"not json"),
            Err(TariffError::Body(_))
        ));
        assert!(matches!(
            RequestShape::from_body(br#"{"pools":"x"}"#),
            Err(TariffError::Body(_))
        ));
        assert!(matches!(
            RequestShape::from_body(br#"{"pools":[],"window":{"from":5,"to":5}}"#),
            Err(TariffError::Body(_))
        ));
        let huge = RequestShape {
            pools: Some(u64::MAX / 2 + 1),
            window_seconds: Some(2 * 86_400),
            body_bytes: 0,
        };
        assert_eq!(
            units_for(Unit::PoolWindow, &huge),
            Err(TariffError::Overflow)
        );
    }

    #[test]
    fn invalid_listings_are_named() {
        let dup = EXAMPLE.replacen("\"id\": \"events\"", "\"id\": \"screen\"", 1);
        assert!(matches!(
            Manifest::from_json(&dup),
            Err(ManifestError::Listing { .. })
        ));
        let bad_url = EXAMPLE.replacen("http://127.0.0.1:4021/screen", "ftp://x/screen", 1);
        assert!(
            matches!(Manifest::from_json(&bad_url), Err(ManifestError::Listing { ref listing, .. }) if listing == "screen")
        );
        let bad_net = EXAMPLE.replacen(
            "\"network\": \"hedera:testnet\"",
            "\"network\": \"eip155:1\"",
            1,
        );
        assert!(matches!(
            Manifest::from_json(&bad_net),
            Err(ManifestError::Listing { .. })
        ));
        let zero_units = EXAMPLE.replacen("\"max_units\": 20", "\"max_units\": 0", 1);
        assert!(matches!(
            Manifest::from_json(&zero_units),
            Err(ManifestError::Listing { .. })
        ));
        let unknown = EXAMPLE.replacen(
            "\"version\": \"1\",",
            "\"version\": \"1\", \"extra\": 1,",
            1,
        );
        assert!(matches!(
            Manifest::from_json(&unknown),
            Err(ManifestError::Json(_))
        ));
    }
}
