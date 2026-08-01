# fl=* expands to every stored field on /select and /mlt

- Issue: #188
- PR: #199
- Branch: `188-fl-wildcard`
- HEAD: `e4254a4` (rebased onto `main` at `8fc2902`)

## What was built

`fl=*` now expands to every **stored** field on `/select` and `/mlt`, composable with named
fields and `score`. A single shared predicate in `render_doc` (`src/core_index.rs`) is asked by
both loops that build the response document — the declared-`[[fields]]` loop and the
`_dynamic`/`_dynamic_text` container walk — each of which previously had its own independent
literal-name filter and neither of which understood `*`.

`src/lib.rs` needed **no change**: confirmed by `git diff 8fc2902..HEAD -- src/lib.rs` returning
empty. `split(',')` already passed `*` through untouched, and `fl` was already present in
`SELECT_PARAMS`.

Coverage moved 66/75 -> 67/75, reproduced locally: `cargo run -- coverage --format json` reports
`{"covered": 67, "uncovered": 8, "total": 75, "fraction": "67/75"}` on this branch's HEAD.

## The headline finding: score was mid-document under fl=*,score

Round-1 review found a second, independent bug that #188 made reachable for the first time:
**`score` was being inserted between the two render loops, so `fl=*,score` put `score` in the
middle of the document, where Solr puts it last.** This is not an edge case — it is the exact
request `search_api_solr` sends on every search. All 21 captured `/select` traces carry
`fl=*,score` verbatim and nothing else. `solr-ref/search-api/trace/00010.json`'s doc keys end
`..., "sm_field_keywords", "hash", "timestamp", "ss_search_api_language", "score"` — `score`
strictly after every dynamic field.

Three things about this are worth recording carefully, because they compound:

1. **Both pre-existing "score last" tests were vacuous.** `select_fl_star_plus_score_puts_score_last`
   and `mlt_fl_wildcard_plus_score_matches_the_fixture_doc_key_order` run against corpora that
   declare no dynamic fields, so the misordering was invisible to either assertion. `wildcard_app()`
   was the only test schema with a dynamic field, and nothing had queried it with `fl=*,score`
   before this change.
2. **A `ponytail:` comment asserted the opposite of the evidence.** It read: "No captured fixture
   actually discriminates score-before-dynamic-fields from score-appended-last." The fixture that
   discriminates it — `solr-ref/search-api/trace/00010.json` — is the same fixture this commit now
   cites as its ground truth. The pattern to flag: a comment claiming no evidence exists, inside a
   change whose own cited ground truth is that evidence.
3. **It reversed a placement a prior commit pinned deliberately.** `1511137 feat(schema): complete
   the v1 schema layer` added `render_doc_orders_score_after_stored_fields_and_before_dynamic_fields`
   as a characterization test for `score` between the two loops. Fixing the bug meant inverting that
   test rather than deleting it, with the history recorded in its doc comment:
   `render_doc_orders_score_after_stored_fields_and_before_dynamic_fields` ->
   `render_doc_orders_score_last_after_dynamic_fields`; expected key order
   `["id","body","score","extra_s"]` -> `["id","body","extra_s","score"]`. Confirmed in the diff.

Two new tests close the vacuity that let this ship once already:
`select_fl_star_plus_score_puts_score_after_dynamic_fields` (against `wildcard_app()`, using a
new `trace_00010_doc_keys()` helper that reads the trace's captured key order and asserts the key
immediately before `score` really is a dynamic-rule field, so the test cannot pass on a corpus
that accidentally has no dynamic fields) and
`preset_fl_star_plus_score_puts_score_last_after_every_dynamic_field` (the production-shaped
preset schema, with its own vacuity guard). Both confirmed present in `tests/select_fl_wildcard.rs`
and `tests/search_api_preset.rs`.

## The two false-positive-green probes

Both `select.fl.wildcard-plus-score` and `mlt.fl.wildcard-plus-score` in `src/coverage.rs` passed
while the implementation dropped `*` entirely, because each checked only that `score` was
present in the response — the same failure class as prior issues #162 and #167. The ticket named
only one of these; there were two.

