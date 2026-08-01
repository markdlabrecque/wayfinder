# Issue #187 — boolean param parsing must match Solr's `StrUtils.parseBool`

Branch: `markdlabrecque/params-boolean-parsing-is-stricter-than-solrs-st` (off `main`).
Commits: `9d45fb0` (fixtures), `60ff39b` (red tests), `9765591` (implementation),
`22d50bb` (arbitrated test change), `37558ab` (review round-1 fixes).

## Corrected premise

The issue claimed Solr accepts `1`/`t`/`yes`/`on` case-insensitively as boolean values.
Captured `solr:9` (port 8996, one-off container, tracer-bullet schema/corpus, removed
afterwards) shows this is wrong: Solr does **not** accept `1`, `0`, `t`, `f`, or `y` —
all five are a 400. The real rule, on the value lowercased:

- `true` if it **starts with** `true`, `on`, or `yes` (`TRUE`, `Yes`, `oN`, `truestuff`,
  `onward`, `yesss` all parse true)
- `false` if it **starts with** `false` or `off`, or **equals** `no` exactly
  (`offside`, `falsey`, `NO` parse false; `noo` does **not** — the `no` arm is an exact
  match, not foldable into the prefix list)
- anything else, including the empty string, is a 400: `error.msg` = `invalid boolean
  value: <raw value>`, `error.code` = 400, `responseHeader.status` = 400

So the bug Wayfinder had was two-sided before this fix: recognized-truthy values
(`TRUE`, `yes`, `on`, and any prefix match) were silently parsed as false, and garbage
values (`1`, `nope`, `maybe`, empty string) were also silently parsed as false instead
of 400ing. Recorded as finding 115 in `docs/solr-ref-findings.md`, appended after 114 (renumbered from 109 across three rebases; main took 109-114 in the meantime).

## What was built

- One shared parser, `params::parse_bool`, implementing exactly the rule above.
- `Params` accessors (`bool_opt`, `bool_or`, `per_field_bool`) returning
  `Result<_, WfError>` so an invalid value 400s with the fixture-verbatim message.
- Every boolean request-param read swept onto the shared parser (~12 sites): `facet`
  (admin UI query form and `/select`), `stats` (`/admin/mbeans`), `hl`, `terms`,
  `commit`/`softCommit`, `overwrite` (default true), `mlt.boost`, `mlt.match.include`
  (default true), `facet.missing` (global and per-field override), and `omitHeader`.
  `admin_mbeans`'s `stats` prefix check stops being a documented deviation — Solr's own
  parser is itself a prefix test — but the `stats=true?omitHeader=false` glued-param
  trace comment is kept, rewritten rather than deleted.
