//! Spec sections 2.3 and 4: quoting through a client that cannot pay. The
//! client holds no signer, never retries and never follows redirects. It
//! sends the real request without `PAYMENT-SIGNATURE`, records the 402 as a
//! quote, and judges it against the pinned listing: `ceiling`,
//! `within_tariff`, `listing_match` and `fee_payer_ok`. Everything a quote
//! is judged on comes from the request the client itself sent: the unit
//! count from the body, the binding from the wire request.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::config::PublicConfig;
use crate::mandate::{asset_decimals, format_amount};
use crate::manifest::{Listing, Method, RequestShape, TariffError, units_for};
use crate::refusal::{Code, Refusal};
use crate::x402::{self, PaymentRequired, Requirement, sha256_hex};

pub const DEFAULT_QUOTE_TIMEOUT: Duration = Duration::from_secs(15);
/// The only protocol version the client interprets.
pub const SUPPORTED_X402_VERSION: u32 = 2;
/// A quote is usable for at most this long, whatever the seller advertises.
pub const MAX_QUOTE_LIFETIME_S: u64 = 120;

#[derive(Debug, thiserror::Error)]
pub enum QuoteError {
    #[error("SELLER_UNREACHABLE: {0}")]
    Unreachable(String),
    #[error("SELLER_UNREACHABLE: answered {status}, not 402")]
    NotPaymentRequired { status: u16 },
    #[error(
        "OUTSIDE_CONSTRAINTS: answered {status} redirect to {location:?}; paid requests never follow redirects"
    )]
    Redirect {
        status: u16,
        location: Option<String>,
    },
    #[error("SELLER_UNREACHABLE: 402 without a decodable PAYMENT-REQUIRED: {0}")]
    Header(String),
    #[error("OUTSIDE_CONSTRAINTS: 402 lists no payment requirement")]
    NoAccepts,
    #[error("OUTSIDE_CONSTRAINTS: amount {0:?} is not a non-negative integer within i64")]
    Amount(String),
    #[error("OUTSIDE_CONSTRAINTS: request size: {0}")]
    RequestTooLarge(TariffError),
    #[error(
        "OUTSIDE_CONSTRAINTS: listing {listing} is on {network}; this runtime is on {configured}"
    )]
    WrongNetwork {
        listing: String,
        network: String,
        configured: String,
    },
    #[error(
        "OUTSIDE_CONSTRAINTS: x402 version {0} is not supported; only {SUPPORTED_X402_VERSION}"
    )]
    Version(u32),
    #[error("facilitator {url} /supported: {problem}")]
    Supported { url: String, problem: String },
}

impl QuoteError {
    pub fn code(&self) -> Code {
        match self {
            Self::Unreachable(_) | Self::NotPaymentRequired { .. } | Self::Header(_) => {
                Code::SellerUnreachable
            }
            Self::Redirect { .. }
            | Self::NoAccepts
            | Self::Amount(_)
            | Self::RequestTooLarge(_)
            | Self::WrongNetwork { .. }
            | Self::Version(_) => Code::OutsideConstraints,
            Self::Supported { .. } => Code::SellerUnreachable,
        }
    }
}

/// The request a quote binds, section 2.3: local metadata, not x402. Built
/// by the client from the listing and the body it transmits, never supplied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteRequest {
    pub method: String,
    pub url: String,
    #[serde(with = "body_base64")]
    pub body: Vec<u8>,
    pub body_hash: String,
    /// Units under the listing's tariff, read from the body.
    pub units: u64,
}

mod body_base64 {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize as _, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        STANDARD.decode(text).map_err(serde::de::Error::custom)
    }
}

impl QuoteRequest {
    /// The binding of the request the client is about to send: the listing's
    /// method and URL, this body, its hash, and the unit count read from it.
    fn for_listing(listing: &Listing, body: Vec<u8>) -> Result<Self, QuoteError> {
        let shape = RequestShape::from_body(&body).map_err(QuoteError::RequestTooLarge)?;
        let units = units_for(listing.tariff.unit, &shape).map_err(QuoteError::RequestTooLarge)?;
        Ok(Self {
            method: listing.method.as_str().to_owned(),
            url: listing.url.clone(),
            body_hash: sha256_hex(&body),
            body,
            units,
        })
    }
}

