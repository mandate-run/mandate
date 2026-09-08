//! Mandate core. Each module names the spec section it implements.

pub mod analysis;
pub mod config;
pub mod evidence;
pub mod hedera;
pub mod ledger;
pub mod mandate;
pub mod plan;
pub mod purchase;
pub mod quote;
pub mod receipts;
#[doc(hidden)]
pub mod testing;
pub mod transcript;
pub mod validate;
pub mod x402;
