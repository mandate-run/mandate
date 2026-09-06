# Threat model

Scope: the buyer runtime. Sellers, the network path and the facilitator are untrusted. References are to [spec.md](spec.md).

| Threat | Vector | Mitigation | Ref |
|---|---|---|---|
| Inflated quote | Seller or intermediary asks more than agreed | Quote must equal tariff-expected amount; per-payment cap | I2, I3 |
| Recipient substitution | 402 names a different `payTo` | Recipient fixed inside the signed transfer; must match manifest `pay_to` | I2 |
| Overspend across steps | Individually affordable purchases strand the task | Cap on settled + outstanding + held; completion reserve held first | I1, I4 |
| Double payment on retry | Timeout mistaken for failure | One idempotency key; no second authorization while one is unresolved; mirror node decides | I5, I6 |
| Replay | Authorization reused | Hedera tx id is single-use on ledger; validity at most 180 s | I5 |
| Prompt injection | Text in a seller response steers spending | Deterministic planner; model output never sets amount, recipient or key | I8 |
| Key exposure | Key reaches logs, model context or sellers | Key exists only in the signer; never serialized; transcript prints tx ids only | I8 |
| Paid, not delivered | Seller settles, returns nothing or garbage | Result validation; receipt `failed`; no refund path in this build | 6, 8 |
| Unfounded claims | Explanation cites events not in evidence | Every cited hash must be in purchased evidence | 8 |
| Stale evidence | Subgraph lagging | Newest block age checked against `max_data_age_s` | 8 |
| Input leakage while quoting | Unpaid probes carry the request body | Only sellers passing constraints are contacted; receipts hold hashes, not inputs | 4, 10 |
| Facilitator misbehavior | Settles late, twice, or never | Cannot change amount or recipient; runtime reconciles by its own tx id; exposure held until resolved | I5 |
| Runaway loop | Re-planning repeats a purchase | Each step bought at most once per mandate; deadline bounds the run | I6, I9 |
| Tampered manifest | Directory lists wrong URL or tariff | Manifest hash recorded in receipt 0; quotes still validated against it | I3, 10 |

Out of scope for this build: seller identity, escrow and refunds, cross-mandate rate limits, mainnet key custody.
