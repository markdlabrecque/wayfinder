# #360 — `ResponseParser` drops the `{word, freq}` extendedResults shape

Branch: `360-extended-results-shape`. Group A.

## The defect

`ResponseParser::parseSpellcheck()`
(`drupal/search_api_wayfinder/src/ResponseParser.php:127`) parses the **flat**
spellcheck named-list into
`['suggestions' => [term => [words]], 'collation' => string]`, matching
`SolrSpellcheckBackendTrait.php:24-42`.

With `spellcheck.extendedResults=true`, Solr returns each suggestion as a
`{word, freq}` object rather than a bare string. That shape is currently
dropped. The ceiling is already marked with a `ponytail:` comment in the source
— read it before starting; it may already record constraints this spec does not.

## Verify before implementing

1. **Does a fixture cover `extendedResults=true`?** Grep
   `solr-ref/responses/`. The parser's docblock at `:114-116` cites
   `spellcheck_flat.json` and `spellcheck_map.json` — check whether either
   carries the extended shape, or whether a new capture is needed. Expected
   values come from the fixture, never from Solr documentation and never from
   what the parser currently produces.
2. **Does anything actually request it?** The `/autocomplete` handler config
   ships `spellcheck.extendedResults=false` as a default (see #351). Establish
   whether `search_api_solr` ever turns it on — grep the now-vendored full source
   for `setExtendedResults` / `extendedResults`. If nothing in 4.4.0 requests it,
   say so in the PR: that does not kill the issue (a stock Solr client may still
   ask), but it changes this from a compatibility bug to a robustness
   improvement, and the PR should describe it honestly as such.
3. **Read `SolrSpellcheckBackendTrait.php:24-42`** in the vendored source and
   confirm what the module does with the parsed structure downstream. If the
   consumer only ever reads the word and discards frequency, preserving `freq`
   has no consumer — worth knowing before designing for it.

## Scope

Parse both shapes. A suggestion entry that is a bare string keeps working
exactly as it does today; an entry that is a `{word, freq}` object is
recognised and its `word` extracted, with `freq` preserved if step 3 shows a
consumer for it and dropped with a `ponytail:` if not.

Detect the shape from the data, not from a `spellcheck.extendedResults` request
param — the parser sees a response, and a response that contains objects should
be parsed as objects regardless of what was asked for. This also means a mixed
or unexpected shape must not fatal: an entry that is neither a string nor an
object with a `word` key should be skipped, not thrown on.

If step 1 shows no fixture exists, capture one: `select` with
`spellcheck=true&spellcheck.extendedResults=true` against real `solr:9`. Append
the block at the **end** of `solr-ref/capture.sh`, run with
`capture.sh --only <prefix>`, and commit the fixtures before anything else.

## Testing

Tests first, red, in
`drupal/search_api_wayfinder/tests/src/Unit/ResponseParserTest.php` (create it if
it does not exist; check first). Cover:

- the flat/bare-string shape still parses identically — a regression test, since
  this edits the path that currently works
- the `{word, freq}` shape parses
- a malformed entry is skipped rather than fataling
- no `spellcheck` block at all still returns `NULL` (existing behaviour, per the
  docblock at `:122`)

## Files

**You own:** `drupal/search_api_wayfinder/src/ResponseParser.php` and its test.

**Siblings own:** `DocumentBuilder.php` (#358, #362), `QueryBuilder.php` (#361),
`src/lib.rs` (#359).

**Dependency:** #351 (`/autocomplete`) sequences after this.

## Definition of done

- Both suggestion shapes parse; malformed entries are skipped, not fatal.
- The three verification findings reported in the PR, including whether 4.4.0
  ever requests `extendedResults` at all.
- Any `ponytail:` comment left in place names its ceiling explicitly.
- Module tests green; Rust gates clean.
