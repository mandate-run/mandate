//! `mandate run`: sections 4, 5, 6, 8, 9, 11 and 12 in order for one
//! mandate. Every decision is printed as it is made and collected into the
//! report; every payment goes through the ledger; every delivery is checked
//! before it authorizes another purchase; every receipt is durable before it
//! is published. A refusal is terminal: nothing after it turns the run into
//! a delivery.

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
use crate::validate::{self, Delivered, Failure, Provenance, Rules, Timed, Validation, reasons_of};
use crate::x402::sha256_hex;

pub const DEFAULT_QUOTE_TIMEOUT: StdDuration = StdDuration::from_secs(15);
/// Paid requests wait this long for a delivery; sellers query the Graph before answering.
pub const DELIVERY_TIMEOUT: StdDuration = StdDuration::from_secs(120);
pub const MIRROR_POLL: StdDuration = StdDuration::from_secs(5);
pub const MAX_RETRIEVALS_PER_RUN: u32 = 4;
/// Publish attempts for the queue when `anchor_before_delivery` is set.
pub const ANCHOR_ATTEMPTS: u32 = 3;

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
    /// Validated, complete, explained, receipts anchored when required.
    Delivered,
    /// Delivered under `degrade` or with a failed validation: see the report.
    DeliveredWithFindings,
    /// A section 2.7 refusal ended the run; nothing further was bought.
    Refused,
    /// `anchor_before_delivery` is set and a receipt is not on HCS: the report is withheld.
    NotAnchored,
}

impl Status {
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Delivered => 0,
            Self::DeliveredWithFindings => 4,
            Self::Refused => 3,
            Self::NotAnchored => 5,
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
) -> Status {
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
    pub steps: Vec<StepReport>,
    pub outcomes: Vec<PoolOutcome>,
    pub claims: Vec<Claim>,
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
    brief_numbers: Vec<String>,
    reservation: Option<(i64, i64, String)>,
    step_no: u32,
    incomplete: bool,
    refused: Option<Refusal>,
}

impl State<'_> {
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

    fn delivered(&self) -> Option<Delivered<'_>> {
        let (screen, at) = self.screen.as_ref()?;
        Some(Delivered {
            screen: Timed {
                body: screen,
                quoted_at: *at,
            },
            events: self.events.as_ref().map(|(e, at)| Timed {
                body: e,
                quoted_at: *at,
            }),
            explanation: self.explanation.as_ref(),
        })
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

    // Section 2.1: the facilitator is the mandate's, and its `/supported` pins the fee payer.
    let (quote_cfg, facilitator_note) = quoter_config(&cfg.public(), mandate);
    let quoter = Quoter::from_config(&quote_cfg, DEFAULT_QUOTE_TIMEOUT).await?;
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
        facilitator: quote_cfg.facilitator_url.clone(),
        quotes: Vec::new(),
        estimates: Vec::new(),
        planning: Vec::new(),
        steps: Vec::new(),
        outcomes: Vec::new(),
        claims: Vec::new(),
        brief_events_used: None,
        explanation: None,
        validation: None,
        rejected_deliveries: Vec::new(),
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
        brief_numbers: Vec::new(),
        reservation: None,
        step_no: 0,
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
        "service budget {} {} cap {} audit {} tinybar; assumption expected_material_pools {}; mandatory brief bound {} bytes",
        format_amount(mandate.budget.service_total, decimals),
        mandate.budget.service_asset,
        format_amount(mandate.constraints.max_single_payment, decimals),
        mandate.budget.audit_total,
        mandate.inputs.expected_material_pools,
        brief_bound
    ));
    st.out.say(format!(
        "facilitator {} signers {:?}{}",
        quote_cfg.facilitator_url,
        st.quoter.signers(),
        facilitator_note
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
    // receipt 0 cannot be, nothing is bought: the duty is unmeetable before
    // the first cent, and only audit budget was at stake.
    let r0 = Receipt::start(&mandate.id, &mandate.hash, &manifest.hash, &now_rfc3339());
    st.ledger.append_receipt(&mandate.id, &r0)?;
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
            return finish(st, cfg, ledger_path, topic).await;
        }
    }

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
    match drive(&mut st).await {
        Ok(()) => {}
        Err(Stop::Refused(r)) => refuse(&mut st, r)?,
        Err(Stop::Error(e)) => return Err(e),
    }

    // Section 9 over R, when anything was delivered and accepted.
    if let Some(delivered) = st.delivered() {
        let rules = st.rules();
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
        if !v.complete || st.incomplete {
            st.out.say("report incomplete: screening only".to_owned());
        }
        if st.explanation.is_none() && st.refused.is_none() {
            st.out
                .say("report has no explanation: the final step was not delivered".to_owned());
        }
        st.report.validation = Some(v);
    }

    finish(st, cfg, ledger_path, topic).await
}

