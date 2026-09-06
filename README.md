# Mandate

Give your agent a mandate, not a credit card.

Mandate buys the evidence an agent needs, tracks every payment authorization against its budget, and explains when it cannot finish. It is a buyer runtime for agents that pay per request over x402: given a purpose, a budget and hard constraints, it collects live quotes, plans the cheapest path that can cover the task, reserves the final step, pays one signed Hedera transfer per purchase, validates what it bought, and writes a receipt for every decision to Hedera Consensus Service.

Built from scratch for ETHOnline 2026: Hedera AI & Agentic Payments, Hedera Open Source, The Graph AI Use Case From Scratch. Status: specification complete, no code yet, private until submission.

## What it does

- Collects real prices by sending each eligible seller the actual request and reading the 402.
- Plans over the remaining work: staged evidence, a partial bundle, or the full bundle, choosing the lowest expected cost among plans whose worst case fits the budget at full coverage.
- Refuses before spending when no plan can guarantee coverage, and says what completion would cost.
- Pays in USDC on Hedera testnet through the Blocky402 facilitator. Settlement is proven by the ledger, never by an HTTP response, and a lost response is recovered without a second payment.
- Validates results as structured claims: every calculation is re-evaluated and every citation must exist in purchased evidence.

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
2. The runtime checks the quote against the seller's tariff, the facilitator's advertised fee payer, the mandate's constraints and the ledger invariant.
3. The runtime builds a Hedera `TransferTransaction` with the facilitator as fee payer, signs it, generates the transaction id itself, and persists the signed bytes before anything leaves the process.
4. The runtime moves the amount from held to outstanding and resends the request with `PAYMENT-SIGNATURE`, carrying a client payment id for idempotency.
5. The seller verifies through Blocky402, does the work, then settles; Blocky402 co-signs and submits. The result returns with `PAYMENT-RESPONSE`.
6. The runtime proves settlement from a consensus receipt query or the mirror node record, moves the amount to settled, validates the result, and queues a receipt for HCS. On a lost response it re-fetches with the same signed payment. Absence on the ledger releases exposure only after the ledger's history has passed the authorization's expiry.

Normative detail: [docs/spec.md](docs/spec.md) sections 3, 6 and 9.

## The Graph

All evidence is live data from the Uniswap v3 subgraph on The Graph Network, id `5zvR82QoaXYFyDEKLZ9t6v9adgnptxYpKpSbxtgVENFV`, queried through the gateway with a Subgraph Studio API key. Sellers read token-denominated TVL at the window's two block heights, count and sum mints, burns and swaps in the window, and return the events with transaction hashes as evidence. Each new investigation queries live data; retries return the original purchased result. Every response states its block range, covered window, truncation and indexing status. `Pool.liquidity` is in-range liquidity and is reported, never used for materiality.

## Harness

Proctor, in `harness/`, builds Hedera services from an executable acceptance contract, tests them under payment failure, and returns a reviewable change with payment evidence. Mandate's recovery behaviors are built and verified through it. It is Mandate's entry to the Hedera Open Source track. Design: [docs/harness.md](docs/harness.md).

## Setup

Not runnable yet. Requires a Hedera testnet account associated with USDC `0.0.429274`, holding the service budget in USDC and the audit budget in HBAR; a Subgraph Studio API key; a model API key. Commands are added once the first settlement gate passes.

## Docs

| File | Answers |
|---|---|
| [docs/mandate.md](docs/mandate.md) | Why this exists and where it goes |
| [docs/spec.md](docs/spec.md) | What the runtime must do; authoritative |
| [docs/threat-model.md](docs/threat-model.md) | What can go wrong, who stops it, what remains |
| [docs/harness.md](docs/harness.md) | What Proctor is, what it checks, and its boundaries |
| [docs/demo.md](docs/demo.md) | What the video shows, in order |
| [docs/requirements.md](docs/requirements.md) | Sponsor requirements, status and sources |
