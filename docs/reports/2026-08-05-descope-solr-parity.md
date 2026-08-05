# Issue #392: descope Solr parity

## Outcome

Solr parity is no longer a source of future work. Wayfinder keeps its shipped wire behavior,
frozen response fixtures, fixture-backed regression tests, and historical client evidence while
retiring the machinery and planning artifacts that existed to drive further convergence.

The work was intentionally split into independently reviewable PRs:

1. PR #404 removed the differential harness, revisit guards, provenance gates, manifests, and
   capture script without changing shipped behavior.
2. PR #405 rewrote governing documentation around current behavior, removed the active parity
   roadmap, and marked completed plans/specs historical.
3. This PR removes the Search API coverage report after an explicit product decision.

## Coverage-report decision

**Decision: remove `coverage_report` and `wayfinder coverage --format json`.**

The report measured Wayfinder against a frozen Search API Solr capture and had reached 75/75. With
no ongoing parity roadmap, that denominator can never intentionally grow and its permanent 100%
result is not a useful operational capability signal. Keeping it would preserve a public API and
CLI command, roughly 2,500 lines of production probing, and a 4,209-line derived contract solely to
restate a historical milestone.

Removed:

- `src/coverage.rs` and the public `coverage_report` export
- the `wayfinder coverage --format json` CLI command
- `tests/search_api_coverage.rs`
- `coverage/search_api_coverage_contract.json`
- the optional coverage invocation in `docs/operations.md`

Retained:

- all frozen response fixtures and fixture-backed feature tests
- the Search API Solr 4.4.0 source snapshot, evidence manifest, and provenance document
- an independent hash/completeness audit for that retained client-source evidence
- every shipped server route, parameter, response shape, and behavior

## Documentation disposition

PR #405 converted conditional descopes into flat unsupported boundaries, including unsupported
edismax parameters, local-params parser types, `q.op`/`qt`, `search_api_solr_admin`, open-ended
custom analyzer families, atomic updates, and optimistic concurrency. It retained the numbered
factual findings as historical evidence and removed only unnumbered capture/harness planning.

## Test-count delta

Counts come from `cargo test -- --list` on each landed or proposed state:

| State | Tests | Delta |
|---|---:|---:|
| Before issue #392 | 1,571 | — |
| After harness removal (#404) | 1,467 | -104 |
| After documentation contract rewrite (#405) | 1,470 | +3 |
| After coverage-report removal | 1,435 | -35 |
| **Overall** | **1,435** | **-136** |

The step-3 net delta is exact: 29 unit tests embedded in `src/coverage.rs` plus 8 tests in
`tests/search_api_coverage.rs` were removed, while 2 focused contracts were added for report
retirement and retained source-evidence integrity.

## Verification

- New retirement test failed first against the existing API, CLI, module, suite, and contract.
- Mutation evidence: temporarily restoring `src/coverage.rs` made the retirement test fail and
  name the resurrected artifact.
- `cargo test --test coverage_report_retirement --test search_api_source_evidence` — passed.
- `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test` — passed after
  implementation and in both independent review rounds; the existing issue-guarded sort-copy
  measurement remains ignored.
- No files under `solr-ref/responses/` changed.

## Review

Round 1 was **must fix** because removal of the old coverage suite also removed the only integrity
audit for the retained Search API client-source snapshot. The fix extracted that audit into
`tests/search_api_source_evidence.rs` and corrected the provenance document.

Round 2 was **must fix** because current source/test comments still cited the deleted coverage
module and contract. At the two-round cap, the foreground Orchestrator selected the narrow-fix
escalation: replace those dangling references with direct behavioral descriptions and flat current
scope boundaries, then rerun targeted and full gates.

## Remaining issue work

The final phase is issue triage: close parity-only issues #385, #389, and #390 with a link to #392,
and determine whether #388 is a shipped analyzer bug that should remain open without parity
framing.
