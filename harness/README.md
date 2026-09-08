# Proctor

Proctor checks Hedera services against an executable acceptance contract, and
tests what happens between payment and delivery: a quote that drifts above its
ceiling, a response lost after settlement, a process that dies after signing.
Those behaviors decide whether an agent can be trusted with money, and no
existing harness covers them.

Design and the comparison with Hedera Harness: [../docs/harness.md](../docs/harness.md).

## The application is never its own oracle

A check has two halves, and passes only when both agree:

- `expect` reads the application's own output, its JSON report, by dotted path.
- `observe` reads what Proctor measured itself, from the fixture sellers'
  journal and the buyer's SQLite ledger, opened read-only as files.

That distinction is the whole point. A buyer with a recovery bug reports
`status: delivered` and correct totals while quietly paying twice; the seller's
journal shows six signed payments where the contract allows three.

## Commands

```sh
cargo run -p proctor -- check harness/tasks/resume.toml --dir .
cargo run -p proctor -- check harness/tasks/resume.toml --dir . --json
```

Exit codes are the four outcomes, in the precedence the design fixes:
0 `PASS`, 1 `IMPLEMENTATION_FAILURE`, 2 `INFRASTRUCTURE_ERROR`,
3 `PAYMENT_UNRESOLVED`. Unresolved exposure outranks everything: while money
is neither spent nor free, nothing is reported as passing and no findings go
to an agent. A timeout, a crash, malformed output and an unreachable service
never become a pass.

`proctor run`, the bounded agent loop, is not implemented yet.

## Tasks

| Task | What it fixes |
|---|---|
| `tasks/lost-response.toml` | The seller settles and drops every response. The buyer must recover each result with the payment it already signed: three settlements, three payment ids, three results served from the store. |
| `tasks/resume.toml` | A run is completed, then resumed over the same ledger. The resumed run must restore what it bought: three signed payments across both runs, never six. |
| `tasks/refusal.toml` | A budget too small for any plan. The refusal is the contract, so exit 3 passes, and the seller must have seen no signed payment at all. |

A task names its hooks, its checks, and where Proctor takes its own
measurements:

```toml
[task]
name = "resume without buying completed work again"
attempts = 3

[hooks]
setup = "harness/scripts/sellers up"
ready = "harness/scripts/sellers ready"
teardown = "harness/scripts/sellers down"

[evidence]
journal = ".proctor/journal/sellers-journal.jsonl"
ledger = ".proctor/work/resume.sqlite"
mandate_id = "proctor-resume"

[[check]]
name = "resume"
run = "harness/scripts/run-resume resume"
expect = { status = "delivered", "totals.outstanding" = "0.00000000" }
observe = { fixture_distinct_payloads = 3, fixture_settlements = 3 }
```

The task's SHA-256 is recorded in every report, so a contract edited to turn a
run green is a different task.

## Measurements

From the journal: `fixture_requests`, `fixture_signed_payloads`,
`fixture_distinct_payloads`, `fixture_payment_ids`, `fixture_settle_calls`,
`fixture_settlements`, `fixture_served_from_store`, `fixture_dropped`,
`fixture_rejected`, `fixture_distinct_results`, `fixture_errors`,
`fixture_routes`, `fixture_requests_<route>`.

From the ledger: `ledger_authorizations`, `ledger_payment_ids`,
`ledger_signed_payloads`, `ledger_submissions`, `ledger_retrievals`,
`ledger_settled`, `ledger_failed`, `ledger_unresolved`, `ledger_sent`,
`ledger_prepared`, `ledger_receipts`, `ledger_receipts_published`,
`ledger_held`.

## Safety

Every child process starts from an empty environment with `PATH`, `HOME` and
`TERM`, plus whatever the task names. Payment credentials reach the buyer from
its own env file and never enter a hook, a check or an agent adapter. A git
worktree contains changes; it is not a security sandbox.

Proctor, its fixtures and the task contracts live outside the tree an agent
edits, and a wrong test is corrected in a separate change, never inside the
attempt it would turn green.

## Reports

Each run writes `.proctor/runs/<id>/report.json`: the outcome, every check with
its assertions and findings, the measurements, and the hashes of the task and
the verifier. It is written even when the run fails, because that is when a
reviewer needs it.
