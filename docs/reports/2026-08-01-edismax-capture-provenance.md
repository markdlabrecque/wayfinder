# Issue #147 — capture edismax semantics (provenance, not new coverage)

Branch `147-capture-edismax-semantics`, PR #200, HEAD `e8f5b8f`, rebased onto
`main` at `8fc2902` (six commits). Reviewer: Opus, two rounds plus a follow-up
pass; approved.

**Up front: the coverage fraction did not move.** It stays `66/75`
(`tests/search_api_coverage.rs::EXPECTED_FRACTION`), and the
`select.q.local-params-edismax.and` coverage arm's expected value is unchanged
at `Some(2)`. What changed is that arm's *provenance* — the expectation moved
from speculative to fixture-derived. Do not read this as a coverage win.

## What landed

Two edismax facts that were previously carried as documentation-derived
inferences are now captured from real `solr:9` with `debugQuery=true`, and the
`select.q.local-params-edismax.and` coverage arm's speculative provenance is
replaced with citations to those captures, enforced by a new test file.

## Fixtures (four, `solr-ref/responses/`)

- `edismax_unquoted_multitoken.json` — `numFound=6`, ids `eA eB eC eD pA pB`.
  The count is what discriminates the phrase-vs-OR question: on the capture
  corpus the OR reading is the only one of the candidate readings that yields
  6 (a phrase reading gives 0, a must-second-clause reading gives 4, a
  single-token reading gives 0).
- `edismax_unquoted_multitoken_debug.json` — the same request with
  `debugQuery=true`; `parsedquery` is exactly one
  `DisjunctionMaxQuery(((title:quick title:rocket) | (body:quick
  body:rocket)))`. One DMQ spanning both tokens, not two — this closes the
  `_TERM_CHAR` inference (that `quick+rocket` is one unquoted clause analysing
  to two tokens, rather than two clauses) outright rather than merely
  corroborating it.
- `edismax_shape_b_debug_parsedquery.json` — `parsedquery` is
  `(+(+DisjunctionMaxQuery((title:quick | body:quick)))) +id:rocket`: the
  whitespace terminator.
- `edismax_shape_b_debug_parsedquery_paren_terminated.json` — `numFound=2`,
  `parsedquery` is `+(+DisjunctionMaxQuery((title:quick | body:quick)))`: the
  `)`-at-run-local-paren-depth-0 terminator.

Captures used `capture.sh`'s existing edismax block (container
`wayfinder-solr-7`, port 8994, core `content`, `title`/`body`, 10-doc corpus,
removed afterwards); the new block was appended at the end
(`solr-ref/capture.sh`, +85 lines). The two debug captures deliberately have
**no `manifest.tsv` row** — `debugQuery` output is not stable enough for the
differential harness to GET verbatim (Wayfinder emits no `debug` section at
all), so they live in `capture.sh` as curl comments only. Confirmed:
`solr-ref/manifest.tsv` gained exactly one row (`+1` line), for
`edismax_unquoted_multitoken` (the non-debug fixture); the two debug fixtures
and their curl commands are comment-only. That asymmetry is load-bearing for
the provenance suite and is worth flagging to anyone auditing manifest
coverage later.

## The headline: three vacuity holes in the provenance suite itself

New suite `tests/edismax_capture_provenance.rs` (504 lines) fails if the
coverage arm's comment, `src/local_params.rs`'s binding-rule doc comment, or
the relevant findings stop naming the fixtures that justify them. Building it
surfaced three ways the guard could have gone green while proving nothing:

1. **Prefix matching.** `edismax_shape_b_debug_parsedquery` is a prefix of
   `edismax_shape_b_debug_parsedquery_paren_terminated`, so a citation of the
   longer name alone would satisfy a naive substring check for the shorter
   fixture too — the two-fixture requirement was vacuous in one direction.
   Fixed with a `cites` helper (verified in the diff at
   `tests/edismax_capture_provenance.rs:119-126`) that additionally requires a
   non-`[A-Za-z0-9_]` boundary (or end-of-string) immediately after the match.
   Verified by substituting each citation across eight files, all now red.
   Note: the reviewer's first proposed fix — require a `.json` suffix after
   the match — would **not** have worked, because the citation forms actually
   in use (backtick-quoted bare name, `capture ... '...'` prose, the manifest
   row) carry no `.json` suffix; the reviewer withdrew that proposal.
2. **Absence-only pinning.** `LOCAL_PARAMS_TESTS`
   (`include_str!("local_params.rs")`) was pinned only by the *absence* of a
   phrase, so an empty `include_str!` (e.g. from a bad path or an accidental
   truncation) would have passed the check trivially. The implementor found
   this while acting on the orchestrator's arbitration of item 3 below — the
   arbitration alone would have left this hole; it took the implementor's
   follow-through to close it. Positive controls were added for it and for
   `MANIFEST`; all seven scanned constants (`CORE_INDEX`, `COVERAGE`,
   `LOCAL_PARAMS_SRC`, `LOCAL_PARAMS_TESTS`, `CAPTURE_SH`, `MANIFEST`,
   `FINDINGS`) are now mutation-tested for non-vacuity.
3. **A logically unsatisfiable blindness control.** Stage 1's control asserted
   both the presence and the absence of the same two phrases as the expiry
   tests in the same file (`f92.contains("not captured")` vs
   `!f92.contains(...)`, and the equivalent pair for `CORE_INDEX`) — a
   contradiction that could never be satisfied by any file state. The
   implementor escalated instead of silently editing a test it did not
   author, correctly per the pipeline rule. The orchestrator authorized
   deleting exactly those two contradictory assertions, conditioned on the
   non-vacuity property being preserved by the scanner controls above; a
   comment in the file (`tests/edismax_capture_provenance.rs`, around lines
   210-224) records why they were removed rather than edited.

