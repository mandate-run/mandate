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
