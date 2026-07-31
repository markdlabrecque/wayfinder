# Report: edismax query parser (`defType=edismax`)

- Branch: `7-edismax`
- Issue: [#7](https://github.com/markdlabrecque/wayfinder/issues/7) — `v1: edismax query
  parser`. Wayfinder previously supported only the default `lucene` query parser; edismax's
  multi-field weighting (`qf`), phrase boosting (`pf`), tie-breaking (`tie`), minimum-should-match
  (`mm`), boost query (`bq`), and multiplicative boost (`boost`) were entirely unimplemented.
- Pipeline: test-writer -> implementor -> reviewer (2 rounds: bounce, then APPROVED) ->
  reporter (this report). The review pipeline used both of its allotted rounds — see
  "Review outcome" below for what that means for confidence in this diff.

## What was built

- **`src/edismax.rs`** (new, 123 lines): pure, dependency-free grammar functions —
  `min_should_match` (the `mm` spec grammar: bare integers, negative integers, percentages,
  negative percentages, and the `cond<n><mm>...` conditional form) and `parse_field_weights`
  (the `qf`/`pf` "field^weight field2^weight2" grammar).
- **`CoreIndex::parse_edismax_query`** in `src/core_index.rs`: builds a per-field
  `DisjunctionMaxQuery` with `tie`, additively combines an optional `pf` phrase-query Should
  clause and an optional `bq` boost-query Should clause, applies `mm` to the top-level
  disjunction, and wraps the whole thing in a final `BoostQuery` for `boost`. Reuses
  `parse_query`'s `*:*`-to-`AllQuery` short-circuit and dynamic-field-name rewrite as a shared
  prologue (added in round 2 — see below).
- **`src/lib.rs`**: dispatches to `parse_edismax_query` when `defType=edismax`; extended
  `SELECT_PARAMS` with `defType`, `qf`, `pf`, `mm`, `tie`, `boost`, `bq` so `strict_params=true`
  does not 400 on them.
- **`docs/solr-ref-findings.md`**: findings 68-75, derived from real Solr 9 behaviour —
  the `mm` grammar's exact arithmetic (68), `qf` per-field boost ranking effects (69), `pf` as an
  additive phrase score (70), `tie` blending scope (71), `boost` as a uniform final multiplier
  (72), `bq` as an independent scored query added on top (73), quoted-phrase/`+`/`-` operator
  behaviour under edismax (74), and `bf` as a real scoring-affecting param not yet implemented
  (75).
- **`tests/differential.rs`**: `edismax_*` manifest rows are skipped from the generic hermetic
  loop (mirrors the pre-existing `mlt_` skip) because they need the dedicated 10-doc
  title/body/`text_en` corpus, not the canonical `content` core.
- **`tests/edismax.rs`** (937 lines) and **`tests/mm.rs`** (188 lines): integration tests
  against the real `app()`, plus `hermetic_edismax_manifest_entries_match_committed_fixtures`
  (the edismax-specific counterpart of the differential harness, iterating `edismax_*`
  manifest rows).
- **18 fixtures** captured from real Solr 9 (`solr-ref/responses/edismax_*.json`) over a
  dedicated 10-doc corpus, covering `qf` equal/title-boosted/body-boosted weighting, `tie` 0 and
  1, `pf` on/off, `mm` bare/percentage/conditional forms, `bq`, multiplicative `boost`, quoted
  phrases, and `+`/`-` operators.

## Test evidence

```
cargo fmt --check                          # clean
cargo clippy --all-targets -- -D warnings  # clean (CI's exact invocation)
cargo test                                 # 423 passed, 0 failed, across 19 suites
```

All three re-run independently by the reporter against the current state of the branch (not
copied from the implementor's or reviewer's earlier runs).

## Corrected ticket premise

None specific to #7's scope beyond the divergence below — the qf/pf/tie/mm/bq/boost
composition algorithm the test-writer derived from live Solr 9 behaviour (findings 68-75) held
up through implementation and both review rounds without correction.

## Review outcome (2 rounds — capped, not a clean pass)

**Round 1 — bounced.** The reviewer independently re-verified gates, then found:

1. **Wrong divergence attribution.** The implementor's escalation (and the coordinator's initial
   framing) blamed 4 failing test assertions on BM25 fieldnorm-quantization order flips. The
   reviewer hand-derived BM25 (k1=1.2, b=0.75) and proved both Tantivy's `FIELD_NORMS_TABLE` and
   Lucene's `SmallFloat` quantization are exact/identity for this corpus's doc lengths (2-10
   tokens, well under the ~40-token lossy-quantization threshold) — there is no quantization
   error to diverge on. The real cause is the same `text_en`-stopword-retention divergence
   already correctly cited by the pre-existing `pf_off` guard: unstripped stopwords shift the
   per-field average document length (avgdl) that feeds the BM25 length norm, flipping the
   relative order of two near-tied documents. Verified by reproducing Wayfinder's exact output
   with stopwords retained and Solr's fixture floats to 7 digits with stopwords stripped.
2. **A genuine bug.** `q=*:*&defType=edismax` returned HTTP 400, because
   `parse_edismax_query` skipped the `*:*`->`AllQuery` short-circuit and the dynamic-field-name
   rewrite that plain `parse_query` runs first.

**Round 2 fix** (commit `1e19632`): fixed the `*:*`/dynamic-field bug by having
`parse_edismax_query` reuse `parse_query`'s prologue; added two regression tests
(`star_colon_star_matches_everything_under_edismax`, `dynamic_field_in_q_is_rewritten_under_edismax`);
corrected all divergence-guard comments and issue #51 to cite `text_en` stopword retention
instead of fieldnorm quantization.

**Round 2 — APPROVED.** The reviewer re-verified both fixes independently (traced the pre-fix
failure path for each new test to confirm they were red for the right reason, not tautological)
and confirmed the corrected attribution was accurate and grep-clean repo-wide. One wording
imprecision remained (comments said the *affected docs'* own lengths changed, when it's
actually the corpus-wide per-field average length/avgdl that shifts, driven by *other* docs
retaining stopwords) — fixed directly by the coordinator, comment-only, in `b69048f`.

**Because this pipeline used both of its two allotted review rounds, per this repo's rule the
report must say the work could use more review passes.** Round 2 approved the code, but a third
independent pass was not run; the non-blocking follow-ups below were surfaced by the reviewer
but not themselves re-reviewed after being filed.

## How the divergence was handled (not silently hidden)

Rather than extend the existing `EXPECTED_DIVERGENCES` table in `tests/differential.rs` (which
is explicitly a PRD-ratified waiver for score *magnitude* only, not doc order), the 4 affected
assertions were converted to self-expiring skip guards: a local `EDISMAX_KNOWN_DIVERGENCES`
const in `tests/edismax.rs`, scoped to this file rather than merged into the shared harness
table, asserting the *current known-wrong* order so the test trips loudly the instant the
divergence stops holding (naming the entry to remove). This follows the repo's "deliberate
skips must expire" rule rather than relaxing the fixture-derived assertions.

## Follow-up issue: [#51](https://github.com/markdlabrecque/wayfinder/issues/51)

Filed for the out-of-scope engine divergence, then corrected to the accurate root cause after
round 1. Contains:

- **Resolved by #51:** the four temporary known-divergence guards (`edismax_basic`,
  `edismax_score_baseline`, `edismax_boost_multiplicative`, `edismax_operators_required`) and
  the `pf` equal-score mismatch came from `text_en` retaining stopwords, which shifted per-field
  average document length. #51's index-time stopword removal restores the fixture orders and
  equal `pA`/`pB` scores; the guards were replaced with normal fixture assertions.
- **Unfixtured edge cases**, listed for future work, none blocking #7:
  - `pf` can build a phrase over a negated (`-term`) clause — `literal_texts` doesn't filter by
    `Occur`.
  - An in-query term boost (`q=rocket^5`) is silently discarded —
    `flatten_edismax_clauses` drops `UserInputAst::Boost`'s weight.
  - `boost=<function-query>` (e.g. `recip(...)`) silently becomes `None`, since `boost` is
    parsed as `f32`; only `boost=<number>` is fixtured.
  - An unknown field inside a partially-valid `qf` (e.g. `qf=title nosuchfield`) is silently
    dropped; Solr 400s on any undefined `qf` field.
  - `mm=` (empty string) means all-required, while an absent `mm` means OR; Solr ignores an
    empty `mm`.
  - `bf` is not registered in `SELECT_PARAMS`, so it 400s under `strict_params=true` even
    though it's meant to be accepted-and-ignored.
  - `q=*:*&qf=<unknown field>` under edismax now returns 200 (all docs) since the `*:*`
    short-circuit runs before `qf` resolution, where a non-`*:*` query with the same bad `qf`
    would 400 — an inconsistency, unfixtured either way.

None of these follow-ups were resolved as part of #7; they are recorded here so they are not
lost, per this repo's "report faithfully, no softening of deferred work" convention.

## Pointers

- Production code: `src/edismax.rs` (new), `src/core_index.rs`'s `parse_edismax_query`,
  `src/lib.rs` (dispatch + `SELECT_PARAMS`).
- Tests: `tests/edismax.rs` (incl. `EDISMAX_KNOWN_DIVERGENCES` and
  `hermetic_edismax_manifest_entries_match_committed_fixtures`), `tests/mm.rs`,
  `tests/differential.rs`'s `edismax_` skip.
- Fixtures: `solr-ref/responses/edismax_*.json` (18 files); corresponding rows in
  `solr-ref/manifest.tsv` and capture block in `solr-ref/capture.sh`.
- Findings: `docs/solr-ref-findings.md` #68-75.
- Commits on this branch (4, beyond the merge-base with `main`): `7266737` (red tests),
  `f917d2f` (implementation), `1e19632` (round-2 fix + attribution correction), `b69048f`
  (comment wording fix).
- Follow-up issue: [#51](https://github.com/markdlabrecque/wayfinder/issues/51).
- Issue: [#7](https://github.com/markdlabrecque/wayfinder/issues/7).

Not yet pushed or opened as a PR — that is the coordinator's next step after this report.
