# #340 -- `{!payload_score}` over `boost_term_payload`

**Date:** 2026-08-04. Closes #340. Branch
`markdlabrecque/issue-340-payload-score-boost` off `main`.

Six commits: `991283a` (pls fixtures), `1647641` (red tests), `13a6d74`
(feature), `a466688` (plsz fixtures), `ee6879f` (review round 2 fixes),
`cc47b04` (review follow-ups). 44 files changed, +2732/-23 across
`main...HEAD`.

## Premise verification

Three corrections to the issue text, each verified against a real `solr:9`
before implementing and posted to the issue thread.

1. **`includeSpanScore` defaults to `false`, not `true`.** A `{!payload_score}`
   score is the raw payload aggregate with no BM25 factor at all --
   `f=boost_term v="dog" func=max` scores d3 exactly `4.5`, and the explicit
   `includeSpanScore=false` form is byte-identical. `includeSpanScore=true`
   gives `1.7091298` instead, which is the opposite of what the reference
   guide's prose implies. This matters beyond being "the docs are wrong": the
   default -- the only form the module ever emits -- is exactly comparable,
   so these fixtures sit outside the PRD's ratified BM25-magnitude divergence
   rather than under it (finding 165).
2. **The issue's scope note is backwards for this parser.** The client emits
   `{!payload_score}` *inline*, never at position 0, and the two forms are
   not equivalent: a position-0 block sets the parser for the whole `q` and
   discards the remainder, so a two-block `q` scores d3 `4.5` rather than
   `dog(4.5)+quick(2.5)=7.0` (`pls_two_terms`). Put something else at position
   0 and the blocks become nested subqueries the lucene parser sums as SHOULD
   clauses -- the shape `SearchApiSolrBackend::preQuery` actually builds
   (`pls_client_shape`, d3 scores `(4.5+2.5+1.0)*2.0 = 16.0`). Inline
   `{!payload_score}` support was therefore in scope, unlike inline
   `{!func}`/`{!boost}`, which the client genuinely never nests and stays
   descoped (finding 168).
3. **`v` is not mandatory.** With no `v` local param, the query text is the
   bound run after `}` -- Solr's general local-params contract
   (`QParser.getParser`), which this parser does not opt out of. This was
   found during review round 2: the first implementation encoded "`v` is
   mandatory" because the only fixture without `v` (`pls_err_no_v`) also had
   no bound text to fall back to, which made the wrong generalization look
   confirmed until the reviewer's suspicion was captured and fixture-backed
   (`plsz_vbound_max`, `plsz_vbound_v_wins`, finding 173).

## What landed

- **`src/function_query.rs`** -- extended with `PayloadFunction`
  (`min`/`max`/`average`/`sum`) and a bespoke `PayloadScoreQuery`/
  `Weight`/`Scorer`, alongside #289's `FunctionScoreQuery` rather than a
  second evaluator. `PayloadColumn` resolves the term's payload-bearing
  ordinals with one prefix-range scan of the fast-field column's term
  dictionary per segment (plus, after review, one exact lookup for the bare
  `<term>` ordinal), then reads per-document factors from it -- no
  per-document string decode.
- **tantivy 0.26 has no postings payloads**, so scoring is a columnar read
  rather than a payload-decoder over postings. `boost_term_payload` carries
  two tokenizers on the same field: the indexing tokenizer strips the
  `|<float>` suffix (so term matching still works for a plain lucene query
  against the field), and a separate fast-field tokenizer keeps the token
  verbatim (`<term>|<float>` or bare `<term>`), because tantivy keeps
  `Index::tokenizers()` and `Index::fast_field_tokenizer()` as independent
  managers. No synthetic sibling field.
- **`src/schema.rs`** -- the `boost_term_payload` field type: whitespace
  tokenization, `LengthFilter` (min 2), lowercasing, dedup, and the
  index-side `|<float>` strip, matching Solr's field-type chain and
  `Utility::flattenKeysToPayloadScore`/`addIndexField`'s 2..100-char skip.
- **`src/core_index.rs`** -- `build_payload_score_query` wires `f`/`v`/`func`
  through both the position-0 path (`parse_function_query_q`-adjacent) and
  the inline path (`extract_nested_queries`), validates `f` present/defined
  and payload-capable, `func` known (case-sensitive), and the query text
  either from `v` or (after review) the bound run following `}`.