- Error-timing split to match the fixtures: params read before the base query runs
  (`facet`, `stats`, `hl`, `omitHeader`) 400 with the error-only envelope (no `response`
  block); `facet.missing`, read inside `facet::facet_counts`, flows out through the
  existing non-`PreQueryFacetError` path so the base query's real `response` block
  rides alongside `error` (issue #35's shape).
- `omitHeader` validated at handler entry across all four endpoints (`/select`,
  `/update`, `/mlt`, `/terms`), since `Params::omit_header()` is called at render time
  and cannot itself fail there.
- Nine fixtures under `solr-ref/responses/bool_*.json`, all with `manifest.tsv` rows so
  `cargo test --test differential` replays them: the true/on/yes prefix families, the
  exact `no` case, one invalid `facet.missing` and one invalid `facet`, and
  `omitHeader=yes`.
- `WfError` gained `Display`/`Error` impls so it can travel the `anyhow` path used by
  `facet.rs` without a second copy of the error message.

## Test evidence — independently re-verified by the reviewer, not self-reported

- `cargo test`: 880 passed, 46 suites.
- `cargo test --test differential`: 29 passed (1 suite) — includes the nine new
  `bool_*` rows.
- `cargo fmt --check`: clean.
- `cargo clippy --all-targets -- -D warnings`: clean (CI's exact invocation).
- Mutation tests run, per CLAUDE.md's guard-the-guard requirement:
  - Breaking `parse_bool`'s reject branch (making an invalid value parse as false):
    caught by the parser's own unit test, both rewritten `assert_error_matches_fixture`
    integration tests, and the differential harness. Reverted after confirming.
  - Restoring the `commit || softCommit` short-circuit (round-1 must-fix 1): caught by
    the new commit/softCommit-ordering regression test. Reverted after confirming.
  - Deleting all four `omitHeader` entry-point guards (round-1 must-fix 2): caught by
    `every_handler_rejects_an_invalid_omit_header`. Deleting only the `/terms` guard
    was caught by that same test naming `/terms` specifically. Reverted after
    confirming.

## Review outcome

Bounced round 1 with three must-fix items, all mutation-verified before round-2
approval:

1. `/update`'s `bool_or("commit")? || bool_or("softCommit")?` short-circuited on `||`,
   so `commit=true&softCommit=nope` never parsed `softCommit` at all and answered 200 —
   silently accepting an invalid boolean, the exact class of bug this issue exists to
   remove. Fixed by binding both to locals before the `||`; a regression test covers
   both orderings.
2. Four `omitHeader` entry-point validations (`/select`, `/update`, `/mlt`, `/terms`)
   were completely unguarded by any test — all four were deletable with the suite still
   green, which would silently 200 an invalid `omitHeader` on every endpoint. Fixed with
   `every_handler_rejects_an_invalid_omit_header` plus per-endpoint invalid-boolean
   cases.
3. `solr-ref/capture.sh`'s nine `#187` `cap` invocations were left as commented-out
   text rather than live calls. Because the script opens with `rm -rf "$OUT"` and
   truncates `manifest.tsv`, a full re-run would have silently deleted all nine
   fixtures and their manifest rows. Made live; the port-8996 provenance prose was
   kept as a comment. `capture.sh` was **not** re-run to fix this — fixtures and
   `manifest.tsv` are untouched, and the call paths were diffed against manifest rows
   177-185 by hand instead.

Approved round 2 after all three were confirmed via the mutation tests above.

## Escalation and arbitration

The two error fixtures' `error.metadata` carry Java class names
(`org.apache.solr.common.SolrException`). Wayfinder does not impersonate Solr's
internal class names in its own error metadata, so widening the shared
`normalize_envelope` (used by every differential/error-shape test in the repo) to
tolerate that would have silently loosened every other test using it. Instead the
relaxation was scoped to a new local helper, `assert_error_matches_fixture`, private to
`tests/bool_params.rs`. It still pins: HTTP status, `responseHeader.status`, the full
`responseHeader.params` echo, presence and contents of the `response` block (the whole
point of the error-timing split), `error.msg` verbatim, `error.code`, top-level key
order, and `error.metadata`'s length and keys — only the two metadata *values* are
relaxed. Re-mutation-tested afterward: breaking `parse_bool`'s reject branch still
fails both rewritten tests, the parser unit test, and the differential harness.

## Documented divergence

An invalid `omitHeader` (e.g. `omitHeader=1`) gets Wayfinder's ordinary JSON 400,
where real Solr returns a Jetty HTML error page. This is because Solr decides header
suppression before its response writer exists, so the container's own HTML error page
answers instead of a JSON envelope. No fixture was captured for this (none was needed
per spec) and no `manifest.tsv` row exists for it, so no `EXPECTED_DIVERGENCES` /
`ACCEPTED_DIVERGENCES` entry applies. Recorded in finding 115 and in the
`Params::omit_header()` doc comment, which was rewritten: its "accept side" ceiling is
now settled by `bool_omit_header_yes.json`; the still-unfixtured ceiling — error
envelopes carry `responseHeader` regardless of `omitHeader` — is kept as-is.

## Follow-ups deferred (not fixed here)

1. **`admin_mbeans`'s `stats` has no invalid-value test.** It is the one swept boolean
   read site in this sweep with no coverage of an invalid input; it currently works
   correctly by construction/probe but is unguarded against a future refactor that
   silently breaks it.
2. **The Jetty-HTML `omitHeader` divergence is asserted in four places
   (finding 115, the `omit_header()` doc comment, this report, and the spec) with no
   captured artifact backing it.** No fixture or raw HTML/status-line capture exists;
   the observed real-Solr status line should be recorded somewhere checkable so the
   claim isn't purely prose.
3. **`src/error.rs`'s new `Display`/`Error` impl on `WfError` lets a `WfError::internal`
   raised inside an `anyhow`-typed module come back as a plain 400** if the handler
   rebuilds the error from `e.to_string()`. Today only `facet.rs` uses this path, and
   only ever raises 400s through it, so nothing is currently miscategorized — but the
   hazard is only a code comment, not a guard or test. Documented in `src/error.rs`,
   unenforced.
4. **The commented-out-`cap`-calls hazard that round-1 must-fix 3 fixed for #187 also
   exists, unfixed, in at least the `#149` (colliding facet response keys) and `#150`
   (duplicate facet local-param keys) blocks of `solr-ref/capture.sh`.** Correction to
   the handoff: the task described these as "#150 and #102" — there is no `#102` block
   in `capture.sh`; the second affected block is `#149`'s
   `facet_collision_field_flat`/`_map` and `facet_collision_query_flat`/`_map` calls
   (deliberately not manifest rows, per that section's own comment). `#150`'s
   `facet_local_params_duplicate_key` **does** have a live `manifest.tsv` row (line
   176), so a wholesale re-run of `capture.sh` would silently delete both a
   manifest-backed fixture and file-only ones. Out of #187's scope; worth its own
   issue to make every commented-out block in `capture.sh` live.

## Gate cap note

Per the reviewer stage's default 2-round cap: this work only needed the two rounds
recorded above (bounced once, approved on the second pass), so the cap was not
exhausted and no additional-review-passes caveat applies.
