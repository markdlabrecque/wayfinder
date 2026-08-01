# Issue #136 — Ratify edismax v1 six-param descope

- Branch: `136-ratify-edismax-descope`
- Worktree: `/Users/mark/Projects/wayfinder-wt/136`

## What was built

Issue #136 ratifies PRD §5's v1 edismax descope of six params (`bf`, `pf2`, `pf3`, `ps`,
`stopwords`, `lowercaseOperators`, and full function-query syntax) with documentation plus an
expiring guard. **No behaviour change.** `strict_params` acceptance of these six params under
`SELECT_PARAMS` is deliberately left alone — that is issue #135's job and remains open; all six
still 400 under `strict_params = true` after this change.

### `docs/PRD.md` (31 insertions / 5 deletions, §5)

Three defects fixed:

1. **`bf` had two dispositions.** The v1 "Out" bullet independently excluded `bf` function
   queries, while the v4 phase table separately listed "Function queries (`bf`, `{!func}`)" as
   in scope for v4 — two places asserting different things about the same param. Now resolved to
   one: the v1 text states `bf` and full function-query syntax are **not** a second, independent
   v1 exclusion, and defers to the v4 phase-table line as the single disposition. It also records
   that v1 has no function-query evaluator at all, so a function-query argument is
   accepted-and-ignored rather than rejected (findings 75 and 83).
2. **`boost` was listed as In and described as "a multiplicative wrapper".** That read as full
   support. Finding 83 records real Solr's `boost` as a function-query parameter, and Wayfinder
   implements only the constant-numeric case
   (`src/lib.rs:1257`: `params.get("boost").and_then(|s| s.parse().ok())`). The PRD now states, in
   one place, that `boost` is supported for its constant-numeric form only, and a function-query
   form parses to no boost and is ignored — the same accept-and-ignore treatment `bf` gets, landing
   with v4.
3. **Capture ratification evidence is now recorded in prose**, not left as an unaudited claim:
   zero client usage of the six descoped params across the 28 committed traces in
   `solr-ref/search-api/trace/`, and none of the six appears in the `captured_parameters`
   denominator in `coverage/search_api_coverage_contract.json` — so building any of them moves
   the coverage fraction by zero. The passage also notes the `stopwords` param/filter-name
   collision (the analyzer filter of the same name is implemented and appears in captured schema
   responses) and points at `tests/edismax_descope_guard.rs` as the expiring guard that fails the
   day this evidence stops holding.

### `tests/edismax_descope_guard.rs` (new, 9 tests)

An expiring guard per CLAUDE.md's deliberate-skips convention: a self-deleting file meant to go
red the day its justifying evidence stops holding, not to be edited back to green when it does.
Structure: 4 forward-looking evidence guards (green by design today, built to fail on a future
trace/coverage-contract change) plus 3 PRD-content assertions and 2 supporting/positive-control
tests. It scans both channels a Solr param can arrive on — top-level query-string parameter names,
and local-param keys inside `{!...}` blocks in query values (`{!edismax qf='...'}`, the form
`search_api_solr` actually uses) — against all 28 committed traces and the coverage contract.

## Pipeline history (including what went wrong)

