# Changelog

All notable changes are documented here. The project has not made a release
yet.

## Unreleased

### Buyer and payment safety

- Added the Mandate runtime, exact x402 payment handling, Hedera settlement,
  SQLite recovery ledger, receipt publishing, and HBAR/HTS budget support.
- Added planning across staged, bundle, and hybrid paths; quote ceilings;
  evidence validation; and explicit refusal, withheld, and unresolved states.
- Hardened recovery for crashes, lost responses, duplicate records, fee-bearing
  tokens, deadlines, and concurrent access to a ledger.

### Reference sellers and evidence

- Added x402-gated reference sellers, durable replay storage, request binding,
  published tariffs, canned fixtures, and Graph-backed pool evidence.
- Added exact decimal and materiality handling, window coverage checks, and
  deterministic cross-SDK signed-payment fixtures.

### Proctor harness

- Added executable task contracts, independent journal and ledger observation,
  structured reports, fixture lifecycle control, and exposure-safe outcomes.
- Added contracts for lost responses, resume without repurchase, and refusal
  before payment, plus an agent-driven repair loop.

### Project quality

- Added workspace and seller CI, pinned Rust and Node toolchains, test
  fixtures, commit-message enforcement, architecture and demo documentation.
