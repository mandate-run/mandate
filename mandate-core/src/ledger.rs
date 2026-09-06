use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use thiserror::Error;

use crate::types::{Authorization, Mandate, PaymentState, Receipt, Reservation, ReservationState};

#[cfg(test)]
mod tests;

pub mod file;

/// A ledger failure. The spec (docs/spec.md section 3) requires the ledger to
/// be durable and every state change to be one transaction; the error type
/// distinguishes storage failures from invariant violations so callers can
/// refuse rather than spend on a broken ledger.
#[derive(Debug, Error)]
pub enum LedgerError {
    #[error("ledger store failure: {0}")]
    Store(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invariant violation: {0}")]
    Invariant(String),
}

pub type LedgerResult<T, E = LedgerError> = std::result::Result<T, E>;

/// Budget exposure columns, invariant I1: `settled + outstanding + held` never
/// exceeds `budget.service.total`, and no amount is counted in two columns (I7).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BudgetSnapshot {
    pub settled: i128,
    pub outstanding: i128,
    pub held: i128,
    pub audit_spent: i128,
}

impl BudgetSnapshot {
    pub fn free(&self, total: i128) -> i128 {
        total - self.settled - self.outstanding - self.held
    }
}

/// The durable ledger. Implementations must persist every change before
/// returning, so a crash never leaves a transmission without its commit (I8).
pub trait Ledger {
    /// Load the mandate this ledger serves. The mandate is part of the ledger:
    /// it pins the budget, constraints and requirements for every later check.
    fn load_mandate(&self, path: &Path) -> LedgerResult<Mandate>;

    fn insert_authorization(&mut self, auth: &Authorization) -> LedgerResult<()>;
    fn get_authorization(&self, id: &str) -> LedgerResult<Option<Authorization>>;
    fn update_authorization(&mut self, auth: &Authorization) -> LedgerResult<()>;
    fn list_authorizations(&self, mandate_id: &str) -> LedgerResult<Vec<Authorization>>;

    fn insert_reservation(&mut self, res: &Reservation) -> LedgerResult<()>;
    fn get_reservations(&self, mandate_id: &str) -> LedgerResult<Vec<Reservation>>;
    fn release_reservation(&mut self, id: &str) -> LedgerResult<()>;

    /// Persist a receipt and return its sequence number (I9).
    fn insert_receipt(&mut self, receipt: &Receipt) -> LedgerResult<u64>;
    fn receipts_since(&self, mandate_id: &str, seq: u64) -> LedgerResult<Vec<Receipt>>;

    fn budget_snapshot(&self, mandate_id: &str) -> LedgerResult<BudgetSnapshot>;
}

/// Reference in-memory ledger used by tests and by the fixture runner. Budget
/// columns are derived from state, never stored: `outstanding` sums
/// authorizations that are `prepared`, `sent` or `unresolved`; `held` sums
/// reservations in `held` (I7).
pub struct InMemoryLedger {
    authorizations: HashMap<String, Authorization>,
    reservations: BTreeMap<String, Reservation>,
    receipts: BTreeMap<(String, u64), Receipt>,
    next_receipt_seq: HashMap<String, u64>,
    audit_spent: HashMap<String, i128>,
}

impl InMemoryLedger {
    pub fn new() -> Self {
        Self {
            authorizations: HashMap::new(),
            reservations: BTreeMap::new(),
            receipts: BTreeMap::new(),
            next_receipt_seq: HashMap::new(),
            audit_spent: HashMap::new(),
        }
    }

    fn mandate_id(&self) -> &str {
        "dev-mandate"
    }

    pub fn add_audit_spend(&mut self, mandate_id: &str, amount: i128) {
        self.audit_spent
            .entry(mandate_id.to_string())
            .and_modify(|e| *e += amount)
            .or_insert(amount);
    }
}

impl Default for InMemoryLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl Ledger for InMemoryLedger {
    fn load_mandate(&self, _path: &Path) -> LedgerResult<Mandate> {
        Ok(crate::fixture::dev_mandate())
    }

    fn insert_authorization(&mut self, auth: &Authorization) -> LedgerResult<()> {
        if self.authorizations.contains_key(&auth.id) {
            return Err(LedgerError::Store(format!(
                "authorization already exists: {}",
                auth.id
            )));
        }
        self.check_budget_after_authorize(auth)?;
        self.authorizations.insert(auth.id.clone(), auth.clone());
        Ok(())
    }

    fn get_authorization(&self, id: &str) -> LedgerResult<Option<Authorization>> {
        Ok(self.authorizations.get(id).cloned())
    }

    fn update_authorization(&mut self, auth: &Authorization) -> LedgerResult<()> {
        if !self.authorizations.contains_key(&auth.id) {
            return Err(LedgerError::NotFound(format!(
                "authorization not found: {}",
                auth.id
            )));
        }
        self.check_budget_after_authorize(auth)?;
        self.authorizations.insert(auth.id.clone(), auth.clone());
        Ok(())
    }

    fn list_authorizations(&self, _mandate_id: &str) -> LedgerResult<Vec<Authorization>> {
        Ok(self.authorizations.values().cloned().collect())
    }

