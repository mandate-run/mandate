//! The independent oracle. Proctor never asks the application how it did:
//! it reads the fixture sellers' journal and the buyer's ledger itself, and
//! turns them into named measurements a task contract can assert on.

use std::collections::BTreeSet;
use std::path::Path;

use serde::Deserialize;

use crate::checks::Measurements;

/// One line of the sellers' journal, as `sellers/src/journal.ts` writes it.
#[derive(Debug, Clone, Deserialize)]
pub struct Entry {
    pub route: String,
    pub payment_id: Option<String>,
    pub signed_payload_hash: Option<String>,
    pub settle_called: bool,
    pub settle_ok: bool,
    pub served_hash: Option<String>,
    pub status: u16,
    pub source: String,
}

/// Why the evidence cannot be trusted. Proctor cannot certify that nothing
/// was paid from a journal it could not fully read, so an unreadable or
/// partly malformed journal is an infrastructure error, never a quiet pass.
#[derive(Debug, thiserror::Error)]
pub enum EvidenceError {
    #[error("{path}: {problem}")]
    Unreadable { path: String, problem: String },
    #[error("{path} line {line}: {problem}")]
    Malformed {
        path: String,
        line: usize,
        problem: String,
    },
}

/// Reads the journal in full. Every line must parse: a fixture that writes
/// one malformed record could otherwise hide exactly the payment a contract
/// asserts did not happen.
pub fn read_journal(path: &Path) -> Result<Vec<Entry>, EvidenceError> {
    let text = std::fs::read_to_string(path).map_err(|e| EvidenceError::Unreadable {
        path: path.display().to_string(),
        problem: e.to_string(),
    })?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Entry>(line) {
            Ok(e) => out.push(e),
            Err(problem) => {
                return Err(EvidenceError::Malformed {
                    path: path.display().to_string(),
                    line: i + 1,
                    problem: problem.to_string(),
                });
            }
        }
    }
    Ok(out)
}

/// What the journal proves about a run, independently of what the buyer says.
pub fn from_journal(entries: &[Entry]) -> Measurements {
    let mut m = Measurements::new();
    let paid: Vec<&Entry> = entries.iter().filter(|e| e.payment_id.is_some()).collect();
    let payloads: BTreeSet<&str> = paid
        .iter()
        .filter_map(|e| e.signed_payload_hash.as_deref())
        .collect();
    let ids: BTreeSet<&str> = paid
        .iter()
        .filter_map(|e| e.payment_id.as_deref())
        .collect();
    let served: BTreeSet<&str> = entries
        .iter()
        .filter_map(|e| e.served_hash.as_deref())
        .collect();
    m.insert("fixture_requests".into(), entries.len().into());
    // Per route, so a contract can name the endpoint it cares about.
    let routes: BTreeSet<&str> = entries.iter().map(|e| e.route.as_str()).collect();
    m.insert("fixture_routes".into(), routes.len().into());
    for route in routes {
        let key = route.rsplit('/').next().unwrap_or(route);
        m.insert(
            format!("fixture_requests_{key}"),
            entries.iter().filter(|e| e.route == route).count().into(),
        );
    }
    // Every transmission that carried a payment: submissions and retrievals.
    m.insert("fixture_signed_payloads".into(), paid.len().into());
    // How many distinct signed payments the seller ever saw. More than one
    // per purchase means the buyer signed twice for the same work.
    m.insert("fixture_distinct_payloads".into(), payloads.len().into());
    m.insert("fixture_payment_ids".into(), ids.len().into());
    m.insert(
        "fixture_settle_calls".into(),
        entries.iter().filter(|e| e.settle_called).count().into(),
    );
    // The only measurement that says money moved.
    m.insert(
        "fixture_settlements".into(),
        entries.iter().filter(|e| e.settle_ok).count().into(),
    );
    m.insert(
        "fixture_served_from_store".into(),
        entries
            .iter()
            .filter(|e| e.source == "store")
            .count()
            .into(),
    );
    m.insert(
        "fixture_dropped".into(),
        entries
            .iter()
            .filter(|e| e.source == "dropped")
            .count()
            .into(),
    );
    m.insert(
        "fixture_rejected".into(),
        entries
            .iter()
            .filter(|e| e.source == "rejected")
            .count()
            .into(),
    );
    m.insert("fixture_distinct_results".into(), served.len().into());
    m.insert(
        "fixture_errors".into(),
        entries.iter().filter(|e| e.status >= 400).count().into(),
    );
    m
}