/// Section 2.3. One 402, judged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Quote {
    pub listing_id: String,
    pub amount: i64,
    pub asset: String,
    pub network: String,
    pub pay_to: String,
    pub fee_payer: Option<String>,
    /// The seller's `maxTimeoutSeconds`, kept as advertised for the payment.
    pub max_timeout_s: u64,
    pub received_at: String,
    pub latency_ms: u64,
    pub ceiling: i64,
    pub within_tariff: bool,
    pub listing_match: bool,
    pub fee_payer_ok: bool,
    pub tariff_version: String,
    /// Decimals of the listing's asset, for the human-readable refusal.
    pub decimals: u32,
    pub request: QuoteRequest,
    /// What the seller sent, kept whole for the payload that pays it.
    pub required: PaymentRequired,
    /// The `accepts` entry this quote is.
    pub accepted: Requirement,
}

impl Quote {
    /// Usable within `received_at + min(max_timeout_s, 120 s)`. The seller's
    /// `maxTimeoutSeconds` stays on the wire requirement for the payment; the
    /// usable lifetime never exceeds the validity a signed transfer can have.
    pub fn valid_until(&self) -> OffsetDateTime {
        let received = OffsetDateTime::parse(&self.received_at, &Rfc3339)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH);
        let seconds = self.max_timeout_s.min(MAX_QUOTE_LIFETIME_S);
        received
            .checked_add(time::Duration::seconds(seconds as i64))
            .unwrap_or(received)
    }

    pub fn usable_at(&self, now: OffsetDateTime) -> bool {
        now < self.valid_until()
    }

    /// I3: a quote pays only when it matches the listing, the fee payer is the
    /// facilitator's, and the amount is within the tariff.
    pub fn refusal(&self) -> Option<Refusal> {
        if !self.listing_match {
            return Some(Refusal {
                code: Code::OutsideConstraints,
                detail: format!(
                    "{} quoted network {} asset {} pay_to {} url {}, not the pinned listing",
                    self.listing_id,
                    self.network,
                    self.asset,
                    self.pay_to,
                    self.required_url().unwrap_or("?")
                ),
            });
        }
        if !self.fee_payer_ok {
            return Some(Refusal {
                code: Code::OutsideConstraints,
                detail: format!(
                    "{} fee payer {:?} is not a facilitator signer",
                    self.listing_id, self.fee_payer
                ),
            });
        }
        if !self.within_tariff {
            return Some(Refusal {
                code: Code::OffTariff,
                detail: format!(
                    "{} ceiling {} quoted {} tariff {}",
                    self.listing_id,
                    format_amount(self.ceiling, self.decimals),
                    format_amount(self.amount, self.decimals),
                    self.tariff_version
                ),
            });
        }
        None
    }

    fn required_url(&self) -> Option<&str> {
        self.required.resource.as_ref()?.get("url")?.as_str()
    }
}

/// A step not yet quotable, estimated at its ceiling, section 4 step 3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Estimate {
    pub listing_id: String,
    pub units: u64,
    pub ceiling: i64,
}

/// The client that cannot pay. Built from a `PublicConfig`, so a process
/// that only quotes never has a key to load.
pub struct Quoter {
    http: reqwest::Client,
    network: String,
    signers: Vec<String>,
    timeout: Duration,
}

impl std::fmt::Debug for Quoter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Quoter")
            .field("network", &self.network)
            .field("signers", &self.signers)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct Supported {
    #[serde(default)]
    signers: std::collections::BTreeMap<String, Vec<String>>,
}

impl Quoter {
    /// Fetches the facilitator's `/supported` once and keeps its Hedera signers.
    pub async fn from_config(cfg: &PublicConfig, timeout: Duration) -> Result<Self, QuoteError> {
        let http = build_http(timeout)?;
        let url = format!("{}/supported", cfg.facilitator_url.trim_end_matches('/'));
        let resp = http
            .get(&url)
            .send()
            .await
            .map_err(|e| QuoteError::Supported {
                url: url.clone(),
                problem: e.to_string(),
            })?;
        if !resp.status().is_success() {
            return Err(QuoteError::Supported {
                url,
                problem: format!("status {}", resp.status()),
            });
        }
        let supported: Supported = resp.json().await.map_err(|e| QuoteError::Supported {
            url: url.clone(),
            problem: e.to_string(),
        })?;
        let network = cfg.network.caip2().to_owned();
        let mut signers = Vec::new();
        for key in ["hedera:*", network.as_str()] {
            if let Some(list) = supported.signers.get(key) {
                signers.extend(list.iter().cloned());
            }
        }
        Self::with_signers(network, signers, timeout)
    }