/// Section 11 and 12: the queue, audit reconciliation, totals, receipts,
/// notices and the final status.
async fn finish(
    mut st: State<'_>,
    cfg: &Config,
    ledger_path: &Path,
    topic: HederaTopicId,
) -> Result<Report, RunError> {
    let mandate = st.mandate;
    let decimals = mandate.budget.service_decimals;
    // Section 11 and I9: the queue, retried when delivery must be anchored.
    // The mirror node lags a few seconds behind consensus; wait briefly for
    // the last fee records.
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
    let anchored = !anchoring || st.report.audit_pending.is_empty();
    let status = final_status(
        st.refused.is_some(),
        st.report.validation.as_ref(),
        st.explanation.is_some(),
        st.incomplete,
        anchored,
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
    st.report.transcript = std::mem::take(&mut st.out.lines);
    Ok(st.report)
}

/// Records a terminal refusal: transcript, report, receipt.
fn refuse(st: &mut State<'_>, r: Refusal) -> Result<(), RunError> {
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

/// Section 5 and the step loop: plan over W, buy the first step, check the
/// delivery, recompute, repeat.
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
            return Err(Stop::Refused(Refusal {
                code: Code::SellerUnreachable,
                detail: format!("{id}: quote expired before signing"),
            }));
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

/// Section 6 `received -> rejected` for a delivery that fails its own
/// checks: the reasons are persisted, a `failed` receipt is written, and
/// the run refuses rather than buying anything on top of it.
async fn reject_delivery(
    st: &mut State<'_>,
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
    st.ledger.append_receipt(&st.mandate.id, &r)?;
    publish(st).await?;
    st.report
        .rejected_deliveries
        .push((a.step.clone(), failures));
    Ok(Stop::Refused(Refusal {
        code: Code::EvidenceInsufficient,
        detail: format!("{} delivery rejected: {reason}", a.step),
    }))
}

/// Section 9 schema and freshness on the deliveries held so far, before any
/// of them authorizes another purchase.
async fn check_deliveries(st: &mut State<'_>, a: &Authorization) -> Result<(), Stop> {
    let failures = match st.delivered() {
        Some(d) => validate::validate_delivery(&st.rules(), &d),
        None => Vec::new(),
    };
    if failures.is_empty() {
        st.out.say(format!(
            "{} delivery checked: schema and freshness pass",
            a.step
        ));
        return Ok(());
    }
    Err(reject_delivery(st, a, failures).await?)
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
        screen
            .header
            .indexed_block_timestamp
            .map(|t| t.to_string())
            .unwrap_or_else(|| "null".to_owned()),
        screen.header.coverage_shortfall,
        screen.header.truncated,
        screen.requests
    ));
    let before = st.screen.replace((screen, quoted_at));
    if let Err(stop) = check_deliveries(st, &purchase.authorization).await {
        st.screen = before;
        return Err(stop);
    }
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
    let total: usize = events.pools.values().map(|p| p.all().count()).sum();
    st.out.say(format!(
        "events delivered: {} pool(s), {} events, truncated {}, requests {}",
        events.pools.len(),
        total,
        events.header.truncated,
        events.requests
    ));
    let before = st.events.replace((events, quoted_at));
    if let Err(stop) = check_deliveries(st, &purchase.authorization).await {
        st.events = before;
        return Err(stop);
    }
    recompute(st);
    Ok(())
}

