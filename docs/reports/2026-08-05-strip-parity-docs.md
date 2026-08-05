# Issue #392 step 2: retire prospective Solr-parity documentation

## Scope

Rewrite governing documentation around Wayfinder's shipped behavior without changing any runtime
feature, endpoint, parameter, response shape, or fixture-backed behavior. Keep
`solr-ref/responses/` as a frozen regression baseline, preserve numbered factual findings, remove
active parity planning and comparison-runner instructions, and mark completed implementation specs
as historical. The separate decision about `src/coverage.rs` is not part of this change.

## Changed behavior

- `CLAUDE.md`, `README.md`, and `docs/PRD.md` now describe the Solr-compatible wire as the
  existing-client interface rather than an ongoing parity target.
- PRD §5 records permanent unsupported boundaries without revisit gates or roadmap choreography.
- The obsolete #289-#302 parity sequencing plan is removed. The completed backend plan and every
  file under `docs/specs/` are explicitly marked as historical records.
- `docs/solr-ref-findings.md` removes only unnumbered capture backlog and differential-harness
  sections. All 188 numbered findings remain with their historical bodies intact.
- `tests/documentation_contract.rs` protects the frozen-fixture policy, current-behavior framing,
  permanent boundaries, exact finding-number set and body digest, historical spec markers, and
  removal of the obsolete parity plan.
- No runtime source, fixture response, or fixture-backed feature test changed.

## Verification

- Red-first documentation contract: the new assertions initially failed against the old policy and
  passed after the rewrite.
- `cargo test --test documentation_contract` — passed (6 tests).
- `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test` — passed after the
  final changes. The existing `sort_copy_bloat` measurement remains ignored by its own issue guard.
- `git diff --check` — passed.
- Mechanical checks: zero files changed under `solr-ref/responses/`; 188 numbered findings retained;
  no runtime files changed.

## Review

Independent review round 1: **must fix**. It found the still-active #289-#302 plan, stale Search API
phase wording, and insufficient finding/spec guards. Those were fixed by removing the plan,
neutralizing completed specs, and strengthening the contract.

Independent review round 2: **must fix**. It found that finding-number set checks could not protect
historical bodies, that bulk wording edits had damaged fixture provenance, and that the completed
backend plan still looked active. The two-round review cap then triggered recoverable foreground
escalation. The chosen narrow fix restored every numbered finding body from `origin/main`, added a
body digest plus duplicate-number guard, corrected the documented gap list, and marked the backend
plan historical. The complete local gate passed on that final state.

## Follow-up

Issue #392's separate step 3 must decide whether the shipped capability report in
`src/coverage.rs` remains useful. This PR intentionally does not make that product decision.
