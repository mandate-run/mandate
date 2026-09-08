//! `mandate run`: sections 4, 5, 6, 8, 9, 11 and 12 in order for one
//! mandate. Every decision is printed as it is made and collected into the
//! report; every payment goes through the ledger; every receipt is durable
//! before it is published.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration as StdDuration;

use serde::Serialize;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::analysis::{self, Brief, Claim, Outcome, PoolOutcome, outcomes_and_claims};
use crate::config::Config;
use crate::evidence::{EventsResponse, Explanation, ScreenResponse, Window};
use crate::hedera::{Consensus, HederaTopicId, MirrorNode, Settlement, Signer};
use crate::ledger::{
    Authorization, DeliveryState, Ledger, LedgerError, MandateRow, PaymentState, ReservationSource,
};
use crate::mandate::{Citations, Evidence, Mandate, SellerPolicy, format_amount};
use crate::manifest::{Capability, Listing, Manifest};
use crate::plan::{self, Plan, PlanKind, Situation};
use crate::publish::{FEE_CAP_TINYBAR, Publisher};
use crate::purchase::{PayError, Payer, Purchase};
use crate::quote::{Estimate, Quote, QuoteError, Quoter};
use crate::receipts::{Outcome as ReceiptOutcome, Receipt};
use crate::refusal::{Code, Refusal};
use crate::transcript;
use crate::validate::{self, Delivered, Provenance, Rules, Timed, Validation};
use crate::x402::sha256_hex;

