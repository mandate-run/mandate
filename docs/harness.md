# Proctor

Working name. A harness that builds and certifies x402 services on Hedera, inspired by Hedera Harness and submitted to its Open Source track. It is part of Mandate: it turns the seller obligations in [spec.md](spec.md) section 12 into executable checks, and its run attestations on HCS are a trust signal the buyer can read at discovery.

## Why a second harness

Hedera Harness drives Cursor or Claude Code to build features into scaffold-hbar projects, Next.js with Hardhat or Foundry, and validates with static checks, Playwright, a semantic grader and an on-chain tier that completes real testnet transactions verified via the mirror node. Payment services are a different target: no browser, no page to grade, and the oracle is the protocol and the ledger. Proctor covers what that tool does not:

| Gap | Proctor |
|---|---|
| Rust and plain Node service projects | Recipe declares build and test commands; no framework assumed |
| x402 v2 conformance | Protocol tier decodes the 402 and checks it field by field |
| Payment services on Hedera | Chain tier makes a real paid request through Blocky402 and verifies it on the mirror node |
| HCS and HTS in the settlement path | Checks receipt messages on a topic and token association before paying |
| Evidence that a run happened | Attestation of commit, report hash and verdict written to HCS |
| Agent lock-in | Any CLI agent via a command template; deterministic tiers before any model is involved |

No browser tier and no semantic tier. Those belong to app harnesses.

## One command, no recipe

`proctor validate <url>` runs the protocol tier against any x402 endpoint. With `--pay` it also runs the chain tier from an ephemeral funded testnet payer and prints the transaction id. This is the tool a Track 1 builder runs before a buyer ever tries to pay them.

## Contract

- **Recipe** `proctor.toml`: project kind, baseline commands, agent command template, PRD paths, max attempts, enabled tiers, network, mirror URL, facilitator URL, attestation topic.
- **Validator**: any executable. It receives the run context as JSON on stdin and prints `{ "name", "pass", "evidence": [], "hint" }`. Exit code is ignored; the JSON is the verdict.
- **Run**: `.proctor/runs/<id>/report.json` with every check, duration, transaction id and HCS sequence number. Checkpoint commits on branch `proctor/<id>` stage explicit paths only.
- **Attestation**: one HCS message `{ run_id, commit, report_hash, verdict, at }`, at most 1024 bytes.

## Tiers

| Tier | Checks | Pass condition |
|---|---|---|
| T0 static | secret scan, forbidden paths, formatting | no findings |
| T1 build | `cargo build`, `cargo test`, `pnpm build`, `pnpm test` as declared | all exit 0 |
| T2 protocol | status is 402; `PAYMENT-REQUIRED` decodes to v2 `PaymentRequired`; an `accepts` entry is `exact` on the declared network; `extra.feePayer` equals the facilitator's `/supported` signer; amount equals the listing tariff for the probe request; `bazaar` info present and valid against its schema; `offer-receipt` offer verifies when present | every check true |
| T3 chain | ephemeral payer funded by the operator; payer associated with the asset when HTS; paid request settles; mirror node shows the transfer with the expected amount, `payTo` and fee payer; a second identical request with the same `payment-identifier` returns the same body and no second transfer; HCS topic shows a receipt naming the transaction; leftover funds swept back | every check true, transaction ids recorded |

## Loop

`proctor run`: for each PRD, invoke the agent with the PRD and the repo; run tiers in order and stop at the first failing tier; on failure invoke the agent again with the failing checks' JSON as feedback, up to max attempts; checkpoint after each attempt; attest the final verdict. `proctor validate` runs T2 and optionally T3 with no agent.

## Use inside Mandate

- Mandate's four sellers are certified by Proctor. Each listing in the manifest carries the HCS message id of its latest passing attestation.
- The buyer's `constraints.sellers` gains a value `certified`: only listings with a valid attestation at the manifest's commit are quoted. Not in the first cut of the runtime.
- Mandate's own increments after the first end-to-end run are built through `proctor run`. Those runs are the before and after evidence for the track.

## Track mapping

| Requirement, quoted | How |
|---|---|
| "Submit meaningful contribution to Hedera Harness (PR acceptable) or build new harness inspired by it" | Proctor, plus one upstream PR adding an x402 conformance shell validator to Hedera Harness |
| "Public GitHub repo/PR with README explaining problem solved" | This document and the crate README |
| "Demo video (≤5 minutes) showing improvement" | A segment of the project video: `proctor validate --pay` against a seller, then a run's attestation on HashScan |
| "Harness for uncovered language/framework" | Rust workspaces and plain Node services |
| "New service coverage" | x402, Blocky402 settlement, HCS receipts, HTS association |
| "Clear before/after developer experience evidence" | Manual gate-one transcript versus one `proctor validate --pay` command; Mandate increments built by `proctor run` |

## Non-goals

Browser tests, model-graded assertions, mainnet, multi-repo orchestration.
