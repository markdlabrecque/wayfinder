# #362 — N+1 identical sort copies per text/string field

**Date:** 2026-08-12. **Branch:** `markdlabrecque/issue-362-n-1-identical-sort`.
**Status:** **implemented** — the N+1 language-specific sort copies are
collapsed to a single language-agnostic `sort_<id>` field.

Follow-up from #342 (see its report,
`docs/reports/2026-08-04-342-language-aware-naming-spellcheck.md`, follow-up #6).

## The behaviour under examination

`DocumentBuilder::buildAddCommand()` writes each text/string field's sort copy
once per enabled site language plus `und`:

```php
foreach ($this->sortLanguages() as $sortLanguage) {        // every lang + 'und'
  $key = $this->fieldMapper->sortFieldName(
    $field->getFieldIdentifier(), $type, $multiValued, $sortLanguage);
  if (!isset($doc[$key])) {
    $doc[$key] = $formatted[0];                            // identical value
  }
}
```

`sortLanguages()` is `array_keys(languageManager->getLanguages())` plus
`'und'`. The value written is `$formatted[0]` (the field's first value) for
**every** language, so for L enabled languages a sortable field carries L+1
copies that differ in name only. This mirrors
`SearchApiSolrBackend.php:1469-1481` ("To allow sorted multilingual searches
we need to fill *all* language-specific sort fields!"). Captured ground truth
agrees: `solr-ref/search-api/trace/00001.json` (an English-only site) carries
both `sort_X3b_en_title` and `sort_X3b_und_title` for the one title value.

## Decision: collapse to one language-agnostic `sort_<id>` field

The divergence is not merely safe — it is the *correct* behaviour given a
divergence Wayfinder has **already** made, and the per-language copies are
load-bearing only in features Wayfinder does not have.

**1. The only reason for per-language copies is language-specific collation,
which Wayfinder does not provide.** In real Solr, `sort_X3b_en_title` is typed
`collated_en` and `sort_X3b_fr_title` is typed `collated_fr`: the *same* string
sorts in a different order under different locale rules. That is the whole
point of filling every language. `presets/search-api.toml:21-25` documents
that Wayfinder has no collation type and maps every `sort_*` to plain `string`:

> A second, distinct divergence: `sort_*` maps to Wayfinder's `string` type as
> a stand-in for Solr's `collated_en`/`collated_und` field type, since
> Wayfinder has no collation type.

So in Wayfinder `sort_X3b_en_title` and `sort_X3b_fr_title` are both plain
strings holding an identical value — there is no per-language ordering, and
sorting on either yields the identical order as the other.

**2. The copy values are byte-identical.** `DocumentBuilder` writes
`$formatted[0]` into every copy; the language only changes the field name. The
captured trace confirms it (`sort_X3b_en_title == sort_X3b_und_title`, same for
`_body`, `_field_sku`, `_field_keywords`, `_type`).

**3. At most two copies are ever *read*, and they hold the same value.** Every
reader of `FieldMapper::sortFieldName()`:

| Reader | Language passed | Resolves to |
|---|---|---|
| `QueryBuilder.php:998` (sort path) | `languages[0]` | `sort_X3b_<languages[0]>_<id>` |
| `QueryBuilder.php:922` (grouping, string fields) | none | the mapped field `ss_<id>` (no sort copy) |
| `QueryBuilder.php:922` (grouping, `solr_text_*` fields) | none | `sort_X3b_und_<id>` |
| `ResponseParser.php:229` (grouping result keys) | none | mirrors the grouping name |
| `DocumentBuilder.php:160` (write path) | every lang + `und` | — (write only) |

So only `sort_X3b_<languages[0]>_<id>` and (in the `solr_text_*` grouping edge
case) `sort_X3b_und_<id>` are ever read. For a site whose first language is
`en`, the `fr`/`de`/`es`/… copies are **write-only waste** — written at index
time, never referenced again. A single `sort_<id>` field serves all four
readers with the identical value and ordering.

**4. Precedent: pre-#342 Wayfinder already used the unqualified name.** The
#342 report (MF-3) notes "pre-#342, both sides agreed on the unqualified
`sort_title`" and it sorted correctly. The per-language sort naming was
introduced by #342 to match search_api_solr's *wire shape*, not because
sorting needed it; the wire-shape match buys nothing in Wayfinder for the
reasons above.

## Measurement

`tests/sort_copy_bloat.rs` (committed, `#[ignore]`'d — a measurement, not an
assertion; run with
`cargo test --test sort_copy_bloat -- --nocapture --ignored`) indexes K = 1200
documents modelled on `trace/00001.json` — the same five sortable source
fields (text `title`/`body`, string `field_sku`/`field_keywords`/`type`) plus
the same non-sortable companions — with sort-copy values varied per document,
into a fresh single-segment core (one commit; the steady-state shape of a
merged index, so the figure reflects column cost, not per-segment overhead).
Two strategies:

- **single** — one language-agnostic `sort_<id>` per sortable field (the
  proposed divergence);
- **multi(L)** — `sort_X3b_<lang>_<id>` for each of L enabled languages plus
  `und` (current behaviour).

Base (non-sort) fields are identical between the two, so the delta is the pure
sort-copy cost. Ratios are stable across runs (absolute bytes vary by a few
hundred bytes of noise):

```
#362 sort-copy bloat measurement (K = 1200 docs, 5 sortable fields)

  strategy           L    index bytes    KiB     vs single
  ─────────────────────────────────────────────────────────
  single (1 copy)    -         302271    295.2        1.00x
  multi (L+1 copies) 1         392771    383.6       1.30x
  multi (L+1 copies) 2         494583    483.0       1.64x
  multi (L+1 copies) 4         683376    667.4       2.26x
  multi (L+1 copies) 8        1048750   1024.2       3.47x
```

The marginal cost is ~linear in L at ~18–20 bytes per document per extra
identical sort column (≈90–100 KiB per copy-layer here). The headline:

- A **monolingual site** (1 enabled language → `en` + `und`, the common case
  and the one in the captured trace) pays **30%** redundant index overhead.
- An **8-language site** pays **~3.5×**.

This is the cost the divergence reclaims, for zero functional change (the
copies are byte-identical and never collated).

## Server impact

None required. `sort_*` is a single `[[dynamic_fields]]` rule
(`presets/search-api.toml:359-362`), so `sort_title` resolves exactly as
`sort_X3b_en_title` does today. The server never inspects the language infix.
The differential harness is unaffected: it tests the server against real Solr,
and sort-field *naming* is a client-side decision. This is a Drupal-module
(`drupal/search_api_wayfinder/`) change, not a `src/` change.

## Risks

- **Wire-format divergence from search_api_solr**, now deliberate and
  documented rather than incidental. `search_api_wayfinder` already diverges
  in kindred ways (no site-hash in the doc id per #301; booleans as strings;
  no collation; the `tm_X3b_<lang>_*` rename itself from #342). Document in
  README alongside the existing divergence list.
- **`first_value` truncation to 128 chars** is unchanged (it lives in
  upstream and is mirrored by value formatting, not by the sort field name);
  the single copy preserves it.
- No regression to grouping: string-field grouping uses the mapped fast field,
  not a sort copy, today; the `solr_text_*` grouping edge case moves from
  `sort_X3b_und_<id>` to `sort_<id>`, which the single copy now provides.

## Implementation (delivered)

All changes in `drupal/search_api_wayfinder/`, TDD (red tests first, confirmed
red for the right reason, then implementation):

- **`FieldMapper::sortFieldName()`** — the sort copy is now
  `encodeSolrName('sort_' . $fieldId)`, dropping the language separator. The
  `$language` argument is retained as a MODE flag only (non-null = sort/index
  path, NULL = grouping path); it no longer appears in the name. The grouping
  path still resolves non-text fields to their mapped field and the text family
  to its sort copy, exactly as before.
- **`DocumentBuilder::buildAddCommand()`** — the `foreach ($this->sortLanguages()
  …)` loop is replaced by a single write (`sortFieldName(…, $language)`, the
  item's own language as the mode flag). The `sortLanguages()` private method
  and the `?LanguageManagerInterface` constructor argument it existed to feed
  are removed (they were added in #342 solely to drive the all-language fill).
- **`QueryBuilder::sortFieldName()`** — passes `$this->languages[0]` as before;
  it now resolves to `sort_<id>` (a no-op source change, the new name comes
  from `FieldMapper`).
- **`WayfinderBackend::indexItems()`** — `new DocumentBuilder(new FieldMapper())`
  (no language manager).
- **README** — the #342 "N+1 identical sort copies" paragraph is rewritten to
  describe the single language-agnostic copy, the divergence rationale, and the
  reindex requirement.

The grouping path (`QueryBuilder`/`ResponseParser`) needed no change: it calls
`sortFieldName()` with no language, which for the text family now resolves to
`sort_<id>` — the same field `DocumentBuilder` writes.

## Test evidence

```
cd drupal/search_api_wayfinder && vendor/bin/phpunit
  427 tests, 747 assertions, 0 failures, 0 errors
```

(280 PHPUnit deprecation notices are the pre-existing baseline, not introduced
here.) Red confirmed first: 17 failures across `FieldMapperTest`,
`DocumentBuilderTest`, and `QueryBuilderTest`, every one the sort-field-name
change (`sort_X3b_<lang>_<id>` -> `sort_<id>`), none unrelated. The
red->green transition is itself the mutation test for the `FieldMapper` name
change (reverting it to `sort' . SEPARATOR . $resolvedLanguage . '_'` makes all
17 fail again).

No server (`src/`) change: `sort_*` is a single dynamic-field rule and the
language infix was never inspected. The differential harness is unaffected —
sort-field naming is a client-side decision. The captured trace
(`solr-ref/search-api/trace/00001.json`) still shows `sort_X3b_en_*` because it
records real `search_api_solr` -> real Solr; this module intentionally diverges
from that sort-field naming, documented in the README and here.

## Reproduce

```
cargo test --test sort_copy_bloat -- --nocapture --ignored
```
