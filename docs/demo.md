# Demo

One video, 3 to 4 minutes, one terminal. Arithmetic is in [mandate.md](mandate.md) section 6. Seller controls for scenarios 2, 4 and 5 are defined in the sellers' README once built; this file fixes what the buyer must print.

Common setup: five Uniswap v3 pools on Ethereum mainnet, 24 h window, transaction-level evidence required, `degrade` false, USDC on `hedera:testnet`, four sellers up with published tariffs. At start only screen and investigate can be quoted live; events and explain are tariff estimates until their inputs exist.

| # | Scenario | Change from baseline | Must appear on screen |
|---|---|---|---|
| 1 | Normal completion | none | `screen 0.0010 live on-tariff`; `investigate 0.0080 live on-tariff`; `events 0.0020 est`; `explain 0.0008 est`; `plan staged expected 0.0038 bound 0.0098`; `plan bundled expected 0.0080 rejected: higher expected`; `reserve explain 0.0008 held`; screen `pay_` id and `0.0.7162784@` tx id; `1 pool material; screening lacks citations; buying events`; events `live on-tariff`; explain `validation passed`; `returned 0.0062`; HCS topic id |
| 2 | Economic adaptation | investigate tariff cut to 0.0006 per pool | `plan bundled expected 0.0030` chosen; one tx id; same report shape |
| 3 | Safe incompletion | budget 0.0030 | `REFUSED plan REQUIREMENT_UNMEETABLE needed 0.0038 available 0.0030`; `settled 0`; no tx id |
| 3b | Degraded run | budget 0.0030, `degrade` true | screen bought; `report incomplete: screening only`; `returned 0.0020` |
| 4 | Integrity refusal | events seller quotes 0.0025 against tariff 0.0020 | after screen: `REFUSED events OFF_TARIFF expected 0.0020 quoted 0.0025`; `plan bundled expected 0.0080` chosen; `returned 0.0010` |
| 5 | Unresolved payment | events seller settles, then returns no body | `events unknown`; `settled via mirror node`; `re-fetched with same pay_ id`; one tx id |

Video order:

| Time | Content |
|---|---|
| 0:00 | One sentence: the agent buys only the evidence its task needs, keeps enough to finish, explains what it refused |
| 0:15 | Scenario 1 live, narrated at the plan choice and at the buying-events decision |
| 1:45 | HashScan: one payment transaction and the HCS topic with receipts |
| 2:15 | Scenario 3 live: refusal before the first cent, then 3b for contrast |
| 3:00 | Scenario 2 or 4, whichever runs cleaner, 30 s |
| 3:30 | Close: repo, both tracks, one line on what comes next |

Scenario 5 is recorded as a backup clip, not in the main cut.
