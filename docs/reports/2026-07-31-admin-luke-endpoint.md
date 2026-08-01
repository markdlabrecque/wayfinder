# Issue #157 -- `GET /solr/{core}/admin/luke`

- Branch: `157-admin-luke`
- Worktree: `/Users/mark/Projects/wayfinder-157`
- Commits: `435f95f` (red tests), `30aeb59` (implementation), `ecc8c4b` (fix: prose
  correction to match what the handler serves)
- Base: `e5f75a8`

## What was built

`GET /solr/{core}/admin/luke` now serves index statistics and a field list, reversing the
#57 descope for this endpoint. The handler is `admin_luke` in `src/lib.rs`.

**Client consumption.** `search_api_solr`'s `SearchApiSolrBackend.php:993` reads
`$data['index']['numDocs']` and nothing else, to print "N items indexed" on the module's
server-status screen. That is the entire consumption surface this endpoint exists to serve.

**Real vs placeholder.** Five figures in `index{}` are real, read off the live searcher per
request: `numDocs`, `maxDoc`, `deletedDocs`, `hasDeletions`, `segmentCount`. The rest of
`index{}` (`version`, `current`, `directory`, `segmentsFile`, `segmentsFileSizeInBytes`,
`userData`) are static placeholders describing Lucene index identity Wayfinder has no
equivalent for. `indexHeapUsageBytes` and `lastModified` are omitted entirely, because the
real Solr response omits both in the captured trace -- emitting them would have been less
faithful than the capture, not more.

**One engine addition.** `CoreIndex::deleted_doc_count()` (`src/core_index.rs`), summing
`SegmentReader::num_deleted_docs()` over the live searcher's segment readers. `maxDoc` is
then `numDocs + deletedDocs`, matching Lucene's semantics for that figure.

**`fields{}`.** Reflects the live schema (`[[fields]]` config), with no Lucene flag strings
(`ITS-----OF-----`), `topTerms`, or `histogram` -- those encode index internals Wayfinder does
not have, and a plausible fake is worse than an omitted key. Dynamic-field *instances* are not
listed: Wayfinder stores dynamic values in the shared `_dynamic`/`_dynamic_text` container, so
there is no per-instance index field to enumerate. The reviewer verified this gap is inert
here -- dynamic rules are already surfaced by `/ui/schema`, and luke's only consumer reads
`index.numDocs` -- unlike sibling #155, where the analogous gap was a real defect.

**Deliberate divergence** recorded in `docs/PRD.md` section 5's v2.75 block. No
`manifest.tsv` row: half of Solr's luke response is Lucene identity that cannot be reproduced
honestly.

## Test evidence (re-run for this report, not copied)

- `cargo fmt --check` -- clean.
- `cargo clippy --all-targets -- -D warnings` -- clean (CI's exact invocation).
- `cargo test` -- 677 passed, 36 suites, 0 failed.
- Coverage: `tests/search_api_coverage.rs` asserts `50/75` (up from `48/75`).
- Ground truth: `solr-ref/search-api/trace/00024.json` (document at `.response.body`).

## Review outcome

Two rounds, both by an independent Opus reviewer.

**Round 1** attacked the two load-bearing numeric claims rather than trusting the diff's
comments:
- Independently verified `maxDoc = numDocs + deletedDocs` from tantivy 0.26.1's source
  (`num_deleted_docs() == max_doc - num_docs`, and `Searcher::num_docs()` documented as
  excluding deletes), then empirically across 12 rounds of index-and-delete with background
  merges, confirming `select?q=*:*`'s `numFound` agrees with luke's `numDocs`.
- Proved the values are live rather than snapshotted at startup: two GETs in one run with
  indexing between them, and by mutating the handler to serve constants and confirming the
  suite catches it.

Both held; no must-fix items from the numeric claims.

**Round 2** bounced three artifact-truth defects, all in prose rather than behaviour:
- `docs/PRD.md` (and two mirroring comments -- the `admin_luke` doc comment and a
  `search_api_coverage.rs` comment) enumerated four real `index{}` figures where the handler
  serves five, omitting `hasDeletions`.
- A stale red-phase doc comment on `luke_route_exists_under_the_core_path` claimed the route
  404s today, three lines above an assertion that it does not.
- The `tests/admin_luke.rs` module header claimed the Lucene-identity placeholders were
  asserted "presence only," when no test asserted them at all.

The third fix added `luke_index_lucene_identity_placeholder_keys_are_present`, closing a real
hole: before it, the handler could have stopped emitting every Lucene-identity key with the
suite staying green. Mutation-proven (dropping a placeholder key from
`admin_luke_index_placeholders` now fails the suite).

Fixed in `ecc8c4b`. Approved with no must-fix items outstanding after the fix.

The implementor also self-authored `luke_unknown_core_is_a_json_404` outside its normal
remit, because the red-phase suite had no unknown-core coverage. The reviewer verified that
test is well-formed and mutation-proven rather than written to pass -- sibling #156 shipped
exactly that bug (missing `check_core`).

## Follow-ups deferred by the reviewer -- not fixed here

1. **Loose coverage-probe contract.** `src/coverage.rs`'s `admin.luke.index` probe is
   `is_object`-only, so an empty `{}` would count as covered. Tighten to require `numDocs` be
   present as a `u64`. Shares this shape with the `terms.terms` and
   `schema.fieldtypes.fieldTypes` probes -- issue #156's report records the same item.
2. **Constant-count mutation slips past one test.** One test indexes exactly 6 docs, the same
   count the trace incidentally has, so a constant-`numDocs=6` mutation would slip past it
   (caught only by a sibling test using 7 docs). A count differing from the trace's would make
   each test independently mutation-tight.
3. **Key order in `index{}` differs from the trace** (`hasDeletions`/`segmentCount` emitted
   before `version`/`current`, where the trace has them after). Inert for JSON consumers,
   noted only.
4. **Coverage-fraction pin will go stale.** The `50/75` assertion in
   `tests/search_api_coverage.rs` conflicts with sibling branches #155 and #158, which are
   also incrementing the same fraction. Whichever merges later must rebase and re-derive the
   fraction rather than mechanically resolving the conflict.

## Bottom line

`GET /solr/{core}/admin/luke` lands, reversing the #57 descope for this endpoint and giving
`search_api_solr`'s server-status screen a real `numDocs` instead of a 404-swallowed `FALSE`.
All local gates green (fmt/clippy clean, 677/36 tests passing), coverage 48/75 -> 50/75. Two
review rounds: round 1 independently verified the `maxDoc` identity and live-per-request
sourcing against tantivy's source and empirical index/delete cycles; round 2 caught three
artifact-truth defects (a PRD/comment undercount of real `index{}` fields, a stale red-phase
doc comment, and an unbacked "presence only" claim) and closed the real test-coverage hole
behind the third one, fixed on this branch in `ecc8c4b`. Four follow-ups deferred, none
blocking: a loose coverage-probe contract shared with two other endpoints, a constant-count
mutation gap in one test, a cosmetic key-order divergence from the trace, and expected
coverage-fraction rebase churn against siblings #155/#158.
