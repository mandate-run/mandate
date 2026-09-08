//! Issue 15: the seven scenarios of docs/demo.md as deterministic execution
//! tests. A scripted market quotes, signs real transfers with the fixed key,
//! settles at once and delivers fixture facts; the tests assert which
//! requests were authorized, which quotes were refused before any
//! authorization, the ledger totals, the reservation moves, and the final
//! report's validity. Amounts are USDC atomic units over the tariffs in
//! `sellers/src/tariffs.ts`.

use mandate::analysis::ClaimType;
use mandate::ledger::Ledger;
use mandate::mandate::Mandate;
use mandate::manifest::Manifest;
use mandate::plan::PlanKind;
use mandate::run::{Inputs, Report, Status, execute};
use mandate::testing::{FakeMarket, FakePublisher, MANDATE_TOML, MANIFEST_JSON, ScriptedQuote};

const POOLS: [&str; 5] = [
    "0x1111111111111111111111111111111111111111",
    "0x2222222222222222222222222222222222222222",
    "0x3333333333333333333333333333333333333333",
    "0x4444444444444444444444444444444444444444",
    "0x5555555555555555555555555555555555555555",
];

/// The demo mandate: five pools, 0.0100 USDC, cap 0.0090, provenance off
/// because the fixture's hashes are synthetic.
fn mandate(service: &str, id: &str) -> Mandate {
    let pools = POOLS
        .iter()
        .map(|p| format!("\"{p}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let text = MANDATE_TOML
        .replace("id = \"demo-1\"", &format!("id = \"{id}\""))
        .replace(
            "service = { total = \"0.0100\", asset = \"0.0.429274\" }",
            &format!("service = {{ total = \"{service}\", asset = \"0.0.429274\" }}"),
        )
        .replace(
            "pools = [\"0x88E6A0C2DDD26FEEB64F039A2C41296FCB3F5640\"]",
            &format!("pools = [{pools}]"),
        )
        .replace("degrade = false", "degrade = false\nprovenance_samples = 0");
    Mandate::from_toml(&text, time::OffsetDateTime::now_utc()).expect("mandate loads")
}

async fn run(m: &Mandate, market: &FakeMarket) -> Report {
    let manifest = Manifest::from_json(MANIFEST_JSON).unwrap();
    let http = reqwest::Client::new();
    let inputs = Inputs {
        mandate: m,
        manifest,
        facilitator_url: "https://api.testnet.blocky402.com".to_owned(),
        facilitator_note: None,
        topic: "0.0.10410389".parse().unwrap(),
        hashscan: "https://hashscan.io/testnet".to_owned(),
        ledger_hint: "memory".to_owned(),
        http: &http,
        quiet: true,
    };
    execute(
        inputs,
        Ledger::in_memory().unwrap(),
        market,
        market,
        &FakePublisher,
    )
    .await
    .expect("run completes")
}

fn authorized(market: &FakeMarket) -> Vec<(String, i64)> {
    market.authorized.lock().unwrap().clone()
}

fn round(r: &Report, n: u32) -> &mandate::run::PlanningRound {
    r.planning.iter().find(|p| p.round == n).expect("round")
}

fn plan(r: &Report, n: u32, kind: PlanKind) -> &mandate::plan::Plan {
    round(r, n)
        .plans
        .iter()
        .find(|p| p.kind == kind)
        .expect("plan")
}

fn hot() -> Vec<String> {
    vec![POOLS[1].to_owned()]
}

#[tokio::test]
async fn scenario_1_normal_completion() {
    let market = FakeMarket::new(hot());
    let m = mandate("0.0100", "s1");
    let r = run(&m, &market).await;
    assert_eq!(r.status, Status::Delivered, "{:?}", r.transcript);
    // Round 1: staged expected 0.0033 bound 0.0093 against bundle 0.0080.
    let staged = plan(&r, 1, PlanKind::Staged);
    assert_eq!((staged.expected, staged.bound), (3_300, 9_300));
    assert_eq!(plan(&r, 1, PlanKind::Bundle).expected, 8_000);
    assert_eq!(round(&r, 1).chosen, Some(PlanKind::Staged));
    // Round 2: events plus explanation 0.0023 against investigate 0.0032.
    assert_eq!(plan(&r, 2, PlanKind::Staged).expected, 2_300);
    assert_eq!(plan(&r, 2, PlanKind::Hybrid).expected, 3_200);
    assert_eq!(round(&r, 2).chosen, Some(PlanKind::Staged));
    let auth = authorized(&market);
    assert_eq!(auth.len(), 3);
    assert_eq!(
        &auth[..2],
        &[("screen".to_owned(), 1_000), ("events".to_owned(), 1_500)]
    );
    assert_eq!(auth[2].0, "explain");
    assert!(
        auth[2].1 > 0 && auth[2].1 <= 800,
        "explain at its brief size: {}",
        auth[2].1
    );
    let v = r.validation.as_ref().unwrap();
    assert!(v.passed && v.complete, "{:?}", v.failures);
    assert_eq!(v.coverage.resolved, 5);
    assert_eq!(r.outcomes.len(), 5);
    assert_eq!(r.reservation_moves[0], "reserve explain 0.000800 held");
    assert!(
        r.reservation_moves
            .iter()
            .any(|l| l.starts_with("reserve released")),
        "{:?}",
        r.reservation_moves
    );
    let t = r.totals.as_ref().unwrap();
    assert_eq!(t.held, "0.000000");
    let spent: i64 = auth.iter().map(|a| a.1).sum();
    assert_eq!(t.settled, mandate::mandate::format_amount(spent, 6));
    assert_eq!(
        t.unspent,
        mandate::mandate::format_amount(10_000 - spent, 6)
    );
    assert_eq!(r.receipts.len(), 4);
    assert!(r.explanation.is_some());
}

#[tokio::test]
async fn scenario_2_live_flip_to_the_bundle() {
    let market = FakeMarket::new(hot());
    market.script(
        "investigate",
        &[ScriptedQuote {
            amount: Some(3_000),
            age_s: 0,
        }],
    );
    let m = mandate("0.0100", "s2");
    let r = run(&m, &market).await;
    assert_eq!(r.status, Status::Delivered, "{:?}", r.transcript);
    assert_eq!(round(&r, 1).chosen, Some(PlanKind::Bundle));
    assert_eq!(plan(&r, 1, PlanKind::Bundle).expected, 3_000);
    assert_eq!(authorized(&market), vec![("investigate".to_owned(), 3_000)]);
    assert!(
        r.reservation_moves.is_empty(),
        "one step needs no reservation"
    );
    let v = r.validation.as_ref().unwrap();
    assert!(v.passed && v.complete, "{:?}", v.failures);
    assert_eq!(v.coverage.resolved, 5);
    assert_eq!(r.outcomes.len(), 5);
    assert_eq!(
        r.claims
            .iter()
            .filter(|c| c.kind == ClaimType::LargeEvent)
            .count(),
        1
    );
    assert_eq!(r.totals.as_ref().unwrap().settled, "0.003000");
    assert_eq!(r.receipts.len(), 2);
}

#[tokio::test]
async fn scenario_3_refusal_before_the_first_cent() {
    let market = FakeMarket::new(hot());
    let m = mandate("0.0030", "s3");
    let r = run(&m, &market).await;
    assert_eq!(r.status, Status::Refused);
    assert!(authorized(&market).is_empty());
    let refusal = r
        .refusals
        .iter()
        .find(|x| x.starts_with("REFUSED REQUIREMENT_UNMEETABLE"))
        .unwrap();
    assert!(
        refusal.contains("bound 0.008000 expected 0.003300 available 0.003000"),
        "{refusal}"
    );
    for kind in ["staged", "hybrid", "bundle"] {
        assert!(refusal.contains(&format!("{kind}: bound")), "{refusal}");
    }
    assert_eq!(r.totals.as_ref().unwrap().settled, "0.000000");
    assert_eq!(r.receipts.len(), 2);
}

#[tokio::test]
async fn scenario_4_events_over_ceiling_falls_back_to_the_bundle() {
    let market = FakeMarket::new(hot());
    market.script(
        "events",
        &[ScriptedQuote {
            amount: Some(2_000),
            age_s: 0,
        }],
    );
    let m = mandate("0.0100", "s4");
    let r = run(&m, &market).await;
    assert_eq!(r.status, Status::Delivered, "{:?}", r.transcript);
    let refusal = r
        .refusals
        .iter()
        .find(|x| x.contains("OFF_TARIFF"))
        .unwrap();
    assert!(
        refusal.contains("events ceiling 0.001500 quoted 0.002000 tariff 2026-09-07"),
        "{refusal}"
    );
    assert_eq!(
        authorized(&market),
        vec![
            ("screen".to_owned(), 1_000),
            ("investigate".to_owned(), 3_200)
        ]
    );
    assert_eq!(round(&r, 2).chosen, Some(PlanKind::Hybrid));
    assert!(!plan(&r, 2, PlanKind::Staged).feasible);
    assert_eq!(
        r.reservation_moves,
        vec![
            "reserve explain 0.000800 held".to_owned(),
            "reserve explain 0.000800 released: plan has one step left".to_owned(),
        ]
    );
    let v = r.validation.as_ref().unwrap();
    assert!(v.passed && v.complete, "{:?}", v.failures);
    assert_eq!(v.coverage.resolved, 5);
    // Every pool keeps the header of the purchase that covers it.
    let quiet_blocks = r.blocks[POOLS[0]];
    let hot_blocks = r.blocks[POOLS[1]];
    assert_ne!(quiet_blocks, hot_blocks);
    let hot_tvl = r
        .claims
        .iter()
        .find(|c| c.pool == POOLS[1] && c.kind == ClaimType::TvlChange)
        .unwrap();
    assert!(hot_tvl.evidence[0].ends_with(&format!("@{}", hot_blocks.0)));
    let quiet_tvl = r
        .claims
        .iter()
        .find(|c| c.pool == POOLS[0] && c.kind == ClaimType::TvlChange)
        .unwrap();
    assert!(quiet_tvl.evidence[0].ends_with(&format!("@{}", quiet_blocks.0)));
    assert_eq!(r.totals.as_ref().unwrap().settled, "0.004200");
    assert_eq!(r.totals.as_ref().unwrap().held, "0.000000");
    assert!(r.explanation.is_some());
}

#[tokio::test]
async fn scenario_5_observed_work_after_the_screen() {
    // Four material pools: events plus explanation 0.0068 ties investigate 0.0068, fewer authorizations win.
    let four: Vec<String> = POOLS[..4].iter().map(|p| p.to_string()).collect();
    let market = FakeMarket::new(four);
    let m = mandate("0.0100", "s5a");
    let r = run(&m, &market).await;
    assert_eq!(r.status, Status::Delivered, "{:?}", r.transcript);
    assert_eq!(plan(&r, 2, PlanKind::Staged).expected, 6_800);
    assert_eq!(plan(&r, 2, PlanKind::Hybrid).expected, 6_800);
    assert_eq!(round(&r, 2).chosen, Some(PlanKind::Hybrid));
    assert_eq!(
        authorized(&market),
        vec![
            ("screen".to_owned(), 1_000),
            ("investigate".to_owned(), 6_800)
        ]
    );
    assert_eq!(r.validation.as_ref().unwrap().coverage.resolved, 5);
    // Five material pools: 0.0083 against 0.0080.
    let five: Vec<String> = POOLS.iter().map(|p| p.to_string()).collect();
    let market = FakeMarket::new(five);
    let m = mandate("0.0100", "s5b");
    let r = run(&m, &market).await;
    assert_eq!(plan(&r, 2, PlanKind::Staged).expected, 8_300);
    assert_eq!(plan(&r, 2, PlanKind::Hybrid).expected, 8_000);
    assert_eq!(
        authorized(&market),
        vec![
            ("screen".to_owned(), 1_000),
            ("investigate".to_owned(), 8_000)
        ]
    );
    assert_eq!(r.status, Status::Delivered, "{:?}", r.transcript);
}

#[tokio::test]
async fn scenario_6_a_stale_quote_is_refreshed_before_planning() {
    // The first investigate quote arrives expired; the fresh one keeps the price: bundle at 0.0030.
    let market = FakeMarket::new(hot());
    market.script(
        "investigate",
        &[
            ScriptedQuote {
                amount: Some(3_000),
                age_s: 130,
            },
            ScriptedQuote {
                amount: Some(3_000),
                age_s: 0,
            },
        ],
    );
    let m = mandate("0.0100", "s6a");
    let r = run(&m, &market).await;
    assert_eq!(r.status, Status::Delivered, "{:?}", r.transcript);
    let quoted: Vec<(String, i64)> = market.quoted.lock().unwrap().clone();
    assert_eq!(
        quoted.iter().filter(|q| q.0 == "investigate").count(),
        2,
        "{quoted:?}"
    );
    assert!(
        r.transcript.iter().any(|l| l.contains("arrived expired")),
        "{:?}",
        r.transcript
    );
    assert_eq!(authorized(&market), vec![("investigate".to_owned(), 3_000)]);
    // The fresh price is higher: the ranking flips back to staged and nothing pays the stale amount.
    let market = FakeMarket::new(hot());
    market.script(
        "investigate",
        &[
            ScriptedQuote {
                amount: Some(3_000),
                age_s: 130,
            },
            ScriptedQuote {
                amount: Some(3_500),
                age_s: 0,
            },
        ],
    );
    let m = mandate("0.0100", "s6b");
    let r = run(&m, &market).await;
    assert_eq!(round(&r, 1).chosen, Some(PlanKind::Staged));
    assert_eq!(plan(&r, 1, PlanKind::Bundle).expected, 3_500);
    let auth = authorized(&market);
    assert_eq!(auth[0], ("screen".to_owned(), 1_000));
    assert!(!auth.iter().any(|a| a.1 == 3_000));
    assert_eq!(r.reservation_moves[0], "reserve explain 0.000800 held");
}

#[tokio::test]
async fn a_tampered_bundle_is_rejected_and_the_run_refuses() {
    let mut market = FakeMarket::new(hot());
    market.tamper_bundle = true;
    market.script(
        "investigate",
        &[ScriptedQuote {
            amount: Some(3_000),
            age_s: 0,
        }],
    );
    let m = mandate("0.0100", "s8");
    let r = run(&m, &market).await;
    assert_eq!(r.status, Status::Refused, "{:?}", r.transcript);
    assert_eq!(r.rejected_deliveries.len(), 1);
    assert_eq!(r.rejected_deliveries[0].0, "investigate");
    assert!(
        r.refusals
            .iter()
            .any(|x| x.contains("EVIDENCE_INSUFFICIENT") && x.contains("rejected: calculation")),
        "{:?}",
        r.refusals
    );
    assert!(r.receipts.iter().any(|x| x.outcome == "failed"));
    assert!(r.explanation.is_none());
}
