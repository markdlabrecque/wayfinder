# Report: `*:*` sub-clause panics the worker and drops the connection

- Branch: `39-wildcard-panic`
- Issue: [#39](https://github.com/markdlabrecque/wayfinder/issues/39) — `q=*:* AND lazy`,
  `q=lazy OR *:*`, and `q=*:* -lazy` panicked the handler and dropped the connection (curl
  rc=52, no HTTP response at all) rather than returning any response, error or otherwise. This
  was an unauthenticated remote-availability bug: one crafted request could kill any connection,
  and no `catch_unwind`/panic-catching layer existed anywhere in the stack to contain it.
- Pipeline: test-writer -> implementor -> reviewer (1 round: APPROVED, with process-only
  must-fix items resolved by the orchestrator, not code defects) -> reporter (this report).

## Root cause

`*:*` as the *whole* query string is already special-cased to `AllQuery` in
`src/core_index.rs`. But as a **sub-clause** of a larger boolean query (`*:* AND lazy`,
`lazy OR *:*`, `*:* -lazy`), the string falls through to
`tantivy::QueryParser::parse_query`, which compiles `*:*` to a `UserInputLeaf::Exists` leaf.
`tantivy-query-grammar-0.26.0`'s `Exists` leaf unconditionally panics via
`.expect("Exist query without a field isn't allowed")` on `set_field(None)` — reachable with
nothing but a crafted `q=` parameter.

The original spec proposed rewriting the sub-clause to `<uniqueKey>:*` (e.g. `id:*`) instead.
The implementor discovered — and documented in `src/core_index.rs` with vendored-source line
citations — that this does **not** work: tantivy's grammar parses any `field:*` as
`UserInputLeaf::Exists` too, and `QueryParser::compute_logical_ast_for_leaf`
(`tantivy-0.26.1/src/query/query_parser/query_parser.rs`) unconditionally rejects `Exists`
leaves with `QueryParserError::UnsupportedQuery`, regardless of whether a field is present —
`Exists` support was apparently never wired up in `QueryParser`. Verified empirically:
`index.parse_query("id:*", "body")` errors rather than parsing. This is a corrected ticket
premise, flagged rather than silently built to, per the project's own convention on wrong
premises.

## The fix (two parts, commit `1c4a408`)

1. **Root-cause fix — `src/core_index.rs`.** A new `rewrite_wildcard_subclause` method
   (`query_str.replace("*:*", "*")`, called before `rewrite_dynamic_fields` in `parse_query`)
   rewrites an embedded `*:*` sub-clause to a bare `*`. The grammar's `leaf()` combinator
   resolves a bare `*` to `UserInputLeaf::All` — tantivy's own native all-docs leaf, no field
   required, the same leaf its own bare-`*` support and whole-string `*:*` special-case already
   produce — rather than `Exists`. Documented as a `ponytail:` substring replace (same ceiling
   as the existing `rewrite_dynamic_fields`): it would also fire inside a quoted phrase that
   literally contains the text `*:*`; no fixture exercises that case, and it is named as a known
   gap rather than silently handled.

2. **Defence in depth — `src/lib.rs`.** Added `tower_http::catch_panic::CatchPanicLayer::custom(handle_panic)`
   to the axum router in `build()`. `handle_panic` converts any caught handler panic into an
   HTTP 500 in a Solr-shaped error envelope (`WfError::internal("wayfinder::PanicError", details).envelope(Envelope::Bare)`)
   instead of dropping the connection. New dependency `tower-http = { version = "0.6", features
   = ["catch-panic"] }` (resolved 0.6.11) in `Cargo.toml`. `Envelope::Bare`'s doc comment in
   `src/error.rs` was widened from PUT-specific wording to also describe this second caller,
   since the panic handler runs outside any single request's parsed `Params` and has nothing to
   echo.

   A new, off-by-default Cargo feature `test-support` (`[features] test-support = []`) gates a
   debug-only route `/solr/{core}/__test_panic__` (`#[cfg(feature = "test-support")]`) that
   panics unconditionally. This exists solely so `tests/panic_recovery.rs` can exercise the real
   production `CatchPanicLayer` against a genuine, deliberate panic, independent of the `*:*`
   bug's own lifecycle — fixing that bug in the same change means the original trigger query
   stops panicking, so the test needs its own trigger. The feature is enabled only via this
   crate's own `[dev-dependencies]` self-reference (`wayfinder = { path = ".", features =
   ["test-support"] }`); because the crate uses `edition = "2024"` (Cargo resolver 3),
   dev-dependency features do not unify into non-test builds, so `cargo build`/`cargo build
   --release` never compile the debug route in.

## Fixture and test evidence

**Fixtures captured from real Solr 9** (per project convention — fixtures are ground truth,
never derived from the implementation): `solr-ref/responses/select_wildcard_and_term.json`,
`select_wildcard_or_term.json`, `select_wildcard_minus_term.json`, captured against a dedicated
container (`wayfinder-solr-39`, port 8990 — deliberately not 8983/8989, owned by concurrently
in-flight issues #8/#9) running the same schema/corpus as the canonical `content` core.
Appended as ordinary `content`-core GET rows to `solr-ref/manifest.tsv` (200s, so
`manifest.tsv` not `manifest-errors.tsv`):

```
select_wildcard_and_term    200  select?q=*:*+AND+lazy&df=body&fl=id,body&wt=json
select_wildcard_or_term     200  select?q=lazy+OR+*:*&df=body&fl=id,body&wt=json
select_wildcard_minus_term  200  select?q=*:*+-lazy&df=body&fl=id,body&wt=json
```

with a matching capture block appended to the end of `solr-ref/capture.sh`. The existing
`hermetic_whole_query_set_matches_committed_fixtures` test in `tests/differential.rs` picks
these up automatically (it iterates every `manifest.tsv` row) — no new test code was needed for
the query-correctness half.

**Unit test** — `src/core_index.rs`, `wildcard_subclause_parses_without_panicking`: pins that
`parse_query` succeeds (does not panic or error) for all three originally-panicking shapes.
Doc comment explicitly notes correctness of the resulting doc set/order is covered separately
by the differential-harness fixtures above; this test only pins that parsing itself succeeds.

**New integration test** — `tests/panic_recovery.rs`
(`panic_in_handler_is_caught_and_returns_solr_error_envelope`): builds the real
`wayfinder::app`, sends a request to the `test-support`-gated debug panic route on a spawned
`tokio::task` (mirroring axum's per-request task boundary, so a still-uncaught panic shows up
as a `JoinError` rather than crashing the test process), and asserts the response is HTTP 500
with a `WfError`-shaped JSON body (`error.msg` string, `error.code == 500`, `error.metadata`
array).

## Pipeline stages

- **test-writer** wrote `tests/panic_recovery.rs` first; confirmed red (panicked at the task
  boundary, surfaced as a `JoinError`) before any production code changed. The three
  differential-harness rows were already red (a real panic reproducing the issue) from the
  fixture capture alone, with no separate test code needed.
- **implementor** made both fixes. Hard gate confirmed before handoff: `cargo fmt --check`
  clean, `cargo clippy --all-targets -- -D warnings` clean, `cargo test` fully green (254
  passed at that point, pre-rebase).
- **reviewer** (Opus, independent) returned **APPROVED**, with empirical verification rather
  than reading alone:
  1. Mutation-tested the `CatchPanicLayer` itself — temporarily neutered it, confirmed
     `panic_recovery` goes red, restored it — proving the layer is what actually catches the
     panic, not some other incidental mechanism.
  2. Ran `strings` on both a debug and a `--release` binary to confirm `__test_panic__` never
     appears in a normal build, while the real routes and the panic-handler's type name do
     appear — confirming the feature gate genuinely keeps the debug route out of production
     builds, not just out of `#[cfg]`-annotated source.
  3. Re-ran the differential harness directly (bypassing `cargo test`'s output-swallowing) to
     confirm all three new fixture rows hit 0 diffs — exact doc order and set match Solr, not
     just document count.
  4. Independently verified the `Exists`-leaf-rejection claim against vendored tantivy source
     line numbers, rather than trusting the implementor's citation.
  5. Live-probed several edge-case query shapes not covered by the three captured fixtures —
     `(*:*) AND lazy`, `*:* AND -lazy`, `+*:* +lazy`, quoted `"*:*"`, `-*:*`, no-space
     `*:*AND lazy` — against a throwaway local instance, checking for silently-wrong (not just
     untested) rewrite behavior. Found none: the one known gap (a literal quoted `*:*` phrase)
     is the accepted, documented `ponytail:` comment, not a silent bug.
- **Process-only must-fix items** (not code defects) were resolved by the orchestrator before
  this report: committed the diff, applied the reviewer's one trivial doc-comment nit
  (`src/error.rs`'s `Envelope::Bare` doc, widened from PUT-specific to also cover the panic
  handler), then rebased onto `origin/main` — which had moved forward significantly in the
  interim (issues #9, #35, #40 all landed, two of which touch `src/lib.rs`/`src/core_index.rs`,
  the same files this change touches). Rebase required one manual conflict resolution in
  `solr-ref/capture.sh` (pure append-order conflict, resolved by keeping both appended blocks in
  sequence, #9's first since it merged first, then #39's).

## Gates (re-verified independently by the reporter, not just trusted from the handoff)

- `cargo fmt --check`: clean.
- `cargo clippy --all-targets -- -D warnings`: clean (CI's exact invocation).
- `cargo test`: **286 passed**, 0 failed, across 13 suites.

These were re-run against the current post-rebase state of the branch, not copied from the
implementor's or reviewer's earlier runs.

## Open follow-ups (reviewer's, non-blocking, deferred — not resolved by this report)

1. **Unverified 500 envelope shape.** No captured Solr 500 fixture exists anywhere in the repo,
   so the panic envelope's exact shape choice (`Envelope::Bare`) is unverified against real
   Solr — real Solr's handler-error shape is presumed closer to `Envelope::NoParams`, but this
   is not confirmed. Recommend recording as an open question in `docs/solr-ref-findings.md`, or
   filing a follow-up issue to capture a real Solr 500 if one can be induced.
2. **Panic payload leaks into the response.** `handle_panic` echoes the raw panic payload
   string into `error.msg` on an unauthenticated endpoint — a dependency panic could carry
   internal detail. The reviewer judged this defensible (Solr itself leaks Java stack traces in
   some error paths) but flagged it as worth a second look.
3. **`-*:*` still 400s.** A pure-negative wildcard query (`-*:*`) returns 400 — a pre-existing
   tantivy limitation unrelated to this fix (plain `-lazy` alone also 400s) — where real Solr
   likely returns 200 with `numFound 0`. This was one of the originally panicking shapes; it no
   longer panics (now 400s loudly instead of dropping the connection) but nobody has captured a
   fixture proving Solr's actual behavior here. Recommend filing a follow-up issue.
4. **Nit.** `rewrite_wildcard_subclause` takes `&self` but does not use it; could be a free
   function or associated function instead. Very low priority.

The review pipeline reached APPROVED within its first round (no bounce was needed on the code
itself — only process items were outstanding), so the 2-round cap was not exhausted here.

## Pointers

- Production code: `src/core_index.rs` (`rewrite_wildcard_subclause`, wired into `parse_query`),
  `src/lib.rs` (`build()`'s `CatchPanicLayer` wiring, `handle_panic`, feature-gated
  `test_panic`), `src/error.rs` (`Envelope::Bare` doc comment), `Cargo.toml` (`test-support`
  feature, `tower-http` dependency, self-referential dev-dependency).
- Tests: `src/core_index.rs`'s `wildcard_subclause_parses_without_panicking`,
  `tests/panic_recovery.rs`'s `panic_in_handler_is_caught_and_returns_solr_error_envelope`,
  `tests/differential.rs`'s `hermetic_whole_query_set_matches_committed_fixtures` (new rows
  picked up automatically).
- Fixtures: `solr-ref/responses/select_wildcard_and_term.json`,
  `select_wildcard_or_term.json`, `select_wildcard_minus_term.json`; `solr-ref/manifest.tsv`
  rows `select_wildcard_and_term`/`select_wildcard_or_term`/`select_wildcard_minus_term`;
  capture block appended to `solr-ref/capture.sh`.
- Issue: [#39](https://github.com/markdlabrecque/wayfinder/issues/39).
