//! Fixtures shared by unit and integration tests: real signed transfers from
//! a fixed throwaway key, so every accounting field a test sees came out of
//! bytes and the same inputs always give the same bytes. Not part of the
//! runtime's behavior.

use std::str::FromStr;

use serde_json::{Map, json};
use time::{Duration, OffsetDateTime};

use hedera::PrivateKey;

use std::collections::BTreeMap;

use crate::evidence::{
    Counts, Event, EventHit, EventsResponse, Header, HourRow, LargeEvents, PoolEvents, PoolScreen,
    ScreenResponse, Snapshot, Sums, TxRef, UnvaluedEvents, Verdict, Window,
};
use crate::hedera::{Asset, HederaAccountId, Signer, Transfer, sign_transfer, valid_duration};
use crate::ledger::{BindingError, MandateRow, PreparedPayment, Request};
use crate::x402::{PaymentPayload, PaymentRequired, Requirement, encode_header};

pub const PAYER: &str = "0.0.10399984";
pub const PAY_TO: &str = "0.0.10409989";
pub const FEE_PAYER: &str = "0.0.7162784";
/// A throwaway ED25519 key with a fixed seed. Never funded.
const SEED_DER: &str = "302e020100300506032b6570042204200202020202020202020202020202020202020202020202020202020202020202";

/// The mandate row the tests share: 0.0100 USDC service, 0.5 HBAR audit.
pub fn mandate_row() -> MandateRow {
    MandateRow {
        id: "m1".to_owned(),
        mandate_hash: "a".repeat(64),
        manifest_hash: "b".repeat(64),
        service_total: 10_000,
        service_asset: "0.0.429274".to_owned(),
        audit_total: 50_000_000,
        max_single_payment: 9_000,
        deadline: time::macros::datetime!(2026-09-13 16:00 UTC),
    }
}

pub fn request() -> Request {
    Request {
        method: "POST".to_owned(),
        url: "http://127.0.0.1:4021/events".to_owned(),
        headers: vec![("content-type".to_owned(), "application/json".to_owned())],
        body: br#"{"pools":["0xabc"]}"#.to_vec(),
    }
}

/// The fixed-seed signer, for tests that sign without a network.
pub fn test_signer() -> Signer {
    Signer::new(
        HederaAccountId::from_str(PAYER).unwrap(),
        PrivateKey::from_str(SEED_DER).expect("fixed key"),
    )
}

/// A within-tariff quote for the events listing, received at `received_at`.
pub fn quote_fixture(amount: i64, received_at: OffsetDateTime) -> crate::quote::Quote {
    let accepted = Requirement {
        scheme: "exact".to_owned(),
        network: "hedera:testnet".to_owned(),
        amount: amount.to_string(),
        pay_to: PAY_TO.to_owned(),
        max_timeout_seconds: 120,
        asset: "0.0.429274".to_owned(),
        extra: Some(json!({ "feePayer": FEE_PAYER })),
    };
    let required = PaymentRequired {
        x402_version: 2,
        error: None,
        resource: None,
        accepts: vec![accepted.clone()],
        extensions: Map::new(),
    };
    let body = br#"{"pools":["0xabc"]}"#.to_vec();
    crate::quote::Quote {
        listing_id: "events".to_owned(),
        amount,
        asset: "0.0.429274".to_owned(),
        network: "hedera:testnet".to_owned(),
        pay_to: PAY_TO.to_owned(),
        fee_payer: Some(FEE_PAYER.to_owned()),
        max_timeout_s: 120,
        received_at: received_at
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap(),
        latency_ms: 1,
        ceiling: amount,
        within_tariff: true,
        listing_match: true,
        fee_payer_ok: true,
        tariff_version: "t".to_owned(),
        decimals: 6,
        request: crate::quote::QuoteRequest {
            method: "POST".to_owned(),
            url: "http://127.0.0.1:4021/events".to_owned(),
            body_hash: crate::x402::sha256_hex(&body),
            body,
            units: 1,
        },
        required,
        accepted,
    }
}

