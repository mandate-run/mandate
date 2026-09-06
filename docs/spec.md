# Mandate runtime specification

Version 0.3, draft. MUST, MUST NOT and SHOULD follow RFC 2119. This document is authoritative; where another document disagrees, this one wins. Terms not defined here are defined in [mandate.md](mandate.md) section 13.

## 1. Scope

The behavior of a buyer runtime executing one mandate against x402 v2 sellers using the `exact` scheme on Hedera, settled by a remote facilitator. Seller obligations the runtime relies on are in section 12. Seller internals, learning across mandates and scheduling of several mandates are out of scope.

Protocol references: x402 v2 core and HTTP transport; the Hedera `exact` scheme; the extensions `bazaar` and `payment-identifier`. The `offer-receipt` extension is deferred until the purchase and recovery path works.

## 2. Objects

Protocol amounts are integers in the asset's atomic unit; USDC has 6 decimals, HBAR has 8. Documents and transcripts show decimal strings. Times are RFC 3339 UTC. Hashes are SHA-256 hex.

### 2.1 Mandate

| Field | Type | Meaning |
|---|---|---|
| id | string | Unique per run |
| principal | account id | Funder and recipient of the report |
| purpose | string | The task in one sentence |
| budget.service.total | amount | Hard cap on service exposure: settled + outstanding + held |
| budget.service.asset | token id | Payment asset; `0.0.0` is HBAR |
| budget.audit.total | amount | Hard cap on HBAR spent on HCS messages and token association |
| budget.reserve_completion | bool | Hold the final step before discretionary spend |
| coverage | enum or int | `all_material`, or `max_pools: N`, the most pools the mandate authorizes deep evidence for |
| constraints.networks | list | CAIP-2 ids, e.g. `hedera:testnet` |
| constraints.facilitator | url | Facilitator base URL; its `/supported` pins the fee payer |
| constraints.manifest | path and hash | Locally approved listing manifest, pinned |
| constraints.sellers | enum | `allowlist`, `tariffed` |
| constraints.allowlist | list | Seller ids, when `allowlist` |
| constraints.max_single_payment | amount | Cap per authorization; also applied when judging plan feasibility |
| constraints.deadline | time | No authorization after this |
| requirements.evidence | enum | `screening`, `transaction` |
| requirements.citations | enum | `required`, `optional` |
| requirements.max_data_age_s | int | Max age of the newest indexed block used |
| requirements.degrade | bool | When no plan can meet coverage and evidence, run the best plan that produces at least `screening` and label the report incomplete. Default false |
| duties.receipts_topic | topic id | Configured HCS topic shared across mandates |
| duties.anchor_before_delivery | bool | Wait for HCS acknowledgement of every receipt before delivering. Default false: deliver with `audit_pending` |
| duties.report_refusals | bool | Include every refusal in the report |
| inputs | object | `pools`, `window_h`, `materiality`, `min_event_usd`, `expected_material_pools` |

`inputs.expected_material_pools` is a declared planning assumption, default 1. It affects plan choice, never feasibility.

### 2.2 Listing, one manifest entry

| Field | Meaning |
|---|---|
| id, seller | Stable ids |
| url, method | Endpoint |
| capability | `screen`, `events`, `investigate`, `explain` |
| produces | `screening`, `transaction`, `report` |
| tariff.version | Changes whenever any tariff field changes |
| tariff.base | Fixed component, atomic units |
| tariff.unit | `pool`, `pool_window`, `input_kb` |
| tariff.unit_price | Atomic units per unit |
| tariff.max_units | Largest request the seller accepts; larger requests are refused, never clamped |
| tariff.rounding | How fractional units count; `input_kb` rounds the request body up to whole KB |
| network, asset, pay_to | Expected payment terms |
| discovery | The seller's `bazaar` info object, optional |

Price for `n` units, `n <= max_units`, is `base + unit_price * n`. A quote is on tariff when its amount equals that price exactly. The manifest is a JSON document the principal approved and pinned by hash; the hash records which document was used and does not authenticate the seller.

### 2.3 Quote

From one 402 response: `listing_id, amount, asset, network, pay_to, fee_payer, max_timeout_s, received_at, expected_amount, on_tariff, fee_payer_ok`, plus Mandate's own request binding `method, url, body_hash`, which is local metadata and not part of x402 `ResourceInfo`. A quote is used only within `received_at + max_timeout_s`; `maxTimeoutSeconds` is the payment completion window, so this is a conservative heuristic, not a seller commitment.

