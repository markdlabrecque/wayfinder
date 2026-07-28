# Report: Solr error shapes (issue #11)

- Branch: `error-shapes` (worktree off `main`)
- Issue: #11 — every error path produces Solr's envelope (`docs/solr-ref-findings.md` finding 10)
- Spec: orchestrator task spec (scratchpad, not committed)

## Pipeline note

The pipeline ran as a single agent doing the stages in order (red tests → implementation → review →
report) rather than four delegated agents: the executing agent was a fork, and forks are barred from
spawning subagents. TDD order was preserved and the red-before-green evidence is below, but the
independent-reviewer property was not.

**Resolved:** the orchestrator subsequently dispatched a fresh independent `reviewer` agent against
commit `69f2627`. Outcome: **approved, mergeable as-is**, round 1 of 1 (the 2-round cap was not hit,
so this work has had the full permitted review depth). The reviewer independently re-ran
`cargo test` (24/24), `cargo fmt --check`, and `cargo clippy --all-targets -- -D warnings` (all
clean), and specifically verified:

- `git diff main --name-status -- solr-ref/responses/` shows adds only — the 25 pre-existing fixtures
  were **not** silently re-captured, which was the highest-severity risk on this branch.
- Scope discipline held: the diff against `main` for `src/schema.rs`, `src/main.rs`,
  `tests/common/mod.rs`, `src/params.rs`, `src/core_index.rs` is empty (siblings own those files).
- No pre-existing test was edited or weakened; `tests/error_shapes.rs` is the only test file touched.
- The `any()` route change does not make `/update` permissive: `check_update_method` still 400s a
  non-POST *before* body parsing, so `GET /update` errors rather than tripping over an empty body.
- Every error path routes through `WfError`; no hand-rolled error responses remain.
- The three-variant `Envelope` maps 1:1 to three fixture-backed shapes — not speculative machinery.

Non-blocking follow-ups the reviewer confirmed are safe to defer are listed in the follow-ups section
below; it added that `manifest-errors.tsv` is not yet wired into #1's differential harness, and that
`tests/error_shapes.rs`'s local `request()` helper (kept local deliberately to avoid colliding with
the concurrent #1 branch) should be consolidated into `tests/common/mod.rs` post-merge.

## What was built

`src/error.rs` (new, 95 lines) — one `WfError` type + `IntoResponse`, the only thing that builds an
error body now. Carries status, a Wayfinder-honest `class` string, the message, the echoed params, and
an `Envelope` variant selecting between Solr's three captured error shapes (finding 13):
`WithParams` (`/select`), `NoParams` (`/update`), `Bare` (unsupported method).

`src/lib.rs` — all three handlers now return `Result<Response, WfError>` and use `?`; the hand-rolled
`error_response` helper is gone. Also:
- routes registered with `any` instead of `get`/`post` (finding 14);
- `check_update_method` rejects non-POST with the bare envelope;
- `check_sort` validates `sort` fields (unknown field, or field without `fast`) and 400s per finding
  11 — ordering itself is still #2's job;
- missing `q` now matches nothing instead of defaulting to `*:*` (finding 12).

## Fixtures captured (Docker was available)

`solr-ref/capture.sh` grew an appended block, plus a `capx` helper for non-GET / non-core-relative
requests, indexed in the new `solr-ref/manifest-errors.tsv`:

| Fixture | Status | What it establishes |
|---|---|---|
| `err_missing_q.json` | 200 | missing `q` → empty result set, not `*:*` |
| `err_update_bad_json.json` | 400 | `/update` errors carry no `params` echo |
| `err_update_put.json` | 400 | unsupported method → no `responseHeader` at all |
| `err_select_delete.json` | 200 | `/select` is method-agnostic |
| `err_missing_core.json` | 404 | unknown core → HTML easter egg, not JSON |

`err_missing_q` is a plain core-relative GET so it went into `manifest.tsv` and the #1 harness will
pick it up for free. The existing 25 fixtures were left byte-identical (captured incrementally) to
keep QTime churn out of the diff.

## Comparison contract implemented

`error.code` and HTTP status match the fixture exactly; `responseHeader.status` mirrors them;
`error.metadata` matches Solr's flat alternating array *shape* with `error-class` /
`root-error-class` keys and Wayfinder-honest values (`wayfinder::Error`, `wayfinder::SyntaxError`, …)
rather than faked Java class names; `error.msg` is asserted non-empty and never compared verbatim.

## Test evidence

12 new tests in `tests/error_shapes.rs`, all deriving expectations from fixtures. Confirmed red
first — 10 of 12 failed for the right reasons (missing `metadata`, `params` present on `/update`
errors, no `sort` validation, `q` defaulting to `*:*`, DELETE /select 405'ing); the 2 that passed
immediately are regression guards (unknown-core 404, unknown params ignored).

Final: `cargo test` — **24 passed, 0 failed** (12 new + the 12 pre-existing tracer-bullet tests,
which needed no edits). `cargo fmt --check` clean. `cargo clippy --all-targets -- -D warnings`
**clean** — and that closes tracer-bullet follow-up 4: both known lints (`result_large_err` on
`check_core`, `collapsible_if` in the commit handling) disappeared with the refactor.

## Deliberate divergence (needs ratification)

**Unknown core:** Solr serves a 404 HTML page; Wayfinder serves 404 with its JSON error envelope.
Status matches, body does not. Justification is that clients parse JSON; recorded as finding 15 and
flagged for the PRD rather than decided unilaterally.

## Follow-ups

1. A valid `sort` is validated then **ignored** — results come back unsorted. Correct per this
   issue's scope, but it is a live divergence until #2 lands; #2 should drop `check_sort` into the
   real sort path rather than leaving two validators.
2. `GET /update` was not captured. Wayfinder currently 400s it as "unsupported method"; Solr
   historically accepts it. Capture and match, or document as out of scope.
3. Unknown core on `/update` uses the `NoParams` envelope by analogy with other `/update` errors —
   uncaptured, since the capture only exercised an unknown core on `/select`.
4. `/admin/ping` is now method-agnostic by analogy with `/select`, also uncaptured.
5. `tests/error_shapes.rs` carries its own `request()` helper (POST/PUT/DELETE) instead of extending
   `tests/common/mod.rs`, to avoid conflicting with the in-flight #1 branch. Consolidate once both
   have merged.
6. This branch adds findings 12–15 to `docs/solr-ref-findings.md` and a second manifest file; #1's
   harness loader should be pointed at `manifest-errors.tsv` too, so the non-GET error cases are
   covered by the differential run rather than only by these unit tests.

## Pointers

- New: `src/error.rs`, `tests/error_shapes.rs`, `solr-ref/manifest-errors.tsv`,
  `solr-ref/responses/err_{missing_q,missing_core,update_bad_json,update_put,select_delete}.json`
- Changed: `src/lib.rs`, `solr-ref/capture.sh`, `solr-ref/manifest.tsv`, `docs/solr-ref-findings.md`
- Untouched, as the spec required: `src/schema.rs` (#10), `src/params.rs` semantics, `src/main.rs`
  (#12), `tests/common/mod.rs` (#1)
