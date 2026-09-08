//! `mandate run`: sections 4, 5, 6, 8, 9, 11 and 12 in order for one
//! mandate, as one decision loop: construct the requests whose bodies are
//! known, collect the quotes that are valid now, price and compare the
//! remaining plans, hold or replace the completion reservation, buy the
//! chosen plan's first step, check the delivery, recompute, update `W`,
//! repeat. A refused quote excludes its listing and returns to planning; a
//! refusal is terminal only when no plan remains. Every payment goes through
//! the ledger; every receipt is durable before it is published.
//!
//! Quoting, paying and publishing are seams, so the same loop runs against
//! the live sellers and the facilitator, or against a scripted market in
//! tests.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration as StdDuration;

use serde::Serialize;
use serde_json::json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::analysis::{self, Claim, Outcome, PoolOutcome, Thresholds, outcomes_and_claims};
use crate::config::{Config, PublicConfig};
use crate::evidence::{EventsResponse, Explanation, ScreenResponse, Window};
use crate::hedera::{HederaTopicId, MirrorNode, Settlement};
use crate::ledger::{
    Authorization, DeliveryState, Ledger, LedgerError, MandateRow, PaymentState, ReservationSource,
};
use crate::mandate::{Citations, Evidence, Mandate, SellerPolicy, format_amount};
use crate::manifest::{Capability, Listing, Manifest};
use crate::plan::{self, Plan, PlanKind, Situation};
use crate::publish::{FEE_CAP_TINYBAR, Published, Publisher};
use crate::purchase::{PayError, Payer, Purchase, ReconcileError, Resume, plan_recovery};
use crate::quote::{Estimate, Quote, QuoteError, Quoter};
use crate::receipts::{Outcome as ReceiptOutcome, Receipt};
use crate::refusal::{Code, Refusal};
use crate::transcript;
use crate::validate::{
    self, Delivered, Failure, Provenance, Purchased, Rules, Timed, Validation, reasons_of,
};
use crate::x402::sha256_hex;

pub const DEFAULT_QUOTE_TIMEOUT: StdDuration = StdDuration::from_secs(15);
/// Paid requests wait this long for a delivery; sellers query the Graph before answering.
pub const DELIVERY_TIMEOUT: StdDuration = StdDuration::from_secs(120);
pub const MIRROR_POLL: StdDuration = StdDuration::from_secs(5);
pub const MAX_RETRIEVALS_PER_RUN: u32 = 4;
/// Publish attempts for the queue when `anchor_before_delivery` is set.
pub const ANCHOR_ATTEMPTS: u32 = 3;
/// A quote is used for planning only while it stays usable this long.
pub const QUOTE_MARGIN: time::Duration = time::Duration::seconds(10);

// Seams.

/// Section 4: one 402 for one listing from a client that cannot pay.
#[allow(async_fn_in_trait)]
pub trait Quoting {
    fn signers(&self) -> Vec<String>;
    async fn quote(
        &self,
        listing: &Listing,
        body: Vec<u8>,
        now: OffsetDateTime,
    ) -> Result<Quote, QuoteError>;
}

impl Quoting for Quoter {
    fn signers(&self) -> Vec<String> {
        Quoter::signers(self).to_vec()
    }

    async fn quote(
        &self,
        listing: &Listing,
        body: Vec<u8>,
        now: OffsetDateTime,
    ) -> Result<Quote, QuoteError> {
        Quoter::quote(self, listing, body, now).await
    }
}

/// Section 6: sign and persist, then drive to settlement with a delivery.
#[allow(async_fn_in_trait)]
pub trait Paying {
    async fn prepare(
        &self,
        ledger: &mut Ledger,
        mandate_id: &str,
        step: &str,
        reservation_id: Option<i64>,
        quote: &Quote,
    ) -> Result<Authorization, PayError>;
    async fn settle(
        &self,
        ledger: &mut Ledger,
        id: i64,
        deadline: OffsetDateTime,
        say: &mut dyn FnMut(String),
    ) -> Result<Purchase, PayError>;
    /// Section 6 recovery for one authorization: reconcile, then drive the
    /// same transitions. Defaults to `settle`, which already reconciles on
    /// every pass.
    async fn resume_one(
        &self,
        ledger: &mut Ledger,
        id: i64,
        deadline: OffsetDateTime,
        say: &mut dyn FnMut(String),
    ) -> Result<Purchase, PayError> {
        self.settle(ledger, id, deadline, say).await
    }
}

impl Paying for Payer<'_> {
    async fn prepare(
        &self,
        ledger: &mut Ledger,
        mandate_id: &str,
        step: &str,
        reservation_id: Option<i64>,
        quote: &Quote,
    ) -> Result<Authorization, PayError> {
        Payer::prepare(self, ledger, mandate_id, step, reservation_id, quote).await
    }

    async fn settle(
        &self,
        ledger: &mut Ledger,
        id: i64,
        deadline: OffsetDateTime,
        say: &mut dyn FnMut(String),
    ) -> Result<Purchase, PayError> {
        Payer::settle(self, ledger, id, deadline, say).await
    }

    async fn resume_one(
        &self,
        ledger: &mut Ledger,
        id: i64,
        deadline: OffsetDateTime,
        say: &mut dyn FnMut(String),
    ) -> Result<Purchase, PayError> {
        Payer::resume_one(self, ledger, id, deadline, say).await
    }
}

/// Section 11: the receipt queue.
#[allow(async_fn_in_trait)]
pub trait Publishing {
    async fn publish_pending(
        &self,
        ledger: &mut Ledger,
        mandate_id: &str,
        say: &mut dyn FnMut(String),
    ) -> Result<Vec<Published>, LedgerError>;
    async fn reconcile_audit(
        &self,
        ledger: &mut Ledger,
        mandate_id: &str,
    ) -> Result<(), ReconcileError>;
}

impl Publishing for Publisher<'_> {
    async fn publish_pending(
        &self,
        ledger: &mut Ledger,
        mandate_id: &str,
        say: &mut dyn FnMut(String),
    ) -> Result<Vec<Published>, LedgerError> {
        Publisher::publish_pending(self, ledger, mandate_id, say).await
    }

    async fn reconcile_audit(
        &self,
        ledger: &mut Ledger,
        mandate_id: &str,
    ) -> Result<(), ReconcileError> {
        Publisher::reconcile_audit(self, ledger, mandate_id).await
    }
}

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
    Publish(#[from] ReconcileError),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("{0}")]
    Other(String),
}

/// Exit status of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Validated, complete, explained, receipts anchored when required.
    Delivered,
    /// Delivered under `degrade` or with a failed validation: see the report.
    DeliveredWithFindings,
    /// A section 2.7 refusal ended the run; nothing further was bought.
    Refused,
    /// `anchor_before_delivery` is set and a receipt is not on HCS: the report is withheld.
    NotAnchored,
    /// A payment is neither settled nor failed: the exposure is still held,
    /// section 2.7 PAYMENT_UNRESOLVED. `mandate reconcile` is the only way out.
    Unresolved,
}

impl Status {
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Delivered => 0,
            Self::DeliveredWithFindings => 4,
            Self::Refused => 3,
            Self::NotAnchored => 5,
            Self::Unresolved => 6,
        }
    }
}

/// The status a run ends with. A refusal is terminal whatever was validated
/// afterwards; a delivery needs the explanation, a passed and complete
/// validation, and anchored receipts when the mandate requires them.
pub fn final_status(
    refused: bool,
    validation: Option<&Validation>,
    explained: bool,
    incomplete: bool,
    anchored: bool,
    unresolved: bool,
) -> Status {
    // Exposure that no record has decided outranks every other outcome: the
    // amount is neither spent nor free until `mandate reconcile` says.
    if unresolved {
        return Status::Unresolved;
    }
    if refused {
        return Status::Refused;
    }
    if !anchored {
        return Status::NotAnchored;
    }
    match validation {
        Some(v) if v.passed && v.complete && explained && !incomplete => Status::Delivered,
        _ => Status::DeliveredWithFindings,
    }
}

