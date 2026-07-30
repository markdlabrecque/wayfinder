# Issue #79 — search_api_wayfinder M5 (polish), final milestone of #57

Worktree: `/Users/mark/Projects/wayfinder-79-polish`
Branch: `79-search-api-wayfinder-polish`

M5 is the final milestone of parent issue #57 (search_api_wayfinder Drupal
backend). M3 (#77, facets) and M4 (#78, MLT + highlighting) are already merged
to `main`; this closes out the parent.

## What was built

- **`src/WayfinderClient.php`**: new `adminSystem()` — `GET {core}/admin/system?wt=json`,
  reusing the same private `request()` transport as `select()`/`mlt()`, so a
  non-200 Solr error envelope becomes a `SearchApiException` the same way it
  does for every other endpoint. Class docblock's endpoint list updated.
- **`src/Plugin/search_api/backend/WayfinderBackend.php`**: `viewSettings()` now
  performs a version handshake — calls `adminSystem()` and appends a "Wayfinder
  version" row read from `lucene.solr-spec-version` (ground truth:
  `solr-ref/responses/admin_system.json`). Degrades to the server-URL row alone
  on `SearchApiException` — deliberately not `\Throwable`, which would also
  swallow a genuine `Error`/`TypeError` on an admin page. Also dropped a dead
  `$config` local.
- **`.github/workflows/ci.yml`**: new `search-api-wayfinder-unit` job on the same
  `push`/`pull_request` triggers as the Rust `test` job — `shivammathur/setup-php@v2`
  (PHP 8.3), `composer install`, `vendor/bin/phpunit`,
  `working-directory: drupal/search_api_wayfinder`. Kept strictly separate from
  the pre-existing `WAYFINDER_INTEGRATION=1` Docker harness job, which stays
  `workflow_dispatch`-only; that decision is recorded in a comment in
  `tests/integration/run.sh`.
- **`drupal/search_api_wayfinder/README.md`**: new. Install steps, the
  `presets/search-api.toml` schema pointer, a "Not supported" list of every
  `ponytail:`-marked descope, and a pointer to the integration harness.
- **Config-schema audit**: cross-checked `config/schema/search_api_wayfinder.schema.yml`
  against every key read or written in `WayfinderBackend.php`
  (`scheme`, `host`, `port`, `path`, `core`, `timeout`, `commitWithin`,
  `highlight`) — all eight declared and read/written consistently, confirmed
  directly against current `config/schema/search_api_wayfinder.schema.yml`.
  **No schema changes were needed**; this item closes as an audit with a clean
  result, not a code change.
- **`tests/src/Unit/WayfinderBackendTest.php`**: test changes described under
  Pipeline below.

## Method-level copies from `search_api_solr`, and why

Locked decision 1 of the plan doc permits copying method-level logic from
`search_api_solr` (both modules are GPL-2.0-or-later). Verified against the
current source (`src/FieldMapper.php`, `src/QueryBuilder.php`) rather than
taken on trust from the implementor's draft:

### `src/FieldMapper.php`

1. **Dynamic-field naming convention** (`fieldName()`, `sortFieldName()`) — type
   prefix + `s`/`m` single/multi infix + `_` + field id (`ts_title`, `tm_body`,
   `sort_*`). Copied because `presets/search-api.toml`, the Wayfinder-side
   schema this module targets, is itself derived from the captured
   `search_api_solr` configset (`solr-ref/search-api/configset/schema.xml`);
   the dynamic-field rules on the server *are* `search_api_solr`'s.
2. **Type prefixes** (`FieldMapper::TYPE_PREFIXES`), from `search_api_solr`'s
   `Utility::getDataTypeInfo()`: `text`→`t`, `string`→`s`, `integer`→`it`,
   `decimal`→`ft`, `date`→`d`, `boolean`→`b`. Same reason as (1); `it`/`ft` are
   two-letter and not derivable from the type name.
3. **Filter-value escaping** (`filterValue()`), matching `search_api_solr`
   4.3.13: `text`/`string`/`boolean` values become Lucene phrases, escaping
   only a literal backslash and double quote inside the phrase; numeric/date
   values stay bare. Copied so identical Search API conditions produce
   identical `fq` strings under either backend.
