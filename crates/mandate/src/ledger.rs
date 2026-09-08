//! Spec section 3: the durable ledger, SQLite, and section 6: the purchase
//! state machine. Every state change is one transaction, and the transitions
//! here are the only way a row changes. Amounts are atomic units.
//!
//! Accounting is derived, never stored: `held` sums reservations in `held`,
//! `outstanding` sums authorizations in `prepared`, `sent` or `unresolved`,
//! `settled` sums authorizations in `settled`. I1 is checked in the same
//! transaction as every change that could raise the sum.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension as _, Row, params};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use crate::hedera::{self, RECORD_GRACE, Settlement};
use crate::receipts::Receipt;
use crate::x402::{self, PaymentPayload, sha256_hex};

pub const MAX_SUBMISSIONS: u32 = 3;
/// Section 6: resends and retrievals at most once per 30 seconds.
pub const RETRY_SPACING: time::Duration = time::Duration::seconds(30);

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("time: {0}")]
    Time(String),
    #[error("mandate {0} is not in the ledger")]
    MandateNotFound(String),
    #[error("mandate {0} is already in the ledger with a different hash")]
    MandateHashDiffers(String),
    #[error("OVER_BUDGET: {amount} exceeds the free service budget {free}")]
    OverBudget { amount: i64, free: i64 },
    #[error("AUDIT_OVER_BUDGET: cap {cap} exceeds the free audit budget {free}")]
    AuditOverBudget { cap: i64, free: i64 },
    #[error("OUTSIDE_CONSTRAINTS: amount {amount} above max_single_payment {cap}")]
    AboveSinglePayment { amount: i64, cap: i64 },
    #[error("OUTSIDE_CONSTRAINTS: deadline {deadline} has passed")]
    PastDeadline { deadline: String },
    #[error("reservation {id} is {state}, not held for step {step}")]
    ReservationNotUsable {
        id: i64,
        state: String,
        step: String,
    },
    #[error("reservation {id} holds {held}, less than {amount}")]
    ReservationTooSmall { id: i64, held: i64, amount: i64 },
    #[error("authorization {id} is {payment}/{delivery}: {wanted}")]
    WrongState {
        id: i64,
        payment: String,
        delivery: String,
        wanted: &'static str,
    },
    #[error("authorization {id} has used all {MAX_SUBMISSIONS} submissions")]
    SubmissionsExhausted { id: i64 },
    #[error("authorization {id} expired at {valid_until}")]
    Expired { id: i64, valid_until: String },
    #[error("authorization {id} already holds a different response")]
    ResponseDiffers { id: i64 },
    #[error("{what} {id} not found")]
    NotFound { what: &'static str, id: i64 },
    #[error("audit charge {id} is {state}: {wanted}")]
    AuditWrongState {
        id: i64,
        state: String,
        wanted: &'static str,
    },
    #[error("OUTSIDE_CONSTRAINTS: payment asset {asset} is not the service asset {service_asset}")]
    AssetMismatch {
        asset: String,
        service_asset: String,
    },
    #[error("authorization {id}: next {what} allowed at {next_at}")]
    TooSoon {
        id: i64,
        what: &'static str,
        next_at: String,
    },
    #[error("audit charge {id}: a charge cannot be negative")]
    AuditChargeNegative { id: i64 },
    #[error("audit charge {id} is still within its validity until {valid_until}")]
    AuditStillValid { id: i64, valid_until: String },
}

/// Why a signature header cannot become a `PreparedPayment`.
#[derive(Debug, thiserror::Error)]
pub enum BindingError {
    #[error("signature header: {0}")]
    Header(#[from] x402::HeaderError),
    #[error("signed transaction is not base64: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("signed transaction: {0}")]
    Bytes(#[from] hedera::Error),
    #[error("header carries payment id {header:?}, expected {expected}")]
    PaymentId {
        header: Option<String>,
        expected: String,
    },
    #[error("signed transfer {0}")]
    Shape(&'static str),
    #[error("accepted {field} {accepted} differs from the signed {signed}")]
    Accepted {
        field: &'static str,
        accepted: String,
        signed: String,
    },
    #[error("quote json: {0}")]
    Json(#[from] serde_json::Error),
}

type Result<T> = std::result::Result<T, LedgerError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaymentState {
    Prepared,
    Sent,
    Settled,
    Failed,
    Unresolved,
}

impl PaymentState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Sent => "sent",
            Self::Settled => "settled",
            Self::Failed => "failed",
            Self::Unresolved => "unresolved",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "prepared" => Self::Prepared,
            "sent" => Self::Sent,
            "settled" => Self::Settled,
            "failed" => Self::Failed,
            _ => Self::Unresolved,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Settled | Self::Failed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryState {
    None,
    Received,
    Validated,
    Rejected,
}

impl DeliveryState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Received => "received",
            Self::Validated => "validated",
            Self::Rejected => "rejected",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "received" => Self::Received,
            "validated" => Self::Validated,
            "rejected" => Self::Rejected,
            _ => Self::None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservationSource {
    CeilingAtMax,
    Quote,
}

impl ReservationSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CeilingAtMax => "ceiling_at_max",
            Self::Quote => "quote",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservationState {
    Held,
    Consumed,
    Released,
}

impl ReservationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Held => "held",
            Self::Consumed => "consumed",
            Self::Released => "released",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "held" => Self::Held,
            "consumed" => Self::Consumed,
            _ => Self::Released,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reservation {
    pub id: i64,
    pub mandate_id: String,
    pub step: String,
    pub amount: i64,
    pub source: ReservationSource,
    pub state: ReservationState,
}

/// The exact request a transmission sends, section 2.5.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    #[serde(with = "body_base64")]
    pub body: Vec<u8>,
}

mod body_base64 {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize as _, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        STANDARD.decode(text).map_err(serde::de::Error::custom)
    }
}

impl Request {
    pub fn hash(&self) -> String {
        sha256_hex(&serde_json::to_vec(self).expect("request serializes"))
    }
}

/// What the signer produced for an approved quote, section 2.5. Every
/// accounting field is read from the signed bytes and the accepted terms in
/// the header; nothing here is caller supplied, and the type cannot be built
/// any other way.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PreparedPayment {
    pub payment_id: String,
    pub tx_id: String,
    pub mirror_id: String,
    /// The runtime account the transfer debits.
    pub payer: String,
    pub amount: i64,
    pub asset: String,
    pub pay_to: String,
    pub fee_payer: String,
    pub valid_start: OffsetDateTime,
    pub valid_until: OffsetDateTime,
    /// The exact `PAYMENT-SIGNATURE` header value.
    pub signature: String,
    /// The approved quote, section 2.3, as JSON; the accepted terms by default.
    pub quote_json: String,
}

impl PreparedPayment {
    /// Decodes the header, inspects the signed transaction, and binds the two:
    /// one debit, one credit of the same amount in one asset, the accepted
    /// amount, asset, recipient and fee payer equal to the signed ones, and
    /// the payment id in the extension equal to `payment_id`.
    pub fn from_signature(
        signature: &str,
        payment_id: &str,
    ) -> std::result::Result<Self, BindingError> {
        let payload: PaymentPayload = x402::decode_header(signature)?;
        if payload.payment_id() != Some(payment_id) {
            return Err(BindingError::PaymentId {
                header: payload.payment_id().map(str::to_owned),
                expected: payment_id.to_owned(),
            });
        }
        let bytes = STANDARD.decode(payload.payload.transaction.trim())?;
        let seen = hedera::inspect(&bytes)?;
        let (asset, debit, credit) = match (seen.hbar.as_slice(), seen.tokens.as_slice()) {
            ([a, b], []) => {
                let (d, c) = if a.1 < 0 { (a, b) } else { (b, a) };
                (
                    hedera::HBAR_ASSET_ID.to_owned(),
                    (d.0.to_string(), d.1),
                    (c.0.to_string(), c.1),
                )
            }
            ([], [a, b]) => {
                if a.token != b.token {
                    return Err(BindingError::Shape("moves two different tokens"));
                }
                let (d, c) = if a.amount < 0 { (a, b) } else { (b, a) };
                (
                    a.token.to_string(),
                    (d.account.to_string(), d.amount),
                    (c.account.to_string(), c.amount),
                )
            }
            _ => {
                return Err(BindingError::Shape(
                    "must be exactly one debit and one credit",
                ));
            }
        };
        if debit.1 >= 0 || credit.1 <= 0 || debit.1 != -credit.1 {
            return Err(BindingError::Shape(
                "debit and credit must be equal and opposite",
            ));
        }
        let amount = credit.1;
        let accepted = &payload.accepted;
        let accepted_amount: i64 = accepted
            .amount
            .parse()
            .map_err(|_| BindingError::Accepted {
                field: "amount",
                accepted: accepted.amount.clone(),
                signed: amount.to_string(),
            })?;
        if accepted_amount != amount {
            return Err(BindingError::Accepted {
                field: "amount",
                accepted: accepted.amount.clone(),
                signed: amount.to_string(),
            });
        }
        let accepted_asset = if accepted.asset.eq_ignore_ascii_case("hbar") {
            hedera::HBAR_ASSET_ID.to_owned()
        } else {
            accepted.asset.clone()
        };
        if accepted_asset != asset {
            return Err(BindingError::Accepted {
                field: "asset",
                accepted: accepted.asset.clone(),
                signed: asset,
            });
        }
        if accepted.pay_to != credit.0 {
            return Err(BindingError::Accepted {
                field: "payTo",
                accepted: accepted.pay_to.clone(),
                signed: credit.0,
            });
        }
        let fee_payer = seen.transaction_id.account_id.to_string();
        if let Some(declared) = accepted.fee_payer()
            && declared != fee_payer
        {
            return Err(BindingError::Accepted {
                field: "feePayer",
                accepted: declared.to_owned(),
                signed: fee_payer,
            });
        }
        let valid_start = seen.transaction_id.valid_start;
        let duration = seen
            .valid_duration
            .unwrap_or(time::Duration::seconds(hedera::MAX_VALID_SECONDS as i64));
        Ok(Self {
            payment_id: payment_id.to_owned(),
            tx_id: seen.transaction_id.to_string(),
            mirror_id: hedera::mirror_id(&seen.transaction_id),
            payer: debit.0,
            amount,
            asset,
            pay_to: credit.0,
            fee_payer,
            valid_start,
            valid_until: valid_start + duration,
            signature: signature.to_owned(),
            quote_json: serde_json::to_string(accepted)?,
        })
    }