The `/mlt` probe also needed its **request** changed, not just its predicate: it omitted
`mlt.mintf`/`mlt.mindf`, so under Solr's real strict defaults (mintf=2, mindf=5, per
`docs/solr-ref-findings.md` finding 64) the 20-doc corpus returns no similar documents at all —
`/response/docs/0` cannot exist regardless of what `fl` does. Left as-is, the item would have
stayed uncovered even after a functionally correct `fl=*` fix. This confirms the caveat #141's
implementor had already flagged.

The new predicate `renders_every_stored_field_plus_score` (`src/coverage.rs`) derives its expected
key set from the same request re-issued with `fl` absent, rather than hardcoding a field list.
**Its real limit, established empirically by review**: it pins that the two *responses* agree
with each other, not that either one matches Solr. A symmetric mutant (reversing the
declared-field iteration order, corrupting both the `fl`-present and `fl`-absent responses
identically) leaves both coverage items covered at 67/75 — the probe cannot see it — while ten
of the fixture-derived tests in `tests/select_fl_wildcard.rs`/`tests/mlt.rs` kill it. The
fixture-derived suites are the ordering oracle here; the coverage probe is not, and should not be
read as one.

`PROBE_SCHEMA` (`src/coverage.rs`) gained one stored `ss_*` dynamic field rule so the real-app
probe leg can distinguish full wildcard expansion from declared-fields-only expansion. Evidence
for landing a change to a schema all 75 coverage probes read: under a "dynamic loop stops
honouring `*`" mutant, the CLI drops to 65/75 with the new rule present, and stays at a
false-green 67/75 without it; the unmutated fraction is unchanged at 67/75 either way; a per-item
status diff across all 75 probes (mutated vs. unmutated, rule present) is byte-identical apart
from the two items the mutant is meant to break.

## Other facts

- Ticket premises corrected during the work: `render_doc` lives in `src/core_index.rs`, not
  `src/lib.rs`; there are two vacuous coverage probes, not the one the ticket named; `fl` was
  already allowlisted in `SELECT_PARAMS`; `render_doc` has two separate render loops, and the
  second (dynamic-field) loop had its own independent literal-name filter that also needed the
  wildcard predicate.
- `fl=score,*` vs. `fl=*,score` ordering equivalence is settled by **inference, not capture** — no
  fixture sends `fl=score,*`. `solr-ref/responses/select_fl_reversed.json` establishes that `fl`
  member order is not doc-key order in general (`fl=body,id` still renders `id, body` first), and
  finding 24 (`docs/solr-ref-findings.md`) establishes that Solr appends its own pseudo-fields
  last. `select_fl_score_then_star_renders_identically_to_star_then_score` asserts the two
  permutations render identically, behind a vacuity guard — without the guard the equality also
  passed on the broken (pre-fix) implementation, because both permutations rendered `{"score":
  ...}` alone with every field dropped. Recording this as a limitation: the equivalence itself is
  not independently confirmed against a real Solr response, only inferred from the two cited
  fixtures.
- #141's expiring guard `mlt_fl_wildcard_plus_score_still_drops_every_field_until_issue_188` was
  deleted now that its reason has stopped holding, and its
  `MLT_EXPECTED_DIVERGENCES` entry was removed (`MLT_EXPECTED_DIVERGENCES` is now `&[]`) rather
  than left to rot — confirmed in the diff.
  `mlt_maxntp_stays_rejected_until_issue_189_implements_it` was left untouched, and
  `SCORE_MAGNITUDE_EXEMPT` was not widened.
- Mutation testing performed and reproduced by review: the pre-#188 implementation, run as a
  mutant, is killed by 18 tests; a "half-fix" mutant that fixes only the dynamic-field loop is
  killed by 2 tests, neither of which is a coverage probe (`PROBE_SCHEMA` had no dynamic field at
  that point in the work, which is what motivated adding the `ss_*` rule); the score-misordering
  mutant is killed by exactly the 3 new/inverted tests, while every pre-existing test stays green
  against it.
- `cargo test` fail-fasts after the lib target finishes; `--no-fail-fast` is required to see the
  full kill set for a mutant. Worth recording as a process note — it hid 16 of the first mutant's
  18 kills during review's initial pass.
