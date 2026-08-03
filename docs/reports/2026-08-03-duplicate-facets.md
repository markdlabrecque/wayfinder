# #299 — two facets on the same field no longer collapse

Client-side only (PHP). No server/Rust change, no new fixtures.

## Problem

Two Search API facets on the same field collapsed to one result.
`QueryBuilder::buildFacets()` pushed the bare mapped field name into every
`facet.field` entry, so two facets over `category` both became
`facet.field=ss_category` and the core answered a single key. `ResponseParser::
parseFacets()` mapped that one key back to deltas by field name — last delta
wins, the rest render empty. The failure was silent.

The server already supported the fix: `{!key=<label>}<field>` relabels a facet's
buckets (`src/facet.rs::split_facet_key`, issue #138), and
`solr-ref/responses/facet_extag_both_facets.json` (finding 138) is real Solr
answering one field under two distinct `{!key=}` labels with different counts.
So this was purely a matter of the client emitting a key per facet and matching
on it.

## Change

`QueryBuilder::buildFacets()` now emits each facet under its own
`{!key=<delta>}<field>`, where `$delta` is the array key of the
`search_api_facets` option (the id Search API uses to look results back up).
Two facets on one field now answer under two distinct keys. The single-facet
shape is unchanged in structure — `count($fields) === 1 ? $fields[0] : $fields`
still yields one string, now `{!key=<delta>}<field>`.

`ResponseParser::parseFacets()` matches the response key back to the delta
directly. It registers **both** the delta and the mapped field name as keys for
each delta: the normal response is delta-keyed (the `{!key=...}` label), and the
hostile-delta fallback (below) is field-name-keyed, so only one of the two ever
appears and either resolves. The `ponytail:` descope comment at its site is
deleted — the descope is closed.

The term/`filter` contract is untouched: double-quoted values, `'!'` for the
`null` missing bucket. `BackendTestBase::checkFacets()` compares that array raw.

### Hostile-delta fallback

The delta is "in practice the facet's field identifier" (a machine name) but is
not constrained to be one. A delta carrying `}` or whitespace would break out of
the `{!key=...}` local-params block (`src/local_params.rs` terminates on `}` and
splits pairs on whitespace). So a delta that is not `[A-Za-z0-9_:-]+` falls back
to the bare mapped field name — the core then keys that facet's buckets by the
field name, and `parseFacets()` resolves it via the field-name registration. The
safe-delta check lives in `QueryBuilder` only; `ResponseParser` needs no regex
because it registers both keys regardless. Mutation-tested: removing the guard
makes `testHostileFacetDeltaFallsBackToBareFieldName` see
`{!key=bad delta}ss_category` and fail.

`README.md`'s "Two facets on the same field collapse" descope bullet is removed.

## What this does NOT fix

- **Per-facet settings** — `buildFacets()` still writes `facet.limit`,
  `facet.mincount`, `facet.sort`, `facet.missing` as global params, so two facets
  on one field still share one settings set (last wins). That needs
  `f.<field>.facet.*`, which is **#296**. Its README bullet stays.
- **OR facets** — still need `{!ex}`/`{!tag}`, which is **#295**. Its README
  bullet stays.

## Tests

TDD, tests first and confirmed red for the right reason before implementation.
New test methods are inserted beside the existing facet tests (before the MLT
block in `QueryBuilderTest`, before the highlighting block in
`ResponseParserTest`) — not appended at the class tail — so they do not collide
with sibling branch #297's MLT test additions.

- `QueryBuilderTest` —
  `testTwoFacetsOnOneFieldEmitDistinctKeyedFacetFieldEntries` (the core case:
  two facets over `category` → two distinct `{!key=}` entries over `ss_category`);
  `testHostileFacetDeltaFallsBackToBareFieldName` (the guard). Three existing
  facet tests had their `facet.field` assertions bumped to the new
  `{!key=<delta>}<field>` wire shape (legitimate wire-format change, not a
  relaxed assertion).
- `ResponseParserTest` —
  `testParseAttachesBothDeltasWhenTwoFacetsShareOneField` (response shape
  derived from `facet_extag_both_facets.json`: two distinct keys, different
  counts, both deltas populated); `testParseResolvesAHostileDeltaByItsBareFieldNameKey`
  (the inverse of the QB hostile-delta test). Three existing facet tests had
  their synthetic response keys moved from the mapped field name to the delta,
  reflecting the new normal response shape.

## Gates

```
cd drupal/search_api_wayfinder && composer install && vendor/bin/phpunit
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

- phpunit: 249 tests, OK (193 PHPUnit deprecations, 0 failures).
- `cargo fmt --check`: clean.
- `cargo clippy --all-targets -- -D warnings`: clean.
- `cargo test`: all suites ok, 0 failed.

No `capture.sh` run — every fixture this needed (`facet_extag_both_facets.json`,
`facet_local_params_key.json`) was already committed.

## Sequencing

Sequence against **#297**, which also edits `QueryBuilder.php`. The shared test
file collision (`QueryBuilderTest.php`) is avoided by insertion-point discipline
(facet tests beside facet tests, MLT tests beside MLT tests). Rebase onto
`main` before merging and re-run the gates — #295/#308 both touch the same
`EXPECTED_DIVERGENCES` block, which is exactly where a green branch plus a green
main fails to imply a green merge.
