# Mandate

**Give your agent a mandate, not a credit card.**

Concept document. Working name. September 2026.

This document states the idea. It is the source the README, the pitch and the demo script draw from. Implementation detail lives elsewhere. Every claim here is either backed by a cited fact or demonstrable in the reference build.

---

## Thesis

Every request on the internet is getting a price. The sellers are ready. The buyer side is thin.

An agent that can be trusted with money is the missing piece of the machine economy, and trust is a property of the buyer, not of the payment rail. Mandate is a runtime that lets an agent hold money under enforceable terms. It buys what a task needs, keeps enough to finish, refuses what it cannot justify, and puts a receipt for every decision on a public ledger.

---

## 1. The shift: a priced internet

The x402 protocol revived the HTTP 402 status code as a checkout. A server answers a request with a price and payment terms. The client pays and retries. No account, no API key, no subscription. In 2026 the protocol moved under an open foundation with Coinbase and Cloudflare among its stewards, The Graph opened a pay-per-query gateway for blockchain data, and Hedera published a native settlement scheme with a hosted facilitator.

The consequence is larger than micropayments. When any endpoint can quote a price per call, every API, model and dataset becomes a purchasable good, and every agent action becomes a purchasing decision. The web is turning into a market in which the customers are machines.

## 2. The gap: the buyer side is an SDK

The seller side is well served. Paywall middleware, facilitators that verify and settle, directories that list what is for sale, routers that pick the cheapest model. The buyer side is an SDK that pays whenever it is asked.

An agent holding a wallet today is a liability:

- It pays a forged or inflated payment request because it cannot tell a quote from a fact.
- It spends its last cent on half an answer because it never asked what finishing would cost.
- It buys the same thing twice on a retry because a timeout looked like a failure.
- It loops, and every loop is a purchase.
- It can be talked into spending by text inside a tool result.
- When any of this happens, nobody can reconstruct what was bought, from whom, or why.

One community measurement of The Graph's pay-per-query gateway, two months after launch, counted a few hundred payments totalling a few dollars. That is one data point, not proof, but it is consistent with the rail not being the bottleneck.

## 3. The idea: a mandate

In finance a mandate is what a principal gives a manager: a purpose, a budget, hard constraints and a duty to account. The manager has discretion inside the mandate and none outside it. The same instrument is what an agent needs.

A key says: spend. A mandate says: achieve this, with at most this, only in these ways, and show me what you did.

Mandate, the runtime, enforces that instrument at the moment of spending. The agent proposes purchases. The mandate disposes. The wallet signs only what the mandate has approved.

### The mandate object

```toml
[mandate]
principal = "0.0.123456"            # who funds it and who is owed the report
purpose = """
Explain any material liquidity change in the listed pools over the
last 24 hours, with transaction-level evidence."""
coverage = "all_material"           # every material pool; capped coverage comes later

[mandate.budget]
service = { total = "0.0100", asset = "USDC" }   # what sellers can be paid
audit = { total = "0.5", asset = "HBAR" }        # HCS receipts and association fees
reserve_completion = true           # never spend what finishing will cost

[mandate.constraints]
networks = ["hedera:testnet"]
facilitator = "https://api.testnet.blocky402.com"
sellers = "allowlist"               # or: any seller with a committed tariff
max_single_payment = "0.0090"
deadline = 2026-09-13T16:00:00Z

[mandate.requirements]
evidence = "transaction"
citations = "required"
max_data_age_s = 3600
degrade = false                     # never quietly deliver less than asked

[mandate.duties]
receipts_topic = "0.0.topic"        # every payment and result hash, in order
report_refusals = true              # every purchase not made, with the reason
```

### What the principal can rely on

These are testable invariants, not intentions.

1. Settled spend plus outstanding authorizations plus the completion reserve never exceeds the budget.
2. No payment leaves outside the allowed networks, assets, sellers and per-payment cap.
3. No payment is made against a quote that contradicts the seller's committed tariff.
4. The outcome is either a report that meets the stated requirements, or an explicit refusal with the reason for every purchase not made.
5. Every payment and every result hash is in the local ledger before the report is delivered and is published to the public ledger; the report lists any receipt still pending.

## 4. Three duties, three behaviors

The runtime has three duties. Each maps to a behavior the demo shows.

| Duty | Behavior shown |
|---|---|
| Buy what the task needs, from whoever quotes best right now | Normal completion: live quotes, cheapest eligible path, task finished under budget |
| Keep enough to finish | Economic adaptation: a price or requirement changes and a different eligible path wins, or a purchase is deferred to protect the finish |
| Account for everything, including what was refused | Safe incompletion: insufficient funds, insufficient evidence or an unresolved payment ends the task with reasons, not with silence |

