# Mandate runtime specification

Version 0.8, draft. MUST, MUST NOT and SHOULD follow RFC 2119. This document is authoritative; where another document disagrees, this one wins. Terms not defined here are defined in [mandate.md](mandate.md) section 13.

## 1. Scope

The behavior of a buyer runtime executing one mandate against x402 v2 sellers using the `exact` scheme on Hedera, settled by a remote facilitator. Seller obligations the runtime relies on are in section 13. Seller internals, learning across mandates and scheduling of several mandates are out of scope.

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
| coverage | enum | `all_material`: every material pool receives deep evidence. Capped coverage is not supported in this version |
| constraints.networks | list | CAIP-2 ids, e.g. `hedera:testnet` |
| constraints.facilitator | url | Facilitator base URL; its `/supported` pins the fee payer |
| constraints.manifest | path and hash | Locally approved listing manifest, pinned |
| constraints.sellers | enum | `allowlist`, `tariffed` |
| constraints.allowlist | list | Seller ids, when `allowlist` |
| constraints.max_single_payment | amount | Cap per authorization; also applied when judging plan feasibility |
| constraints.deadline | time | No authorization after this |
| constraints.eth_rpc | url | Public Ethereum JSON-RPC, used only for provenance spot-checks; required when `provenance_samples` is positive |
| requirements.evidence | enum | `screening`, `transaction` |
| requirements.citations | enum | `required`, `optional` |
| requirements.max_data_age_s | int | Max age of the newest indexed block used |
| requirements.provenance_samples | int | Cited transaction hashes checked against `eth_rpc` per report, at most the number cited. Default 3; 0 disables sampling and the report says `not_run` |
| requirements.brief_events | int | Most supporting events per pool in the explanation brief. Default 5 |
| requirements.degrade | bool | When no plan can meet coverage and evidence, run the best plan that produces at least `screening` and label the report incomplete. Default false |
| duties.receipts_topic | topic id | Configured HCS topic shared across mandates |
| duties.anchor_before_delivery | bool | Wait for HCS acknowledgement of every receipt before delivering. Default false: deliver with `audit_pending` |
| duties.report_refusals | bool | Include every refusal in the report |
| inputs | object | `pools`, `window_h`, `materiality`, `min_event_usd`, `expected_material_pools` |

A mandate that violates a field rule above is rejected at load with the field named; nothing is quoted or spent.

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
| network, asset, pay_to | Approved payment terms |
| discovery | The seller's `bazaar` info object, optional |

The tariff is a price ceiling. For `n` units, `n <= max_units`, the ceiling is `base + unit_price * n`. A quote is within tariff when its amount is at or below the ceiling for the request's unit count; a seller may compete below its ceiling. Reservations and bounds always use the ceiling. The manifest is a JSON document the principal approved and pinned by hash; the hash records which document was used and does not authenticate the seller.

### 2.3 Quote

From one 402 response: `listing_id, amount, asset, network, pay_to, fee_payer, max_timeout_s, received_at, ceiling, within_tariff, listing_match, fee_payer_ok`, plus Mandate's own request binding `method, url, body_hash`, which is local metadata and not part of x402 `ResourceInfo`. `listing_match` is true only when network, asset, `pay_to` and URL all equal the pinned listing. A quote is used only within `received_at + min(max_timeout_s, 120 s)`; `maxTimeoutSeconds` is the payment completion window and stays on the wire requirement, so this is a conservative heuristic, not a seller commitment. The runtime quotes only listings on its own network and only responses with `x402Version` 2.

### 2.4 Reservation

`id, step, amount, source (ceiling_at_max | quote), state (held | consumed | released)`.

### 2.5 Authorization

`id, quote_id, payment_id, tx_id, amount, valid_start, valid_until, signed_bytes, request, submissions, retrievals, payment_state, delivery_state, response_body, response_hash`.

- `payment_state`: `prepared | sent | settled | failed | unresolved`. Terminal: `settled`, `failed`.
- `delivery_state`: `none | received | validated | rejected`.
- `request` is the exact method, URL, headers and body sent; `signed_bytes` is the exact payload; every transmission sends these and nothing else.
- `submissions` counts transmissions made while the payment was not yet settled. `retrievals` counts transmissions made to fetch a result after settlement. Each counter is incremented and committed before the send it counts.
- `payment_id` is `pay_` plus a UUID v4, carried as `extensions["payment-identifier"].info.id`. `tx_id` is `fee_payer@valid_start`, generated by the runtime.

