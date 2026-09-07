# Requirements

Sponsor requirements for ETHOnline 2026, quoted from the prize pages, mapped to what satisfies them. Deadline: Sunday 2026-09-13 12:00 EDT. Status values: `todo`, `doing`, `done` with evidence.

## Hedera: AI & Agentic Payments

Up to 3 teams, $2,000 each. Video: five minutes or less.

| Requirement, quoted | Satisfied by | Status |
|---|---|---|
| "Host a live x402-gated service on Hedera testnet or mainnet, settled through the Blocky402 facilitator." | Four seller endpoints on `hedera:testnet`, facilitator `api.testnet.blocky402.com` | todo |
| "Build a platform or agent that consumes that service and completes at least one real paid request end to end." | `mandate run` completes the normal scenario; tx id in transcript and on HCS | todo |
| "Public GitHub repo with a README covering setup, architecture, and the payment flow." | README sections Setup, Architecture, Payment flow; repo made public before submission | todo |
| "Demo video of five minutes or less showing the paid request executing." | One video, 3 to 4 minutes, shared with The Graph | todo |

Extra points pursued:

| Extra point, quoted | Satisfied by | Status |
|---|---|---|
| "Pay-per-call inference, data, or compute metering rather than a flat per-request charge" | Tariffs priced per pool, per pool-window, per input KB | todo |
| "Agent discovery via UCP, or a directory that makes your service findable by other agents" | Manifest of listings served over HTTP; every 402 carries the x402 `bazaar` discovery extension | todo |
| "HTS tokens or custom fee schedules in the settlement path" | USDC, HTS token `0.0.429274` on testnet | todo |
| "Verifiable payment audit trails on HCS" | One receipt per decision on an HCS topic | todo |

Not pursued: A2A or ACP negotiation, ERC-8004 or HCS-14 identity, Scheduled Transactions.

## The Graph: Best AI Tooling or AI Use Case, From Scratch

1st $2,500, 2nd $1,500, 3rd $1,000. Video: two to four minutes. Pool: Start Fresh.

| Requirement, quoted | Satisfied by | Status |
|---|---|---|
| "Use The Graph as a load-bearing part of the project ... the agent/app uses The Graph (Subgraphs, the Subgraph MCP, or Substreams) as its source of blockchain data." | All analysis data comes from Uniswap v3 subgraph `5zvR82QoaXYFyDEKLZ9t6v9adgnptxYpKpSbxtgVENFV` on The Graph Network; a public Ethereum RPC is used only to spot-check that cited transactions exist | todo |
| "Consume live data from a Graph provider, for example querying Subgraphs with an API key from Subgraph Studio" | Sellers query `gateway.thegraph.com` with a Subgraph Studio API key | todo |
| "Do meaningful work with the data: reasoning, decisions, automation, or a natural-language interface, not just printing a raw query result." | Materiality screening, evidence sufficiency and purchase decisions, cited explanation | todo |
| "Open-source the code with a clear README or SKILL.md so judges can run it, and submit a public repository plus a short demo video (two to four minutes)." | README Setup runnable on a clean machine; same video | todo |
| "Select the pool that matches how you built: Start Fresh for net-new" | First commit `f6cc4fa`, 2026-09-06, no prior code | done |

## Hedera: Open Source, Improve the Hedera Harness

Up to 2 teams, $1,000 each. Design in [harness.md](harness.md).

| Requirement, quoted | Satisfied by | Status |
|---|---|---|
| "Submit meaningful contribution to Hedera Harness (PR acceptable) or build new harness inspired by it" | Proctor in `harness/`, a standalone harness with the inspiration and differences documented | todo |
| "Public GitHub repo/PR with README explaining problem solved" | `harness.md` and the crate README | todo |
| "Demo video (≤5 minutes) showing improvement" | Segment of the project video | todo |

Extra points pursued: "Harness for uncovered language/framework", "New service coverage", "Tests, documentation, or examples included", "Clear before/after developer experience evidence".

## One video

ETHGlobal requires one 2 to 4 minute video per project and rejects others at upload; the sponsor limits above are looser and do not apply. One project may apply to up to 3 partner prizes, and all tracks of one partner count as one, so Mandate applies to Hedera, both tracks, and The Graph. The video must serve all three. Must show on screen: a 402 response, a payment settling with a Hedera transaction id, a live subgraph query, the decision to buy more evidence, a live quote changing the chosen plan, a refusal with its reason, a lost response recovered without a second payment, the HCS receipts, and one Proctor before and after: a failing check, the fix, a passing rerun, as two `proctor run` attempts or two `proctor check` runs.

## Sources

All quotes and facts above were verified on 2026-09-06.

- Hedera prize page: https://ethglobal.com/events/ethonline2026/prizes/hedera
- The Graph prize page: https://ethglobal.com/events/ethonline2026/prizes/the-graph
- ETHGlobal submission rules: https://ethglobal.com/events/ethonline2026/info/details
- Blocky402 testnet `/supported`, probed live: https://api.testnet.blocky402.com/supported
- Hedera exact scheme: https://github.com/x402-foundation/x402/blob/main/specs/schemes/exact/scheme_exact_hedera.md
- payment-identifier extension: https://github.com/x402-foundation/x402/blob/main/specs/extensions/payment_identifier.md
- Hedera Harness: https://github.com/hedera-dev/hedera-harness
