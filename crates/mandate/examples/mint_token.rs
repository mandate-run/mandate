//! Creates an HTS fungible token on testnet from the buyer key and funds the
//! buyer with it, so the settlement path can be proven in a token rather than
//! only in HBAR. The token is a stand-in for a stablecoin: six decimals, the
//! buyer as treasury, no freeze or KYC keys, so any account with automatic
//! associations can receive it.
//!
//! ```text
//! cargo run -p mandate --example mint_token                 # 20.000000 units
//! cargo run -p mandate --example mint_token -- MUSD 50      # symbol, amount
//! ```
//!
//! Prints the token id to pin in a mandate's `budget.service.asset`. Circle's
//! testnet USDC `0.0.429274` is the same code path; this exists because the
//! Circle faucet reports sending without delivering, and an HTS settlement
//! should not wait on someone else's faucet.

use anyhow::Context as _;
use hedera::{Hbar, TokenCreateTransaction, TokenType};
use mandate::config::Config;

/// The decimals a dollar-denominated token uses, and what the runtime's
/// amount parsing assumes for USDC.
const DECIMALS: u32 = 6;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let symbol = args.first().cloned().unwrap_or_else(|| "MUSD".to_owned());
    let whole: u64 = args
        .get(1)
        .map(|a| a.parse())
        .transpose()
        .context("amount must be a whole number of units")?
        .unwrap_or(20);
    let supply = whole * 10u64.pow(DECIMALS);

    let cfg = Config::load()?;
    let signer = cfg.signer()?;
    let consensus = signer.consensus(cfg.network);
    let client = consensus.client();

    let receipt = TokenCreateTransaction::new()
        .name("Mandate test dollar")
        .symbol(&symbol)
        .token_type(TokenType::FungibleCommon)
        .decimals(DECIMALS)
        .initial_supply(supply)
        // The buyer holds the whole supply and pays sellers from it.
        .treasury_account_id(signer.account_id)
        .max_transaction_fee(Hbar::new(30))
        .execute(client)
        .await?
        .get_receipt(client)
        .await?;
    let token = receipt.token_id.context("receipt carries no token id")?;

    println!("token {token} symbol {symbol} decimals {DECIMALS}");
    println!(
        "minted {}.{:0>6} to {}",
        supply / 10u64.pow(DECIMALS),
        supply % 10u64.pow(DECIMALS),
        signer.account_id
    );
    println!("{}/token/{token}", cfg.network.hashscan());
    println!();
    println!("To pay in it, set the asset in the mandate and the sellers:");
    // The runtime knows the decimals of HBAR and USDC and nothing else, so a
    // minted token must declare them or the mandate is refused at load.
    println!(
        "  budget.service = {{ total = \"1.000000\", asset = \"{token}\", decimals = {DECIMALS} }}"
    );
    println!("  SELLER_ASSET={token} in sellers/.env");
    println!();
    println!("Then restart the sellers, refetch the manifest they serve and");
    println!("pin its hash, since a manifest names the asset its listings take:");
    println!("  harness/scripts/token-run {token}");
    println!();
    println!("The seller account must accept the token: with unlimited");
    println!("automatic associations it already does, otherwise associate it once.");
    Ok(())
}