### 2.6 Receipt

`seq, mandate_id, step, listing_id, seller, amount, asset, tx_id, payment_id_hash, request_hash, response_hash, outcome (start | paid | refused | failed | unresolved), reason, latency_ms, at`. Receipt 0 has outcome `start` and carries `mandate_hash`, `manifest_hash` and `spec_version` instead of a purchase. The payment id itself is never published.

### 2.7 Refusal reasons

| Code | Meaning |
|---|---|
| OVER_BUDGET | amount exceeds free service budget |
| RESERVE_VIOLATION | purchase would consume the completion reserve |
| OUTSIDE_CONSTRAINTS | network, asset, seller, recipient, URL, fee payer, per-payment cap, request size or deadline |
| OFF_TARIFF | quote amount above the listing's ceiling |
| EVIDENCE_INSUFFICIENT | a pending or undetermined pool remains and no affordable purchase can resolve it |
| REQUIREMENT_UNMEETABLE | no feasible plan satisfies coverage and evidence; per-plan reasons include `brief_too_large` |
| SELLER_UNREACHABLE | no 402 within timeout during quoting |
| PAYMENT_UNRESOLVED | authorization neither settled nor failed by the deadline |

Validation failure reasons, recorded on `rejected` deliveries: `coverage`, `calculation`, `citation`, `prose`, `provenance`, `freshness`, `schema`.

## 3. Invariants

The ledger is durable, in SQLite. Every state change below is one transaction.

- I1. `settled + outstanding + held <= budget.service.total` after every change. `outstanding` sums authorizations whose `payment_state` is `prepared`, `sent` or `unresolved`; `held` sums reservations in `held`.
- I2. An authorization MUST equal its approved quote in amount, asset, network, `pay_to`, `fee_payer` and request binding. `fee_payer` MUST equal the signer the facilitator advertises for the network in `/supported`. `amount <= constraints.max_single_payment`.
- I3. A quote MUST have `listing_match` true, else OUTSIDE_CONSTRAINTS, and `within_tariff` true, else OFF_TARIFF. Only listings in the pinned manifest are quoted. Paid requests MUST NOT follow redirects.
- I4. When `reserve_completion` is true, a reservation for the final step priced at `ceiling_at_max` MUST be held before any authorization for a non-final step.
- I5. Settlement evidence is the set of consensus records for `tx_id` with nonce 0, read from the mirror node. `settled` requires a record in that set with result SUCCESS whose transfers debit the runtime account by `amount` in `asset` and credit `pay_to` by `amount`. Records with result DUPLICATE_TRANSACTION are ignored; resends produce them in normal operation. `failed` requires that no SUCCESS record exists and at least one non-duplicate record carries a failure result. An empty set by `valid_until + 30 s` is `unresolved`, and exposure is kept; nothing releases it except a later record. A receipt query is a free hint that a record exists or that the transaction failed; it is never evidence. No HTTP response changes `payment_state`.
- I6. One `payment_id` per logical purchase, reused on every submission and retrieval. A purchase whose payment settled MUST NOT acquire a second authorization because its delivery is missing.
- I7. Accounting moves are atomic transfers: `held -> outstanding` on `prepared`; `outstanding -> settled` on `settled`; `outstanding -> free` on `failed`. No amount is counted in two columns.
- I8. Only a persisted authorization is transmitted. `submissions` MUST NOT exceed 3. `retrievals` are bounded by the deadline. A counter is committed before the send it counts, so a crash between commit and send costs one attempt and never bypasses the cap. After a crash, every authorization that is not terminal, or that is `settled` with `delivery_state` `none` or `received`, resumes under section 6; nothing is re-signed.
- I9. Receipts are durable locally in `seq` order before publication. Publication failure never repeats a purchase. When `anchor_before_delivery` is false, the report is delivered with `audit_pending` listing unpublished sequence numbers.
- I10. `audit_spent <= budget.audit.total`. A refusal with zero service spend may still spend audit budget.
- I11. No amount, recipient or key material originates from model output. Every claim in a report is computed by the runtime from purchased facts; a model only turns claims into prose.
- I12. Nothing is authorized after `constraints.deadline`.
- I13. The chain pinned listing, quote, signed transfer, settlement record MUST match at every link. A broken link is refused or recorded as failed; it is never repaired by re-signing.

