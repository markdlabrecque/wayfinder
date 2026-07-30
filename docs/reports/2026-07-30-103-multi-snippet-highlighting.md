# Issue #103 — hl.snippets > 1 is a structural no-op (single-fragment ceiling in highlight_field)

## Result

`CoreIndex::highlight_field` (`src/core_index.rs`) previously returned at most one
snippet per field per doc, regardless of `hl.snippets`, because Tantivy's public
`SnippetGenerator` only exposes its single best-scoring fragment
(`select_best_fragment_combination` is a private fn in `tantivy-0.26.1/src/snippet/mod.rs`).
`hl.snippets` was accepted, threaded through `src/highlight.rs`, and then silently
truncated back to one entry by the underlying primitive — a wire-contract lie caught by
issue #56's coverage work (`select.highlight.snippets` moved from covered to uncovered,
`docs/reports/2026-07-30-56-coverage-denominator.md`).

`highlight_field` is now a mask-and-resnippet loop: extract the best fragment, locate its
byte range in the source text, blank those bytes with ASCII spaces (preserving offsets and
UTF-8 validity), and re-run extraction on the masked remainder — repeated until either the
caller's `hl.snippets` cap (now threaded into `highlight_field` as a new `snippets_cap: usize`
parameter) or a defensive `MAX_SNIPPETS_PER_FIELD = 100` outer ceiling is hit. Masked spans
can never be re-matched, so no fragment repeats, and each pass retires at least one
occurrence — which is what bounds the loop. `src/highlight.rs`'s existing `.take(snippets_cap)`
stays in place as a wire-layer safety net (belt and braces, not a substitute).

`select.highlight.snippets` is expected to flip from uncovered back to covered the next time
`wayfinder coverage` is run against issue #56's contract, since the probe now genuinely
discriminates a real cap from a single-fragment ceiling.

## Review verdict

**Approved after 2 rounds** (the pipeline's max before leftovers become follow-ups; see
below — nothing was left over here, but the process should still be read as having used its
full budget).

- **Round 1: bounced**, two must-fix items:
  1. The loop was eager and uncapped regardless of the actual `hl.snippets` requested — a real
     perf regression on long `body` fields (up to 100 `SnippetGenerator` passes per field per
     doc on *every* request, not just ones asking for many snippets).
  2. The dedupe guard's `ponytail` comment framed a genuine Solr divergence (two distinct
     occurrences whose rendered fragments happen to be byte-identical collapse to one snippet
     here; Solr would return both) as a design opinion rather than declaring it as a divergence.
- **Round 2: approved.** The implementor (same agent, resumed) threaded the cap down into
  `highlight_field` (`snippets_cap: usize`, bounded by `min(snippets_cap, MAX_SNIPPETS_PER_FIELD)`,
  with two independent bound checks since a deduped pass burns an iteration without filling a
  slot), rewrote the ponytail comment as an honest four-item divergence declaration, added
  `#[allow(clippy::too_many_arguments)]` matching existing precedent (`parse_edismax_query`),
  replaced a silent `if let Some` mask-write skip with an `.expect()` (making an assumed-
  unreachable branch loud instead of silent), and added one new test
  (`highlight_field_stops_extracting_once_the_cap_is_filled`). The reviewer independently
  re-ran mutations rather than trusting the implementor's self-report — an instrumented
  out-of-tree pass-count check confirmed `hl.snippets=1` costs exactly one `SnippetGenerator`
  pass, closing the round-1 perf concern. No must-fix remained.

## Test evidence

Verified directly in the worktree before writing this report:

```
cargo fmt --check                              # clean
cargo clippy --all-targets -- -D warnings      # clean
cargo test                                     # 523 passed (27 suites), 0 failed
```

New/changed tests (`src/core_index.rs` unless noted):

- `highlight_field_extracts_up_to_cap_when_cap_is_below_match_count` (new, red-first)
- `highlight_field_returns_every_match_when_cap_exceeds_match_count` (new, red-first)
- `highlight_field_handles_adjacent_occurrences_without_duplicates_or_panics` (new,
  written green as a safety-contract test guarding against panics/duplicates on
  close-together occurrences)
- `highlight_field_stops_extracting_once_the_cap_is_filled` (added in review round 2, to
  pin the newly-threaded cap)
