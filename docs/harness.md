# Proctor

Working name. Proctor builds Hedera services from an acceptance contract, tests their behavior under payment failure, and returns a reviewable change with payment evidence. It is a standalone tool inspired by Hedera Harness and is Mandate's entry to the Hedera Open Source track. Mandate is the first project built with it.

![Proctor loop](img/proctor-loop.svg)

## Why a second harness

Hedera Harness drives Cursor or Claude Code to build features into scaffold-hbar projects, Next.js with Hardhat or Foundry, and validates with static checks, Playwright, a semantic grader and an on-chain tier that completes testnet transactions from a burner signer. The official x402 end-to-end suite tests client, server and facilitator combinations across languages, Hedera included. Neither tests what happens between payment and delivery: a quote that drifts from the agreed price, a response lost after settlement, a process that dies after signing. Those behaviors decide whether an agent can be trusted with money. Proctor tests them and drives a coding agent to fix them.

| Difference | Proctor |
|---|---|
| Unit of work | An executable acceptance contract, not a product brief |
| Project type | Anything built and started from commands; Rust and Node services first |
| Oracle | The protocol and the ledger; no browser, no model-graded tier |
| Failure testing | Fault-injecting fixtures for quote drift, lost responses and restarts, plus a separately labelled live pass |
| Loop safety | Four outcomes; infrastructure errors stop the loop; the agent cannot alter the contract |
| Paid checks | Run as mandates through Mandate core, with their own budget and durable ledger |

## Commands

- `proctor check tasks/<task>.toml` runs the contract once against the current implementation. No agent. A task with only protocol checks against a URL is the smallest task.
- `proctor run tasks/<task>.toml` runs the bounded loop: check, hand failures to the coding agent, inspect the change, recheck, stop at pass, at the attempt limit, or at the first infrastructure error. Output: a patch on branch `proctor/<run>`, a report, and transaction references.

## Task contract

A task file declares hooks, checks and limits. Proctor hashes it at the start of a run; a changed hash aborts the attempt. The agent edits the worktree, never the task, the checks or Proctor. Example, illustrative until the crate exists:

```toml
[task]
name = "recover a paid request after a lost response"
attempts = 3

[hooks]                       # commands with timeouts; output captured
setup = "scripts/sellers up --fault drop-response-after-settle"
ready = "scripts/sellers ready"
teardown = "scripts/sellers down"

[agent]
adapter = "adapters/claude-code"   # executable; task and findings arrive on stdin

[[check]]
name = "build"
run = "cargo build --workspace && pnpm -C sellers build"

[[check]]
name = "recovery"
run = "mandate run fixtures/lost-response.yaml --json"
expect = { step = "events", authorizations = 1, state = "settled", result_recovered = true, outstanding = "0" }

[[check]]
name = "restart"
run = "scripts/crash-after prepared && mandate resume --json"
expect = { authorizations = 1, outstanding = "0" }

[live]                        # optional; reported under its own heading
facilitator = "https://api.testnet.blocky402.com"
mandate = "fixtures/live-acceptance.yaml"
```

## Outcomes

| Outcome | Meaning | Loop |
|---|---|---|
| PASS | every check met its expectation | stop, report |
| IMPLEMENTATION_FAILURE | a check ran and its expectation was not met | findings to the agent; next attempt |
| INFRASTRUCTURE_ERROR | a hook failed, a check crashed or returned malformed output, or the facilitator, mirror node or Graph gateway was unreachable | stop; nothing goes to the agent |
| PAYMENT_UNRESOLVED | a live test purchase is neither settled nor provably unexecuted | stop; exposure recorded in the journal; resume later |

## Initial scenarios

| Scenario | Fixture | Contract, in short |
|---|---|---|
| Quote drift | events seller quotes above tariff | refused OFF_TARIFF; plan re-chosen; no authorization for the drifted quote |
| Lost response | seller settles, then drops the response | settlement proven from the ledger; result fetched with the original signed payment; exactly one authorization; budget columns match |
| Restart after authorization | buyer exits at state `prepared` through a test-only crash point | on resume the same signed bytes are sent; no second authorization; budget intact |

Fixtures are the reference sellers with fault switches, run locally. A live pass runs the same contract through Blocky402 on testnet with one real test purchase and is reported under its own heading. Fixture results are never presented as proof of settlement.

## Modules

| Module | Responsibility |
|---|---|
| hooks | run setup, ready, check and teardown commands with timeouts; capture output |
| agent | invoke one adapter executable with the task and the previous findings; one adapter first |
| checks | run checks, compare to expectations, classify into the four outcomes |
| journal | persist attempts, task hash, configuration, evidence, transaction ids and any unresolved exposure under `.proctor/runs/<id>/` |
| hedera | inspect 402 requirements, execute bounded test purchases as mandates through Mandate core, reconcile settlement, optionally publish the report hash to HCS |

Proctor depends on Mandate core for signing, reconciliation and the ledger. Mandate does not depend on Proctor.

## Boundaries

- The agent cannot approve changes to its own contract. Contract and verifier version are frozen per run. A wrong test is fixed in a separate change, never inside the attempt it would turn green.
- Infrastructure failure stops the loop. A facilitator outage never causes a rewrite of working payment code.
- Paid checks spend from a bounded test account under a mandate: allowlist of the endpoint under test, per-payment cap, durable authorizations. Restarting an attempt does not refill the allowance.
- Payment credentials are not in the agent's environment or the application's. A git worktree contains changes; it is not a security sandbox.

## Evidence and the video

The report states which checks passed, against which endpoint and configuration, at what time, with transaction ids. An HCS message carrying the report hash makes that statement timestamped and identifiable; it does not prove a deployment still runs the tested code. The video segment is a two-attempt transcript from building Mandate: a first attempt that fails a recovery check, the agent's change, and a second attempt that passes with a live transaction reference. If development passes first time, the segment checks out the commit before the fix, lets Proctor catch it, and says so.

## Track mapping

| Requirement, quoted | How |
|---|---|
| "build new harness inspired by it" | Proctor, standalone, with the inspiration and differences documented above |
| "Public GitHub repo/PR with README explaining problem solved" | This document and the crate README |
| "Demo video (≤5 minutes) showing improvement" | The two-attempt segment of the project video |
| "Harness for uncovered language/framework" | Rust workspaces and Node services |
| "New service coverage" | x402 payment recovery on Hedera, HCS receipts, HTS association |
| "Clear before/after developer experience evidence" | Manual reproduction of a payment bug versus `proctor run`; Mandate's recovery increments built through it |

## Deferred

Seller certification, automatic account provisioning, a plugin marketplace, more than one agent adapter, multi-agent orchestration, and an upstream PR unless something worth contributing appears.