/// A real signed transfer of `amount` in `asset`, distinct per `n`, wrapped
/// in a `PAYMENT-SIGNATURE` header whose accepted terms match the bytes.
pub fn signed_payment(n: u32, amount: i64, asset: &str, now: OffsetDateTime) -> PreparedPayment {
    signed_payment_for(n, amount, asset, now, |_| {}).expect("terms match the bytes")
}

/// Like [`signed_payment`], with a hook that edits the accepted terms before
/// the header is built, for tests that need the terms to disagree with the bytes.
pub fn signed_payment_for(
    n: u32,
    amount: i64,
    asset: &str,
    now: OffsetDateTime,
    edit: impl FnOnce(&mut Requirement),
) -> Result<PreparedPayment, BindingError> {
    let signer = Signer::new(
        HederaAccountId::from_str(PAYER).unwrap(),
        PrivateKey::from_str(SEED_DER).expect("fixed key"),
    );
    let nodes = [HederaAccountId::from_str("0.0.3").unwrap()];
    let transfer = Transfer {
        fee_payer: HederaAccountId::from_str(FEE_PAYER).unwrap(),
        pay_to: HederaAccountId::from_str(PAY_TO).unwrap(),
        asset: Asset::parse(asset).unwrap(),
        amount,
        node_account_ids: &nodes,
        valid_start: now - Duration::seconds(5) + Duration::nanoseconds(i64::from(n)),
        valid_duration: valid_duration(120),
    };
    let signed = sign_transfer(&signer, &transfer).expect("signs");
    let mut accepted = Requirement {
        scheme: "exact".to_owned(),
        network: "hedera:testnet".to_owned(),
        amount: amount.to_string(),
        pay_to: PAY_TO.to_owned(),
        max_timeout_seconds: 120,
        asset: asset.to_owned(),
        extra: Some(json!({ "feePayer": FEE_PAYER })),
    };
    edit(&mut accepted);
    let required = PaymentRequired {
        x402_version: 2,
        error: None,
        resource: None,
        accepts: vec![accepted.clone()],
        extensions: Map::new(),
    };
    let payment_id = format!("pay_{n:0>32}");
    let header = encode_header(&PaymentPayload::new(
        &required,
        &accepted,
        signed.base64(),
        &payment_id,
    ));
    PreparedPayment::from_signature(&header, &payment_id)
}

/// The example mandate, as `docs/spec.md` section 2.1 shows it.
pub const MANDATE_TOML: &str = r#"
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

/// The demo pool and a 24 h window, for evidence fixtures.
pub const POOL: &str = "0x88e6a0c2ddd26feeb64f039a2c41296fcb3f5640";
pub const FROM: u64 = 1_788_800_400;
pub const TO: u64 = 1_788_886_800;

fn snapshot(t0: &str, t1: &str) -> Snapshot {
    Snapshot {
        tvl_token0: t0.to_owned(),
        tvl_token1: t1.to_owned(),
        tvl_usd: Some("1000".to_owned()),
        liquidity: "1".to_owned(),
        token0_price_usd: Some("1".to_owned()),
        token1_price_usd: Some("2000".to_owned()),
    }
}

/// The header every fixture response shares: indexed a minute past the window.
pub fn header_fixture() -> Header {
    Header {
        deployment_id: "Qm".to_owned(),
        block_start: 100,
        block_end: 200,
        block_end_timestamp: TO - 5,
        indexed_block: 210,
        indexed_block_timestamp: Some(TO + 60),
        indexing_errors: false,
        window_requested: Window { from: FROM, to: TO },
        window_covered: Window { from: FROM, to: TO },
        coverage_shortfall: false,
        truncated: false,
    }
}

