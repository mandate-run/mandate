//! Spec section 10: exact-scheme signer, mirror node record sets, settlement.
//!
//! The signer is our copy of r402-hedera's `create_partially_signed_transfer`
//! (0.21.0), extended with caller-supplied node account ids, `valid_start` and
//! validity duration. HCS submission arrives with slice B.

use std::str::FromStr;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hedera::{
    AccountId, AnyTransaction, Hbar, PrivateKey, TokenId, TransactionId, TransferTransaction,
};
use serde::Deserialize;
use time::{Duration, OffsetDateTime};

pub use hedera::{AccountId as HederaAccountId, TokenId as HederaTokenId};

/// Longest validity the runtime signs, section 10.
pub const MAX_VALID_SECONDS: u64 = 120;
/// How far in the past `valid_start` is set to absorb clock skew, section 10.
pub const VALID_START_SKEW: Duration = Duration::seconds(5);
/// Grace after `valid_until` before an empty record set means `unresolved`, section 6.
pub const RECORD_GRACE: Duration = Duration::seconds(30);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("hedera sdk: {0}")]
    Sdk(Box<hedera::Error>),
    #[error("mirror node http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("mirror node returned {status} for {url}")]
    Status { status: u16, url: String },
    #[error("transaction bytes carry no transaction id")]
    MissingTransactionId,
    #[error("transaction bytes are not a transfer")]
    NotATransfer,
}

impl From<hedera::Error> for Error {
    fn from(e: hedera::Error) -> Self {
        Self::Sdk(Box::new(e))
    }
}

/// The runtime payment account and its key. The key never leaves this type.
pub struct Signer {
    pub account_id: AccountId,
    key: PrivateKey,
}

impl std::fmt::Debug for Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Signer")
            .field("account_id", &self.account_id)
            .finish_non_exhaustive()
    }
}

impl Signer {
    pub fn new(account_id: AccountId, key: PrivateKey) -> Self {
        Self { account_id, key }
    }

    /// Parses `0.0.x` and a DER or raw hex private key.
    pub fn from_strings(account_id: &str, private_key: &str) -> Result<Self, Error> {
        Ok(Self::new(
            AccountId::from_str(account_id)?,
            PrivateKey::from_str(private_key)?,
        ))
    }

    /// A throwaway ed25519 key for tests and dry runs.
    pub fn ephemeral(account_id: AccountId) -> Self {
        Self::new(account_id, PrivateKey::generate_ed25519())
    }
}

/// What is being paid: HBAR in tinybars or an HTS token in its atomic units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asset {
    Hbar,
    Token(TokenId),
}

/// The asset id the x402 Hedera scheme uses for HBAR.
pub const HBAR_ASSET_ID: &str = "0.0.0";

impl Asset {
    /// Parses an x402 asset field: `0.0.0` or `HBAR` for HBAR, else a token id.
    pub fn parse(asset: &str) -> Result<Self, Error> {
        if asset == HBAR_ASSET_ID || asset.eq_ignore_ascii_case("hbar") {
            Ok(Self::Hbar)
        } else {
            Ok(Self::Token(TokenId::from_str(asset)?))
        }
    }
}

impl std::fmt::Display for Asset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Hbar => f.write_str(HBAR_ASSET_ID),
            Self::Token(id) => write!(f, "{id}"),
        }
    }
}

/// One exact payment to sign.
#[derive(Debug, Clone)]
pub struct Transfer<'a> {
    /// The facilitator account that submits and pays fees; becomes the transaction id account.
    pub fee_payer: AccountId,
    pub pay_to: AccountId,
    pub asset: Asset,
    /// Atomic units. The runtime is debited this amount, `pay_to` is credited it.
    pub amount: i64,
    pub node_account_ids: &'a [AccountId],
    pub valid_start: OffsetDateTime,
    pub valid_duration: Duration,
}

/// A frozen, buyer-signed transaction ready to travel in `payload.transaction`.
#[derive(Debug, Clone)]
pub struct SignedTransfer {
    pub transaction_id: TransactionId,
    pub bytes: Vec<u8>,
    pub valid_until: OffsetDateTime,
}

impl SignedTransfer {
    pub fn base64(&self) -> String {
        STANDARD.encode(&self.bytes)
    }

    pub fn mirror_id(&self) -> String {
        mirror_id(&self.transaction_id)
    }
}