/// The public configuration quoting runs on: the mandate's facilitator,
/// section 2.1, with the environment's value noted when it differs.
pub fn quoter_config(env: &PublicConfig, mandate: &Mandate) -> (PublicConfig, Option<String>) {
    let pinned = mandate
        .constraints
        .facilitator
        .trim_end_matches('/')
        .to_owned();
    let from_env = env.facilitator_url.trim_end_matches('/').to_owned();
    let note = (pinned != from_env).then(|| {
        format!("facilitator {pinned} from the mandate; FACILITATOR_URL {from_env} ignored")
    });
    let cfg = PublicConfig {
        facilitator_url: pinned,
        ..env.clone()
    };
    (cfg, note)
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
    pub round: u32,
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
    pub facilitator: String,
    pub quotes: Vec<Quote>,
    pub estimates: Vec<(String, u64, i64)>,
    pub planning: Vec<PlanningRound>,
    pub reservation_moves: Vec<String>,
    pub steps: Vec<StepReport>,
    pub outcomes: Vec<PoolOutcome>,
    pub claims: Vec<Claim>,
    /// Observation blocks per pool, from the purchase that covers it.
    pub blocks: BTreeMap<String, (u64, u64)>,
    pub brief_events_used: Option<u32>,
    pub explanation: Option<Explanation>,
    pub validation: Option<Validation>,
    /// Deliveries rejected before the report, with their reasons.
    pub rejected_deliveries: Vec<(String, Vec<Failure>)>,
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

/// What `execute` needs beyond the seams.
pub struct Inputs<'a> {
    pub mandate: &'a Mandate,
    pub manifest: Manifest,
    pub facilitator_url: String,
    pub facilitator_note: Option<String>,
    pub topic: HederaTopicId,
    pub hashscan: String,
    /// Where the ledger lives, for the reconcile notice.
    pub ledger_hint: String,
    /// The client provenance checks use; unused when no sample is chosen.
    pub http: &'a reqwest::Client,
    pub quiet: bool,
    /// Section 6: recover the authorizations this ledger already holds for
    /// the mandate, then carry on, instead of refusing a second run.
    pub resume: bool,
}

/// One purchase's evidence as the run holds it.
struct Owned {
    label: String,
    screen: Option<(ScreenResponse, OffsetDateTime)>,
    events: Option<(EventsResponse, OffsetDateTime)>,
}

/// Everything a run holds while it moves through the loop.
struct State<'a, Q, P, U> {
    mandate: &'a Mandate,
    manifest: Manifest,
    ledger: Ledger,
    quoter: &'a Q,
    payer: &'a P,
    publisher: &'a U,
    http: &'a reqwest::Client,
    out: Transcript,
    report: Report,
    window: Window,
    pools: Vec<String>,
    /// Listings whose quote was refused or unreachable, excluded for the run.
    unusable: BTreeMap<String, String>,
    /// The latest quote per listing, reused while usable and bound to the same body.
    live_quotes: BTreeMap<String, Quote>,
    evidence: Vec<Owned>,
    explanation: Option<Explanation>,
    outcomes: Vec<PoolOutcome>,
    claims: Vec<Claim>,
    brief: Option<analysis::Brief>,
    brief_numbers: Vec<String>,
    /// The completion reservation: id, amount, final step.
    reservation: Option<(i64, i64, String)>,
    step_no: u32,
    round: u32,
    incomplete: bool,
    refused: Option<Refusal>,
}

impl<Q, P, U> State<'_, Q, P, U> {
    fn thresholds(&self) -> Thresholds<'_> {
        Thresholds {
            materiality: &self.mandate.inputs.materiality,
            min_event_usd: &self.mandate.inputs.min_event_usd,
        }
    }

    fn rules(&self) -> Rules<'_> {
        Rules {
            required: &self.pools,
            window: self.window,
            evidence: self.mandate.requirements.evidence,
            citations: self.mandate.requirements.citations,
            max_data_age_s: self.mandate.requirements.max_data_age_s,
            provenance_samples: self.mandate.requirements.provenance_samples,
            degrade: self.mandate.requirements.degrade,
            thresholds: self.thresholds(),
            input_numbers: vec![
                self.mandate.inputs.window_h.to_string(),
                self.pools.len().to_string(),
            ],
            brief_numbers: self.brief_numbers.clone(),
        }
    }

    fn delivered(&self) -> Delivered<'_> {
        Delivered {
            purchases: self
                .evidence
                .iter()
                .map(|o| Purchased {
                    label: o.label.clone(),
                    screen: o.screen.as_ref().map(|(s, at)| Timed {
                        body: s,
                        quoted_at: *at,
                    }),
                    events: o.events.as_ref().map(|(e, at)| Timed {
                        body: e,
                        quoted_at: *at,
                    }),
                })
                .collect(),
            explanation: self.explanation.as_ref(),
        }
    }

    fn screened(&self) -> bool {
        self.evidence.iter().any(|o| o.screen.is_some())
    }

    fn decimals(&self) -> u32 {
        self.mandate.budget.service_decimals
    }
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

/// Runs one mandate file against the live sellers, facilitator and network.
/// `quiet` suppresses the streamed transcript; the caller prints the report.
pub async fn run(
    cfg: &Config,
    mandate_path: &Path,
    ledger_path: &Path,
    id: Option<&str>,
    quiet: bool,
    resume: bool,
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
    // Section 2.1: the facilitator is the mandate's, and its `/supported` pins the fee payer.
    let (quote_cfg, facilitator_note) = quoter_config(&cfg.public(), &mandate);
    let quoter = Quoter::from_config(&quote_cfg, DEFAULT_QUOTE_TIMEOUT).await?;
    let payer = Payer {
        signer: &signer,
        mirror: &mirror,
        http: &http,
        poll: MIRROR_POLL,
        max_retrievals: MAX_RETRIEVALS_PER_RUN,
    };
    let publisher = Publisher {
        consensus: &consensus,
        mirror: &mirror,
        topic,
        fee_cap: FEE_CAP_TINYBAR,
    };
    let ledger = Ledger::open(ledger_path)?;
    let inputs = Inputs {
        mandate: &mandate,
        manifest,
        facilitator_url: quote_cfg.facilitator_url.clone(),
        facilitator_note,
        topic,
        hashscan: cfg.network.hashscan().to_owned(),
        ledger_hint: ledger_path.display().to_string(),
        http: &http,
        quiet,
        resume,
    };
    execute(inputs, ledger, &quoter, &payer, &publisher).await
}