/// One pool's screen: token0 TVL 1000 to 1100 when `material`, else to 1010;
/// a 250000 USD swap hit when `large_swap`; two hourly rows.
pub fn screen_fixture(material: bool, large_swap: bool) -> ScreenResponse {
    let mut pools = BTreeMap::new();
    // The sellers' order: large_event reasons first, then tvl_change.
    let mut reasons = Vec::new();
    if large_swap {
        reasons.push("large_event:swap".to_owned());
    }
    if material {
        reasons.push("tvl_change:token0".to_owned());
    }
    pools.insert(
        POOL.to_owned(),
        PoolScreen {
            start: Some(snapshot("1000", "500")),
            end: Some(snapshot(if material { "1100" } else { "1010" }, "500")),
            absent_at_start: false,
            hours: vec![
                HourRow {
                    period_start_unix: FROM,
                    tvl_usd: "1".to_owned(),
                    volume_usd: "100.5".to_owned(),
                    tx_count: "3".to_owned(),
                },
                HourRow {
                    period_start_unix: FROM + 3600,
                    tvl_usd: "1".to_owned(),
                    volume_usd: "200".to_owned(),
                    tx_count: "4".to_owned(),
                },
            ],
            large_events: LargeEvents {
                swap: large_swap.then(|| EventHit {
                    transaction: TxRef {
                        id: "0xswap".to_owned(),
                    },
                    amount_usd: "250000".to_owned(),
                }),
                ..Default::default()
            },
            unvalued_events: UnvaluedEvents::default(),
            truncated: false,
            coverage_shortfall: false,
            verdict: if material || large_swap {
                Verdict::Material
            } else {
                Verdict::NonMaterial
            },
            reasons,
            error: None,
        },
    );
    ScreenResponse {
        header: header_fixture(),
        pools,
        requests: 4,
    }
}

/// One held swap in transaction `0xswap`, valued at `amount` or unvalued.
pub fn events_fixture(amount: Option<&str>, truncated: bool) -> EventsResponse {
    let mut pools = BTreeMap::new();
    pools.insert(
        POOL.to_owned(),
        PoolEvents {
            swaps: vec![Event {
                id: "s1".to_owned(),
                transaction: TxRef {
                    id: "0xswap".to_owned(),
                },
                log_index: Some(1),
                timestamp: FROM + 1,
                amount0: "1".to_owned(),
                amount1: "-1".to_owned(),
                amount_usd: amount.map(str::to_owned),
                origin: "0xo".to_owned(),
                owner: None,
                tick_lower: None,
                tick_upper: None,
            }],
            mints: vec![],
            burns: vec![],
            counts: Counts {
                swap: 1,
                mint: 0,
                burn: 0,
            },
            sum_amount_usd: Sums {
                swap: amount.unwrap_or("0").to_owned(),
                mint: "0".to_owned(),
                burn: "0".to_owned(),
            },
            amount_usd_nulls: u64::from(amount.is_none()),
            truncated,
        },
    );
    EventsResponse {
        header: header_fixture(),
        cap: 5000,
        pools,
        requests: 8,
    }
}

/// A 66-byte transaction hash derived from a pool and a serial, so the
/// screen and the events builders cite the same hashes for the same pool.
pub fn pool_tx(pool: &str, n: u64) -> String {
    format!("0x{n:0>24x}{}", &pool.to_lowercase()[2..])
}

