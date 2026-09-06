# Threat model

Scope: the buyer runtime. Sellers, the network path and the facilitator are untrusted. Trusted: the principal's pinned manifest, the facilitator's `/supported`, Hedera consensus and its mirror node, the pinned Graph deployment, the public Ethereum RPC used for spot-checks. References are to [spec.md](spec.md).

| Threat | Enforced by | Control | Residual loss | Ref |
|---|---|---|---|---|
| Inflated quote | buyer | quote at or below the listing's ceiling; per-payment cap | none | I3 |
| Quote not matching the approved listing | buyer | recipient, asset, network and URL must equal the pinned listing; paid requests never follow redirects | none | I3, I13 |
| Recipient or fee payer substitution in the signed transfer | buyer | both fixed inside the transfer the buyer signs; fee payer pinned to `/supported` | none | I2 |
| Facilitator submits a different transaction under the reserved id | buyer, ledger | settlement requires a record whose transfers match debit, credit, asset and amount; a receipt status is never evidence | misaccounting prevented; delay | I5 |
| Overspend across steps | buyer | cap on settled + outstanding + held; completion reserve; full-coverage bound before the first purchase | none | I1, I4, 5 |
| Double payment on retry | buyer, ledger | one payment id per purchase; resend the same signed bytes; submissions capped and committed before sending; tx id single-use on ledger | none | I6, I8 |
| Crash between sign and send, after settlement before saving the result, or after saving before validation | buyer | signed bytes, request and response persisted; payment and delivery tracked separately; recovery resends, fetches or validates | none | I8, 6 |
| No ledger record after expiry | buyer | stays unresolved and reserved; only a later record changes it | funds held until a record appears | I5 |
| Duplicate records from resends | buyer | every record for the id is examined; DUPLICATE_TRANSACTION ignored; only a non-duplicate failure is terminal | none | I5 |
| Mirror node lag or outage | buyer | receipt query as hint; absence never counts as proof | delay | I5 |
| Prompt injection | buyer | deterministic planner; claims computed by the runtime; model output never sets amount, recipient or key; prose numbers checked against claims | none | I11, 8, 9 |
| Key exposure | buyer | key exists only in the signer; never serialized; transcript prints tx ids only | none | I11 |
| Paid, no useful result | seller obligation, buyer | seller settles after work; validation; seller excluded after one failure | at most one per-payment cap per seller | 6, 9, 13 |
| Stored result replayed by a third party | seller obligation | id bound to payer, route, fingerprint and terms; retrieval needs the original signed payment; only the id hash goes on HCS | none | 13, 2.6 |
| Fabricated but internally consistent facts | buyer, trust | calculations re-evaluated; citations resolved; sampled transaction hashes checked against Ethereum for existence and pool involvement | one purchase per seller; amounts beyond the samples rest on the subgraph and the seller | 8, 9 |
| Incomplete investigation presented as complete | buyer | coverage validated over the original required set; pending and undetermined pools make the report incomplete or refuse | none | 5, 9 |
| Stale or partial evidence | seller obligation, buyer | block range, truncated and indexing flags; `undetermined` instead of no change; freshness check | none | 7, 9 |
| Input leakage while quoting | buyer | only pinned, allowed sellers contacted; receipts hold hashes | task input seen by quoted sellers | 4, 11 |
| Runaway loop | buyer | each logical purchase once; deadline | none | I6, I12 |
| Wrong or tampered manifest | trust | principal approves and pins; hash in receipt 0; quotes still checked | on-tariff payments to a wrong seller, within caps | 2.2, I3 |
| HCS publication failure | buyer | local durable receipts; retry queue; `audit_pending`; never a repurchase | audit trail delayed | I9 |

Out of scope for this build: seller identity, escrow and refunds, rate limits across mandates, mainnet key custody.
