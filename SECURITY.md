# Security policy

## Reporting a vulnerability

Please report vulnerabilities privately, using GitHub's **Report a
vulnerability** option for this repository. If it is unavailable, contact a
maintainer privately. Do not open a public issue while the problem is
unresolved.

## Scope and safety

This is hackathon software on Hedera **testnet**. It has not been audited and
must not be used with mainnet funds.

Some scanners flag two synthetic test keys in
[`crates/mandate/src/hedera.rs`](crates/mandate/src/hedera.rs) and
[`crates/mandate/src/testing.rs`](crates/mandate/src/testing.rs). They are the
all-`01` and all-`02` byte seeds used for deterministic tests, respectively,
and are documented as never funded. They are not live credentials.
