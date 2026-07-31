# Issue #104 — `hl.fragsize=0` means whole field, not "unset"

## Result

`select.highlight.fragsize`'s probe could not distinguish `hl.fragsize=0` from any other
value, because no fixture used a field long enough for "whole field" and "fragmented" to look
different (`common::corpus()`'s `doc1.body` is four words). A dedicated Solr 9 capture
(container `wayfinder-solr-104`, port 8995, core `fragsize104`, one ~310-char `text_en` `body`
document) settled it: real Solr's `hl.fragsize=0` returns the entire field, unfragmented, as a
single snippet — for **both** `hl.method=unified` (default) and `hl.method=original` (finding
81, `docs/solr-ref-findings.md`). Wayfinder previously did not do this: it filtered an explicit
`0` out as if absent, silently falling back to `DEFAULT_FRAGSIZE` (100) under
`hl.method=original` or Tantivy's own 150-char `SnippetGenerator` default otherwise — both of
which truncate a field this long.

`src/highlight.rs` now special-cases an explicit `hl.fragsize=0` ahead of the existing
`hl.method=original`-vs-everything-else split (finding 54), mapping it to a new
`core_index::WHOLE_FIELD_MAX_CHARS` (`usize::MAX`) sentinel so `SnippetGenerator` never
fragments, regardless of `hl.method`. A second-order bug surfaced fixing this: Tantivy's
fragment stops at the last token's `offset_to`, dropping trailing non-token text (the field's
final "."), which real Solr keeps. `src/core_index.rs`'s new whole-field branch in
`highlight_field` re-seats the fragment's HTML inside the untouched head/tail text, encoded with
a local `encode_minimal` reimplementation of `Snippet::to_html`'s internal HTML-escaping (its
5-entity table verified byte-identical to `htmlescape::encode_minimal`'s source by the
reviewer).

