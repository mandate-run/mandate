//! Proctor: build Hedera services from an executable acceptance contract and
//! test their behavior under payment failure. Design: docs/harness.md.

pub mod check;
pub mod contract;
pub mod exec;
pub mod journal;
