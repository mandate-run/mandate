//! Mandate core. Each module names the spec section it implements.

pub mod analysis;
pub mod config;
pub mod evidence;
pub mod hedera;
pub mod ledger;
pub mod mandate;
pub mod manifest;
pub mod plan;
pub mod publish;
pub mod purchase;
pub mod quote;
pub mod receipts;
pub mod refusal;
pub mod run;
// Compiled for this crate's own unit tests, and for anything that asks for
// the fixtures by name. A release build of the binary has neither.
#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
pub mod testing;
pub mod transcript;
pub mod validate;
pub mod x402;