`coverage_arm_preamble` (the helper that slices out the target coverage arm's
own comment block) first used a magic 30-line window; a follow-up pass removed
the magic number entirely — it now cuts the text before the arm at the
previous arm's `" =>"` boundary (`head.rsplit_once("\" =>")`,
`tests/edismax_capture_provenance.rs:167`) and then keeps the trailing
contiguous `//`-prefixed run, so the slice is exactly the target arm's own
preceding comment block, nothing more and nothing borrowed from a neighbour.
Its own blindness control pins both non-emptiness and exclusion of the
neighbouring probe's content. Record the bug this caught on its first run:
`split_once` leaves the arm's own indentation as a trailing whitespace-only
line in the head slice, so without `trim_end()` the trailing-comment-run
search (`rposition` looking for the last non-`//` line) finds that
whitespace-only line first and the returned slice comes back **empty** —
exactly the failure the blindness control exists to catch. This is called out
explicitly in the source comment at line 168-170.

## Limitations recorded, not papered over

**Two residual inferences remain and are filed as issue #197** (open,
confirmed: "capture: two residual edismax facts still rest on inference after
#147") rather than silently treated as settled:

- Finding 92's `-` form (part of the `autoGeneratePhraseQueries` picture) is
  still inferred, not captured.
- Finding 91's depth-independence claim (that the whitespace terminator
  applies "at any paren depth") is still inferred from `numFound` consistency
  across the seven Shape-B traces, not confirmed by a parse tree at every
  depth.

These are named explicitly as inferences in the source comments this PR adds
(`src/local_params.rs`, `src/coverage.rs`) so they cannot silently graduate
into "captured" in a future reading of the code. #197 tracks settling them.

Also for the record: a second reviewer suggestion was raised and withdrawn —
pinning the literal string `"**Documentation-derived, not captured"`, which
never existed in the pre-capture text because it wrapped mid-hyphen
(`"...not\ncaptured"` across a line break or similar); asserting its absence
would have been vacuously true. The reviewer verified this on commit
`2640d4e` (`grep -c` returns 0) and withdrew the suggestion before it was
implemented. Two of the reviewer's own suggestions being wrong, with the
implementor/orchestrator correctly pushing back both times, is itself worth
recording — it is not a case of the review pass finding nothing.

## Other facts

- `99aa4de` retires the two pre-capture staleness assertions now that their
  reason (the captures not existing yet) stopped holding, per CLAUDE.md's
  "deliberate skips must expire," rather than leaving them to rot green.
- Issue #194 (a `[[fields]]` entry named `_dynamic`/`_dynamic_text` panics
  inside tantivy) was filed off observations made during this work; confirmed
  open, unrelated to this PR's scope, not fixed here.
- Constraint honored: this report did not touch `src/core_index.rs`,
  `src/coverage.rs`'s other arms, `tests/select_fl_wildcard.rs`, or
  `tests/search_api_preset.rs` (owned by sibling branch #188); those files
  appear in the diff only as the implementor's own changes, not this
  reporting step's.

## Evidence (re-run and verified independently in this worktree, not merely
restated)

- `cargo test --no-fail-fast`: **830 tests passed, 42 suites, ~50s**.
  `cargo test` alone fail-fasts after the lib target — `--no-fail-fast` is
  required to see the full mutation kill set described above.
- `cargo fmt --check`: clean.
- `cargo clippy --all-targets -- -D warnings` (CI's exact invocation): clean.
- Coverage fraction: confirmed `66/75` via
  `tests/search_api_coverage.rs::EXPECTED_FRACTION`. Unchanged by this PR, as
  stated above.
- Reviewer: Opus, two rounds (the pipeline's default cap) plus one follow-up
  pass to close the `cites` boundary and absence-only-pinning holes above.

## Process note for the pipeline

Stage 1 did not commit its red tests separately from later work in this
wave, so "the implementor edited no test" was not independently verifiable
from git history alone — both reviewers in this wave raised it. Per
`~/.claude/projects/-Users-mark-Projects-wayfinder/memory/wayfinder-v1-pipeline-state.md`-adjacent
convention (stage 1 commits red tests separately), stage 1 should commit with
a `test:` prefix before handing off to the implementor in future waves.

## Diff summary (verified against `git diff 8fc2902..HEAD`)

13 files changed, 1571 insertions(+), 205 deletions(-): four new fixtures
under `solr-ref/responses/`, one new manifest row, an 85-line append to
`capture.sh`, a new `tests/edismax_capture_provenance.rs` (504 lines) and a
substantially rewritten `tests/edismax.rs` (+501), `src/coverage.rs` (+32,
the citation comment above the `select.q.local-params-edismax.and` arm),
`src/local_params.rs` (+29, the binding-rule provenance comment), and
`docs/solr-ref-findings.md` (+75/-... net, findings 90/91/92 updated to cite
the captures). `tests/local_params.rs` lost 154 lines — the two expired
pre-capture staleness guards plus surrounding scaffolding. No discrepancy
found between the handoff summary and the actual diff; all claims in this
report were independently re-verified (source excerpts, `gh issue view`,
fresh `cargo test`/`fmt`/`clippy` runs) rather than restated from the summary
alone.
