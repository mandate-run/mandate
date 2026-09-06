# Mandate

Give your agent a mandate, not a credit card.

Mandate buys the evidence an agent needs, tracks every payment authorization against its budget, and explains when it cannot finish. It is a buyer runtime for agents that pay per request over x402: given a purpose, a budget and hard constraints, it collects live quotes, plans the cheapest path that can cover the task, reserves the final step, pays one signed Hedera transfer per purchase, validates what it bought, and writes a receipt for every decision to Hedera Consensus Service.

Built from scratch for ETHOnline 2026: Hedera AI & Agentic Payments, Hedera Open Source, The Graph AI Use Case From Scratch.

Status: specification complete; buyer runtime implemented in Rust with a fixture demo mode (simulated sellers, settlement and evidence) that replays the demo scenarios end to end. Live x402 sellers, Hedera testnet settlement, The Graph evidence and the Proctor harness are not wired up yet.

## What it does

- Collects real prices by sending each eligible seller the actual request and reading the 402.
- Plans over the remaining work: staged evidence, a partial bundle, or the full bundle, choosing the lowest expected cost among plans whose worst case fits the budget at full coverage.
- Refuses before spending when no plan can guarantee coverage, and says what completion would cost.
- Pays in USDC on Hedera testnet through the Blocky402 facilitator. Settlement is proven by the ledger, never by an HTTP response, and a lost response is recovered without a second payment.
- Computes every claim itself from purchased facts; a model only writes prose. Every calculation is re-evaluated, every citation must exist in purchased evidence, and a sample of cited transactions is checked against Ethereum directly.

## Architecture

![Mandate architecture](docs/img/mandate-architecture.svg)

| Component | Role | Built with |
|---|---|---|
| mandate | buyer runtime and CLI | Rust, r402-hedera for signing, SQLite |
| sellers | four x402-gated reference endpoints with published tariffs and durable result storage | TypeScript, `@x402/hedera`, x402 HTTP resource server with per-request pricing |
| manifest | listings and tariffs, approved and pinned by the principal; each 402 also carries the x402 `bazaar` discovery info | JSON |
| facilitator | verifies and settles; fee payer `0.0.7162784` on testnet | Blocky402, `api.testnet.blocky402.com` |

## Payment flow

1. The runtime sends the real request without payment. The seller answers 402 with `PAYMENT-REQUIRED`: amount, asset, `payTo`, `feePayer`, timeout.
2. The runtime checks the quote against the pinned listing's recipient, asset, network and price ceiling, the facilitator's advertised fee payer, the mandate's constraints and the ledger invariant.
3. The runtime builds a Hedera `TransferTransaction` with the facilitator as fee payer, signs it, generates the transaction id itself, and persists the signed bytes before anything leaves the process.
4. The runtime moves the amount from held to outstanding and resends the request with `PAYMENT-SIGNATURE`, carrying a client payment id for idempotency.
5. The seller verifies through Blocky402, does the work, then settles; Blocky402 co-signs and submits. The result returns with `PAYMENT-RESPONSE`.
6. The runtime proves settlement from the mirror node's records for its own transaction id, ignoring duplicates and requiring transfers that match the approved debit and credit. It moves the amount to settled, validates the result, and queues a receipt for HCS. On a lost response it re-fetches with the same signed payment. An absent record keeps the amount reserved until a later reconciliation finds one.

Normative detail: [docs/spec.md](docs/spec.md) sections 3, 6 and 10.

## The Graph

All evidence is live data from the Uniswap v3 subgraph on The Graph Network, id `5zvR82QoaXYFyDEKLZ9t6v9adgnptxYpKpSbxtgVENFV`, queried through the gateway with a Subgraph Studio API key. Sellers read token-denominated TVL at the window's two block heights, count and sum mints, burns and swaps in the window, and return the events with transaction hashes as evidence. Each new investigation queries live data; retries return the original purchased result. Every response states its block range, covered window, truncation and indexing status. The runtime computes per-pool outcomes and claims from those facts, decides what to buy next, and checks the explanation against them. `Pool.liquidity` is in-range liquidity and is reported, never used for materiality.

## Harness

Proctor, in `harness/`, builds Hedera services from an executable acceptance contract, tests them under payment failure, and returns a reviewable change with payment evidence. Mandate's recovery behaviors are built and verified through it. It is Mandate's entry to the Hedera Open Source track. Design: [docs/harness.md](docs/harness.md).

## Build, test and run