/// `0.0.x-seconds-nanos`, the form the mirror node accepts in paths.
pub fn mirror_id(id: &TransactionId) -> String {
    format!(
        "{}-{}-{:09}",
        id.account_id,
        id.valid_start.unix_timestamp(),
        id.valid_start.nanosecond()
    )
}

/// `min(max_timeout_seconds, 120)` seconds, section 10.
pub fn valid_duration(max_timeout_seconds: u64) -> Duration {
    Duration::seconds(max_timeout_seconds.min(MAX_VALID_SECONDS) as i64)
}

/// `now - 5 s`, section 10.
pub fn valid_start_now() -> OffsetDateTime {
    OffsetDateTime::now_utc() - VALID_START_SKEW
}

/// Builds, freezes and signs the transfer with the runtime key only.
pub fn sign_transfer(signer: &Signer, t: &Transfer<'_>) -> Result<SignedTransfer, Error> {
    let mut tx = TransferTransaction::new();
    match t.asset {
        Asset::Hbar => {
            tx.hbar_transfer(signer.account_id, Hbar::from_tinybars(-t.amount))
                .hbar_transfer(t.pay_to, Hbar::from_tinybars(t.amount));
        }
        Asset::Token(token) => {
            tx.token_transfer(token, signer.account_id, -t.amount)
                .token_transfer(token, t.pay_to, t.amount);
        }
    }
    let mut transaction_id = TransactionId::generate(t.fee_payer);
    transaction_id.valid_start = t.valid_start;
    tx.transaction_id(transaction_id)
        .node_account_ids(t.node_account_ids.iter().copied())
        .transaction_valid_duration(t.valid_duration);
    tx.freeze()?;
    tx.sign(signer.key.clone());
    let bytes = tx.to_bytes()?;
    Ok(SignedTransfer {
        transaction_id,
        bytes,
        valid_until: t.valid_start + t.valid_duration,
    })
}

/// One token movement inside a transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenMovement {
    pub token: TokenId,
    pub account: AccountId,
    pub amount: i64,
}

/// What signed bytes actually say. Used by tests, by the ledger before it
/// persists bytes, and by Proctor.
#[derive(Debug, Clone)]
pub struct Inspected {
    pub transaction_id: TransactionId,
    pub node_account_ids: Vec<AccountId>,
    pub valid_duration: Option<Duration>,
    pub hbar: Vec<(AccountId, i64)>,
    pub tokens: Vec<TokenMovement>,
}

/// Decodes transaction bytes without a network.
pub fn inspect(bytes: &[u8]) -> Result<Inspected, Error> {
    let any = AnyTransaction::from_bytes(bytes)?;
    let transaction_id = any
        .get_transaction_id()
        .ok_or(Error::MissingTransactionId)?;
    let node_account_ids = any
        .get_node_account_ids()
        .map(<[AccountId]>::to_vec)
        .unwrap_or_default();
    let valid_duration = any.get_transaction_valid_duration();
    let tx = any
        .downcast::<TransferTransaction>()
        .map_err(|_| Error::NotATransfer)?;
    let mut hbar: Vec<(AccountId, i64)> = tx
        .get_hbar_transfers()
        .into_iter()
        .map(|(account, amount)| (account, amount.to_tinybars()))
        .collect();
    hbar.sort_by_key(|(account, _)| account.to_string());
    let mut tokens = Vec::new();
    for (token, accounts) in tx.get_token_transfers() {
        for (account, amount) in accounts {
            tokens.push(TokenMovement {
                token,
                account,
                amount,
            });
        }
    }
    tokens.sort_by_key(|m| (m.token.to_string(), m.account.to_string()));
    Ok(Inspected {
        transaction_id,
        node_account_ids,
        valid_duration,
        hbar,
        tokens,
    })
}