`select.highlight.fragsize`'s coverage probe (`src/coverage.rs`) was sharpened from a
presence-only check to a real assertion of the whole-field text, seeded via a new
`HL_FRAGSIZE_PROBE_DOCS` doc (mirroring the `HL_SNIPPETS_PROBE_DOCS` precedent from issue
#103). This flipped the probe to failing pre-fix and back to passing post-fix; the entry was
already scored covered pre-#104 via the weak presence check, so `tests/search_api_coverage.rs`'s
`expected_uncovered` fraction is unchanged at 42/75 — this closes a probe-sharpening gap, not a
new-coverage gap.

`hl_fragsize_small_truncated.json` (`hl.method=original&hl.fragsize=40`, captured in the same
block as a documentation/contrast fixture) became a normal compatibility row in issue #51:
Wayfinder's built-in `text_en` now removes the English stopword "the" before stemming, so its
fragment-boundary token stream and output match Solr's. #51 removed the temporary
`ACCEPTED_DIVERGENCES` waiver and restored the ordinary differential assertion.

## Review verdict

**Approved after 2 rounds** (the pipeline's max before leftovers become follow-ups — this
work used its full budget and the report should be read that way).

- **Round 1: bounced**, two must-fix items:
  1. `hl_fragsize_small_truncated`'s `ACCEPTED_DIVERGENCES` arm inherited a full-envelope waiver
     pattern meant for status-level divergences, when this row is actually a matching JSON
     envelope except one string — a future regression elsewhere in that row's response could
     have passed silently.
  2. `docs/solr-ref-findings.md` finding 81 overstated its evidence, claiming "exactly one
     snippet regardless of `hl.snippets`" was fixture-pinned when no capture in this block
     actually sent `hl.snippets` — it's a correct inference, not a captured fact.
  Also flagged non-blocking: dead/misleadingly-commented code in `src/core_index.rs`
  (unreachable head-handling under the sentinel) and `src/highlight.rs` (a now-dead
  `.filter(|&n| n > 0)`).
- **Round 2: approved.** The implementor (same agent, resumed) tightened the divergence arm to
  the scoped diff-based waiver described above; corrected the finding-81 overclaim in both
  `docs/solr-ref-findings.md` and the mirroring `ponytail:` comment in `src/core_index.rs`;
  folded in both non-blocking follow-ups (explanatory comment kept instead of deleted for the
  head-handling code, since a future Tantivy version could produce a non-zero `base`; the dead
  filter simplified). Full gate re-run green. The reviewer independently re-verified the
  tightened diff arm both ways — confirmed it's satisfiable as landed, and (going further than
  the implementor's own mutation test) injected a genuine response-side regression to confirm
  the new assertion actually catches an envelope-level regression on that row, not just a
  `WAIVED_PATH` typo. Two tiny non-blocking doc-wording follow-ups remained (a leftover
  botched-edit sentence fragment in finding 81, and a self-contradiction in the
  `src/core_index.rs` head-handling comment claiming `base` is always 0 while also speculating
  about cases where it wouldn't be) — **both fixed directly by the orchestrator after round-2
  approval** (pure prose/comment corrections, no logic touched; `cargo fmt --check` and
  `cargo build --tests` re-verified clean afterward, no re-review sought).

The reviewer independently verified `encode_minimal`'s entity table against `htmlescape` 0.3.1's
actual source, constructed a synthetic multi-hit test case to confirm the whole-field branch
highlights *every* occurrence in a multi-match field (not just the first), ran the full gate
independently, and mutation-tested the trailing-reseat logic and the then-applicable divergence
guard. Issue #51 later removed that analyzer divergence.

## Test evidence

Independently re-run in this worktree before writing this report (not transcribed from the
pipeline history):

```
cargo fmt --check                              # clean
cargo clippy --all-targets -- -D warnings      # clean ("No issues found")
cargo test                                     # 526 passed (27 suites, 32.90s), 0 failed
```

New/changed tests:

- `tests/highlighting.rs`: `hl_fragsize_zero_whole_field_matches_fixture`,
  `hl_fragsize_zero_whole_field_method_original_matches_fixture` (new, fixture-backed, against
  an isolated single-doc `long_field_app`, not the shared 5-doc `common::corpus()`).
- `src/coverage.rs`: `fragsize_zero_returns_whole_field_not_a_fragment` (new); the
  `"select.highlight.fragsize"` probe arm rewritten from a presence check to a whole-field-text
  assertion, seeded via new `HL_FRAGSIZE_PROBE_DOCS`.
- `tests/differential.rs`: `fragsize_app()`/`FRAGSIZE_SCHEMA_TOML` (new hermetic app, mirroring
  the `version99_app()` precedent) so the 3 new `manifest-errors.tsv` rows for the `fragsize104`
  core run against a matching Wayfinder app rather than falling through to `content_app`; the
  `hl_fragsize_small_truncated` `ACCEPTED_DIVERGENCES` arm with its self-expiring
  stopword-asymmetry guard and scoped `WAIVED_PATH` diff.
- `tests/search_api_coverage.rs`: fraction assertion comment updated to record why 42/75 is
  unchanged (already covered pre-#104 via the weak presence check).

Mutation testing: reviewer confirmed the tightened `WAIVED_PATH` diff arm both (a) does not
false-fail on the landed response, and (b) does catch an injected response-side regression on
that row — going beyond the implementor's own self-reported check.

## New fixtures / capture

Not run via a full `capture.sh` invocation, deliberately, to avoid churning the ~190+
already-committed tracked fixtures (re-running `capture.sh` rewrites all of them and
`QTime`/`_version_`/`rid` churn would dirty every other branch's diff). Instead: a dedicated
Solr 9 container/core/port, one document indexed, 3 responses captured and committed:

- `solr-ref/responses/hl_fragsize_zero_whole_field.json`
- `solr-ref/responses/hl_fragsize_zero_whole_field_method_original.json`
- `solr-ref/responses/hl_fragsize_small_truncated.json` (documentation/contrast fixture only,
  not wired into a bespoke cargo test beyond the differential-harness row above)

3 rows appended to `solr-ref/manifest-errors.tsv`; a documented, reproducible capture block
appended to the end of `solr-ref/capture.sh` (own container/port/core/wait-loop/`capf()`
helper, mirroring the issue-#99 `_version_` block's style). New finding 81 added to
`docs/solr-ref-findings.md`.

## Deliberate descopes / accepted residual gaps

- `hl_fragsize_small_truncated.json` was temporarily classified as an
  `ACCEPTED_DIVERGENCES` entry for the old `text_en` stopword mismatch. Issue #51 removed that
  mismatch and restored ordinary fixture comparison.
- Wayfinder returns exactly one snippet for `hl.fragsize=0` regardless of `hl.snippets` — an
  **inference, not a captured fact** (finding 81 states this explicitly): none of the three
  fixtures in this capture sent `hl.snippets`, so real Solr's answer to
  `hl.fragsize=0&hl.snippets=3` is uncaptured.
- No test pins the multi-occurrence whole-field case. The reviewer verified it manually by
  constructing a synthetic multi-hit case, but nothing in the committed suite would catch a
  regression there.

## Suggested follow-ups for future issues

- **HTML-escaping divergence, undocumented and unfixtured**: Wayfinder HTML-escapes highlight
  output via `Snippet::to_html`/`encode_minimal`, while Solr's default `hl.encoder` is unset (no
  escaping). A field containing `&`/`<` diverges from Solr today. The new whole-field path
  returns more field content per request than before, making this more likely to surface in
  practice. Flagged by the reviewer in both rounds; needs its own ticket.
- Add a fixture/test pinning the multi-occurrence whole-field highlight case (verified correct
  manually during review, not covered by the suite).
- Capture `hl.fragsize=0&hl.snippets=3` (or similar) against real Solr to confirm or correct the
  "exactly one snippet" inference in finding 81.
- Per the pipeline's 2-round cap, this work used its full review budget; a third pass could
  still find something, particularly around the two areas above that the reviewer flagged but
  did not block on.

## Files changed

- `src/highlight.rs` — explicit `hl.fragsize=0` short-circuits ahead of the `hl.method` branch,
  mapping to `WHOLE_FIELD_MAX_CHARS`.
- `src/core_index.rs` — new `WHOLE_FIELD_MAX_CHARS` sentinel, `encode_minimal` HTML-entity
  reimplementation, whole-field branch in `highlight_field` re-seating head/tail text around
  Tantivy's token-bounded fragment.
- `src/coverage.rs` — `select.highlight.fragsize` probe sharpened to a whole-field-text
  assertion; new `HL_FRAGSIZE_PROBE_DOCS`; new `fragsize_zero_returns_whole_field_not_a_fragment`
  test.
- `tests/highlighting.rs` — 2 new fixture-backed tests against an isolated single-doc app.
- `tests/differential.rs` — new `fragsize_app()`/`FRAGSIZE_SCHEMA_TOML`; `hl_fragsize_small_truncated`
  accepted-divergence arm with self-expiring guard and scoped `WAIVED_PATH` diff.
- `tests/search_api_coverage.rs` — fraction-unchanged comment.
- `docs/solr-ref-findings.md` — new finding 81.
- `solr-ref/capture.sh` — new capture block appended at the end.
- `solr-ref/manifest-errors.tsv` — 3 new rows.
- `solr-ref/responses/hl_fragsize_zero_whole_field.json`,
  `hl_fragsize_zero_whole_field_method_original.json`, `hl_fragsize_small_truncated.json` — new,
  untracked fixtures.

Branch: `104-fragsize-whole-field` (worktree `wayfinder-104-fragsize-fixture`), off `main` at
`ca6ce84`. Not yet committed as of this report.
