//! CLI command implementations. The transports are simulated by the fixture
//! runner; persistence is the JSON file ledger. Each function is pure enough
//! to test in-process.

use std::path::Path;

use crate::cli::init::Command as InitCommand;
use crate::cli::ledger_cmd::Command as LedgerCommand;
use crate::cli::receipts::Command as ReceiptsCommand;
use crate::cli::reconcile::Command as ReconcileCommand;
use crate::cli::run::Command as RunCommand;
use crate::fixture::{self, SERVICE_DECIMALS};
use crate::ledger::{file::FileLedger, Ledger, LedgerState};
use crate::types::*;

pub const DEFAULT_LEDGER: &str = "ledger.json";
pub const DEFAULT_MANIFEST: &str = "manifest.json";
pub const DEFAULT_TRANSCRIPT: &str = "transcript.json";

/// `mandate init`: write the mandate, manifest and an empty ledger.
pub fn execute_init(cmd: &InitCommand) -> Result<(), String> {
    let dir = &cmd.out_dir;
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;

    let mut mandate = fixture::dev_mandate();
    mandate.principal = cmd.principal.clone();
    mandate.purpose = cmd.purpose.clone();
    mandate.budget.service.total = cmd.service_total;
    mandate.budget.audit.total = cmd.audit_total;

    write_json(&dir.join("mandate.json"), &mandate)?;
    write_json(&dir.join(DEFAULT_MANIFEST), &fixture::dev_listings())?;
    let state = LedgerState {
        mandate_id: mandate.id.clone(),
        ..Default::default()
    };
    write_json(&dir.join(DEFAULT_LEDGER), &state)?;
    Ok(())
}

/// `mandate run`: execute the fixture scenario against the file ledger and
/// return the transcript (and its JSON serialization).
pub fn execute_run(cmd: &RunCommand) -> Result<Transcript, String> {
    let mandate: Mandate = read_json(&cmd.mandate_path)?;
    let listings: Vec<Listing> = read_json(&cmd.manifest_path)?;

    // Each run starts from a clean ledger for this mandate.
    write_json(
        &cmd.ledger_path,
        &LedgerState {
            mandate_id: mandate.id.clone(),
            ..Default::default()
        },
    )?;
    let mut ledger = FileLedger::open_or_create(&cmd.ledger_path, &mandate.id)
        .map_err(|e| format!("open ledger: {e}"))?;

    let run = fixture::run_scenario(&mandate, &listings, cmd.scenario)
        .map_err(|e| format!("run: {e}"))?;

    // Persist the final ledger state through the file ledger.
    let state = run.state;
    for auth in &state.authorizations {
        ledger.insert_authorization(auth).map_err(|e| e.to_string())?;
    }
    for res in &state.reservations {
        ledger.insert_reservation(res).map_err(|e| e.to_string())?;
    }
    for receipt in &state.receipts {
        ledger.insert_receipt(receipt).map_err(|e| e.to_string())?;
    }
    if state.audit_spent > 0 {
        ledger
            .add_audit_spend(&mandate.id, state.audit_spent)
            .map_err(|e| e.to_string())?;
    }

    let transcript = run.transcript;
    write_json(&cmd.transcript_path, &transcript)?;
    Ok(transcript)
}

