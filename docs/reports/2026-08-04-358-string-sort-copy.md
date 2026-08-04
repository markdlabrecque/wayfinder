# Issue #358 — string fields get no language-specific `sort_*` copy

Date: 2026-08-04
Branch: `markdlabrecque/issue-358-string-fields-get`
Issue: #358 (follow-up from #342). Spec: `docs/specs/358-string-sort-copy.md`.

## Outcome

`search_api_wayfinder` now writes language-specific `sort_*` copies for ordinary
string fields (`ss_*` / `sm_*`) as well as text fields, matching captured
`search_api_solr` / `solr:9`. Query sorting resolves string fields to those
copies; grouping continues to group on the ordinary single-valued `ss_*` mapped
field, matching upstream's separate grouping path. The copy is scalar and
first-value-wins. Text-field behaviour is unchanged, and non-`t`/`s` types
(integer, decimal, date, boolean) still get no `sort_*` copy.

## The three verification findings

1. **The exact gate.** The vendored 4.4.0 source uses a *first-character* test on
   the mapped Solr field name, not a narrow prefix list
   (`coverage/search_api_solr_4.4.0_source/.../SearchApiSolrBackend.php:1448-
   1454`): a mapped name beginning with `t` **or** `s` qualifies, then `twm_suggest`
   and `spellcheck` are excluded by name. The copy is written at `:1482-1485`.
   So text (`tm_*`) **and** string (`ss_*`/`sm_*`) get a copy; `solr_string_storage`
   (`z*`), integer (`it*`), decimal (`ft*`), date (`d*`) and boolean (`b*`) do not.

2. **Multi-valued string sort-copy value (from the trace).** The real-client/
   real-Solr capture answers it — no assumption needed. `solr-ref/search-api/
   trace/00001.json` contains `sm_field_keywords = ["animals","classic","pangram"]`
   with `sort_X3b_en_field_keywords = "animals"`, and `sm_field_topics =
   ["legacy","documentation"]` with `sort_X3b_en_field_topics = "legacy"`. A
   multi-valued string's sort copy is therefore the **first** value, exactly like
   text (finding #153). The single-valued shape is pinned in the same trace:
   `ss_field_sku = "ART-001"` → `sort_X3b_en_field_sku = "ART-001"` (and the
   matching `sort_X3b_und_*`). The query trace `00011.json` sorts on
   `sort_X3b_en_field_sku asc`, confirming the query side resolves strings to the
   copy.

3. **`FieldMapper::sortFieldName()` was text-specific.** It tested only
   `isTextPrefix(...)`, so a string sort resolved to `ss_*`/`sm_*` rather than the
   sort copy. The fix shares upstream's mapped-name gate with `DocumentBuilder`.
   `sortFieldName()` is now used by two kinds of caller, distinguished by whether
   they pass a resolved language:

   - Sorting (`QueryBuilder::buildSort`) and indexing (`DocumentBuilder`) always
     pass a language and receive `sort_X3b_<lang>_<id>` for every `t`/`s` field.
   - Grouping (`QueryBuilder`/`ResponseParser`) has no resolved sort language and
     omits the argument; a `NULL` language resolves to the mapped name for every
     non-text type, so grouping stays on `ss_*` as upstream does
     (`SearchApiSolrBackend.php:4600`, `reset($field_names)`).

   This nullable-language signal is the reason no sibling-owned *source* file
   (`QueryBuilder.php`, `ResponseParser.php`) had to change: the grouping call
   sites already omit the language, so the single shared method can serve both
   paths correctly.

## Implementation

- `FieldMapper` gains `usesLanguageSpecificSortCopy(string $fieldName): bool` —
  the mapped-name predicate (`t`/`s`, excluding the two sinks). This is the single
  source of truth for the gate, used by both `DocumentBuilder` (whether to write a
  copy) and `sortFieldName()` (what name to resolve to).
- `FieldMapper::sortFieldName()` is broadened from the text-only `isTextPrefix`
  gate to `usesLanguageSpecificSortCopy`, and `$language` made nullable to carry
  the sort-vs-group distinction above. Text's resolved name is unchanged.
- `DocumentBuilder` replaces its `if ($type === 'text')` gate with
  `usesLanguageSpecificSortCopy($name)`; the first-value and first-write rules and
  the per-language fan-out are untouched.
- No fixture was added or recaptured: `00001` already contains both the single-
  and multi-valued string cases the spec required, and re-running `capture.sh`
  would churn `_version_`/`rid`/`QTime` across every branch's diff.

## Test-first evidence

Tests were written first from the trace and confirmed red for the intended
reasons before any production change. Before the fix, the focused PHPUnit run
failed six assertions:

- `DocumentBuilder` emitted no `sort_X3b_en_field_sku` / `sort_X3b_und_field_sku`
  (single-valued string) and no `sort_X3b_*_field_pick` (multi-valued string);
  the text+string coexistence regression also failed.
- `FieldMapper::sortFieldName()` returned `ss_field_sku` / `sm_field_keywords`
  instead of the trace-derived `sort_*` names.
- `QueryBuilder` produced `sm_tags asc` instead of `sort_X3b_und_tags asc`.

After the fix all six pass. Guard tests added alongside stay green both before
and after, pinning what must *not* change: an integer field gets no sort copy
(`its_field_rating`), and a grouping caller (no language) keeps a string field on
`ss_field_sku` / `sm_field_keywords`.

### Mutation test of the grouping guard

The nullable-language grouping signal is the subtle part of this change, so it
was mutation-tested per the working agreement. Removing the
`&& ($language !== NULL || str_starts_with($mappedName, 't'))` clause makes
grouping on a string field resolve to `sort_*` instead of the mapped `ss_*`. Two
tests catch that immediately:

- `ResponseParserTest::testGroupedResponseFlattensGroupsAndUsesNgroupsAsCount`
  (looks up `grouped.sort_X3b_und_type` instead of the keyed `grouped.ss_type`
  → count 0 instead of 2), and
- `FieldMapperTest::testSortFieldNameWithoutLanguageKeepsGroupingOnMappedStringField`
  (direct unit assertion).

## Validation

- `cd drupal/search_api_wayfinder && vendor/bin/phpunit` — 425 tests, 749
  assertions, green (PHPUnit reports only the suite's pre-existing deprecations).
- `cargo fmt --check` — clean.
- `cargo clippy --all-targets -- -D warnings` — clean.
- `cargo test` — green and hermetic (67 test binaries, 0 failures), including the
  differential fixture suite. No Rust source changed; the module is pure PHP.

## Notes / follow-ups

- The two fixed sinks (`twm_suggest`, `spellcheck_*`) are excluded from the sort
  gate by name, mirroring upstream. In `DocumentBuilder` they never reach the gate
  anyway (they branch off into their accumulation sinks earlier); the name check
  keeps `usesLanguageSpecificSortCopy()` correct in isolation for any other
  caller.
- The language fan-out question (how many language copies get written per field)
  is #362 and explicitly out of scope here; string copies fan out exactly the way
  text copies do today.