## 4. Quoting

1. Filter listings by `constraints` and by the capabilities the candidate plans need. Filtered listings are never contacted.
2. Quote only steps whose request body is fully known now and whose unit count is at most `max_units`. Send the real request from a client that holds no signer, never retries and never follows redirects. Record the 402 as a Quote; compute `ceiling`, `within_tariff`, `listing_match` and `fee_payer_ok`.
3. Steps whose body depends on an earlier result are estimated at the ceiling and quoted once their input exists.
4. No 402 within `quote_timeout_ms`, or any non-402 response, is SELLER_UNREACHABLE.
5. Expired quotes are re-requested, never reused.

## 5. Planning

Two sets. `R`, the required coverage, is every pool in `inputs.pools`, fixed at start. `W`, the pending work, is the pools whose outcome is not yet `non_material` or `supported`: all of `R` before screening; after screening, the pools whose outcome is `pending` or `undetermined`, section 8. Plans are chosen over `W`; the final report is validated over `R`.

| Plan | Steps | Report from |
|---|---|---|
| staged | screen unscreened pools; events for pools in `W`; explain | explain |
| hybrid | screen unscreened pools; investigate pools in `W` | investigate |
| bundle | investigate all pools in `W` | investigate |

Two numbers per plan. `expected` uses live quotes where they exist and ceilings at expected quantities otherwise; the expected quantity for events and investigate is `expected_material_pools` while nothing has been screened and `|W|` once the screen has delivered, since observed work replaces the assumption. `bound` uses live quotes where they exist and ceilings at maximum quantities otherwise, which is `|W|`. A live quote counts only while it is unexpired at planning time and bound to the exact request the step would send; an expired or mismatched quote is re-requested before the plans are compared. A plan is feasible when `bound <= budget.service.total - settled - outstanding`, no single step exceeds `max_single_payment` at its maximum quantity, and every step's input fits its listing's `max_units`. For the staged plan the last condition means the mandatory brief bound for `|R|` pools, section 8, fits the explain listing; a staged plan that fails it is rejected with reason `brief_too_large`. The hybrid and bundle plans obtain their explanation from `investigate` and are unaffected. Quantities are never reduced below `|W|` to make a plan fit.

Choose the feasible plan with the lowest `expected`. Ties go to fewer authorizations. When no plan is feasible: refuse REQUIREMENT_UNMEETABLE before any purchase, reporting the lowest `bound` among plans, the lowest `expected`, the available budget, and each plan's reason; when `requirements.degrade` is true, run instead the lowest-`expected` feasible plan producing at least `screening`, and label the report incomplete.

Re-plan after every step over the new `W`: construct the requests whose bodies are known, collect the valid quotes, compare the remaining plans, hold or replace the completion reservation, buy the chosen plan's first step, validate the delivery, recompute. A refused quote excludes its listing and returns to planning; it is a terminal refusal only when no plan remains. Hybrid names screen followed by investigate and is not a separate execution path: after the screen the two remaining jobs are events plus explanation, or investigate. The chosen plan, both numbers, the assumption used, and every rejected plan with its reason MUST appear in the transcript. A reservation is released when its step is quoted lower or is no longer needed, and replaced when the plan's final step changes.

## 6. Purchase state machine

Ordering for one purchase: hold budget durably; build and sign; persist `tx_id`, `signed_bytes` and `request` while moving `held -> outstanding`; commit the counter; send; reconcile. No send happens before its commit. Payment and delivery advance independently.

Payment transitions:

| From | Event | To | Effect |
|---|---|---|---|
| approved | payload signed; `signed_bytes`, `request`, `tx_id` persisted | prepared | reservation `consumed`; `outstanding += amount` |
| prepared | `submissions` committed as 1, then request transmitted with `PAYMENT-SIGNATURE` | sent | reconciliation every 5 s |
| prepared, sent, unresolved | I5 SUCCESS record with matching transfers | settled | `outstanding -= amount`; `settled += amount` |
| prepared, sent, unresolved | I5 failure, no SUCCESS record | failed | `outstanding -= amount`; receipt `failed` |
| prepared, sent | `valid_until + 30 s` passed, empty record set | unresolved | exposure kept; receipt `unresolved` at deadline |

Delivery transitions:

| From | Event | To | Effect |
|---|---|---|---|
| none | 2xx with body | received | persist `response_body`, `response_hash`, `PAYMENT-RESPONSE` |
| received | payment `settled` and section 9 passes | validated | purchase accepted |
| received | payment `settled` and section 9 fails | rejected | receipt `failed` with reason; any replacement is a new logical purchase with a new `payment_id`, allowed only within I1 and I4 |
| received | payment `failed` | received | anomaly `unpaid_delivery` in the report; no action |

Rules:

- Resend: `payment_state` in `prepared` or `sent`, `delivery_state` `none`, before `valid_until`, `submissions < 3`: commit `submissions + 1`, then resend `signed_bytes` with the same `payment_id`, at most once per 30 s.
- Retrieve: `payment_state` `settled`, `delivery_state` `none`, before the deadline: commit `retrievals + 1`, then send the same `PAYMENT-SIGNATURE` and `payment_id`, at most once per 30 s. Sellers serve the stored result, section 13. Retrievals are not submissions and are not capped by I8's limit of three.
- Recovery: on start, load every authorization that is not terminal, or that is `settled` with `delivery_state` `none` or `received`, and apply the rows and rules above. `none` needs a retrieval; `received` needs validation. Settlement observed before any HTTP response is the `prepared -> settled` row.
- Reconcile: `mandate reconcile <mandate_id>` re-runs I5 for every non-terminal authorization at any later time, including after the deadline. It is the only way an `unresolved` authorization changes state.

## 7. Evidence contract

Every evidence response carries `deployment_id`, `block_start`, `block_end`, `block_end_timestamp`, `indexed_block`, `indexed_block_timestamp`, `indexing_errors`, `window_requested`, `window_covered`, `coverage_shortfall`, `truncated`, and per pool the facts below. A seller MUST NOT report no material change when `truncated` or `coverage_shortfall` is true or a USD valuation needed for the verdict is null; it reports `undetermined`. A GraphQL error or a null USD field yields `undetermined`, never a zero.

Observation blocks: `block_start` and `block_end` are the last blocks at or before each window end in which subgraph state changed, read from the `transactions` entity at the indexed head; facts are stated at those blocks. An observation block may be older than the boundary it stands for; the state is the same, since nothing in the subgraph changed in between. `coverage_shortfall` is true when `indexed_block_timestamp` is before the window end, and `window_covered.to` is then the indexed head; otherwise `window_covered` equals `window_requested`. Events are selected by timestamp range and are exact regardless. Every query of one response is pinned to `indexed_block` and read from `deployment_id`; a different deployment mid-response is an error.

Screen windows are hour-aligned; an unaligned request is rejected before payment, and hourly rows then lie inside the window. Screening facts per pool: `Pool.totalValueLockedToken0`, `totalValueLockedToken1`, `totalValueLockedUSD` and `liquidity` at `block_start` and at `block_end`, with each token's USD price at those blocks; hourly `tvlUSD`, `volumeUSD` and `txCount` from `poolHourDatas` over the window; per event type the largest mint, burn or swap with `amountUSD >= inputs.min_event_usd`, or none; and whether any mint or burn in the window has a null `amountUSD`, which leaves that check incomplete. A pool absent at `block_start` is `undetermined` until events are held; absence is not a zero. The screen costs one request per pool whatever the activity. `liquidity` is in-range liquidity, reported and unused.

A pool is material when the relative change in either token TVL is at least `inputs.materiality`, or any single event has `amountUSD >= inputs.min_event_usd`. When a token's starting TVL is zero, its relative change is undefined; that token counts as material when its ending TVL is nonzero and the USD value of the change is at least `min_event_usd`. Zero material pools is a valid, complete result.

| Bought | Required | Action |
|---|---|---|
| screening | screening | proceed to explain |
| screening | transaction | buy events for pools in `W`; if none affordable, EVIDENCE_INSUFFICIENT |
| transaction | any | proceed to explain |

Events facts per pool: every mint, burn and swap in the window with `transaction.id`, `logIndex`, `timestamp`, `amount0`, `amount1`, `amountUSD`, `origin`, and for mints and burns `owner`, `tickLower`, `tickUpper`, where `logIndex` and a burn's `owner` may be null in the subgraph and a citation then rests on the transaction hash; per type the count and the summed `amountUSD`, with the number of null `amountUSD` values; paginated by id at the head block to completion or `truncated` set at the listing's cap.