/// The run over the seams: sections 4 to 12 for one mandate.
pub async fn execute<Q: Quoting, P: Paying, U: Publishing>(
    inputs: Inputs<'_>,
    mut ledger: Ledger,
    quoter: &Q,
    payer: &P,
    publisher: &U,
) -> Result<Report, RunError> {
    let mandate = inputs.mandate;
    let manifest = inputs.manifest;
    let now = OffsetDateTime::now_utc();
    let decimals = mandate.budget.service_decimals;
    let pools: Vec<String> = mandate
        .inputs
        .pools
        .iter()
        .map(|p| p.to_lowercase())
        .collect();
    // The window is the task. A first run fixes it from the clock, hour
    // aligned; a resume restores what the ledger holds, so evidence already
    // bought is never judged against a window that moved under it.
    let stored = ledger.mandate(&mandate.id).ok().map(|m| m.window);
    let window = match stored {
        Some((from, to)) if inputs.resume && to > from => Window { from, to },
        _ => {
            let to = now.unix_timestamp() as u64 / 3600 * 3600;
            Window {
                from: to - u64::from(mandate.inputs.window_h) * 3600,
                to,
            }
        }
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

    let row = MandateRow {
        id: mandate.id.clone(),
        mandate_hash: mandate.hash.clone(),
        manifest_hash: manifest.hash.clone(),
        service_total: mandate.budget.service_total,
        service_asset: mandate.budget.service_asset.clone(),
        audit_total: mandate.budget.audit_total,
        max_single_payment: mandate.constraints.max_single_payment,
        deadline: mandate.constraints.deadline,
        window: (window.from, window.to),
    };
    let existing = ledger
        .mandate(&mandate.id)
        .ok()
        .map(|_| ledger.authorizations(&mandate.id))
        .transpose()?
        .unwrap_or_default();
    if !existing.is_empty() && !inputs.resume {
        return Err(RunError::AlreadyRan(mandate.id.clone()));
    }
    ledger.insert_mandate(&row, now)?;

    let report = Report {
        status: Status::Refused,
        mandate_id: mandate.id.clone(),
        mandate_hash: mandate.hash.clone(),
        manifest_hash: manifest.hash.clone(),
        pools: pools.clone(),
        window,
        service_asset: mandate.budget.service_asset.clone(),
        facilitator: inputs.facilitator_url.clone(),
        quotes: Vec::new(),
        estimates: Vec::new(),
        planning: Vec::new(),
        reservation_moves: Vec::new(),
        steps: Vec::new(),
        outcomes: Vec::new(),
        claims: Vec::new(),
        blocks: BTreeMap::new(),
        brief_events_used: None,
        explanation: None,
        validation: None,
        rejected_deliveries: Vec::new(),
        refusals: Vec::new(),
        anomalies: Vec::new(),
        totals: None,
        topic: inputs.topic.to_string(),
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
        publisher,
        http: inputs.http,
        out: Transcript {
            quiet: inputs.quiet,
            lines: Vec::new(),
        },
        report,
        window,
        pools,
        unusable: BTreeMap::new(),
        live_quotes: BTreeMap::new(),
        evidence: Vec::new(),
        explanation: None,
        outcomes: Vec::new(),
        claims: Vec::new(),
        brief: None,
        brief_numbers: Vec::new(),
        reservation: None,
        step_no: 0,
        round: 0,
        incomplete: false,
        refused: None,
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
        "coverage all_material over {} pool(s), window {} to {} ({} h), evidence {}, citations {}",
        st.pools.len(),
        window.from,
        window.to,
        mandate.inputs.window_h,
        mandate.requirements.evidence.as_str(),
        mandate.requirements.citations.as_str()
    ));
    st.out.say(format!(
        "service budget {} {} cap {} audit {} tinybar; assumption expected_material_pools {} until the screen; mandatory brief bound {} bytes",
        format_amount(mandate.budget.service_total, decimals),
        mandate.budget.service_asset,
        format_amount(mandate.constraints.max_single_payment, decimals),
        mandate.budget.audit_total,
        mandate.inputs.expected_material_pools,
        brief_bound
    ));
    st.out.say(format!(
        "facilitator {} signers {:?}{}",
        inputs.facilitator_url,
        st.quoter.signers(),
        inputs
            .facilitator_note
            .as_deref()
            .map(|n| format!("; {n}"))
            .unwrap_or_default()
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

    // Receipt 0, durable then published. When delivery must be anchored and
    // receipt 0 cannot be, nothing is bought.
    let r0 = Receipt::start(&mandate.id, &mandate.hash, &manifest.hash, &now_rfc3339());
    st.ledger
        .append_receipt_once(&mandate.id, &r0, Some("start"))?;
    publish(&mut st).await?;
    if mandate.duties.anchor_before_delivery {
        for _ in 1..ANCHOR_ATTEMPTS {
            if st.ledger.unpublished(&mandate.id)?.is_empty() {
                break;
            }
            tokio::time::sleep(MIRROR_POLL).await;
            publish(&mut st).await?;
        }
        if !st.ledger.unpublished(&mandate.id)?.is_empty() {
            let audit = st.ledger.audit_accounts(&mandate.id)?;
            refuse(
                &mut st,
                Refusal {
                    code: Code::OutsideConstraints,
                    detail: format!(
                        "anchor_before_delivery: receipt 0 is not on HCS after {ANCHOR_ATTEMPTS} attempts; audit budget free {} tinybar, fee cap {}",
                        audit.free(),
                        FEE_CAP_TINYBAR
                    ),
                },
            )?;
            return finish(st, inputs.hashscan, inputs.ledger_hint).await;
        }
    }

    // Section 6: recover what the ledger already holds, before anything new
    // is quoted or bought. A resumed authorization is never re-signed and
    // never acquires a second payment id.
    let mut complete = false;
    if !existing.is_empty() {
        st.out.say(format!(
            "resuming {} authorization(s) from the ledger",
            existing.len()
        ));
        match recover(&mut st, &existing).await {
            // The recovered job already holds its final deliverable: it goes
            // straight to validation, never back into planning.
            Ok(true) => {
                st.out
                    .say("recovery complete: the report was already bought".to_owned());
                complete = true;
            }
            Ok(false) => {}
            Err(e) => {
                match e {
                    Stop::Refused(r) => refuse(&mut st, r)?,
                    Stop::Error(e) => return Err(e),
                    Stop::Replan => {}
                }
                return finish(st, inputs.hashscan, inputs.ledger_hint).await;
            }
        }
    }

    // Sections 4 to 8: the decision loop.
    if !complete {
        match drive(&mut st).await {
            Ok(()) => {}
            Err(Stop::Refused(r)) => refuse(&mut st, r)?,
            Err(Stop::Replan) => {}
            Err(Stop::Error(e)) => return Err(e),
        }
    }

    // Section 9 over R, when anything was delivered and accepted.
    if st.screened() {
        let (mut v, blocks) = {
            let rules = st.rules();
            let delivered = st.delivered();
            let v = validate::validate(&rules, &delivered, &st.outcomes, &st.claims);
            (v, delivered.blocks())
        };
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
                let key = format!("{}:rejected", a.payment_id);
                st.ledger.append_receipt_once(&mandate.id, &r, Some(&key))?;
            }
        }
        if !v.complete || st.incomplete {
            st.out.say("report incomplete: screening only".to_owned());
        }
        if st.explanation.is_none() && st.refused.is_none() {
            st.out
                .say("report has no explanation: the final step was not delivered".to_owned());
        }
        st.report.blocks = blocks;
        st.report.validation = Some(v);
    }
    finish(st, inputs.hashscan, inputs.ledger_hint).await
}

