# #297 — scope MoreLikeThis results to the index

Client-side only. The original issue body asserted `/mlt` reads no `fq` at all;
that was wrong when filed and is explicitly corrected in the issue: the server
side landed in **#192** (`feat(mlt): honour fq, mlt.match.include/offset and
json.nl on /mlt`), so this PR does no server work, runs no `capture.sh`, and
adds no `docs/solr-ref-findings.md` entry.

## Problem

`QueryBuilder::buildMlt()` (`drupal/search_api_wayfinder/src/QueryBuilder.php`)
sent only `q`, `mlt.fl` and paging. The seed lookup happened to be scoped
(ids are prefixed `$index->id() . '-' . $option['id']`), but the *similar
documents* were not: on a core holding more than one index, `search_api_mlt`
could return documents belonging to a sibling index.

The stale limitation was asserted in two places that had not been updated when
#192 landed:

- a `ponytail:` block in `buildMlt()`'s docblock ("Wayfinder's /mlt accepts no
  fq at all", plus an out-of-date `MLT_PARAMS` enumeration);
- a "Not supported" bullet in `drupal/search_api_wayfinder/README.md`.

## Premise verification (read-only)

- `MLT_PARAMS` in `src/lib.rs` contains `fq`, with a comment stating it is
  *implemented*, not merely allowlisted, citing finding 98.
- Finding 98 confirms `fq` on `/mlt` filters only the similar-docs result set,
  never the seed-doc resolution.
- The fixtures are committed and in `solr-ref/manifest.tsv`:
  `mlt_fq_scope`, `mlt_fq_seed_not_filtered`, `mlt_fq_multiple_and`.

## Fix

`buildMlt()` now sends the same index scope `build()` seeds —
`index_id:"<id>"` (locked decision 2, core multi-index-per-core wiring) — via a
new private `indexScopeFilter(IndexInterface): string` helper that both call
sites use, so the convention cannot drift between them. `build()`'s seed line
was rewritten to call the helper; behaviour there is unchanged.

`q` keeps its `id:` seed lookup unchanged (composite `{index_id}-{item_id}`,
escaped through `FieldMapper::filterValue()`).

Both stale-prose sites were deleted outright rather than hedged — the
capability exists and is fixture-backed. The other README descopes were left
alone (the issue notes that file is not authoritative and the remaining six
were spot-checked as still holding).

## Test + TDD

- `testBuildMltScopesResultsToTheIndex` in
  `drupal/search_api_wayfinder/tests/src/Unit/QueryBuilderTest.php`, inserted
  next to the existing MLT cases (not at the end of the class — that file is
  the one genuine collision site with the #299 facet work).
- **Confirmed red for the right reason** before the fix: `Failed asserting
  that null is identical to 'index_id:"my_index"'` (`Undefined array key "fq"`
  — `buildMlt()` sent no `fq`). Green after.

## Green evidence

- `vendor/bin/phpunit` — **246 passed, 0 failed** (was 245 + the new case).
- `cargo fmt --check`, `cargo fmt --check --manifest-path bench/Cargo.toml` —
  clean.
- `cargo clippy --all-targets -- -D warnings` (root + bench) — clean (CI's
  exact invocation).
- `cargo test` (root + bench) — all suites ok, 0 failed. Diff touches no
  `.rs`; run to confirm the tree is green on the branch, not because the
  change could affect it.

No `capture.sh` run, no server diff, no findings entry.
