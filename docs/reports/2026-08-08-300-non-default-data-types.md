# #300 — the `search_api_solr` non-default Search API data types

**Date:** 2026-08-08. **Branch:** `markdlabrecque/issue-300-only-six-default`.
**Spec:** finding **134** (the type enumeration) and the new **151** (the
authoritative prefix table) in `docs/solr-ref-findings.md`; the dynamic-field
naming convention in `solr-ref/search-api/configset/schema.xml` +
`schema_extra_fields.xml`.

`WayfinderBackend::supportsDataType()` accepted exactly six types — `text`,
`string`, `integer`, `decimal`, `date`, `boolean` — and `FieldMapper` mapped only
those, so any index ported from `search_api_solr` lost fields configured with any
other type **silently at configuration time**.

## Scope: the twelve non-default types, classified

Finding 134 lists twelve non-default types plus the open-ended `solr_text_custom:*`
family. Fetching the full `search_api_solr` 4.4.0 source (finding 151) pinned the
prefix each maps to and — decisively — that two of the twelve are special-cased to
fixed sink fields *before* the generic prefix logic. The classification:

| Type | Prefix | Verdict |
|---|---|---|
| `solr_string_storage` | `z` | **supported** → Wayfinder `string` (indexed-when-Solr-isn't divergence) |
| `solr_string_docvalues` | `zdv` | **supported** → Wayfinder `string` + fast (same divergence) |
| `solr_text_unstemmed` | `tu` | **supported** → `text_general` (text_general is unstemmed — faithful) |
| `solr_text_omit_norms` | `to` | **supported** → `text_general` (norms stay on — scoring divergence) |
| `solr_text_wstoken` | `tw` | **supported** → `text_general` (tokenizer + norms divergence) |
| `solr_text_suggester` | fixed `twm_suggest` | **supported** → fixed static sink; feeds #291's SuggestComponent |
| `solr_date_range` | `dr` | **descoped** — Wayfinder `date` holds one instant, not a `[start TO end]` range; needs a new server-side type |
| `solr_text_spellcheck` | fixed `spellcheck_<lang>` | **descoped at the time** — language-specific sink; FieldMapper had no language-aware naming yet. *Resolved in #342: FieldMapper is now language-aware and the type is supported.* |
| `solr_text_custom` / `solr_text_custom_omit_norms` | `tc` / `toc` | **descoped** — site-defined analyzer escape hatch (`SolrFieldType` entities); preset has no equivalent |
| `location` / `rpt` | `loc` / `rpt` | **descoped** — spatial, scope of #292 |

The decision rule (finding 151): a type is expressible as a normal prefix+infix
dynamic field iff it has a prefix and is not one of the two fixed-field sinks.
`suggester` is language-*independent* (`twm_suggest`), so it needs no mapper
architecture change and lands here; `spellcheck` is language-*dependent*
(`spellcheck_<lang>`), so it waits for language-aware naming.

## What changed

- `FieldMapper` (`src/FieldMapper.php`): five new prefixes in `TYPE_PREFIXES`
  (`z`, `zdv`, `tu`, `to`, `tw`); `solr_text_suggester` special-cased to the fixed
  `SUGGESTER_SINK_FIELD = 'twm_suggest'`; a new `isTextType()` (mirrors
  `SearchApiSolrBackend.php:2706-2708`'s `solr_text_*` → `text` normalisation) and
  `isStringType()` so `formatValue`/`filterValue` treat `solr_text_*` as fulltext and
  `solr_string_*` as phrase-quoted strings. Without `isTextType`, a
  `solr_text_unstemmed` `TextValue` object would `json_encode` to `{}` — the exact
  #83 regression.
- `WayfinderBackend::supportsDataType()`: the six defaults plus the six newly-
  supported types, one per line; a comment block names every descope and its
  reason so the refusal is explicit, not silent.
- `presets/search-api.toml`: ten new `[[dynamic_fields]]` rules
  (`zs_`/`zm_`/`zdvs_`/`zdvm_`/`tus_`/`tum_`/`tos_`/`tom_`/`tws_`/`twm_`) plus the
  static `twm_suggest` sink. `zdvs_`/`zdvm_` carry `fast = true` (Solr docValues);
  `zs_`/`zm_` do not. The static field wins over the `twm_*` dynamic rule for the
  exact name `twm_suggest` (longest-match + static-wins, `src/schema.rs`).
- `README.md` "Not supported": the old "only the six default types" bullet is
  rewritten to state what is now supported (with the two accepted divergences) and
  what remains descoped, each with its reason.

## Two accepted divergences (documented, not papered over)

1. **`solr_string_storage` / `solr_string_docvalues` are queryable on Wayfinder.**
   In Solr they are `indexed=false` (storage/docValues-only). Wayfinder has no
   unindexed field (`src/lib.rs:1664`: "`indexed` is true for every type there
   is"), so they are indexed here. This is a *superset* of their Solr capability:
   a field deliberately kept out of search by typing it storage-only **becomes
   searchable** on Wayfinder. Recorded in the README and the preset header.
2. **The `solr_text_*` variants collapse onto `text_general`.** Their Solr
   analyzer-chain distinctions (unstemmed vs. omit-norms vs. whitespace-tokenized)
   are not preserved — a scoring-quality divergence, not a data-integrity one. The
   fields round-trip and are queryable.

## Testing

- `FieldMapperTest`: `fieldName` cases for all five new prefix types (s/m) and the
  `twm_suggest` sink; `formatValue` proves a `solr_text_*` `TextValue` casts to a
  plain string (the `{}` regression guard); a new `filterValue` provider covers
  phrase-quoting for `solr_text_*` / `solr_string_*`.
- `WayfinderBackendTest`: `supportsDataTypeProvider` asserts every accepted type
  returns `TRUE` and every descope (`solr_date_range`, `solr_text_spellcheck`,
  `solr_text_custom*`, `location`, `rpt`, unknown) returns `FALSE`.
- `tests/search_api_preset.rs`: `DYNAMIC_FIELDS`/`STATIC_FIELDS` contract entries
  for every new pattern + `twm_suggest`; round-trip query tests per prefix class
  (including `twm_wstoken` dynamic vs. `twm_suggest` static, proving both
  destinations resolve); a `zdvs_*` facet test (fast/docValues); the stored-fields
  retrieval test extended to all new fields.

**Mutation guards** (CLAUDE.md: a guard whose whole value is failing correctly
gets mutation-tested):
- `supportsDataType()`: leaking `solr_date_range` into the supported list fails the
  `…FALSE` case; reverted green.
- `filterValue` `isStringType()`: dropping it fails both `solr_string_*` phrase
  cases; reverted green.

## Commands / results

```
cd drupal/search_api_wayfinder && composer install && vendor/bin/phpunit   # 317 ok (was 267, +50)
cargo fmt --check                                                          # clean
cargo clippy --all-targets -- -D warnings                                  # clean
cargo test                                                                 # all green (search_api_preset 33 -> 45)
```

## Follow-ups

- **#291 (SuggestComponent)** is unblocked: the `twm_suggest` sink field now
  exists and `solr_text_suggester` is accepted. #291 builds the query half. One
  known ceiling to address there: multiple `solr_text_suggester` fields on one
  item — `DocumentBuilder` assigns `$doc['twm_suggest']` per field (last wins),
  where search_api_solr accumulates; full sink-field accumulation is a
  `DocumentBuilder` change that belongs with the component.
- **`solr_text_spellcheck`** needs language-aware field naming
  (`spellcheck_<lang>`), which the FieldMapper does not yet do for any type — its
  own piece of work, coordinated with spellcheck.
  *Resolved in #342: the FieldMapper now names every text-family field per
  language (`tm_X3b_<lang>_<id>`) and the spellcheck sink as
  `spellcheck_<lang>`, and the backend declares `solr_text_spellcheck`
  supported. A breaking rename — see the README's "Language-aware text field
  names" section for the reindex requirement.*
- **`solr_date_range`** needs a new server-side date-range type (Wayfinder `date`
  holds a single instant).

## Review

Single-author branch; gates green. No differential fixtures added — this issue is
a config/wire-format transliteration of an already-captured configset
(`schema.xml`/`schema_extra_fields.xml` are the ground truth the preset rules and
tests derive from), not new Solr behaviour to capture.
