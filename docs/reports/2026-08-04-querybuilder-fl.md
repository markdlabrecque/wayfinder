# Issue #361 — QueryBuilder `fl` projection

## Outcome

`QueryBuilder::build()` now sends `fl=id,index_id,score` on normal `/select`
requests. The projection contains everything the v1 result-item path needs and
does not return the index-side `twm_suggest` or `spellcheck_*` sink fields.

The change deliberately does not alter stored fields, `mlt.fl`, `terms.fl`, or
`hl.fl`.

## Compatibility findings

1. Wayfinder's select renderer accepts literal field names and positive `*`
   globs. `src/core_index.rs:679-681` defines the matcher and gives only `*`
   special meaning; `src/core_index.rs:3100-3131` documents and applies literal
   and wildcard inclusion. There is no negative/exclusion form. The captured
   fixtures `solr-ref/responses/select_fl_missing.json` and
   `solr-ref/responses/select_fl_reversed.json` establish literal selection and
   silent omission of unknown fields; the wildcard suite is in
   `tests/select_fl_wildcard.rs`.
2. Vendored `search_api_solr` 4.4.0 always calls `setFields()` for select
   requests. Its baseline required fields are Search API ID, language, and
   relevance (`coverage/search_api_solr_4.4.0_source/src/Plugin/search_api/backend/SearchApiSolrBackend.php:2108-2145`).
   It either returns requested mapped values plus that required set, the
   required set alone for rendered Views, or `*,score` when configured to
   retrieve data without an explicit field selection (`:2163-2202`).
3. Wayfinder's `ResponseParser::parse()` reads only `id` and `score` from each
   document (`drupal/search_api_wayfinder/src/ResponseParser.php:66-72`). It
   derives the Search API item ID by removing the query index's prefix from the
   composite document ID. The backend's locked v1 contract is
   `id,index_id,score`; Search API reloads entities by ID, and support for
   `search_api_retrieved_field_values` is deferred
   (`docs/plans/57-search-api-wayfinder-backend.md:165-168`).

Because the consumer contract is three fixed fields, the lack of exclusion
syntax does not force a brittle enumeration of dynamic schema fields. Using `*`
would reproduce the defect by including both sink families.

## Tests

Tests were added first in
`drupal/search_api_wayfinder/tests/src/Unit/QueryBuilderTest.php`. Before the
implementation, the focused run failed because `fl` was absent (one failed
exact-contract assertion and three dependent errors). After implementation:

- the exact projection is asserted;
- both sink families are asserted absent;
- `id` and `score`, the fields read by `ResponseParser`, are individually
  asserted present;
- a stored document containing ordinary content plus both sinks is projected
  under the generated `fl`, then parsed into a real Search API result item with
  its identity and score intact;
- the existing tests continue to pin `mlt.fl`, `terms.fl`, and `hl.fl`.

Validation completed on 2026-08-04:

- `vendor/bin/phpunit tests/src/Unit/QueryBuilderTest.php` — 101 tests, 155
  assertions passed;
- `vendor/bin/phpunit` — 418 tests, 730 assertions passed;
- `cargo fmt --check` — passed;
- `cargo clippy --all-targets -- -D warnings` — passed;
- `cargo test` — passed, hermetic.
