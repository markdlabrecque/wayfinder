# Issue #77 — search_api_wayfinder M3 (facets), parent #57

Worktree: `/Users/mark/Projects/wayfinder-77-facets`
Branch: `77-search-api-wayfinder-facets` (uncommitted at time of writing; HEAD is
`488c79e feat(search-api): translate filters sorts and paging`, the #76 work this
branch builds on — no `main` merge pending changes beyond that ancestor).

## What was built

Facet support for the Drupal Search API backend module, per M3 of
`docs/plans/57-search-api-wayfinder-backend.md`:

- `src/QueryBuilder.php`: new private `buildFacets()`, called from `build()`. Maps
  `facet.field` through the same `FieldMapper` used for filters/sorts. Global facet
  params (`facet.limit`/`facet.mincount`/`facet.missing`/`facet.sort`) resolve
  "last facet that states a setting wins" — Wayfinder core has no per-field facet
  override, a real wire limitation, documented with a `ponytail:` comment
  (~line 298-303). `facet.missing` is emitted as the literal strings `'true'`/`'false'`,
  `isset`-guarded like the other params. `limit => 0` (Search API's "no limit"
  convention) is translated to Wayfinder's `-1` unlimited sentinel rather than
  passed through verbatim — `facet.limit=0` on the wire truncates to zero buckets
  per `solr-ref/responses/facet_limit_zero.json` vs `facet_limit_unlimited.json`.
- `src/ResponseParser.php`: new private `parseFacets()`. Builds a facet-delta-to
  -mapped-field-name lookup from the query's `search_api_facets` option, walks
  Solr's flat `facet_counts.facet_fields` array-pairs shape, emits
  `['count' => int, 'filter' => string]` per bucket. Filter values are
  double-quoted (e.g. `'"article_category"'`) to match `search_api_solr`'s
  `Database.php:2848` convention and the vendored `BackendTestBase::checkFacets()`
  conformance expectations. `'!'` is the sentinel for the missing/null bucket.
- `src/WayfinderClient.php`: `encodeQuery()` generalized from special-casing only
  `fq` arrays to handling any array-valued param (needed for multi-value
  `facet.field`) — reviewed for safety: only 3 call sites (`select`/`update`/`ping`),
  only `fq` and `facet.field` reach it as arrays, both use the same repeated-key
  wire convention Wayfinder core expects.
- `src/Plugin/search_api/backend/WayfinderBackend.php`: `getSupportedFeatures()`
  now returns `['search_api_facets']` (was `[]`), with a `ponytail:` comment noting
  MLT (M4) is intentionally still unadvertised and that Wayfinder's facets are
  AND-only (no `{!ex}`/`{!tag}` OR-facet support).

## Test evidence

- `vendor/bin/phpunit` (from `drupal/search_api_wayfinder/`): 94 tests, 137
  assertions, green. 66 pre-existing PHPUnit-11 deprecation warnings, unrelated to
  this diff — reproduced in isolation against untouched test files.
- `cargo test`: 490 passed, 23 suites, green.
- Mutation check: reverting both round-1 fixes (unquoted filter value,
  `facet.limit=0` passthrough) simultaneously produces 3 failures; restoring
  returns green.

## Review outcome

Round 1 — **BOUNCE**, 2 must-fix:
1. Facet `filter` values must be double-quoted, not bare — confirmed against the
   vendored `BackendTestBase` conformance tests and `search_api_solr`'s
   `Database.php:2848`.
2. `limit => 0` must map to Wayfinder's `-1` unlimited sentinel, not pass through
   as `facet.limit=0` — confirmed against `solr-ref/responses/facet_limit_zero.json`
   / `facet_limit_unlimited.json` and `src/facet.rs`.

Both fixed. Two non-blocking items were also fixed proactively in the same round:
`facet.missing` isset-guard consistency, and a `ponytail:` comment naming the
delta-collision-on-duplicate-field-name behavior.

Round 2 — **APPROVED**. Both fixes independently re-verified against the same
source citations (not just a re-read of the round-1 report). Reviewer additionally
confirmed `src/facet.rs:218-221`'s `facet.sort` default derivation isn't disturbed
by the 0→-1 translation, and that all `facet.*` params are present in
`SELECT_PARAMS` (`src/lib.rs:80-86`) so `strict_params=true` won't 400 them.

Two review rounds were used but the process is capped at 2 by policy; this work
could use a further pass, particularly on the OR-facet conformance gap noted below.

## Follow-ups (not yet actioned)

- `WayfinderClientTest.php` only pins the `encodeQuery()` array-repeat
  generalization via `fq`; add a `facet.field` case too.
- Declaring `search_api_facets` (without `_operator_or`) is correct per the v2
  plan's locked decision, but means 3 of 4 blocks in the vendored
  `BackendTestBase::checkFacets()` assert OR-facet tag-exclusion semantics
  Wayfinder cannot express. This repo's `phpunit.xml.dist` is unit-only so nothing
  fails today, but it should be recorded as a known/deliberate conformance
  divergence (analogous to `EXPECTED_DIVERGENCES` in `tests/differential.rs` on
  the Rust side) before M4 builds further on this module.
- `ResponseParser::parseFacets()` returns `[]` (not per-delta empty arrays) when
  facets were requested but the response has no `facet_counts` — could produce an
  undefined-key access in a naive consumer.
- `QueryBuilder::buildFacets()` doesn't de-duplicate `facet.field` when two facet
  deltas map to the same field, causing Wayfinder core to do the aggregation
  twice (wasted work, not incorrect). A `ponytail:` comment already names the
  *result*-collision consequence but not this redundancy.

## Outstanding process note

This work is not yet committed in the worktree (all changes above are in the
working tree on top of `488c79e`). It still needs a conventional-commit and a PR
per this repo's workflow (`Closes #77`) before it can merge.
