# #291 — suggest: the SuggestComponent path and search_api_autocomplete integration

**Date:** 2026-08-03
**Branch:** `markdlabrecque/issue-291-suggestcomponent-path-search`
**Closes:** #291

## Decision (the scope question the issue opened with)

> Decide, from `coverage/search_api_solr_4.4.0_source`, which of the two paths
> the module actually uses for autocomplete: `terms`-based, `suggest`-based, or
> both. **Build only the evidenced path.**

**Answer: terms-based only.** Recorded as findings 154–156 in
`docs/solr-ref-findings.md`, grounded in a fresh source sweep of both
`search_api_solr` 4.4.0 (the vendored three-file snapshot) and the fetched
`search_api_autocomplete` `8.x-1.x` source.

- `SolrConnectorPluginBase::getSuggesterQuery()` (`createSuggester()`,
  lines 935–937) is **defined but never called** anywhere in the backend. The
  only other "Suggester" mentions are a UI warning string and the comment
  marking `twm_suggest` as the suggester's backing field.
- `getAutocompleteSuggestions()` (3973–3994) routes through
  `setAutocompleteTermQuery()` (4033–4039), which sets exactly
  `terms.fl`/`terms.prefix`/`terms.limit` on Solarium's **Terms** component.
  The `twm_suggest` sink field is reached *as a `terms.fl` value* (4011–4012:
  "We explicit allow to get terms from twm_suggest"), never as a
  `suggest.dictionary`.

So `search_api_solr` 4.4.0 has **no `/suggest` emission**, and the
`terms_prefix_*`/`terms_limit_*` fixtures captured in #308 (findings 141/142)
are the wire evidence for the path that is actually used.

**Consequence: no `/suggest` server route is built.** There is no client
evidence to build it from, and adding one without a capture would violate the
compatibility contract ("A feature with no fixtures needs new ones"). The
already-shipped `/terms` endpoint (`src/lib.rs`, landed in #155 and widened to
`terms.prefix`/`terms.limit` in #308) is the whole server side of this feature.

## What changed

The remaining work was the **Drupal-side surface** over `/terms`:

1. **`WayfinderBackend::getSupportedFeatures()`** advertises
   `'search_api_autocomplete'`. The Server suggester's `getBackend()` gates on
   `supportsFeature('search_api_autocomplete')` (OR-ed with the `instanceof`
   checks — finding 155), so the flag alone activates the backend. Like
   `search_api_solr`, the backend does **not** formally implement the
   (documentation-only) `AutocompleteBackendInterface`; the method is
   duck-typed.

2. **`WayfinderBackend::getAutocompleteSuggestions()`** (new) — the duck-typed
   `AutocompleteBackendInterface` method. Builds a `/terms` request via
   `QueryBuilder`, sends it via `WayfinderClient::terms()`, and folds the
   interleaved `[term, count, ...]` lists into `SuggestionInterface[]` via
   `SuggestionFactory::createFromSuggestionSuffix(suffix, count)` — mirroring
   `search_api_solr`'s `getAutocompleteTermSuggestions` (finding 156),
   including the cross-field term merge. A transport failure degrades to an
   empty suggestion list (not an exception out of the widget), mirroring
   `search_api_solr`'s catch around the autocomplete query.

3. **`QueryBuilder::buildAutocompleteTerms()`** (new) — emits
   `terms=true`/`terms.fl`/`terms.prefix`/`terms.limit`/`omitHeader=true`. The
   field set is the query's fulltext fields intersected with the index, mapped
   through `FieldMapper::fieldName()`; every `solr_text_suggester` field
   collapses to `twm_suggest` (#300/finding 151), deduped so the dictionary is
   not requested twice. No `q`/`fq`: the Terms component scans the dictionary,
   it does not run a search.

4. **`WayfinderClient::terms()`** (new) — `GET {core}/terms`, same transport
   and error-envelope handling as `select()`/`mlt()`.

5. **`composer.json`** adds `drupal/search_api_autocomplete: ^1.0` to
   `require-dev` with a matching `autoload-dev` shim, so the PHPUnit suite can
   exercise the real `SuggestionFactory`. It is **not** in the module's
   `.info.yml` dependencies: `search_api_autocomplete` is a *soft* runtime
   dependency (the backend duck-types the method, only ever invoked from inside
   an installed `search_api_autocomplete`), matching `search_api_solr`'s own
   approach.

6. **`docs/solr-ref-findings.md`** — findings 154–156 (the decision and the
   discovery/suggestion contracts).

7. **`README.md`** — corrected a wrong premise: the `twm_suggest` sink field is
   read by the **terms** component (`terms.fl=twm_suggest`), not the
   SuggestComponent.

## Why no new fixtures

The issue said "Needs new fixtures against a configured `solr:9` suggester."
That was premised on building `/suggest`. Since the evidenced path is `/terms`
and its fixtures already exist (`terms_prefix_*`/`terms_limit_*` from #308), no
new capture is needed and `solr-ref/capture.sh` is unchanged. The Drupal side
is unit-tested with a mocked `WayfinderClient` (no Solr), the same hermetic
gate contract as the rest of the suite.

## Testing

TDD: red tests first (confirmed failing for the right reasons — undefined
methods / absent feature flag), then implementation to green.

- `WayfinderClientTest`: `terms()` transport — decoded body on 200, requests
  `/terms` (not `/select`), error envelope → `SearchApiException`.
- `QueryBuilderTest`: `buildAutocompleteTerms()` — the evidenced param set,
  default limit 10, `twm_suggest` dedup across `solr_text_suggester` fields,
  multi-field as repeated `terms.fl`, honours the query's fulltext-field
  subset.
- `WayfinderBackendTest`: `getSupportedFeatures` includes
  `search_api_autocomplete`; `getAutocompleteSuggestions` builds the terms
  query and parses suggestions (suffix = term minus the typed prefix), merges
  terms across fields, returns `[]` on an empty/absent terms block, and
  returns `[]` on a transport error.

The transport-error guard is **mutation-tested**: removing the `try`/`catch`
makes `testGetAutocompleteSuggestionsReturnsEmptyArrayOnTransportError` fail
(the exception propagates instead of returning `[]`), then reverted.

## Gates (all green)

```
cargo fmt --check                                    # clean
cargo clippy --all-targets -- -D warnings            # clean
cargo test                                           # 61 suites, 0 failures
cargo fmt --check --manifest-path bench/Cargo.toml   # clean
cargo clippy --manifest-path bench/Cargo.toml ...    # clean
cargo test --manifest-path bench/Cargo.toml          # clean
cd drupal/search_api_wayfinder && vendor/bin/phpunit # 333 tests (was 318, +15)
```

## Follow-ups

- **End-to-end integration** is not added to the manual Docker harness
  (`WAYFINDER_INTEGRATION=1 …/run.sh`); the unit tests cover the logic with
  mocked transport. A future slice could extend that harness to drive a real
  `search_api_autocomplete` Server suggester against a live Wayfinder, the same
  way it already drives `search()`.
- `$search->getOptions()` is not consulted (result-count estimation etc.).
  `search_api_solr`'s terms path passes the term frequency as the result count
  unconditionally, which this matches; the option is for richer suggestion
  shapes none of the captured evidence exercises.
- If a real client ever emits `/suggest` (a future `search_api_solr` version or
  a non-stock suggester plugin), that is the trigger to capture fixtures and
  build the route — not this issue.