/// Section 11 and 12: the queue, audit reconciliation, totals, receipts,
/// notices and the final status.
async fn finish<Q, P: Paying, U: Publishing>(
    mut st: State<'_, Q, P, U>,
    hashscan: String,
    ledger_hint: String,
) -> Result<Report, RunError> {
    let mandate = st.mandate;
    let decimals = mandate.budget.service_decimals;
    publish(&mut st).await?;
    let anchoring = mandate.duties.anchor_before_delivery;
    if anchoring {
        for _ in 1..ANCHOR_ATTEMPTS {
            if st.ledger.unpublished(&mandate.id)?.is_empty() {
                break;
            }
            tokio::time::sleep(MIRROR_POLL).await;
            publish(&mut st).await?;
        }
    }
    // The mirror node lags a few seconds behind consensus; wait briefly for
    // the last fee records.
    for _ in 0..4 {
        st.publisher
            .reconcile_audit(&mut st.ledger, &mandate.id)
            .await?;
        if st.ledger.audit_submitted_charges(&mandate.id)?.is_empty() {
            break;
        }
        tokio::time::sleep(MIRROR_POLL).await;
    }
    let accounts = st.ledger.accounts(&mandate.id)?;
    let audit = st.ledger.audit_accounts(&mandate.id)?;
    let auths = st.ledger.authorizations(&mandate.id)?;
    let unresolved: Vec<&Authorization> = auths
        .iter()
        .filter(|a| a.payment_state == PaymentState::Unresolved)
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
        st.report.topic,
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
        hashscan,
        st.report.topic
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
            ledger_hint
        );
        st.out.say(notice.clone());
        st.report.reconcile_notice = Some(notice);
    }
    let anchored = !anchoring || st.report.audit_pending.is_empty();
    let status = final_status(
        st.refused.is_some(),
        st.report.validation.as_ref(),
        st.explanation.is_some(),
        st.incomplete,
        anchored,
        !unresolved.is_empty(),
    );
    if status == Status::NotAnchored {
        st.out.say(format!(
            "anchor_before_delivery: receipts {:?} are not on HCS; the report is withheld",
            st.report.audit_pending
        ));
    } else {
        st.report.explanation = st.explanation.clone();
    }
    st.out.say(format!(
        "status {}",
        serde_json::to_string(&status)
            .unwrap_or_default()
            .trim_matches('"')
    ));
    st.report.status = status;
    st.report.outcomes = st.outcomes.clone();
    st.report.claims = st.claims.clone();
    st.report.brief_events_used = st.brief.as_ref().map(|b| b.events_used);
    st.report.transcript = std::mem::take(&mut st.out.lines);
    Ok(st.report)
}

/// Section 6 recovery over every authorization the ledger already holds.
/// Each is reconciled and driven to a terminal payment state with its
/// delivery; a recovered delivery is taken into evidence exactly as a fresh
/// one, so the run continues through the same loop. Unresolved exposure
/// stops the run: the amount is neither spent nor free until a record says.
async fn recover<Q, P: Paying, U: Publishing>(
    st: &mut State<'_, Q, P, U>,
    existing: &[Authorization],
) -> Result<bool, Stop> {
    let plan = plan_recovery(&st.ledger, &st.mandate.id, OffsetDateTime::now_utc())?;
    for r in &plan {
        let a = r.authorization();
        st.out.say(format!(
            "  {} {}: {} (payment {}, delivery {}, submissions {}, retrievals {})",
            a.step,
            a.tx_id,
            match r {
                Resume::Resend(_) => "resend the stored bytes",
                Resume::Backoff(..) => "wait for the 30 s spacing",
                Resume::AwaitRecord(_) => "wait for the record set",
                Resume::Unresolved(_) => "unresolved: exposure kept",
                Resume::Retrieve(_) => "retrieve with the original payment",
                Resume::Validate(_) => "validate the stored delivery",
            },
            a.payment_state.as_str(),
            a.delivery_state.as_str(),
            a.submissions,
            a.retrievals
        ));
    }
    // An interrupted run may have left a completion hold; adopt it rather
    // than holding a second time and stranding the first.
    if let Some(r) = st
        .ledger
        .held_reservations(&st.mandate.id)?
        .into_iter()
        .next()
    {
        st.out.say(format!(
            "adopting the completion reserve for {}: {}",
            r.step,
            format_amount(r.amount, st.decimals())
        ));
        st.reservation = Some((r.id, r.amount, r.step));
    }
    let mut complete = false;
    for a in existing {
        // A delivery already accepted is evidence, not work: restore it so
        // the run knows what it owns before it plans anything else.
        if !a.needs_resume() {
            if a.delivery_state == DeliveryState::Validated
                || a.delivery_state == DeliveryState::Received
            {
                complete |= restore(st, a).await?;
            }
            continue;
        }
        st.step_no += 1;
        let mut lines = Vec::new();
        let purchase = st
            .payer
            .resume_one(
                &mut st.ledger,
                a.id,
                st.mandate.constraints.deadline,
                &mut |l| lines.push(l),
            )
            .await?;
        for l in lines {
            st.out.say(l);
        }
        let decimals = st.decimals();
        record_purchase(st, purchase.clone(), decimals).await?;
        let row = &purchase.authorization;
        if row.payment_state == PaymentState::Unresolved {
            return Err(Stop::Refused(Refusal {
                code: Code::PaymentUnresolved,
                detail: format!(
                    "{} {}: no record by {}; exposure kept",
                    row.step, row.tx_id, row.valid_until
                ),
            }));
        }
        if row.payment_state != PaymentState::Settled || row.delivery_state == DeliveryState::None {
            return Err(Stop::Refused(Refusal {
                code: Code::EvidenceInsufficient,
                detail: format!(
                    "{} payment {} delivery {}",
                    row.step,
                    row.payment_state.as_str(),
                    row.delivery_state.as_str()
                ),
            }));
        }
        // The recovered body is evidence like any other delivery.
        let Some(listing) = st
            .manifest
            .listings
            .iter()
            .find(|l| l.id == row.step)
            .cloned()
        else {
            continue;
        };
        let quote: Quote = serde_json::from_str(&row.quote_json).map_err(|e| {
            Stop::Refused(Refusal {
                code: Code::EvidenceInsufficient,
                detail: format!("{}: stored quote does not parse: {e}", row.step),
            })
        })?;
        let _ = listing;
        let l = quote_listing_of(st, &row.step)?;
        complete |= accept(st, &l, &quote, &purchase).await?;
    }
    Ok(complete)
}

/// The listing a stored step names, still in the pinned manifest.
fn quote_listing_of<Q, P, U>(st: &State<'_, Q, P, U>, step: &str) -> Result<Listing, Stop> {
    st.manifest
        .listings
        .iter()
        .find(|l| l.id == step)
        .cloned()
        .ok_or_else(|| {
            Stop::Refused(Refusal {
                code: Code::OutsideConstraints,
                detail: format!("{step}: the purchase's listing is not in the pinned manifest"),
            })
        })
}

/// Takes a delivery the ledger already holds back into runtime state: the
/// body it paid for, and the explanation when that was the final step. The
/// stored response is authoritative, so nothing is fetched or bought again.
async fn restore<Q, P, U: Publishing>(
    st: &mut State<'_, Q, P, U>,
    a: &Authorization,
) -> Result<bool, Stop> {
    let Some(body) = a.response_body.clone() else {
        return Ok(false);
    };
    let listing = quote_listing_of(st, &a.step)?;
    let quote: Quote = serde_json::from_str(&a.quote_json).map_err(|e| {
        Stop::Refused(Refusal {
            code: Code::EvidenceInsufficient,
            detail: format!("{}: stored quote does not parse: {e}", a.step),
        })
    })?;
    st.out.say(format!(
        "{} restored from the ledger: {} bytes, delivery {}",
        a.step,
        body.len(),
        a.delivery_state.as_str()
    ));
    let purchase = Purchase {
        authorization: a.clone(),
        settlement: Settlement::Settled {
            consensus_timestamp: a.consensus_timestamp.clone().unwrap_or_default(),
            duplicates_ignored: a.duplicates_ignored as usize,
        },
        body: Some(body),
        latency_ms: None,
        records: 1,
    };
    accept(st, &listing, &quote, &purchase).await
}

