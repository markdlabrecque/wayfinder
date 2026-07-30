# Issue #78 — search_api_wayfinder M4 (MLT + highlighting), parent #57

Worktree: `/Users/mark/Projects/wayfinder-78-mlt-hl`
Branch: `78-search-api-wayfinder-mlt-hl` (1 ahead of `origin/main`: the red-test
commit `48a55ed test(search-api): red tests for M4 MLT + highlighting`, on top of
`93bb1eb feat(search-api): M3 facets for search_api_wayfinder backend (#91)`).
The implementation itself is uncommitted in the working tree — see "Outstanding
process note" below.

## What was built

MLT (More Like This) routing and highlighting for the Drupal Search API backend
module, per M4 of `docs/plans/57-search-api-wayfinder-backend.md`:

- `src/QueryBuilder.php`: `build()` gained a second parameter,
  `bool $highlighting = FALSE` — deliberately not read off the query as a query
  option. Core's own `Highlight` processor
  (`vendor/drupal/search_api/src/Plugin/search_api/processor/Highlight.php`)
  never touches the query object; it only reads back `highlighted_fields` extra
  data a backend already populated, so there is no per-query hook to key off.
  The plan doc's locked decision pins this module to `search_api_solr`'s
  convention instead: a backend-level config setting, with the backend passing
  the flag in. New public `buildMlt(QueryInterface $query): array` routes on the
  `search_api_mlt` query option (`['id' => <item id>, 'fields' => <field ids>]`,
  shape confirmed against
  `\Drupal\search_api\Plugin\views\argument\SearchApiMoreLikeThis::query()`, the
  only core call site that sets it). It builds
  `q = 'id:' . filterValue($index->id() . '-' . $option['id'], 'string')` (escaped
  through the same `FieldMapper::filterValue()` every other value in this class
  goes through — see round 1 below) and `mlt.fl` as a comma-joined list of mapped
  fields, matching the captured comma convention
  (`solr-ref/responses/mlt_baseline.json`). Extracted `buildPaging()`,
  `buildHighlighting()`, `mapFieldNames()`, `fulltextFieldIds()` as private
  helpers shared between `build()` and `buildMlt()`.
- `src/ResponseParser.php`: `parse()` now reads a `highlighting` response block,
  reverse-maps dynamic field names (e.g. `ts_body`) back to Search API field
  ids, and sets `highlighted_fields` extra data on matching items. Absent
  entirely (not an empty array) when the response has no `highlighting` block.
- `src/WayfinderClient.php`: new `mlt(array $params): array`, mirroring
  `select()`'s HTTP call and error-handling shape exactly.
- `src/Plugin/search_api/backend/WayfinderBackend.php`: `getSupportedFeatures()`
  adds `search_api_mlt`. `search()` routes to `mlt()`/`buildMlt()` when the
  `search_api_mlt` option is present, otherwise `select()` with
  `build($query, !empty($this->configuration['highlight']))`. Added a real
  `highlight` boolean to `defaultConfiguration()`, the config form, and
  `submitConfigurationForm()`.
- `config/schema/search_api_wayfinder.schema.yml`: added `highlight: boolean`.

## Test evidence

- `vendor/bin/phpunit` (from `drupal/search_api_wayfinder/`): 110 tests, 160
  assertions, green. 82 pre-existing PHPUnit-11 `@covers`-deprecation warnings
  (up from 80 pre-M4 by exactly 2 — one per new test, not a new warning class).
- `cargo test`: 490 passed, 23 suites, green — unaffected, no Rust changes this
  milestone.
- Mutation check: reverting each of the three round-1 fixes individually
  reproduces the corresponding test failure; restoring returns green.

## Review outcome

