# #358 — string fields get no language-specific `sort_*` copy

Branch: `358-string-sort-copy`. Group A — safe to run alongside 359–362.

## The defect

`search_api_solr` writes a language-specific `sort_*` copy for a field when the
mapped Solr field name starts with **`t` or `s`** — see
`SearchApiSolrBackend::addIndexField()` in
`coverage/search_api_solr_4.4.0_source/src/Plugin/search_api/backend/SearchApiSolrBackend.php`.
That covers string fields (`ss_*`, `sm_*`), not just text fields (`ts_*`, `tm_*`).

Wayfinder's `DocumentBuilder` gates the sort copy on `$type === 'text'`
(`drupal/search_api_wayfinder/src/DocumentBuilder.php:121`). String fields
therefore get no `sort_*` copy at all, and a Drupal index sorted on a string
field sorts on the mapped `ss_*`/`sm_*` field instead of the `sort_*` copy Solr
would use.

This is a divergence, not a documented simplification. Nothing in the PRD
sanctions it.

## Ground truth

`solr-ref/search-api/trace/*.json` contains `sort_X3b_en_field_sku` — a string
field (`field_sku`) with a language-specific sort copy, captured from real
`search_api_solr` against real `solr:9`. **Derive the expected field names and
values from the trace, not from what `DocumentBuilder` currently produces.**

## Verify before implementing

Three things, all cheap, all of which change the shape of the fix:

1. **Read `addIndexField()` and confirm the exact gate.** The issue states
   "starts with `t` or `s`". Confirm that against the source and record the real
   line numbers. If the condition is narrower (a specific prefix list rather
   than a first-character test), build to what the source says.
2. **Establish what value a multi-valued string field's sort copy carries.**
   For text, #302 / finding 153 settled that it is the *first* value, and
   `DocumentBuilder.php:122-137` documents that at length. Do **not** assume the
   same holds for strings — check the trace for a multi-valued `sm_*` field with
   a `sort_*` copy and see what it actually contains. If the trace has no such
   case, say so and pick the text behaviour for consistency, with a `ponytail:`
   naming the untested assumption.
3. **Check `FieldMapper::sortFieldName()` handles string types.** It was built
   for text (#342). If it hard-codes anything text-specific, that is part of
   this change.

## Scope

Extend the sort-copy write in `DocumentBuilder` to string fields, matching the
gate the source actually uses. Keep the existing text behaviour exactly as it is
— it is pinned by tests and by finding 153, and this change must not disturb it.

Out of scope: the fan-out question (how many language copies get written) is
#362. Write string sort copies the same way text ones are written today,
whatever that turns out to be after #362 lands or doesn't.

## Testing

Tests first, red, from the trace. Cover at least:

- a single-valued string field gets its `sort_*` copy, with the name the trace
  shows (`sort_X3b_en_field_sku` shape)
- a multi-valued string field, per whatever verification step 2 established
- text fields are unchanged — an explicit regression test, since this edits the
  branch they run through
- a field type that should get *no* sort copy still gets none

Extend `drupal/search_api_wayfinder/tests/src/Unit/DocumentBuilderTest.php`.

## Files

**You own:** `drupal/search_api_wayfinder/src/DocumentBuilder.php`,
`drupal/search_api_wayfinder/tests/src/Unit/DocumentBuilderTest.php`, and
`FieldMapper.php` only if step 3 requires it.

**Siblings own:** `QueryBuilder.php` (#361), `ResponseParser.php` (#360),
`src/lib.rs` (#359). #362 also touches `DocumentBuilder.php` — coordinate, and
prefer landing this one first since #362 may end in no change at all.

## Definition of done

- String fields get `sort_*` copies matching the captured trace.
- Text-field behaviour provably unchanged.
- The three verification findings reported in the PR body.
- Module tests green; `cargo test`, `cargo fmt --check`,
  `cargo clippy --all-targets -- -D warnings` clean.
