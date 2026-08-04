# #342 — language-aware field naming + spellcheck wiring (Drupal half)

**Date:** 2026-08-04. **Branch:** `markdlabrecque/issue-342-real-suggestions-collations`.

## Scope correction

Issue #342 originally read as two halves: real spellcheck suggestions/collations
on the server, and Drupal-side wiring. The server half was **already delivered**
before this task started, by `d97b442 feat(spellcheck): generate real
suggestions and collations (#228)` on 2026-08-01. The issue text's claim that it
was "closed without implementation" is stale. This was verified directly
against the real fixture `solr-ref/responses/spellcheck_flat.json`, which
contains populated `spellcheck.suggestions` and `spellcheck.collations`. **No
server (`src/`) code was touched by this task.**

The work delivered here is the Drupal half, scoped by the user to cover *all*
text fields' naming, not just spellcheck.

## What was built

All changes are in `drupal/search_api_wayfinder/`.

- **`FieldMapper`** — `fieldName()` gained a `$language = 'und'` parameter.
  Text-family types (`text`, `solr_text_unstemmed`, `solr_text_omit_norms`,
  `solr_text_wstoken`) now emit `tm_X3b_<lang>_<id>` — always the `m` infix
  regardless of Drupal cardinality, mirroring
  `SearchApiSolrBackend.php:2450-2473`. Added a private `encodeSolrName()`
  (chars outside `[a-zA-Z0-9_]` become `_X<lowercase hex>_`, so `;` ->
  `_X3b_`) and a new `spellcheckDictionary()` helper. `solr_text_spellcheck`
  maps to the fixed sink `spellcheck_<lang>` with `-` replaced by `_` and
  **no** language separator (`SearchApiSolrBackend.php:2440-2446`).
- **`LanguageResolver`** (new) — resolves query language in order: a
  `search_api_language` condition on the query, else enabled site languages
  from the injected `LanguageManagerInterface`, else `['und']`.
- **`QueryBuilder`** — expands field references across language variants at
  every relevant call site; `buildSpellcheck()` emits the spellcheck request
  params; sorts on `sort_X3b_<languages[0]>_<id>`.
- **`DocumentBuilder`** — names fields by the item's own language;
  accumulates spellcheck sink values (reusing the #339 suggester-accumulation
  branch); writes each text field's sort copy for **every** enabled site
  language plus `und` (first-write-wins).
- **`ResponseParser`** — parses the flat spellcheck named-list envelope into
  `['suggestions' => [term => [words]], 'collation' => string]` per
  `SolrSpellcheckBackendTrait.php:24-42`; reads language-variant doc fields.
- **`WayfinderBackend`** — language-manager DI in `create()`, advertises
  `search_api_spellcheck`, accepts `solr_text_spellcheck`, passes the manager
  into `DocumentBuilder`.

No preset or server change was needed: `tm_*` catches `tm_X3b_de_body` as a
fallback, and longest-pattern-wins keeps `tm_X3b_en_*` on `text_en`
(`src/schema.rs:295`).

**Breaking change:** text field names change on the wire (`ts_body`/`tm_body`
style names become `tm_X3b_<lang>_body`). Existing indexes need a full
reindex. This is documented in `README.md` and `docs/PRD.md`.

**Ground truth used:** captured client traces in
`solr-ref/search-api/trace/*.json` (e.g. `tm_X3b_en_body`, `tm_X3b_und_title`;
non-text fields such as `ss_type`/`its_nid` carry no language) took precedence
over spec prose where the two disagreed. The frozen upstream source in
`coverage/search_api_solr_4.4.0_source/` was the second authority.

## Process and review outcome

Full TDD pipeline (test-writer / implementor / reviewer / reporter), three
review rounds.

**The spec itself was wrong twice, and was corrected by downstream stages:**

1. It specified excluding `twm_suggest`/`spellcheck_*` sink fields from `fl`.
   `QueryBuilder` builds no plain `fl` at all — only `mlt.fl`/`terms.fl`/`hl.fl`.
   The test-writer caught this; the deliverable was withdrawn mid-pipeline and
   is now a follow-up (below) rather than implemented.
2. It described `encodeSolrName()` as "char + hex" (`tmX3ben_body`) when the
   trace evidence shows underscore-wrapped `_X3b_`. The implementor followed
   the trace, not the spec prose.

A stage-1 test bug (`FieldMapperTest.php:131` omitted the 5th constructor
argument) was **escalated by the implementor rather than edited**, and routed
back to the test-writer (commit `c494aaf`), preserving the "the implementor
edited no test" invariant.

**Review round 1** found three must-fix bugs:

- **MF-1** — negated conditions (`<>`, `NOT IN`, `NOT BETWEEN`) OR'd across
  language variants formed a tautology matching every document (a document
  only ever carries the variant it was indexed in, so the other disjunct's
  negation is unconditionally true). An exclusion filter used for
  access/visibility filtering would have leaked data.
- **MF-2** — `spellcheck.dictionary` sent the raw langcode (`de-AT`) while the
  index sink is `spellcheck_de_AT`; the server does a literal string lookup,
  so hyphenated langcodes would silently return a permanently empty envelope.
  `en`/`de` happened to agree, which hid the bug in initial testing.
- **MF-3** — sort copies were written for the item's own language but queried
  for `languages[0]`, so non-first-language documents sorted as missing on a
  multilingual site. This was a regression this branch introduced (pre-#342,
  both sides agreed on the unqualified `sort_title`).

