# #254 — small benchmark correctness gaps

Issue: [#254]. Predecessor: [#251](2026-08-01-251-bench-cold-warm.md).

## Approved scope

Close four ticket gaps in the benchmark tooling: correct stale RSS wording, add the internal ping after the Solr reload, ensure cold-pass latency has the complete established sample set, and make the query-result-cache statistic parser fail cleanly for malformed input. Numeric benchmark rows are not changed.

## Changed behavior

- `bench/src/bin/render_report.rs` now uses accurate RSS wording; the review fix also asserts the required positive wording exactly, rather than only rejecting the stale phrase.
- `bench/run.sh` pings Solr internally after its cache-flushing core reload and verifies cold-pass completeness before accepting latency output.
- `bench/run.sh` now enforces `bench/src/corpus.rs`'s established contract: cold results require all **48 distinct terms** and matching latency samples. Equality alone cannot reject an already-truncated two-line `terms.txt` when its latency file also has two lines, so both the known 48-term set and matching sample count are required.
- `bench/query_result_cache_stat.py` accepts only strict integer-shaped statistic values; malformed registry shape and invalid values fail explicitly instead of producing tracebacks.
- `docs/benchmarks.md` corrects the RSS explanatory note only; numeric rows remain unchanged.

Files changed: `bench/query_result_cache_stat.py`, `bench/run.sh`, `bench/src/bin/render_report.rs`, `bench/tests/query_result_cache_metrics.rs`, `bench/tests/render_report_notes.rs`, `bench/tests/run_sh_cold_warm.rs`, new `bench/tests/run_sh_cold_latency_samples.rs`, and `docs/benchmarks.md`.

## TDD and mutation evidence

Focused tests were first red for stale RSS wording, the missing internal ping, missing sample-count seam/calls, and all three traceback cases. They passed after implementation.

Two deliberate mutations were made and restored:

- disabling the 48-distinct-terms check made its focused test fail with status 101;
- disabling the registry-shape check made its focused test fail with status 101.

## Review

Independent review round 1: **BOUNCED** solely because the RSS test was negative-only. The fix added an exact positive assertion for the corrected wording. Independent review round 2: **APPROVED**, at the default two-round cap.

## Verification

Reviewer gate passed before rebase; after rebasing onto `origin/main` at `97e717f`, the full local gate passed again:

- root `cargo test`: 948 passed;
- `bench/` `cargo test`: 74 passed;
- root and `bench/` `cargo fmt --check`;
- root and `bench/` `cargo clippy --all-targets -- -D warnings`;
- `shellcheck bench/run.sh`.

No Docker/live benchmark was rerun: this is hermetic correctness and wording work, and benchmark numeric rows are unchanged.

## Follow-ups and risks

None. No accepted deviations.

[#254]: https://github.com/markdlabrecque/wayfinder/issues/254
[#251]: https://github.com/markdlabrecque/wayfinder/issues/251