### 2.4 Reservation

`id, step, amount, source (tariff_at_max | quote), state (held | consumed | released)`.

### 2.5 Authorization

`id, quote_id, payment_id, tx_id, amount, valid_start, valid_until, signed_bytes, state (prepared | sent | delivered | settled | expired | unresolved)`.

`payment_id` is `pay_` plus a UUID v4, carried as `extensions["payment-identifier"].info.id` in the PaymentPayload. `tx_id` is `fee_payer@valid_start`, generated by the runtime. `signed_bytes` is the exact payload sent; a resend transmits these bytes and nothing else.

### 2.6 Receipt

`seq, mandate_id, step, listing_id, seller, amount, asset, tx_id, payment_id_hash, request_hash, response_hash, outcome (paid | refused | failed | unresolved), reason, latency_ms, at`. Receipt 0 carries `mandate_hash`, `manifest_hash` and `spec_version` instead of a purchase. The payment id itself is never published.

### 2.7 Refusal reasons

| Code | Meaning |
|---|---|
| OVER_BUDGET | amount exceeds free service budget |
| RESERVE_VIOLATION | purchase would consume the completion reserve |
| OUTSIDE_CONSTRAINTS | network, asset, seller, fee payer, per-payment cap, request size or deadline |
| OFF_TARIFF | quote amount differs from expected amount |
| EVIDENCE_INSUFFICIENT | result below `requirements.evidence` and no affordable upgrade |
| REQUIREMENT_UNMEETABLE | no feasible plan satisfies coverage and evidence |
| SELLER_UNREACHABLE | no 402 within timeout during quoting |
| PAYMENT_UNRESOLVED | authorization neither settled nor provably unexecuted at deadline |

## 3. Invariants

The ledger is durable, in SQLite. Every state change below is one transaction.

- I1. `settled + outstanding + held <= budget.service.total` after every change. `outstanding` sums authorizations in `prepared`, `sent`, `delivered` or `unresolved`; `held` sums reservations in `held`.
- I2. An authorization MUST equal its approved quote in amount, asset, network, pay_to, fee_payer and request binding. `fee_payer` MUST equal the signer the facilitator advertises for the network in `/supported`. `amount <= constraints.max_single_payment`.
- I3. A quote MUST be on tariff or it is refused OFF_TARIFF. Only listings in the pinned manifest are quoted.
- I4. When `reserve_completion` is true, a reservation for the final step priced at `tariff_at_max` MUST be held before any authorization for a non-final step.
- I5. `settled` requires ledger evidence: a receipt query returning SUCCESS for `tx_id`, or a mirror node record for `tx_id` with result SUCCESS and transfers matching amount, asset and `pay_to`. `expired` requires both that `valid_until` has passed and that the mirror node's latest ingested consensus timestamp is later than `valid_until` with no record for `tx_id`. Anything else is `unresolved` and keeps its exposure. No HTTP response changes this.
- I6. One `payment_id` per logical purchase, reused on every retry. A logical purchase whose payment settled MUST NOT acquire a second authorization because its response is missing.
- I7. Accounting moves are atomic transfers: `held -> outstanding` when an authorization is prepared, `outstanding -> settled` on I5 evidence, `outstanding -> free` on `expired`. No amount is counted in two columns.
- I8. Only a persisted authorization is transmitted. After a crash, every authorization in `prepared` or `sent` is resent from `signed_bytes` and reconciled; nothing is re-signed.
- I9. Receipts are durable locally in `seq` order before publication. Publication failure never repeats a purchase. When `anchor_before_delivery` is false, the report is delivered with `audit_pending` listing unpublished sequence numbers.
- I10. `audit_spent <= budget.audit.total`. A refusal with zero service spend may still spend audit budget.
- I11. No amount, recipient or key material originates from model output.
- I12. Nothing is authorized after `constraints.deadline`.

## 4. Quoting

1. Filter listings by `constraints` and by the capabilities the candidate plans need. Filtered listings are never contacted.
2. Quote only steps whose request body is fully known now and whose unit count is at most `max_units`. Send the real request from a client that holds no signer and never retries. Record the 402 as a Quote; compute `expected_amount`, `on_tariff` and `fee_payer_ok`.
3. Steps whose body depends on an earlier result are estimated from the tariff and quoted once their input exists.
4. No 402 within `quote_timeout_ms`, or any non-402 response, is SELLER_UNREACHABLE.
5. Expired quotes are re-requested, never reused.

