# Issue #99: Internal `_version_` field

Approved plan: `docs/plans/99-version-field.md`.

## Completed

- Added `_version_` as an internal, non-user-configured, non-stored Tantivy `i64` fast field.
- Versions use a per-core Unix-epoch-millisecond-seeded `AtomicI64`; each validated document receives an increasing value immediately before writer insertion.
- Authorized `_version_` only in the existing stats validation and aggregation path. It remains unavailable to user input, schema declarations, copy/default-field configuration, sorting, and faceting, including under exact or wildcard dynamic rules.
- Accepted `function` as a select parameter and matched Solr's `stats.field=_version_&function=max(_version_)` behavior: `function` is echoed and `_version_` remains the sole stats key.
- Added a clear reindex error when opening an index created before the internal field existed.
- Captured and recorded real Solr 9 evidence in `stats_version_max.json` and finding 80.

The PRD scope remains deliberately narrow: atomic update modifiers, `versions=true` update responses, stale-write checks, and 409 conflicts are out of scope.

## Files changed

- `src/core_index.rs`
- `src/lib.rs`
- `src/schema.rs`
- `src/stats.rs`
- `tests/version_field.rs`
- `tests/differential.rs`
- `solr-ref/capture.sh`
- `solr-ref/manifest-errors.tsv`
- `solr-ref/responses/stats_version_max.json`
- `docs/solr-ref-findings.md`
- `docs/plans/99-version-field.md`

## Verification

| Command/check | Result |
|---|---|
| `cargo test --test version_field` before implementation | Red: two tests failed for the expected missing internal field. |
| Mutation: replace `fetch_add` with `load` | Targeted monotonicity test failed as intended; mutation reverted. |
| Exact/wildcard dynamic-rule reservation regressions | Red before the final narrow fix (forged `_version_` accepted with HTTP 200); green after it. |
| `cargo test --test version_field && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test` | Green; targeted suite 7/7 and complete suite passed. |

## Review

- Round 1 found two must-fix issues: user-schema ownership and overbroad general resolver exposure. Both were fixed with regressions.
- Round 2 found an exact/wildcard dynamic-rule bypass. The two-round cap was enforced; a narrow root-cause fix made reserved `_version_` names never resolve dynamically.
- Targeted red/green evidence and the full gate prove the final fix. A workflow file-lock limitation required the Orchestrator to execute post-fix gates.
- No third review occurred, so final independent reviewer approval was unavailable under the cap.

## Outstanding

No known remaining code defect or deferred implementation follow-up. An additional independent review could still help because the review cap prevented a post-fix approval pass.