## 5. Principles

1. **The model proposes. The mandate disposes.** No language model holds a key, sees a key, or sets an amount. The runtime computes every claim from purchased facts; a model only turns claims into prose.
2. **Quotes are facts, not metadata.** Prices come from live 402 responses to the actual request, collected by a client that cannot pay.
3. **Reserve the finish before spending on the middle.** A sequence of individually affordable purchases can strand a task. The runtime prices the final step from a committed tariff and holds that amount first.
4. **Every authorization is exposure until the ledger says otherwise.** A timeout after signing releases nothing. The buyer generated the transaction id, so it can ask the ledger directly. Absence is never proof: an authorization with no record stays reserved until a record appears.
5. **Sellers publish ceilings. Buyers hold them to it.** A tariff is a versioned price ceiling with unit definitions and an input cap. It makes future steps reservable, lets a seller compete below it, and makes any quote above it refusable.
6. **Refusal is a first-class outcome.** A refused purchase carries a reason a human can read: over budget, outside constraints, off tariff, evidence insufficient, requirement unmeetable.
7. **Receipts, not logs.** Hashes, amounts, counterparties and transaction ids go on chain. Prompts, data and results do not.
8. **Evidence over verdicts.** Report what the data shows and cite it. Do not emit a score that cannot be explained.

## 6. Anatomy of a run

The reference mission is a DeFi liquidity investigation. The evidence is live on The Graph. The payments settle on Hedera. Four services are listed, each an ordinary x402 endpoint with a published tariff. All four are reference providers operated by the Mandate team: the market is real in protocol and simulated in vendors, and the runtime is not told which is which.

| Listing | Product | Price ceiling |
|---|---|---|
| screen | Token TVL and activity at the window's ends per pool, from block-height queries | 0.0002 per pool |
| events | Mints, burns and swaps in the window with transaction hashes | 0.0015 per pool-window |
| investigate | screen plus events plus explanation, bundled | 0.0020 plus 0.0012 per pool |
| explain | Prose from a bounded brief of claims the runtime computed | 0.0001 per KB of brief, at most 8 KB |

Mandate: five pools, service budget 0.0100 USDC, coverage all material pools, per-payment cap 0.0090, transaction-level citations required, planning assumption one material pool.

The ceilings make the choice depend on what screening finds. With one to three material pools the staged path is cheapest. At four the two paths tie at the explanation's ceiling and the tie-break on fewer authorizations picks the partial bundle; a live explanation quote below the ceiling flips that. At five the full bundle wins outright.

**Normal completion**

| Step | Action | Settled | Held | Free |
|---|---|---|---|---|
| 0 | Reserve the explanation at its maximum price | 0 | 0.0008 | 0.0092 |
| 1 | Quote screen and investigate live; estimate events and explain. Staged expected 0.0033, bound 0.0093. Hybrid expected 0.0042, bound 0.0090. Bundle 0.0080. All feasible. Choose staged | 0 | 0.0008 | 0.0092 |
| 2 | Buy screen for five pools | 0.0010 | 0.0008 | 0.0082 |
| 3 | One pool material. Re-plan: events plus explain 0.0023 against a one-pool bundle 0.0032. Buy events for that pool | 0.0025 | 0.0008 | 0.0067 |
| 4 | The runtime computes outcomes and claims; the brief is 5 KB; explain quotes 0.0005. Consume the reserve, release 0.0003. Buy. Validator confirms coverage, every calculation and every citation; provenance samples transactions on Ethereum when the mandate sets `eth_rpc` and a positive `provenance_samples`, which the fixture runs do not | 0.0030 | 0 | 0.0070 |
| 5 | Receipts published. Deliver the report; 0.0070 unspent | 0.0030 | 0 | 0.0070 |

**Economic adaptation.** The bundle seller quotes 0.0030 live for five pools, below its published ceiling of 0.0080 and below the staged expectation of 0.0033. The bundle is chosen before any screening. Nothing else changed.

**Safe incompletion.** Service budget 0.0030. Before buying anything the runtime prices every plan at full coverage: the cheapest bound is the full bundle at 0.0080, the cheapest expectation is staged at 0.0033. Neither fits. It refuses with both numbers and spends nothing. If the mandate permits degrading, it buys the screen alone for 0.0010, delivers the screening facts labelled incomplete, and leaves 0.0020 unspent.

