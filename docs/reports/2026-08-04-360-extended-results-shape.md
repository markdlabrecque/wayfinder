# #360 — search-api: ResponseParser drops the `{word, freq}` extendedResults shape

**Date:** 2026-08-04. Issue #360 (open). Branch
`markdlabrecque/issue-360-responseparser-drops-word` off `main`. Follow-up to
#342. This report documents the fix to
`drupal/search_api_wayfinder/src/ResponseParser::parseSpellcheck()`.

## The defect

`parseSpellcheck()` reduces Solr's flat spellcheck named-list into the
`search_api_spellcheck` extra-data shape
(`['suggestions' => [term => [word, ...]], 'collation' => string]`) that
mirrors `SolrSpellcheckBackendTrait.php:24-42`. Each suggestion member was kept
only when `is_scalar()`:

```php
$words = array_values(array_map('strval', array_filter(
  (array) ($info['suggestion'] ?? []),
  static fn ($word): bool => is_scalar($word)
)));
```

With `spellcheck.extendedResults=true` Solr returns each member as a
`{word, freq}` **object** rather than a bare string. `is_scalar()` is false for
arrays, so every extended-results suggestion was silently dropped, leaving an
empty list — and the trait's `if ($keys)` guard then dropped the whole term.
The ceiling was marked with a `ponytail:` comment ("extendedResults yields an
empty suggestion list").

## The three verification findings (reported honestly)

1. **No fixture covered `extendedResults=true`.** `spellcheck_flat.json` /
   `spellcheck_map.json` carry only the bare-string default shape. A new
   fixture was captured: `solr-ref/responses/spellcheck_360_extended.json`,
   real `solr:9` with `spellcheck=true&spellcheck.extendedResults=true&json.nl=flat`,
   same `en` corpus/dictionary as the #223 block so it is directly comparable
   to `spellcheck_flat.json`. Ground truth: each member is
   `{"word":"quick","freq":2}`; Solr also adds `origFreq`/`correctlySpelled`,
   which the parser ignores.

2. **Nothing in the vendored 4.4.0 source requests it.** Grepping
   `coverage/search_api_solr_4.4.0_source/` (excluding the configset XML/YML,
   which only *default* the param to `false`) for `setExtendedResults` /
   `extendedResults` finds no PHP caller. Combined with Wayfinder's own
   `SELECT_PARAMS` (`src/lib.rs`) not admitting `spellcheck.extendedResults` —
   so under `strict_params=true` the server 400s it — this shape never reaches
   the parser from Wayfinder today. **This is a robustness improvement, not a
   live compatibility bug**; a stock Solr client pointed at the index could
   still produce it.

3. **The downstream consumer discards `freq`.**
   `SolrSpellcheckBackendTrait::extractSpellCheckSuggestions()` (the trait whose
   structure this extra data mirrors) loops `$correction->getWords()` and reads
   only `$word['word']` — frequency is never used. So `freq` is dropped (with a
   `ponytail:` naming that ceiling) rather than carried for no consumer.

## The fix

Detect the member shape from the data, not from a request param — the parser
sees a response and must parse whatever shape it carries:

```php
$words = [];
foreach ((array) ($info['suggestion'] ?? []) as $suggestion) {
  if (is_string($suggestion)) {
    $words[] = $suggestion;
  }
  elseif (is_array($suggestion) && is_string($suggestion['word'] ?? NULL)) {
    $words[] = $suggestion['word'];
  }
}
```

- A bare string keeps working exactly as before (a regression test pins this).
- A `{word, freq}` object is reduced to its `word`.
- Anything else — a malformed object with no `word`, a number, `NULL` — is
  **skipped, not fatal**. Tightening the bare branch from `is_scalar` to
  `is_string` was deliberate: an integer is "neither a string nor an object
  with a `word` key", and the spec requires it skipped; the mutation test below
  pins that.

## Why the fixture has no manifest row

This fixture is a shape reference for the PHP parser, not a Wayfinder
wire-parity fixture: Wayfinder 400s `spellcheck.extendedResults` (the param is
not in `SELECT_PARAMS`, and admitting it is a separate concern from #360), so a
differential row would diverge on HTTP status rather than wire shape. The
capture block (`solr-ref/capture.sh`, appended) therefore writes the file only
— the same shape-reference-without-manifest pattern the `extract_*` fixtures
use (`cap_extract`). The differential harness is unchanged and green.

## Testing

Tests in `ResponseParserTest.php`, written first and confirmed red for the right
reason before implementing:

- `testParseSpellcheckExtendedResultsShapeExtractsWord` — the `{word, freq}`
  shape parses; inline array mirrors `spellcheck_360_extended.json` including
  the ignored `origFreq`/`correctlySpelled` keys.
- `testParseSpellcheckSkipsMalformedSuggestionMembers` — a malformed object
  (no `word`), a number, and `NULL` are skipped; a bare string and a valid
  `{word}` object around them still parse, in order.
- Existing regression coverage unchanged: the flat/bare-string shape
  (`testParseSpellcheckFlatFormPopulatesSuggestionsAndCollation`), empty-list
  drop, first-collation-only, no-spellcheck-block ⇒ no extra data, and no
  collations ⇒ no `collation` key.

**Mutation test** (per the "code whose value is failing correctly" rule):
weakening `is_string($suggestion)` back to `is_scalar($suggestion)` lets the
integer `7` leak through, and
`testParseSpellcheckSkipsMalformedSuggestionMembers` fails (`7` appears in the
output). Reverted; full suite green.

## Verification

- `vendor/bin/phpunit` — 427 tests, OK (search_api_wayfinder module).
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test` — clean / all pass (incl. the differential harness).
- `cargo fmt/clippy/test --manifest-path bench/Cargo.toml` — clean.
- `bash -n solr-ref/capture.sh` — syntax OK; the block re-runs cleanly under
  `capture.sh --only '^spellcheck_360_'`.

## Files

- `drupal/search_api_wayfinder/src/ResponseParser.php` — the fix.
- `drupal/search_api_wayfinder/tests/src/Unit/ResponseParserTest.php` — tests.
- `solr-ref/capture.sh` — appended the #360 capture block.
- `solr-ref/responses/spellcheck_360_extended.json` — new ground-truth fixture.
