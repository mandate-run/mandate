# Mandate

Give your agent a mandate, not a credit card.

Mandate buys the evidence an agent needs, tracks every payment authorization against its budget, and explains when it cannot finish. It is a buyer runtime for agents that pay per request over x402: given a purpose, a budget and hard constraints, it collects live quotes, plans the cheapest path that can cover the task, reserves the final step, pays one signed Hedera transfer per purchase, validates what it bought, and writes a receipt for every decision to Hedera Consensus Service.

Built from scratch for ETHOnline 2026: Hedera AI & Agentic Payments, Hedera Open Source, The Graph AI Use Case From Scratch. Status: one-pool runs settle on testnet with receipts on HCS; private until submission.

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
| mandate | buyer runtime and CLI | Rust, own x402 types and exact-scheme signer on the Hedera SDK, SQLite |
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

All evidence is live data from the Uniswap v3 subgraph on The Graph Network, id `5zvR82QoaXYFyDEKLZ9t6v9adgnptxYpKpSbxtgVENFV`, queried through the gateway with a Subgraph Studio API key. Sellers read token-denominated TVL at the window's two observation blocks, hourly volume and transaction counts, and the largest event above the materiality threshold, and return the events with transaction hashes as evidence. Each new investigation queries live data; retries return the original purchased result. Every response states its block range, covered window, truncation and indexing status. The runtime computes per-pool outcomes and claims from those facts, decides what to buy next, and checks the explanation against them. `Pool.liquidity` is in-range liquidity and is reported, never used for materiality.

## Harness

Proctor, in `harness/`, builds Hedera services from an executable acceptance contract, tests them under payment failure, and returns a reviewable change with payment evidence. Mandate's recovery behaviors are built and verified through it. It is Mandate's entry to the Hedera Open Source track. Design: [docs/harness.md](docs/harness.md).

## Setup

Prerequisites: Rust 1.96 with `protoc` on the path, Node 24 with pnpm 11, and a Hedera testnet account funded from the [portal](https://portal.hedera.com). USDC runs need the account associated with `0.0.429274` and funded from [faucet.circle.com](https://faucet.circle.com); HBAR runs need nothing else. Live Graph data needs a Subgraph Studio API key; prose from a model needs an Anthropic key. Without them the sellers serve canned facts and a template explanation, which is enough for every command below.

1. Buyer credentials. Copy `crates/mandate/.env.example` to `crates/mandate/.env`, fill `MANDATE_ACCOUNT_ID` and `MANDATE_PRIVATE_KEY`, and `chmod 600` the file. The key never leaves that process.
2. Receipts topic. `cargo run -p mandate -- topic create` prints `HCS_TOPIC_ID=...`; add it to `.env` and to `duties.receipts_topic` in your mandate file.
3. Sellers. `cd sellers && pnpm install`, copy `.env.example` to `.env`, set `SELLER_PAY_TO` to a second testnet account. Start them with canned facts and HBAR pricing: `GRAPH_FIXTURE=material SELLER_ASSET=HBAR pnpm sellers`. `GRAPH_FIXTURE=quiet` serves a pool with nothing material; a `GRAPH_API_KEY` without `GRAPH_FIXTURE` queries the subgraph live; no `SELLER_ASSET` prices in USDC.
4. Pin the manifest. `curl http://127.0.0.1:4021/manifest.json > examples/manifest.hbar.json`, then put its `sha256sum` into `constraints.manifest.hash` of the mandate file. The shipped examples pin the manifest the fixture sellers serve for `pay_to` `0.0.10409989`; with your own seller account, refetch and re-pin.
5. Run. From the repo root:

```sh
cargo run -p mandate -- run examples/one-pool.hbar.toml --id demo-$(date +%s)
cargo run -p mandate -- run examples/one-pool.hbar.toml --id demo-$(date +%s) --json
cargo run -p mandate -- run examples/one-pool-short.hbar.toml --id short-$(date +%s)
```

The first prints the transcript of spec section 12 as it happens: quotes, the three plans with `expected` and `bound`, the explain reserve, each payment with its `pay_` id and `0.0.7162784@` transaction id, settlement from the mirror node record, per-pool outcomes, validation by name, totals and the HCS sequence numbers. The second prints the report and transcript as one JSON document. The third refuses `REQUIREMENT_UNMEETABLE` before any purchase and still publishes its receipts. Exit codes: 0 delivered, 3 refused, 4 delivered with findings, 2 a mandate or manifest that does not load. Every run needs a fresh mandate id, hence `--id`; the ledger is `mandate.sqlite` in the working directory unless `--ledger` says otherwise, and `mandate reconcile <id>` re-reads the mirror node for anything left unresolved.

## Docs

| File | Answers |
|---|---|
| [docs/mandate.md](docs/mandate.md) | Why this exists and where it goes |
| [docs/spec.md](docs/spec.md) | What the runtime must do; authoritative |
| [docs/threat-model.md](docs/threat-model.md) | What can go wrong, who stops it, what remains |
| [docs/harness.md](docs/harness.md) | What Proctor is, what it checks, and its boundaries |
| [docs/demo.md](docs/demo.md) | What the video shows, in order |
| [docs/requirements.md](docs/requirements.md) | Sponsor requirements, status and sources |

## License

Apache-2.0, see [LICENSE](LICENSE).