- **Stage 1** wrote the guard split into 4 forward-looking evidence guards (green by design,
  built to go red if the descope's justification stops holding) plus 3 PRD-content assertions
  (red until the prose existed, since it referenced text that did not yet exist). It flagged its
  own content assertions as matching on bare tokens — a known weakness going into stage 2.
- **Stage 2 confirmed that flag was correct**: `lower.contains("function")` **already passed
  against the unfixed PRD**, because the section already contained the phrase "`bf` function
  queries" — so the assertion proved nothing specific about `boost`. All three PRD-content
  assertions were tightened to require the relevant terms co-located within one blank-line-
  separated paragraph block, rather than anywhere in the section.
- **Review round 1 bounced it on a real hole**: the guard as built scanned only top-level
  query-string parameter names, but the captured client (`search_api_solr`) never sends `qf` that
  way — it appears solely as a local param inside the `q` value, e.g. `{!edismax qf='...'}`. The
  corpus's entire local-param key set is `{qf, key}`. The realistic future wire form of a newly
  descoped param arriving (`{!edismax qf='...' ps=2}`) would have left the query-string-names-only
  guard green — exactly the "permanently green lie" the deliberate-skips convention in CLAUDE.md
  exists to prevent.
- **Round 2 fixed it** by extending the scan to also parse `{!...}` blocks inside query values and
  collect their local-param keys, and added a positive control
  (`the_scan_sees_the_local_param_channel_the_client_actually_uses`) asserting the scan actually
  observes `qf` via that channel, so the local-param scan cannot silently regress to a no-op.
  Mutation evidence: with `ps=2` injected as a local param into trace 00005, the old
  (names-only) scan stayed **green**, and the new scan correctly goes **red**.
- **Review round 2 approved.** The reviewer extracted `collect_local_param_keys` into a standalone
  binary and diffed the observed key sets before/after: the new scan adds exactly `qf` and `key`,
  with zero pollution from quoted local-param values in the current corpus. It reproduced the
  round-1 mutation five separate ways (`ps`, `bf`, `pf2`, a nested `{!...}` block, and a second
  block later in the same query value) and confirmed each one flips the guard red. It also
  verified the lexer's `else { break }` on an unterminated `{!` (no matching `}`) is lossless for
  every trace in the corpus, not merely a conservative guess — no trace has an unterminated block.

Two rounds were used, the default cap. This work stopped at the cap rather than getting further
independent scrutiny; per CLAUDE.md, that should be stated plainly rather than read as full
sign-off equivalent to unlimited review.

## Test evidence (re-run, not copied)

- `cargo test` — 606 passed across 32 suites.
- `cargo test --test edismax_descope_guard` — 9 passed.
- `cargo fmt --check` — clean.
- `cargo clippy --all-targets -- -D warnings` — clean.

## Follow-ups deferred by the reviewer

1. **The lexer's quoted-value ceiling.** `collect_local_param_keys` is a deliberately shallow
   lexer: any `=` inside a quoted local-param value (e.g. `bq='title:"ps=2"'`) is read as a key,
   so that example yields a spurious `ps` key and a false RED whose failure message wrongly points
   at the descope needing revisiting. Nothing in the current 28-trace corpus has such a value. The
   orchestrator added a comment on `collect_local_param_keys` naming this consequence explicitly
   and the correct fix (skip `=` occurring inside a `'`/`"` region), and stated the fix must not be
   to weaken the guard.
2. **Body-borne params are only partly guarded.** `no_trace_carries_a_form_encoded_body` rules out
   `application/x-www-form-urlencoded` request bodies, but Solr's JSON Request API also accepts
   `application/json` POSTs to `/select` with params nested under `{"params":{...}}`, and trace
   00001 proves this client does POST JSON (to a different endpoint today). A future JSON-body
   `/select` trace could carry a descoped param invisibly to this scan. The 28-trace count guard
   (`trace_corpus_is_the_28_traces_...`) is the backstop for that gap, since any new trace trips
   it regardless of what it contains. The orchestrator narrowed the test's docstring to say this
   precisely rather than implying full body coverage.
3. Both of the above were comment/docstring-only edits made by the orchestrator directly after the
   2-round review cap — not a third review round. No test behaviour or assertion changed.

## Explicitly out of scope, left for #135

This ticket deliberately left `SELECT_PARAMS` in `src/lib.rs` untouched. All six descoped params
(`bf`, `pf2`, `pf3`, `ps`, `stopwords`, `lowercaseOperators`) still 400 under
`strict_params = true`. Wiring them into `SELECT_PARAMS` (accepted-and-ignored per the PRD text
this issue just wrote) is issue #135's job and remains open.
