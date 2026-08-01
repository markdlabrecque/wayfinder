# Issue #189: `mlt.maxntp`

## Approved spec and changed behavior

Implement Solr-compatible signed-i32 parsing for `mlt.maxntp`: absent defaults to 5000; values `<= 0` impose a zero-token cap; malformed or out-of-range values return the 400 `NoParams` envelope, after `q` parsing. The cap counts analyzer-emitted tokens before noise filtering and resets for each stored value.

## Implementation

Implemented the MLT cap and parsing behavior in the MLT/search path, added low/high and malformed/overflow Solr fixtures with manifest and capture provenance, and added semantic Search API coverage (68/75 to 69/75). Tests cover custom-analyzer/multi-value behavior, cap edges, error envelopes, and malformed-`q` precedence.

Changed implementation/test/fixture files: `src/core_index.rs`, `src/lib.rs`, `src/coverage.rs`, `tests/mlt.rs`, `tests/search_api_coverage.rs`, `solr-ref/capture.sh`, `solr-ref/manifest.tsv`, `solr-ref/manifest-errors.tsv`, `solr-ref/responses/mlt_maxntp_low.json`, `solr-ref/responses/mlt_maxntp_invalid.json`, and `solr-ref/responses/mlt_maxntp_overflow.json`; Solr findings were recorded in `docs/solr-ref-findings.md`.

## Evidence

- Live `solr:9`: `mlt.maxntp=1` returned 0 similar documents; `5000` returned 4. `0` and `-1` returned 200 with a zero cap. `abc` and `2147483648` returned 400. Malformed `q` wins over malformed `mlt.maxntp`.
- Mutation: changing the cap comparison to an ineffective threshold made the fixture test fail; reverted. Omitting `NoParams` made the differential response fail at `responseHeader.params`; fixed. The precedence test kills the former pre-`q` parse ordering.
- Final gate (provided evidence): `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test` — PASS.
- Final gate (provided evidence): `git diff --check` — PASS.
- This reporting step did not run tests or gates.

## Review verdict

Round 1 found hollow coverage/provenance and missing discriminating tests; these were fixed. Round 2 found missing error fixtures/precedence coverage and capture-provenance placement. The two-round review cap was reached; recoverable escalation was addressed with narrow fixes. Final review outcome is technically complete subject to CI.

## Follow-ups and risks

No deferred follow-ups or accepted deviations. No unresolved technical risks; CI is pending and remains the sole outstanding release gate.
