# Proctor

Build Hedera services from an executable acceptance contract and test their
behavior under payment failure — Mandate's entry to the Hedera Open Source
track. Design and boundaries: [docs/harness.md](../docs/harness.md).

Proctor drives a **task**: a TOML acceptance contract that brings a system up,
runs checks, and compares `expect` (assertions over the application's own
output) with `observe` (measurements Proctor takes itself from fixture
journals and ledgers). A check passes only when both agree; the application
under test is never its own oracle.

```bash
cargo build -p proctor

# run the contract once against the current implementation (no agent)
cargo run -p proctor -- check proctor/tasks/recover-lost-response.toml
```

`proctor check` writes every attempt under `.proctor/runs/<id>/` with the
task's frozen hash, the check results and the report. `proctor run` — the
bounded loop that hands failures to a coding agent and re-checks — is
scaffolded but needs the agent adapter; the CLI fails loudly rather than
silently pretending to loop.

Outcomes (fixed precedence): `PASS`, `IMPLEMENTATION_FAILURE`,
`INFRASTRUCTURE_ERROR`, `PAYMENT_UNRESOLVED`.

## Task contract

```toml
[task]
name = "recover a paid request after a lost response"
attempts = 3

[hooks]
setup = "cd sellers && npm install"           # bring the system up
ready = "curl -sf http://localhost:4021/healthz"
teardown = "pkill -f 'node .*server.mjs' || true"

[agent]
adapter = "adapters/claude-code"              # run command uses this

[[check]]
name = "live recovery with a lost response"
run = "mandate run ... --live --json"          # the application's own output
expect = { "steps[0].submissions" = 1, "steps[0].retrievals" = 1 }   # over that output
observe = { fixture_results = 3 }              # measured by Proctor, not the app

[live]                                        # optional live pass
facilitator = "https://api.testnet.blocky402.com"
```

## Status

Scaffold: contract parsing, hooks with timeouts, `expect` (dotted JSON paths,
array indexing) and `observe` (fixture result store), journaling and the
outcome classification all work under `proctor check`. Missing before it can
earn the track's points: the agent loop (`proctor run`), journal/ledger
snapshotting at crash points, the `ready`-probing of the verifier for
infrastructure classification, and the live-pass reporting with HCS
attestation.
