# Report: Solr highlighting (`hl`/`hl.fl` and friends)

- Branch: `4-highlighting`
- Issue: [#4](https://github.com/markdlabrecque/wayfinder/issues/4) — implement Solr wire-compatible
  highlighting (`hl`, `hl.fl`, `hl.snippets`, `hl.fragsize`, `hl.simple.pre`/`hl.simple.post`).
- Pipeline: test-writer -> implementor -> reviewer (2 rounds: round 1 BOUNCE, round 2 APPROVED)
  -> reporter (this report). Five commits, in order:
  1. `ff5a048` test(highlighting): capture Solr highlighting fixtures
  2. `1fc51e4` test(highlighting): add failing highlighting integration tests
  3. `8e58deb` feat(highlight): implement hl/hl.fl highlighting (issue #4)
  4. `f698c01` test(differential): drop hl_* EXPECTED_DIVERGENCES entries
  5. `01aa9d7` fix(highlight): return 400 for invalid hl.fl fields instead of 500

## What was built

1. **9 new fixtures (`solr-ref/responses/hl_*.json`) and `manifest.tsv` rows**, captured against a
   dedicated `wayfinder-solr-4` container (port 8991, same schema/corpus as the canonical `content`
   core), then torn down after capture. `solr-ref/capture.sh` gained a self-contained appended block
   for this. Covers: basic `hl.fl`, `hl.snippets`, `hl.simple.pre`/`.post` custom markers,
   `hl.fragsize` under the unified default (no-truncation control) and under
   `hl.method=original` (truncation), a no-field-match doc, comma- and space-separated multi-field
   `hl.fl`, and default `hl.fl` (via `df`, no `hl.fl` given).

2. **Findings 52-55 in `docs/solr-ref-findings.md`**, plus a correction to the "Not yet captured"
   section (highlighting removed from the not-yet-captured list; a follow-up line added for the
   `hl.fl`-invalid-field 400 shape, see below). The findings pin: the `highlighting` envelope's
   always-present/per-doc, present/absent-per-field, never-`[]` shape (51); `hl.snippets` as a cap
   never a pad (52); `hl.fl`'s default is `df`, not `*` (53); `hl.fragsize` truncation only visibly
   observable under `hl.method=original` in this fixture set, not under Solr's real
   `hl.method=unified` default (54).

3. **`src/highlight.rs` (new module)** — Solr wire semantics for highlighting, parallel to
   `crate::facet`. Builds the `highlighting` response object: parses `hl.fl` (comma/space
   separated, `df` default), resolves `hl.snippets`/`hl.fragsize`/`hl.simple.pre`/`.post`, validates
   every named field via `check_highlightable` before touching Tantivy, and calls
   `CoreIndex::highlight_field` once per doc/field on the page. `InvalidHlField` wraps a validation
   failure (undefined or non-text field) so `select` can distinguish it from a genuine internal
   error via `downcast_ref`, the same pattern `check_sort`/`facet::check_facetable` already use.

4. **`CoreIndex::highlight_field` (`src/core_index.rs`)** — the Tantivy-facing primitive, one
   `SnippetGenerator::create`/`snippet_from_doc` call per doc/field, returning an empty `Vec` (not a
   single empty-string entry) on no term overlap.

5. **Wiring in `src/lib.rs`**: `hl`/`hl.fl`/`hl.snippets`/`hl.fragsize`/`hl.simple.pre`/
   `hl.simple.post`/`hl.method` added to `SELECT_PARAMS`; the `select` handler gates the whole block
   on `hl=true`, builds it from the already-paginated `page` (highlighting is scoped to the returned
   page, not the full hit list — unfixtured, a reasonable-but-unverified choice), and on error
   downcasts to `highlight::InvalidHlField` to choose between a 400 (with the base query's
   `response` block attached via `WfError::with_response`, issue #35's precedent) and a 500 for
   anything else.

6. **`tests/highlighting.rs`** — 9 fixture-derived integration tests (one per `hl_*` fixture) plus,
   added in the final commit, `hl_undefined_field_is_400_and_carries_the_base_querys_response_block`,
   `hl_non_text_field_is_400`, and `strict_params_accepts_every_implemented_highlight_param`.

7. **`tests/differential.rs`** — the 9 `hl_*` placeholder rows added to `EXPECTED_DIVERGENCES` in
   commit 2 (as an unbuilt-feature to-do) were all deleted in commit 4 once the feature made every
   `hl_*` fixture match; `EXPECTED_DIVERGENCES` now holds only the pre-existing `ping` entry.

## Judgment calls / documented gaps (per the compatibility contract — flagged, not papered over)

- **`hl.fragsize` is only honored under an explicit `hl.method=original`.** Solr's real default,
  `hl.method=unified`, was captured and found not to meaningfully truncate this fixture set's
  short, punctuation-free fields at any `hl.fragsize` value (finding 55) — a real `unified`
  fragmenter is out of the issue's own stated scope, so under the (unset) default `hl.fragsize` is
  silently ignored and Tantivy's own 150-char `SnippetGenerator` default applies instead. Marked
  with a `ponytail:` comment in `src/highlight.rs`.
- **`hl.snippets > 1` is honored as an upper bound only.** Tantivy's public `SnippetGenerator` API
  can never yield more than one real snippet per doc/field (`select_best_fragment_combination` is
  private in `tantivy-0.26.1`), confirmed against the crate source during review — not a
  Wayfinder-side shortcut.
- **Unfixtured, reasonable-but-unverified choices** (no captured Solr response exercises these):
  the default-`hl.fl`-via-`df` highlightability validation path when the resolved default field is
  non-text; `hl.fl=*`; empty `hl.fl=`; dynamic-field names; the `.with_response()` shape on the two
  new `hl.fl` 400 paths (inferred from `facet_unknown_field.json`'s precedent, issue #35, not from a
  captured `hl_*` error fixture); highlighting scoped to the paginated page rather than the full hit
  list.
- **A real, orthogonal BM25/ranking divergence was discovered incidentally, not filed as an
  issue.** While capturing `hl_no_field_match`, a bare `q=category:animals` term query showed
  Wayfinder ordering `doc4` before `doc1` where Solr orders `doc1` before `doc4` — a BM25/norm
  ranking difference with nothing to do with highlighting. Worked around in the fixture by using
  `q=*:*&fq=category:animals` instead, which ties every matching doc's score so the ascending-doc
  tie-break is deterministic on both engines and the fixture isolates only the highlighting fact it
  exists to pin. **This divergence is not filed as a GitHub issue and should be**, since as
  written it is only documented in a commit message and finding 52's parenthetical, not tracked
  anywhere actionable.

## Test evidence

Re-run directly by the reporter, current branch tip:

- `cargo test`: **298 passed** (14 suites), 0 failed.
- `cargo fmt --check`: clean.
- `cargo clippy --all-targets -- -D warnings`: clean ("No issues found").

## Review outcome

**Round 1: BOUNCE.** The reviewer verified most of the implementor's judgment calls as correct
rather than papered-over (the `hl.fragsize`/`hl.method` split traced to a fixture, not a guess; the
`hl.snippets` ceiling confirmed against `tantivy-0.26.1` source as a genuine public-API limit;
finding 52's three-way present/absent/absent-key logic confirmed as real behavior, not an artifact
of serde's defaults). Must-fix: an undefined or non-text `hl.fl` field surfaced as a 500
(`WfError::internal`) rather than a 400, on two reachable user-input paths — inconsistent with the
`check_sort`/`facet.field` sibling precedent already in the codebase. 5-minute items: a stale "Not
yet captured" line in `docs/solr-ref-findings.md` still listing highlighting as uncaptured, no
`strict_params` regression test for the new `hl.*` params, and a missing `ponytail:` comment
marking the `hl.fragsize`/`hl.method` ceiling.

Returned to the original implementor, who fixed all of it in `01aa9d7`: added `InvalidHlField` +
`check_highlightable` pre-validation in `src/highlight.rs`, wired the `downcast_ref` branch in
`select` to render a 400 with `.with_response()` for both paths, added the two new tests
(`hl_undefined_field_is_400_and_carries_the_base_querys_response_block`, `hl_non_text_field_is_400`)
and `strict_params_accepts_every_implemented_highlight_param`, fixed the stale doc line, and added
the `ponytail:` comment.

**Round 2: APPROVED.** Gates re-verified independently by the reviewer (298 tests, fmt/clippy
clean). Three non-blocking follow-ups noted (below), and the reviewer stated the round-2 pass did
not exhaust the 2-round cap (round 1 bounced, round 2 approved without a further bounce) — but see
the "Note" below regarding depth of coverage.

**Note per pipeline convention:** although the numeric cap was not exhausted, this feature involved
substantial new production code (a whole new module, a new Tantivy-facing primitive, and Solr wire
semantics with several unfixtured judgment calls). Given the volume of unfixtured/inferred behavior
listed above, a further review pass focused specifically on those inferred paths (rather than the
already-covered fixture-derived core) would still be worthwhile if this is revisited.

## Follow-ups noted by the reviewer (non-blocking, deferred)

1. **`error.code` is not asserted in the two new 400 tests.** Sibling tests in `tests/faceting.rs`
   for the analogous `facet.field` unknown-field 400 do assert `error.code`; the two new
   `hl_undefined_field_is_400_and_carries_the_base_querys_response_block`/`hl_non_text_field_is_400`
   tests in `tests/highlighting.rs` check `status` and `error.msg` but not `error.code`.
2. **The default-`hl.fl`-via-`df` highlightability check can now 400 on a non-text default field,
   with no fixture backing that specific behavior.** If a core's `df` resolves to a non-text field
   and `hl=true` is set with no explicit `hl.fl`, the new pre-validation will reject it — plausible,
   but unverified against a captured Solr response.
3. **`InvalidHlField` does not implement `Error::source()`.** Cosmetic; `Display` forwards
   correctly, but the wrapped `anyhow::Error` is not exposed via the standard `source()` chain.

## Follow-up worth its own issue (discovered during this work, not filed)

The BM25/ranking divergence found while capturing `hl_no_field_match` (see "Judgment calls" above):
Wayfinder orders `doc4` before `doc1` for a bare term query on the `category` field
(`q=category:animals`) where Solr orders `doc1` before `doc4`. This is unrelated to highlighting and
was worked around in the fixture, not fixed. No GitHub issue exists for it yet; recommend filing one
before this fact is lost to a commit message.

## Pointers

- Production code: `src/highlight.rs` (new module — `highlighting`, `InvalidHlField`,
  `check_highlightable`, `doc_key`), `src/core_index.rs` (`CoreIndex::highlight_field`),
  `src/lib.rs` (`SELECT_PARAMS` additions, `select`'s `highlighting_result` block and
  `downcast_ref::<highlight::InvalidHlField>` branching).
- Tests: `tests/highlighting.rs` (12 tests: 9 fixture-derived, 2 new 400-path tests, 1
  `strict_params` regression guard), `tests/differential.rs` (`EXPECTED_DIVERGENCES` — the 9
  `hl_*` placeholder rows added then removed).
- Fixtures/capture: `solr-ref/capture.sh` (appended `wayfinder-solr-4`/port-8991 block),
  `solr-ref/manifest.tsv` (9 new `hl_*` rows), `solr-ref/responses/hl_*.json` (9 fixtures).
- Docs: `docs/solr-ref-findings.md` — findings 52-55 (new), "Not yet captured" section correction
  and the added note on the unfixtured `hl.fl`-error `.with_response()` inference.
- Issue: [#4](https://github.com/markdlabrecque/wayfinder/issues/4).
