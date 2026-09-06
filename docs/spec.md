# Mandate runtime specification

Version 0.1, draft. MUST, MUST NOT and SHOULD follow RFC 2119. Terms not defined here are defined in [mandate.md](mandate.md) section 13.

## 1. Scope

The behavior of a buyer runtime executing one mandate against x402 v2 sellers using the `exact` scheme on Hedera, settled by a remote facilitator. Seller internals, learning across mandates and multi-mandate scheduling are out of scope.

## 2. Objects

Amounts are decimal strings in the asset's smallest unit. Times are RFC 3339 UTC. Hashes are SHA-256 hex.

### 2.1 Mandate

| Field | Type | Meaning |
|---|---|---|
| id | string | Unique per run |
| principal | account id | Funder and recipient of the report |
| purpose | string | The task in one sentence |
| budget.total | amount | Hard cap on all exposure |
| budget.asset | token id | Payment asset; `0.0.0` is HBAR |
| budget.reserve_completion | bool | Hold the final step before discretionary spend |
| constraints.networks | list | CAIP-2 ids, e.g. `hedera:testnet` |
| constraints.sellers | enum | `allowlist`, `tariffed`, `any` |
| constraints.allowlist | list | Seller ids, when `allowlist` |
| constraints.max_single_payment | amount | Cap per authorization |
| constraints.deadline | time | No authorization after this |
| requirements.evidence | enum | `screening`, `transaction` |
| requirements.citations | enum | `required`, `optional` |
| requirements.max_data_age_s | int | Max age of the newest indexed block used |
| duties.receipts_topic | topic id | HCS topic for receipts; created if absent |
| duties.report_refusals | bool | Include every refusal in the report |
| inputs | object | Task inputs: pools, window, materiality, min_event_usd |

### 2.2 Offer, one manifest entry

| Field | Meaning |
|---|---|
| id, seller | Stable ids |
| url, method | Endpoint |
| capability | `screen`, `events`, `investigate`, `explain` |
| produces | `screening`, `transaction`, `report` |
| tariff.version | Changes whenever any tariff field changes |
| tariff.base | Fixed component |
| tariff.unit | `pool`, `pool_window`, `input_kb` |
| tariff.unit_price | Price per unit |
| tariff.input_cap | Maximum units the seller will price |
| network, asset, pay_to | Expected payment terms |

Expected price for `n` units is `base + unit_price * min(n, input_cap)`. A quote is on tariff when its amount equals the expected price for the request's unit count exactly.

### 2.3 Quote

From one 402 response: `offer_id, amount, asset, network, pay_to, fee_payer, max_timeout_s, resource.url, resource.method, resource.body_hash, received_at, expected_amount, on_tariff`.

### 2.4 Reservation

`id, step, amount, source (tariff_at_cap | quote), state (held | consumed | released)`.

### 2.5 Authorization

`id, quote_id, idempotency_key, tx_id, amount, valid_start, valid_until, state (sent | settled | expired | unknown)`. The runtime generates `tx_id` as `fee_payer@valid_start` before sending.

### 2.6 Receipt

`seq, mandate_id, step, offer_id, seller, amount, asset, tx_id, request_hash, response_hash, outcome (paid | refused | failed), reason, latency_ms, at`. Receipt 0 carries `mandate_hash` and `spec_version` instead of a purchase.

### 2.7 Refusal reasons

| Code | Meaning |
|---|---|
| OVER_BUDGET | amount exceeds free budget |
| RESERVE_VIOLATION | purchase would consume the completion reserve |
| OUTSIDE_CONSTRAINTS | network, asset, seller, cap or deadline |
| OFF_TARIFF | quote amount differs from expected amount |
| EVIDENCE_INSUFFICIENT | result below `requirements.evidence` and no affordable upgrade |
| REQUIREMENT_UNMEETABLE | no eligible offer can satisfy a requirement |
| SELLER_UNREACHABLE | no 402 within timeout during quoting |
| PAYMENT_UNRESOLVED | authorization state unknown at deadline |

## 3. Invariants

The ledger is durable, in SQLite. Every state change below is one transaction.