/// What the buyer's own ledger holds, read as a file rather than asked for.
/// A missing ledger yields nothing, so a task that never pays needs none.
pub fn from_ledger(path: &Path, mandate_id: &str) -> Measurements {
    let mut m = Measurements::new();
    let Ok(conn) = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    ) else {
        return m;
    };
    let count = |sql: &str| -> Option<i64> {
        conn.query_row(sql, rusqlite::params![mandate_id], |r| r.get(0))
            .ok()
    };
    if let Some(n) = count("SELECT COUNT(*) FROM authorizations WHERE mandate_id = ?1") {
        m.insert("ledger_authorizations".into(), n.into());
    }
    if let Some(n) =
        count("SELECT COUNT(DISTINCT payment_id) FROM authorizations WHERE mandate_id = ?1")
    {
        m.insert("ledger_payment_ids".into(), n.into());
    }
    if let Some(n) =
        count("SELECT COUNT(DISTINCT signature) FROM authorizations WHERE mandate_id = ?1")
    {
        m.insert("ledger_signed_payloads".into(), n.into());
    }
    if let Some(n) =
        count("SELECT COALESCE(SUM(submissions), 0) FROM authorizations WHERE mandate_id = ?1")
    {
        m.insert("ledger_submissions".into(), n.into());
    }
    if let Some(n) =
        count("SELECT COALESCE(SUM(retrievals), 0) FROM authorizations WHERE mandate_id = ?1")
    {
        m.insert("ledger_retrievals".into(), n.into());
    }
    for state in ["settled", "failed", "unresolved", "sent", "prepared"] {
        if let Ok(n) = conn.query_row(
            "SELECT COUNT(*) FROM authorizations WHERE mandate_id = ?1 AND payment_state = ?2",
            rusqlite::params![mandate_id, state],
            |r| r.get::<_, i64>(0),
        ) {
            m.insert(format!("ledger_{state}"), n.into());
        }
    }
    if let Some(n) = count("SELECT COUNT(*) FROM receipts WHERE mandate_id = ?1") {
        m.insert("ledger_receipts".into(), n.into());
    }
    if let Some(n) =
        count("SELECT COUNT(*) FROM receipts WHERE mandate_id = ?1 AND hcs_sequence IS NOT NULL")
    {
        m.insert("ledger_receipts_published".into(), n.into());
    }
    if let Some(n) = count(
        "SELECT COALESCE(SUM(amount), 0) FROM reservations WHERE mandate_id = ?1 AND state = 'held'",
    ) {
        m.insert("ledger_held".into(), n.into());
    }
    m
}

