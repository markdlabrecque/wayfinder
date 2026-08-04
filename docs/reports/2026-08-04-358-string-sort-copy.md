# Issue #358 — string-field sort copies

Date: 2026-08-04
Branch: `358-string-sort-copy`

## Outcome

`search_api_wayfinder` now writes language-specific `sort_*` copies for
ordinary string fields as well as text fields. Query sorting resolves string
fields to those copies, while grouping continues to use the ordinary
single-valued `ss_*` field, matching upstream's separate grouping path.

The copy remains scalar and first-value-wins. Existing text behavior is
unchanged, and numeric fields still receive no `sort_*` copy.

## Verification findings

1. The vendored `search_api_solr` 4.4.0 source uses a first-character gate on
   the mapped Solr field name, not a narrow prefix list:
   `SearchApiSolrBackend.php:1447-1455` accepts names beginning with `t` or
   `s`, then excludes `twm_suggest` and `spellcheck`. It writes the copy at
   lines 1482-1485.
2. The existing real-client/real-Solr capture answers the multi-value question.
   `solr-ref/search-api/trace/00001.json` contains
   `sm_field_keywords=["animals","classic","pangram"]` with
   `sort_X3b_en_field_keywords="animals"`, and
   `sm_field_topics=["legacy","documentation"]` with
   `sort_X3b_en_field_topics="legacy"`. String sort copies therefore take the
   first indexed value, just like text; no assumption or new `ponytail:` is
   needed. The same trace pins the single-valued shape:
   `ss_field_sku="ART-001"` becomes
   `sort_X3b_en_field_sku="ART-001"`.
3. `FieldMapper::sortFieldName()` was text-specific: it tested only
   `isTextPrefix(...)`, so a string sort resolved to `ss_*`/`sm_*`. The mapper
   now shares the upstream mapped-name gate with `DocumentBuilder`. Actual
   sort callers pass a resolved language explicitly and receive `sort_*`;
   grouping callers omit one and retain the upstream `ss_*` grouping field.

## Implementation

- `DocumentBuilder` applies the sort-copy write to mapped `t*` and `s*` fields,
  preserving the existing first-write and first-value behavior and the
  suggester/spellcheck exclusions.
- `FieldMapper` centralizes that mapped-name predicate and resolves explicit
  string sorts to `sort_X3b_<language>_<field>`.
- The query-sort regression expectation now pins a multi-valued string sort to
  `sort_X3b_und_tags`; reserved Search API pseudo-fields remain unchanged.

No fixture was added or recaptured: trace `00001` already contains both the
single- and multi-valued string cases required by the scope authority.

## Test-first evidence

Before production changes, the focused PHPUnit run failed four assertions for
the intended reasons:

- `DocumentBuilder` emitted no `sort_X3b_en_field_sku` or
  `sort_X3b_en_field_keywords` keys.
- `FieldMapper` returned `ss_field_sku` and `sm_field_keywords` instead of the
  trace-derived `sort_*` names.

After the fix, the focused tests and full module suite pass. Coverage includes
single-valued string, multi-valued string with a first value that is neither
minimum nor maximum, the pre-existing explicit text regressions, and a numeric
field negative case.

## Validation

- `cd drupal/search_api_wayfinder && vendor/bin/phpunit` — 423 tests, 741
  assertions, green (PHPUnit reports the suite's existing deprecations).
- `cargo fmt --check` — green.
- `cargo clippy --all-targets -- -D warnings` — green.
- `cargo test` — green and hermetic, including the differential fixture suite.
