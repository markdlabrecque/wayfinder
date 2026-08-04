# #302 — multi-valued text sorting uses the first value (recorded no-op)

**Date:** 2026-08-03. **Branch:** `markdlabrecque/issue-302-multivalued-text`.
**Spec:** issue #302; finding **153** in `docs/solr-ref-findings.md`.

The `README` descope said sorting on a multi-valued **text** field used the
field's first value (`DocumentBuilder` writes `$formatted[0]` into the
`sort_*` field), and flagged a possible divergence from Solr's min/max
multi-value selector. Issue #302 asked to check what Solr actually does before
building anything, and to expect the "already bug-compatible" branch.

## What Solr / search_api_solr actually does

Three independent lines of evidence agree: **the `sort_*` field takes exactly
one value — the first — so Solr never min/max-selects across a text sort field.**

1. **No `copyField` feeds `sort_*`.** The only `copyField` mentions in the
   captured configset are comment boilerplate (`schema.xml:42,44`).
   `sort_*` (`schema_extra_fields.xml:77`, type `collated_und` /
   `ICUCollationField`, `indexed=false docValues=true`) is written directly by
   the module.
2. **`search_api_solr` source copies the first value only.**
   `SearchApiSolrBackend::addIndexField()` returns "the first value of `$values`
   that has been added to the index" (`coverage/.../SearchApiSolrBackend.php`,
   `@return` at `:2726`), and the caller writes that scalar into each
   language-specific `sort_*` field with
   `if (!$doc->{$key}) { $doc->addField($key, $first_value); }` (`:1485`). The
   sibling path for non-text multi-valued fields names the workaround outright
   (`:1495`): "we use the same hackish workaround like the DB backend: just copy
   the first value in a single value field for sorting."
3. **The captured live-`solr:9` trace confirms it.**
   `solr-ref/search-api/trace/00001.json`: a document with
   `sm_field_topics = ["legacy", "documentation"]` indexes as
   `sort_X3b_en_field_topics = "legacy"` and `sort_X3b_und_field_topics =
   "legacy"` — the first value, not the min (`documentation`) nor the max.
   Across every update trace, **zero** `sort_*` fields carry more than one value
   (8 scalars, 0 lists).

So the "Solr selects min for asc / max for desc" divergence the issue feared
does not exist. Wayfinder's `DocumentBuilder` already writes `$formatted[0]`,
matching captured `search_api_solr` / `solr:9`. The non-text path is unaffected
and already correct: it sorts on the actual multi-valued fast field, where
Wayfinder's native Lucene min/max selector (`src/collector.rs`) **is** what Solr
does.

## What changed

This is a recorded no-op on the wire (no behaviour change); the diff is the
documentation and the fixture pin.

- `src/DocumentBuilder.php`: the `ponytail:` descope comment at the sort-field
  write is replaced with a confirmed-correct note citing the source line, the
  captured trace, and finding #153. The `ponytail:` marker comes off — this is
  no longer a deliberate simplification, it is correct behaviour.
- `README.md` ("Not supported"): the "Multi-valued text sorting uses the first
  value" bullet is removed. It is not a descope: it matches Solr, so there is
  nothing for a user to beware of that they would not also have on real Solr.
- `docs/solr-ref-findings.md`: appended finding **153** recording the
  investigation and conclusion.
- `tests/src/Unit/DocumentBuilderTest.php`: added
  `testBuildAddCommandMultivaluedTextSortTakesFirstValueNotMinMax`, which pins
  first-value selection with an input whose first value is **neither its min nor
  its max** (`['mango', 'apple', 'zebra']` → `sort_field_pick = 'mango'`).

### Why a new test was needed

The pre-existing `testBuildAddCommandAddsDeterministicTextSortCopies` already
asserted the sort copy equals the first value, but its input
`['First paragraph', 'Second paragraph']` sorts first == min, so it would also
pass under a `min($formatted)` regression. The new test uses a discriminating
input so the tempting wrong "fix" (min/max selection) fails the suite instead of
passing it.

## Verification

```
cd drupal/search_api_wayfinder && composer install && vendor/bin/phpunit --filter DocumentBuilder
# OK (8 tests, 21 assertions) — was 7 tests before
```

**Mutation test** (CLAUDE.md: code whose value is failing correctly gets
mutation-tested). With `$formatted[0]` → `min($formatted)` in `DocumentBuilder`:

- `testBuildAddCommandMultivaluedTextSortTakesFirstValueNotMinMax` **FAILS**
  (`-mango / +apple`) — the pin catches the regression.
- `testBuildAddCommandAddsDeterministicTextSortCopies` still passes — confirming
  the new test is the discriminator.

Reverted; production code is unchanged on the wire.

No Rust change, so no `cargo` gate is load-bearing here; the Rust sort path was
only read, not edited.