/// A screen over `pools` for `window`, each pool material or quiet, with an
/// observation header derived from `head` so two purchases can differ:
/// blocks `head - 7100` to `head - 100`, indexed at `head`, timestamp
/// `window.to + 60`. Material pools move token0 TVL 1000 to 1100 and carry a
/// 250000 USD swap hit; quiet pools move to 1010 with no hit.
pub fn screen_for(pools: &[(String, bool)], window: Window, head: u64) -> ScreenResponse {
    let mut out = BTreeMap::new();
    for (pool, material) in pools {
        let hours = (0..(window.to - window.from) / 3600)
            .map(|h| HourRow {
                period_start_unix: window.from + h * 3600,
                tvl_usd: "4500000".to_owned(),
                volume_usd: if *material {
                    "30000".to_owned()
                } else {
                    "1500.25".to_owned()
                },
                tx_count: if *material {
                    "40".to_owned()
                } else {
                    "2".to_owned()
                },
            })
            .collect();
        out.insert(
            pool.clone(),
            PoolScreen {
                start: Some(snapshot("1000", "500")),
                end: Some(snapshot(if *material { "1100" } else { "1010" }, "500")),
                absent_at_start: false,
                hours,
                large_events: LargeEvents {
                    swap: material.then(|| EventHit {
                        transaction: TxRef {
                            id: pool_tx(pool, 1),
                        },
                        amount_usd: "250000".to_owned(),
                    }),
                    ..Default::default()
                },
                unvalued_events: UnvaluedEvents::default(),
                truncated: false,
                coverage_shortfall: false,
                verdict: if *material {
                    Verdict::Material
                } else {
                    Verdict::NonMaterial
                },
                reasons: if *material {
                    vec![
                        "large_event:swap".to_owned(),
                        "tvl_change:token0".to_owned(),
                    ]
                } else {
                    vec![]
                },
                error: None,
            },
        );
    }
    ScreenResponse {
        header: header_for(window, head),
        pools: out,
        requests: 3 + pools.len() as u64,
    }
}

pub fn header_for(window: Window, head: u64) -> Header {
    Header {
        deployment_id: "QmFixture".to_owned(),
        block_start: head - 7100,
        block_end: head - 100,
        block_end_timestamp: window.to - 11,
        indexed_block: head,
        indexed_block_timestamp: Some(window.to + 60),
        indexing_errors: false,
        window_requested: window,
        window_covered: window,
        coverage_shortfall: false,
        truncated: false,
    }
}

/// Events for `pools` in `window`: for each pool the 250000 USD swap the
/// screen cites plus two smaller ones.
pub fn events_for(pools: &[String], window: Window, head: u64) -> EventsResponse {
    let mut out = BTreeMap::new();
    for pool in pools {
        let swap = |n: u64, amount: &str, offset: u64| Event {
            id: format!("{pool}#{n}"),
            transaction: TxRef {
                id: pool_tx(pool, n),
            },
            log_index: Some(3),
            timestamp: window.from + offset,
            amount0: "125000".to_owned(),
            amount1: "-56.25".to_owned(),
            amount_usd: Some(amount.to_owned()),
            origin: "0x000000000000000000000000000000000000fee1".to_owned(),
            owner: None,
            tick_lower: None,
            tick_upper: None,
        };
        out.insert(
            pool.clone(),
            PoolEvents {
                swaps: vec![
                    swap(1, "250000", 600),
                    swap(2, "40000.75", 1200),
                    swap(3, "1500", 1800),
                ],
                mints: vec![],
                burns: vec![],
                counts: Counts {
                    swap: 3,
                    mint: 0,
                    burn: 0,
                },
                sum_amount_usd: Sums {
                    swap: "291500.75".to_owned(),
                    mint: "0".to_owned(),
                    burn: "0".to_owned(),
                },
                amount_usd_nulls: 0,
                truncated: false,
            },
        );
    }
    EventsResponse {
        header: header_for(window, head),
        cap: 5000,
        pools: out,
        requests: 3 + 3 * pools.len() as u64,
    }
}