4. **Property-path cardinality walk** (`isMultiValued()` /
   `propertyPathIsMultiValued()`), ported from `search_api_solr`'s
   `getPropertyPathCardinality()`. Reads cardinality from the index's property
   definitions (`getFieldStorageDefinition()->getCardinality()`), not from
   `isList()`, because real content-entity field definitions
   (`FieldDefinitionInterface`) return `TRUE` from `isList()` unconditionally.
   Note: the underlying defect this guards against was already fixed on this
   branch's history (`ea13629`, ahead of M3); this milestone's audit confirms
   the fixed logic is what's in place, not a new change.
5. **Innermost-property unwrapping** (`unwrapProperty()`), mirroring
   `FieldsHelper::getInnerProperty()` — unwraps list definitions to their item
   definition and data-reference definitions to their target definition before
   descending, needed because `FieldDefinitionInterface` extends
   `ListDataDefinitionInterface`, not `ComplexDataDefinitionInterface`.

### `src/QueryBuilder.php`

6. **Highlighting is a backend config setting, not a query option**
   (`build()`'s `$highlighting` argument; plan doc locked decision 6) — Search
   API core's highlight processor never touches the query object, so there is
   no per-query hook to key off; the backend reads its own `highlight` config
   setting instead.
7. **One-member range normalized to a scalar comparison** (`buildCondition()`),
   from `search_api_solr` 4.3.13: a `BETWEEN`/`NOT BETWEEN` with a
   single-member array becomes `=`/`<>` rather than an invalid Lucene range.
8. **NULL/`*` wildcard range endpoints** (`rangeEndpoint()`) — a `NULL` or
   literal `'*'` endpoint renders as `*`; otherwise normal filter-value
   formatting applies.
9. **Empty-array rejection and NULL-member handling in filter-query building**,
   from `search_api_solr` 4.3.13's `createFilterQuery()`, split across two
   sites confirmed present in `src/QueryBuilder.php`: `listValues()` rejects an
   empty `IN`/`NOT IN` array outright (and rejects a literal `'*'` member),
   while `inQuery()`/`notInQuery()` pull a `NULL` member out of the list before
   phrase escaping and turn it into a missing-field alternative
   (`(*:* -field:[* TO *])`) rather than an escaped empty phrase.

(7)-(9) are copied for the same reason as (3): Search API core and contrib
(facets, views) build these condition shapes assuming Solr-backend semantics,
so diverging turns a working query under `search_api_solr` into a syntax error
or a silently different result set under Wayfinder.

## Deliberate descopes

All `ponytail:`-marked in `src/`, and listed in the new README's "Not
supported" section: site hash omitted from doc ids; six default Search API
data types only; multi-valued text sorts use the first value only; MLT not
scoped by index (`/mlt` reads no `fq`); OR facets unadvertised (no
`{!ex}`/`{!tag}`); facet limit/mincount/missing/sort are global, not
per-field; two facets on one field collapse.

## Pipeline

1. **test-writer**: wrote 2 red tests for the `viewSettings()` version
   handshake — `testViewSettingsIncludesVersionStringFromAdminSystem` and
   `testViewSettingsStillIncludesServerUrl`.
2. **implementor**: built `adminSystem()`, the version-handshake
   `viewSettings()` change with graceful degradation, the README, the new CI
   PHPUnit job, and the config-schema audit (no changes needed).
3. **reviewer round 1 — BOUNCED**, 3 must-fix items:
   - The version assertion was `str_contains(..., '9.10.1')`, which matches
     either `lucene.solr-spec-version` *or* `lucene.solr-impl-version` — the
     fixture's impl-version string is `"9.10.1 c135e63... - gerlowskija - ..."`,
     which itself contains `"9.10.1"`. Not hypothetical: `src/lib.rs` emits
     `solr-impl-version` as `format!("{version} wayfinder")`, so reading the
     wrong field against a real server would silently render
     `"9.0.0 wayfinder"` in the admin panel with the original test still green.
   - The graceful-degradation branch and the version guard were untested —
     both original tests fed a 200 response, so the entire `try`/`catch` could
     be deleted with the suite still green.
   - `catch (\Throwable $e)` was overly broad.

   Non-blocking follow-ups noted in the same round: a wrong citation in the
   copy-list (item 9 attributed to `listValues()` alone rather than split
   across `listValues()`/`inQuery()`/`notInQuery()`), a `ponytail:` comment
   gap, a README link clarity issue, and an accepted-risk note on
   `composer.lock` (see Follow-ups below).
4. **implementor fixes**: all 3 must-fix items resolved, plus every
   non-blocking item except the `composer.lock` risk (left as a documented
   risk, not a code fix):
   - Tightened the version assertion to `assertSame('9.10.1', ...)` with a
     label-based row lookup.
   - Added two data-provider-driven tests covering the degradation path:
     `testViewSettingsDegradesGracefullyWhenAdminSystemFails` (error envelope,
     bare non-200, `ConnectException` — both of `WayfinderClient::request()`'s
     failure arms) and `testViewSettingsOmitsVersionRowWhenResponseLacksVersion`
     (missing `lucene` block, missing key, empty string, non-string, empty
     body) — 8 new cases combined.
   - Narrowed the catch to `catch (SearchApiException $e)`.
   - Incidental test-helper fix required to make the label assertion
     meaningful: `createBackend()` now also stubs
     `TranslationInterface::translateString()`. `TranslatableMarkup::render()`
     — what casting a row's `label` to string goes through — calls
     `translateString()`, not `translate()`; without the stub every label
     stringified to `''` and the new assertions would have passed vacuously.
   - Test count: 112 → 120 tests (179 assertions).
5. **reviewer round 2 — APPROVED**. Independently re-ran all 4 of the
   implementor's claimed mutations against a scratch copy rather than trusting
   the transcript, and got matching results in every case:
   - `solr-spec-version` → `solr-impl-version`: 1 failure.
   - delete the `try`/`catch`: 3 errors.
   - weaken the `is_string($version) && $version !== ''` guard to `TRUE`:
     5 failures.
   - drop the `translateString` stub: 1 failure.

   Independently re-ran `vendor/bin/phpunit`: 120 tests, 179 assertions, 0
   failures. Confirmed the `composer.lock` risk is real but acceptable — a
   documentation item, not a blocking bug.

Two review rounds were used, which is this pipeline's cap. Round 1 found 3 real
must-fix defects the green suite had been silently agreeing with (a wrong-field
assertion, an untested failure branch, an overly broad catch); round 2 was a
substantive independent re-verification (all 4 mutations re-run from scratch),
not a rubber stamp. Given the cap was reached, this work could still use a
further review pass, particularly on the broader module (this milestone only
re-reviewed the version-handshake diff, not the full accumulated
`search_api_wayfinder` surface).

## Test evidence

Confirmed directly by the reporter (not just claimed by the implementor/reviewer transcripts):

```
$ cd drupal/search_api_wayfinder && vendor/bin/phpunit
...
OK, but there were issues!
Tests: 120, Assertions: 179, PHPUnit Deprecations: 86.
```

120 tests, 179 assertions, 0 failures, 0 errors — matches the pipeline's
claimed final state. The 86 PHPUnit deprecation warnings are pre-existing
PHPUnit-11 deprecation noise, not failures, and are unrelated to this diff.

Mutation testing (per this repo's convention for code whose whole value is
failing correctly), each reverted after confirming red — see round-2 detail
above for the 4 cases and their results.

`php -l` clean on every touched PHP file; `.github/workflows/ci.yml` parses as
valid YAML. No Rust code touched by this milestone.

## Review outcome

Approved after 2 rounds (the pipeline's cap). Round 1 bounced on 3 real
must-fix defects; round 2 independently re-ran all 4 claimed mutations plus
the full test suite and confirmed the fixes hold. See Pipeline above for the
full detail. As above: the cap was reached, so this milestone's diff has had
exactly the two passes the process allows, not more — flagging per policy that
it could use a further pass.

## Follow-ups

- **`composer.lock` is gitignored, so the new CI job's `composer install`
  floats dependency versions rather than pinning them.** This works today —
  `composer/installers` isn't a transitive dependency that pulls
  `drupal/search_api` to a different vendor layout — but an upstream release
  landing between CI runs could break the pipeline with no code change in this
  repo. Flagged by round-1 review, accepted as a known risk rather than fixed
  in this branch. It's a hermetic-CI reliability concern that isn't specific to
  this milestone (it would affect any future PHP CI job in this repo the same
  way), so it's worth a small follow-up issue to either commit
  `composer.lock` or pin `drupal/search_api`/`composer/installers` versions
  explicitly — but it is not a blocker and doesn't need to gate this PR.
- The copy-list citation for item 9 (empty-array rejection / NULL-member
  handling) was corrected in this report to name `listValues()` +
  `inQuery()`/`notInQuery()` rather than `listValues()` alone, per round-1
  review.
- No further deferred code follow-ups from this milestone's review — the
  3 must-fix items were fully resolved and independently re-verified in
  round 2.