Round 1 — **BOUNCE**, 3 must-fix:
1. `buildMlt()` emitted a dead `fq=index_id:"..."` scoping filter with a false
   comment claiming it prevented cross-index MLT leakage. Wayfinder's
   `MLT_PARAMS` (`src/lib.rs:116-132`) has no `fq` and the `/mlt` handler never
   reads it, so the param was silently dropped (or 400'd under
   `strict_params`) while doing nothing. Removed; replaced with a `ponytail:`
   comment naming the real ceiling — MLT is unscoped by index until
   server-side support lands, a genuine documented gap, not papered over.
2. The MLT seed item id was interpolated into the query unescaped
   (`'id:"' . $index->id() . '-' . $option['id'] . '"'`) — a real injection
   hole, since Search API item ids are datasource-derived and can contain `"`
   or `\`. Fixed to reuse `FieldMapper::filterValue()`'s existing escaping,
   with a new test covering a quote/backslash-bearing item id.
3. The pre-existing `InvalidArgumentException` guard for a missing
   `search_api_mlt.id` had no test coverage. Added one, mutation-tested.

Both new tests (`testBuildMltEscapesQuotesAndBackslashesInTheSeedItemId`,
`testBuildMltRejectsAnMltOptionWithoutASeedItemId`) were added to
`tests/src/Unit/QueryBuilderTest.php` only — no existing test modified or
weakened.

Round 2 — **APPROVED**. All three fixes independently re-verified against
`src/lib.rs`, `FieldMapper.php`'s escaping order, and the new tests' actual
exercised code path (confirmed non-vacuous). The decision to leave
`InvalidArgumentException` rather than switch to `SearchApiException` (round 1
flagged this as nice-to-fix, not must-fix) was confirmed as the consistent
choice — `buildCondition`, `buildFacets`, and `listValues` all already throw the
same type from the same call path.

Two review rounds were used but the process is capped at 2 by policy; this
work could use a further pass, particularly on the MLT cross-index scoping gap
and the highlighting/MLT interaction noted below.

## Follow-ups (not yet actioned)

- Convert `QueryBuilder`'s four `InvalidArgumentException` throws (including the
  new MLT guard) to `SearchApiException` together in one pass, since
  `BackendSpecificInterface::search()` contracts that type and all four
  currently diverge from it consistently. Won't WSOD today since
  `SearchApiQuery::execute()` catches `\Exception` broadly, but worth aligning.
- `buildMlt()` emits `mlt.fl` unconditionally, so an MLT option with
  empty/unmapped `fields` sends a bare `mlt.fl=`; traced server-side
  (`core_index.rs:1777`) to produce an empty result set (semantically "no
  similar docs") rather than erroring, which is benign but diverges from
  Solr's "all fields" default when `mlt.fl` is absent.
- Highlighting is only wired on the `/select` path; an MLT query never
  requests `hl` even with the backend's `highlight` config on, since
  Wayfinder's `MLT_PARAMS` doesn't include `hl`/`hl.fl` at all. This is forced
  by the server's current param set, not a client-side gap, but worth
  revisiting if MLT result teasers become a requirement.
- Config schema: `highlight` (and pre-existing `port`/`timeout`)
  checkbox/textfield values arrive as PHP int/string but the schema declares
  stricter types (`boolean`/`integer`) — same pre-existing casting gap across
  all three, worth fixing in one pass.
- Neither `build()` nor `buildMlt()` sends `fl`, so `ResponseParser` always
  falls back to `score = 1.0` for every item (pre-existing since M1, not
  introduced by M4, but touches the same response-parsing code this milestone
  extended).
- MLT's cross-index leakage gap named in round 1's fix (#1 above) is a real,
  currently-shipping limitation, not just a comment: a core holding more than
  one Wayfinder-backed index can have MLT return documents from a sibling
  index. The fix is server-side (`/mlt` honouring `fq`, with its own captured
  fixture) and is out of scope for this client-side milestone.

## Outstanding process note

The implementation (all files under "What was built" except the red-test
commit) is not yet committed in the worktree — it sits in the working tree on
top of `48a55ed`. It still needs a conventional commit and a PR per this repo's
workflow (`Closes #78`) before it can merge.
