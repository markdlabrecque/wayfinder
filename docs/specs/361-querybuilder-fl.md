# #361 — `QueryBuilder` sends no `fl`, so sink fields come back inside docs

Branch: `361-querybuilder-fl`. Group A.

## The defect

`drupal/search_api_wayfinder/src/QueryBuilder.php` builds no plain `fl` param at
all. It builds `mlt.fl` (`:125`), `terms.fl` (`:161`) and `hl.fl` (`:214`), but
nothing that restricts the fields returned in each document.

Every stored field therefore comes back in every doc — including the index-side
sink fields `twm_suggest` (the suggester sink, `presets/search-api.toml:100`) and
`spellcheck_<lang>` (the spellcheck sinks from #342). These are plumbing. No
consumer wants them, they inflate every response, and they leak internal schema
shape to anything reading the wire.

This was originally specified as part of #342 and withdrawn once the test-writer
established there is no existing `fl` to amend. Adding one is its own change,
because it has to *enumerate* the fields the response actually needs — that is
the whole difficulty, and the reason it was split out.

## The hard part, stated plainly

An `fl` that is too narrow silently breaks features. A field omitted from `fl`
is absent from the doc, and every consumer that reads it degrades quietly rather
than erroring. So the enumeration must be derived, not guessed.

**Work out the required field set from the consumers, not from the schema.**
Read `ResponseParser::parse()` (`ResponseParser.php:36`) and everything it
reaches, and establish exactly which document keys are read: the id field, score
if requested, language, datasource, whatever the result-item construction needs.
Then read what `search_api` itself expects from a result item. The set is the
union of those, not "everything except the sinks".

Consider seriously whether an **exclusion** model is the safer shape here — Solr
supports globbing in `fl` (`fl=*`), and if the real requirement is "everything
except two known sinks", enumerating hundreds of dynamic fields to exclude two
is both fragile and wrong the moment a new field type lands. Establish what
`fl` syntax Wayfinder's `select` actually supports before designing around it —
check `src/lib.rs` and the `fl` fixtures in `solr-ref/responses/`. If glob or
exclusion syntax is unsupported server-side, that constraint decides the design,
and it may make this issue depend on server work that does not exist yet. **If
that happens, say so and stop rather than building a brittle enumeration.**

## Verify before implementing

1. What `fl` syntax does Wayfinder's `select` support today — bare names,
   globs, exclusions? Cite the fixture or the code.
2. What does `search_api_solr` itself send as `fl`? Grep the vendored 4.4.0
   source. If it sends something specific, that is the compatibility target and
   this becomes a much more constrained change.
3. Which document keys does `ResponseParser` and its downstream actually read?
   List them in the PR body.

## Scope

Add a plain `fl` to the select request built by `QueryBuilder`, carrying the
field set established above, such that `twm_suggest` and `spellcheck_<lang>` no
longer appear in returned documents.

Out of scope: changing what gets *stored*. The sinks are stored deliberately and
`/terms` reads `twm_suggest` directly — do not touch the preset.

## Testing

Tests first, red, in
`drupal/search_api_wayfinder/tests/src/Unit/QueryBuilderTest.php`. Cover:

- an `fl` is present on the select request, with the enumerated set
- `twm_suggest` and `spellcheck_*` are not in it
- every field `ResponseParser` reads **is** in it — ideally driven off the same
  list rather than a hand-copied duplicate, so the two cannot drift
- the existing `mlt.fl` / `terms.fl` / `hl.fl` behaviour is unchanged

Then the integration check that matters: a full round-trip where a result item is
built from a response produced under the new `fl`, asserting nothing the parser
needs went missing. A unit test on the param string alone would not have caught
the failure mode this issue is really about.

## Files

**You own:** `drupal/search_api_wayfinder/src/QueryBuilder.php` and its test.

**Siblings own:** `ResponseParser.php` (#360) — you will *read* it; do not edit
it. `DocumentBuilder.php` (#358, #362).

## Definition of done

- `fl` present, sinks excluded, every consumed field provably included.
- The three verification findings in the PR body, including the `fl`-syntax
  constraint and what `search_api_solr` sends.
- Round-trip test proving no consumed field was dropped.
- Module tests green; Rust gates clean.