Requirements: a recent stable Rust toolchain (the workspace targets 1.96). No
external services, keys or accounts are needed for the fixture demo.

### Build

```bash
cargo build --workspace          # debug
cargo build --release --workspace
```

### Test

```bash
cargo test --workspace           # unit + fixture scenario + CLI integration tests
cargo clippy --workspace -- -D warnings
```

The test suite pins the demo arithmetic from docs/mandate.md section 6 (plan
costs, refusal numbers, totals), exercises the ledger invariants (I1, I2, I7,
I8) and round-trips the CLI commands against a temp directory.

### Run

The whole CLI surface:

```bash
cargo run -p mandate-cli -- --help            # every command and option
cargo run -p mandate-cli -- init --out-dir <dir>   # write mandate, manifest, empty ledger
cargo run -p mandate-cli -- run --scenario <name>  # execute one demo scenario
cargo run -p mandate-cli -- ledger <mandate_id>    # budget exposure + authorizations
cargo run -p mandate-cli -- receipts <mandate_id>  # receipts in sequence
cargo run -p mandate-cli -- reconcile <mandate_id> # re-check settlement of open authorizations
```

`run` writes the final ledger state and a `transcript.json` (pass `--json` to
print the transcript as JSON instead of the demo-style text).

## Launch the demo

```bash
# 1. create the demo mandate, pinned manifest and empty ledger
cargo run -p mandate-cli -- init --out-dir mandate-demo

# 2. scenario 1: normal completion (staged path chosen)
cargo run -p mandate-cli -- run \
  --mandate-path mandate-demo/mandate.json \
  --manifest-path mandate-demo/manifest.json \
  --ledger-path mandate-demo/ledger.json \
  --transcript-path mandate-demo/transcript.json \
  --scenario normal

# 3. the other demo scenarios: bundle | refusal | offerriff
cargo run -p mandate-cli -- run --mandate-path mandate-demo/mandate.json \
  --manifest-path mandate-demo/manifest.json \
  --ledger-path mandate-demo/ledger.json \
  --transcript-path mandate-demo/transcript.json \
  --scenario bundle
```

What each scenario shows:

| Scenario | Output to look for |
|---|---|
| `normal` | `plan staged expected 0.0033 bound 0.0093 chosen`; three purchases settle; `outcomes: 1 supported, 4 non_material; claims 4`; `validation passed: coverage 5/5`; `unspent 0.0072` |
| `bundle` | the investigate seller quotes 0.0030 live, below its 0.0080 ceiling; `plan bundle expected 0.0030 bound 0.0030 chosen`; one purchase |
| `refusal` | with a budget nothing fits, `REFUSED REQUIREMENT_UNMEETABLE needed bound 0.0080 needed expected 0.0033 available 0.0030`; `settled 0.0000`; run it against a small-budget mandate: `cargo run -p mandate-cli -- init --out-dir mandate-demo-refusal --service-total 0.0030` |
| `offtariff` | after screening, the events seller quotes 0.0020 against its 0.0015 ceiling; `REFUSED OFF_TARIFF`; the hybrid plan (0.0032) wins; `unspent 0.0058` |

After a run, inspect what was persisted:

```bash
cargo run -p mandate-cli -- ledger dev-mandate
cargo run -p mandate-cli -- receipts dev-mandate
cargo run -p mandate-cli -- reconcile dev-mandate   # everything settled: 0 unresolved
```

The fixture executes the real ledger, planning, purchase state machine, brief
and validation logic against simulated quotes, settlement records and evidence
(matching the arithmetic in docs/mandate.md section 6). The video script in
docs/demo.md narrates these scenarios; the fixture's brief is 3 KB, so its
explain quote and totals are slightly smaller than that script assumed.

Going live (not wired up yet) requires a Hedera testnet account associated
with USDC `0.0.429274` holding the service budget in USDC and the audit budget
in HBAR; a Subgraph Studio API key; a model API key; and the reference
sellers.

## Docs

| File | Answers |
|---|---|
| [docs/mandate.md](docs/mandate.md) | Why this exists and where it goes |
| [docs/spec.md](docs/spec.md) | What the runtime must do; authoritative |
| [docs/threat-model.md](docs/threat-model.md) | What can go wrong, who stops it, what remains |
| [docs/harness.md](docs/harness.md) | What Proctor is, what it checks, and its boundaries |
| [docs/demo.md](docs/demo.md) | What the video shows, in order |
| [docs/requirements.md](docs/requirements.md) | Sponsor requirements, status and sources |
