# Mandate

Give your agent a mandate, not a credit card.

Mandate is a buyer runtime for agents that pay per request over x402. Given a purpose, a budget and hard constraints, it buys the evidence a task needs from live-quoted sellers, keeps enough budget to finish, refuses what it cannot justify, and writes a receipt for every decision to Hedera Consensus Service.

Built from scratch for ETHOnline 2026: Hedera AI & Agentic Payments, The Graph AI Use Case From Scratch. Status: pre-alpha, private until submission.

## What it does

- Collects real prices by sending each eligible seller the actual request and reading the 402.
- Plans the cheapest path that meets the task's evidence requirement, checks that the worst case fits the budget, and reserves the final step first.
- Pays in USDC on Hedera testnet through the Blocky402 facilitator, one signed transfer per purchase.
- Validates results: every cited transaction must exist in purchased evidence.
- Stops with reasons, before spending, when no affordable plan can meet the requirement.

## Architecture

```
mandate.yaml -> mandate (Rust)
                  quoter     unsigned requests, parses PAYMENT-REQUIRED
                  planner    expected cost and worst-case bound per plan
                  ledger     SQLite: settled + outstanding + held <= budget
                  signer     Hedera TransferTransaction via r402-hedera
                  validator  citations, freshness, schema, seller receipt
                  receipts   one HCS message per decision
                     |  x402 v2 exact, hedera:testnet, payment-identifier
                     v
                sellers (TypeScript, one process, four routes)
                  screen . events . investigate . explain
                     |                          |
                     v                          v
                Uniswap v3 subgraph         model API
                (Subgraph Studio key)

                Blocky402 verifies, co-signs and submits each transfer
```

| Component | Role | Built with |
|---|---|---|
| mandate | buyer runtime and CLI | Rust, r402-hedera for signing, SQLite |
| sellers | four x402-gated endpoints with published tariffs | TypeScript, `@x402/hedera`, x402 HTTP resource server with per-request pricing |
| manifest | listings and tariffs, served over HTTP; each 402 also carries the x402 `bazaar` discovery info | JSON |
| facilitator | verifies and settles; fee payer `0.0.7162784` on testnet | Blocky402, `api.testnet.blocky402.com` |

## Payment flow

1. The runtime sends the real request without payment. The seller answers 402 with `PAYMENT-REQUIRED`: amount, asset, `payTo`, `feePayer`, timeout.
2. The runtime checks the quote against the seller's tariff, the facilitator's advertised fee payer, the mandate's constraints and the ledger invariant.
3. The runtime builds a Hedera `TransferTransaction` with the facilitator as fee payer, signs it, and generates the transaction id itself.
4. The runtime records the amount as outstanding and resends the request with `PAYMENT-SIGNATURE`, carrying a client payment id for idempotency.
5. The seller verifies through Blocky402, does the work, then settles; Blocky402 co-signs and submits. The result returns with `PAYMENT-RESPONSE`.
6. The runtime treats the mirror node as the only proof of settlement, moves the amount to settled, validates the result, and appends a receipt to HCS. On a timeout it polls its own transaction id and never pays twice.

Normative detail: [docs/spec.md](docs/spec.md) sections 3, 6 and 9.

## The Graph

All evidence is live data from the Uniswap v3 subgraph on The Graph Network, id `5zvR82QoaXYFyDEKLZ9t6v9adgnptxYpKpSbxtgVENFV`, queried through the gateway with a Subgraph Studio API key. Sellers read `PoolHourData.tvlUSD` at the window's ends to screen for material change, and return mints, burns and swaps with transaction hashes as evidence. `Pool.liquidity` is in-range liquidity and is reported, never used for materiality. The runtime decides what to buy from those results, and the explanation is checked against them. Nothing is cached or seeded.

## Setup

Not runnable yet. Requires a Hedera testnet account associated with USDC `0.0.429274`, funded with the mandate budget in USDC and a little HBAR for association and HCS fees; a Subgraph Studio API key; a model API key. Commands are added once the first settlement gate passes.

## Docs

| File | Answers |
|---|---|
| [docs/mandate.md](docs/mandate.md) | Why this exists and where it goes |
| [docs/spec.md](docs/spec.md) | What the runtime must do |
| [docs/threat-model.md](docs/threat-model.md) | What can go wrong and what stops it |
| [docs/demo.md](docs/demo.md) | What the video shows, in order |
| [docs/requirements.md](docs/requirements.md) | Sponsor requirements and status |