    /// Replaces the stored quote with the full section 2.3 quote.
    pub fn with_quote(mut self, quote_json: String) -> Self {
        self.quote_json = quote_json;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authorization {
    pub id: i64,
    pub mandate_id: String,
    pub step: String,
    pub reservation_id: Option<i64>,
    pub quote_json: String,
    pub payment_id: String,
    pub tx_id: String,
    pub mirror_id: String,
    pub payer: String,
    pub amount: i64,
    pub asset: String,
    pub pay_to: String,
    pub fee_payer: String,
    pub valid_start: OffsetDateTime,
    pub valid_until: OffsetDateTime,
    pub signature: String,
    pub request: Request,
    pub submissions: u32,
    pub retrievals: u32,
    pub last_submission_at: Option<OffsetDateTime>,
    pub last_retrieval_at: Option<OffsetDateTime>,
    pub payment_state: PaymentState,
    pub delivery_state: DeliveryState,
    pub response_body: Option<Vec<u8>>,
    pub response_hash: Option<String>,
    pub payment_response: Option<String>,
    pub consensus_timestamp: Option<String>,
    pub duplicates_ignored: u32,
    pub failure_results: Option<String>,
    pub reject_reason: Option<String>,
}

impl Authorization {
    /// When the next submission may go out under the 30 s spacing.
    pub fn next_submission_at(&self) -> Option<OffsetDateTime> {
        self.last_submission_at.map(|t| t + RETRY_SPACING)
    }

    /// When the next retrieval may go out under the 30 s spacing.
    pub fn next_retrieval_at(&self) -> Option<OffsetDateTime> {
        self.last_retrieval_at.map(|t| t + RETRY_SPACING)
    }

    /// Section 6 recovery: not terminal, or settled without a validated delivery.
    pub fn needs_resume(&self) -> bool {
        !self.payment_state.is_terminal()
            || (self.payment_state == PaymentState::Settled
                && matches!(
                    self.delivery_state,
                    DeliveryState::None | DeliveryState::Received
                ))
    }
}

/// Service accounting, section 3. `total` is `budget.service.total`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Accounts {
    pub total: i64,
    pub settled: i64,
    pub outstanding: i64,
    pub held: i64,
}

impl Accounts {
    pub fn free(&self) -> i64 {
        self.total - self.settled - self.outstanding - self.held
    }
}

/// Audit accounting, I10. Reserved caps count until reconciled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditAccounts {
    pub total: i64,
    pub reserved: i64,
    pub charged: i64,
}

impl AuditAccounts {
    pub fn spent(&self) -> i64 {
        self.reserved + self.charged
    }

    pub fn free(&self) -> i64 {
        self.total - self.spent()
    }
}

/// What the ledger keeps about a mandate: the budget and the two constraints
/// it enforces by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MandateRow {
    pub id: String,
    pub mandate_hash: String,
    pub manifest_hash: String,
    pub service_total: i64,
    pub service_asset: String,
    pub audit_total: i64,
    pub max_single_payment: i64,
    pub deadline: OffsetDateTime,
}

