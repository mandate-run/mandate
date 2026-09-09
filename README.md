# Mandate

Give your agent a mandate, not a credit card.

Mandate buys the evidence an agent needs, tracks every payment authorization against its budget, and explains when it cannot finish. It is a buyer runtime for agents that pay per request over x402: given a purpose, a budget and hard constraints, it collects live quotes, plans the cheapest path that can cover the task, reserves the final step, pays one signed Hedera transfer per purchase, validates what it bought, and writes a receipt for every decision to Hedera Consensus Service.

Built from scratch for ETHOnline 2026: Hedera AI & Agentic Payments, Hedera Open Source, The Graph AI Use Case From Scratch. Status: five-pool runs settle on testnet with receipts on HCS, plan competition, recovery from lost responses and crashes, and a harness that verifies all of it. Private until submission.

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
| proctor | harness that checks payment behavior against executable contracts, measuring the seller journal and buyer ledger independently | Rust |

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

Prerequisites: Rust 1.96 with `protoc` on the path, Node 24 with pnpm 11, and
two Hedera testnet accounts from the [portal](https://portal.hedera.com). The
buyer needs its key and a few HBAR; the seller only receives, so its id is
enough. They must differ: a transfer to yourself is one net movement, not a
debit and a credit, and the runtime refuses to bind it as a payment.

```sh
cp crates/mandate/.env.example crates/mandate/.env   # MANDATE_ACCOUNT_ID, MANDATE_PRIVATE_KEY
cp sellers/.env.example sellers/.env                 # SELLER_PAY_TO
harness/scripts/setup
```

`setup` installs the seller dependencies, builds, creates a receipts topic on
testnet, pins the served manifest and that topic into the example mandates,
and starts the sellers with canned Graph facts. It stops with an explanation
rather than a confusing failure if either account is missing.

Then, from the repo root:

```sh
cargo run -p mandate -- run examples/five-pools.hbar.toml --id demo-$(date +%s)
cargo run -p mandate -- run examples/five-pools-short.hbar.toml --id short-$(date +%s)
cargo run -p proctor  -- check harness/tasks/resume.toml --dir .
```

The first buys evidence for five pools and settles three real payments on
testnet, printing the transcript of spec section 12 as it happens: quotes, the
three plans with `expected` and `bound`, the explain reserve, each payment with
its `pay_` id and `0.0.7162784@` transaction id, settlement read from the
mirror node, per-pool outcomes, validation by name, totals, and the HCS
sequence numbers. Add `--json` for the report as one document. The second
refuses `REQUIREMENT_UNMEETABLE` before spending anything and still publishes
its receipts. The third is the harness checking that a resumed run never pays
twice.

No API keys are needed for any of that: the sellers serve canned Graph facts
and a template explanation. A `GRAPH_API_KEY` from Subgraph Studio in
`sellers/.env`, with no `GRAPH_FIXTURE`, switches them to the live Uniswap v3
subgraph; an `ANTHROPIC_API_KEY` switches the explanation to a model.

The asset is a field of the mandate, so the same code path settles HBAR and
any HTS token. To pay in a token you mint yourself:

```sh
cargo run -p mandate --example mint_token -- MUSD 50
```

That prints a token id. Put it in `SELLER_ASSET` in `sellers/.env` and in the
mandate's `budget.service`, with `decimals` when the runtime cannot know them,
as `examples/five-pools.hts.toml` shows. `examples/*.usdc.toml` are the same
runs in Circle's testnet USDC `0.0.429274`, for a buyer associated with it and
funded from [faucet.circle.com](https://faucet.circle.com).

Exit codes: 0 delivered, 3 refused, 4 delivered with findings, 5 withheld
because `anchor_before_delivery` is set and a receipt is not yet on HCS, 6 a
payment neither settled nor failed, 2 a mandate or manifest that does not load.
Every run needs a fresh mandate id, hence `--id`; the ledger is
`mandate.sqlite` unless `--ledger` says otherwise, and `mandate reconcile <id>`
re-reads the mirror node for anything left unresolved.

A run that stops mid-purchase, whether the process died or a response never
came, is finished by `mandate resume <file> --ledger <path> --id <id>`. It
reconciles every open authorization against the mirror node, resends or
retrieves the bytes already signed, and carries on where it stopped. Nothing is
signed twice and no purchase acquires a second payment id, so a crash costs at
most one attempt.

## Proctor

The payment behaviors above are verified by a harness in [harness/](harness/),
not only by unit tests. Proctor runs executable task contracts in which every
check has two halves: `expect` reads the buyer's own report, and `observe` reads
what Proctor measured itself from the sellers' journal and the buyer's ledger.

That distinction is the point. A buyer with a recovery bug reports
`status: delivered` with correct totals while quietly paying twice; the seller's
journal shows six signed payments where the contract allows three. Proctor
caught exactly that defect during development, from the journal rather than from
anything the application said about itself.

```sh
cargo run -p proctor -- check harness/tasks/lost-response.toml --dir .
```

Three contracts ship: a response lost after settlement, a resumed run that must
not repurchase, and a refusal that must leave the seller with no signed payment
at all. Design and the comparison with Hedera Harness:
[docs/harness.md](docs/harness.md).

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