    /// A quoter with known facilitator signers; tests and offline use.
    pub fn with_signers(
        network: String,
        signers: Vec<String>,
        timeout: Duration,
    ) -> Result<Self, QuoteError> {
        Ok(Self {
            http: build_http(timeout)?,
            network,
            signers,
            timeout,
        })
    }

    pub fn signers(&self) -> &[String] {
        &self.signers
    }

    /// One 402 for one listing. Never retries; a redirect is refused, not
    /// followed. A listing outside the configured network is refused before
    /// anything is sent. The unit count comes from `body`, the binding from
    /// the request actually transmitted.
    pub async fn quote(
        &self,
        listing: &Listing,
        body: Vec<u8>,
        now: OffsetDateTime,
    ) -> Result<Quote, QuoteError> {
        if listing.network != self.network {
            return Err(QuoteError::WrongNetwork {
                listing: listing.id.clone(),
                network: listing.network.clone(),
                configured: self.network.clone(),
            });
        }
        let request = QuoteRequest::for_listing(listing, body)?;
        let ceiling = listing
            .ceiling(request.units)
            .map_err(QuoteError::RequestTooLarge)?;
        let mut builder = match listing.method {
            Method::Get => self.http.get(&listing.url),
            Method::Post => self.http.post(&listing.url),
        };
        if !request.body.is_empty() {
            builder = builder
                .header("content-type", "application/json")
                .body(request.body.clone());
        }
        let started = Instant::now();
        let resp = builder
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|e| QuoteError::Unreachable(e.to_string()))?;
        let latency_ms = started.elapsed().as_millis() as u64;
        let status = resp.status();
        if status.is_redirection() {
            return Err(QuoteError::Redirect {
                status: status.as_u16(),
                location: resp
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned),
            });
        }
        if status.as_u16() != 402 {
            return Err(QuoteError::NotPaymentRequired {
                status: status.as_u16(),
            });
        }
        let header = resp
            .headers()
            .get(x402::HEADER_REQUIRED)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| QuoteError::Header("header missing".to_owned()))?
            .to_owned();
        let required: PaymentRequired =
            x402::decode_header(&header).map_err(|e| QuoteError::Header(e.to_string()))?;
        if required.x402_version != SUPPORTED_X402_VERSION {
            return Err(QuoteError::Version(required.x402_version));
        }
        if required.accepts.is_empty() {
            return Err(QuoteError::NoAccepts);
        }
        let url_ok = required
            .resource
            .as_ref()
            .and_then(|r| r.get("url"))
            .and_then(|u| u.as_str())
            .map(|u| same_url(u, &listing.url))
            .unwrap_or(false);
        let matches_listing = |r: &Requirement| {
            r.scheme == "exact"
                && r.network == listing.network
                && r.asset == listing.asset
                && r.pay_to == listing.pay_to
        };
        let (accepted, listing_match) = match required.accepts.iter().find(|r| matches_listing(r)) {
            Some(r) => (r.clone(), url_ok),
            None => (
                required
                    .accepts
                    .iter()
                    .find(|r| r.scheme == "exact" && r.network == self.network)
                    .unwrap_or(&required.accepts[0])
                    .clone(),
                false,
            ),
        };
        let amount = parse_amount(&accepted.amount)?;
        let fee_payer = accepted.fee_payer().map(str::to_owned);
        let fee_payer_ok = fee_payer
            .as_deref()
            .is_some_and(|f| self.signers.iter().any(|s| s == f));
        Ok(Quote {
            listing_id: listing.id.clone(),
            amount,
            asset: accepted.asset.clone(),
            network: accepted.network.clone(),
            pay_to: accepted.pay_to.clone(),
            fee_payer,
            max_timeout_s: accepted.max_timeout_seconds,
            received_at: now.format(&Rfc3339).unwrap_or_default(),
            latency_ms,
            ceiling,
            within_tariff: amount <= ceiling,
            listing_match,
            fee_payer_ok,
            tariff_version: listing.tariff.version.clone(),
            decimals: asset_decimals(&listing.asset).unwrap_or(0),
            request,
            required,
            accepted,
        })
    }
}

fn build_http(timeout: Duration) -> Result<reqwest::Client, QuoteError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
        .map_err(|e| QuoteError::Unreachable(e.to_string()))
}