/// Records a terminal refusal: transcript, report, receipt.
fn refuse<Q, P, U>(st: &mut State<'_, Q, P, U>, r: Refusal) -> Result<(), RunError> {
    st.out.say(r.to_string());
    st.report.refusals.push(r.to_string());
    let receipt = Receipt {
        seq: 0,
        mandate_id: st.mandate.id.clone(),
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
    st.ledger.append_receipt(&st.mandate.id, &receipt)?;
    st.refused = Some(r);
    Ok(())
}

/// Why the step loop stopped, or why a round starts over.
enum Stop {
    Refused(Refusal),
    /// The quote in hand expired before signing: quote and plan again.
    Replan,
    Error(RunError),
}

impl<E: Into<RunError>> From<E> for Stop {
    fn from(e: E) -> Self {
        Stop::Error(e.into())
    }
}

async fn publish<Q, P, U: Publishing>(st: &mut State<'_, Q, P, U>) -> Result<(), RunError> {
    let mut lines = Vec::new();
    st.publisher
        .publish_pending(&mut st.ledger, &st.mandate.id, &mut |l| lines.push(l))
        .await?;
    for l in lines {
        st.out.say(l);
    }
    Ok(())
}

/// One live quote, recorded for planning or as unusable with its reason.
async fn quote_listing<Q: Quoting, P, U>(
    st: &mut State<'_, Q, P, U>,
    listing: &Listing,
    body: Vec<u8>,
) -> Option<Quote> {
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
                    "{} {} live, within ceiling {} ({} units, {} ms, usable until {})",
                    listing.id,
                    format_amount(q.amount, q.decimals),
                    format_amount(q.ceiling, q.decimals),
                    q.request.units,
                    q.latency_ms,
                    q.valid_until().format(&Rfc3339).unwrap_or_default()
                ));
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

/// A quote valid for planning and paying now: the live one when it is still
/// usable past the margin and bound to this body, else a fresh 402, asked
/// twice when the first arrives already expired.
async fn valid_quote<Q: Quoting, P, U>(
    st: &mut State<'_, Q, P, U>,
    listing: &Listing,
    body: Vec<u8>,
) -> Option<Quote> {
    if st.unusable.contains_key(&listing.id) {
        return None;
    }
    let now = OffsetDateTime::now_utc();
    if let Some(q) = st.live_quotes.get(&listing.id)
        && q.usable_at(now + QUOTE_MARGIN)
        && q.request.body == body
    {
        return Some(q.clone());
    }
    if let Some(q) = st.live_quotes.remove(&listing.id) {
        st.out.say(format!(
            "{} quote {} re-requested: {}",
            listing.id,
            format_amount(q.amount, q.decimals),
            if q.request.body == body {
                "expired"
            } else {
                "bound to another request"
            }
        ));
    }
    for _ in 0..2 {
        let q = quote_listing(st, listing, body.clone()).await?;
        if q.usable_at(OffsetDateTime::now_utc() + QUOTE_MARGIN) {
            return Some(q);
        }
        st.out.say(format!(
            "{} quote arrived expired; asking again",
            listing.id
        ));
    }
    let r = Refusal {
        code: Code::SellerUnreachable,
        detail: format!("{}: quotes arrive expired", listing.id),
    };
    st.out.say(r.to_string());
    st.unusable.insert(listing.id.clone(), r.to_string());
    st.report.refusals.push(format!("{}: {r}", listing.id));
    None
}

fn pending_pools<Q, P, U>(st: &State<'_, Q, P, U>) -> Vec<String> {
    if !st.screened() {
        return st.pools.clone();
    }
    st.outcomes
        .iter()
        .filter(|o| o.outcome.pending_work())
        .map(|o| o.pool.clone())
        .collect()
}

/// Pools that a purchase can still resolve: `pending` ones. `undetermined`
/// pools carry incomplete facts no purchase repairs.
fn resolvable_pools<Q, P, U>(st: &State<'_, Q, P, U>) -> Vec<String> {
    st.outcomes
        .iter()
        .filter(|o| o.outcome == Outcome::Pending)
        .map(|o| o.pool.clone())
        .collect()
}

fn listing_with<Q, P, U>(st: &State<'_, Q, P, U>, cap: Capability) -> Option<Listing> {
    st.manifest.with_capability(cap).next().cloned()
}

/// Section 4 steps 2 and 3 for this round: the requests whose bodies are
/// fully known now, quoted; the rest is estimated at the ceiling.
async fn gather<Q: Quoting, P, U>(
    st: &mut State<'_, Q, P, U>,
    pending: &[String],
) -> Result<BTreeMap<String, Quote>, Stop> {
    let mut wanted: Vec<(Listing, Vec<u8>)> = Vec::new();
    if !st.screened() {
        let body = evidence_body(&st.pools, st.window, st.mandate);
        for cap in [Capability::Screen, Capability::Investigate] {
            if let Some(l) = listing_with(st, cap) {
                wanted.push((l, body.clone()));
            }
        }
    } else if !pending.is_empty() {
        let body = evidence_body(pending, st.window, st.mandate);
        for cap in [Capability::Events, Capability::Investigate] {
            if let Some(l) = listing_with(st, cap) {
                wanted.push((l, body.clone()));
            }
        }
    } else if let Some(l) = listing_with(st, Capability::Explain) {
        // W is empty: the brief is known, so the final step is quotable.
        let bound = (l.tariff.max_units * crate::manifest::KB) as usize;
        let delivered = st.delivered();
        let blocks = delivered.blocks();
        let events_owned: Option<EventsResponse> = None;
        let _ = events_owned;
        let brief = analysis::build_brief(
            analysis::BriefInput {
                mandate_id: &st.mandate.id,
                window: st.window,
                blocks: &blocks,
                outcomes: &st.outcomes,
                claims: &st.claims,
                events: delivered
                    .purchases
                    .iter()
                    .rev()
                    .find_map(|p| p.events.map(|e| e.body)),
                brief_events: st.mandate.requirements.brief_events,
            },
            bound,
        )
        .map_err(|e| {
            Stop::Refused(Refusal {
                code: Code::RequirementUnmeetable,
                detail: format!("brief_too_large: {e}"),
            })
        })?;
        st.out.say(format!(
            "brief {} bytes ({} KB), {} supporting event(s) per pool",
            brief.body.len(),
            brief.body.len().div_ceil(1024),
            brief.events_used
        ));
        st.brief_numbers.clear();
        analysis::brief_numbers(&brief.json, &mut st.brief_numbers);
        wanted.push((l, brief.body.clone()));
        st.brief = Some(brief);
    }
    let mut valid = BTreeMap::new();
    for (listing, body) in wanted {
        if let Some(q) = valid_quote(st, &listing, body).await {
            valid.insert(listing.id.clone(), q);
        }
    }
    Ok(valid)
}

