# Demo

One video, 3 to 4 minutes, one terminal. Arithmetic for each scenario is in [mandate.md](mandate.md) section 6. Seller controls for scenarios 2, 4 and 5 are defined in the sellers' README once built; this file fixes only what the buyer must print.

Common setup: five Uniswap v3 pools on Ethereum mainnet, 24 h window, transaction-level evidence required, USDC on `hedera:testnet`, four sellers up with published tariffs.

| # | Scenario | Change from baseline | Must appear on screen |
|---|---|---|---|
| 1 | Normal completion | none | four quotes, all on tariff; `plan staged 0.0038 < bundled 0.0080`; `reserve explain 0.0008 held`; screen tx id; `1 pool material; screening lacks citations; buying events`; events tx id; explain tx id; `validation passed`; `returned 0.0062`; HCS topic id |
| 2 | Economic adaptation | investigate tariff cut to 0.0006 per pool | `plan bundled 0.0030 < staged 0.0038`; one tx id; same report shape |
| 3 | Safe incompletion | budget 0.0030 | screen tx id; `REFUSED events OVER_BUDGET needed 0.0020 available 0.0012`; `REFUSED explain REQUIREMENT_UNMEETABLE`; `report incomplete`; `returned 0.0020` |
| 4 | Integrity refusal | events seller quotes 0.0025 against tariff 0.0020 | `REFUSED events OFF_TARIFF expected 0.0020 quoted 0.0025`; plan falls back to bundled |
| 5 | Unresolved payment | events seller settles, then returns no body | `events unknown`; `settled via mirror node`; result re-fetched; no second tx id |

Video order:

| Time | Content |
|---|---|
| 0:00 | One sentence: the agent buys only the evidence its task needs, keeps enough to finish, explains what it refused |
| 0:15 | Scenario 1 live, narrated at the plan choice and at the buying-events decision |
| 1:45 | HashScan: one payment transaction and the HCS topic with receipts |
| 2:15 | Scenario 3 live, the two refusal lines |
| 3:00 | Scenario 2 or 4, whichever runs cleaner, 30 s |
| 3:30 | Close: repo, both tracks, one line on sub-mandates |

Scenario 5 is recorded as a backup clip, not in the main cut.
