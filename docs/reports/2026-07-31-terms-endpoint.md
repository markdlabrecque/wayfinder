# Issue #155 -- `GET /solr/{core}/terms`

- Branch: `155-terms-endpoint`
- Worktree: `/Users/mark/Projects/wayfinder-155`
- Commits: `1b96d60` (red tests), `fa8fdb4` (implementation), `b123eea` (round-2 review fixes:
  delete false finding 93, gate on `terms=true`, reject non-text `terms.fl`), `b095cb7` (red
  tests: dynamic-field resolution and `json.nl` honesty gaps), `a0e8bf4` (fix: resolve dynamic
  `terms.fl` names, stop ignoring `json.nl`), `f73cd1d` (test: pin the rejection reason, not
  just the 400)
- Base: rebased onto `origin/main` `a1b637b`

## What was built

`GET /solr/{core}/terms` now serves Solr's TermsComponent -- enumeration of a field's analyzed
inverted-index term dictionary with per-term document frequency. This is the last of a
four-endpoint batch (#155/#156/#157/#158) closing coverage-contract gaps against
`search_api_solr` 4.4.0. The handler is `terms` in `src/lib.rs`, backed by
`CoreIndex::field_terms` in `src/core_index.rs`.

**Shape, read off ground truth** `solr-ref/search-api/trace/00028.json`
(`GET .../terms?omitHeader=true&wt=json&json.nl=flat&terms=true&terms.fl=tm_X3b_en_title`):
- `terms=true` gates the component; absent or `terms=false` produces no `terms` block at all
  (an unconditional block was the round-2-fixed defect below).
- `terms.fl` is repeatable; each field gets its own flat `[term, count, term, count, ...]`
  array under `terms`, `json.nl=flat`'s shape and the only one this endpoint renders.
- Ordering is Solr's `terms.sort=count` default: count descending, ties broken term-ascending.
  `terms.limit` defaults to 10.
- A `terms.fl` naming an undefined field, or a defined-but-non-text field, is a 400 with no
  `response` key (no base query to have partially run).

**The significant scope gap, and how it was resolved.** The first implementation (`fa8fdb4`)
400'd `terms.fl=tm_X3b_en_title` as an undefined field -- the exact request
`search_api_solr` actually sends (it is the only field the module's own traced request names),
and the field in the ground-truth fixture itself, because `check_terms_field` consulted
`WayfinderSchema::field` directly rather than resolving dynamic field names the way `/select`
does. The orchestrator rejected the cheaper option of documenting this as a known limitation:
an endpoint that rejects its own ground-truth request is not implemented, root cause or nothing.
Fixed in `a0e8bf4` by reusing `CoreIndex::field_target` (via a new public
`resolves_field_name`) for existence, and `WayfinderSchema::resolved_value_kind` for the
text-type check, so a dynamic name now resolves the same way on `/terms` as it already does on
`/select`. This went through its own red-tests-then-implementation cycle (`b095cb7` then
`a0e8bf4`), the second inside this single branch.

**Term enumeration over a Tantivy JSON container** (`CoreIndex::field_terms`), recorded here
because it is not obvious from the diff and matters for future work touching the same path: a
dictionary key for a value inside the shared dynamic-field JSON container is
`<path bytes><JSON_END_OF_PATH><type tag><term utf-8>` after `serialized_value_bytes()` strips
the 1-byte type tag. Verified directly against the pinned tantivy 0.26.1 sources:
`JSON_END_OF_PATH = 0u8` (`tantivy-common-0.11.0/src/json_path_writer.rs:10`),
`Type::Str = b's'` (`schema/field_type.rs:55`), `TERM_TYPE_TAG_LEN = 1`
(`schema/term.rs:29`), the field layout documented at `schema/term.rs:298`. `0x00` cannot occur
inside a path (`core/json_utils.rs:88`, `postings/json_postings_writer.rs:119`), which is what
makes prefix-anchored enumeration sound: `field_terms` builds the field's own address prefix,
streams each segment's inverted index, seeks `.range().ge(&prefix)`, breaks at the first key
that no longer carries the prefix, and sums `doc_freq` across segments for the survivors.

## Test evidence (re-run for this report, not copied)

- `cargo fmt --check` -- clean.
- `cargo clippy --all-targets -- -D warnings` -- clean (CI's exact invocation).
- `cargo test` -- 701 passed, 37 suites, 0 failed.
- Coverage: `cargo run -- coverage --format json` (measured, not asserted at run time) and
  `tests/search_api_coverage.rs` both show `50/75 -> 53/75`. Three items flip at once: the
  `GET /solr/{core}/terms` endpoint itself, the `terms.enumeration` request semantic, and the
  `terms.terms` response field. Denominator unchanged -- no new contract items, three
  previously-uncovered ones now answered.
- Ground truth: `solr-ref/search-api/trace/00028.json`. No `solr-ref/manifest.tsv` row exists
  for `terms` -- see follow-ups.

## Review outcome

Two rounds (the pipeline's default cap), both by an independent Opus reviewer. This work could
use further review passes beyond the two the cap allowed -- nothing in either round certified
the diff as exhaustively checked, only that the specific attacks made came back clean.

**Round 1** found two must-fix items:

1. **A fabricated Solr-ref finding.** "Finding 93" claimed Tantivy's English `StopWordFilter`
   retains the word "over" where Solr's `text_en` drops it. This was false in both directions:
   the captured `stopwords_en.txt` does not contain "over", and Tantivy's inlined English list
   is the same 33-word Lucene list. The false claim had propagated into three places --
   `docs/solr-ref-findings.md`, a test in `tests/terms.rs`, and a `ponytail:` comment in
   `src/lib.rs` -- all three were deleted in `b123eea`, with no substitute finding recorded.
   The round-2 reviewer independently re-verified that both Tantivy's and Lucene's English stop
   lists are the same 33 words and neither contains "over".
2. **An untestable rejection assertion.** `terms_dynamic_field_of_non_text_type_is_rejected`
   originally asserted only HTTP 400 plus a message substring naming the field -- a shape
   satisfied equally by `wayfinder::UndefinedField` and the intended
   `wayfinder::TermsUnsupportedField`, so the test could not tell the two rejection reasons
   apart. Tightened in `f73cd1d` to assert the error code at `/error/metadata/3` and the
   "non-text field" wording specifically, with a mirror assertion added to
   `terms_dynamic_name_matching_no_rule_is_still_a_400` pinning `UndefinedField` from the other
   direction. Mutation-proven: swapping the two error codes in `check_terms_field` now fails
   the suite.

Also fixed in the same round-1 pass (`b123eea`): the `terms` block was being emitted
unconditionally rather than gated on `terms=true`, and a `terms.fl` naming an undefined or
non-text field was not rejected at all.

**Round 2** verified both fixes directly rather than trusting the round-1 diff's claims --
independently re-derived the English stopword lists from source rather than re-reading the
deleted finding, and re-ran the tightened assertions to confirm they now distinguish
`wayfinder::UndefinedField` from `wayfinder::TermsUnsupportedField`. It also caught the dynamic
`terms.fl` resolution gap described above (`a0e8bf4`'s fix), and the `json.nl` param being
accepted with any value while the handler always rendered flat regardless (now `map`/`arrarr`/
`arrmap` 400, matching the meaning `src/facet.rs`'s `JsonNl` already gives those values).
Approved with no must-fix items outstanding after the fixes.

## Follow-ups deferred by the reviewer -- filed as issues

1. **#160** -- the schema loader accepts duplicate `[[field_types]]` names, silently leaving
   the second definition dead. Pre-existing, surfaced (not caused) by this batch's review.
2. **#162** -- three coverage response-field probes (including this endpoint's `terms.terms`)
   accept an empty container as "covered," with no content assertion.
3. **#164** -- a dynamic field name containing a dot resolves but never matches: the read path
   splits on `.`, the write path does not. Pre-existing, shared with `/select`, not caused by
   this branch.

## Follow-up NOT yet filed

No `solr-ref/manifest.tsv` row exists for `terms`, so the differential harness does not cover
this endpoint. Adding one needs a capture against the differential core, and is likely to
surface a Tantivy-vs-Solr analyzer difference: the captured `solr-ref/search-api/configset`
uses `StandardTokenizerFactory`, a `LengthFilterFactory min="2"`, a
`WordDelimiterGraphFilterFactory`, and an `accents_en.txt` char-filter mapping that Wayfinder's
`text_en` chain has no counterpart for. None of that is a verified finding yet -- the capture is
what would settle it -- and no issue has been opened for it. This is a real gap in the batch's
compatibility evidence, not a closed loop.

## Bottom line

`GET /solr/{core}/terms` lands, the last of the #155/#156/#157/#158 batch, giving
`search_api_solr` the term-enumeration endpoint it needs and taking coverage from 50/75 to
53/75 (three items: the endpoint, `terms.enumeration`, `terms.terms`). All local gates green
(fmt/clippy clean, 701/37 tests passing). Two review rounds: round 1 caught a fabricated
Solr-ref finding that had propagated into docs, tests, and a code comment, and an assertion
that could not distinguish its two rejection paths -- both fixed on this branch; round 1 also
caught the `terms=true` gating bug and the missing `terms.fl` validation, both fixed alongside.
Round 2 verified all of the above,
caught an ignored `json.nl` param, and caught the larger gap: the first implementation rejected
`search_api_solr`'s actual request (a dynamic field name) as undefined. That was root-caused by
reusing `/select`'s field resolution rather than documented as a limitation, in its own
red-then-green cycle on this branch.
Three follow-ups filed as issues (#160, #162, #164), none blocking and none specific to this
endpoint. One follow-up not yet filed: no differential-harness manifest row exists for `terms`,
so this endpoint's Solr-wire fidelity is untested against a live Solr beyond the single captured
trace, and the capture is expected to surface real analyzer differences when it happens.
