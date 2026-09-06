# Mandate

Give your agent a mandate, not a credit card.

Mandate is a buyer runtime for agents that pay per request over x402. Given a purpose, a budget and hard constraints, it buys the evidence a task needs from live-quoted sellers, keeps enough budget to finish, refuses what it cannot justify, and writes a receipt for every decision to Hedera Consensus Service.

Built from scratch for ETHOnline 2026: Hedera AI & Agentic Payments, The Graph AI Use Case From Scratch. Status: pre-alpha, private until submission.

## What it does

- Collects real prices by sending each eligible seller the actual request and reading the 402.
- Plans the cheapest path that meets the task's evidence requirement, reserving the final step first.
- Pays in USDC on Hedera testnet through the Blocky402 facilitator, one signed transfer per purchase.
- Validates results: every cited transaction must exist in purchased evidence.
- Stops with reasons when funds, evidence or payment status make finishing impossible.

## Architecture

```
mandate.yaml -> mandate (Rust)
                  quoter     unsigned requests, parses PAYMENT-REQUIRED
                  planner    staged vs bundled, completion reserve
                  ledger     SQLite: settled + outstanding + held <= budget
                  signer     builds and signs the Hedera TransferTransaction
                  validator  citations, freshness, schema
                  receipts   one HCS message per decision
                     |  x402 v2, exact, hedera:testnet
                     v
                sellers (TypeScript, one process, four routes)
                  screen . events . investigate . explain
                     |                          |
                     v                          v
                Uniswap v3 subgraph         model API
                (Subgraph Studio key)

                Blocky402 verifies, co-signs and submits each transfer
```

| Component | Role | Language |
|---|---|---|
| mandate | buyer runtime and CLI | Rust |
| sellers | four x402-gated endpoints with published tariffs | TypeScript |
| manifest | offers and tariffs, served over HTTP | JSON |
| ledger | durable budget state | SQLite |

## Payment flow

1. The runtime sends the real request without payment. The seller answers 402 with `PAYMENT-REQUIRED`: amount, asset, `payTo`, `feePayer`, timeout.
2. The runtime checks the quote against the seller's tariff, the mandate's constraints and the ledger invariant.
3. The runtime builds a Hedera `TransferTransaction` with the facilitator as fee payer, signs it, and generates the transaction id itself.
4. The runtime records the amount as outstanding and resends the request with `PAYMENT-SIGNATURE`.
5. The seller forwards the payment to Blocky402, which verifies, co-signs and submits it, then does the work and returns the result.
6. The runtime confirms settlement against the mirror node, moves the amount to settled, validates the result, and appends a receipt to HCS. On a timeout it polls its own transaction id and never pays twice.

Normative detail: [docs/spec.md](docs/spec.md) sections 3, 6 and 9.

## The Graph

All evidence is live data from the Uniswap v3 subgraph on The Graph Network, queried with a Subgraph Studio API key. Sellers compute liquidity deltas and materiality from pool snapshots and return mints, burns and swaps with transaction hashes. The runtime decides what to buy from those results, and the explanation is checked against them. Nothing is cached or seeded.

## Setup

Not runnable yet. Requires a Hedera testnet account associated with USDC `0.0.429274` and funded with the mandate budget, a Subgraph Studio API key, and a model API key. Commands are added once the first settlement gate passes.

## Docs

| File | Answers |
|---|---|
| [docs/mandate.md](docs/mandate.md) | Why this exists and where it goes |
| [docs/spec.md](docs/spec.md) | What the runtime must do |
| [docs/threat-model.md](docs/threat-model.md) | What can go wrong and what stops it |
| [docs/demo.md](docs/demo.md) | What the video shows, in order |
| [docs/requirements.md](docs/requirements.md) | Sponsor requirements and status |