/// `mandate reconcile`: re-check settlement for every non-terminal
/// authorization (spec section 6, Reconcile). In the fixture there are no new
/// mirror records, so an authorization still `prepared`/`sent` past its
/// `valid_until + 30 s` becomes `unresolved`; nothing else changes. Returns a
/// human-readable summary and the list of unresolved authorization ids.
pub fn execute_reconcile(cmd: &ReconcileCommand) -> Result<String, String> {
    let mut ledger = FileLedger::open_or_create(&cmd.ledger_path, &cmd.mandate_id)
        .map_err(|e| format!("open ledger: {e}"))?;

    let now = chrono::Utc::now();
    let mut unresolved = Vec::new();
    let mut checked = 0usize;
    let auths = ledger
        .list_authorizations(&cmd.mandate_id)
        .map_err(|e| e.to_string())?;
    for mut auth in auths {
        if matches!(
            auth.payment_state,
            PaymentState::Settled | PaymentState::Failed
        ) {
            continue;
        }
        checked += 1;
        // I5: no SUCCESS record and at least one failure record -> failed;
        // empty record set past valid_until + 30 s -> unresolved.
        if auth.valid_until + chrono::Duration::seconds(30) < now {
            if auth.payment_state != PaymentState::Unresolved {
                auth.payment_state = PaymentState::Unresolved;
                ledger
                    .update_authorization(&auth)
                    .map_err(|e| e.to_string())?;
            }
            unresolved.push(auth.id.clone());
        }
    }
    Ok(format!(
        "reconciled {} non-terminal authorization(s); {} unresolved",
        checked,
        unresolved.len()
    ))
}

/// `mandate ledger`: budget snapshot plus every authorization.
pub fn execute_ledger(cmd: &LedgerCommand) -> Result<String, String> {
    let ledger = FileLedger::open_or_create(&cmd.ledger_path, &cmd.mandate_id)
        .map_err(|e| format!("open ledger: {e}"))?;
    let snap = ledger
        .budget_snapshot(&cmd.mandate_id)
        .map_err(|e| e.to_string())?;
    let auths = ledger
        .list_authorizations(&cmd.mandate_id)
        .map_err(|e| e.to_string())?;
    let mut out = String::new();
    out.push_str(&format!(
        "budget settled {} outstanding {} held {} audit {}\n",
        fmt_amount(snap.settled, SERVICE_DECIMALS),
        fmt_amount(snap.outstanding, SERVICE_DECIMALS),
        fmt_amount(snap.held, SERVICE_DECIMALS),
        fmt_amount(snap.audit_spent, 8),
    ));
    for a in &auths {
        out.push_str(&format!(
            "  auth {} {} {} payment {:?} delivery {:?} submissions {} retrievals {}\n",
            a.id,
            fmt_amount(a.amount, SERVICE_DECIMALS),
            a.tx_id,
            a.payment_state,
            a.delivery_state,
            a.submissions,
            a.retrievals
        ));
    }
    Ok(out)
}

/// `mandate receipts`: receipts in sequence, with pending publication state.
pub fn execute_receipts(cmd: &ReceiptsCommand) -> Result<String, String> {
    let ledger = FileLedger::open_or_create(&cmd.ledger_path, &cmd.mandate_id)
        .map_err(|e| format!("open ledger: {e}"))?;
    let receipts = ledger
        .receipts_since(&cmd.mandate_id, 0)
        .map_err(|e| e.to_string())?;
    let mut out = String::new();
    for r in &receipts {
        out.push_str(&format!(
            "  seq {} {} {} outcome {:?} {}\n",
            r.seq,
            r.step,
            fmt_amount(r.amount, SERVICE_DECIMALS),
            r.outcome,
            r.tx_id.as_deref().unwrap_or("-")
        ));
    }
    Ok(out)
}

// ---- text rendering ----