/// Section 5 for this round. Estimates for unquoted steps are printed with
/// the quotes table on the first round.
fn plan_round<Q, P, U>(
    st: &mut State<'_, Q, P, U>,
    pending: &[String],
    valid: &BTreeMap<String, Quote>,
) -> Result<Plan, Refusal> {
    let decimals = st.decimals();
    let accounts = st.ledger.accounts(&st.mandate.id).map_err(|e| Refusal {
        code: Code::OverBudget,
        detail: e.to_string(),
    })?;
    let free = accounts.free() + st.reservation.as_ref().map(|r| r.1).unwrap_or(0);
    let quotes: BTreeMap<(String, u64), i64> = valid
        .values()
        .map(|q| ((q.listing_id.clone(), q.request.units), q.amount))
        .collect();
    let sit = Situation {
        manifest: &st.manifest,
        required: st.pools.len() as u64,
        unscreened: if st.screened() {
            0
        } else {
            st.pools.len() as u64
        },
        pending: pending.len() as u64,
        expected_material: u64::from(st.mandate.inputs.expected_material_pools),
        window_seconds: st.window.to.abs_diff(st.window.from),
        free,
        max_single_payment: st.mandate.constraints.max_single_payment,
        decimals,
        quotes: &quotes,
        unusable: &st.unusable,
        brief_kb: st
            .brief
            .as_ref()
            .map(|b| (b.body.len() as u64).div_ceil(crate::manifest::KB)),
    };
    if st.round == 1 {
        let mut estimates = Vec::new();
        for l in &st.manifest.listings {
            if valid.contains_key(&l.id) || st.unusable.contains_key(&l.id) {
                continue;
            }
            let units = match l.capability {
                Capability::Events => {
                    st.pools.len() as u64
                        * sit
                            .window_seconds
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
        let quotes: Vec<Quote> = valid.values().cloned().collect();
        st.out.say(
            transcript::quotes_table(&quotes, &estimates, decimals)
                .trim_end()
                .to_owned(),
        );
        st.report.estimates = estimates
            .iter()
            .map(|e| (e.listing_id.clone(), e.units, e.ceiling))
            .collect();
    }
    let plans = plan::price_all(&sit);
    let chosen = plan::choose(&plans, &sit).map(|p| p.kind);
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
        round: st.round,
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

/// I4 for the chosen plan: a reservation for its final step at the bound
/// amount while a non-final step remains, replaced when the plan's final
/// step changes, kept when the final step is the only one left so the
/// purchase consumes it, and released when the plan no longer needs it.
fn settle_reservation<Q, P, U>(st: &mut State<'_, Q, P, U>, plan: &Plan) -> Result<(), RunError> {
    let decimals = st.decimals();
    let now = OffsetDateTime::now_utc();
    let final_step = plan
        .bound_steps
        .last()
        .map(|s| (s.listing_id.clone(), s.amount));
    let existing = st.reservation.take();
    let wanted = match (&final_step, &existing) {
        (Some((step, amount)), _)
            if plan.bound_steps.len() > 1 && st.mandate.budget.reserve_completion =>
        {
            Some((step.clone(), *amount))
        }
        (Some((step, _)), Some((_, _, held_step)))
            if plan.bound_steps.len() == 1 && step == held_step =>
        {
            // The final step is the only step left: the purchase consumes the hold.
            st.reservation = existing;
            return Ok(());
        }
        _ => None,
    };
    if let Some((id, amount, step)) = &existing
        && wanted
            .as_ref()
            .is_some_and(|(w, a)| w == step && *a == *amount)
    {
        st.reservation = Some((*id, *amount, step.clone()));
        return Ok(());
    }
    if let Some((id, amount, step)) = existing {
        st.ledger.release(id, now)?;
        let line = format!(
            "reserve {step} {} released: plan {}",
            format_amount(amount, decimals),
            if wanted.is_some() {
                "switched"
            } else {
                "has one step left"
            }
        );
        st.out.say(line.clone());
        st.report.reservation_moves.push(line);
    }
    if let Some((step, amount)) = wanted {
        let r = st.ledger.hold(
            &st.mandate.id,
            &step,
            amount,
            ReservationSource::CeilingAtMax,
            now,
        )?;
        let line = format!("reserve {step} {} held", format_amount(amount, decimals));
        st.out.say(line.clone());
        st.report.reservation_moves.push(line);
        st.reservation = Some((r.id, r.amount, step));
    }
    Ok(())
}

/// The decision loop.
async fn drive<Q: Quoting, P: Paying, U: Publishing>(
    st: &mut State<'_, Q, P, U>,
) -> Result<(), Stop> {
    loop {
        st.round += 1;
        let pending = pending_pools(st);
        // Section 7: pools no purchase can resolve.
        if st.screened() {
            let resolvable = resolvable_pools(st);
            let stuck: Vec<&String> = pending.iter().filter(|p| !resolvable.contains(p)).collect();
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
        }
        let pending: Vec<String> = if st.screened() {
            resolvable_pools(st)
        } else {
            pending
        };
        st.out.say(format!(
            "round {}: {} pool(s) pending{}",
            st.round,
            pending.len(),
            if st.screened() { "" } else { " (unscreened)" }
        ));

        // 1 and 2: known requests, valid quotes.
        let valid = gather(st, &pending).await?;

        // 3: plans over the remaining job.
        let plan = match plan_round(st, &pending, &valid) {
            Ok(p) => p,
            Err(r) if st.round == 1 && st.step_no == 0 => return Err(Stop::Refused(r)),
            Err(r) => {
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

        // 4: the completion reservation follows the plan.
        settle_reservation(st, &plan)?;

        // 5: the plan's first step, with the quote gathered this round.
        let Some(step) = plan.expected_steps.first().cloned() else {
            return Ok(());
        };
        let Some(listing) = st
            .manifest
            .listings
            .iter()
            .find(|l| l.id == step.listing_id)
            .cloned()
        else {
            return Err(Stop::Refused(Refusal {
                code: Code::RequirementUnmeetable,
                detail: format!("listing {} vanished", step.listing_id),
            }));
        };
        let Some(quote) = valid.get(&listing.id).cloned() else {
            // The step was priced at its ceiling without a quote: quote it now.
            st.out.say(format!(
                "{}: no valid quote in hand; quoting before purchase",
                listing.id
            ));
            continue;
        };
        let reservation_id = match st.reservation.as_ref() {
            Some((id, _, step)) if *step == listing.id => Some(*id),
            _ => None,
        };
        let purchase = match buy(st, &listing, &quote, reservation_id).await {
            Ok(p) => p,
            Err(Stop::Replan) => continue,
            Err(e) => return Err(e),
        };
        let done = accept(st, &listing, &quote, &purchase).await?;
        if done {
            return Ok(());
        }
    }
}

/// Section 6 `received -> rejected` for a delivery that fails its own
/// checks: the reasons are persisted, a `failed` receipt is written, the
/// facts are dropped, and the run refuses rather than buying on top of it.
async fn reject_delivery<Q, P, U: Publishing>(
    st: &mut State<'_, Q, P, U>,
    a: &Authorization,
    failures: Vec<Failure>,
) -> Result<Stop, RunError> {
    let reasons: Vec<&str> = reasons_of(&failures)
        .iter()
        .map(|r| r.as_str())
        .collect::<Vec<_>>()
        .into_iter()
        .collect();
    let reason = reasons.join(",");
    st.out.say(format!(
        "{} rejected: {} ({} finding(s))",
        a.step,
        reason,
        failures.len()
    ));
    for f in &failures {
        st.out.say(format!("  {} {}", f.reason.as_str(), f.detail));
    }
    st.ledger
        .mark_rejected(a.id, &reason, OffsetDateTime::now_utc())?;
    let mut r = purchase_receipt(st, a, ReceiptOutcome::Failed, None, None);
    r.reason = Some(format!(
        "validation {reason}: {}",
        failures.first().map(|f| f.detail.as_str()).unwrap_or("")
    ));
    let key = format!("{}:rejected", a.payment_id);
    st.ledger
        .append_receipt_once(&st.mandate.id, &r, Some(&key))?;
    publish(st).await?;
    st.report
        .rejected_deliveries
        .push((a.step.clone(), failures));
    Ok(Stop::Refused(Refusal {
        code: Code::EvidenceInsufficient,
        detail: format!("{} delivery rejected: {reason}", a.step),
    }))
}

/// Takes a settled, delivered purchase into evidence after the section 9
/// delivery checks, recomputes outcomes and claims, and says whether the
/// run's final deliverable is now in hand.
async fn accept<Q, P, U: Publishing>(
    st: &mut State<'_, Q, P, U>,
    listing: &Listing,
    quote: &Quote,
    purchase: &Purchase,
) -> Result<bool, Stop> {
    let quoted_at = OffsetDateTime::parse(&quote.received_at, &Rfc3339)
        .unwrap_or_else(|_| OffsetDateTime::now_utc());
    let body = purchase.body.as_deref().unwrap_or_default();
    let a = &purchase.authorization;
    let (owned, explanation, done, bundle_check) = match listing.capability {
        Capability::Screen => {
            let screen: ScreenResponse = parse_body("screen", body)?;
            st.out.say(format!(
                "screen delivered: deployment {} blocks {}..{} indexed {} at {} coverage_shortfall {} truncated {} requests {}",
                screen.header.deployment_id,
                screen.header.block_start,
                screen.header.block_end,
                screen.header.indexed_block,
                screen
                    .header
                    .indexed_block_timestamp
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| "null".to_owned()),
                screen.header.coverage_shortfall,
                screen.header.truncated,
                screen.requests
            ));
            (
                Owned {
                    label: listing.id.clone(),
                    screen: Some((screen, quoted_at)),
                    events: None,
                },
                None,
                false,
                None,
            )
        }
        Capability::Events => {
            let events: EventsResponse = parse_body("events", body)?;
            let total: usize = events.pools.values().map(|p| p.all().count()).sum();
            st.out.say(format!(
                "events delivered: {} pool(s), {} events, truncated {}, blocks {}..{}, requests {}",
                events.pools.len(),
                total,
                events.header.truncated,
                events.header.block_start,
                events.header.block_end,
                events.requests
            ));
            (
                Owned {
                    label: listing.id.clone(),
                    screen: None,
                    events: Some((events, quoted_at)),
                },
                None,
                false,
                None,
            )
        }
        Capability::Investigate => {
            let bundle: Bundle = match serde_json::from_slice(body) {
                Ok(b) => b,
                Err(e) => {
                    let f = vec![Failure {
                        reason: validate::Reason::Schema,
                        detail: format!(
                            "{}: response does not match the bundle schema: {e}",
                            listing.id
                        ),
                    }];
                    return Err(reject_delivery(st, a, f).await?);
                }
            };
            st.out.say(format!(
                "investigate delivered: {} pool(s) screened, events for {}, blocks {}..{}, requests {}",
                bundle.screen.pools.len(),
                bundle.events.as_ref().map(|e| e.pools.len()).unwrap_or(0),
                bundle.screen.header.block_start,
                bundle.screen.header.block_end,
                bundle.requests
            ));
            let ours = outcomes_and_claims(
                &bundle.screen,
                bundle.events.as_ref(),
                st.mandate.requirements.evidence,
                st.thresholds(),
            );
            let check = bundle_matches(&bundle.outcomes, &bundle.claims, &ours);
            (
                Owned {
                    label: listing.id.clone(),
                    screen: Some((bundle.screen, quoted_at)),
                    events: bundle.events.map(|e| (e, quoted_at)),
                },
                Some(bundle.explanation),
                true,
                Some(check),
            )
        }
        Capability::Explain => {
            let x: Explanation = parse_body("explain", body)?;
            st.out.say(format!(
                "explanation from {} ({} input bytes): {}",
                x.model, x.input_bytes, x.prose
            ));
            st.explanation = Some(x);
            return Ok(true);
        }
    };
    st.evidence.push(owned);
    let is_bundle = bundle_check.is_some();
    let mut failures = validate::validate_deliveries(&st.rules(), &st.delivered());
    if let Some(check) = bundle_check {
        failures.extend(check);
    }
    if !failures.is_empty() {
        st.evidence.pop();
        return Err(reject_delivery(st, a, failures).await?);
    }
    st.out.say(format!(
        "{} delivery checked: schema and freshness pass",
        listing.id
    ));
    if is_bundle {
        st.out.say(format!(
            "{}: seller's outcomes and claims equal the runtime's recomputation",
            listing.id
        ));
    }
    recompute(st);
    if let Some(x) = explanation {
        st.out.say(format!(
            "explanation from {} ({} input bytes): {}",
            x.model, x.input_bytes, x.prose
        ));
        st.explanation = Some(x);
    }
    Ok(done)
}

/// What a bundle must carry, section 8: facts, the seller's outcomes and
/// claims, and the prose.
#[derive(serde::Deserialize)]
struct Bundle {
    screen: ScreenResponse,
    events: Option<EventsResponse>,
    outcomes: Vec<PoolOutcome>,
    claims: Vec<Claim>,
    explanation: Explanation,
    #[serde(default)]
    requests: u64,
}

/// Section 8's equality requirement on a bundle: the seller's outcomes and
/// claims are what the runtime computes from the delivered facts. Order is
/// not part of the requirement: a seller may answer in request order, the
/// runtime computes in pool order.
pub fn bundle_matches(
    bundle_outcomes: &[PoolOutcome],
    bundle_claims: &[Claim],
    ours: &(Vec<PoolOutcome>, Vec<Claim>),
) -> Vec<Failure> {
    let mut out = Vec::new();
    let theirs_o = sorted_outcomes(bundle_outcomes);
    let ours_o = sorted_outcomes(&ours.0);
    let theirs_c = sorted_claims(bundle_claims);
    let ours_c = sorted_claims(&ours.1);
    if theirs_o != ours_o {
        out.push(Failure {
            reason: validate::Reason::Calculation,
            detail: format!(
                "bundle outcomes differ from the recomputation: {}",
                first_difference(
                    theirs_o
                        .iter()
                        .map(|o| format!("{} {}", o.pool, o.outcome.as_str())),
                    ours_o
                        .iter()
                        .map(|o| format!("{} {}", o.pool, o.outcome.as_str())),
                )
            ),
        });
    }
    if theirs_c != ours_c {
        out.push(Failure {
            reason: validate::Reason::Calculation,
            detail: format!(
                "bundle claims ({}) differ from the recomputation ({}): {}",
                theirs_c.len(),
                ours_c.len(),
                first_difference(
                    theirs_c
                        .iter()
                        .map(|c| format!("{} {:?} {:?}", c.pool, c.kind, c.values)),
                    ours_c
                        .iter()
                        .map(|c| format!("{} {:?} {:?}", c.pool, c.kind, c.values)),
                )
            ),
        });
    }
    out
}

/// The first entry that differs, for a message a human can act on.
fn first_difference(
    theirs: impl Iterator<Item = String>,
    ours: impl Iterator<Item = String>,
) -> String {
    let (theirs, ours): (Vec<String>, Vec<String>) = (theirs.collect(), ours.collect());
    for (i, t) in theirs.iter().enumerate() {
        match ours.get(i) {
            Some(o) if o == t => {}
            Some(o) => return format!("seller {t}, runtime {o}"),
            None => return format!("seller has {t}, the runtime has nothing"),
        }
    }
    match ours.get(theirs.len()) {
        Some(o) => format!("the runtime has {o}, the seller has nothing"),
        None => "no entry differs".to_owned(),
    }
}

fn sorted_outcomes(o: &[PoolOutcome]) -> Vec<PoolOutcome> {
    let mut v = o.to_vec();
    v.sort_by(|a, b| a.pool.cmp(&b.pool));
    v
}

fn sorted_claims(c: &[Claim]) -> Vec<Claim> {
    let mut v = c.to_vec();
    v.sort_by(|a, b| {
        (&a.pool, a.kind as u8, &a.evidence).cmp(&(&b.pool, b.kind as u8, &b.evidence))
    });
    v
}

fn parse_body<T: serde::de::DeserializeOwned>(what: &str, body: &[u8]) -> Result<T, Stop> {
    serde_json::from_slice(body).map_err(|e| {
        Stop::Refused(Refusal {
            code: Code::EvidenceInsufficient,
            detail: format!("{what}: response does not match the schema: {e}"),
        })
    })
}

fn recompute<Q, P, U>(st: &mut State<'_, Q, P, U>) {
    let (o, c) = validate::compute(
        &st.delivered(),
        st.mandate.requirements.evidence,
        st.thresholds(),
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

/// One paid step: prepare, settle, receipt.
async fn buy<Q, P: Paying, U: Publishing>(
    st: &mut State<'_, Q, P, U>,
    listing: &Listing,
    quote: &Quote,
    reservation_id: Option<i64>,
) -> Result<Purchase, Stop> {
    let step_name = listing.id.clone();
    let a = match st
        .payer
        .prepare(
            &mut st.ledger,
            &st.mandate.id,
            &step_name,
            reservation_id,
            quote,
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
        Err(PayError::QuoteExpired(id)) => {
            st.out.say(format!(
                "{id}: quote expired before signing; quoting and planning again"
            ));
            st.live_quotes.remove(&id);
            return Err(Stop::Replan);
        }
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
    {
        if *held > a.amount {
            let line = format!(
                "reserve released {}",
                format_amount(held - a.amount, quote.decimals)
            );
            st.out.say(line.clone());
            st.report.reservation_moves.push(line);
        }
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
    record_purchase(st, purchase.clone(), quote.decimals).await?;
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

/// The step row, the receipt and the anomaly note every settled or failed
/// purchase leaves behind, whether it was bought this run or recovered from
/// the ledger. Exactly one receipt per terminal transition.
async fn record_purchase<Q, P, U: Publishing>(
    st: &mut State<'_, Q, P, U>,
    purchase: Purchase,
    decimals: u32,
) -> Result<(), RunError> {
    let a = &purchase.authorization;
    st.report.steps.push(StepReport {
        step: st.step_no,
        listing_id: a.step.clone(),
        payment_id: a.payment_id.clone(),
        tx_id: a.tx_id.clone(),
        amount: format_amount(a.amount, decimals),
        submissions: a.submissions,
        retrievals: a.retrievals,
        payment_state: a.payment_state.as_str().to_owned(),
        delivery_state: a.delivery_state.as_str().to_owned(),
        records: purchase.records,
        duplicates_ignored: a.duplicates_ignored,
        latency_ms: purchase.latency_ms,
        consensus_timestamp: a.consensus_timestamp.clone(),
    });
    let outcome = match &purchase.settlement {
        Settlement::Settled { .. } => ReceiptOutcome::Paid,
        Settlement::Failed { .. } | Settlement::Anomaly { .. } => ReceiptOutcome::Failed,
        Settlement::Absent { .. } => ReceiptOutcome::Unresolved,
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
    let key = format!("{}:{}", a.payment_id, receipt.outcome_str());
    let written = st
        .ledger
        .append_receipt_once(&st.mandate.id, &receipt, Some(&key))?;
    if written.is_none() {
        st.out.say(format!(
            "{} {}: receipt already recorded",
            a.step,
            receipt.outcome_str()
        ));
    }
    publish(st).await?;
    if a.payment_state == PaymentState::Failed && a.delivery_state != DeliveryState::None {
        st.report
            .anomalies
            .push(format!("unpaid_delivery {} {}", a.step, a.tx_id));
    }
    Ok(())
}

fn purchase_receipt<Q, P, U>(
    st: &State<'_, Q, P, U>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::CoverageResult;

    fn validation(passed: bool, complete: bool) -> Validation {
        Validation {
            passed,
            complete,
            failures: Vec::new(),
            coverage: CoverageResult {
                required: 1,
                resolved: 1,
                pending: Vec::new(),
                undetermined: Vec::new(),
            },
            calculations: 0,
            references: 0,
            transaction_citations: 0,
            prose_numbers: 0,
            provenance: Provenance::NotApplicable,
        }
    }

    #[test]
    fn a_refusal_is_terminal_and_a_delivery_needs_the_explanation() {
        let ok = validation(true, true);
        assert_eq!(
            final_status(true, Some(&ok), false, false, true, false),
            Status::Refused
        );
        assert_eq!(
            final_status(false, Some(&ok), false, false, true, false),
            Status::DeliveredWithFindings
        );
        assert_eq!(
            final_status(false, Some(&ok), true, false, true, false),
            Status::Delivered
        );
        assert_eq!(
            final_status(
                false,
                Some(&validation(false, true)),
                true,
                false,
                true,
                false
            ),
            Status::DeliveredWithFindings
        );
        assert_eq!(
            final_status(
                false,
                Some(&validation(true, false)),
                true,
                false,
                true,
                false
            ),
            Status::DeliveredWithFindings
        );
        assert_eq!(
            final_status(false, Some(&ok), true, true, true, false),
            Status::DeliveredWithFindings
        );
        assert_eq!(
            final_status(false, None, false, false, true, false),
            Status::DeliveredWithFindings
        );
        assert_eq!(
            final_status(false, Some(&ok), true, false, false, false),
            Status::NotAnchored
        );
        // Unresolved exposure outranks a refusal and a delivery alike.
        assert_eq!(
            final_status(true, Some(&ok), true, false, true, true),
            Status::Unresolved
        );
        assert_eq!(
            final_status(false, Some(&ok), true, false, true, true),
            Status::Unresolved
        );
        assert_eq!(Status::Unresolved.exit_code(), 6);
        assert_eq!(Status::NotAnchored.exit_code(), 5);
        assert_eq!(Status::Refused.exit_code(), 3);
    }

    #[test]
    fn the_mandates_facilitator_wins_over_the_environment() {
        let env = PublicConfig {
            network: crate::config::Network::Testnet,
            facilitator_url: "https://other.example".to_owned(),
            mirror_node_url: "https://testnet.mirrornode.hedera.com".to_owned(),
            hcs_topic_id: None,
        };
        let mandate = crate::mandate::Mandate::from_toml(
            crate::testing::MANDATE_TOML,
            time::macros::datetime!(2026-09-08 09:00 UTC),
        )
        .unwrap();
        let (cfg, note) = quoter_config(&env, &mandate);
        assert_eq!(cfg.facilitator_url, "https://api.testnet.blocky402.com");
        assert!(
            note.unwrap()
                .contains("FACILITATOR_URL https://other.example ignored")
        );
        let same = PublicConfig {
            facilitator_url: "https://api.testnet.blocky402.com/".to_owned(),
            ..env
        };
        assert!(quoter_config(&same, &mandate).1.is_none());
    }

    #[test]
    fn a_bundle_with_other_claims_is_a_calculation_failure() {
        use crate::testing::{events_fixture, screen_fixture};
        let screen = screen_fixture(true, true);
        let events = events_fixture(Some("250000"), false);
        let t = Thresholds {
            materiality: "0.05",
            min_event_usd: "100000",
        };
        let ours = outcomes_and_claims(&screen, Some(&events), Evidence::Transaction, t);
        assert!(bundle_matches(&ours.0, &ours.1, &ours).is_empty());
        let f = bundle_matches(&[], &[], &ours);
        assert_eq!(f.len(), 2);
        assert!(f.iter().all(|f| f.reason == validate::Reason::Calculation));
        // Order is not part of the requirement.
        let mut shuffled = ours.clone();
        shuffled.0.reverse();
        shuffled.1.reverse();
        assert!(bundle_matches(&shuffled.0, &shuffled.1, &ours).is_empty());
        // A changed value still fails, and the message names it.
        let mut changed = ours.1.clone();
        changed[0]
            .values
            .insert("change".into(), serde_json::json!("9"));
        let f = bundle_matches(&ours.0, &changed, &ours);
        assert_eq!(f.len(), 1);
        assert!(
            f[0].detail.contains("seller ") && f[0].detail.contains("runtime "),
            "{}",
            f[0].detail
        );
    }
}