**Integrity refusal.** After the screen, the events seller quotes 0.0020 against a published ceiling of 0.0015. The quote is under budget. It is refused anyway, with the tariff version cited, and the one-pool bundle at 0.0032 becomes the cheapest path to a cited answer.

**Unresolved payment.** The events request times out after the authorization was sent. The 0.0015 stays outstanding. The runtime asks the ledger for the transaction id it generated: the consensus receipt as a hint, then every mirror node record for that id. Resends produce duplicate records, and those are ignored. If a record shows the transfer with the expected amounts, it fetches the result again with the same signed payment and payment id, and the seller returns the stored result. If the only records show failure, the amount is released. If no record appears, the amount stays reserved and the report says so; only a later reconciliation that finds a record changes that. It never pays twice and never assumes.

## 7. What already exists, and what this adds

| Prior work | What it does | What Mandate adds |
|---|---|---|
| x402 client SDKs | Pay when a 402 arrives | Judgment about whether to pay at all |
| Service directories and Bazaar | Find and rank sellers | A task budget across many purchases and the discipline to stop |
| LLM routers with x402 settlement | Pick a model per call by price and speed | Multi-step procurement of evidence, not just inference |
| AgentRouter, ETHGlobal Lisbon 2026 | Inference marketplace on Hedera, cheapest provider, budget cap, HCS log | Completion reserves, evidence sufficiency, tariff binding, refusal with reasons |
| Tally, Hedera x402 bounty 2026 | Hard ceiling, bill audit against a signed price, seller cut-off | The same discipline applied across a task with several sellers and steps |
| 402Pilot, arXiv 2026 | Learns provider value with bandits in replay | Live settlement, exposure accounting and refusal semantics for that learning to sit on |

The honest claim is narrow: the agent buys the evidence its task needs, keeps enough to finish, and explains every purchase it refused. AgentRouter and Tally each do parts of this with real settlement. We have not found one runtime that does all of it, and the claim is about the combination, not the parts.

## 8. Why Hedera and The Graph first

Hedera's x402 scheme has a property the mandate depends on. The buyer builds and signs the transfer itself, with the facilitator as fee payer, so the amount and recipient are fixed before anything leaves the buyer, and the buyer knows the transaction id before it sends. Sub-cent settlement with a facilitator paying fees makes purchases at 0.0002 sane. The consensus service gives the receipts a public, ordered, timestamped home for a fraction of a cent.

The Graph is the evidence. A liquidity investigation cannot be answered from static data or from a model's memory. Pool state, mints, burns and swaps with their transaction hashes come from live subgraph queries, and the report is only as good as its citations.

Neither is a sponsor decoration. The first mission is not possible without both.

## 9. The road

**Stage one. One mandate, one mission.** This week. One agent, four sellers, real settlement through the Hedera facilitator, live Graph evidence, public receipts, three behaviors on video.

**Stage two. An embeddable buyer.** A runtime any agent framework can host, the way every framework hosts tool calling. Any x402 seller, any supported chain, the same mandate semantics and the same guarantees.

**Stage three. Portable mandates.** The mandate becomes a signed object: hashed on chain when issued, enforceable by ledger allowances and agent identity standards, auditable by anyone holding the receipts. A principal can prove what an agent was allowed to do, and an agent can prove it stayed inside.

**Stage four. Sub-mandates.** An agent hires another agent by carving a smaller, stricter mandate out of its own. Budgets nest the way cost centers nest in a company. This is how an organization of agents spends without a human approving every cent, and it is the buyer-side counterpart to agent-to-agent protocols.

## 10. Why the industry should care

- **It makes agents fundable.** Companies gave every employee a card only after spend controls and receipts were enforced at the card. The same layer lets them give every agent a wallet.
- **It creates real markets.** Sellers compete only when buyers compare live quotes and walk away. Procurement pressure, not paywalls, turns paid APIs into a market.
- **It makes machine conclusions auditable.** "How do you know?" becomes "show me the receipts," and the receipts are literal, ordered and paid.
- **It introduces a unit.** Cost per verified conclusion: settled spend divided by conclusions whose every claim passed citation. Measured, not estimated.

## 11. What Mandate is not

- Not a marketplace. It buys from markets. It does not run one.
- Not a router. Choosing a provider is one small step inside a purchase decision.
- Not a payment protocol, a chain, a token or a model.
- Not a seller. The reference sellers exist to make the buyer demonstrable.

## 12. Open questions

