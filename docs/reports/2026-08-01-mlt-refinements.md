# Issue #141 — MLT refinements (`fq`, `mlt.match.include`/`.offset`, `json.nl`)

- Branch: `141-mlt-refinements`
- Worktree: `/Users/mark/Projects/wayfinder-141`
- Head: `e10adbf`, rebased onto `153fb13`
- Commits: `7617525` (red tests + fixtures), `6bb9bcb` (feat), `b5cbc0e` (round-2 review fix),
  `e10adbf` (round-2 review test pins)
- Findings: appended as #98-101 in `docs/solr-ref-findings.md` (cited here by number, not
  re-added; renumbered twice during the batch, see "Merge sequencing" below)
- Follow-ups filed: #188, #189 (both open)

## What was built

`fq` on `/mlt` — repeated params AND together, 400 on a malformed one, and applied to the
similar-docs search only, never to seed resolution (finding 98: `mlt_fq_seed_not_filtered.json`
excludes the seed doc's own category via `fq` and real Solr still resolves `match` to it).
`mlt.match.offset` selects the nth `q` hit as the seed and drives `match.start` to the same
value — not cosmetic; the seed document genuinely changes (finding 99,
`mlt_match_offset.json`). `mlt.match.include=false` omits the `match` key from the envelope
entirely rather than emitting an empty object (finding 100, `mlt_match_include_false.json`).
`json.nl=map` renders an empty `interestingTerms` as `{}` instead of the default `flat` shape's
`[]` (finding 101, `mlt_json_nl_map_empty_terms.json`). All four are registered on
`MLT_PARAMS` in `src/lib.rs`. Eight fixtures were captured live against `solr:9`:
`mlt_fq_scope.json`, `mlt_fq_seed_not_filtered.json`, `mlt_fq_multiple_and.json`,
`mlt_match_include_false.json`, `mlt_match_offset.json`, `mlt_json_nl_map_empty_terms.json`,
`mlt_fl_wildcard_score.json`, `mlt_maxntp_noop.json`.

## Two params cut from scope, and why

The issue named five params; two were split out into their own issues rather than implemented
or accepted-and-ignored here.

- **`fl=*,score` -> #188.** `CoreIndex::render_doc` has no wildcard support in `fl` at all —
  verified directly, not just inferred: `fl=*,score` on `/select` today returns only `score`,
  dropping every other field, even though a captured `search_api_solr` trace
  (`solr-ref/search-api/trace/00010.json`) shows real Solr returning everything. This is a
  `render_doc` gap shared with `/select`, not `/mlt`-specific, so it is not this issue's to fix.
  Two expiring guards pin the gap meanwhile:
  `mlt_fl_wildcard_plus_score_still_drops_every_field_until_issue_188` and the
  `mlt_fl_wildcard_score` entry in `MLT_EXPECTED_DIVERGENCES`.
- **`mlt.maxntp` -> #189.** Read directly from source, not inferred from the param name: Tantivy
  0.26.1's `tantivy::query::more_like_this::MoreLikeThis` struct exposes no analogue of Lucene's
  `maxNumTokensParsed`. And the param is not a safe no-op to allowlist-and-ignore the way `TZ`/
  `bf` are: confirmed live, real Solr's `mlt.maxntp=1` against `mlt11` drops the
  astronomy-cluster match count from 4 to 0 — the param demonstrably narrows results at low
  values. Allowlisting it while ignoring it would convert a loud 400 into a silent wrong
  answer, the #181 failure mode. It stays off `MLT_PARAMS`, so `strict_params = true` keeps
  400ing it, guarded by the expiring
  `mlt_maxntp_stays_rejected_until_issue_189_implements_it`.

Both cuts left expiring guards rather than silence: each fails the moment its param is
allowlisted, rather than rotting into a permanently green assumption.

## Review outcome — two rounds, and the interesting finding is about the guard, not the feature

**Round 1** bounced on two items. First, a third surviving mutant on the `fq` validation path:
wrapping the parse loop in `if !hits.is_empty()` passed all 777 tests, because the only test
exercised a `q` that resolves. Second, and more subtly, the `MLT_EXPECTED_DIVERGENCES` entry for
`mlt_fl_wildcard_score` would not expire when #188 lands — the exact rot its own doc comment
claimed to prevent. Simulating the #188 fix turned the guard test red as designed, but the
manifest loop stayed green, because the residual diffs were BM25 score magnitudes, a PRD-
ratified divergence #188 can never fix. The guard's failure message compounded this by
instructing the #188 implementor to drop the entry, which would have turned the suite red on
score magnitude instead.

**Round 2** fixed both. The score-blanking branch became a `SCORE_MAGNITUDE_EXEMPT` set
covering both rows that request `score` in `fl`, so the entry now expires on the wildcard gap
alone; the failure message was rewritten to an instruction that works verbatim. The `fq` test
gained a no-seed case.

**Round 2 review** returned CONFIRMED, and measured the exemption's cost rather than arguing it:
with #188 simulated and the row removed from the exempt set, the loop fails with exactly seven
diffs, all score magnitudes, no non-score diff — so the exemption is necessary and hides
nothing but the ratified divergence. It also followed the rewritten failure message literally
and got a 30/30 green suite. It found one further non-blocking survivor (gating the `fq` parse
on the *presence* of `q` rather than the hit count), which was then folded in along with a
fifth the implementor went looking for on its own: error-swallowing on a non-first `fq`,
invisible because the existing multi-`fq` test sends two valid filters.

Per CLAUDE.md's default two-round cap: this review used both rounds and closed everything
raised in round 1. The cap was reached, not exhausted with anything outstanding — but per the
pipeline's own rule, two rounds is the default cap, not evidence the work has had all the
review it could use.

## Merge sequencing, worth recording

This was the last of a ten-ticket batch and the third branch to contend on the same two files.
Finding numbers collided three ways — all of #154, #140, and #141 initially claimed 96 — and
the branch was renumbered twice, landing on 98-101. The overall coverage fraction moved four
times during the batch, and `tests/search_api_coverage.rs` carries three hard-coded pins plus
prose figures plus changelog comments that must all move together. The changelog now records
each landed step rather than overwriting: `62/75 (#139) -> 63/75 (#154) -> 64/75 (#140) -> 66/75
(#141)`.

## Evidence

Re-run on `e10adbf`, rebased onto `153fb13`:

- `cargo fmt --check` — clean.
- `cargo clippy --all-targets -- -D warnings` — clean.
- `cargo test` — 812 passed, 41 suites, 0 failed.
- `cargo test --test differential` — 27 passed.
- `cargo run -- coverage --format json` — **66/75** overall, with `mlt.filters` and
  `mlt.match-include-and-offset` flipped to covered.
- Mutation evidence was confirmed to survive the rebase by diffing `mlt()` pre- and post-rebase
  (`e42bb9a` -> `e10adbf`): three hunks, all comment-only (finding-number renumbering).

## Follow-ups

- **#188** — `fl=*,score` wildcard support belongs in `render_doc`, shared by `/select` and
  `/mlt`. Additionally, the existing `mlt.fl.wildcard-plus-score` coverage probe
  (`src/coverage.rs`) asserts only that `score` is present, so it could not see the wildcard bug
  even once results exist — it reads uncovered today only because that query's
  `mlt.mintf`/`mlt.mindf` defaults return no similar docs. Tightening that probe's assertion is
  #188's to do alongside the fix.
- **#189** — `mlt.maxntp` has no Tantivy `MoreLikeThis` equivalent and stays off `MLT_PARAMS`,
  400ing under `strict_params` until it is implemented or explicitly descoped further.

## Bottom line

`/mlt` now honours `fq` (similar-docs only, never the seed), `mlt.match.offset` (changes which
document seeds the query), `mlt.match.include=false` (drops `match` entirely), and `json.nl`
(shapes empty `interestingTerms`), all four registered on `MLT_PARAMS` and pinned by eight live-
captured fixtures. `fl=*,score` and `mlt.maxntp` were deliberately cut to #188 and #189 rather
than implemented against a wrong premise or silently ignored. Review ran both rounds of the
default cap; round 1's most consequential finding was not in the feature code but in a
divergence-tracking guard that would not have expired when its own follow-up landed — fixed by
scoping the score-magnitude exemption narrowly and confirmed by simulating the #188 fix.
Coverage moved 64/75 -> 66/75; all local gates are green (812/41 tests, 27/27 differential, fmt
and clippy clean).