/// One record from `/api/v1/transactions/{id}`. Unknown fields are ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct MirrorRecord {
    pub transaction_id: String,
    pub result: String,
    #[serde(default)]
    pub nonce: u32,
    #[serde(default)]
    pub consensus_timestamp: String,
    #[serde(default)]
    pub transfers: Vec<HbarEntry>,
    #[serde(default)]
    pub token_transfers: Vec<TokenEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HbarEntry {
    pub account: String,
    pub amount: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TokenEntry {
    pub token_id: String,
    pub account: String,
    pub amount: i64,
}

#[derive(Deserialize)]
struct MirrorRecords {
    #[serde(default)]
    transactions: Vec<MirrorRecord>,
}

/// Read-only mirror node access. Free; no consensus node queries.
#[derive(Debug, Clone)]
pub struct MirrorNode {
    client: reqwest::Client,
    base_url: String,
}

impl MirrorNode {
    pub fn new(client: reqwest::Client, base_url: impl Into<String>) -> Self {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        Self { client, base_url }
    }

    /// Every record for a transaction id, including duplicates. 404 means none yet.
    pub async fn records(&self, mirror_id: &str) -> Result<Vec<MirrorRecord>, Error> {
        let url = format!("{}/api/v1/transactions/{mirror_id}", self.base_url);
        let resp = self.client.get(&url).send().await?;
        if resp.status().as_u16() == 404 {
            return Ok(Vec::new());
        }
        if !resp.status().is_success() {
            return Err(Error::Status {
                status: resp.status().as_u16(),
                url,
            });
        }
        Ok(resp.json::<MirrorRecords>().await?.transactions)
    }

    /// Consensus node account ids to freeze against, section 10.
    pub async fn node_account_ids(&self, limit: usize) -> Result<Vec<AccountId>, Error> {
        #[derive(Deserialize)]
        struct Nodes {
            #[serde(default)]
            nodes: Vec<Node>,
        }
        #[derive(Deserialize)]
        struct Node {
            node_account_id: String,
        }
        let url = format!("{}/api/v1/network/nodes?limit={limit}", self.base_url);
        let resp = self.client.get(&url).send().await?;
        if !resp.status().is_success() {
            return Err(Error::Status {
                status: resp.status().as_u16(),
                url,
            });
        }
        let nodes = resp.json::<Nodes>().await?.nodes;
        nodes
            .iter()
            .map(|n| AccountId::from_str(&n.node_account_id).map_err(Error::from))
            .collect()
    }
}

/// The transfers a settled record must contain.
#[derive(Debug, Clone)]
pub struct Expected {
    pub asset: Asset,
    pub from: AccountId,
    pub to: AccountId,
    pub amount: i64,
}

pub const DUPLICATE: &str = "DUPLICATE_TRANSACTION";

/// I5 applied to a record set. Only nonce-zero records count, and
/// `DUPLICATE_TRANSACTION` records are ignored everywhere: a duplicate says
/// another submission of the same id was accepted first, whose own record
/// decides the outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settlement {
    /// A `SUCCESS` record with the expected transfers.
    Settled {
        consensus_timestamp: String,
        duplicates_ignored: usize,
    },
    /// No `SUCCESS` record and at least one non-duplicate failure record.
    Failed {
        results: Vec<String>,
        duplicates_ignored: usize,
    },
    /// A `SUCCESS` record exists but its transfers are not the expected ones.
    /// Never released automatically; reconcile by hand.
    Anomaly {
        results: Vec<String>,
        duplicates_ignored: usize,
    },
    /// No record that decides anything yet, duplicates included. Absence releases nothing.
    Absent { duplicates_ignored: usize },
}

/// Classifies a record set, section 10 and invariant I5.
pub fn settlement(records: &[MirrorRecord], expected: &Expected) -> Settlement {
    let nonce_zero: Vec<&MirrorRecord> = records.iter().filter(|r| r.nonce == 0).collect();
    let duplicates_ignored = nonce_zero.iter().filter(|r| r.result == DUPLICATE).count();
    let considered: Vec<&MirrorRecord> = nonce_zero
        .iter()
        .copied()
        .filter(|r| r.result != DUPLICATE)
        .collect();
    if considered.is_empty() {
        return Settlement::Absent { duplicates_ignored };
    }
    let results = || {
        considered
            .iter()
            .map(|r| r.result.clone())
            .collect::<Vec<_>>()
    };
    let successes: Vec<&MirrorRecord> = considered
        .iter()
        .copied()
        .filter(|r| r.result == "SUCCESS")
        .collect();
    if successes.is_empty() {
        return Settlement::Failed {
            results: results(),
            duplicates_ignored,
        };
    }
    match successes.iter().find(|r| has_transfers(r, expected)) {
        Some(r) => Settlement::Settled {
            consensus_timestamp: r.consensus_timestamp.clone(),
            duplicates_ignored,
        },
        None => Settlement::Anomaly {
            results: results(),
            duplicates_ignored,
        },
    }
}