/// One audit budget charge. `reserved` before the transaction id exists,
/// `submitted` once the id is known and until the mirror record decides,
/// `reconciled` at the charged fee, `overrun` when the record charged more
/// than the cap, `released` when nothing was ever sent or nothing was recorded
/// after the validity plus grace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditCharge {
    pub id: i64,
    pub mandate_id: String,
    pub purpose: String,
    pub cap: i64,
    pub charged: Option<i64>,
    pub tx_id: Option<String>,
    pub mirror_id: Option<String>,
    pub valid_until: Option<OffsetDateTime>,
    pub state: String,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS mandates (
  id TEXT PRIMARY KEY,
  mandate_hash TEXT NOT NULL,
  manifest_hash TEXT NOT NULL,
  service_total INTEGER NOT NULL,
  service_asset TEXT NOT NULL,
  audit_total INTEGER NOT NULL,
  max_single_payment INTEGER NOT NULL,
  deadline TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS reservations (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  mandate_id TEXT NOT NULL REFERENCES mandates(id),
  step TEXT NOT NULL,
  amount INTEGER NOT NULL CHECK (amount >= 0),
  source TEXT NOT NULL CHECK (source IN ('ceiling_at_max', 'quote')),
  state TEXT NOT NULL CHECK (state IN ('held', 'consumed', 'released')),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS authorizations (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  mandate_id TEXT NOT NULL REFERENCES mandates(id),
  step TEXT NOT NULL,
  reservation_id INTEGER REFERENCES reservations(id),
  quote_json TEXT NOT NULL,
  payment_id TEXT NOT NULL UNIQUE,
  tx_id TEXT NOT NULL UNIQUE,
  mirror_id TEXT NOT NULL,
  payer TEXT NOT NULL,
  amount INTEGER NOT NULL CHECK (amount > 0),
  asset TEXT NOT NULL,
  pay_to TEXT NOT NULL,
  fee_payer TEXT NOT NULL,
  valid_start TEXT NOT NULL,
  valid_until TEXT NOT NULL,
  signature TEXT NOT NULL,
  request_json TEXT NOT NULL,
    submissions INTEGER NOT NULL DEFAULT 0 CHECK (submissions BETWEEN 0 AND 3),
  retrievals INTEGER NOT NULL DEFAULT 0,
  last_submission_at TEXT,
  last_retrieval_at TEXT,
  payment_state TEXT NOT NULL CHECK (payment_state IN ('prepared', 'sent', 'settled', 'failed', 'unresolved')),
  delivery_state TEXT NOT NULL CHECK (delivery_state IN ('none', 'received', 'validated', 'rejected')),
  response_body BLOB,
  response_hash TEXT,
  payment_response TEXT,
  consensus_timestamp TEXT,
  duplicates_ignored INTEGER NOT NULL DEFAULT 0,
  failure_results TEXT,
  reject_reason TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS receipts (
  mandate_id TEXT NOT NULL REFERENCES mandates(id),
  seq INTEGER NOT NULL,
  json TEXT NOT NULL,
  hcs_sequence INTEGER,
  hcs_tx_id TEXT,
  published_at TEXT,
  PRIMARY KEY (mandate_id, seq)
);
CREATE TABLE IF NOT EXISTS audit_charges (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  mandate_id TEXT NOT NULL REFERENCES mandates(id),
  purpose TEXT NOT NULL,
    cap INTEGER NOT NULL CHECK (cap >= 0),
  charged INTEGER CHECK (charged IS NULL OR charged >= 0),
  tx_id TEXT,
  mirror_id TEXT,
  valid_until TEXT,
  state TEXT NOT NULL CHECK (state IN ('reserved', 'submitted', 'reconciled', 'overrun', 'released')),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
"#;

pub struct Ledger {
    conn: Connection,
}

fn rfc3339(t: OffsetDateTime) -> Result<String> {
    t.format(&Rfc3339)
        .map_err(|e| LedgerError::Time(e.to_string()))
}

fn parse_time(s: &str) -> Result<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339).map_err(|e| LedgerError::Time(format!("{s}: {e}")))
}

impl Ledger {
    /// Opens or creates the ledger file. WAL keeps committed transactions
    /// durable across a process kill.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        Self::init(conn)
    }

    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// Records a mandate. Idempotent for the same hash.
    pub fn insert_mandate(&mut self, m: &MandateRow, now: OffsetDateTime) -> Result<()> {
        let tx = self.conn.transaction()?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT mandate_hash FROM mandates WHERE id = ?1",
                [&m.id],
                |r| r.get(0),
            )
            .optional()?;
        match existing {
            Some(h) if h == m.mandate_hash => {}
            Some(_) => return Err(LedgerError::MandateHashDiffers(m.id.clone())),
            None => {
                tx.execute(
                    "INSERT INTO mandates (id, mandate_hash, manifest_hash, service_total, service_asset, audit_total, max_single_payment, deadline, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        m.id,
                        m.mandate_hash,
                        m.manifest_hash,
                        m.service_total,
                        m.service_asset,
                        m.audit_total,
                        m.max_single_payment,
                        rfc3339(m.deadline)?,
                        rfc3339(now)?
                    ],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn mandate(&self, id: &str) -> Result<MandateRow> {
        self.conn
            .query_row(
                "SELECT id, mandate_hash, manifest_hash, service_total, service_asset, audit_total, max_single_payment, deadline FROM mandates WHERE id = ?1",
                [id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, i64>(6)?,
                        r.get::<_, String>(7)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| LedgerError::MandateNotFound(id.to_owned()))
            .and_then(|(id, mandate_hash, manifest_hash, service_total, service_asset, audit_total, max_single_payment, deadline)| {
                Ok(MandateRow {
                    id,
                    mandate_hash,
                    manifest_hash,
                    service_total,
                    service_asset,
                    audit_total,
                    max_single_payment,
                    deadline: parse_time(&deadline)?,
                })
            })
    }

    pub fn accounts(&self, mandate_id: &str) -> Result<Accounts> {
        accounts_in(&self.conn, mandate_id)
    }

    pub fn audit_accounts(&self, mandate_id: &str) -> Result<AuditAccounts> {
        audit_in(&self.conn, mandate_id)
    }

    /// Holds `amount` for `step`. I1 is checked here.
    pub fn hold(
        &mut self,
        mandate_id: &str,
        step: &str,
        amount: i64,
        source: ReservationSource,
        now: OffsetDateTime,
    ) -> Result<Reservation> {
        let tx = self.conn.transaction()?;
        let accounts = accounts_in(&tx, mandate_id)?;
        if amount > accounts.free() {
            return Err(LedgerError::OverBudget {
                amount,
                free: accounts.free(),
            });
        }
        let at = rfc3339(now)?;
        tx.execute(
            "INSERT INTO reservations (mandate_id, step, amount, source, state, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, 'held', ?5, ?5)",
            params![mandate_id, step, amount, source.as_str(), at],
        )?;
        let id = tx.last_insert_rowid();
        let r = reservation_in(&tx, id)?;
        tx.commit()?;
        Ok(r)
    }

    /// Releases a held reservation. Anything else is unchanged.
    pub fn release(&mut self, reservation_id: i64, now: OffsetDateTime) -> Result<Reservation> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE reservations SET state = 'released', updated_at = ?2 WHERE id = ?1 AND state = 'held'",
            params![reservation_id, rfc3339(now)?],
        )?;
        let r = reservation_in(&tx, reservation_id)?;
        tx.commit()?;
        Ok(r)
    }

    pub fn reservation(&self, id: i64) -> Result<Reservation> {
        reservation_in(&self.conn, id)
    }

    /// Section 6 `approved -> prepared`: persists the signed payment and the
    /// request, consumes the reservation, moves `held -> outstanding`. I1, I2's
    /// amount cap and I12 are checked here.
    pub fn prepare(
        &mut self,
        mandate_id: &str,
        step: &str,
        reservation_id: Option<i64>,
        payment: &PreparedPayment,
        request: &Request,
        now: OffsetDateTime,
    ) -> Result<Authorization> {
        let tx = self.conn.transaction()?;
        let mandate = mandate_in(&tx, mandate_id)?;
        if now >= mandate.deadline {
            return Err(LedgerError::PastDeadline {
                deadline: rfc3339(mandate.deadline)?,
            });
        }
        if payment.amount > mandate.max_single_payment {
            return Err(LedgerError::AboveSinglePayment {
                amount: payment.amount,
                cap: mandate.max_single_payment,
            });
        }
        if payment.asset != mandate.service_asset {
            return Err(LedgerError::AssetMismatch {
                asset: payment.asset.clone(),
                service_asset: mandate.service_asset,
            });
        }
        let accounts = accounts_in(&tx, mandate_id)?;
        let mut freed = 0;
        if let Some(rid) = reservation_id {
            let r = reservation_in(&tx, rid)?;
            if r.state != ReservationState::Held || r.step != step || r.mandate_id != mandate_id {
                return Err(LedgerError::ReservationNotUsable {
                    id: rid,
                    state: r.state.as_str().to_owned(),
                    step: r.step,
                });
            }
            if r.amount < payment.amount {
                return Err(LedgerError::ReservationTooSmall {
                    id: rid,
                    held: r.amount,
                    amount: payment.amount,
                });
            }
            freed = r.amount;
        }
        if payment.amount > accounts.free() + freed {
            return Err(LedgerError::OverBudget {
                amount: payment.amount,
                free: accounts.free() + freed,
            });
        }
        let at = rfc3339(now)?;
        if let Some(rid) = reservation_id {
            tx.execute(
                "UPDATE reservations SET state = 'consumed', updated_at = ?2 WHERE id = ?1",
                params![rid, at],
            )?;
        }
        tx.execute(
            "INSERT INTO authorizations (mandate_id, step, reservation_id, quote_json, payment_id, tx_id, mirror_id, payer, amount, asset, pay_to, fee_payer, valid_start, valid_until, signature, request_json, payment_state, delivery_state, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?17, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, 'prepared', 'none', ?16, ?16)",
            params![
                mandate_id,
                step,
                reservation_id,
                payment.quote_json,
                payment.payment_id,
                payment.tx_id,
                payment.mirror_id,
                payment.amount,
                payment.asset,
                payment.pay_to,
                payment.fee_payer,
                rfc3339(payment.valid_start)?,
                rfc3339(payment.valid_until)?,
                payment.signature,
                serde_json::to_string(request)?,
                at,
                payment.payer
            ],
        )?;
        let id = tx.last_insert_rowid();
        let a = authorization_in(&tx, id)?;
        tx.commit()?;
        Ok(a)
    }

    /// Section 6 first send and resend: commits `submissions + 1` and returns
    /// the row whose `signature` and `request` are the only things to send.
    pub fn commit_submission(&mut self, id: i64, now: OffsetDateTime) -> Result<Authorization> {
        let tx = self.conn.transaction()?;
        let a = authorization_in(&tx, id)?;
        if !matches!(a.payment_state, PaymentState::Prepared | PaymentState::Sent)
            || a.delivery_state != DeliveryState::None
        {
            return Err(wrong(&a, "prepared or sent with no delivery"));
        }
        if now >= a.valid_until {
            return Err(LedgerError::Expired {
                id,
                valid_until: rfc3339(a.valid_until)?,
            });
        }
        if a.submissions >= MAX_SUBMISSIONS {
            return Err(LedgerError::SubmissionsExhausted { id });
        }
        if let Some(next) = a.next_submission_at()
            && now < next
        {
            return Err(LedgerError::TooSoon {
                id,
                what: "submission",
                next_at: rfc3339(next)?,
            });
        }
        tx.execute(
            "UPDATE authorizations SET submissions = submissions + 1, payment_state = 'sent', last_submission_at = ?2, updated_at = ?2 WHERE id = ?1",
            params![id, rfc3339(now)?],
        )?;
        let a = authorization_in(&tx, id)?;
        tx.commit()?;
        Ok(a)
    }

    /// Section 6 retrieve: commits `retrievals + 1` for a settled purchase
    /// without a delivery, before the deadline.
    pub fn commit_retrieval(&mut self, id: i64, now: OffsetDateTime) -> Result<Authorization> {
        let tx = self.conn.transaction()?;
        let a = authorization_in(&tx, id)?;
        if a.payment_state != PaymentState::Settled || a.delivery_state != DeliveryState::None {
            return Err(wrong(&a, "settled with no delivery"));
        }
        let mandate = mandate_in(&tx, &a.mandate_id)?;
        if now >= mandate.deadline {
            return Err(LedgerError::PastDeadline {
                deadline: rfc3339(mandate.deadline)?,
            });
        }
        if let Some(next) = a.next_retrieval_at()
            && now < next
        {
            return Err(LedgerError::TooSoon {
                id,
                what: "retrieval",
                next_at: rfc3339(next)?,
            });
        }
        tx.execute(
            "UPDATE authorizations SET retrievals = retrievals + 1, last_retrieval_at = ?2, updated_at = ?2 WHERE id = ?1",
            params![id, rfc3339(now)?],
        )?;
        let a = authorization_in(&tx, id)?;
        tx.commit()?;
        Ok(a)
    }

    /// Section 6 `none -> received`. Recording the same body again changes
    /// nothing; a different body is an error, never an overwrite.
    pub fn record_delivery(
        &mut self,
        id: i64,
        body: &[u8],
        payment_response: Option<&str>,
        now: OffsetDateTime,
    ) -> Result<Authorization> {
        let tx = self.conn.transaction()?;
        let a = authorization_in(&tx, id)?;
        let hash = sha256_hex(body);
        match a.delivery_state {
            DeliveryState::None => {
                tx.execute(
                    "UPDATE authorizations SET delivery_state = 'received', response_body = ?2, response_hash = ?3, payment_response = ?4, updated_at = ?5 WHERE id = ?1",
                    params![id, body, hash, payment_response, rfc3339(now)?],
                )?;
            }
            _ if a.response_hash.as_deref() == Some(&hash) => {}
            _ => return Err(LedgerError::ResponseDiffers { id }),
        }
        let a = authorization_in(&tx, id)?;
        tx.commit()?;
        Ok(a)
    }

    /// I5 applied to a row. Terminal rows never change; a settled observation
    /// counted once stays counted once. `Absent` becomes `unresolved` only
    /// after `valid_until + 30 s`, and `unresolved` keeps its exposure.
    pub fn record_settlement(
        &mut self,
        id: i64,
        settlement: &Settlement,
        now: OffsetDateTime,
    ) -> Result<Authorization> {
        let tx = self.conn.transaction()?;
        let a = authorization_in(&tx, id)?;
        if !a.payment_state.is_terminal() {
            let at = rfc3339(now)?;
            match settlement {
                Settlement::Settled {
                    consensus_timestamp,
                    duplicates_ignored,
                } => {
                    tx.execute(
                        "UPDATE authorizations SET payment_state = 'settled', consensus_timestamp = ?2, duplicates_ignored = ?3, updated_at = ?4 WHERE id = ?1",
                        params![id, consensus_timestamp, *duplicates_ignored as i64, at],
                    )?;
                }
                Settlement::Failed {
                    results,
                    duplicates_ignored,
                } => {
                    tx.execute(
                        "UPDATE authorizations SET payment_state = 'failed', failure_results = ?2, duplicates_ignored = ?3, updated_at = ?4 WHERE id = ?1",
                        params![id, results.join(","), *duplicates_ignored as i64, at],
                    )?;
                }
                Settlement::Anomaly {
                    results,
                    duplicates_ignored,
                } => {
                    tx.execute(
                        "UPDATE authorizations SET payment_state = 'unresolved', failure_results = ?2, duplicates_ignored = ?3, updated_at = ?4 WHERE id = ?1",
                        params![id, format!("anomaly:{}", results.join(",")), *duplicates_ignored as i64, at],
                    )?;
                }
                Settlement::Absent { duplicates_ignored } => {
                    if now > a.valid_until + RECORD_GRACE
                        && a.payment_state != PaymentState::Unresolved
                    {
                        tx.execute(
                            "UPDATE authorizations SET payment_state = 'unresolved', duplicates_ignored = ?2, updated_at = ?3 WHERE id = ?1",
                            params![id, *duplicates_ignored as i64, at],
                        )?;
                    }
                }
            }
        }
        let a = authorization_in(&tx, id)?;
        tx.commit()?;
        Ok(a)
    }

    /// Section 6 `received -> validated`.
    pub fn mark_validated(&mut self, id: i64, now: OffsetDateTime) -> Result<Authorization> {
        self.mark_delivery(id, "validated", None, now)
    }

    /// Section 6 `received -> rejected` with a section 2.7 validation reason.
    pub fn mark_rejected(
        &mut self,
        id: i64,
        reason: &str,
        now: OffsetDateTime,
    ) -> Result<Authorization> {
        self.mark_delivery(id, "rejected", Some(reason), now)
    }

    fn mark_delivery(
        &mut self,
        id: i64,
        to: &str,
        reason: Option<&str>,
        now: OffsetDateTime,
    ) -> Result<Authorization> {
        let tx = self.conn.transaction()?;
        let a = authorization_in(&tx, id)?;
        if a.payment_state != PaymentState::Settled || a.delivery_state != DeliveryState::Received {
            return Err(wrong(&a, "settled and received"));
        }
        tx.execute(
            "UPDATE authorizations SET delivery_state = ?2, reject_reason = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, to, reason, rfc3339(now)?],
        )?;
        let a = authorization_in(&tx, id)?;
        tx.commit()?;
        Ok(a)
    }

    pub fn authorization(&self, id: i64) -> Result<Authorization> {
        authorization_in(&self.conn, id)
    }

    pub fn authorizations(&self, mandate_id: &str) -> Result<Vec<Authorization>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {AUTH_COLUMNS} FROM authorizations WHERE mandate_id = ?1 ORDER BY id"
        ))?;
        let rows = stmt.query_map([mandate_id], authorization_from_row)?;
        rows.map(|r| r.map_err(LedgerError::from).and_then(|x| x))
            .collect()
    }

    /// The rows section 6 recovery must resume.
    pub fn resumable(&self, mandate_id: &str) -> Result<Vec<Authorization>> {
        Ok(self
            .authorizations(mandate_id)?
            .into_iter()
            .filter(Authorization::needs_resume)
            .collect())
    }

    /// Appends a receipt with the next `seq`, durable before publication (I9).
    pub fn append_receipt(&mut self, mandate_id: &str, receipt: &Receipt) -> Result<Receipt> {
        let tx = self.conn.transaction()?;
        mandate_in(&tx, mandate_id)?;
        let next: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq) + 1, 0) FROM receipts WHERE mandate_id = ?1",
            [mandate_id],
            |r| r.get(0),
        )?;
        let mut stored = receipt.clone();
        stored.seq = next as u64;
        stored.mandate_id = mandate_id.to_owned();
        let message = stored
            .message()
            .map_err(|e| LedgerError::Time(e.to_string()))?;
        tx.execute(
            "INSERT INTO receipts (mandate_id, seq, json) VALUES (?1, ?2, ?3)",
            params![
                mandate_id,
                next,
                String::from_utf8_lossy(&message).into_owned()
            ],
        )?;
        tx.commit()?;
        Ok(stored)
    }

    pub fn receipts(&self, mandate_id: &str) -> Result<Vec<(Receipt, Option<u64>)>> {
        let mut stmt = self.conn.prepare(
            "SELECT json, hcs_sequence FROM receipts WHERE mandate_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([mandate_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?))
        })?;
        rows.map(|r| {
            let (json, seq) = r?;
            Ok((serde_json::from_str(&json)?, seq.map(|s| s as u64)))
        })
        .collect()
    }

    /// Sequence numbers not yet on HCS, for `audit_pending`.
    pub fn unpublished(&self, mandate_id: &str) -> Result<Vec<u64>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq FROM receipts WHERE mandate_id = ?1 AND hcs_sequence IS NULL ORDER BY seq",
        )?;
        let rows = stmt.query_map([mandate_id], |r| r.get::<_, i64>(0))?;
        rows.map(|r| r.map(|s| s as u64).map_err(LedgerError::from))
            .collect()
    }

    pub fn mark_published(
        &mut self,
        mandate_id: &str,
        seq: u64,
        hcs_sequence: u64,
        hcs_tx_id: &str,
        now: OffsetDateTime,
    ) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE receipts SET hcs_sequence = ?3, hcs_tx_id = ?4, published_at = ?5 WHERE mandate_id = ?1 AND seq = ?2 AND hcs_sequence IS NULL",
            params![mandate_id, seq as i64, hcs_sequence as i64, hcs_tx_id, rfc3339(now)?],
        )?;
        if n == 0 {
            return Err(LedgerError::NotFound {
                what: "unpublished receipt",
                id: seq as i64,
            });
        }
        Ok(())
    }

    /// Reserves an audit fee cap before an HCS submit. I10 is checked here.
    pub fn reserve_audit(
        &mut self,
        mandate_id: &str,
        purpose: &str,
        cap: i64,
        now: OffsetDateTime,
    ) -> Result<AuditCharge> {
        let tx = self.conn.transaction()?;
        let audit = audit_in(&tx, mandate_id)?;
        if cap > audit.free() {
            return Err(LedgerError::AuditOverBudget {
                cap,
                free: audit.free(),
            });
        }
        let at = rfc3339(now)?;
        tx.execute(
            "INSERT INTO audit_charges (mandate_id, purpose, cap, state, created_at, updated_at) VALUES (?1, ?2, ?3, 'reserved', ?4, ?4)",
            params![mandate_id, purpose, cap, at],
        )?;
        let id = tx.last_insert_rowid();
        let c = audit_charge_in(&tx, id)?;
        tx.commit()?;
        Ok(c)
    }

    /// The transaction id is known and about to be sent. From here the charge
    /// can only be reconciled from the mirror node, or released once the
    /// validity plus grace has passed with no record.
    pub fn audit_submitted(
        &mut self,
        id: i64,
        tx_id: &str,
        mirror_id: &str,
        valid_until: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<AuditCharge> {
        let tx = self.conn.transaction()?;
        let c = audit_charge_in(&tx, id)?;
        if c.state != "reserved" {
            return Err(LedgerError::AuditWrongState {
                id,
                state: c.state,
                wanted: "reserved",
            });
        }
        tx.execute(
            "UPDATE audit_charges SET state = 'submitted', tx_id = ?2, mirror_id = ?3, valid_until = ?4, updated_at = ?5 WHERE id = ?1",
            params![id, tx_id, mirror_id, rfc3339(valid_until)?, rfc3339(now)?],
        )?;
        let c = audit_charge_in(&tx, id)?;
        tx.commit()?;
        Ok(c)
    }

    /// The charged fee from the mirror node record replaces the cap. A charge
    /// above the cap is recorded as `overrun` rather than hidden; a negative
    /// charge is refused.
    pub fn audit_reconciled(
        &mut self,
        id: i64,
        charged: i64,
        now: OffsetDateTime,
    ) -> Result<AuditCharge> {
        if charged < 0 {
            return Err(LedgerError::AuditChargeNegative { id });
        }
        let tx = self.conn.transaction()?;
        let c = audit_charge_in(&tx, id)?;
        if c.state != "submitted" {
            return Err(LedgerError::AuditWrongState {
                id,
                state: c.state,
                wanted: "submitted",
            });
        }
        let to = if charged > c.cap {
            "overrun"
        } else {
            "reconciled"
        };
        tx.execute(
            "UPDATE audit_charges SET state = ?2, charged = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, to, charged, rfc3339(now)?],
        )?;
        let c = audit_charge_in(&tx, id)?;
        tx.commit()?;
        Ok(c)
    }

    /// No record after the validity plus grace: the network never charged.
    pub fn audit_not_recorded(&mut self, id: i64, now: OffsetDateTime) -> Result<AuditCharge> {
        let tx = self.conn.transaction()?;
        let c = audit_charge_in(&tx, id)?;
        if c.state != "submitted" {
            return Err(LedgerError::AuditWrongState {
                id,
                state: c.state,
                wanted: "submitted",
            });
        }
        let valid_until = c.valid_until.ok_or(LedgerError::NotFound {
            what: "audit validity",
            id,
        })?;
        if now <= valid_until + RECORD_GRACE {
            return Err(LedgerError::AuditStillValid {
                id,
                valid_until: rfc3339(valid_until)?,
            });
        }
        tx.execute(
            "UPDATE audit_charges SET state = 'released', updated_at = ?2 WHERE id = ?1",
            params![id, rfc3339(now)?],
        )?;
        let c = audit_charge_in(&tx, id)?;
        tx.commit()?;
        Ok(c)
    }

    /// Nothing was ever sent; the cap goes back to the budget. Only from `reserved`.
    pub fn release_audit(&mut self, id: i64, now: OffsetDateTime) -> Result<AuditCharge> {
        let tx = self.conn.transaction()?;
        let c = audit_charge_in(&tx, id)?;
        if c.state != "reserved" {
            return Err(LedgerError::AuditWrongState {
                id,
                state: c.state,
                wanted: "reserved",
            });
        }
        tx.execute(
            "UPDATE audit_charges SET state = 'released', updated_at = ?2 WHERE id = ?1",
            params![id, rfc3339(now)?],
        )?;
        let c = audit_charge_in(&tx, id)?;
        tx.commit()?;
        Ok(c)
    }

    /// Charges whose transaction is known and undecided, for reconciliation.
    pub fn audit_submitted_charges(&self, mandate_id: &str) -> Result<Vec<AuditCharge>> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM audit_charges WHERE mandate_id = ?1 AND state = 'submitted' ORDER BY id",
        )?;
        let ids: Vec<i64> = stmt
            .query_map([mandate_id], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        ids.into_iter()
            .map(|id| audit_charge_in(&self.conn, id))
            .collect()
    }
}

