# 0001: repo layout and toolchains

Date: 2026-09-07. Issue: #1. Status: accepted.

**Decision.** One repository: a Cargo workspace with `crates/mandate` and `harness`, one pnpm package in `sellers/`, one env file per tree, toolchains pinned in the tree, TOML for mandate files and task contracts.

**Why.** Proctor depends on Mandate core for signing and the ledger, judges get one README and one video, and a shared root env would hand the buyer key to sellers and to agent subprocesses.

**Consequences.** Rust via `rust-toolchain.toml`, edition 2024, `protoc` required by the Hedera SDK build; Node 24 as the supported major, any minor, pnpm 11.9.0 via `packageManager`, x402 packages pinned to 2.25.0. The x402 wire types are our own and tolerate unknown fields, since r402-protocol's structs deny them; the Hedera signer is our copy of r402-hedera's `create_partially_signed_transfer` with mirror-node node ids and `valid_start = now - 5 s`, and no r402 crate is a dependency. Mandate files are TOML like Proctor tasks. Secrets live in `crates/mandate/.env`, `sellers/.env` and `harness/.env`; Proctor starts children with an allowlisted environment. CI runs fmt, build, clippy and test for Rust and build and typecheck for the sellers on every push. Ruled out: separate repositories, Rust sellers, a TypeScript buyer, a shared `.env`.

**Amended 2026-09-09.** The toolchain pin moved from 1.96.0 to 1.98.1, the
current stable: the workspace builds, tests, lints and formats clean on it,
and a project submitted for review should be on the compiler a reviewer has.
The pin lives in `rust-toolchain.toml` alone, so CI and a clean clone follow
it without a second place to update.