fn has_transfers(r: &MirrorRecord, e: &Expected) -> bool {
    let from = e.from.to_string();
    let to = e.to.to_string();
    match e.asset {
        Asset::Hbar => {
            r.transfers
                .iter()
                .any(|t| t.account == from && t.amount == -e.amount)
                && r.transfers
                    .iter()
                    .any(|t| t.account == to && t.amount == e.amount)
        }
        Asset::Token(token) => {
            let token = token.to_string();
            r.token_transfers
                .iter()
                .any(|t| t.token_id == token && t.account == from && t.amount == -e.amount)
                && r.token_transfers
                    .iter()
                    .any(|t| t.token_id == token && t.account == to && t.amount == e.amount)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acct(s: &str) -> AccountId {
        AccountId::from_str(s).unwrap()
    }

    #[test]
    fn signs_with_fee_payer_id_nodes_and_duration() {
        let signer = Signer::ephemeral(acct("0.0.5"));
        let nodes = [acct("0.0.3"), acct("0.0.4")];
        let valid_start = valid_start_now();
        let t = Transfer {
            fee_payer: acct("0.0.7162784"),
            pay_to: acct("0.0.111"),
            asset: Asset::parse("0.0.429274").unwrap(),
            amount: 1000,
            node_account_ids: &nodes,
            valid_start,
            valid_duration: valid_duration(600),
        };
        let signed = sign_transfer(&signer, &t).unwrap();
        assert_eq!(signed.valid_until, valid_start + Duration::seconds(120));
        let bytes = STANDARD.decode(signed.base64()).unwrap();
        assert_eq!(bytes, signed.bytes);

        let seen = inspect(&signed.bytes).unwrap();
        assert_eq!(seen.transaction_id.account_id, acct("0.0.7162784"));
        assert_eq!(seen.transaction_id.valid_start, valid_start);
        assert_eq!(seen.node_account_ids, nodes);
        assert_eq!(seen.valid_duration, Some(Duration::seconds(120)));
        assert!(seen.hbar.is_empty());
        let token = TokenId::from_str("0.0.429274").unwrap();
        assert_eq!(
            seen.tokens,
            vec![
                TokenMovement {
                    token,
                    account: acct("0.0.111"),
                    amount: 1000
                },
                TokenMovement {
                    token,
                    account: acct("0.0.5"),
                    amount: -1000
                },
            ]
        );

        let id = signed.mirror_id();
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts[0], "0.0.7162784");
        assert_eq!(parts[1], valid_start.unix_timestamp().to_string());
        assert_eq!(parts[2].len(), 9);
    }

    #[test]
    fn hbar_transfer_signs_too() {
        let signer = Signer::ephemeral(acct("0.0.5"));
        let nodes = [acct("0.0.3")];
        let t = Transfer {
            fee_payer: acct("0.0.7162784"),
            pay_to: acct("0.0.111"),
            asset: Asset::parse("0.0.0").unwrap(),
            amount: 250,
            node_account_ids: &nodes,
            valid_start: valid_start_now(),
            valid_duration: valid_duration(30),
        };
        let seen = inspect(&sign_transfer(&signer, &t).unwrap().bytes).unwrap();
        assert_eq!(seen.valid_duration, Some(Duration::seconds(30)));
        assert_eq!(
            seen.hbar,
            vec![(acct("0.0.111"), 250), (acct("0.0.5"), -250)]
        );
        assert!(seen.tokens.is_empty());
    }

    #[test]
    fn hbar_asset_ids() {
        assert_eq!(Asset::parse("0.0.0").unwrap(), Asset::Hbar);
        assert_eq!(Asset::parse("HBAR").unwrap(), Asset::Hbar);
        assert_eq!(Asset::Hbar.to_string(), "0.0.0");
        assert!(matches!(
            Asset::parse("0.0.429274").unwrap(),
            Asset::Token(_)
        ));
    }

    #[test]
    fn duration_is_capped() {
        assert_eq!(valid_duration(600), Duration::seconds(120));
        assert_eq!(valid_duration(120), Duration::seconds(120));
        assert_eq!(valid_duration(45), Duration::seconds(45));
    }

    const MIRROR: &str = r#"{"transactions":[
      {"bytes":null,"charged_tx_fee":72000,"consensus_timestamp":"1757200000.123456789","entity_id":null,"max_fee":"100000000","memo_base64":"","name":"CRYPTOTRANSFER","nft_transfers":[],"node":"0.0.3","nonce":0,"parent_consensus_timestamp":null,"result":"SUCCESS","scheduled":false,"staking_reward_transfers":[],"token_transfers":[{"token_id":"0.0.429274","account":"0.0.111","amount":1000,"is_approval":false},{"token_id":"0.0.429274","account":"0.0.5","amount":-1000,"is_approval":false}],"transaction_hash":"AA==","transaction_id":"0.0.7162784-1757199995-000000001","transfers":[{"account":"0.0.3","amount":3200,"is_approval":false},{"account":"0.0.98","amount":68800,"is_approval":false},{"account":"0.0.7162784","amount":-72000,"is_approval":false}],"valid_duration_seconds":"120","valid_start_timestamp":"1757199995.000000001"},
      {"consensus_timestamp":"1757200000.223456789","name":"CRYPTOTRANSFER","node":"0.0.4","nonce":0,"result":"DUPLICATE_TRANSACTION","token_transfers":[],"transaction_id":"0.0.7162784-1757199995-000000001","transfers":[{"account":"0.0.4","amount":100,"is_approval":false},{"account":"0.0.7162784","amount":-100,"is_approval":false}]}
    ]}"#;

    fn expected() -> Expected {
        Expected {
            asset: Asset::parse("0.0.429274").unwrap(),
            from: acct("0.0.5"),
            to: acct("0.0.111"),
            amount: 1000,
        }
    }

    #[test]
    fn mirror_records_parse_with_unknown_fields() {
        let set: MirrorRecords = serde_json::from_str(MIRROR).unwrap();
        assert_eq!(set.transactions.len(), 2);
        assert_eq!(set.transactions[0].token_transfers.len(), 2);
        assert_eq!(set.transactions[1].result, "DUPLICATE_TRANSACTION");
    }

    #[test]
    fn settlement_ignores_duplicates() {
        let set: MirrorRecords = serde_json::from_str(MIRROR).unwrap();
        assert_eq!(
            settlement(&set.transactions, &expected()),
            Settlement::Settled {
                consensus_timestamp: "1757200000.123456789".to_owned(),
                duplicates_ignored: 1
            }
        );
    }

    #[test]
    fn duplicates_alone_decide_nothing() {
        assert_eq!(
            settlement(&[], &expected()),
            Settlement::Absent {
                duplicates_ignored: 0
            }
        );
        let set: MirrorRecords = serde_json::from_str(MIRROR).unwrap();
        let only_duplicate = vec![set.transactions[1].clone()];
        assert_eq!(
            settlement(&only_duplicate, &expected()),
            Settlement::Absent {
                duplicates_ignored: 1
            }
        );
    }

    #[test]
    fn failed_needs_a_non_duplicate_failure_record() {
        let set: MirrorRecords = serde_json::from_str(MIRROR).unwrap();
        let mut failed = set.transactions[1].clone();
        failed.result = "INSUFFICIENT_TOKEN_BALANCE".to_owned();
        let records = vec![failed, set.transactions[1].clone()];
        assert_eq!(
            settlement(&records, &expected()),
            Settlement::Failed {
                results: vec!["INSUFFICIENT_TOKEN_BALANCE".to_owned()],
                duplicates_ignored: 1
            }
        );
    }

    #[test]
    fn child_records_and_wrong_transfers() {
        let set: MirrorRecords = serde_json::from_str(MIRROR).unwrap();
        let mut child = set.transactions[0].clone();
        child.nonce = 1;
        child.token_transfers.clear();
        let with_child = vec![child, set.transactions[1].clone()];
        assert_eq!(
            settlement(&with_child, &expected()),
            Settlement::Absent {
                duplicates_ignored: 1
            }
        );

        let wrong_amount = Expected {
            amount: 999,
            ..expected()
        };
        assert_eq!(
            settlement(&set.transactions, &wrong_amount),
            Settlement::Anomaly {
                results: vec!["SUCCESS".to_owned()],
                duplicates_ignored: 1
            }
        );
    }
}