fn wrong(a: &Authorization, wanted: &'static str) -> LedgerError {
    LedgerError::WrongState {
        id: a.id,
        payment: a.payment_state.as_str().to_owned(),
        delivery: a.delivery_state.as_str().to_owned(),
        wanted,
    }
}

fn mandate_in(conn: &Connection, id: &str) -> Result<MandateRow> {
    let row = conn
        .query_row(
            "SELECT id, mandate_hash, manifest_hash, service_total, service_asset, audit_total, max_single_payment, deadline FROM mandates WHERE id = ?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, String>(7)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| LedgerError::MandateNotFound(id.to_owned()))?;
    Ok(MandateRow {
        id: row.0,
        mandate_hash: row.1,
        manifest_hash: row.2,
        service_total: row.3,
        service_asset: row.4,
        audit_total: row.5,
        max_single_payment: row.6,
        deadline: parse_time(&row.7)?,
    })
}

fn accounts_in(conn: &Connection, mandate_id: &str) -> Result<Accounts> {
    let mandate = mandate_in(conn, mandate_id)?;
    let held: i64 = conn.query_row(
        "SELECT COALESCE(SUM(amount), 0) FROM reservations WHERE mandate_id = ?1 AND state = 'held'",
        [mandate_id],
        |r| r.get(0),
    )?;
    let outstanding: i64 = conn.query_row(
        "SELECT COALESCE(SUM(amount), 0) FROM authorizations WHERE mandate_id = ?1 AND payment_state IN ('prepared', 'sent', 'unresolved')",
        [mandate_id],
        |r| r.get(0),
    )?;
    let settled: i64 = conn.query_row(
        "SELECT COALESCE(SUM(amount), 0) FROM authorizations WHERE mandate_id = ?1 AND payment_state = 'settled'",
        [mandate_id],
        |r| r.get(0),
    )?;
    Ok(Accounts {
        total: mandate.service_total,
        settled,
        outstanding,
        held,
    })
}

fn audit_in(conn: &Connection, mandate_id: &str) -> Result<AuditAccounts> {
    let mandate = mandate_in(conn, mandate_id)?;
    let reserved: i64 = conn.query_row(
        "SELECT COALESCE(SUM(cap), 0) FROM audit_charges WHERE mandate_id = ?1 AND state IN ('reserved', 'submitted')",
        [mandate_id],
        |r| r.get(0),
    )?;
    let charged: i64 = conn.query_row(
        "SELECT COALESCE(SUM(charged), 0) FROM audit_charges WHERE mandate_id = ?1 AND state IN ('reconciled', 'overrun')",
        [mandate_id],
        |r| r.get(0),
    )?;
    Ok(AuditAccounts {
        total: mandate.audit_total,
        reserved,
        charged,
    })
}

fn reservation_in(conn: &Connection, id: i64) -> Result<Reservation> {
    conn.query_row(
        "SELECT id, mandate_id, step, amount, source, state FROM reservations WHERE id = ?1",
        [id],
        |r| {
            Ok(Reservation {
                id: r.get(0)?,
                mandate_id: r.get(1)?,
                step: r.get(2)?,
                amount: r.get(3)?,
                source: if r.get::<_, String>(4)? == "quote" {
                    ReservationSource::Quote
                } else {
                    ReservationSource::CeilingAtMax
                },
                state: ReservationState::parse(&r.get::<_, String>(5)?),
            })
        },
    )
    .optional()?
    .ok_or(LedgerError::NotFound {
        what: "reservation",
        id,
    })
}

fn audit_charge_in(conn: &Connection, id: i64) -> Result<AuditCharge> {
    let row = conn
        .query_row(
            "SELECT id, mandate_id, purpose, cap, charged, tx_id, mirror_id, valid_until, state FROM audit_charges WHERE id = ?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, Option<String>>(7)?,
                    r.get::<_, String>(8)?,
                ))
            },
        )
        .optional()?
        .ok_or(LedgerError::NotFound {
            what: "audit charge",
            id,
        })?;
    Ok(AuditCharge {
        id: row.0,
        mandate_id: row.1,
        purpose: row.2,
        cap: row.3,
        charged: row.4,
        tx_id: row.5,
        mirror_id: row.6,
        valid_until: row.7.as_deref().map(parse_time).transpose()?,
        state: row.8,
    })
}

