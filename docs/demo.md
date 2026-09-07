# Demo

One video, 3 to 4 minutes, one terminal. Arithmetic is in [mandate.md](mandate.md) section 6; rules are in [spec.md](spec.md). Tariffs are the sellers' fixture values; once the fixture exists this table is regenerated from it. Seller controls for scenarios 2, 4 and 5 are defined in the sellers' README once built. This file fixes what the buyer must print.

Common setup: five Uniswap v3 pools on Ethereum mainnet, 24 h window, coverage all material pools, transaction-level citations required, `degrade` false, service budget 0.0100 USDC, per-payment cap 0.0090, USDC on `hedera:testnet`, four sellers up with published tariffs. At start only screen and investigate can be quoted live; events and explain are tariff estimates until their inputs exist.

| # | Scenario | Change from baseline | Must appear on screen |
|---|---|---|---|
| 1 | Normal completion | none | `screen 0.0010 live, within ceiling`; `investigate 0.0080 live, within ceiling`; `events 0.0015 per pool ceiling`; `explain 0.0008 ceiling`; `plan staged expected 0.0033 bound 0.0093 chosen`; `plan hybrid expected 0.0042 bound 0.0090`; `plan bundle 0.0080`; `reserve explain 0.0008 held`; screen `pay_` id and `0.0.7162784@` tx id; `payment settled: record matches`; `outcomes after screen: 1 pending, 4 non_material`; `events 0.0023 vs bundle 0.0032; buying events`; `outcomes: 1 supported, 4 non_material; claims 4`; `brief 5 KB; explain quoted 0.0005; reserve released 0.0003`; `validation passed: coverage 5/5, calculations, citations, provenance 3/3`; `unspent 0.0070`; HCS topic id |
| 2 | Economic adaptation | investigate seller quotes 0.0030 live, below its 0.0080 ceiling | `investigate 0.0030 live, within ceiling`; `plan bundle 0.0030 chosen`; one tx id; same report shape |
| 3 | Safe incompletion | service budget 0.0030 | `REFUSED REQUIREMENT_UNMEETABLE bound 0.0080 expected 0.0033 available 0.0030`; `settled 0`; `audit spent` nonzero; no tx id |
| 3b | Degraded run, optional per decision 0002 | service budget 0.0030, `degrade` true | screen bought; `report incomplete: screening only`; `unspent 0.0020` |
| 4 | Integrity refusal | events seller quotes 0.0020 above its 0.0015 ceiling | after screen: `REFUSED events OFF_TARIFF ceiling 0.0015 quoted 0.0020`; `plan hybrid 0.0032 chosen`; `unspent 0.0058` |
| 5 | Lost response | events seller settles, then drops the response | `events sent, no response; submissions 3`; `payment settled: record matches`; `delivery none; retrieval 1 with original payment`; `delivery received`; one tx id; `unspent 0.0070` |
| 6 | Absent record | events seller never settles; no record appears | `events unresolved; 0.0015 reserved`; `run: mandate reconcile <id>`; report lists the unresolved authorization |
| 7 | Duplicate record | buyer resends once; mirror shows a DUPLICATE_TRANSACTION record and a SUCCESS record | `records 2, duplicates ignored 1`; `payment settled: record matches`; one tx id |

Main cut:

| Time | Content |
|---|---|
| 0:00 | One sentence: Mandate buys the evidence an agent needs, tracks every payment against its budget, and explains when it cannot finish |
| 0:15 | Scenario 1 live, narrated at the plan choice and at the buying-events decision |
| 1:20 | Scenario 2: the investigate seller quotes below its ceiling, the plan flips to bundle, same report shape |
| 1:40 | HashScan: one payment transaction and the HCS topic with receipts |
| 2:05 | Scenario 3: refusal before the first cent |
| 2:30 | Scenario 5: the lost response, settlement found on the ledger, result recovered, no second payment |
| 3:05 | Proctor before and after: a failing recovery check, the fix, a passing rerun with a live transaction reference; two `proctor run` attempts when the agent loop ships, otherwise two `proctor check` runs around a manual fix |
| 3:45 | Close: repo, three tracks, one line on what comes next |

Scenarios 3b, 4, 6 and 7 are recorded as backup clips, not in the main cut; 3b only if the degrade path survives the cut order in decision 0002.