async fn buy_explain(st: &mut State<'_>) -> Result<(), Stop> {
    let listing = listing_with(st, Capability::Explain)?;
    let bound = (listing.tariff.max_units * crate::manifest::KB) as usize;
    let blocks = st
        .screen
        .as_ref()
        .map(|(s, _)| (s.header.block_start, s.header.block_end))
        .unwrap_or((0, 0));
    let brief = analysis::build_brief(
        analysis::BriefInput {
            mandate_id: &st.mandate.id,
            window: st.window,
            blocks,
            outcomes: &st.outcomes,
            claims: &st.claims,
            events: st.events.as_ref().map(|(e, _)| e),
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
    st.report.brief_events_used = Some(brief.events_used);
    st.brief_numbers.clear();
    analysis::brief_numbers(&brief.json, &mut st.brief_numbers);
    st.out.say(format!(
        "brief {} bytes ({} KB), {} supporting event(s) per pool",
        brief.body.len(),
        brief.body.len().div_ceil(1024),
        brief.events_used
    ));
    let quote = usable_quote(st, &listing, brief.body.clone()).await?;
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
    Ok(())
}

async fn buy_investigate(st: &mut State<'_>, pending: &[String]) -> Result<(), Stop> {
    let listing = listing_with(st, Capability::Investigate)?;
    let body = evidence_body(pending, st.window, st.mandate);
    let quote = usable_quote(st, &listing, body).await?;
    buy_investigate_with(st, &listing, quote).await
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
}

/// Section 8's equality requirement on a bundle: the seller's outcomes and
/// claims are what the runtime computes from the delivered facts.
pub fn bundle_matches(
    bundle_outcomes: &[PoolOutcome],
    bundle_claims: &[Claim],
    ours: &(Vec<PoolOutcome>, Vec<Claim>),
) -> Vec<Failure> {
    let mut out = Vec::new();
    if bundle_outcomes != ours.0 {
        out.push(Failure {
            reason: validate::Reason::Calculation,
            detail: format!(
                "bundle outcomes {} differ from the recomputation {}",
                serde_json::to_string(bundle_outcomes).unwrap_or_default(),
                serde_json::to_string(&ours.0).unwrap_or_default()
            ),
        });
    }
    if bundle_claims != ours.1 {
        out.push(Failure {
            reason: validate::Reason::Calculation,
            detail: format!(
                "bundle claims ({}) differ from the recomputation ({})",
                bundle_claims.len(),
                ours.1.len()
            ),
        });
    }
    out
}

/// The bundle: facts, the seller's outcomes and claims, prose. The runtime
/// checks the facts like any delivery, recomputes outcomes and claims from
/// them and requires equality, section 8; anything else is a rejected
/// delivery.
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
    let body = purchase.body.as_deref().unwrap_or_default();
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
            return Err(reject_delivery(st, &purchase.authorization, f).await?);
        }
    };
    // One head for the bundle: the screen and the events share the quote time.
    let (prior_screen, prior_events) = (st.screen.clone(), st.events.clone());
    match st.screen.as_mut() {
        Some((mine, _)) => {
            // Hybrid: the bundle covers W and is the authority for those pools.
            for (pool, facts) in &bundle.screen.pools {
                mine.pools.insert(pool.clone(), facts.clone());
            }
        }
        None => st.screen = Some((bundle.screen.clone(), quoted_at)),
    }
    st.events = bundle.events.clone().map(|e| (e, quoted_at));
    let ours = outcomes_and_claims(
        &bundle.screen,
        bundle.events.as_ref(),
        st.mandate.requirements.evidence,
        st.thresholds(),
    );
    let mut failures = match st.delivered() {
        Some(d) => validate::validate_delivery(&st.rules(), &d),
        None => Vec::new(),
    };
    failures.extend(bundle_matches(&bundle.outcomes, &bundle.claims, &ours));
    if !failures.is_empty() {
        st.screen = prior_screen;
        st.events = prior_events;
        return Err(reject_delivery(st, &purchase.authorization, failures).await?);
    }
    st.out.say(format!(
        "{}: seller's outcomes and claims equal the runtime's recomputation",
        listing.id
    ));
    recompute(st);
    st.out.say(format!(
        "explanation from {} ({} input bytes): {}",
        bundle.explanation.model, bundle.explanation.input_bytes, bundle.explanation.prose
    ));
    st.explanation = Some(bundle.explanation);
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
        // Probe: an off-tariff explain quote after a passing validation.
        assert_eq!(
            final_status(true, Some(&ok), false, false, true),
            Status::Refused
        );
        assert_eq!(
            final_status(false, Some(&ok), false, false, true),
            Status::DeliveredWithFindings
        );
        assert_eq!(
            final_status(false, Some(&ok), true, false, true),
            Status::Delivered
        );
        assert_eq!(
            final_status(false, Some(&validation(false, true)), true, false, true),
            Status::DeliveredWithFindings
        );
        assert_eq!(
            final_status(false, Some(&validation(true, false)), true, false, true),
            Status::DeliveredWithFindings
        );
        assert_eq!(
            final_status(false, Some(&ok), true, true, true),
            Status::DeliveredWithFindings
        );
        assert_eq!(
            final_status(false, None, false, false, true),
            Status::DeliveredWithFindings
        );
        // Probe: anchoring required with receipts pending.
        assert_eq!(
            final_status(false, Some(&ok), true, false, false),
            Status::NotAnchored
        );
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
        // Probe: empty seller outcomes and claims.
        let f = bundle_matches(&[], &[], &ours);
        assert_eq!(f.len(), 2);
        assert!(f.iter().all(|f| f.reason == validate::Reason::Calculation));
    }
}