/// The four listings of the concept doc in USDC atomic units, served at the
/// reference sellers' addresses.
pub const MANIFEST_JSON: &str = r#"{"version":"2026-09-07","listings":[
  {"id":"screen","seller":"mandate-sellers","url":"http://127.0.0.1:4021/screen","method":"post","capability":"screen","produces":"screening","tariff":{"version":"2026-09-07","base":0,"unit":"pool","unit_price":200,"max_units":20},"network":"hedera:testnet","asset":"0.0.429274","pay_to":"0.0.10409989"},
  {"id":"events","seller":"mandate-sellers","url":"http://127.0.0.1:4021/events","method":"post","capability":"events","produces":"transaction","tariff":{"version":"2026-09-07","base":0,"unit":"pool_window","unit_price":1500,"max_units":20},"network":"hedera:testnet","asset":"0.0.429274","pay_to":"0.0.10409989"},
  {"id":"investigate","seller":"mandate-sellers","url":"http://127.0.0.1:4021/investigate","method":"post","capability":"investigate","produces":"report","tariff":{"version":"2026-09-07","base":2000,"unit":"pool","unit_price":1200,"max_units":20},"network":"hedera:testnet","asset":"0.0.429274","pay_to":"0.0.10409989"},
  {"id":"explain","seller":"mandate-sellers","url":"http://127.0.0.1:4021/explain","method":"post","capability":"explain","produces":"report","tariff":{"version":"2026-09-07","base":0,"unit":"input_kb","unit_price":100,"max_units":8},"network":"hedera:testnet","asset":"0.0.429274","pay_to":"0.0.10409989"}
]}"#;

/// One scripted 402: `amount` or the ceiling when `None`, received `age_s`
/// ago so a quote can arrive already expired.
#[derive(Debug, Clone, Copy)]
pub struct ScriptedQuote {
    pub amount: Option<i64>,
    pub age_s: i64,
}

/// A deterministic market for execution tests: it quotes from scripts,
/// signs real transfers with the fixed key, settles every authorization at
/// once, and delivers fixture facts shaped to the request. It records what
/// was quoted and what was authorized.
pub struct FakeMarket {
    signer: Signer,
    mirror: crate::hedera::MirrorNode,
    http: reqwest::Client,
    pub scripts: std::sync::Mutex<BTreeMap<String, std::collections::VecDeque<ScriptedQuote>>>,
    /// Pools the fixture treats as material.
    pub material: Vec<String>,
    pub thresholds: (String, String),
    head: std::sync::Mutex<u64>,
    /// Deliver bundles whose outcomes and claims are empty.
    pub tamper_bundle: bool,
    pub quoted: std::sync::Mutex<Vec<(String, i64)>>,
    pub authorized: std::sync::Mutex<Vec<(String, i64)>>,
}