- **Quality beyond citations.** Citation validity proves a claim is grounded, not that it is right or complete. Task-specific validators and post-payment feedback belong above the runtime.
- **Tariff standardization.** A committed tariff should be an x402 extension the seller advertises in the 402 itself, not a convention of one buyer.
- **Non-tariffed sellers.** They can be quoted but not reserved against. The runtime should say so and treat them as last-step purchases only.
- **Write-off policy.** An authorization that never produces a record stays reserved forever under this design. A principal may want to accept that loss after a delay. That is a policy about risk, not a proof of nonpayment, and it is deliberately absent from the first version.
- **Disputes.** A seller that takes payment and fails to deliver is recorded and avoided. Refunds and escrow are out of scope until a settlement scheme supports them cleanly.
- **Cross-chain mandates.** One budget across several rails needs a common unit and exposure accounting per rail.
- **Learning.** Which sellers deserve trust over time is a learning problem. The runtime provides the ground truth, receipts and outcomes, for that layer to learn from.
- **Privacy during quoting.** The task input reaches every seller quoted. Quote only allowlisted sellers, and put input hashes rather than inputs in receipts.

## 13. Glossary

- **Mandate.** Purpose, budget, constraints, requirements and duties issued by a principal to an agent.
- **Listing.** A purchasable service with a capability, a tariff and payment terms, published in a manifest.
- **Seller offer.** A quote the seller has signed under the x402 offer-receipt extension.
- **Seller receipt.** A signed statement from the seller that a payment was received for a resource.
- **Payment id.** The client-generated identifier reused across retries, per the x402 payment-identifier extension.
- **Quote.** The price and terms in a live 402 response to a specific request.
- **Tariff.** A versioned price ceiling with unit definitions and an input cap that a seller publishes in advance.
- **Claim.** A typed statement the runtime computes from purchased facts, with its calculation and evidence references.
- **Brief.** The bounded input to the explanation step: outcomes, claims and a few supporting events.
- **Reservation.** Budget held for a future step, priced from a tariff at the input cap.
- **Authorization.** A signed payment handed to a seller and not yet confirmed by the ledger.
- **Settlement.** The ledger record that the payment executed.
- **Receipt.** The buyer's on-chain record of one decision: counterparty, amount, transaction id, payment id hash, request hash, result hash, outcome.
- **Coverage.** The requirement that every material pool receives deep evidence. A capped form is a later option.
- **Outcome.** The runtime's verdict on one pool: non_material, pending, supported or undetermined.
- **Audit budget.** HBAR set aside for receipts and token association, bounded separately from what sellers can be paid.
- **Refusal.** A purchase not made, with a machine-readable and human-readable reason.

## 14. Lines for reuse

One paragraph:

> Every request on the internet is getting a price, and agents are becoming the customers. Sellers have checkouts. Agents have keys, and a key is a liability. Mandate gives an agent a budget, hard limits and a duty to account, and enforces them at the moment of spending. It buys the evidence a task needs, keeps enough to finish, refuses what it cannot justify, and puts a receipt for every decision on a public ledger. First mission: explain a DeFi liquidity event with live evidence from The Graph, paid for on Hedera, for less than a cent.

Short lines:

- Give your agent a mandate, not a credit card.
- x402 built the checkout. Mandate built the buyer.
- Agents that know what they can afford to finish.
- Every claim has a receipt.
- The buyer side of the machine economy.

---

## Sources

- Hedera x402 exact scheme specification: https://github.com/x402-foundation/x402/blob/main/specs/schemes/exact/scheme_exact_hedera.md
- Blocky402 facilitator, Hedera networks: https://blocky402.com/docs/networks/
- Hedera prize track, ETHOnline 2026: https://ethglobal.com/events/ethonline2026/prizes/hedera
- The Graph prize track, ETHOnline 2026: https://ethglobal.com/events/ethonline2026/prizes/the-graph
- The Graph forum, community measurement of x402 gateway adoption, July 2026: https://forum.thegraph.com/t/whats-actually-blocking-x402-adoption-on-the-graph-awareness-discovery-and-one-round-trip-payments/7009
- AgentRouter, ETHGlobal Lisbon 2026: https://ethglobal.com/showcase/agentrouter-deqhv
- Tally, Hedera x402 bounty: https://github.com/Madhav-Gupta-28/Tally
- Hedera x402 bounty winners, August 2026: https://hedera.com/blog/x402-bounty-on-hedera-winners-announced/
- 402Pilot, arXiv, August 2026: https://arxiv.org/abs/2608.01341
- r402 Rust SDK with Hedera support: https://github.com/qntx/r402