## 8. Analysis

The runtime computes the following from purchased facts, deterministically, before any explanation is bought.

Per-pool outcome:

| Outcome | Meaning |
|---|---|
| non_material | facts complete and below both thresholds |
| pending | material by complete screening facts; `requirements.evidence` is `transaction` and no transaction-level evidence is held yet |
| supported | material and the held evidence meets `requirements.evidence` |
| undetermined | `truncated` or `coverage_shortfall` is true, a needed valuation is null, an event in the window has no valuation, or facts are missing; checked before materiality, so incomplete facts are never `supported` |

Under `requirements.evidence` `screening`, a material pool with complete screening facts is `supported` immediately.

Claims, each with `type`, `pool`, `values`, `calculation` and `evidence`:

| Type | Calculation | Permitted evidence |
|---|---|---|
| tvl_change | `(tvl_end - tvl_start) / tvl_start` per token from block-height facts; `tvl_end - tvl_start` when `tvl_start` is zero | two fact ids |
| large_event | `amountUSD` of one event, compared with `min_event_usd` | one transaction hash |
| largest_event | when events are held and none reaches `min_event_usd`: `amountUSD` of the largest held event, stated as below the threshold; it gives a supported pool the transaction citation section 9 requires | one transaction hash |
| activity_summary | `volumeUSD` and `txCount` summed over the window's hourly facts, and the largest event per type from screening; per-type counts and summed `amountUSD` once events are held | aggregate fact ids |

Brief format: fixed fields; decimals with at most 18 significant digits, 42-byte addresses, 66-byte hashes, RFC 3339 timestamps. A pool's outcome and its mandatory claims, at most two `tvl_change`, one `large_event` or `largest_event`, and one `activity_summary`, occupy at most 1024 bytes; the header at most 512 bytes. The mandatory brief bound is therefore `512 + 1024 * |R|` bytes. It is a feasibility condition of the staged plan, section 5, not of the mandate.

Brief content: the per-pool outcomes for `R`, the claims, and for each supported pool up to `brief_events` supporting events ordered by `amountUSD`, with fact ids and transaction hashes. `brief_events` is reduced one at a time until the brief fits; the loop ends at zero, which is valid. Coverage lives in outcomes and claims, never in the event list. The brief is the only input the explain step receives.

For a bundled `investigate` report, the runtime recomputes outcomes and claims from the delivered facts and requires equality with the seller's.

## 9. Result validation

- Coverage: every pool in `R` has an outcome. A report is complete when no pool is `pending` or `undetermined`. Otherwise it is incomplete under `degrade`, or refuses EVIDENCE_INSUFFICIENT when no affordable purchase can resolve the remaining pools.
- Claims: every `evidence` reference resolves to purchased evidence; every `calculation` re-evaluates to its value; `claim.pool` is in `R`; the evidence kind matches the table in section 8.
- Citations: when `citations` is `required`, every claim carries at least one evidence reference of its permitted kind, and every supported pool has at least one claim. A supported pool whose transaction-level evidence contains at least one event MUST have at least one claim citing a transaction hash.
- Prose: every number in the prose appears among claim values or fact values.
- Provenance: from the transaction hashes cited by claims and listed in the brief, `min(provenance_samples, hashes)` are chosen deterministically and checked with `eth_getTransactionReceipt` against `eth_rpc`; each MUST exist with status 1 and list the pool address among its log addresses. When no transaction hash is cited, provenance is `not_applicable`, whatever the setting; a report of non-material pools cites block-height facts and passes on those. When hashes are cited and `provenance_samples` is 0, provenance is `not_run` and the report says so. A positive `provenance_samples` with no `constraints.eth_rpc` fails mandate validation before any purchase; a missing RPC never downgrades a requested check. This verifies existence and pool involvement, not amounts.
- Freshness: `indexed_block_timestamp` within `requirements.max_data_age_s` of the quote's `received_at`; `indexing_errors` false; no pool is `non_material` while `coverage_shortfall` or `truncated` is true.
- Schema: the response conforms to the listing's output schema.

The guarantee this establishes: validated calculations, evidence references, coverage of the required set, and sampled provenance where transactions are cited. It does not establish causation, completeness beyond the covered window, or amounts beyond what the subgraph reports.

## 10. Hedera exact binding