- Reviewed in a SHA-verified `git archive` copy, independent of the working tree: fmt clean,
  clippy clean (CI's exact invocation), 844 tests green, both mutants reproduced with the same
  kill counts, and the branch's schemas confirmed not to trip #195's duplicate-name and
  shadowed-builtin-type guards that landed on `main` since this branch started.
- This report's own verification (reporter stage) re-ran all of the above independently against
  this worktree's HEAD (`e4254a4`, matching PR #199's `headRefOid`): `cargo fmt --check` clean,
  `cargo clippy --all-targets -- -D warnings` clean ("No issues found"), `cargo test --no-fail-fast`
  844 passed, coverage CLI 67/75, and the diff for every claim above (score reordering,
  `src/lib.rs` untouched, the two vacuous-probe fixes, `PROBE_SCHEMA`'s new rule, the deleted
  `MLT_EXPECTED_DIVERGENCES` entry and expiring guard, the finding-55->64 citation fixes in
  `tests/mlt.rs`) was read directly and matches this report and the review summary handed to the
  reporter. No discrepancy found.

## Follow-ups filed

- **#196** (open): partial `fl` patterns (`fl=ss_*`) fall through the new predicate as literal
  field names and silently return a document missing those fields — a silent wrong answer where
  a 400 would be preferable. The `ponytail:` comment in `render_doc` naming this ceiling is
  legitimate (no captured trace sends a partial pattern), but the issue needs a fixture-backed
  decision between expanding the pattern and rejecting it.
- **#198** (open): systematic off-by-nine finding citations in `tests/mlt.rs`. Four sites were
  fixed in this branch (finding 55 -> 64, confirmed in the diff of `tests/mlt.rs`). Review found
  more that this branch did not touch: 54 -> 63 at five sites, plus single sites at 56 -> 65 and
  57 -> 66, and 53 -> 62. This is explicitly **not** a blanket +9 substitution —
  `tests/highlighting.rs:66,82` cite finding 55 correctly as-is — so each citation needs checking
  against what the numbered finding in `docs/solr-ref-findings.md` actually says before touching
  it.

## Process notes for the pipeline itself

- Stage 1 (test-writer) never committed its red tests as a separate commit on this branch, so
  "the implementor edited no test" was **not independently verifiable from git history** here.
  Review substituted mutation evidence (the pre-fix mutant killed by 18 tests, the half-fix mutant
  killed by 2) to demonstrate the tests were not vacuous, but this is a weaker substitute for the
  commit-boundary check the pipeline is supposed to provide. Stage 1 should commit its red tests
  separately on future issues in this repo.
- What review itself flagged as least-scrutinised, having capped out its passes: the
  `PROBE_SCHEMA` change touches an artifact all 75 coverage probes read on every run. Confidence
  there currently rests on a per-item status snapshot (mutated vs. unmutated) plus a green suite —
  which is evidence that no probe's *verdict* moved, not evidence that no probe's *evidence*
  quietly weakened while staying green. Per the reviewer's 2-round cap, this is a recoverable
  escalation, not a resolved item: **this area could use a further review pass** before treating
  the `PROBE_SCHEMA` change as fully scrutinised.

## Test evidence

- `cargo fmt --check` -- clean.
- `cargo clippy --all-targets -- -D warnings` -- clean, "No issues found" (CI's exact command).
- `cargo test --no-fail-fast` -- 844 passed, 0 failed (42 suites).
- `cargo run -- coverage --format json` -- `{"covered": 67, "uncovered": 8, "total": 75,
  "fraction": "67/75"}`.
- Mutation testing (reproduced by review, described above): pre-#188 mutant killed by 18 tests;
  dynamic-loop-only half-fix mutant killed by 2 tests; score-misordering mutant killed by exactly
  3 tests (2 new, 1 inverted), with no regression in any pre-existing test.

## Review outcome

Round-1 review returned must-fix findings (the score-ordering bug and the second vacuous probe)
rather than an outright block or a silent approval; both were fixed on this branch and
re-verified. Review capped at 2 rounds per the pipeline default and flagged the `PROBE_SCHEMA`
change as the least-scrutinised remaining area (see Process notes above) rather than declaring it
fully clean — that gap is unresolved and is not being softened here.