impl FakeMarket {
    pub fn new(material: Vec<String>) -> Self {
        Self {
            signer: test_signer(),
            mirror: crate::hedera::MirrorNode::new(reqwest::Client::new(), "http://127.0.0.1:9"),
            http: reqwest::Client::new(),
            scripts: std::sync::Mutex::new(BTreeMap::new()),
            material,
            thresholds: ("0.05".to_owned(), "100000".to_owned()),
            head: std::sync::Mutex::new(25_000_000),
            tamper_bundle: false,
            quoted: std::sync::Mutex::new(Vec::new()),
            authorized: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Scripts the quotes of one listing in order; the last one repeats.
    pub fn script(&self, listing: &str, quotes: &[ScriptedQuote]) {
        self.scripts
            .lock()
            .unwrap()
            .insert(listing.to_owned(), quotes.iter().copied().collect());
    }

    fn next_head(&self) -> u64 {
        let mut h = self.head.lock().unwrap();
        *h += 500;
        *h
    }

    fn deliver(&self, listing: &str, body: &[u8]) -> Vec<u8> {
        let v: serde_json::Value = serde_json::from_slice(body).unwrap_or(serde_json::Value::Null);
        let pools: Vec<String> = v["pools"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|p| p.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let window = Window {
            from: v["window"]["from"].as_u64().unwrap_or(0),
            to: v["window"]["to"].as_u64().unwrap_or(0),
        };
        let head = self.next_head();
        let flags: Vec<(String, bool)> = pools
            .iter()
            .map(|p| (p.clone(), self.material.contains(p)))
            .collect();
        match listing {
            "screen" => serde_json::to_vec(&screen_for(&flags, window, head)).unwrap(),
            "events" => serde_json::to_vec(&events_for(&pools, window, head)).unwrap(),
            "investigate" => {
                let screen = screen_for(&flags, window, head);
                let hot: Vec<String> = flags
                    .iter()
                    .filter(|(_, m)| *m)
                    .map(|(p, _)| p.clone())
                    .collect();
                let events = (!hot.is_empty()).then(|| events_for(&hot, window, head));
                let t = crate::analysis::Thresholds {
                    materiality: &self.thresholds.0,
                    min_event_usd: &self.thresholds.1,
                };
                let (mut outcomes, mut claims) = crate::analysis::outcomes_and_claims(
                    &screen,
                    events.as_ref(),
                    crate::mandate::Evidence::Transaction,
                    t,
                );
                if self.tamper_bundle {
                    outcomes.clear();
                    claims.clear();
                }
                let prose = outcomes
                    .iter()
                    .map(|o| format!("Pool {} is {}.", o.pool, o.outcome.as_str()))
                    .collect::<Vec<_>>()
                    .join(" ");
                serde_json::to_vec(&json!({
                    "screen": screen,
                    "events": events,
                    "outcomes": outcomes,
                    "claims": claims,
                    "explanation": { "prose": prose, "model": "fake", "input_bytes": 1 },
                    "requests": 9,
                }))
                .unwrap()
            }
            _ => {
                let outcomes = v["brief"]["o"].as_array().cloned().unwrap_or_default();
                let prose = outcomes
                    .iter()
                    .map(|o| {
                        format!(
                            "Pool {} is {}.",
                            o["p"].as_str().unwrap_or("?"),
                            o["o"].as_str().unwrap_or("?")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                serde_json::to_vec(
                    &json!({ "prose": prose, "model": "fake", "input_bytes": body.len() }),
                )
                .unwrap()
            }
        }
    }
}

impl crate::run::Quoting for FakeMarket {
    fn signers(&self) -> Vec<String> {
        vec![FEE_PAYER.to_owned()]
    }

    async fn quote(
        &self,
        listing: &crate::manifest::Listing,
        body: Vec<u8>,
        now: OffsetDateTime,
    ) -> Result<crate::quote::Quote, crate::quote::QuoteError> {
        let shape = crate::manifest::RequestShape::from_body(&body)
            .map_err(crate::quote::QuoteError::RequestTooLarge)?;
        let units = crate::manifest::units_for(listing.tariff.unit, &shape)
            .map_err(crate::quote::QuoteError::RequestTooLarge)?;
        let ceiling = listing
            .ceiling(units)
            .map_err(crate::quote::QuoteError::RequestTooLarge)?;
        let scripted = {
            let mut scripts = self.scripts.lock().unwrap();
            match scripts.get_mut(&listing.id) {
                Some(q) if q.len() > 1 => q.pop_front(),
                Some(q) => q.front().copied(),
                None => None,
            }
        };
        let amount = scripted.and_then(|s| s.amount).unwrap_or(ceiling);
        let received = now - Duration::seconds(scripted.map(|s| s.age_s).unwrap_or(0));
        self.quoted
            .lock()
            .unwrap()
            .push((listing.id.clone(), amount));
        let accepted = Requirement {
            scheme: "exact".to_owned(),
            network: listing.network.clone(),
            amount: amount.to_string(),
            pay_to: listing.pay_to.clone(),
            max_timeout_seconds: 120,
            asset: listing.asset.clone(),
            extra: Some(json!({ "feePayer": FEE_PAYER })),
        };
        let required = PaymentRequired {
            x402_version: 2,
            error: None,
            resource: Some(json!({ "url": listing.url })),
            accepts: vec![accepted.clone()],
            extensions: Map::new(),
        };
        Ok(crate::quote::Quote {
            listing_id: listing.id.clone(),
            amount,
            asset: listing.asset.clone(),
            network: listing.network.clone(),
            pay_to: listing.pay_to.clone(),
            fee_payer: Some(FEE_PAYER.to_owned()),
            max_timeout_s: 120,
            received_at: received
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap(),
            latency_ms: 1,
            ceiling,
            within_tariff: amount <= ceiling,
            listing_match: true,
            fee_payer_ok: true,
            tariff_version: listing.tariff.version.clone(),
            decimals: crate::mandate::asset_decimals(&listing.asset).unwrap_or(0),
            request: crate::quote::QuoteRequest {
                method: listing.method.as_str().to_owned(),
                url: listing.url.clone(),
                body_hash: crate::x402::sha256_hex(&body),
                body,
                units,
            },
            required,
            accepted,
        })
    }
}

impl crate::run::Paying for FakeMarket {
    async fn prepare(
        &self,
        ledger: &mut crate::ledger::Ledger,
        mandate_id: &str,
        step: &str,
        reservation_id: Option<i64>,
        quote: &crate::quote::Quote,
    ) -> Result<crate::ledger::Authorization, crate::purchase::PayError> {
        let payer = crate::purchase::Payer {
            signer: &self.signer,
            mirror: &self.mirror,
            http: &self.http,
            poll: std::time::Duration::from_secs(1),
            max_retrievals: 1,
        };
        let nodes = [HederaAccountId::from_str("0.0.3").unwrap()];
        let a = payer.sign_and_persist(ledger, mandate_id, step, reservation_id, quote, &nodes)?;
        self.authorized
            .lock()
            .unwrap()
            .push((step.to_owned(), a.amount));
        Ok(a)
    }

    async fn settle(
        &self,
        ledger: &mut crate::ledger::Ledger,
        id: i64,
        _deadline: OffsetDateTime,
        say: &mut dyn FnMut(String),
    ) -> Result<crate::purchase::Purchase, crate::purchase::PayError> {
        let now = OffsetDateTime::now_utc();
        let a = ledger.commit_submission(id, now)?;
        let body = self.deliver(&a.step, &a.request.body);
        ledger.record_delivery(id, &body, Some("fake"), now)?;
        let settlement = crate::hedera::Settlement::Settled {
            consensus_timestamp: "1.000000001".to_owned(),
            duplicates_ignored: 0,
        };
        let a = ledger.record_settlement(id, &settlement, now)?;
        say(format!(
            "{} payment settled: record matches; records 1, duplicates ignored 0",
            a.step
        ));
        Ok(crate::purchase::Purchase {
            body: Some(body),
            authorization: a,
            settlement,
            latency_ms: Some(1),
            records: 1,
        })
    }
}

/// Publishes to nowhere: every receipt is marked with a synthetic sequence.
pub struct FakePublisher;

impl crate::run::Publishing for FakePublisher {
    async fn publish_pending(
        &self,
        ledger: &mut crate::ledger::Ledger,
        mandate_id: &str,
        say: &mut dyn FnMut(String),
    ) -> Result<Vec<crate::publish::Published>, crate::ledger::LedgerError> {
        let mut out = Vec::new();
        for seq in ledger.unpublished(mandate_id)? {
            let hcs = 1000 + seq;
            ledger.mark_published(mandate_id, seq, hcs, "0.0.1@1.1", OffsetDateTime::now_utc())?;
            say(format!(
                "receipt {seq} published as HCS sequence {hcs} (fake)"
            ));
            out.push(crate::publish::Published {
                seq,
                hcs_sequence: hcs,
                tx_id: "0.0.1@1.1".to_owned(),
            });
        }
        Ok(out)
    }

    async fn reconcile_audit(
        &self,
        _ledger: &mut crate::ledger::Ledger,
        _mandate_id: &str,
    ) -> Result<(), crate::purchase::ReconcileError> {
        Ok(())
    }
}
