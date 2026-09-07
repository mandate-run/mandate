//! One-time testnet setup from the buyer key: creates the seller account with
//! a fresh ED25519 key and unlimited automatic token associations, so both
//! accounts can receive USDC without an association transaction.
//!
//! ```text
//! cargo run -p mandate --example setup
//! ```
//!
//! Reads `MANDATE_ACCOUNT_ID` and `MANDATE_PRIVATE_KEY` from the process
//! environment or `crates/mandate/.env`. Prints the seller account id for
//! `sellers/.env` and the seller key once; the key is not needed by any
//! process in this repository and should be kept outside it.

use std::collections::HashMap;
use std::str::FromStr;

use anyhow::Context as _;
use hedera::{AccountCreateTransaction, AccountId, Client, Hbar, PrivateKey};

fn env(name: &str) -> anyhow::Result<String> {
    if let Ok(v) = std::env::var(name)
        && !v.is_empty()
    {
        return Ok(v);
    }
    let text = std::fs::read_to_string("crates/mandate/.env")
        .or_else(|_| std::fs::read_to_string(".env"))
        .with_context(|| format!("{name} is not set and no .env file was found"))?;
    let file: HashMap<&str, &str> = text
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim(), v.trim()))
        .collect();
    file.get(name)
        .filter(|v| !v.is_empty())
        .map(|v| (*v).to_owned())
        .with_context(|| format!("{name} is not set"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let buyer = AccountId::from_str(&env("MANDATE_ACCOUNT_ID")?)?;
    let buyer_key = PrivateKey::from_str(&env("MANDATE_PRIVATE_KEY")?)?;
    let client = Client::for_testnet();
    client.set_operator(buyer, buyer_key);

    let seller_key = PrivateKey::generate_ed25519();
    let receipt = AccountCreateTransaction::new()
        .set_key_without_alias(seller_key.public_key())
        .initial_balance(Hbar::new(10))
        .max_automatic_token_associations(-1)
        .account_memo("mandate seller")
        .execute(&client)
        .await?
        .get_receipt(&client)
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