/// Render the transcript the way the demo prints it (docs/demo.md), following
/// the order of spec section 12.
pub fn render_text(t: &Transcript) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "mandate {}: coverage all_material, planning assumption {} material pool, mandatory brief bound {} bytes\n",
        t.mandate, t.assumption, t.brief_bound
    ));

    out.push_str("quotes:\n");
    for q in &t.quotes {
        match q.source.as_str() {
            "live" => out.push_str(&format!(
                "  {} {} live, within ceiling {}, listing match {}, fee payer ok {}\n",
                q.listing_id, q.amount, q.ceiling, q.listing_match, q.fee_payer_ok
            )),
            _ => out.push_str(&format!(
                "  {} {} ceiling (estimate)\n",
                q.listing_id, q.amount
            )),
        }
    }

    for phase in &t.plan_phases {
        out.push_str(&format!(
            "plan {} expected {} bound {} chosen ({} authorizations, assumption {})\n",
            phase.chosen.name, phase.chosen.expected, phase.chosen.bound, phase.chosen.authorizations, t.assumption
        ));
        for r in &phase.rejected {
            out.push_str(&format!(
                "  rejected {} expected {} bound {} ({})\n",
                r.name, r.expected, r.bound, r.reason
            ));
        }
    }

    for res in &t.reservations {
        out.push_str(&format!(
            "reserve {} {} {} ({})\n",
            res.step, res.amount, res.state, res.source
        ));
    }

    for s in &t.steps {
        out.push_str(&format!(
            "step {} pay {} tx {} submissions {} retrievals {}\n",
            s.step, s.payment_id, s.tx_id, s.submissions, s.retrievals
        ));
        for tr in &s.transitions {
            match tr.record_count {
                Some(n) => out.push_str(&format!(
                    "  {} -> {} (records {}, duplicates ignored {})\n",
                    tr.from,
                    tr.to,
                    n,
                    tr.duplicates_ignored.unwrap_or(0)
                )),
                None => out.push_str(&format!("  {} -> {}\n", tr.from, tr.to)),
            }
        }
        out.push_str("  payment settled: record matches\n");
    }

    let supported: usize = t.outcomes.iter().filter(|o| o.outcome == "supported").count();
    let pending: usize = t.outcomes.iter().filter(|o| o.outcome == "pending").count();
    let non_material: usize = t.outcomes.iter().filter(|o| o.outcome == "non_material").count();
    let claims: usize = t.outcomes.iter().map(|o| o.claim_count).sum();
    out.push_str(&format!(
        "outcomes: {} supported, {} pending, {} non_material; claims {}\n",
        supported, pending, non_material, claims
    ));

    if !t.refusals.is_empty() {
        out.push_str("refusals:\n");
        for r in &t.refusals {
            out.push_str(&format!(
                "  REFUSED {} needed bound {} needed expected {} available {}\n",
                r.code, r.needed_bound, r.needed_expected, r.available
            ));
            if let Some(reason) = &r.reason {
                out.push_str(&format!("    reason: {reason}\n"));
            }
        }
    }

    if t.steps.is_empty() {
        out.push_str("no report produced; every purchase refused\n");
    } else if t.validation.complete {
        out.push_str(&format!(
            "validation passed: coverage {}, calculations {}, citations {}, provenance {}\n",
            t.validation.coverage, t.validation.calculations, t.validation.citations, t.validation.provenance
        ));
    } else {
        out.push_str(&format!(
            "validation FAILED: coverage {}, calculations {}, citations {}, provenance {}, incomplete: {}\n",
            t.validation.coverage,
            t.validation.calculations,
            t.validation.citations,
            t.validation.provenance,
            t.validation.incomplete_reason.as_deref().unwrap_or("unknown")
        ));
    }

    out.push_str(&format!(
        "totals: settled {} released {} unspent {} unresolved {} audit spent {}\n",
        t.totals.settled, t.totals.released, t.totals.unspent, t.totals.unresolved, t.totals.audit_spent
    ));
    out.push_str(&format!(
        "receipts: HCS topic {}, pending publication {}\n",
        t.receipts.topic,
        if t.receipts.pending.is_empty() {
            "none".to_string()
        } else {
            format!("{:?}", t.receipts.pending)
        }
    ));
    if t.totals.unresolved != "0.0000" {
        out.push_str("notice: run `mandate reconcile <mandate_id>` for the unresolved authorization\n");
    }
    out
}

// ---- file helpers ----

pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))
}

pub fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| format!("serialize {}: {e}", path.display()))?;
    std::fs::write(path, bytes).map_err(|e| format!("write {}: {e}", path.display()))
}
