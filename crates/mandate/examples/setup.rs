//! One-time testnet setup from the buyer key: creates the seller account with
//! a fresh ED25519 key and unlimited automatic token associations, so both
//! accounts can receive USDC without an association transaction.
//!
//! ```text
//! cargo run -p mandate --example setup
//! ```
//!
//! Reads the buyer configuration like the CLI does, see `mandate::config`.
//! Prints the seller account id for `sellers/.env` and the seller key once;
//! the key is not needed by any process in this repository and should be
//! kept outside it.

use anyhow::Context as _;
use hedera::{AccountCreateTransaction, Hbar, PrivateKey};
use mandate::config::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = Config::load()?;
    let consensus = cfg.signer()?.consensus(cfg.network);

    let seller_key = PrivateKey::generate_ed25519();
    let receipt = AccountCreateTransaction::new()
        .set_key_without_alias(seller_key.public_key())
        .initial_balance(Hbar::new(10))
        .max_automatic_token_associations(-1)
        .account_memo("mandate seller")
        .execute(consensus.client())
        .await?
        .get_receipt(consensus.client())
        .await?;
    let seller = receipt
        .account_id
        .context("receipt carries no account id")?;
    println!("seller account created, 10 HBAR, unlimited automatic associations");
    println!("SELLER_PAY_TO={seller}");
    println!("seller private key, keep outside the repository:");
    println!("{}", seller_key.to_string_der());
    Ok(())
}
