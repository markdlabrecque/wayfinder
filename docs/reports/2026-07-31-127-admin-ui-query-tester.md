# Issue #127 — v2.5: query tester — form over /select, rendering the JSON response

Worktree: `/Users/mark/Projects/wayfinder-127-query-tester`
Branch: `127-admin-ui-query-tester`

Delivers the second page of PRD §5 "v2.5 — Admin web UI," following the
tracer bullet in #94: `GET /ui/query`, a form for `q`/`fq`/`fl`/`rows`/
`start`/`facet.field` submitted against the core's own `/select` handler,
rendering that endpoint's real JSON response on the page.

## What was built

- **`src/lib.rs`**: new `GET /ui/query` route + `query_ui` handler. On a
  non-empty query string, `query_ui` calls `select()` directly — the same
  function `/solr/{core}/select` routes to, same `SELECT_PARAMS`/
  `strict_params`/`check_sort` path — so there is no second query-parsing or
  execution path to drift from the wire API. The handler's own HTTP status is
  `select`'s status, not a fixed 200, so a malformed query surfaces the real
  `/select` error (status and JSON body) verbatim, with the form still
  rendered underneath so the operator can correct and resubmit. A new
  `submitted_query()` helper strips `key=` (explicit-empty) params before
  forwarding to `/select`, because an untouched `fq` box on a GET form
  submission would otherwise arrive as `fq=` — a blank filter query that
  400s — everything else forwards percent-encoded and verbatim.
- **`src/admin_ui.rs`**: `QueryForm`/`QueryPage` askama structs,
  `render_query_page()`, and `html_safe_json()`. `render_query_page` renders
  the form (echoing back whatever was submitted, including after an error)
  and, when a query has run, the `/select` response body. `html_safe_json()`
  pretty-prints the JSON and neutralizes only the three characters that could
  break out of the page's `<pre>` context (`&`, `<`, `>`) via their `\uXXXX`
  JSON escapes — legal JSON, parses back to the identical value — rather than
  HTML-escaping, which would corrupt the JSON text itself. No field is
  dropped, renamed, or summarized: what reaches the page is `/select`'s wire
  response, `QTime` included.
- **`templates/query.html`** (new): the form plus a conditional result block
  (`has_result`), matching `#94`'s askama/no-JS-build-step convention.
- **`tests/admin_ui_query_tester.rs`** (new, 6 tests, written red-first by the
  test-writer stage): first-load empty state (form renders, no query
  executed — no `responseHeader`/`numFound` in the body); submission renders
  the real `/select` response for `q`/`rows` (checked via structural JSON
  equality against a direct `/select` call, order-insensitive, via
  `common::normalize_envelope` applied symmetrically to both sides — see
  below); `fq`/`fl`/`rows`/`start` combined; `facet.field` results
  (`facet_counts.facet_fields`); error-path fidelity (a `sort=body desc` 400,
  the same fixture-backed case `tests/error_shapes.rs`/`tests/sort.rs` use,
  surfaces `/select`'s real status and message verbatim, not a UI-only
  validation message); and a read-only guarantee (`numFound` unchanged after
  a tester query).

## Review cycle (2 rounds)

**Round 1 — bounced, one must-fix, two quick-fixes.**

- **Must-fix**: `html_safe_json()` was stripping `responseHeader.QTime` from
  the rendered page. This existed only to satisfy an *asymmetric* test
  comparison — the test normalized the expected/`/select` side but not the
  rendered/candidate side, so the implementation had been built to make that
  comparison pass by matching production output to the test rather than the
  other way around. That is a real divergence from "renders the raw JSON
  response" and exactly the "widen the normalizer/production code to hide a
  divergence" move this repo's CLAUDE.md forbids.
- **Quick-fix 1**: no regression test for reflected XSS through the form's
  *attribute* context. Issue #94's escaping precedent only covered a text
  context (the core name); the query tester echoes submitted values into
  `value="..."` attributes, a different injection vector (a `"` breaking out
  of the attribute).
- **Quick-fix 2**: the existing markup-neutralization unit test used a
  non-representative payload rather than a breakout-shaped one for the
  `<pre>` context the JSON actually renders inside.
- **Follow-ups noted, non-blocking** (see below): the `fq=` vs
  valueless-`fq` inconsistency in `submitted_query()`; no way to express an
  intentionally-empty `q=`/`fl=` through the form; repeated-param/checkbox
  redisplay not fully round-tripping.

