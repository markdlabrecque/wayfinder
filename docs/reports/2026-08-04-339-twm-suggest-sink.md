# #339 — accumulate every `solr_text_suggester` field into the `twm_suggest` sink

**Date:** 2026-08-04. **Branch:** `markdlabrecque/issue-339-twm-suggest-sink`.
**Spec:** the ceiling recorded in
`docs/reports/2026-08-08-300-non-default-data-types.md:110` and its "Follow-ups"
entry — `DocumentBuilder` assigns `$doc['twm_suggest']` per field (last wins),
where `search_api_solr` accumulates.

`DocumentBuilder::buildAddCommand()` assigned `$doc['twm_suggest'] = ...` per
field. Because `FieldMapper::fieldName()` collapses every `solr_text_suggester`
field to the one fixed sink field, an item with two or more suggester fields
silently lost all but the last field's suggestions. Autocomplete (live over
`/terms` since #291) returned a short list with no error — a silent
data-integrity bug, not a crash.

## What changed

`drupal/search_api_wayfinder/src/DocumentBuilder.php` only. A dedicated
suggester branch accumulates with
`array_merge($doc[$name] ?? [], array_values($formatted))` in item-field
iteration order, then `continue`s. The `continue` is load-bearing: falling
through to the generic assignment re-assigns the key and destroys the
accumulation just built (confirmed by mutation test, below). No `FieldMapper`
change was needed — the fixed-sink mapping from #300 was already correct; only
`DocumentBuilder`'s per-field assignment was wrong.

Commits: `fc7c695` (red tests), `d9602af` (implementation, closes #339),
`afa9e03` (review follow-ups).

## Deliberate divergence (`ponytail:` comment in `DocumentBuilder.php`)

The sink is **always an array**, regardless of each contributing field's own
cardinality, because `presets/search-api.toml` declares `twm_suggest` as
`multi_valued = true`. Solarium's `Document::addField()` emits a scalar when a
key ends up with exactly one value, so this differs from `search_api_solr`'s
request body shape for the single-suggester-field case. Judged safe:

- No `solr-ref` fixture captures an `/update` request body, so no captured
  behaviour is contradicted — the shape is request-side and invisible in any
  response fixture.
- `src/core_index.rs:1127-1131` unwraps a one-element array to a scalar
  (finding 48e), and the `>1`-value rejection at `core_index.rs:1139` cannot
  fire for a `multi_valued` field.
- `tests/search_api_preset.rs:518` and `:1215` already send and expect an
  array for this field.
- Autocomplete reads the sink via `terms.fl=twm_suggest`, never the raw
  indexed document.

## Testing

Four new `DocumentBuilderTest` cases, written red first and committed
separately from the implementation (`fc7c695`):

- two-suggester accumulation, asserting item-field iteration order is
  preserved
- one-element-array shape for a single single-valued suggester field
- a zero-value suggester field creates no key (regression guard — this case
  was already green against `main`'s `DocumentBuilder`, confirming it wasn't
  broken and won't be)
- suggester fields alongside plain text/typed fields, with no `sort_*` copy
  created for the sink

3 of the 4 cases were confirmed failing against `main`'s `DocumentBuilder` in
an isolated copy before the fix (the zero-value case is the one exception, by
design — it's a guard, not a red-test target).

**Mutation guards** (the fix's whole value is the accumulate-not-overwrite
behaviour, so it was mutation-tested):

- plain assignment instead of `array_merge` → 2 test failures
- reversed `array_merge` argument order → 2 failures (proves ordering is
  genuinely pinned by the test, not incidental)
- removing the `continue` → 1 failure + 2 errors
- forcing scalar-when-one shape (instead of always-array) → 1 failure

All four reverted to green after confirming.

## Commands / results

```
cd drupal/search_api_wayfinder && vendor/bin/phpunit   # 338 tests / 630 assertions OK (was 334)
cargo fmt --check                                       # clean
cargo clippy --all-targets -- -D warnings               # clean
cargo test                                              # 1261 passed across 63 suites
```

Gates were re-run independently by the reviewer stage, not just self-reported
by the implementor.

## Review

Independent reviewer stage (Opus), read-only. Verdict: **APPROVED**, no
must-fix items. Four nice-to-have items were raised; two were applied in
`afa9e03`, and two were judged out of scope and are carried forward below as
open follow-ups rather than filed as issues, per instruction for this report.

## Follow-ups (observations, not filed as issues)

- **Pre-existing, undocumented `sort_*` gap:** `search_api_solr` creates
  `sort_*` copies for any field name starting with `t` or `s` (excluding
  `twm_suggest`/`spellcheck`, `SearchApiSolrBackend.php:1453`), plus a
  single-valued twin for names matching `^([a-z]+)m(_.*)`. That means
  `tus_*`/`tom_*`/`tws_*`/`zs_*`/`zm_*` all get sort copies upstream, while
  Wayfinder's exact `$type === 'text'` guard produces none for them. Worth its
  own issue.
- **Pre-existing field-name collision:** a multi-valued `solr_text_wstoken`
  field whose id is literally `suggest` maps to `twm_suggest` (`tw` + `m` +
  `_suggest`). If it is iterated after a suggester field, the generic
  (non-suggester) assignment path clobbers the accumulated sink. Not
  introduced by #339 and not guarded speculatively by this fix.
- Also noted: `search_api_solr` excludes the sink field from `sort_*` copies
  by name, which is why this fix's `continue` (skipping the generic path
  entirely for the sink) matches upstream behaviour rather than diverging
  from it.