## 5. Planning

Remaining work is the set of pools still needing evidence, `P`, and whether a report is still owed. Before screening, `P` is every pool. After screening, `P` is the material pools. Candidate plans over remaining work:

| Plan | Steps | Report from |
|---|---|---|
| staged | screen unscreened pools; events for material pools; explain | explain |
| hybrid | screen unscreened pools; investigate material pools | investigate |
| bundle | investigate all pools in `P` | investigate |

Two numbers per plan. `expected` prices unknown quantities with `expected_material_pools`. `bound` prices them with the coverage maximum: `|P|` under `all_material`, else `min(|P|, max_pools)`. A plan is feasible when `bound <= budget.service.total - settled - outstanding` and no single step exceeds `max_single_payment` at its maximum quantity. Quantities are never reduced below the coverage maximum to make a plan fit.

Choose the feasible plan with the lowest `expected`. Ties go to fewer authorizations. When no plan is feasible: refuse REQUIREMENT_UNMEETABLE before any purchase, reporting the lowest `bound` among plans, the lowest `expected`, and the available budget; when `requirements.degrade` is true, run instead the lowest-`expected` feasible plan producing at least `screening`, and label the report incomplete.

Re-plan after every step over the new remaining work. The chosen plan, both numbers, the assumption used, and every rejected plan with its reason MUST appear in the transcript. A reservation is released when its step is quoted lower or is no longer needed.

## 6. Purchase state machine

Ordering for one purchase: hold budget durably; build and sign; persist `tx_id`, `signed_bytes` and the request binding while moving `held -> outstanding`; send; reconcile. The send happens only after the persist commits.

| From | Event | To | Effect |
|---|---|---|---|
| quoted | I1 to I4 and I12 pass | approved | reservation `held` for `amount` unless one exists |
| approved | payload signed and persisted | prepared | reservation `consumed`; `outstanding += amount` |
| prepared | request sent with `PAYMENT-SIGNATURE` | sent | start reconciliation: receipt query, then mirror node every 5 s |
| sent | 2xx with body | delivered | store `response_hash` and `PAYMENT-RESPONSE`; keep reconciling |
| sent | timeout or 5xx | sent | keep reconciling; resend `signed_bytes` with the same `payment_id` at most once per 30 s while `valid_until` has not passed |
| prepared, sent, delivered | I5 evidence of settlement | settled | `outstanding -= amount`; `settled += amount`; if no body, fetch with the same `PAYMENT-SIGNATURE` and `payment_id` |
| prepared, sent, delivered | I5 conditions for expiry | expired | `outstanding -= amount`; receipt `failed` if delivered, else may re-quote |
| prepared, sent, delivered | deadline reached, neither above | unresolved | exposure kept; receipt `unresolved`; report says so |
| settled with body | section 8 passes | accepted | |
| settled with body | section 8 fails | rejected | receipt `failed`; no second authorization for this purchase |

Recovery after a crash: load every authorization not in a terminal state and resume its row above. Settlement observed before any HTTP response is the `prepared -> settled` row.

## 7. Evidence contract

Every evidence response carries `deployment_id`, `block_start`, `block_end`, `block_end_timestamp`, `indexing_errors`, `window_requested`, `window_covered`, `truncated`, and per pool the facts below. A seller MUST NOT report no material change when `truncated` is true or a USD valuation needed for the verdict is null; it reports `undetermined`.

Screening facts per pool: `Pool.totalValueLockedToken0`, `totalValueLockedToken1`, `totalValueLockedUSD` and `liquidity` queried at `block_start` and at `block_end` using block-height queries; mint, burn and swap counts and summed `amountUSD` within the window from event queries. Block heights are the last block at or before each window end; the covered window is their timestamps. `liquidity` is in-range liquidity, reported and unused.

A pool is material when the relative change in either token TVL is at least `inputs.materiality`, or any single event has `amountUSD >= inputs.min_event_usd`. Zero material pools is a valid, complete result.

| Bought | Required | Action |
|---|---|---|
| screening | screening | proceed to explain |
| screening | transaction | buy events for material pools; if none affordable, EVIDENCE_INSUFFICIENT |
| transaction | any | proceed to explain |