pub const DEFAULT_QUOTE_TIMEOUT: StdDuration = StdDuration::from_secs(15);
/// Paid requests wait this long for a delivery; sellers query the Graph before answering.
pub const DELIVERY_TIMEOUT: StdDuration = StdDuration::from_secs(120);
pub const MIRROR_POLL: StdDuration = StdDuration::from_secs(5);
pub const MAX_RETRIEVALS_PER_RUN: u32 = 4;

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("mandate: {0}")]
    Mandate(#[from] crate::mandate::MandateError),
    #[error("manifest: {0}")]
    Manifest(#[from] crate::manifest::ManifestError),
    #[error("ledger: {0}")]
    Ledger(#[from] LedgerError),
    #[error("mandate {0} already ran in this ledger; use a new id or another --ledger")]
    AlreadyRan(String),
    #[error("facilitator: {0}")]
    Quote(#[from] QuoteError),
    #[error("hedera: {0}")]
    Hedera(#[from] crate::hedera::Error),
    #[error("payment: {0}")]
    Pay(#[from] PayError),
    #[error("publish: {0}")]
    Publish(#[from] crate::purchase::ReconcileError),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("{0}")]
    Other(String),
}

/// Exit status of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Validated and complete.
    Delivered,
    /// Delivered under `degrade` or with a failed validation: see the report.
    DeliveredWithFindings,
    /// A section 2.7 refusal; nothing further was bought.
    Refused,
}

impl Status {
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Delivered => 0,
            Self::DeliveredWithFindings => 4,
            Self::Refused => 3,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StepReport {
    pub step: u32,
    pub listing_id: String,
    pub payment_id: String,
    pub tx_id: String,
    pub amount: String,
    pub submissions: u32,
    pub retrievals: u32,
    pub payment_state: String,
    pub delivery_state: String,
    pub records: usize,
    pub duplicates_ignored: u32,
    pub latency_ms: Option<u64>,
    pub consensus_timestamp: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanningRound {
    pub pending: Vec<String>,
    pub assumption_expected_material: u64,
    pub plans: Vec<Plan>,
    pub chosen: Option<PlanKind>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Totals {
    pub settled: String,
    pub outstanding: String,
    pub unspent: String,
    pub held: String,
    pub audit_spent_tinybar: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReceiptLine {
    pub seq: u64,
    pub outcome: String,
    pub hcs_sequence: Option<u64>,
}

/// The report and transcript, section 12, as one document for `--json`.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub status: Status,
    pub mandate_id: String,
    pub mandate_hash: String,
    pub manifest_hash: String,
    pub pools: Vec<String>,
    pub window: Window,
    pub service_asset: String,
    pub quotes: Vec<Quote>,
    pub estimates: Vec<(String, u64, i64)>,
    pub planning: Vec<PlanningRound>,
    pub steps: Vec<StepReport>,
    pub outcomes: Vec<PoolOutcome>,
    pub claims: Vec<Claim>,
    pub brief_events_used: Option<u32>,
    pub explanation: Option<Explanation>,
    pub validation: Option<Validation>,
    pub refusals: Vec<String>,
    pub anomalies: Vec<String>,
    pub totals: Option<Totals>,
    pub topic: String,
    pub receipts: Vec<ReceiptLine>,
    pub audit_pending: Vec<u64>,
    pub reconcile_notice: Option<String>,
    pub transcript: Vec<String>,
}

/// Prints lines as they happen unless `quiet`, and keeps them for the report.
pub struct Transcript {
    pub quiet: bool,
    pub lines: Vec<String>,
}

impl Transcript {
    pub fn say(&mut self, line: impl Into<String>) {
        let line = line.into();
        if !self.quiet {
            println!("{line}");
        }
        self.lines.push(line);
    }
}

/// Everything a run holds while it moves through the steps.
struct State<'a> {
    mandate: &'a Mandate,
    manifest: Manifest,
    ledger: Ledger,
    quoter: Quoter,
    payer: Payer<'a>,
    publisher: Option<Publisher<'a>>,
    mirror: &'a MirrorNode,
    http: &'a reqwest::Client,
    out: Transcript,
    report: Report,
    window: Window,
    pools: Vec<String>,
    quotes: BTreeMap<(String, u64), i64>,
    unusable: BTreeMap<String, String>,
    live_quotes: BTreeMap<String, Quote>,
    screen: Option<(ScreenResponse, OffsetDateTime)>,
    events: Option<(EventsResponse, OffsetDateTime)>,
    explanation: Option<Explanation>,
    outcomes: Vec<PoolOutcome>,
    claims: Vec<Claim>,
    reservation: Option<(i64, i64, String)>,
    step_no: u32,
    incomplete: bool,
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}

fn evidence_body(pools: &[String], window: Window, mandate: &Mandate) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "pools": pools,
        "window": { "from": window.from, "to": window.to },
        "inputs": { "materiality": mandate.inputs.materiality, "min_event_usd": mandate.inputs.min_event_usd },
    }))
    .expect("body serializes")
}

/// Runs one mandate file. `quiet` suppresses the streamed transcript; the
/// caller prints the report instead.
pub async fn run(
    cfg: &Config,
    mandate_path: &Path,
    ledger_path: &Path,
    id: Option<&str>,
    quiet: bool,
) -> Result<Report, RunError> {
    let started = OffsetDateTime::now_utc();
    let mut mandate = Mandate::from_path(mandate_path, started)?;
    if let Some(id) = id {
        mandate.id = id.to_owned();
    }
    let manifest_path = mandate_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(&mandate.constraints.manifest_path);
    let manifest = Manifest::load_pinned(&manifest_path, &mandate.constraints.manifest_hash)?;
    let signer = cfg.signer()?;
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(DELIVERY_TIMEOUT)
        .build()?;
    let mirror = MirrorNode::new(http.clone(), cfg.mirror_node_url.clone());
    let consensus = signer.consensus(cfg.network);
    let topic: HederaTopicId = mandate
        .duties
        .receipts_topic
        .parse()
        .map_err(|e| RunError::Other(format!("receipts_topic: {e}")))?;
    run_with(
        cfg,
        &mandate,
        manifest,
        ledger_path,
        quiet,
        &signer,
        &http,
        &mirror,
        &consensus,
        topic,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_with(
    cfg: &Config,
    mandate: &Mandate,
    manifest: Manifest,
    ledger_path: &Path,
    quiet: bool,
    signer: &Signer,
    http: &reqwest::Client,
    mirror: &MirrorNode,
    consensus: &Consensus,
    topic: HederaTopicId,
) -> Result<Report, RunError> {
    let now = OffsetDateTime::now_utc();
    let decimals = mandate.budget.service_decimals;
    let pools: Vec<String> = mandate
        .inputs
        .pools
        .iter()
        .map(|p| p.to_lowercase())
        .collect();
    let to = now.unix_timestamp() as u64 / 3600 * 3600;
    let window = Window {
        from: to - u64::from(mandate.inputs.window_h) * 3600,
        to,
    };

    // Section 4 step 1: listings outside the constraints are never contacted.
    let eligible: Vec<Listing> = manifest
        .listings
        .iter()
        .filter(|l| mandate.constraints.networks.contains(&l.network))
        .filter(|l| l.asset == mandate.budget.service_asset)
        .filter(|l| match &mandate.constraints.sellers {
            SellerPolicy::Allowlist(ids) => ids.contains(&l.seller),
            SellerPolicy::Tariffed => true,
        })
        .cloned()
        .collect();
    let filtered = Manifest {
        version: manifest.version.clone(),
        listings: eligible,
        hash: manifest.hash.clone(),
    };

    let mut ledger = Ledger::open(ledger_path)?;
    let row = MandateRow {
        id: mandate.id.clone(),
        mandate_hash: mandate.hash.clone(),
        manifest_hash: manifest.hash.clone(),
        service_total: mandate.budget.service_total,
        service_asset: mandate.budget.service_asset.clone(),
        audit_total: mandate.budget.audit_total,
        max_single_payment: mandate.constraints.max_single_payment,
        deadline: mandate.constraints.deadline,
    };
    if ledger.mandate(&mandate.id).is_ok() && !ledger.authorizations(&mandate.id)?.is_empty() {
        return Err(RunError::AlreadyRan(mandate.id.clone()));
    }
    ledger.insert_mandate(&row, now)?;

    let quoter = Quoter::from_config(&cfg.public(), DEFAULT_QUOTE_TIMEOUT).await?;
    let payer = Payer {
        signer,
        mirror,
        http,
        poll: MIRROR_POLL,
        max_retrievals: MAX_RETRIEVALS_PER_RUN,
    };
    let publisher = Publisher {
        consensus,
        topic,
        fee_cap: FEE_CAP_TINYBAR,
    };
    let report = Report {
        status: Status::Refused,
        mandate_id: mandate.id.clone(),
        mandate_hash: mandate.hash.clone(),
        manifest_hash: manifest.hash.clone(),
        pools: pools.clone(),
        window,
        service_asset: mandate.budget.service_asset.clone(),
        quotes: Vec::new(),
        estimates: Vec::new(),
        planning: Vec::new(),
        steps: Vec::new(),
        outcomes: Vec::new(),
        claims: Vec::new(),
        brief_events_used: None,
        explanation: None,
        validation: None,
        refusals: Vec::new(),
        anomalies: Vec::new(),
        totals: None,
        topic: topic.to_string(),
        receipts: Vec::new(),
        audit_pending: Vec::new(),
        reconcile_notice: None,
        transcript: Vec::new(),
    };
    let mut st = State {
        mandate,
        manifest: filtered,
        ledger,
        quoter,
        payer,
        publisher: Some(publisher),
        mirror,
        http,
        out: Transcript {
            quiet,
            lines: Vec::new(),
        },
        report,
        window,
        pools,
        quotes: BTreeMap::new(),
        unusable: BTreeMap::new(),
        live_quotes: BTreeMap::new(),
        screen: None,
        events: None,
        explanation: None,
        outcomes: Vec::new(),
        claims: Vec::new(),
        reservation: None,
        step_no: 0,
        incomplete: false,
    };

    // Section 12: mandate summary.
    let brief_bound = analysis::mandatory_brief_bound(st.pools.len());
    st.out.say(format!(
        "mandate {} hash {} manifest {} spec {}",
        mandate.id,
        &mandate.hash[..16],
        &manifest.hash[..16],
        crate::receipts::SPEC_VERSION
    ));
    st.out.say(format!(
        "coverage all_material over {} pool(s), window {} to {} ({} h), evidence {:?}, citations {:?}",
        st.pools.len(),
        window.from,
        window.to,
        mandate.inputs.window_h,
        mandate.requirements.evidence.as_str(),
        mandate.requirements.citations.as_str()
    ));
    st.out.say(format!(
        "service budget {} {} cap {} audit {} tinybar; assumption expected_material_pools {}; mandatory brief bound {} bytes",
        format_amount(mandate.budget.service_total, decimals),
        mandate.budget.service_asset,
        format_amount(mandate.constraints.max_single_payment, decimals),
        mandate.budget.audit_total,
        mandate.inputs.expected_material_pools,
        brief_bound
    ));
    st.out.say(format!(
        "listings eligible {} of {}: {}",
        st.manifest.listings.len(),
        manifest.listings.len(),
        st.manifest
            .listings
            .iter()
            .map(|l| l.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    ));

    // Receipt 0, durable then published.
    let r0 = Receipt::start(&mandate.id, &mandate.hash, &manifest.hash, &now_rfc3339());
    st.ledger.append_receipt(&mandate.id, &r0)?;
    publish(&mut st).await?;

    // Section 4: quote what is fully known now, estimate the rest.
    quote_known(&mut st).await;
    let mut estimates = Vec::new();
    for l in &st.manifest.listings {
        if st.live_quotes.contains_key(&l.id) || st.unusable.contains_key(&l.id) {
            continue;
        }
        let units = match l.capability {
            Capability::Events => {
                st.pools.len() as u64
                    * st.window
                        .to
                        .abs_diff(st.window.from)
                        .div_ceil(crate::manifest::WINDOW_SECONDS)
                        .max(1)
            }
            Capability::Explain => l.tariff.max_units,
            _ => st.pools.len() as u64,
        };
        if let Ok(ceiling) = l.ceiling(units) {
            estimates.push(Estimate {
                listing_id: l.id.clone(),
                units,
                ceiling,
            });
        }
    }
    let quotes: Vec<Quote> = st.live_quotes.values().cloned().collect();
    st.out.say(
        transcript::quotes_table(&quotes, &estimates, decimals)
            .trim_end()
            .to_owned(),
    );
    for (id, reason) in &st.unusable {
        st.out.say(format!("{id}: {reason}"));
    }
    st.report.estimates = estimates
        .iter()
        .map(|e| (e.listing_id.clone(), e.units, e.ceiling))
        .collect();

    // Section 5 then 6, 7, 8 until the report exists or a refusal ends the run.
    let outcome = drive(&mut st).await;
    match outcome {
        Ok(()) => {}
        Err(Stop::Refused(r)) => {
            st.out.say(r.to_string());
            st.report.refusals.push(r.to_string());
            let receipt = Receipt {
                seq: 0,
                mandate_id: mandate.id.clone(),
                outcome: ReceiptOutcome::Refused,
                at: now_rfc3339(),
                step: Some(st.step_no + 1),
                listing_id: None,
                seller: None,
                amount: None,
                asset: None,
                tx_id: None,
                payment_id_hash: None,
                request_hash: None,
                response_hash: None,
                reason: Some(r.to_string()),
                latency_ms: None,
                mandate_hash: None,
                manifest_hash: None,
                spec_version: None,
            };
            st.ledger.append_receipt(&mandate.id, &receipt)?;
            st.report.status = Status::Refused;
        }
        Err(Stop::Error(e)) => return Err(e),
    }

    // Section 9 over R, when anything was delivered.
    if let Some((screen, screen_at)) = st.screen.as_ref() {
        let rules = Rules {
            required: &st.pools,
            evidence: mandate.requirements.evidence,
            citations: mandate.requirements.citations,
            max_data_age_s: mandate.requirements.max_data_age_s,
            provenance_samples: mandate.requirements.provenance_samples,
            degrade: mandate.requirements.degrade,
            min_event_usd: &mandate.inputs.min_event_usd,
            input_numbers: vec![
                mandate.inputs.window_h.to_string(),
                st.pools.len().to_string(),
            ],
        };
        let delivered = Delivered {
            screen: Timed {
                body: screen,
                quoted_at: *screen_at,
            },
            events: st.events.as_ref().map(|(e, at)| Timed {
                body: e,
                quoted_at: *at,
            }),
            explanation: st.explanation.as_ref(),
        };
        let mut v = validate::validate(&rules, &delivered, &st.outcomes, &st.claims);
        if let Provenance::Pending { samples } = &v.provenance {
            let rpc = mandate.constraints.eth_rpc.clone().unwrap_or_default();
            let checked = validate::check_provenance(st.http, &rpc, samples).await;
            v = v.with_provenance(checked);
        }
        let provenance = match &v.provenance {
            Provenance::NotApplicable => {
                "provenance not_applicable (no transaction hash cited)".to_owned()
            }
            Provenance::NotRun { cited } => {
                format!("provenance not_run ({cited} hash(es) cited, provenance_samples 0)")
            }
            Provenance::Checked { samples } => format!(
                "provenance {}/{}: {}",
                samples.iter().filter(|s| s.ok).count(),
                samples.len(),
                samples
                    .iter()
                    .map(|s| format!(
                        "{} {}",
                        &s.tx[..12],
                        if s.ok { "ok" } else { s.detail.as_str() }
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Provenance::Pending { .. } => "provenance pending".to_owned(),
        };
        st.out.say(format!(
            "validation {}: coverage {}/{}, calculations {}, references {}, transaction citations {}, prose numbers {}, {provenance}",
            if v.passed { "passed" } else { "failed" },
            v.coverage.resolved,
            v.coverage.required,
            v.calculations,
            v.references,
            v.transaction_citations,
            v.prose_numbers
        ));
        for f in &v.failures {
            st.out.say(format!("  {} {}", f.reason.as_str(), f.detail));
        }
        // Section 6: received -> validated or rejected, one receipt per rejection.
        let reasons: Vec<String> = v.reasons().iter().map(|r| r.as_str().to_owned()).collect();
        let auths = st.ledger.authorizations(&mandate.id)?;
        for a in auths.iter().filter(|a| {
            a.payment_state == PaymentState::Settled && a.delivery_state == DeliveryState::Received
        }) {
            if v.passed {
                st.ledger.mark_validated(a.id, OffsetDateTime::now_utc())?;
            } else {
                let reason = reasons.join(",");
                st.ledger
                    .mark_rejected(a.id, &reason, OffsetDateTime::now_utc())?;
                let mut r = purchase_receipt(&st, a, ReceiptOutcome::Failed, None, None);
                r.reason = Some(format!("validation {reason}"));
                st.ledger.append_receipt(&mandate.id, &r)?;
            }
        }
        st.report.status = if v.passed && v.complete && !st.incomplete {
            Status::Delivered
        } else {
            Status::DeliveredWithFindings
        };
        if !v.complete || st.incomplete {
            st.out.say("report incomplete: screening only".to_owned());
        }
        st.report.validation = Some(v);
    }

    // Section 12: totals, receipts, notices. The mirror node lags a few
    // seconds behind consensus; wait briefly for the last fee records.
    publish(&mut st).await?;
    if let Some(p) = st.publisher.as_ref() {
        for _ in 0..4 {
            p.reconcile_audit(&mut st.ledger, st.mirror, &mandate.id)
                .await?;
            if st.ledger.audit_submitted_charges(&mandate.id)?.is_empty() {
                break;
            }
            tokio::time::sleep(MIRROR_POLL).await;
        }
    }
    let accounts = st.ledger.accounts(&mandate.id)?;
    let audit = st.ledger.audit_accounts(&mandate.id)?;
    let unresolved: Vec<&Authorization> = Vec::new();
    let auths = st.ledger.authorizations(&mandate.id)?;
    let unresolved: Vec<&Authorization> = auths
        .iter()
        .filter(|a| a.payment_state == PaymentState::Unresolved)
        .chain(unresolved)
        .collect();
    st.out.say(format!(
        "totals settled {} outstanding {} held {} unspent {} audit spent {} tinybar",
        format_amount(accounts.settled, decimals),
        format_amount(accounts.outstanding, decimals),
        format_amount(accounts.held, decimals),
        format_amount(accounts.free(), decimals),
        audit.spent()
    ));
    st.report.totals = Some(Totals {
        settled: format_amount(accounts.settled, decimals),
        outstanding: format_amount(accounts.outstanding, decimals),
        unspent: format_amount(accounts.free(), decimals),
        held: format_amount(accounts.held, decimals),
        audit_spent_tinybar: audit.spent(),
    });
    let receipts = st.ledger.receipts(&mandate.id)?;
    st.report.receipts = receipts
        .iter()
        .map(|(r, seq)| ReceiptLine {
            seq: r.seq,
            outcome: r.outcome_str().to_owned(),
            hcs_sequence: *seq,
        })
        .collect();
    st.report.audit_pending = st.ledger.unpublished(&mandate.id)?;
    st.out.say(format!(
        "HCS topic {} receipts {}{}; {}/topic/{}",
        topic,
        receipts
            .iter()
            .map(|(r, s)| format!(
                "{}:{}",
                r.seq,
                s.map(|s| s.to_string())
                    .unwrap_or_else(|| "pending".to_owned())
            ))
            .collect::<Vec<_>>()
            .join(" "),
        if st.report.audit_pending.is_empty() {
            String::new()
        } else {
            format!(" audit_pending {:?}", st.report.audit_pending)
        },
        cfg.network.hashscan(),
        topic
    ));
    if !unresolved.is_empty() {
        let notice = format!(
            "unresolved authorizations {}: run `mandate reconcile {} --ledger {}`",
            unresolved
                .iter()
                .map(|a| a.tx_id.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            mandate.id,
            ledger_path.display()
        );
        st.out.say(notice.clone());
        st.report.reconcile_notice = Some(notice);
    }
    st.report.outcomes = st.outcomes.clone();
    st.report.claims = st.claims.clone();
    st.report.explanation = st.explanation.clone();
    st.report.transcript = std::mem::take(&mut st.out.lines);
    Ok(st.report)
}

/// Why the step loop stopped early.
enum Stop {
    Refused(Refusal),
    Error(RunError),
}

impl<E: Into<RunError>> From<E> for Stop {
    fn from(e: E) -> Self {
        Stop::Error(e.into())
    }
}

async fn publish(st: &mut State<'_>) -> Result<(), RunError> {
    let Some(p) = st.publisher.as_ref() else {
        return Ok(());
    };
    let mut lines = Vec::new();
    p.publish_pending(&mut st.ledger, &st.mandate.id, &mut |l| lines.push(l))
        .await?;
    for l in lines {
        st.out.say(l);
    }
    Ok(())
}

/// Quotes screen and investigate over R now; events and explain wait for their inputs.
async fn quote_known(st: &mut State<'_>) {
    let body = evidence_body(&st.pools, st.window, st.mandate);
    let listings: Vec<Listing> = st.manifest.listings.clone();
    for l in listings
        .iter()
        .filter(|l| matches!(l.capability, Capability::Screen | Capability::Investigate))
    {
        quote_listing(st, l, body.clone()).await;
    }
}

/// One live quote, recorded for planning or as unusable with its reason.
async fn quote_listing(st: &mut State<'_>, listing: &Listing, body: Vec<u8>) -> Option<Quote> {
    let now = OffsetDateTime::now_utc();
    match st.quoter.quote(listing, body, now).await {
        Ok(q) => {
            st.report.quotes.push(q.clone());
            if let Some(r) = q.refusal() {
                st.out.say(format!(
                    "{} quoted {}: {r}",
                    listing.id,
                    format_amount(q.amount, q.decimals)
                ));
                st.unusable.insert(listing.id.clone(), r.to_string());
                st.report.refusals.push(format!("{}: {r}", listing.id));
                st.live_quotes.remove(&listing.id);
                None
            } else {
                st.out.say(format!(
                    "{} {} live, within ceiling {} ({} units, {} ms)",
                    listing.id,
                    format_amount(q.amount, q.decimals),
                    format_amount(q.ceiling, q.decimals),
                    q.request.units,
                    q.latency_ms
                ));
                st.quotes
                    .insert((listing.id.clone(), q.request.units), q.amount);
                st.unusable.remove(&listing.id);
                st.live_quotes.insert(listing.id.clone(), q.clone());
                Some(q)
            }
        }
        Err(e) => {
            let r = Refusal {
                code: e.code(),
                detail: e.to_string(),
            };
            st.out.say(format!("{}: {r}", listing.id));
            st.unusable.insert(listing.id.clone(), r.to_string());
            st.report.refusals.push(format!("{}: {r}", listing.id));
            None
        }
    }
}

fn pending_pools(st: &State<'_>) -> Vec<String> {
    match &st.screen {
        None => st.pools.clone(),
        Some(_) => st
            .outcomes
            .iter()
            .filter(|o| o.outcome.pending_work())
            .map(|o| o.pool.clone())
            .collect(),
    }
}

/// Pools that a purchase can still resolve: `pending` ones. `undetermined`
/// pools carry incomplete facts no purchase repairs.
fn resolvable_pools(st: &State<'_>) -> Vec<String> {
    st.outcomes
        .iter()
        .filter(|o| o.outcome == Outcome::Pending)
        .map(|o| o.pool.clone())
        .collect()
}

fn plan_round(st: &mut State<'_>, pending: &[String]) -> Result<Plan, Refusal> {
    let accounts = st.ledger.accounts(&st.mandate.id).map_err(|e| Refusal {
        code: Code::OverBudget,
        detail: e.to_string(),
    })?;
    let free = accounts.free() + st.reservation.as_ref().map(|r| r.1).unwrap_or(0);
    let sit = Situation {
        manifest: &st.manifest,
        required: st.pools.len() as u64,
        unscreened: if st.screen.is_none() {
            st.pools.len() as u64
        } else {
            0
        },
        pending: pending.len() as u64,
        expected_material: u64::from(st.mandate.inputs.expected_material_pools),
        window_seconds: st.window.to.abs_diff(st.window.from),
        free,
        max_single_payment: st.mandate.constraints.max_single_payment,
        decimals: st.mandate.budget.service_decimals,
        quotes: &st.quotes,
        unusable: &st.unusable,
    };
    let plans = plan::price_all(&sit);
    let chosen = plan::choose(&plans, &sit).map(|p| p.kind);
    let decimals = sit.decimals;
    for p in &plans {
        let steps = p
            .expected_steps
            .iter()
            .map(|s| {
                format!(
                    "{} {} {}",
                    s.listing_id,
                    format_amount(s.amount, decimals),
                    s.source.as_str()
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let verdict = match &chosen {
            Ok(k) if *k == p.kind => "chosen".to_owned(),
            _ if p.feasible => "feasible".to_owned(),
            _ => format!("rejected: {}", p.reasons.join("; ")),
        };
        st.out.say(format!(
            "plan {} expected {} bound {} authorizations {} [{steps}] {verdict}",
            p.kind.as_str(),
            format_amount(p.expected, decimals),
            format_amount(p.bound, decimals),
            p.authorizations
        ));
    }
    let picked = chosen
        .as_ref()
        .ok()
        .and_then(|k| plans.iter().find(|p| p.kind == *k).cloned());
    st.report.planning.push(PlanningRound {
        pending: pending.to_vec(),
        assumption_expected_material: sit.expected_material,
        plans,
        chosen: chosen.as_ref().ok().copied(),
    });
    match (chosen, picked) {
        (Ok(_), Some(p)) => Ok(p),
        (Err(r), _) => Err(r),
        (Ok(k), None) => Err(Refusal {
            code: Code::RequirementUnmeetable,
            detail: format!("plan {} vanished", k.as_str()),
        }),
    }
}

/// Section 5 and the step loop: plan over W, buy the first step, recompute, repeat.
async fn drive(st: &mut State<'_>) -> Result<(), Stop> {
    let mut pending = st.pools.clone();
    let mut first = true;
    loop {
        let plan = match plan_round(st, &pending) {
            Ok(p) => p,
            Err(r) if first => return Err(Stop::Refused(r)),
            Err(r) => {
                // A pending pool remains and no plan can pay for it.
                let r = Refusal {
                    code: Code::EvidenceInsufficient,
                    detail: format!("{} pool(s) pending; {}", pending.len(), r.detail),
                };
                if st.mandate.requirements.degrade {
                    st.out.say(format!("{r}; degrade: delivering incomplete"));
                    st.report.refusals.push(r.to_string());
                    st.incomplete = true;
                    return Ok(());
                }
                return Err(Stop::Refused(r));
            }
        };
        // I4: hold the final step before any non-final authorization.
        if first
            && st.mandate.budget.reserve_completion
            && plan.bound_steps.len() > 1
            && st.reservation.is_none()
        {
            let last = plan.bound_steps.last().expect("steps");
            let r = st.ledger.hold(
                &st.mandate.id,
                &last.listing_id,
                last.amount,
                ReservationSource::CeilingAtMax,
                OffsetDateTime::now_utc(),
            )?;
            st.out.say(format!(
                "reserve {} {} held",
                last.listing_id,
                format_amount(last.amount, st.mandate.budget.service_decimals)
            ));
            st.reservation = Some((r.id, r.amount, last.listing_id.clone()));
        }
        first = false;
        let Some(next) = plan.expected_steps.first().cloned() else {
            return Ok(());
        };
        let done = match next.capability {
            Capability::Screen => {
                buy_screen(st).await?;
                false
            }
            Capability::Events => {
                buy_events(st, &pending).await?;
                false
            }
            Capability::Explain => {
                buy_explain(st).await?;
                true
            }
            Capability::Investigate => {
                buy_investigate(st, &pending).await?;
                true
            }
        };
        if done {
            return Ok(());
        }
        // Section 7: what remains after this delivery.
        let still = pending_pools(st);
        let resolvable = resolvable_pools(st);
        let stuck: Vec<&String> = still.iter().filter(|p| !resolvable.contains(p)).collect();
        if !stuck.is_empty() {
            let r = Refusal {
                code: Code::EvidenceInsufficient,
                detail: format!(
                    "undetermined with incomplete facts, no purchase resolves: {}",
                    stuck
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            };
            if st.mandate.requirements.degrade {
                st.out
                    .say(format!("{r}; degrade: continuing to the explanation"));
                st.report.refusals.push(r.to_string());
                st.incomplete = true;
            } else {
                return Err(Stop::Refused(r));
            }
        }
        pending = resolvable;
    }
}

fn listing_with(st: &State<'_>, cap: Capability) -> Result<Listing, Stop> {
    st.manifest
        .with_capability(cap)
        .next()
        .cloned()
        .ok_or_else(|| {
            Stop::Refused(Refusal {
                code: Code::RequirementUnmeetable,
                detail: format!("no {cap:?} listing").to_lowercase(),
            })
        })
}

/// A usable quote for `listing` with `body`: the live one when still valid and
/// bound to the same body, else a fresh 402.
async fn usable_quote(st: &mut State<'_>, listing: &Listing, body: Vec<u8>) -> Result<Quote, Stop> {
    let now = OffsetDateTime::now_utc();
    if let Some(q) = st.live_quotes.get(&listing.id)
        && q.usable_at(now + time::Duration::seconds(10))
        && q.request.body == body
    {
        return Ok(q.clone());
    }
    match quote_listing(st, listing, body).await {
        Some(q) => Ok(q),
        None => Err(Stop::Refused(Refusal {
            code: Code::SellerUnreachable,
            detail: st
                .unusable
                .get(&listing.id)
                .cloned()
                .unwrap_or_else(|| format!("{}: no usable quote", listing.id)),
        })),
    }
}

/// One paid step: prepare, settle, receipt. Returns the purchase and its body.
async fn buy(
    st: &mut State<'_>,
    listing: &Listing,
    quote: &Quote,
    reservation_id: Option<i64>,
) -> Result<Purchase, Stop> {
    let now = OffsetDateTime::now_utc();
    let step_name = listing.id.clone();
    let a = match st
        .payer
        .prepare(
            &mut st.ledger,
            &st.mandate.id,
            &step_name,
            reservation_id,
            quote,
            now,
        )
        .await
    {
        Ok(a) => a,
        Err(PayError::Ledger(LedgerError::OverBudget { amount, free })) => {
            let accounts = st.ledger.accounts(&st.mandate.id).map_err(RunError::from)?;
            let code = if amount <= free + accounts.held {
                Code::ReserveViolation
            } else {
                Code::OverBudget
            };
            return Err(Stop::Refused(Refusal {
                code,
                detail: format!(
                    "{} needs {} with {} free",
                    listing.id,
                    format_amount(amount, quote.decimals),
                    format_amount(free, quote.decimals)
                ),
            }));
        }
        Err(PayError::Ledger(
            e @ (LedgerError::AboveSinglePayment { .. }
            | LedgerError::PastDeadline { .. }
            | LedgerError::AssetMismatch { .. }),
        )) => {
            return Err(Stop::Refused(Refusal {
                code: Code::OutsideConstraints,
                detail: e.to_string(),
            }));
        }
        Err(PayError::Refused(r)) => return Err(Stop::Refused(r)),
        Err(e) => return Err(e.into()),
    };
    st.step_no += 1;
    st.out.say(format!(
        "{} authorized: {} {} payment {} tx {} valid until {}",
        listing.id,
        format_amount(a.amount, quote.decimals),
        a.asset,
        a.payment_id,
        a.tx_id,
        a.valid_until.format(&Rfc3339).unwrap_or_default()
    ));
    if let Some((_, held, _)) = st.reservation.as_ref()
        && reservation_id.is_some()
        && *held > a.amount
    {
        st.out.say(format!(
            "reserve released {}",
            format_amount(held - a.amount, quote.decimals)
        ));
    }
    if reservation_id.is_some() {
        st.reservation = None;
    }
    let mut lines = Vec::new();
    let purchase = st
        .payer
        .settle(
            &mut st.ledger,
            a.id,
            st.mandate.constraints.deadline,
            &mut |l| lines.push(l),
        )
        .await?;
    for l in lines {
        st.out.say(l);
    }
    let a = &purchase.authorization;
    st.report.steps.push(StepReport {
        step: st.step_no,
        listing_id: listing.id.clone(),
        payment_id: a.payment_id.clone(),
        tx_id: a.tx_id.clone(),
        amount: format_amount(a.amount, quote.decimals),
        submissions: a.submissions,
        retrievals: a.retrievals,
        payment_state: a.payment_state.as_str().to_owned(),
        delivery_state: a.delivery_state.as_str().to_owned(),
        records: purchase.records,
        duplicates_ignored: a.duplicates_ignored,
        latency_ms: purchase.latency_ms,
        consensus_timestamp: a.consensus_timestamp.clone(),
    });
    let outcome = match (&purchase.settlement, a.payment_state) {
        (Settlement::Settled { .. }, _) => ReceiptOutcome::Paid,
        (Settlement::Failed { .. } | Settlement::Anomaly { .. }, _) => ReceiptOutcome::Failed,
        (Settlement::Absent { .. }, PaymentState::Unresolved) => ReceiptOutcome::Unresolved,
        (Settlement::Absent { .. }, _) => ReceiptOutcome::Unresolved,
    };
    let reason = match &purchase.settlement {
        Settlement::Failed { results, .. } => Some(results.join(",")),
        Settlement::Anomaly { results, .. } => Some(format!("anomaly {}", results.join(","))),
        Settlement::Absent { .. } => Some("no record".to_owned()),
        Settlement::Settled { .. } if a.delivery_state == DeliveryState::None => {
            Some("settled without delivery".to_owned())
        }
        Settlement::Settled { .. } => None,
    };
    let receipt = purchase_receipt(st, a, outcome, purchase.latency_ms, reason);
    st.ledger
        .append_receipt(&st.mandate.id, &receipt)
        .map_err(RunError::from)?;
    publish(st).await?;
    if a.payment_state == PaymentState::Failed && a.delivery_state != DeliveryState::None {
        st.report
            .anomalies
            .push(format!("unpaid_delivery {} {}", listing.id, a.tx_id));
    }
    if a.payment_state != PaymentState::Settled || a.delivery_state == DeliveryState::None {
        return Err(Stop::Refused(Refusal {
            code: if a.payment_state == PaymentState::Unresolved {
                Code::PaymentUnresolved
            } else {
                Code::EvidenceInsufficient
            },
            detail: format!(
                "{} payment {} delivery {}",
                listing.id,
                a.payment_state.as_str(),
                a.delivery_state.as_str()
            ),
        }));
    }
    Ok(purchase)
}

fn purchase_receipt(
    st: &State<'_>,
    a: &Authorization,
    outcome: ReceiptOutcome,
    latency_ms: Option<u64>,
    reason: Option<String>,
) -> Receipt {
    let seller = st
        .manifest
        .listings
        .iter()
        .find(|l| l.id == a.step)
        .map(|l| l.seller.clone());
    Receipt {
        seq: 0,
        mandate_id: st.mandate.id.clone(),
        outcome,
        at: now_rfc3339(),
        step: Some(st.step_no),
        listing_id: Some(a.step.clone()),
        seller,
        amount: Some(format_amount(a.amount, st.mandate.budget.service_decimals)),
        asset: Some(a.asset.clone()),
        tx_id: Some(a.tx_id.clone()),
        payment_id_hash: Some(sha256_hex(a.payment_id.as_bytes())),
        request_hash: Some(a.request.hash()),
        response_hash: a.response_hash.clone(),
        reason,
        latency_ms,
        mandate_hash: None,
        manifest_hash: None,
        spec_version: None,
    }
}

fn parse_body<T: serde::de::DeserializeOwned>(what: &str, body: &[u8]) -> Result<T, Stop> {
    serde_json::from_slice(body).map_err(|e| {
        Stop::Refused(Refusal {
            code: Code::EvidenceInsufficient,
            detail: format!("{what}: response does not match the schema: {e}"),
        })
    })
}

fn recompute(st: &mut State<'_>) {
    let Some((screen, _)) = st.screen.as_ref() else {
        return;
    };
    let (o, c) = outcomes_and_claims(
        screen,
        st.events.as_ref().map(|(e, _)| e),
        st.mandate.requirements.evidence,
        &st.mandate.inputs.min_event_usd,
    );
    let summary: BTreeMap<&str, usize> = o.iter().fold(BTreeMap::new(), |mut m, o| {
        *m.entry(o.outcome.as_str()).or_default() += 1;
        m
    });
    st.out.say(format!(
        "outcomes: {}; claims {}",
        summary
            .iter()
            .map(|(k, v)| format!("{v} {k}"))
            .collect::<Vec<_>>()
            .join(", "),
        c.len()
    ));
    for po in &o {
        st.out.say(format!(
            "  {} {} [{}]",
            po.pool,
            po.outcome.as_str(),
            po.reasons.join(", ")
        ));
    }
    st.outcomes = o;
    st.claims = c;
}

async fn buy_screen(st: &mut State<'_>) -> Result<(), Stop> {
    let listing = listing_with(st, Capability::Screen)?;
    let body = evidence_body(&st.pools, st.window, st.mandate);
    let quote = usable_quote(st, &listing, body).await?;
    let quoted_at = OffsetDateTime::parse(&quote.received_at, &Rfc3339)
        .unwrap_or_else(|_| OffsetDateTime::now_utc());
    let purchase = buy(st, &listing, &quote, None).await?;
    let screen: ScreenResponse =
        parse_body("screen", purchase.body.as_deref().unwrap_or_default())?;
    st.out.say(format!(
        "screen delivered: deployment {} blocks {}..{} indexed {} at {} coverage_shortfall {} truncated {} requests {}",
        screen.header.deployment_id,
        screen.header.block_start,
        screen.header.block_end,
        screen.header.indexed_block,
        screen.header.indexed_block_timestamp.map(|t| t.to_string()).unwrap_or_else(|| "null".to_owned()),
        screen.header.coverage_shortfall,
        screen.header.truncated,
        screen.requests
    ));
    st.screen = Some((screen, quoted_at));
    recompute(st);
    Ok(())
}

async fn buy_events(st: &mut State<'_>, pending: &[String]) -> Result<(), Stop> {
    let listing = listing_with(st, Capability::Events)?;
    let body = evidence_body(pending, st.window, st.mandate);
    let quote = usable_quote(st, &listing, body).await?;
    // The live events quote replaces the estimate: decide again before paying.
    let bundle = st
        .manifest
        .with_capability(Capability::Investigate)
        .next()
        .cloned();
    if let Some(inv) = bundle {
        let inv_body = evidence_body(pending, st.window, st.mandate);
        let inv_units = pending.len() as u64;
        let inv_amount = st
            .quotes
            .get(&(inv.id.clone(), inv_units))
            .copied()
            .or_else(|| inv.ceiling(inv_units).ok());
        let explain = st
            .manifest
            .with_capability(Capability::Explain)
            .next()
            .and_then(|e| e.tariff.ceiling_at_max().ok())
            .unwrap_or(0);
        if let Some(inv_amount) = inv_amount
            && !st.unusable.contains_key(&inv.id)
        {
            st.out.say(format!(
                "events {} vs bundle {}; {}",
                format_amount(quote.amount + explain, quote.decimals),
                format_amount(inv_amount, quote.decimals),
                if quote.amount + explain <= inv_amount {
                    "buying events"
                } else {
                    "buying the bundle"
                }
            ));
            if quote.amount + explain > inv_amount {
                let q = usable_quote(st, &inv, inv_body).await?;
                if q.amount <= inv_amount {
                    return buy_investigate_with(st, &inv, q).await;
                }
            }
        }
    }
    let quoted_at = OffsetDateTime::parse(&quote.received_at, &Rfc3339)
        .unwrap_or_else(|_| OffsetDateTime::now_utc());
    let purchase = buy(st, &listing, &quote, None).await?;
    let events: EventsResponse =
        parse_body("events", purchase.body.as_deref().unwrap_or_default())?;
    let total: u64 = events
        .pools
        .values()
        .map(|p| p.counts.swap + p.counts.mint + p.counts.burn)
        .sum();
    st.out.say(format!(
        "events delivered: {} pool(s), {} events, nulls {}, truncated {}, requests {}",
        events.pools.len(),
        total,
        events
            .pools
            .values()
            .map(|p| p.amount_usd_nulls)
            .sum::<u64>(),
        events.header.truncated,
        events.requests
    ));
    st.events = Some((events, quoted_at));
    recompute(st);
    Ok(())
}

async fn buy_explain(st: &mut State<'_>) -> Result<(), Stop> {
    let listing = listing_with(st, Capability::Explain)?;
    let bound = (listing.tariff.max_units * crate::manifest::KB) as usize;
    let events = st.events.as_ref().map(|(e, _)| e);
    let (brief, bytes, used) = analysis::build_brief(
        &st.mandate.id,
        st.window,
        &st.outcomes,
        &st.claims,
        events,
        st.mandate.requirements.brief_events,
        bound,
    )
    .map_err(|e| {
        Stop::Refused(Refusal {
            code: Code::RequirementUnmeetable,
            detail: format!("brief_too_large: {e}"),
        })
    })?;
    st.report.brief_events_used = Some(used);
    st.out.say(format!(
        "brief {} bytes ({} KB), {} supporting event(s) per pool",
        bytes.len(),
        bytes.len().div_ceil(1024),
        used
    ));
    let body = serde_json::to_vec(&json!({ "brief": brief })).expect("brief serializes");
    let quote = usable_quote(st, &listing, body).await?;
    let reservation_id = match st.reservation.as_ref() {
        Some((id, _, step)) if *step == listing.id => Some(*id),
        Some((id, _, _)) => {
            st.ledger
                .release(*id, OffsetDateTime::now_utc())
                .map_err(RunError::from)?;
            st.out
                .say("reserve released: the final step changed".to_owned());
            st.reservation = None;
            None
        }
        None => None,
    };
    let purchase = buy(st, &listing, &quote, reservation_id).await?;
    let x: Explanation = parse_body("explain", purchase.body.as_deref().unwrap_or_default())?;
    st.out.say(format!(
        "explanation from {} ({} input bytes): {}",
        x.model, x.input_bytes, x.prose
    ));
    st.explanation = Some(x);
    let _ = Brief {
        mandate_id: String::new(),
        window: st.window,
        outcomes: vec![],
        claims: vec![],
        events: BTreeMap::new(),
    };
    Ok(())
}

async fn buy_investigate(st: &mut State<'_>, pending: &[String]) -> Result<(), Stop> {
    let listing = listing_with(st, Capability::Investigate)?;
    let body = evidence_body(pending, st.window, st.mandate);
    let quote = usable_quote(st, &listing, body).await?;
    buy_investigate_with(st, &listing, quote).await
}

/// The bundle: facts, the seller's outcomes and claims, prose. The runtime
/// recomputes outcomes and claims from the delivered facts and requires
/// equality, section 8.
async fn buy_investigate_with(
    st: &mut State<'_>,
    listing: &Listing,
    quote: Quote,
) -> Result<(), Stop> {
    let quoted_at = OffsetDateTime::parse(&quote.received_at, &Rfc3339)
        .unwrap_or_else(|_| OffsetDateTime::now_utc());
    if let Some((id, _, _)) = st.reservation.take() {
        st.ledger
            .release(id, OffsetDateTime::now_utc())
            .map_err(RunError::from)?;
        st.out
            .say("reserve released: the bundle is one authorization".to_owned());
    }
    let purchase = buy(st, listing, &quote, None).await?;
    let v: Value = parse_body("investigate", purchase.body.as_deref().unwrap_or_default())?;
    let screen: ScreenResponse =
        serde_json::from_value(v.get("screen").cloned().unwrap_or(Value::Null)).map_err(|e| {
            Stop::Refused(Refusal {
                code: Code::EvidenceInsufficient,
                detail: format!("investigate.screen: {e}"),
            })
        })?;
    let events: Option<EventsResponse> = match v.get("events") {
        None | Some(Value::Null) => None,
        Some(e) => Some(serde_json::from_value(e.clone()).map_err(|e| {
            Stop::Refused(Refusal {
                code: Code::EvidenceInsufficient,
                detail: format!("investigate.events: {e}"),
            })
        })?),
    };
    let theirs_outcomes: Vec<PoolOutcome> =
        serde_json::from_value(v.get("outcomes").cloned().unwrap_or(Value::Null))
            .unwrap_or_default();
    let theirs_claims: Vec<Claim> =
        serde_json::from_value(v.get("claims").cloned().unwrap_or(Value::Null)).unwrap_or_default();
    let explanation: Explanation = serde_json::from_value(
        v.get("explanation").cloned().unwrap_or(Value::Null),
    )
    .map_err(|e| {
        Stop::Refused(Refusal {
            code: Code::EvidenceInsufficient,
            detail: format!("investigate.explanation: {e}"),
        })
    })?;
    // One head for the bundle: the screen and the events share the quote time.
    let had_screen = st.screen.is_some();
    if had_screen {
        // Hybrid: the bundle covers W; keep the earlier screen's facts for the other pools.
        // The bundle's screen is the authority for the pools it covers.
        let (mine, _) = st.screen.as_mut().expect("screen");
        for (pool, facts) in &screen.pools {
            mine.pools.insert(pool.clone(), facts.clone());
        }
    } else {
        st.screen = Some((screen.clone(), quoted_at));
    }
    st.events = events.clone().map(|e| (e, quoted_at));
    recompute(st);
    let (ours_o, ours_c) = outcomes_and_claims(
        &screen,
        events.as_ref(),
        st.mandate.requirements.evidence,
        &st.mandate.inputs.min_event_usd,
    );
    if ours_o != theirs_outcomes || ours_c != theirs_claims {
        st.report.anomalies.push(format!(
            "{}: seller outcomes or claims differ from the recomputation",
            listing.id
        ));
        st.out.say(format!("{}: seller's outcomes or claims differ from the runtime's recomputation; the runtime's stand", listing.id));
    } else {
        st.out.say(format!(
            "{}: seller's outcomes and claims equal the runtime's recomputation",
            listing.id
        ));
    }
    st.out.say(format!(
        "explanation from {} ({} input bytes): {}",
        explanation.model, explanation.input_bytes, explanation.prose
    ));
    st.explanation = Some(explanation);
    Ok(())
}

impl Evidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Screening => "screening",
            Self::Transaction => "transaction",
        }
    }
}

impl Citations {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Optional => "optional",
        }
    }
}