- **`src/local_params.rs`** -- the `{!payload_score ...}` block is now
  recognized inline as well as at position 0; `bound_token_len` picked up a
  `ponytail:` naming the bare-`^n`-after-`}` ceiling (see Follow-ups).
- **20 `pls_*` fixtures** on a dedicated `pls` core (5 docs, d3 carrying
  `dog` twice at different payloads -- the only way the four functions are
  distinguishable), plus **12 `plsz_*` fixtures** on a second `plsz` core
  added specifically to fixture-back finding 172 (payload-free occurrence
  scores `1.0`) without disturbing the committed `pls_*` set.

## Review history

Round 1 raised 2 must-fix items; round 2 raised none and approved, with 4
non-blocking follow-ups.

- **Round 1, item 1 (real bug):** the weight boost (`^n` on a
  `{!payload_score}` block) was dropped. `PayloadScoreScorer` never reads its
  child's score, so the `boost` tantivy's `BoostQuery` hands down through
  `Weight::scorer` was forwarded into the child and silently lost there.
  Fixed in `ee6879f` by applying it to the payload aggregate in `score()`
  instead, and (in `cc47b04`) passing `1.0` rather than `boost` to the child
  scorer so "applied to the aggregate, not the child" is explicit rather than
  comment-dependent.
- **Round 1, item 2 (suspect docstring, turned out real):** the reviewer
  flagged the "`None` for no payload -> caller scores `0.0`" docstring as
  unverifiable from the diff alone. It was wrong in a way round 1's own guess
  also got wrong: a payload-free occurrence contributes the factor `1.0`, not
  `0.0` and not nothing -- Solr's `PayloadDecoder` decodes a null payload to
  `1f` rather than skipping the position. Round 1 additionally guessed that
  only `sum` would be affected; that guess was also wrong, since all four
  functions see the same `1.0` factor in their input list. Fixed in `ee6879f`
  (finding 172); the `plsz` core exists specifically to fixture this, with
  `plsz_mixed_min` as the discriminating row (skipping the bare occurrence
  would give `2.0` instead of the correct `1.0`).
- **Round 1's `Weight::explain` prescription was rejected on evidence, not
  overridden.** Round 1 also said to apply the boost inside `explain` too.
  That was not done: tantivy's `BoostWeight::explain` already multiplies the
  wrapped explanation by the boost, so applying it a second time inside
  `PayloadScoreWeight::explain` would double-apply it. Round 2 verified this
  against the tantivy source and confirmed the rejection was correct.
- **Round 2 approved with no must-fix items.** It confirmed the boost fix,
  confirmed the explain rejection, verified `unwrap_or(1.0)` is genuinely
  unreachable (swapping it for `.expect()` panicked zero times across the
  suite), confirmed all 12 `plsz` rows genuinely replay through the
  differential harness rather than being silently skipped, confirmed the
  fixtures are real Solr output, and confirmed no `pls_*` expectation moved
  in the process. It raised 4 non-blocking follow-ups, all addressed in
  `cc47b04`:
  1. the `v`-defaults-to-bound-run gap (finding 173) -- implemented, plus two
     `plsz_vbound_*` fixtures;
  2. duplicate-identical-values behavior (finding 174) -- two
     `plsz_dup_*` fixtures showing `RemoveDuplicatesTokenFilter` is
     position-scoped, so two identical `bird|2.0` occurrences both count;
  3. the bare-`{!block}^2` ceiling -- given a home as a `ponytail:` on
     `bound_token_len`, naming it as an extractor-wide limit shared with
     `{!edismax}`, unfixtured in both directions;
  4. passing `1.0` rather than `boost` into the child scorer, making the
     "boost applies to the aggregate, not the child" invariant explicit
     rather than resting on a comment.

Two review rounds is the reviewer's default cap in this pipeline. Round 2
approved outright, so no escalation was needed here, but per the pipeline
rule this work has had exactly the capped number of passes, not an open-ended
number -- a third pass was not run.

## Gates / evidence

