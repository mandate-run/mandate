# Demo

One video, 3 to 4 minutes, one terminal. Arithmetic is in [mandate.md](mandate.md) section 6; rules are in [spec.md](spec.md). Tariffs are the sellers' fixture values; once the fixture exists this table is regenerated from it. Seller controls for scenarios 2, 4 and 5 are defined in the sellers' README once built. This file fixes what the buyer must print.

Common setup: five Uniswap v3 pools on Ethereum mainnet, 24 h window, coverage all material pools, transaction-level citations required, `degrade` false, service budget 0.0100 USDC, per-payment cap 0.0090, USDC on `hedera:testnet`, four sellers up with published tariffs. At start only screen and investigate can be quoted live; events and explain are tariff estimates until their inputs exist.

| # | Scenario | Change from baseline | Must appear on screen |
|---|---|---|---|
| 1 | Normal completion | none | `screen 0.0010 live on-tariff`; `investigate 0.0080 live on-tariff`; `events 0.0015 per pool est`; `explain 0.0008 max est`; `plan staged expected 0.0033 bound 0.0093 chosen`; `plan hybrid expected 0.0042 bound 0.0090`; `plan bundle 0.0080`; `reserve explain 0.0008 held`; screen `pay_` id and `0.0.7162784@` tx id; `settled via receipt query`; `1 pool material; screening lacks citations; events 0.0023 vs bundle 0.0032; buying events`; `explain 5 KB quoted 0.0005; reserve released 0.0003`; `validation passed: claims, calculations, citations`; `unspent 0.0070`; HCS topic id |
| 2 | Economic adaptation | investigate tariff: no base, 0.0006 per pool | `plan bundle 0.0030 chosen`; one tx id; same report shape |
| 3 | Safe incompletion | service budget 0.0030 | `REFUSED REQUIREMENT_UNMEETABLE bound 0.0080 expected 0.0033 available 0.0030`; `settled 0`; `audit spent` nonzero; no tx id |
| 3b | Degraded run | service budget 0.0030, `degrade` true | screen bought; `report incomplete: screening only`; `unspent 0.0020` |
| 4 | Integrity refusal | events seller quotes 0.0020 against tariff 0.0015 | after screen: `REFUSED events OFF_TARIFF expected 0.0015 quoted 0.0020`; `plan hybrid 0.0032 chosen`; `unspent 0.0058` |
| 5 | Lost response | events seller settles, then drops the response | `events sent, no response`; `settled via mirror node`; `re-fetched with original payment`; one tx id; `unspent 0.0070` |

Main cut:

| Time | Content |
|---|---|
| 0:00 | One sentence: Mandate buys the evidence an agent needs, tracks every payment against its budget, and explains when it cannot finish |
| 0:15 | Scenario 1 live, narrated at the plan choice and at the buying-events decision |
| 1:40 | HashScan: one payment transaction and the HCS topic with receipts |
| 2:05 | Scenario 3: refusal before the first cent, then 3b for contrast |
| 2:45 | Scenario 5: the lost response, settlement found on the ledger, result recovered, no second payment |
| 3:15 | Proctor: `proctor validate --pay` against the events seller, then the run attestation on HashScan |
| 3:45 | Close: repo, three tracks, one line on what comes next |

Scenarios 2 and 4 are recorded as backup clips, not in the main cut.
