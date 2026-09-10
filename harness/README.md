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
3 `PAYMENT_UNRESOLVED`.

## What never becomes a pass

- **Exposure.** Every authorization that is `prepared`, `sent` or
  `unresolved` is money neither spent nor free. While any exists the run
  reports `PAYMENT_UNRESOLVED` and stops before buying anything more. A check
  marked `recovers = true` may run then; nothing else may.
- **A timeout.** The command leads its own process group and the group is
  terminated, so nothing keeps submitting payments after Proctor has returned.
- **Evidence Proctor could not read in full.** A journal with one malformed
  line is an infrastructure error naming the line number, because "nothing was
  paid" cannot be certified from a journal that was partly unreadable.
- **A report that could not be written.** The report is the record a reviewer
  reads; failing to save it is an infrastructure error, not a quiet pass.
- **A crash, malformed application output, or an unreachable dependency.**

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
`fixture_routes`, `fixture_requests_<route>`, and `fixture_journal_missing`
when the task names a journal the fixture has not written yet.

From the ledger: `ledger_authorizations`, `ledger_payment_ids`,
`ledger_signed_payloads`, `ledger_submissions`, `ledger_retrievals`,
`ledger_settled`, `ledger_failed`, `ledger_unresolved`, `ledger_sent`,
`ledger_prepared`, `ledger_receipts`, `ledger_receipts_published`,
`ledger_held`.

## Payment state is never discarded

A ledger holding exposure is resumed, never deleted: the row is the only way
to reconcile a payment that may still settle. A completed ledger is archived
under `.proctor/run-<port>/work/archive/` rather than overwritten. Each run
keeps its own journal, ledger and port under `.proctor/run-<port>/`, and stops
only the fixture process it started, by recorded pid, so a concurrent run or a
demo elsewhere on the machine is never killed.

The runner builds the buyer under test before running it, so a check can never
pass against yesterday's binary.

## Safety

Every child process starts from an empty environment with `PATH`, `HOME` and
`TERM`. A task can set additional variables explicitly in its command. Payment
credentials are not inherited; the buyer reads its own env file. Hooks and
checks execute local code with filesystem access, so this does not isolate
secrets from malicious commands. A git worktree is not a security sandbox.

The task is loaded and hashed before execution. The agent loop and a separate
protected verifier worktree are not implemented. A check holds a worktree lock
for its lifetime so another check cannot replace its fixture or journal.
Timeouts stop the command's process group; fixture teardown stops its owned
descendants. Ledgers containing recoverable payments, pending receipts or audit
charges are resumed. Unreadable ledgers stop the run and remain in place.

Script regressions use temporary databases and a local test server, without
payment credentials: `python3 harness/scripts/test_scripts.py`.

## Reports

Each run writes `.proctor/runs/<id>/report.json`: the outcome, every check with
its assertions and findings, infrastructure diagnostics, the measurements,
the task hash and Proctor's package version. It does not yet hash the verifier
binary. Loaded tasks produce a report even when setup or preflight fails;
an unreadable task produces an early structured error with `--json`.