- `cargo test`: 64 suites / 1275 tests, 0 failed. Run independently at report
  time, not taken from the `cc47b04` commit message.
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`: both
  clean, likewise re-run.
- Hermetic: no network, no Docker; the fixtures were captured earlier and are
  replayed from committed JSON.
- All 32 `pls_*`/`plsz_*` rows replay through the differential harness
  (`tests/differential.rs`). 29 wire-match exactly (0 diffs, scores within
  `1e-3`). 3 are declared divergences, confirmed by reading the harness
  directly rather than trusting this summary:
  - `pls_err_nonpayload` in `ACCEPTED_DIVERGENCES` -- `f=body` (a plain
    `text_general` field, no payloads) is an uncaught NPE / HTTP 500 upstream
    with a `trace` and no `msg`; Wayfinder answers 400 naming the field.
    Ratified permanently in PRD divergence 11 (reproducing an upstream crash
    is not a goal). The check arm re-asserts the fixture is still the 500/NPE
    shape before accepting Wayfinder's 400, so this cannot rot into a false
    excuse if the fixture ever changed.
  - `pls_span_true` in `EXPECTED_DIVERGENCES_MANIFEST_ERRORS` --
    `includeSpanScore=true` is descoped; Wayfinder returns the payload-only
    score (`4.5`) rather than Lucene's span-plus-payload blend (`1.7091298`).
    Self-expiring: the entry says to remove it if an inline span-score
    evaluator ever lands.
  - `pls_multiterm_v` in `EXPECTED_DIVERGENCES_MANIFEST_ERRORS` -- a
    multi-term `v` is real Solr's ordered `SpanNearQuery` (finding 171);
    Wayfinder supports single-term `v` only. Self-expiring on the same
    pattern.
- Differential repeated 3x with no flake (per the `ee6879f` commit message).
- Mutation-tested, across both review rounds:
  - the two failing-correctly guards named in the original red-test spec
    (case-sensitive `func` match, `v` length-filter behavior) --
    `tests/payload_score.rs` names these as the spec-C mutation guards;
  - dropping the boost multiply on the aggregate;
  - scoring a payload-free occurrence as `0.0`;
  - skipping a payload-free occurrence entirely -- caught only by
    `plsz_mixed_min`, since `unwrap_or(1.0)`'s fallback path gives the same
    answer on the simpler `plsz_bare_*` rows;
  - the `v`-vs-bound-run fallback both ways -- ignoring `v` fails
    `plsz_vbound_v_wins`, dropping the bound-run fallback fails
    `plsz_vbound_max`.

Each of these is reported as a single fixture or a named pair of fixtures
where that is what the evidence actually is; none of them is backed by more
rows than stated above.

## Follow-ups (named descopes)

- **Multi-term `v`.** Real Solr builds an ordered `SpanNearQuery` over the
  payload field when `v` analyzes to more than one term (finding 171). Not
  implemented; `pls_multiterm_v` is a self-expiring guard in
  `EXPECTED_DIVERGENCES_MANIFEST_ERRORS`. Not on the client's path (every
  `boost_term` value the module writes is a single `sprintf('%s|%.1F')`
  token), but reachable by anyone indexing the field type directly.
- **`includeSpanScore=true`.** Lucene's span-plus-payload blend. Not
  implemented; `pls_span_true` is a self-expiring guard in the same list.
  Also not on the client's path -- the module never emits this parameter.
- **Inline `{!func}`/`{!boost}` remain position-0 only**, inherited from
  #289. `{!payload_score}` is the one parser that got inline support in this
  branch, because it is the one the client actually nests; `{!func}`/
  `{!boost}` still 400 on an inline block via `extract_nested_queries`'s
  unsupported-parser path.
- **The bare-`{!block}^2` extractor ceiling.** `bound_token_len` ends a
  bound token at whitespace or `)`, so `{!payload_score ...}^2` is silently
  unboosted -- the `^2` is swallowed into the discarded bound token and
  dropped. `({!payload_score ...})^2` works, because `)` ends the token
  first. Given a `ponytail:` comment naming it as a limit of the extractor
  itself (shared with `{!edismax}`), not of any one parser; unfixtured in
  both directions (no capture puts a boost on a nested block, and the client
  never emits one). Lifting it needs the extractor taught that `^`/`~` end a
  bound token too.
- **`ms`/`rord` remain off the client path**, inherited from #289 (date/
  ordinal function queries); not touched by this branch.

## Divergence from committed fixtures

None. No `pls_*` expectation moved during review; the two rounds only added
new `plsz_*` fixtures and fixed the code to match findings already captured
against real Solr.
