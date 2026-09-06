# Threat model

Scope: the buyer runtime. Sellers, the network path and the facilitator are untrusted. Trusted: the principal's pinned manifest, the facilitator's `/supported`, Hedera consensus and its mirror node, the pinned Graph deployment. References are to [spec.md](spec.md).

| Threat | Enforced by | Control | Residual loss | Ref |
|---|---|---|---|---|
| Inflated quote | buyer | quote must equal tariff price; per-payment cap | none | I2, I3 |
| Recipient or fee payer substitution | buyer | both fixed inside the signed transfer; fee payer pinned to `/supported` | none | I2 |
| Overspend across steps | buyer | cap on settled + outstanding + held; completion reserve; full-coverage bound before the first purchase | none | I1, I4, 5 |
| Double payment on retry | buyer, ledger | one payment id per purchase; resend the same signed bytes; tx id single-use on ledger | none | I6, I8 |
| Crash between sign and send | buyer | authorization persisted before send; recovery resends and reconciles | none | I8 |
| Mirror node lag or outage | buyer | absence counts only after ingestion passes expiry; otherwise exposure stays reserved | funds held until resolved | I5 |
| Prompt injection | buyer | deterministic planner; model output never sets amount, recipient or key; prose numbers checked against facts | none | I11, 8 |
| Key exposure | buyer | key exists only in the signer; never serialized; transcript prints tx ids only | none | I11 |
| Paid, no useful result | seller obligation, buyer | seller settles after work; validation; seller excluded after one failure | at most one per-payment cap per seller | 6, 8, 12 |
| Stored result replayed by a third party | seller obligation | id bound to payer, route, fingerprint and terms; retrieval needs the original signed payment; only the id hash goes on HCS | none | 12, 2.6 |
| Unfounded or miscalculated claims | buyer | structured claims; calculations re-evaluated; citations required | none | 8 |
| Stale or partial evidence | seller obligation, buyer | block range, truncated and indexing flags; `undetermined` instead of no change; freshness check | none | 7, 8 |
| Input leakage while quoting | buyer | only pinned, allowed sellers contacted; receipts hold hashes | task input seen by quoted sellers | 4, 10 |
| Facilitator misbehavior | ledger | cannot change amount or recipient; reconciliation by the runtime's own tx id | delay | I5 |
| Runaway loop | buyer | each logical purchase once; deadline | none | I6, I12 |
| Wrong or tampered manifest | trust | principal approves and pins; hash in receipt 0; quotes still checked | on-tariff payments to a wrong seller, within caps | 2.2, I3 |
| HCS publication failure | buyer | local durable receipts; retry queue; `audit_pending`; never a repurchase | audit trail delayed | I9 |

Out of scope for this build: seller identity, escrow and refunds, rate limits across mandates, mainnet key custody.