const AUTH_COLUMNS: &str = "id, mandate_id, step, reservation_id, quote_json, payment_id, tx_id, mirror_id, amount, asset, pay_to, fee_payer, valid_start, valid_until, signature, request_json, submissions, retrievals, payment_state, delivery_state, response_body, response_hash, payment_response, consensus_timestamp, duplicates_ignored, failure_results, reject_reason, payer, last_submission_at, last_retrieval_at";

fn authorization_from_row(r: &Row<'_>) -> rusqlite::Result<Result<Authorization>> {
    let valid_start: String = r.get(12)?;
    let valid_until: String = r.get(13)?;
    let request_json: String = r.get(15)?;
    let payment_state: String = r.get(18)?;
    let delivery_state: String = r.get(19)?;
    let last_submission_at: Option<String> = r.get(28)?;
    let last_retrieval_at: Option<String> = r.get(29)?;
    let built = (|| -> Result<Authorization> {
        Ok(Authorization {
            id: r.get(0)?,
            mandate_id: r.get(1)?,
            step: r.get(2)?,
            reservation_id: r.get(3)?,
            quote_json: r.get(4)?,
            payment_id: r.get(5)?,
            tx_id: r.get(6)?,
            mirror_id: r.get(7)?,
            amount: r.get(8)?,
            asset: r.get(9)?,
            pay_to: r.get(10)?,
            fee_payer: r.get(11)?,
            valid_start: parse_time(&valid_start)?,
            valid_until: parse_time(&valid_until)?,
            signature: r.get(14)?,
            request: serde_json::from_str(&request_json)?,
            submissions: r.get::<_, i64>(16)? as u32,
            retrievals: r.get::<_, i64>(17)? as u32,
            last_submission_at: last_submission_at.as_deref().map(parse_time).transpose()?,
            last_retrieval_at: last_retrieval_at.as_deref().map(parse_time).transpose()?,
            payment_state: PaymentState::parse(&payment_state),
            delivery_state: DeliveryState::parse(&delivery_state),
            response_body: r.get(20)?,
            response_hash: r.get(21)?,
            payment_response: r.get(22)?,
            consensus_timestamp: r.get(23)?,
            duplicates_ignored: r.get::<_, i64>(24)? as u32,
            failure_results: r.get(25)?,
            reject_reason: r.get(26)?,
            payer: r.get(27)?,
        })
    })();
    Ok(built)
}