- `snippets_cap_is_distinguishable_from_default` (`src/coverage.rs`) — changed from
  asserting the old 1-snippet ceiling as the *desired* behavior to asserting the real
  3-snippet behavior; the assertion failure message and doc comment were rewritten from
  "update this to 3 when #103 lands" framing to describing the landed behavior.
- `semantic_covered`'s `select.highlight.snippets` comment (`src/coverage.rs`) rewritten
  from an issue-#103-pending note to a description of what actually discriminates a real
  cap from a single-fragment ceiling.

Mutation testing: the implementor reported 3 mutations exercised across the two rounds (2
killed, 1 — the dedupe guard — found unkilled and disclosed rather than hidden, see gap 4
below). The reviewer independently re-ran a pass-count mutation in round 2 (did not trust
the implementor's report) and confirmed the perf fix.

## Deliberate descopes / accepted residual gaps

These are genuine open items, not hidden debt — none were treated as must-fix by either
review round:

1. **No test pins that `src/highlight.rs` passes the real `snippets_cap`** (rather than, say,
   `usize::MAX`) down to `highlight_field`. The `.take(snippets_cap)` wire-layer safety net
   keeps response bodies byte-identical either way, so only perf/pass-count would differ if
   this regressed, and there's no cheap behavioral test for that today.
2. **The dedupe guard (`highlight_field`, ~line 1852) is an unfixture-backed, currently
   undecided Solr divergence.** Two distinct occurrences whose rendered fragments happen to
   be byte-identical (repeated boilerplate, far enough apart to fragment separately) collapse
   into one snippet here; Solr would return both. Documented in code as ponytail "gap 4."
   Remains an unkilled mutant — no test currently proves this specific case either way.
3. **No non-ASCII masking test.** The reviewer reasoned through UTF-8 safety and found it
   sound (mask boundaries always fall on real matched-term byte ranges, i.e. real char
   boundaries), but this is unpinned by a test.
4. **No multivalued-text-field highlight test.** The reviewer diffed the field-text assembly
   logic against tantivy 0.26.1's `snippet_from_doc` source and confirmed a match today, but a
   future tantivy version bump could silently drift this without a test catching it.
5. **Minor/non-blocking:** the last loop iteration masks and UTF-8-revalidates the full field
   text even on the final pass before breaking (wasted work, not a correctness issue) —
   reviewer suggested moving the cap check earlier in the loop body as a future cleanup, not
   required for approval.

Also carried over from `ponytail` gaps 1–3 in the code itself (`src/core_index.rs`, above
`highlight_field`): fragment selection/ordering is Tantivy's own greedy tie-break applied to
shrinking text, not Solr's real multi-fragment scoring, with no minimum-gap-between-fragments
notion; cost is one `SnippetGenerator` pass per snippet returned (bounded by `snippets_cap`,
so ordinary `hl.snippets=1` requests are unaffected); and `MAX_SNIPPETS_PER_FIELD = 100` is a
defensive outer ceiling independent of `snippets_cap`.

## Suggested follow-ups for future issues

- Decide belt-and-braces vs. testability for the `snippets_cap`-wiring gap (#1 above) — either
  add a pass-count-observable test or explicitly accept the gap.
- Capture a real-Solr fixture with two far-apart, byte-identical-rendering occurrences of a
  term, to settle gap 4's divergence claim and pin/kill that mutant either way.
- Add non-ASCII and multivalued-text-field highlight tests.
- Re-run `wayfinder coverage` and confirm `select.highlight.snippets` flips from uncovered
  back to covered now that #103 is closed (per issue #56's report).

## Files changed

- `src/core_index.rs` — `highlight_field` mask-and-resnippet loop, new `snippets_cap`
  parameter, new/changed tests.
- `src/highlight.rs` — threads `snippets_cap` down to `highlight_field`; `.take(snippets_cap)`
  retained as a wire-layer safety net.
- `src/coverage.rs` — `snippets_cap_is_distinguishable_from_default` test updated to assert
  real (3-snippet) behavior; `select.highlight.snippets` probe comment updated.
- `tests/search_api_coverage.rs` — minor accompanying update.

Branch: `103-multi-snippet-highlighting` (worktree `wayfinder-103-multi-snippet`), off `main`
at `bb44cc4`. Not yet committed as of this report.