- I1. `settled + outstanding + held <= budget.total` after every change. `outstanding` sums authorizations in `sent` or `unknown`; `held` sums reservations in `held`.
- I2. An authorization MUST equal its approved quote in amount, asset, network, pay_to, fee_payer and resource.
- I3. A tariffed offer's quote MUST be on tariff or it is refused OFF_TARIFF. Untariffed offers are eligible only when `constraints.sellers` is `any`, and only for the final step.
- I4. When `reserve_completion` is true, a reservation for the final step priced at `tariff_at_cap` MUST be held before any authorization for a non-final step.
- I5. An authorization leaves `outstanding` only when the mirror node returns its `tx_id`, then `settled`, or when `valid_until + 30 s` has passed and the mirror node returns nothing, then `expired`. No HTTP response changes this.
- I6. One idempotency key per logical request. A retry MUST NOT create a second authorization while one for the same key is `sent` or `unknown`.
- I7. Receipts MUST be appended in `seq` order. The report MUST NOT be delivered before HCS acknowledges the last receipt.
- I8. No amount, recipient or key material originates from model output.
- I9. Nothing is authorized after `constraints.deadline`.

## 4. Quoting

1. Filter offers by `constraints` and by the capabilities the candidate plans need. Filtered offers are never contacted.
2. Send each remaining offer the real request from a client that holds no signer and never retries. Record the 402 as a Quote. Compute `expected_amount` and `on_tariff`.
3. No 402 within `quote_timeout_ms`, or any non-402 response, is SELLER_UNREACHABLE.
4. A quote expires at `received_at + max_timeout_s`. Expired quotes are re-requested, never reused.

## 5. Planning

A plan is an ordered list of steps, each bound to one capability and, once quoted, one offer.

| Plan | Steps |
|---|---|
| staged | screen all pools, events for material pools, explain |
| bundled | investigate all pools |

Expected cost is the sum of known quotes plus `tariff_at_cap` for unquoted future steps. Choose the lowest expected cost among plans whose final product satisfies `requirements`. Ties go to fewer authorizations. The chosen plan, its expected cost and every rejected plan MUST appear in the transcript. Re-plan after every step with the actual result. A reservation is released when its step is quoted lower or is no longer needed.

## 6. Purchase state machine

| From | Event | To | Effect |
|---|---|---|---|
| quoted | I1 to I4 and I9 pass | approved | hold `amount` unless already reserved |
| quoted | any check fails | refused | receipt with reason |
| approved | transfer signed, request sent with `PAYMENT-SIGNATURE` | sent | `outstanding += amount` |
| sent | 2xx with body | settled | `settled += amount`, `outstanding -= amount`, store `response_hash` |
| sent | timeout or 5xx | unknown | poll mirror node for `tx_id` every 5 s |
| unknown | mirror node shows `tx_id` | settled | re-fetch result with the same idempotency key |
| unknown | `valid_until + 30 s` passed, no record | expired | `outstanding -= amount`; may re-quote |
| settled | section 8 passes | accepted | |
| settled | section 8 fails | rejected | receipt `failed`; upgrade only within I1 and I4 |

## 7. Evidence sufficiency

| Bought | Required | Action |
|---|---|---|
| screening | screening | proceed to explain |
| screening | transaction | buy events for material pools; if unaffordable, EVIDENCE_INSUFFICIENT |
| transaction | any | proceed to explain |

A pool is material when `abs(liquidity_end - liquidity_start) / liquidity_start >= inputs.materiality` or any single event has `amountUSD >= inputs.min_event_usd`.

## 8. Result validation

- Every transaction hash cited in an explanation MUST be present in purchased evidence. One miss fails.
- The newest block timestamp in evidence MUST be within `requirements.max_data_age_s` of the quote's `received_at`.
- The response MUST conform to the offer's output schema.

## 9. Hedera exact binding

Per the x402 Hedera `exact` scheme. The runtime builds a `TransferTransaction` with `transaction_id.account_id = quote.fee_payer` and `valid_start = now`; `transaction_valid_duration = min(quote.max_timeout_s, 180)` seconds; one debit from the runtime account and one credit of `quote.amount` in `quote.asset` to `quote.pay_to`; node account ids from the network address book; the runtime key's signature only. The serialized bytes, base64, form `payload.transaction` of a `PaymentPayload` whose `resource` and `accepted` copy the quote. The payload travels base64-encoded in `PAYMENT-SIGNATURE`. Any settlement data the seller returns is recorded but never trusted; I5 governs.

The runtime account holds exactly `budget.total` in `budget.asset` and is associated with that token. The facilitator pays network fees.

## 10. Receipts

One HCS message per receipt: JSON, at most 1024 bytes, section 2.6 fields only. Never inputs, prompts, evidence or reports. One topic per mandate, created at start; its id appears in the transcript and the report.

## 11. Transcript

Printed in order: mandate summary; quotes table with offer, amount, expected, on_tariff, latency; chosen plan, expected cost, rejected plans; per step the `tx_id`, settlement state, latency, validation result; every refusal with code, needed and available amounts; totals settled, released, returned; HCS topic id.