**Round 2 (same implementor session) — approved.** The orchestrator
authorized editing the test in this instance because it was a coupled
production+test normalization bug, not a disputed assertion: removed the
`QTime`-stripping from `html_safe_json()` so it renders the true wire
response verbatim, and made `contains_embedded_json_matching()` in the test
apply `common::normalize_envelope` to the *candidate* side too, symmetric
with the expected side (this also fixed a latent incompleteness in the old
asymmetric check — it hadn't stripped `_version_`/`_root_` on the candidate
side either, which just hadn't mattered yet given today's test schema). Added
`html_safe_json_drops_nothing_from_the_envelope` as the permanent unit-level
guard against reintroducing the stripping bug, now that the integration
suite is deliberately neutral on those fields. Added both requested security
tests (`query_page_escapes_submitted_values_in_the_form_attribute_context`,
and a rewritten `html_safe_json_neutralises_markup_without_changing_the_
parsed_value` using a `</pre><script>...</script>&amp;` breakout payload).
Documented the three non-blocking follow-ups in code comments
(`submitted_query()`'s doc comment) rather than fixing them, keeping them in
scope for a future issue rather than silently expanding #127.

**Round 2 review (Opus, final) — independently re-read the diff**, not just
re-ran gates: confirmed the normalization fix is genuinely symmetric and
applied at the right layer (the integration test is now neutral/correct on
`QTime`/`_version_`/`_root_` rather than either over- or under-constraining
production code; the new unit test, not the integration suite, is what
actually guards the stripping bug going forward). Verified both new security
tests assert real positive-and-negative conditions (payload absent
unescaped, payload present escaped) rather than just presence. Confirmed the
unconditional escape chain in `html_safe_json` covers all response fields,
including `responseHeader.params` (an attacker-controlled echo of whatever
was submitted). **Approved**, with one non-blocking gap carried into this
report (see Follow-ups) plus round 1's three.

Both review rounds re-ran `cargo fmt --check`, `cargo clippy --all-targets
-- -D warnings`, `cargo test`, and `cargo test --test
admin_ui_query_tester` themselves rather than trusting the implementor's
claims, per this repo's reviewer convention.

## Test evidence

Re-run directly by this reporter against the current tree, not copied from
an earlier stage's claim:

```
cargo fmt --check                                         -> clean (exit 0)
cargo clippy --all-targets --quiet -- -D warnings          -> No issues found (exit 0)
cargo test --quiet                                         -> 560 passed (28 suites, 37.54s)
cargo test --quiet --test admin_ui_query_tester            -> 6 passed (1 suite, 0.35s)
```

The 6 tests in `tests/admin_ui_query_tester.rs`:
`query_tester_first_load_renders_the_form_without_executing_a_query`,
`query_tester_submission_renders_the_real_select_response`,
`query_tester_submission_respects_fl_fq_rows_start`,
`query_tester_submission_renders_facet_field_results`,
`query_tester_surfaces_the_same_400_error_select_returns`,
`query_tester_is_read_only_and_does_not_mutate_the_index`.

`src/admin_ui.rs` also gained unit tests covering the query-page path
specifically: `query_page_first_load_has_the_form_and_no_result_block`,
`query_page_echoes_the_submitted_values_back_into_the_form`,
`query_page_renders_the_status_and_the_json_body`,
`html_safe_json_drops_nothing_from_the_envelope`,
`html_safe_json_neutralises_markup_without_changing_the_parsed_value`,
`query_page_escapes_submitted_values_in_the_form_attribute_context`,
`html_safe_json_passes_through_a_non_json_body_escaped` — these are counted
within the 560-passed total above, not a separate suite.

## Mutation evidence

Per the pipeline summary handed to this reporter, the round-2 implementor
mutation-tested 4 ways and reverted each after confirming the corresponding
test caught it:

- Reintroduce `QTime`-stripping in `html_safe_json()` — caught by
  `html_safe_json_drops_nothing_from_the_envelope`.
- Drop `numFound` from the rendered output — caught by
  `query_tester_submission_renders_the_real_select_response`.
- Delete one of the three escape-chain replacements
  (`&`/`<`/`>` -> `\uXXXX`) in `html_safe_json` — caught by
  `html_safe_json_neutralises_markup_without_changing_the_parsed_value`.
- Mark a template field `|safe` where it shouldn't be (bypassing askama's
  auto-escaping) — caught by
  `query_page_escapes_submitted_values_in_the_form_attribute_context`.

This reporter did not independently re-apply these mutations (unlike the
round-2 reviewer, who re-applied 3 of them itself per the summary handed
over); this evidence is carried forward as reported by the pipeline, not
independently re-verified beyond the four gates re-run above.

## Review outcome

**Approved after 2 rounds** — the maximum this repo's pipeline allows before
a leftover must escalate to the orchestrator rather than consume a third
review pass. No item escalated past round 2 here; everything round 1 raised
was closed in round 2 and independently confirmed. Per the "max 2 rounds"
convention, this report notes explicitly: this work has had exactly the
pipeline's maximum review capacity, not more, and the gap the round-2
reviewer flagged as non-blocking (below) is a legitimate candidate for a
further pass if one becomes available before this is built on further.

## Follow-ups (all deferred, none fixed in this PR)

1. **No end-to-end test that `QTime` (or any wire field) survives through
   `query_ui` -> `render_query_page` -> rendered HTML.** Only
   `html_safe_json` is unit-tested directly for field-preservation
   (`html_safe_json_drops_nothing_from_the_envelope`); the integration suite
   deliberately normalizes `QTime` away on both sides, so it cannot catch a
   regression at the handler-composition level. Low risk today, since
   `render_query_page` delegates to `html_safe_json` in one line with no
   transformation in between — but flagged by the round-2 reviewer as
   something that would matter the moment a transformation is inserted
   between `select()`'s response and the render call.
2. **`submitted_query()`'s `fq=` vs valueless-`fq` inconsistency**: the
   empty-value-drop rule is written against the raw query-string segment,
   not the decoded value, so a bare `&fq` (no `=`) survives even though
   `Params` decodes it identically to `fq=` (which is dropped). Documented
   in the function's own doc comment as a deliberate, accepted asymmetry
   for a form UI (unreachable from an actual browser submission) rather than
   fixed.
3. **An intentionally-empty `q=` or `fl=` cannot be expressed through the
   form.** `/select` distinguishes an absent param from an explicitly empty
   one; the tester's empty-value-drop logic (needed so an untouched form box
   doesn't 400 the query) collapses both cases to "absent."
4. **Repeated-param and checkbox redisplay does not fully round-trip** —
   noted by round 1, not independently re-verified by this reporter, carried
   forward as-is.

Whoever picks up the next v2.5 milestone (schema view, index stats, or
ping/health, per issue #94's own follow-up list) should read this list
before extending `src/admin_ui.rs` or `submitted_query()` further — none of
these four are fixed, only deferred.