fn authorization_in(conn: &Connection, id: i64) -> Result<Authorization> {
    conn.query_row(
        &format!("SELECT {AUTH_COLUMNS} FROM authorizations WHERE id = ?1"),
        [id],
        authorization_from_row,
    )
    .optional()?
    .ok_or(LedgerError::NotFound {
        what: "authorization",
        id,
    })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{mandate_row, request, signed_payment, signed_payment_for};
    use time::Duration;
    use time::macros::datetime;

    const NOW: OffsetDateTime = datetime!(2026-09-08 09:00 UTC);
    const USDC: &str = "0.0.429274";

    fn ledger() -> Ledger {
        let mut l = Ledger::in_memory().unwrap();
        l.insert_mandate(&mandate_row(), NOW).unwrap();
        l
    }

    fn payment(n: u32, amount: i64) -> PreparedPayment {
        signed_payment(n, amount, USDC, NOW)
    }

    fn settled() -> Settlement {
        Settlement::Settled {
            consensus_timestamp: "1788800010.000000001".to_owned(),
            duplicates_ignored: 1,
        }
    }

    #[test]
    fn a_prepared_payment_reads_everything_from_the_signed_bytes() {
        let p = payment(7, 1_234);
        assert_eq!(p.amount, 1_234);
        assert_eq!(p.asset, USDC);
        assert_eq!(p.payer, "0.0.10399984");
        assert_eq!(p.pay_to, "0.0.10409989");
        assert_eq!(p.fee_payer, "0.0.7162784");
        assert!(p.tx_id.starts_with("0.0.7162784@"));
        assert!(p.mirror_id.starts_with("0.0.7162784-"));
        assert_eq!(p.valid_until - p.valid_start, Duration::seconds(120));
        assert!(p.quote_json.contains("\"payTo\":\"0.0.10409989\""));
    }

    #[test]
    fn a_header_whose_terms_disagree_with_the_bytes_is_refused() {
        let forged = signed_payment_for(1, 16_000, USDC, NOW, |accepted| {
            accepted.amount = "1000".to_owned()
        });
        assert!(matches!(
            forged,
            Err(BindingError::Accepted {
                field: "amount",
                ..
            })
        ));
        let wrong_recipient = signed_payment_for(2, 1_000, USDC, NOW, |accepted| {
            accepted.pay_to = "0.0.5".to_owned()
        });
        assert!(matches!(
            wrong_recipient,
            Err(BindingError::Accepted { field: "payTo", .. })
        ));
        let wrong_asset = signed_payment_for(3, 1_000, USDC, NOW, |accepted| {
            accepted.asset = "0.0.0".to_owned()
        });
        assert!(matches!(
            wrong_asset,
            Err(BindingError::Accepted { field: "asset", .. })
        ));
        let good = payment(4, 1_000);
        assert!(matches!(
            PreparedPayment::from_signature(&good.signature, "pay_other_id_0000000000000000000"),
            Err(BindingError::PaymentId { .. })
        ));
        assert!(matches!(
            PreparedPayment::from_signature("bm90IGpzb24=", "pay_x"),
            Err(BindingError::Header(_))
        ));
    }

    #[test]
    fn the_payment_asset_must_be_the_service_asset() {
        let mut l = ledger();
        let hbar = signed_payment(1, 1_000, "0.0.0", NOW);
        assert!(matches!(
            l.prepare("m1", "screen", None, &hbar, &request(), NOW),
            Err(LedgerError::AssetMismatch { .. })
        ));
        assert_eq!(l.accounts("m1").unwrap().outstanding, 0);
    }

    #[test]
    fn hold_prepare_settle_moves_amounts_between_columns() {
        let mut l = ledger();
        let r = l
            .hold("m1", "explain", 800, ReservationSource::CeilingAtMax, NOW)
            .unwrap();
        assert_eq!(
            l.accounts("m1").unwrap(),
            Accounts {
                total: 10_000,
                settled: 0,
                outstanding: 0,
                held: 800
            }
        );
        let q = l
            .hold("m1", "events", 1_500, ReservationSource::Quote, NOW)
            .unwrap();
        let a = l
            .prepare(
                "m1",
                "events",
                Some(q.id),
                &payment(1, 1_400),
                &request(),
                NOW,
            )
            .unwrap();
        assert_eq!(a.payment_state, PaymentState::Prepared);
        assert_eq!(a.submissions, 0);
        assert_eq!(
            l.reservation(q.id).unwrap().state,
            ReservationState::Consumed
        );
        assert_eq!(
            l.accounts("m1").unwrap(),
            Accounts {
                total: 10_000,
                settled: 0,
                outstanding: 1_400,
                held: 800
            }
        );
        l.record_settlement(a.id, &settled(), NOW).unwrap();
        assert_eq!(
            l.accounts("m1").unwrap(),
            Accounts {
                total: 10_000,
                settled: 1_400,
                outstanding: 0,
                held: 800
            }
        );
        l.release(r.id, NOW).unwrap();
        assert_eq!(l.accounts("m1").unwrap().held, 0);
        assert_eq!(l.accounts("m1").unwrap().free(), 8_600);
    }

    #[test]
    fn i1_refuses_what_does_not_fit_by_the_signed_amount() {
        let mut l = ledger();
        let r = l
            .hold("m1", "explain", 9_000, ReservationSource::CeilingAtMax, NOW)
            .unwrap();
        assert!(matches!(
            l.hold("m1", "events", 1_001, ReservationSource::Quote, NOW),
            Err(LedgerError::OverBudget {
                amount: 1_001,
                free: 1_000
            })
        ));
        assert!(matches!(
            l.prepare("m1", "events", None, &payment(1, 1_001), &request(), NOW),
            Err(LedgerError::OverBudget { .. })
        ));
        l.prepare("m1", "events", None, &payment(2, 1_000), &request(), NOW)
            .unwrap();
        assert_eq!(l.accounts("m1").unwrap().free(), 0);
        l.release(r.id, NOW).unwrap();
        assert!(matches!(
            l.prepare("m1", "events", None, &payment(3, 9_001), &request(), NOW),
            Err(LedgerError::AboveSinglePayment {
                amount: 9_001,
                cap: 9_000
            })
        ));
        l.prepare("m1", "a", None, &payment(4, 8_000), &request(), NOW)
            .unwrap();
        assert!(matches!(
            l.prepare("m1", "b", None, &payment(5, 8_000), &request(), NOW),
            Err(LedgerError::OverBudget {
                amount: 8_000,
                free: 1_000
            })
        ));
        assert_eq!(l.accounts("m1").unwrap().outstanding, 9_000);
    }

    #[test]
    fn i12_deadline_is_enforced_at_prepare() {
        let mut l = ledger();
        assert!(matches!(
            l.prepare(
                "m1",
                "screen",
                None,
                &payment(2, 100),
                &request(),
                datetime!(2026-09-13 16:00 UTC)
            ),
            Err(LedgerError::PastDeadline { .. })
        ));
    }

    #[test]
    fn a_reservation_serves_only_its_step_and_covers_the_amount() {
        let mut l = ledger();
        let r = l
            .hold("m1", "events", 1_000, ReservationSource::Quote, NOW)
            .unwrap();
        assert!(matches!(
            l.prepare(
                "m1",
                "explain",
                Some(r.id),
                &payment(1, 500),
                &request(),
                NOW
            ),
            Err(LedgerError::ReservationNotUsable { .. })
        ));
        assert!(matches!(
            l.prepare(
                "m1",
                "events",
                Some(r.id),
                &payment(1, 1_001),
                &request(),
                NOW
            ),
            Err(LedgerError::ReservationTooSmall {
                held: 1_000,
                amount: 1_001,
                ..
            })
        ));
        l.prepare(
            "m1",
            "events",
            Some(r.id),
            &payment(1, 900),
            &request(),
            NOW,
        )
        .unwrap();
        assert!(matches!(
            l.prepare(
                "m1",
                "events",
                Some(r.id),
                &payment(2, 100),
                &request(),
                NOW
            ),
            Err(LedgerError::ReservationNotUsable { .. })
        ));
    }

    #[test]
    fn i8_counters_commit_first_cap_at_three_and_keep_30_seconds_apart() {
        let mut l = ledger();
        let a = l
            .prepare("m1", "screen", None, &payment(1, 1_000), &request(), NOW)
            .unwrap();
        let s1 = l.commit_submission(a.id, NOW).unwrap();
        assert_eq!((s1.submissions, s1.payment_state), (1, PaymentState::Sent));
        assert_eq!(s1.signature, payment(1, 1_000).signature);
        assert_eq!(s1.request, request());
        assert_eq!(s1.next_submission_at(), Some(NOW + Duration::seconds(30)));
        assert!(matches!(
            l.commit_submission(a.id, NOW + Duration::seconds(29)),
            Err(LedgerError::TooSoon {
                what: "submission",
                ..
            })
        ));
        l.commit_submission(a.id, NOW + Duration::seconds(30))
            .unwrap();
        let s3 = l
            .commit_submission(a.id, NOW + Duration::seconds(60))
            .unwrap();
        assert_eq!(s3.submissions, 3);
        assert!(matches!(
            l.commit_submission(a.id, NOW + Duration::seconds(90)),
            Err(LedgerError::SubmissionsExhausted { .. })
        ));
        let late = l
            .prepare("m1", "events", None, &payment(2, 1_000), &request(), NOW)
            .unwrap();
        assert!(matches!(
            l.commit_submission(late.id, NOW + Duration::seconds(200)),
            Err(LedgerError::Expired { .. })
        ));
    }

    #[test]
    fn retrievals_need_settlement_the_deadline_and_spacing() {
        let mut l = ledger();
        let a = l
            .prepare("m1", "screen", None, &payment(1, 1_000), &request(), NOW)
            .unwrap();
        l.commit_submission(a.id, NOW).unwrap();
        assert!(matches!(
            l.commit_retrieval(a.id, NOW),
            Err(LedgerError::WrongState { .. })
        ));
        l.record_settlement(a.id, &settled(), NOW).unwrap();
        let r = l.commit_retrieval(a.id, NOW).unwrap();
        assert_eq!(r.retrievals, 1);
        assert!(matches!(
            l.commit_retrieval(a.id, NOW + Duration::seconds(10)),
            Err(LedgerError::TooSoon {
                what: "retrieval",
                ..
            })
        ));
        let r2 = l
            .commit_retrieval(a.id, NOW + Duration::seconds(30))
            .unwrap();
        assert_eq!(r2.retrievals, 2);
        assert!(matches!(
            l.commit_retrieval(a.id, datetime!(2026-09-14 00:00 UTC)),
            Err(LedgerError::PastDeadline { .. })
        ));
        l.record_delivery(a.id, b"{}", Some("resp"), NOW).unwrap();
        assert!(matches!(
            l.commit_retrieval(a.id, NOW + Duration::seconds(60)),
            Err(LedgerError::WrongState { .. })
        ));
    }

    #[test]
    fn settlement_is_observed_once_and_duplicates_alone_keep_exposure() {
        let mut l = ledger();
        let a = l
            .prepare("m1", "screen", None, &payment(1, 1_000), &request(), NOW)
            .unwrap();
        l.commit_submission(a.id, NOW).unwrap();
        let absent = Settlement::Absent {
            duplicates_ignored: 1,
        };
        let still = l
            .record_settlement(a.id, &absent, NOW + Duration::seconds(60))
            .unwrap();
        assert_eq!(still.payment_state, PaymentState::Sent);
        assert_eq!(l.accounts("m1").unwrap().outstanding, 1_000);
        let late = l
            .record_settlement(a.id, &absent, NOW + Duration::seconds(200))
            .unwrap();
        assert_eq!(late.payment_state, PaymentState::Unresolved);
        assert_eq!(
            l.accounts("m1").unwrap().outstanding,
            1_000,
            "unresolved keeps the exposure"
        );
        let s = l
            .record_settlement(a.id, &settled(), NOW + Duration::seconds(300))
            .unwrap();
        assert_eq!(s.payment_state, PaymentState::Settled);
        assert_eq!(s.duplicates_ignored, 1);
        let again = l
            .record_settlement(a.id, &settled(), NOW + Duration::seconds(400))
            .unwrap();
        assert_eq!(again, s, "a second observation changes nothing");
        assert_eq!(
            l.accounts("m1").unwrap(),
            Accounts {
                total: 10_000,
                settled: 1_000,
                outstanding: 0,
                held: 0
            }
        );
        let failed_after = l
            .record_settlement(
                a.id,
                &Settlement::Failed {
                    results: vec!["X".to_owned()],
                    duplicates_ignored: 0,
                },
                NOW,
            )
            .unwrap();
        assert_eq!(
            failed_after.payment_state,
            PaymentState::Settled,
            "terminal rows never move"
        );
    }

    #[test]
    fn a_non_duplicate_failure_releases_and_an_anomaly_holds() {
        let mut l = ledger();
        let a = l
            .prepare("m1", "screen", None, &payment(1, 1_000), &request(), NOW)
            .unwrap();
        let f = l
            .record_settlement(
                a.id,
                &Settlement::Failed {
                    results: vec!["INSUFFICIENT_TOKEN_BALANCE".to_owned()],
                    duplicates_ignored: 2,
                },
                NOW,
            )
            .unwrap();
        assert_eq!(f.payment_state, PaymentState::Failed);
        assert_eq!(
            f.failure_results.as_deref(),
            Some("INSUFFICIENT_TOKEN_BALANCE")
        );
        assert_eq!(l.accounts("m1").unwrap().free(), 10_000);

        let b = l
            .prepare("m1", "events", None, &payment(2, 1_000), &request(), NOW)
            .unwrap();
        let an = l
            .record_settlement(
                b.id,
                &Settlement::Anomaly {
                    results: vec!["SUCCESS".to_owned()],
                    duplicates_ignored: 0,
                },
                NOW,
            )
            .unwrap();
        assert_eq!(an.payment_state, PaymentState::Unresolved);
        assert!(an.failure_results.unwrap().starts_with("anomaly:"));
        assert_eq!(l.accounts("m1").unwrap().outstanding, 1_000);
    }

    #[test]
    fn delivery_is_recorded_once_and_validated_after_settlement() {
        let mut l = ledger();
        let a = l
            .prepare("m1", "screen", None, &payment(1, 1_000), &request(), NOW)
            .unwrap();
        l.commit_submission(a.id, NOW).unwrap();
        let d = l
            .record_delivery(a.id, b"{\"ok\":true}", Some("pr"), NOW)
            .unwrap();
        assert_eq!(d.delivery_state, DeliveryState::Received);
        assert_eq!(
            d.response_hash.as_deref(),
            Some(sha256_hex(b"{\"ok\":true}").as_str())
        );
        assert!(
            matches!(
                l.mark_validated(a.id, NOW),
                Err(LedgerError::WrongState { .. })
            ),
            "not settled yet"
        );
        l.record_delivery(a.id, b"{\"ok\":true}", Some("pr"), NOW)
            .unwrap();
        assert!(matches!(
            l.record_delivery(a.id, b"other", None, NOW),
            Err(LedgerError::ResponseDiffers { .. })
        ));
        l.record_settlement(a.id, &settled(), NOW).unwrap();
        let v = l.mark_validated(a.id, NOW).unwrap();
        assert_eq!(v.delivery_state, DeliveryState::Validated);
        assert!(!v.needs_resume());
        let b = l
            .prepare("m1", "events", None, &payment(2, 1_000), &request(), NOW)
            .unwrap();
        l.record_settlement(b.id, &settled(), NOW).unwrap();
        l.record_delivery(b.id, b"x", None, NOW).unwrap();
        let rj = l.mark_rejected(b.id, "citation", NOW).unwrap();
        assert_eq!(
            (rj.delivery_state, rj.reject_reason.as_deref()),
            (DeliveryState::Rejected, Some("citation"))
        );
    }

    #[test]
    fn resumable_rows_are_exactly_the_section_6_set() {
        let mut l = ledger();
        let prepared = l
            .prepare("m1", "a", None, &payment(1, 100), &request(), NOW)
            .unwrap();
        let sent = l
            .prepare("m1", "b", None, &payment(2, 100), &request(), NOW)
            .unwrap();
        l.commit_submission(sent.id, NOW).unwrap();
        let settled_none = l
            .prepare("m1", "c", None, &payment(3, 100), &request(), NOW)
            .unwrap();
        l.record_settlement(settled_none.id, &settled(), NOW)
            .unwrap();
        let settled_received = l
            .prepare("m1", "d", None, &payment(4, 100), &request(), NOW)
            .unwrap();
        l.record_settlement(settled_received.id, &settled(), NOW)
            .unwrap();
        l.record_delivery(settled_received.id, b"x", None, NOW)
            .unwrap();
        let done = l
            .prepare("m1", "e", None, &payment(5, 100), &request(), NOW)
            .unwrap();
        l.record_settlement(done.id, &settled(), NOW).unwrap();
        l.record_delivery(done.id, b"x", None, NOW).unwrap();
        l.mark_validated(done.id, NOW).unwrap();
        let failed = l
            .prepare("m1", "f", None, &payment(6, 100), &request(), NOW)
            .unwrap();
        l.record_settlement(
            failed.id,
            &Settlement::Failed {
                results: vec!["X".to_owned()],
                duplicates_ignored: 0,
            },
            NOW,
        )
        .unwrap();
        let ids: Vec<i64> = l
            .resumable("m1")
            .unwrap()
            .into_iter()
            .map(|a| a.id)
            .collect();
        assert_eq!(
            ids,
            vec![prepared.id, sent.id, settled_none.id, settled_received.id]
        );
    }

    #[test]
    fn receipts_take_the_next_seq_and_track_publication() {
        let mut l = ledger();
        let r0 = l
            .append_receipt(
                "m1",
                &Receipt::start(
                    "m1",
                    &"a".repeat(64),
                    &"b".repeat(64),
                    "2026-09-08T09:00:00Z",
                ),
            )
            .unwrap();
        assert_eq!(r0.seq, 0);
        let mut r1 = Receipt::start("m1", "x", "y", "2026-09-08T09:00:01Z");
        r1.outcome = crate::receipts::Outcome::Refused;
        r1.seq = 99;
        let r1 = l.append_receipt("m1", &r1).unwrap();
        assert_eq!(r1.seq, 1, "the ledger assigns seq");
        assert_eq!(l.unpublished("m1").unwrap(), vec![0, 1]);
        l.mark_published("m1", 0, 7, "0.0.10399984@1.2", NOW)
            .unwrap();
        assert_eq!(l.unpublished("m1").unwrap(), vec![1]);
        assert!(l.mark_published("m1", 0, 8, "again", NOW).is_err());
        let stored = l.receipts("m1").unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].1, Some(7));
        assert_eq!(stored[1].0.outcome, crate::receipts::Outcome::Refused);
    }

    #[test]
    fn audit_charges_keep_their_identity_and_never_create_budget() {
        let mut l = ledger();
        let valid_until = NOW + Duration::seconds(120);
        let c = l.reserve_audit("m1", "receipt:0", 5_000_000, NOW).unwrap();
        assert_eq!(l.audit_accounts("m1").unwrap().spent(), 5_000_000);
        assert!(matches!(
            l.reserve_audit("m1", "receipt:1", 45_000_001, NOW),
            Err(LedgerError::AuditOverBudget {
                cap: 45_000_001,
                free: 45_000_000
            })
        ));
        let s = l
            .audit_submitted(
                c.id,
                "0.0.10399984@1788800000.1",
                "0.0.10399984-1788800000-000000001",
                valid_until,
                NOW,
            )
            .unwrap();
        assert_eq!(
            (s.state.as_str(), s.tx_id.as_deref(), s.valid_until),
            (
                "submitted",
                Some("0.0.10399984@1788800000.1"),
                Some(valid_until)
            )
        );
        assert!(
            matches!(
                l.release_audit(c.id, NOW),
                Err(LedgerError::AuditWrongState { .. })
            ),
            "identity known: never releasable by hand"
        );
        assert_eq!(l.audit_submitted_charges("m1").unwrap().len(), 1);
        assert!(matches!(
            l.audit_reconciled(c.id, -50, NOW),
            Err(LedgerError::AuditChargeNegative { .. })
        ));
        assert!(matches!(
            l.audit_not_recorded(c.id, valid_until + Duration::seconds(30)),
            Err(LedgerError::AuditStillValid { .. })
        ));
        let done = l.audit_reconciled(c.id, 61_000, NOW).unwrap();
        assert_eq!(
            (done.state.as_str(), done.charged),
            ("reconciled", Some(61_000))
        );
        assert_eq!(
            l.audit_accounts("m1").unwrap(),
            AuditAccounts {
                total: 50_000_000,
                reserved: 0,
                charged: 61_000
            }
        );
        assert!(
            matches!(
                l.audit_reconciled(c.id, 1, NOW),
                Err(LedgerError::AuditWrongState { .. })
            ),
            "reconciled once"
        );

        let o = l.reserve_audit("m1", "receipt:1", 1_000, NOW).unwrap();
        l.audit_submitted(o.id, "t2", "m2", valid_until, NOW)
            .unwrap();
        let over = l.audit_reconciled(o.id, 2_500, NOW).unwrap();
        assert_eq!(
            (over.state.as_str(), over.charged),
            ("overrun", Some(2_500))
        );
        assert_eq!(l.audit_accounts("m1").unwrap().charged, 63_500);

        let n = l.reserve_audit("m1", "receipt:2", 5_000_000, NOW).unwrap();
        l.audit_submitted(n.id, "t3", "m3", valid_until, NOW)
            .unwrap();
        let released = l
            .audit_not_recorded(n.id, valid_until + Duration::seconds(31))
            .unwrap();
        assert_eq!(released.state, "released");

        let d = l.reserve_audit("m1", "receipt:3", 5_000_000, NOW).unwrap();
        l.release_audit(d.id, NOW).unwrap();
        assert_eq!(l.audit_accounts("m1").unwrap().spent(), 63_500);
    }

    #[test]
    fn a_reopened_file_holds_every_committed_transition() {
        let dir = std::env::temp_dir().join(format!(
            "mandate-ledger-{}-{}",
            std::process::id(),
            NOW.unix_timestamp()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ledger.sqlite");
        let (auth_id, res_id) = {
            let mut l = Ledger::open(&path).unwrap();
            l.insert_mandate(&mandate_row(), NOW).unwrap();
            let r = l
                .hold("m1", "explain", 800, ReservationSource::CeilingAtMax, NOW)
                .unwrap();
            let a = l
                .prepare("m1", "events", None, &payment(1, 1_500), &request(), NOW)
                .unwrap();
            l.commit_submission(a.id, NOW).unwrap();
            l.append_receipt("m1", &Receipt::start("m1", "h", "m", "t"))
                .unwrap();
            (a.id, r.id)
        };
        let mut l = Ledger::open(&path).unwrap();
        l.insert_mandate(&mandate_row(), NOW).unwrap();
        let a = l.authorization(auth_id).unwrap();
        assert_eq!(
            (a.payment_state, a.submissions, a.last_submission_at),
            (PaymentState::Sent, 1, Some(NOW))
        );
        assert_eq!(a.signature, payment(1, 1_500).signature);
        assert_eq!(l.reservation(res_id).unwrap().state, ReservationState::Held);
        assert_eq!(
            l.accounts("m1").unwrap(),
            Accounts {
                total: 10_000,
                settled: 0,
                outstanding: 1_500,
                held: 800
            }
        );
        assert_eq!(l.unpublished("m1").unwrap(), vec![0]);
        let mut other = mandate_row();
        other.mandate_hash = "z".repeat(64);
        assert!(matches!(
            l.insert_mandate(&other, NOW),
            Err(LedgerError::MandateHashDiffers(_))
        ));
        std::fs::remove_dir_all(&dir).ok();
    }
}