Events facts per pool: every mint, burn and swap in the window with `transaction.id`, `logIndex`, `timestamp`, `amount0`, `amount1`, `amountUSD`, `origin`, and for mints and burns `owner`, `tickLower`, `tickUpper`; paginated to completion or `truncated` set.

## 8. Result validation

An explanation is a structured document: a list of claims, each with `type` (`tvl_change`, `large_event`, `activity_summary`), `pool`, `values` taken from facts, `calculation` as an arithmetic expression over fact values, and `evidence` as transaction hashes or fact ids, followed by prose.

- Every `evidence` hash MUST be present in purchased evidence; every fact id MUST resolve.
- Every `calculation` MUST evaluate to the claimed value from the referenced facts.
- When `citations` is `required`, there MUST be at least one claim and every claim MUST carry at least one evidence reference.
- Every number in the prose MUST appear among claim values or fact values.
- `block_end_timestamp` MUST be within `requirements.max_data_age_s` of the quote's `received_at`, and `indexing_errors` MUST be false.
- The response MUST conform to the listing's output schema.

The guarantee this establishes: validated calculations and evidence references. It does not establish causation or completeness beyond the covered window.

## 9. Hedera exact binding

Per the x402 Hedera `exact` scheme. The runtime builds a `TransferTransaction` with `transaction_id.account_id = quote.fee_payer` and `valid_start = now - 5 s`; `transaction_valid_duration = min(quote.max_timeout_s, 120)` seconds; one debit from the runtime account and one credit of `quote.amount` in `quote.asset` to `quote.pay_to`; node account ids from the mirror node `/api/v1/network/nodes`; the runtime key's signature only.

The serialized bytes, base64, form `payload.transaction` of a PaymentPayload whose `resource` and `accepted` copy the quote and whose `extensions` carry `payment-identifier`. The payload travels base64-encoded in `PAYMENT-SIGNATURE`. `PAYMENT-RESPONSE` is recorded and never trusted.

Reconciliation sources: a `TransactionReceiptQuery` for `tx_id` against consensus nodes, available for about three minutes after consensus and free of charge; then `/api/v1/transactions/{fee_payer}-{seconds}-{nanos}` on the mirror node, checking `result` and `transfers` or `token_transfers`. Mirror ingestion progress is the `consensus_timestamp` of the latest transaction the mirror node returns.

The runtime payment account holds exactly `budget.service.total` of the service asset, is associated with that token when it is an HTS token, and holds `budget.audit.total` in HBAR for association and HCS fees. The facilitator pays transfer fees.

## 10. Receipts

One HCS message per receipt: JSON, at most 1024 bytes, section 2.6 fields only. Never inputs, prompts, evidence, reports or payment ids. One configured topic; `mandate_id` and `seq` identify the run. Receipts are written to the local ledger first and published by a queue that retries; I9 governs delivery.

## 11. Transcript

Printed in order: mandate summary with coverage and the planning assumption; quotes table with listing, amount, expected, on_tariff, fee_payer_ok, latency, plus tariff estimates for unquoted steps; chosen plan with expected and bound, rejected plans with reasons; per step the `payment_id`, `tx_id`, state transitions with times and evidence source, validation result; every refusal with code, needed bound, needed expected and available amounts; totals settled, released, unspent, audit spent; HCS topic id and any `audit_pending` sequence numbers.

## 12. Seller obligations the runtime relies on

The four reference sellers are operated by the Mandate team. Any seller that meets these obligations can be listed.

- Publish a listing with a versioned tariff, refuse requests above `max_units`, and quote exactly the tariff price for the request received.
- Include the `bazaar` discovery info in every 402.
- Honor `payment-identifier` per its specification: bind the id to payer, route, request fingerprint and payment terms; return the stored result for a matching retry without a second settlement; answer 409 when the id matches but the request differs. Result retrieval requires the original `PAYMENT-SIGNATURE`; the id alone retrieves nothing.
- Store results durably at least until the mandate deadline. Each new purchase queries live data; only retries are served from storage.
- Verify, then do the work, then settle. A failed handler MUST NOT settle. This limits, and does not eliminate, the loss from a seller that settles and withholds value; that loss is bounded by `max_single_payment` and ends the seller's participation for the mandate.
