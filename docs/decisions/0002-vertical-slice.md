# 0002: vertical slice order

Date: 2026-09-07. Issue: #2. Status: accepted.

**Decision.** Build in six slices: settle, one-pool paid flow, plan competition, failure, ship-ready, ship. When a day slips, cut in this order: provenance sampling, `proctor run`, the degrade path, the duplicate and absent record tasks, the HCS retry queue.

**Why.** The product claim is procurement under a budget, so plan competition ships before breadth and is never cut. The video's mandatory list in docs/requirements.md fixes what else cannot be cut.

**Consequences.** Slice dates: Mon 7 settle, Tue 8 one-pool flow with a single HCS submit, Wed 9 plan competition and refusals over five pools, Thu 10 failure behaviors as `proctor check` tasks, Fri 11 validation and a clean-clone run, Sat 12 video and submission. Hybrid is not a runtime plan type: spec section 5 already re-plans over `W` after every step, and after the screen the choice is events plus explain against investigate for the whole remaining job, explanation cost and bundle tariff included, never a per-pool pick, so no separate code path exists. Pool count is configuration, not a cut item. Provenance sampling that was cut reports `not_run`; `not_applicable` means there were no hashes to sample.

**Amended 2026-09-10.** Hybrid did become a runtime plan type. `PlanKind` has
a `Hybrid` variant that competes alongside staged and bundle, building screen
for the unscreened pools then investigate for the pending ones. The reasoning
above was that re-planning after every step made a third kind redundant; in
practice naming it is what lets a refusal say why all three were unaffordable,
which `harness/tasks/refusal.toml` asserts. Spec section 5 lists all three.