    fn insert_reservation(&mut self, res: &Reservation) -> LedgerResult<()> {
        self.check_budget_after_hold(res)?;
        self.reservations.insert(res.id.clone(), res.clone());
        Ok(())
    }

    fn get_reservations(&self, _mandate_id: &str) -> LedgerResult<Vec<Reservation>> {
        Ok(self.reservations.values().cloned().collect())
    }

    fn release_reservation(&mut self, id: &str) -> LedgerResult<()> {
        self.reservations.remove(id);
        Ok(())
    }

    fn insert_receipt(&mut self, receipt: &Receipt) -> LedgerResult<u64> {
        let key = (receipt.mandate_id.clone(), receipt.seq);
        self.receipts.insert(key, receipt.clone());
        let seq = receipt.seq;
        self.next_receipt_seq
            .entry(receipt.mandate_id.clone())
            .and_modify(|e| *e = (*e).max(seq + 1))
            .or_insert(seq + 1);
        Ok(seq)
    }

    fn receipts_since(&self, mandate_id: &str, seq: u64) -> LedgerResult<Vec<Receipt>> {
        Ok(self
            .receipts
            .range((mandate_id.to_string(), seq)..)
            .filter(|((id, _), _)| id == mandate_id)
            .map(|(_, r)| r.clone())
            .collect())
    }

    fn budget_snapshot(&self, mandate_id: &str) -> LedgerResult<BudgetSnapshot> {
        let mut settled = 0i128;
        let mut outstanding = 0i128;
        for auth in self.authorizations.values() {
            match auth.payment_state {
                PaymentState::Settled => settled += auth.amount,
                PaymentState::Prepared | PaymentState::Sent | PaymentState::Unresolved => {
                    outstanding += auth.amount
                }
                PaymentState::Failed => {}
            }
        }
        let held = self
            .reservations
            .values()
            .filter(|r| r.state == ReservationState::Held)
            .map(|r| r.amount)
            .sum();
        Ok(BudgetSnapshot {
            settled,
            outstanding,
            held,
            audit_spent: self.audit_spent.get(mandate_id).copied().unwrap_or(0),
        })
    }
}

/// Serializable snapshot of a ledger, used for CLI persistence and for
/// exchanging state between the fixture runner and the file-backed ledger.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct LedgerState {
    pub mandate_id: String,
    pub authorizations: Vec<Authorization>,
    pub reservations: Vec<Reservation>,
    pub receipts: Vec<Receipt>,
    pub audit_spent: i128,
}

impl InMemoryLedger {
    pub fn export(&self) -> LedgerState {
        let mut receipts: Vec<Receipt> = self.receipts.values().cloned().collect();
        receipts.sort_by_key(|r| (r.mandate_id.clone(), r.seq));
        LedgerState {
            mandate_id: self.mandate_id().to_string(),
            authorizations: self.authorizations.values().cloned().collect(),
            reservations: self.reservations.values().cloned().collect(),
            receipts,
            audit_spent: self.audit_spent.get(self.mandate_id()).copied().unwrap_or(0),
        }
    }

    pub fn import(state: LedgerState) -> Self {
        let mut ledger = Self::new();
        for auth in state.authorizations {
            ledger.authorizations.insert(auth.id.clone(), auth);
        }
        for res in state.reservations {
            ledger.reservations.insert(res.id.clone(), res);
        }
        let mut next_receipt_seq = std::collections::HashMap::new();
        for receipt in state.receipts {
            let seq = receipt.seq;
            ledger.receipts.insert((receipt.mandate_id.clone(), seq), receipt.clone());
            next_receipt_seq
                .entry(receipt.mandate_id.clone())
                .and_modify(|e: &mut u64| *e = (*e).max(seq + 1))
                .or_insert(seq + 1);
        }
        ledger.next_receipt_seq = next_receipt_seq;
        ledger
            .audit_spent
            .insert(state.mandate_id.clone(), state.audit_spent);
        ledger
    }

    /// I1: settling or authorizing must not push exposure past the budget.
    fn check_budget_after_authorize(&self, auth: &Authorization) -> LedgerResult<()> {
        let mandate = crate::fixture::dev_mandate();
        let mut snap = self.budget_snapshot(&mandate.id)?;
        // Recompute as if this authorization were outstanding.
        snap.outstanding += auth.amount;
        if snap.settled + snap.outstanding + snap.held > mandate.budget.service.total {
            return Err(LedgerError::Invariant(format!(
                "authorization {} ({}) would exceed service budget {}",
                auth.id, auth.amount, mandate.budget.service.total
            )));
        }
        Ok(())
    }

    fn check_budget_after_hold(&self, res: &Reservation) -> LedgerResult<()> {
        let mandate = crate::fixture::dev_mandate();
        let mut snap = self.budget_snapshot(&mandate.id)?;
        snap.held += res.amount;
        if snap.settled + snap.outstanding + snap.held > mandate.budget.service.total {
            return Err(LedgerError::Invariant(format!(
                "reservation {} ({}) would exceed service budget {}",
                res.id, res.amount, mandate.budget.service.total
            )));
        }
        Ok(())
    }
}