# Contributing to Mandate

Thanks for helping make Mandate easier to evaluate and use.

## Setup

Use Rust 1.98 with `protoc` on your `PATH`, plus Node 24 and pnpm 11. Enable
the repository's commit-message hook once per clone:

```sh
git config core.hooksPath .githooks
```

Two Hedera testnet accounts are needed only for a run against testnet. The
test suites need neither account.

## Verify your change

Run the full suite before opening a pull request:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
python3 harness/scripts/test_scripts.py
cd sellers && pnpm test
```

At the time this was checked, those commands ran 175 Rust tests, 10 Python
tests, and 83 seller tests. Clippy and the formatting check completed without
findings.

## Commit guidelines

The enforcing authority is [`.githooks/commit-msg`](.githooks/commit-msg).
It rejects a message that does not follow these rules.

- Use `type(scope): description`; scope is optional.
- Use one of `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`,
  `build`, `ci`, `chore`, or `revert`.
- Keep the subject to 50 characters including its type and colon; use a
  lowercase description with no trailing period.
- If present, leave line two blank, then use at most three `- ` bullets.
  Each bullet is at most 50 characters, starts lowercase, and there are no
  blank lines within the body.
- Do not add an attribution trailer.

Keep commits granular: do not combine separable goals.

## Branches and pull requests

Use one branch and one pull request per issue. Name the branch `N-slug`. Start
the PR body with `Closes #N`, or `Part of #N` when the issue remains open to
collect evidence. Rebase-merge only after CI is green. A PR that changes
[`docs/spec.md`](docs/spec.md) or [`docs/decisions/`](docs/decisions/) waits
for the other maintainer's approval.

## Where things live

| Path | Purpose |
|---|---|
| `crates/mandate` | buyer runtime |
| `sellers` | x402-gated reference endpoints |
| `harness` | Proctor acceptance harness |
| `docs/spec.md` | authoritative behaviour specification |

## Testing philosophy

An application is never its own oracle. Tests that matter check what an
independent observer measured, not what the application reported.
