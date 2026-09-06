//! A file-backed ledger for the CLI: every mutation is persisted to a JSON
//! file before returning, so `mandate run` output survives to
//! `mandate reconcile`, `mandate ledger` and `mandate receipts`.
//!
//! The spec (docs/spec.md section 3) requires a durable SQLite ledger; this
//! JSON file ledger is the demo-grade stand-in until the libsql store lands.
//! The `Ledger` trait boundary is the seam where that swap happens.

use std::path::{Path, PathBuf};

use super::{InMemoryLedger, Ledger, LedgerError, LedgerResult, LedgerState};
use crate::types::{Authorization, Mandate, Receipt, Reservation};

pub struct FileLedger {
    path: PathBuf,
    inner: InMemoryLedger,
}

impl FileLedger {
    pub fn add_audit_spend(&mut self, mandate_id: &str, amount: i128) -> LedgerResult<()> {
        self.inner.add_audit_spend(mandate_id, amount);
        self.save()
    }

    /// Open an existing ledger file, or create an empty one for `mandate_id`
    /// when it does not exist yet.
    pub fn open_or_create(path: &Path, mandate_id: &str) -> LedgerResult<Self> {
        let inner = match std::fs::read(path) {
            Ok(bytes) => {
                let state: LedgerState = serde_json::from_slice(&bytes).map_err(|e| {
                    LedgerError::Store(format!("corrupt ledger {}: {e}", path.display()))
                })?;
                InMemoryLedger::import(state)
            }
            Err(_) => InMemoryLedger::new(),
        };
        let _ = mandate_id;
        Ok(Self {
            path: path.to_path_buf(),
            inner,
        })
    }

    pub fn save(&self) -> LedgerResult<()> {
        let state = self.inner.export();
        let bytes = serde_json::to_vec_pretty(&state)
            .map_err(|e| LedgerError::Store(format!("serialize ledger: {e}")))?;
        std::fs::write(&self.path, bytes)
            .map_err(|e| LedgerError::Store(format!("write {}: {e}", self.path.display())))?;
        Ok(())
    }
}

impl Ledger for FileLedger {
    fn load_mandate(&self, path: &Path) -> LedgerResult<Mandate> {
        let bytes = std::fs::read(path)
            .map_err(|e| LedgerError::Store(format!("read {}: {e}", path.display())))?;
        serde_json::from_slice(&bytes)
            .map_err(|e| LedgerError::Store(format!("parse {}: {e}", path.display())))
    }

    fn insert_authorization(&mut self, auth: &Authorization) -> LedgerResult<()> {
        self.inner.insert_authorization(auth)?;
        self.save()
    }

    fn get_authorization(&self, id: &str) -> LedgerResult<Option<Authorization>> {
        self.inner.get_authorization(id)
    }

    fn update_authorization(&mut self, auth: &Authorization) -> LedgerResult<()> {
        self.inner.update_authorization(auth)?;
        self.save()
    }

    fn list_authorizations(&self, mandate_id: &str) -> LedgerResult<Vec<Authorization>> {
        self.inner.list_authorizations(mandate_id)
    }

    fn insert_reservation(&mut self, res: &Reservation) -> LedgerResult<()> {
        self.inner.insert_reservation(res)?;
        self.save()
    }

    fn get_reservations(&self, mandate_id: &str) -> LedgerResult<Vec<Reservation>> {
        self.inner.get_reservations(mandate_id)
    }

    fn release_reservation(&mut self, id: &str) -> LedgerResult<()> {
        self.inner.release_reservation(id)?;
        self.save()
    }

    fn insert_receipt(&mut self, receipt: &Receipt) -> LedgerResult<u64> {
        let seq = self.inner.insert_receipt(receipt)?;
        self.save()?;
        Ok(seq)
    }

    fn receipts_since(&self, mandate_id: &str, seq: u64) -> LedgerResult<Vec<Receipt>> {
        self.inner.receipts_since(mandate_id, seq)
    }

    fn budget_snapshot(&self, mandate_id: &str) -> LedgerResult<super::BudgetSnapshot> {
        self.inner.budget_snapshot(mandate_id)
    }
}