Per the x402 Hedera `exact` scheme. The runtime builds a `TransferTransaction` with `transaction_id.account_id = quote.fee_payer` and `valid_start = now - 5 s`; `transaction_valid_duration = min(quote.max_timeout_s, 120)` seconds; one debit from the runtime account and one credit of `quote.amount` in `quote.asset` to `quote.pay_to`; node account ids from the mirror node `/api/v1/network/nodes`; the runtime key's signature only.

The serialized bytes, base64, form `payload.transaction` of a PaymentPayload whose `resource` and `accepted` copy the quote and whose `extensions` carry `payment-identifier`. The payload travels base64-encoded in `PAYMENT-SIGNATURE`. `PAYMENT-RESPONSE` is recorded and never trusted.

Reconciliation: a `TransactionReceiptQuery` against consensus nodes, free of charge, is the hint that a record exists or that the transaction failed. Evidence is `/api/v1/transactions/{fee_payer}-{seconds}-{nanos}` on the mirror node, which returns every record for the id; the runtime reads `result`, `nonce`, `transfers` and `token_transfers` of each. Paid record queries are not used.

The runtime payment account holds exactly `budget.service.total` of the service asset, is associated with that token when it is an HTS token, and holds `budget.audit.total` in HBAR for association and HCS fees. The facilitator pays transfer fees.

Before signing an HTS payment, the runtime requires mirror-node metadata for the exact fungible token, an absent fee-schedule key, and an explicitly empty custom-fee schedule. Mutable, fee-bearing or unverifiable tokens are refused. Reconciliation checks aggregate buyer debits as well as the expected transfer. One runtime at a time owns a ledger file; it holds an operating-system file lock until the ledger closes.

Opening an older ledger adds missing recovery columns without deleting payments or receipts. Retry timestamps are conservatively backfilled from the last stored update. If a legacy mandate has paid work but no persisted analysis window, resuming refuses to choose a new window; reconcile its payments and migrate the original window from the stored requests first.

## 11. Receipts

One HCS message per receipt: JSON, at most 1024 bytes, section 2.6 fields only. Never inputs, prompts, evidence, reports or payment ids. One configured topic; `mandate_id` and `seq` identify the run. Receipts are written to the local ledger first and published by a queue that retries; I9 governs delivery.

An audit submit's transaction id is persisted before submission. A lost acknowledgment does not authorize a replacement: the queue looks up the original transaction and matches its consensus timestamp and message bytes on the configured topic to recover the sequence number. A positive failure permits retry after fee reconciliation. An absent or ambiguous record retains the fee cap even after expiry. A confirmed success with a message not yet available leaves the receipt pending without submitting it again.

## 12. Transcript

Printed in order: mandate summary with coverage, the planning assumption and the mandatory brief bound; quotes table with listing, amount, ceiling, within_tariff, listing_match, fee_payer_ok, latency, plus ceiling estimates for unquoted steps; chosen plan with expected and bound, rejected plans with reasons; per step the `payment_id`, `tx_id`, submissions, retrievals, payment and delivery transitions with times, record counts including duplicates ignored; per-pool outcomes and claim counts; validation result including provenance samples, `not_applicable` or `not_run`; every refusal with code, needed bound, needed expected and available amounts; totals settled, released, unspent, unresolved, audit spent; HCS topic id and any `audit_pending` sequence numbers; a reconcile notice when anything is unresolved.

## 13. Seller obligations the runtime relies on

The four reference sellers are operated by the Mandate team. Any seller that meets these obligations can be listed.

- Publish a listing with a versioned tariff, refuse requests above `max_units`, and quote at or below the ceiling for the request received.
- Include the `bazaar` discovery info in every 402.
- Honor `payment-identifier` per its specification: bind the id to payer, route, query, body, payment terms and the signed transaction; return the stored result for a matching resend or retrieval without a second settlement; answer 409 when the id matches but the request or the authorization differs; run one attempt per id at a time, letting identical concurrent requests wait for the first; store a result only after a successful settlement and a successful response. Result retrieval requires the original `PAYMENT-SIGNATURE`; the id alone retrieves nothing.
- Store results durably at least until the mandate deadline. Each new purchase queries live data; only resends and retrievals are served from storage.
- Verify, then do the work, then settle. A failed handler MUST NOT settle. This limits, and does not eliminate, the loss from a seller that settles and withholds value; that loss is bounded by `max_single_payment` and ends the seller's participation for the mandate.