/// Everything Proctor measured for one attempt, or why it could not. An
/// evidence source the task names and Proctor cannot read in full stops the
/// run: assertions over partial evidence would certify the wrong thing.
pub fn gather(evidence: &crate::task::Evidence, dir: &Path) -> Result<Measurements, EvidenceError> {
    let mut m = Measurements::new();
    if let Some(journal) = &evidence.journal {
        let path = dir.join(journal);
        // A journal the fixture has not written yet is not evidence of
        // anything, and a check before the first request is legitimate.
        if path.exists() {
            m.extend(from_journal(&read_journal(&path)?));
        } else {
            m.insert("fixture_journal_missing".into(), true.into());
        }
    }
    if let (Some(ledger), Some(id)) = (&evidence.ledger, &evidence.mandate_id) {
        m.extend(from_ledger(&dir.join(ledger), id));
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(payment: &str, payload: &str, settled: bool, source: &str, served: &str) -> String {
        format!(
            r#"{{"ts":"t","route":"POST /events","payment_id":"{payment}","signed_payload_hash":"{payload}","settle_called":true,"settle_ok":{settled},"served_hash":"{served}","status":200,"source":"{source}"}}"#
        )
    }

    #[test]
    fn the_journal_shows_one_payment_recovered_by_a_retrieval() {
        // A lost response: three submissions and one retrieval, all the same
        // signed payment, one settlement, one result.
        let lines = [
            entry("pay_1", "hash_a", true, "dropped", "res_1"),
            entry("pay_1", "hash_a", false, "dropped", "res_1"),
            entry("pay_1", "hash_a", false, "dropped", "res_1"),
            entry("pay_1", "hash_a", false, "store", "res_1"),
        ];
        let entries: Vec<Entry> = lines
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let m = from_journal(&entries);
        assert_eq!(m["fixture_signed_payloads"], 4);
        assert_eq!(
            m["fixture_distinct_payloads"], 1,
            "one signature throughout"
        );
        assert_eq!(m["fixture_payment_ids"], 1, "one payment id, I6");
        assert_eq!(m["fixture_settlements"], 1, "the seller settled once");
        assert_eq!(
            m["fixture_served_from_store"], 1,
            "the retrieval was served from the store"
        );
        assert_eq!(m["fixture_dropped"], 3);
        assert_eq!(
            m["fixture_distinct_results"], 1,
            "the same result throughout"
        );
    }

    #[test]
    fn a_second_signature_for_one_purchase_is_visible() {
        // What a double payment looks like from outside the application.
        let entries: Vec<Entry> = [
            entry("pay_1", "hash_a", true, "live", "res_1"),
            entry("pay_2", "hash_b", true, "live", "res_1"),
        ]
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
        let m = from_journal(&entries);
        assert_eq!(m["fixture_distinct_payloads"], 2);
        assert_eq!(m["fixture_payment_ids"], 2);
        assert_eq!(
            m["fixture_settlements"], 2,
            "money moved twice for one result"
        );
        assert_eq!(m["fixture_distinct_results"], 1);
    }

    #[test]
    fn a_missing_journal_is_itself_a_measurement() {
        let ev = crate::task::Evidence {
            journal: Some("nowhere.jsonl".to_owned()),
            ledger: None,
            mandate_id: None,
        };
        let m = gather(&ev, Path::new("/tmp")).unwrap();
        assert_eq!(m["fixture_journal_missing"], true);
    }

    /// The probe: a journal holding one valid unpaid request and one
    /// malformed payment record passed assertions requiring zero payments.
    /// Proctor cannot certify "nothing was paid" from evidence it could not
    /// read, so this is an infrastructure error naming the line.
    #[test]
    fn a_malformed_journal_line_is_never_silently_dropped() {
        let dir = std::env::temp_dir().join(format!("proctor-journal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("journal.jsonl");
        let valid = entry("pay_1", "hash_a", true, "live", "res_1");
        std::fs::write(
            &path,
            format!(
                "{}\n{{\"route\":\"POST /events\",\"payment_id\":\n",
                valid.replace("\"payment_id\":\"pay_1\"", "\"payment_id\":null")
            ),
        )
        .unwrap();
        let e = read_journal(&path).unwrap_err();
        let text = e.to_string();
        assert!(
            text.contains("line 2"),
            "the diagnostic names the line: {text}"
        );
        let ev = crate::task::Evidence {
            journal: Some("journal.jsonl".to_owned()),
            ledger: None,
            mandate_id: None,
        };
        assert!(
            gather(&ev, &dir).is_err(),
            "partial evidence never measures"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_ledger_is_read_as_a_file_not_asked() {
        use mandate::ledger::Ledger;
        use mandate::testing::{mandate_row, request, signed_payment};
        let dir = std::env::temp_dir().join(format!("proctor-obs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ledger.sqlite");
        let now = time::OffsetDateTime::now_utc();
        {
            let mut l = Ledger::open(&path).unwrap();
            l.insert_mandate(&mandate_row(), now).unwrap();
            let p = signed_payment(1, 1_500, "0.0.429274", now);
            let a = l
                .prepare("m1", "events", None, &p, &request(), now)
                .unwrap();
            l.commit_submission(a.id, now).unwrap();
        }
        let m = from_ledger(&path, "m1");
        assert_eq!(m["ledger_authorizations"], 1);
        assert_eq!(m["ledger_payment_ids"], 1);
        assert_eq!(m["ledger_signed_payloads"], 1);
        assert_eq!(m["ledger_submissions"], 1);
        assert_eq!(m["ledger_prepared"], 0);
        assert_eq!(m["ledger_sent"], 1);
        assert_eq!(m["ledger_settled"], 0);
        // A ledger that is not there measures nothing, rather than failing.
        assert!(from_ledger(&dir.join("absent.sqlite"), "m1").is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