Notably, **the orchestrator's own round-1 instruction was itself wrong**: it
said "negated operators use AND," which contradicts upstream's NULL semantics
(`= NULL` ANDs, `<> NULL` ORs — `SearchApiSolrBackend.php:3455-3459`). The
implementor pushed back and substituted a polarity-of-emitted-clause rule
instead of an operator-name rule; the reviewer verified the pushback was
correct.

**Review round 2** found a fourth must-fix:

- **MF-4** — the `IN` arm of the same combination logic keyed off
  `!$hasValue` instead of `$hasNull`, making `IN [a, NULL]` on a text field
  the mirror-image tautology (an absent-language document satisfies the
  per-variant clause unconditionally). Fixed to
  `$hasNull = in_array(NULL, $value, TRUE); return $operator === 'NOT IN' ? !$hasNull : $hasNull;`.
  Sampled test coverage for this logic was replaced with an exhaustive
  12-row operator x NULL-shape truth table, so a third instance of the same
  class of bug is caught by construction rather than by luck.

**Pattern worth carrying forward:** two independent review rounds each found
a tautology in the same condition-combination logic, each time in a shape the
provider's sampled tests never reached. The response was exhaustive
truth-table coverage plus stating the governing rule ("does a document
lacking this language variant satisfy the per-variant clause?") in the
docblock, not just fixing the two instances found.

## Test evidence (verified independently by the orchestrator, not just self-reported)

```
cd drupal/search_api_wayfinder && vendor/bin/phpunit
  414 tests, 722 assertions, 0 failures, 0 errors
```
Stable across three consecutive runs. The 271 PHPUnit deprecation notices are
pre-existing baseline noise (not introduced by this branch).

```
cargo test              # 1261 passed, 63 suites
cargo fmt --check       # clean
cargo clippy --all-targets -- -D warnings   # clean
```

Fix commits `7412916..HEAD` touched only `src/QueryBuilder.php` and
`README.md` — no test files edited alongside a fix.

One intermittent Rust failure was seen and chased down during verification:
`tests/online_snapshot.rs` fails under CPU contention (a non-blocking HTTP
read returns `EWOULDBLOCK` at `:88`; the merge-observation assertion at
`:318` then times out). It passed 3/3 on an unloaded machine. This branch has
a zero-line diff against `main` for `*.rs`, `Cargo.toml`, `Cargo.lock`, and
`presets/`, confirming it is pre-existing flakiness, not something this
branch introduced. Recorded as a follow-up below.

## Commits

```
ce9b52a test(drupal): red tests for issue #342 language-aware naming + spellcheck
c494aaf test(drupal): pass the missing language arg in the spellcheck en case
03bf1b6 feat(search-api): language-aware field naming and spellcheck wiring
dc3c760 docs: record language-aware naming and delivered spellcheck (#342)
ffb079c test(drupal): round-2 red tests for MF-1/MF-2/MF-3 review bounce
f6d1910 fix(search-api): negated conditions AND across variants, shared spellcheck dictionary transform
d7e9429 docs(search-api): negated conditions AND variants, all-language sort copies
7412916 test(drupal): exhaustive truth-table coverage for MF-4 condition-combination logic
46b7f6c fix(search-api): IN with a NULL member ANDs its language variants (#342)
f88506d docs(search-api): state the variant-combination rule, note sort-copy growth
```

## Follow-ups (deferred, all filed)

1. **#357** — `tests/online_snapshot.rs` is load-sensitive and flakes under parallel
   test load — pre-existing, not introduced by this branch.
2. **#358** — string-field sort naming divergence: traces show
   `sort_X3b_en_field_sku`; upstream gates the sort copy on the Solr field
   name starting with `t` **or** `s`, while Wayfinder's `sortFieldName()`
   only special-cases text. Pre-existing divergence, not introduced here.
3. **#359** — the server consumes only the first `spellcheck.dictionary` value when
   multiple are sent (a per-resolved-language repeated param is emitted, but
   the server only honors one).
4. **#360** — `ResponseParser` drops the `{word, freq}` extendedResults suggestion
   shape entirely — marked with a `ponytail:` comment naming the ceiling,
   not implemented.
5. **#361** — `twm_suggest`/`spellcheck_<lang>` sink fields are returned inside `docs`
   for want of an explicit `fl` in `QueryBuilder`. This was the withdrawn
   spec deliverable (item 1 above) — `QueryBuilder` never emits a plain
   `fl` today, so there is nothing to exclude sinks from; adding `fl`
   emission is a separate, out-of-scope feature.
6. **#362** — with N enabled site languages, every text field now carries N+1
   identical sort copies (one per language plus `und`). This matches
   upstream's own behavior exactly, but it is real index-size growth worth
   tracking.

## Reviewer cap note

Review reached its default two-round cap (round 1: MF-1/MF-2/MF-3; round 2:
MF-4) and a third round was run to confirm round 2's fix and extend coverage
to the full truth table. Given that both prior rounds each found a live
tautology bug in the same area, the combination logic in
`QueryBuilder::buildCondition()` / `isNegatedClause()` is the piece of this
change most likely to still reward a further, fresh review pass — the
exhaustive truth table closes the shapes that were tested, not necessarily
every shape a future reviewer could construct.