fn same_url(a: &str, b: &str) -> bool {
    match (url::Url::parse(a), url::Url::parse(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Non-negative integer within i64, else OUTSIDE_CONSTRAINTS.
pub fn parse_amount(text: &str) -> Result<i64, QuoteError> {
    let t = text.trim();
    if t.is_empty() || !t.chars().all(|c| c.is_ascii_digit()) {
        return Err(QuoteError::Amount(text.to_owned()));
    }
    t.parse::<i64>()
        .map_err(|_| QuoteError::Amount(text.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use time::macros::datetime;

    const NOW: OffsetDateTime = datetime!(2026-09-08 09:00 UTC);
    const FEE_PAYER: &str = "0.0.7162784";

    /// Serves exactly one HTTP response on `port` and hands back the request text.
    fn serve_on(
        port: u16,
        status: &str,
        headers: Vec<(String, String)>,
        body: &str,
    ) -> std::sync::mpsc::Receiver<String> {
        let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let status = status.to_owned();
        let body = body.to_owned();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = vec![0u8; 65536];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let mut out = format!(
                "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n",
                body.len()
            );
            for (k, v) in &headers {
                out.push_str(&format!("{k}: {v}\r\n"));
            }
            out.push_str("\r\n");
            out.push_str(&body);
            stream.write_all(out.as_bytes()).unwrap();
            stream.flush().ok();
            tx.send(request).ok();
        });
        rx
    }

    fn free_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    }

    fn listing(base: &str, method: &str, unit: &str, unit_price: i64, network: &str) -> Listing {
        let m = Manifest::from_json(&format!(
            r#"{{"version":"1","listings":[{{"id":"screen","seller":"s","url":"{base}/screen","method":"{method}",
            "capability":"screen","produces":"screening",
            "tariff":{{"version":"t1","base":0,"unit":"{unit}","unit_price":{unit_price},"max_units":20}},
            "network":"{network}","asset":"0.0.429274","pay_to":"0.0.10409989"}}]}}"#
        ))
        .unwrap();
        m.listings[0].clone()
    }

    fn required_header(accepts: &str, url: &str, version: u32) -> (String, String) {
        let json = format!(
            r#"{{"x402Version":{version},"error":"Payment required","resource":{{"url":"{url}"}},"accepts":[{accepts}],"extensions":{{"payment-identifier":{{"info":{{"required":true}},"schema":{{}}}}}}}}"#
        );
        (
            "PAYMENT-REQUIRED".to_owned(),
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, json),
        )
    }

    fn accept(amount: &str, pay_to: &str, asset: &str, fee_payer: &str, timeout: u64) -> String {
        format!(
            r#"{{"scheme":"exact","network":"hedera:testnet","amount":"{amount}","asset":"{asset}","payTo":"{pay_to}","maxTimeoutSeconds":{timeout},"extra":{{"feePayer":"{fee_payer}"}}}}"#
        )
    }

    fn quoter() -> Quoter {
        Quoter::with_signers(
            "hedera:testnet".to_owned(),
            vec![FEE_PAYER.to_owned()],
            Duration::from_secs(5),
        )
        .unwrap()
    }

    fn body(pools: usize) -> Vec<u8> {
        let list: Vec<String> = (0..pools).map(|i| format!("\"0x{i:040x}\"")).collect();
        format!(
            r#"{{"pools":[{}],"window":{{"from":1788800400,"to":1788886800}}}}"#,
            list.join(",")
        )
        .into_bytes()
    }

    struct Served {
        listing: Listing,
        rx: std::sync::mpsc::Receiver<String>,
    }

    fn serve_402(
        accepts: &str,
        unit_price: i64,
        url_override: Option<&str>,
        version: u32,
    ) -> Served {
        let port = free_port();
        let base = format!("http://127.0.0.1:{port}");
        let listing = listing(&base, "post", "pool", unit_price, "hedera:testnet");
        let url = url_override
            .map(str::to_owned)
            .unwrap_or_else(|| listing.url.clone());
        let rx = serve_on(
            port,
            "402 Payment Required",
            vec![required_header(accepts, &url, version)],
            "{}",
        );
        Served { listing, rx }
    }

    #[tokio::test]
    async fn a_matching_402_becomes_a_quote_bound_to_the_wire_request() {
        let s = serve_402(
            &accept("1000", "0.0.10409989", "0.0.429274", FEE_PAYER, 120),
            200,
            None,
            2,
        );
        let q = quoter().quote(&s.listing, body(5), NOW).await.unwrap();
        let sent = s.rx.recv().unwrap();
        assert!(sent.starts_with("POST /screen HTTP/1.1"), "{sent}");
        assert!(
            !sent.to_lowercase().contains("payment-signature"),
            "never a signature"
        );
        assert!(
            sent.ends_with(&String::from_utf8(body(5)).unwrap()),
            "the real body"
        );
        assert_eq!(
            (q.request.method.as_str(), q.request.url.as_str()),
            ("POST", s.listing.url.as_str())
        );
        assert_eq!(q.request.body_hash, sha256_hex(&body(5)));
        assert_eq!(q.request.units, 5, "units read from the body sent");
        assert_eq!(
            (
                q.amount,
                q.ceiling,
                q.within_tariff,
                q.listing_match,
                q.fee_payer_ok
            ),
            (1000, 1000, true, true, true)
        );
        assert_eq!(q.fee_payer.as_deref(), Some(FEE_PAYER));
        assert_eq!(q.refusal(), None);
        assert!(q.usable_at(NOW + time::Duration::seconds(119)));
        assert!(!q.usable_at(NOW + time::Duration::seconds(120)));
    }

    #[tokio::test]
    async fn units_come_from_the_body_so_one_pool_cannot_buy_a_five_pool_ceiling() {
        let s = serve_402(
            &accept("1000", "0.0.10409989", "0.0.429274", FEE_PAYER, 120),
            200,
            None,
            2,
        );
        let q = quoter().quote(&s.listing, body(1), NOW).await.unwrap();
        assert_eq!(
            (q.request.units, q.ceiling, q.within_tariff),
            (1, 200, false)
        );
        let r = q.refusal().unwrap();
        assert_eq!(r.code, Code::OffTariff);
        assert_eq!(
            r.to_string(),
            "REFUSED OFF_TARIFF screen ceiling 0.000200 quoted 0.001000 tariff t1"
        );
    }

    #[tokio::test]
    async fn a_listing_on_another_network_is_never_contacted() {
        let port = free_port();
        let l = listing(
            &format!("http://127.0.0.1:{port}"),
            "post",
            "pool",
            200,
            "hedera:mainnet",
        );
        let err = quoter().quote(&l, body(1), NOW).await.unwrap_err();
        assert!(matches!(err, QuoteError::WrongNetwork { .. }), "{err}");
        assert_eq!(err.code(), Code::OutsideConstraints);
    }

    #[tokio::test]
    async fn the_usable_lifetime_is_capped_at_120_seconds() {
        let s = serve_402(
            &accept("1000", "0.0.10409989", "0.0.429274", FEE_PAYER, 600),
            200,
            None,
            2,
        );
        let q = quoter().quote(&s.listing, body(5), NOW).await.unwrap();
        assert_eq!(
            q.max_timeout_s, 600,
            "the wire value is kept for the payment"
        );
        assert_eq!(q.accepted.max_timeout_seconds, 600);
        assert!(q.usable_at(NOW + time::Duration::seconds(119)));
        assert!(!q.usable_at(NOW + time::Duration::seconds(121)));
    }

    #[tokio::test]
    async fn an_unsupported_x402_version_is_refused() {
        let s = serve_402(
            &accept("1000", "0.0.10409989", "0.0.429274", FEE_PAYER, 120),
            200,
            None,
            99,
        );
        let err = quoter().quote(&s.listing, body(5), NOW).await.unwrap_err();
        assert!(matches!(err, QuoteError::Version(99)), "{err}");
        assert_eq!(err.code(), Code::OutsideConstraints);
    }

    #[tokio::test]
    async fn several_accepts_only_one_matching_the_listing() {
        let accepts = format!(
            "{},{},{}",
            accept("999", "0.0.5", "0.0.429274", FEE_PAYER, 120),
            accept("900", "0.0.10409989", "0.0.429274", FEE_PAYER, 120),
            accept("1", "0.0.10409989", "0.0.0", FEE_PAYER, 120)
        );
        let s = serve_402(&accepts, 200, None, 2);
        let q = quoter().quote(&s.listing, body(5), NOW).await.unwrap();
        assert_eq!(
            (q.amount, q.listing_match, q.pay_to.as_str()),
            (900, true, "0.0.10409989")
        );
    }

    #[tokio::test]
    async fn wrong_recipient_url_or_fee_payer_is_outside_constraints() {
        let s = serve_402(
            &accept("1000", "0.0.5", "0.0.429274", FEE_PAYER, 120),
            200,
            None,
            2,
        );
        let q = quoter().quote(&s.listing, body(5), NOW).await.unwrap();
        assert!(!q.listing_match);
        assert_eq!(q.refusal().unwrap().code, Code::OutsideConstraints);

        let s = serve_402(
            &accept("1000", "0.0.10409989", "0.0.429274", FEE_PAYER, 120),
            200,
            Some("http://evil.example/screen"),
            2,
        );
        let q = quoter().quote(&s.listing, body(5), NOW).await.unwrap();
        assert!(
            !q.listing_match,
            "resource url must equal the pinned listing url"
        );

        let s = serve_402(
            &accept("1000", "0.0.10409989", "0.0.429274", "0.0.999", 120),
            200,
            None,
            2,
        );
        let q = quoter().quote(&s.listing, body(5), NOW).await.unwrap();
        assert_eq!((q.listing_match, q.fee_payer_ok), (true, false));
        assert_eq!(q.refusal().unwrap().code, Code::OutsideConstraints);
    }

    #[tokio::test]
    async fn bad_amounts_are_outside_constraints() {
        for amount in ["99999999999999999999", "-5", "1.5", "abc", ""] {
            let s = serve_402(
                &accept(amount, "0.0.10409989", "0.0.429274", FEE_PAYER, 120),
                200,
                None,
                2,
            );
            let err = quoter().quote(&s.listing, body(5), NOW).await.unwrap_err();
            assert!(matches!(err, QuoteError::Amount(_)), "{amount}: {err}");
            assert_eq!(err.code(), Code::OutsideConstraints);
        }
    }

    #[tokio::test]
    async fn redirects_are_refused_and_never_followed() {
        let port = free_port();
        let l = listing(
            &format!("http://127.0.0.1:{port}"),
            "get",
            "input_kb",
            0,
            "hedera:testnet",
        );
        let rx = serve_on(
            port,
            "302 Found",
            vec![(
                "location".to_owned(),
                "http://127.0.0.1:1/elsewhere".to_owned(),
            )],
            "",
        );
        let err = quoter().quote(&l, vec![], NOW).await.unwrap_err();
        assert!(
            matches!(err, QuoteError::Redirect { status: 302, .. }),
            "{err}"
        );
        assert_eq!(err.code(), Code::OutsideConstraints);
        let sent = rx.recv().unwrap();
        assert!(sent.starts_with("GET /screen HTTP/1.1"));
    }

    #[tokio::test]
    async fn non_402_answers_and_dead_sellers_are_unreachable() {
        let port = free_port();
        let l = listing(
            &format!("http://127.0.0.1:{port}"),
            "get",
            "input_kb",
            0,
            "hedera:testnet",
        );
        let _rx = serve_on(port, "200 OK", vec![], "{}");
        let err = quoter().quote(&l, vec![], NOW).await.unwrap_err();
        assert!(matches!(
            err,
            QuoteError::NotPaymentRequired { status: 200 }
        ));
        assert_eq!(err.code(), Code::SellerUnreachable);

        let dead = listing(
            &format!("http://127.0.0.1:{}", free_port()),
            "get",
            "input_kb",
            0,
            "hedera:testnet",
        );
        let err = quoter().quote(&dead, vec![], NOW).await.unwrap_err();
        assert!(matches!(err, QuoteError::Unreachable(_)));
    }

    #[tokio::test]
    async fn requests_above_max_units_or_without_a_shape_are_never_sent() {
        let l = listing("http://127.0.0.1:1", "post", "pool", 200, "hedera:testnet");
        let err = quoter().quote(&l, body(21), NOW).await.unwrap_err();
        assert!(matches!(
            err,
            QuoteError::RequestTooLarge(TariffError::AboveMaxUnits {
                units: 21,
                max_units: 20
            })
        ));
        let err = quoter().quote(&l, b"{}".to_vec(), NOW).await.unwrap_err();
        assert!(
            matches!(
                err,
                QuoteError::RequestTooLarge(TariffError::MissingPools(_))
            ),
            "{err}"
        );
        let err = quoter()
            .quote(&l, b"not json".to_vec(), NOW)
            .await
            .unwrap_err();
        assert!(
            matches!(err, QuoteError::RequestTooLarge(TariffError::Body(_))),
            "{err}"
        );
    }
}
