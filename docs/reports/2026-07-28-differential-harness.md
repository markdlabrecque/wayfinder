# Report: Differential test harness (Solr vs Wayfinder)

- Branch: `differential-harness`
- Issue: [#1](https://github.com/markdlabrecque/wayfinder/issues/1) — "v1: differential test
  harness (Solr vs Wayfinder)", part of v1 (PRD §5). Built early per the issue's own rationale:
  it is the evidence behind every other v1 issue and the compatibility claim itself.
- Spec: task spec supplied to test-writer (temp file, not committed). Orchestrator decisions it
  fixed: test-only, no new `src/` unless a real divergence is found; query set is
  `solr-ref/manifest.tsv` (single source of truth, extended via `capture.sh`); two modes sharing
  one differ — hermetic default (`cargo test --test differential`, in-process Wayfinder vs
  committed fixtures) and live (`WAYFINDER_DIFF_SOLR=1 cargo test --test differential`, vs a
  running Solr); normaliser must log every path it touches, no blanket "ignore unknown keys"
  escape hatch; ranked-ID-list diff mode with score tolerance for relevance queries.

## What was built

Files:
- `tests/common/diff.rs` (313 lines, new) — `normalize`/`Normalized` (drops
  `responseHeader.QTime`, per-doc `_version_`/`_root_`, `error.msg`/`error.metadata` while
  keeping `error.code`, each touched path recorded and returned alongside the normalised value);
  `score_tolerance()` — a documented function (deliberately not a bare const, per test-writer's
  call, so the tolerance value is a visible, commented decision) returning `1e-3`, justified by
  the 5-doc corpus's adjacent-BM25-score gaps of ~1e-1+; `diff`/`Diff`/`DiffReport` — recursive
  structural walk that unions keys from both sides of every object so an extra-in-actual or
  missing-on-either-side key registers as a diff, not just changed values; `diff_ranked_ids` —
  ordered `id`-list comparison (order matters, not just membership); `doc_ids` — extracts the
  ordered id list from an envelope; `ManifestEntry`/`load_manifest` — parses
  `solr-ref/manifest.tsv`, skipping blank lines and `#` comments, tolerant of trailing columns;
  `fetch_live` — shells out to `curl` via `std::process::Command`, deliberately avoiding a
  `reqwest` dependency (spec explicitly allowed either; implementor chose the zero-dependency
  path).
- `tests/differential.rs` (556 lines, new) — 18 integration tests: normaliser/differ unit tests
  over fixture pairs with known diffs, manifest-loader tests, the hermetic whole-query-set test
  (`hermetic_whole_query_set_matches_committed_fixtures`), and the env-gated live counterpart
  (`live_solr_matches_committed_query_set`, no-ops unless `WAYFINDER_DIFF_SOLR=1`).
- `tests/common/mod.rs` (+11 lines) — `pub mod diff;` wiring, and `#![allow(dead_code)]` at the
  module root with a comment explaining why: `tracer_bullet.rs` and `differential.rs` are
  separate test binaries that each use disjoint subsets of `common`'s helpers, so per-item
  suppression can't work — "unused" depends on which binary is compiling the module.
- `docs/solr-ref-findings.md` (+48 lines) — new "Differential harness (issue #1)" section: how
  to run both modes, how to add a query (via `capture.sh` + re-capture, never hand-edited), and a
  full explanation of the `EXPECTED_DIVERGENCES` self-expiring descope list (see below).

No `src/` changes, no `Cargo.toml`/`Cargo.lock` changes — confirmed independently by this
reporter (`git diff --stat` shows only the two test files as untracked additions plus the two
documented modifications above).

## The substantive event: real divergences found and correctly escalated, not papered over

On its first real run against the full manifest, the harness failed on 10 of 25 entries. The
implementor did not widen the normaliser or build missing features to make them pass — it
escalated to the orchestrator with the diff evidence, which is the process working as designed.
The 10 failures were genuine, currently-real Wayfinder-vs-Solr divergences, all caused by
features that do not exist yet, not by a broken harness:

- **`select_sort`, `err_bad_sort`** — no `sort` implementation. PRD §7 explicitly lists sort as
  out of tracer-bullet scope. Owned by issue #2.
- **`facet_mincount`, `facet_limit`, `facet_missing`, `facet_query`, `facet_json_nl_map`,
  `facet_zero`, `facet_all_filtered`** — the tracer bullet counts facets over the *hit set*, but
  Solr's `facet.field` enumerates the *entire term dictionary* (so a zero-hit query returns all
  terms at count 0, not an empty array). This is a different data source, not a tolerance tweak
  — worth stating plainly here because it means issue #3's real scope is larger than its issue
  text implies. `facet.missing`, `facet.query`, and `json.nl=map` were also never built. Owned by
  issue #3.
- **`ping`** — the fixture's `responseHeader.params` carries Solr ping-handler artifacts
  including a per-run `rid` counter that no implementation can reproduce byte-for-byte;
  `tracer_bullet.rs::ping_reports_ok` already carves this same case out by only asserting ping's
  essential shape rather than diffing the full envelope.
- **`select_term` passed.** BM25 ranked order matches Solr exactly — the ranked free-text
  relevance work explicitly deferred from the tracer bullet to this issue is done and is now
  evidence-backed by a real diff, not just an assumption.

### Orchestrator ruling

Test-only scope stands: no `src/` changes, no feature work in this issue. The 10 entries were
descoped into an explicit `EXPECTED_DIVERGENCES: &[(&str, &str)]` list in `tests/differential.rs`
— every entry carries a mandatory reason string naming the owning issue (`#2` or `#3`). The
whole-query-set test still computes the real diff for every listed entry; it only excuses a
listed entry from failing the *test*. Critically, the list is a live check, not a static skip:
if a listed entry's diff ever comes back empty (i.e. the underlying feature ships), the test
fails and tells you which entry to delete. There is no code path for an entry to sit in the list
after the fix has landed without the suite turning red first — the descope expires itself when
issues #2/#3 land, rather than rotting into a permanent green lie.

The implementor verified the guard actually fires, not just by inspection: it planted a bogus
`select_all` entry (a query that genuinely matches) into `EXPECTED_DIVERGENCES`, confirmed the
test failed with the expected "stopped diverging, remove this entry" message, then reverted the
change before handoff.

## Test evidence

Stage 1 (test-writer) wrote 18 tests in `tests/differential.rs` plus a stubbed
`tests/common/diff.rs` (every function body `todo!()`). Confirmed red before implementation: 2
passed / 16 failed, all failures `not yet implemented` panics (no compile errors), and
`tracer_bullet.rs` stayed 12/12 throughout. The test-writer made two interface calls of its own:
`score_tolerance` is a function rather than a bare constant, so the tolerance value reads as a
visible, documented decision at the call site rather than a magic number; and the diff/touched-
path string formats are pinned with exact-string assertions.

Stage 2 (implementor) brought `diff.rs` to green with no test edits and no disputes over test
correctness — the only escalation was the real-divergence finding above, which is a scope
question, not a test dispute.

Verified independently by this reporter, `command cargo test` (bypassing shell aliases), full
output:

```
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.31s
     Running unittests src/lib.rs (target/debug/deps/wayfinder-f5cb2155803ff508)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/main.rs (target/debug/deps/wayfinder-80041d8a7da54b80)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/differential.rs (target/debug/deps/differential-22e815937c8c1582)

running 18 tests
test live_solr_matches_committed_query_set ... ok
test doc_ids_extracts_ordered_id_list_from_an_envelope ... ok
test load_manifest_parses_every_line_of_the_real_manifest ... ok
test load_manifest_skips_blanks_and_comments_and_tolerates_trailing_columns ... ok
test normalize_drops_qtime_and_logs_touched_path ... ok
test normalize_drops_error_msg_and_metadata_but_keeps_code ... ok
test ranked_id_order_difference_fails_even_with_identical_membership ... ok
test ranked_id_order_matching_passes ... ok
test params_object_equality_is_key_order_insensitive_by_construction ... ok
test differing_qtime_does_not_appear_as_a_diff ... ok
test differing_error_msg_and_metadata_do_not_appear_as_a_diff ... ok
test score_outside_tolerance_fails ... ok
test differing_error_code_is_still_a_diff ... ok
test diff_fails_on_facet_count_changed ... ok
test diff_fails_on_doc_reordered ... ok
test diff_fails_on_numfound_off_by_one ... ok
test score_within_tolerance_passes_and_is_logged ... ok
test hermetic_whole_query_set_matches_committed_fixtures ... ok

test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.12s

     Running tests/tracer_bullet.rs (target/debug/deps/tracer_bullet-1415712e72de5d58)

running 12 tests
test select_zero_results_has_correct_envelope ... ok
test select_doc_with_no_value_for_optional_multi_valued_field_omits_key ... ok
test facet_on_multi_valued_field_matches_flat_alternating_array_shape ... ok
test select_all_returns_all_docs_with_default_fl_and_no_internal_fields ... ok
test select_with_fq_filters_results ... ok
test select_pagination_start_and_rows ... ok
test select_rows_zero_returns_empty_docs_but_correct_num_found ... ok
test select_without_facet_param_has_no_facet_counts_key ... ok
test ping_reports_ok ... ok
test select_unknown_param_is_ignored_but_echoed ... ok
test select_unknown_fl_field_is_silently_dropped ... ok
test select_pagination_past_the_end_returns_empty_docs ... ok

test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.76s

   Doc-tests wayfinder

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

30/30 across the crate (18 new + 12 pre-existing tracer-bullet tests unaffected). Note
`live_solr_matches_committed_query_set` reports `ok` without touching the network: it early-
returns as a no-op when `WAYFINDER_DIFF_SOLR` is unset — confirmed by reading the gating in
`tests/differential.rs:518-519`. Plain `cargo test` stays fully hermetic, per spec.

Also verified independently, `command cargo fmt --check`: clean, no output.

Also verified independently, `command cargo clippy --all-targets -- -D warnings`: forced a full
rebuild first (`cargo clean -p wayfinder`) to rule out a stale-cache false negative, then re-ran
— clean build, zero warnings. This differs from the tracer-bullet baseline, which had two known
pre-existing lints (`result_large_err` at `src/lib.rs:68`, `collapsible_if` at `src/lib.rs:131`)
that the spec explicitly permitted to remain; since this issue made no `src/` changes those two
lints are presumably still latent in a plain non-`-D warnings` clippy run, but the strict gate
required by the spec (`cargo clippy --all-targets` no *new* warnings) passes because nothing in
this diff introduces any.

## Review outcome

Stage 3 (reviewer): **Approved, round 1 of 1**. The 2-round cap was not hit, so this work has had
the full permitted review depth and no "needs more review passes" flag applies.

The reviewer re-ran the gates itself independently (30 passed, fmt + clippy clean, confirmed no
`src/`/`Cargo.toml`/`Cargo.lock` changes) and specifically attacked the PRD §8 failure mode (an
over-eager normaliser silently greening the suite):
- Confirmed `normalize` touches exactly four things (`responseHeader.QTime`, per-doc
  `_version_`/`_root_`, `error.msg`/`error.metadata` while keeping `error.code`) with no wildcard
  matching and no branch that silently skips comparison.
- Confirmed `diff_at` unions keys from both sides of every object, so both an extra-in-actual key
  and a missing-on-either-side key register as diffs — neither direction can silently pass.
- Ran the query set with `--nocapture` to directly confirm the differ genuinely catches the
  heterogeneous flat facet arrays rather than passing by coincidence.
- Confirmed the score-tolerance path is gated on the literal key `"score"` and is always logged
  when it fires (not conditionally).
- Confirmed the `EXPECTED_DIVERGENCES` guard is a live, computed check against the real diff
  output — not a static `#[ignore]`-style skip.
- Confirmed `#![allow(dead_code)]` in `tests/common/mod.rs` is structurally necessary, not lazy
  suppression: `tracer_bullet.rs` pulls in `common` but uses nothing from `common::diff`, so
  per-item annotation cannot work across the two test binaries.

## Follow-ups (deferred by reviewer, not yet actioned)

1. `diff_ranked_ids` compares `id` order only, not scores — the spec's "ranked-ID-list mode with
   score tolerance" implies both. Moot today because no current fixture's `fl` includes `score`,
   but a future relevance fixture that does include scores would have a real score divergence
   silently ignored by this path.
2. No current fixture contains a `score` field at all, so the score-tolerance code path is
   exercised only by the synthetic unit tests in `tests/differential.rs`, never by the live
   query set against real fixtures.
3. `err_bad_sort`'s self-expiry relies on falling through to the content-diff branch rather than
   an explicit check — correct by inspection, but the reviewer flagged it as the least obvious
   code path in the file and wants an explanatory comment.
4. `manifest-errors.tsv` (added by issue #11 on branch `error-shapes`, for non-core-relative-GET
   error fixtures) is not yet wired into this harness.

Additionally, for the record: the live-Solr mode (`WAYFINDER_DIFF_SOLR=1`) has not been exercised
end-to-end in this session — only the hermetic mode against committed fixtures has been run and
carries the evidence above. The live path is implemented and gated correctly, but genuinely
running it against a live Solr container has not happened yet.

## Pointers

- Tests (new): `tests/differential.rs`, `tests/common/diff.rs`
- Tests (modified): `tests/common/mod.rs` (+11 lines: `pub mod diff;` wiring, `dead_code`
  allowance)
- Docs (modified): `docs/solr-ref-findings.md` (+48 lines: "Differential harness (issue #1)"
  section — how to run both modes, how to add a query, `EXPECTED_DIVERGENCES` explained)
- Query set / fixtures: `solr-ref/manifest.tsv`, `solr-ref/responses/*.json`,
  `solr-ref/capture.sh`
- Descope list: `tests/differential.rs::EXPECTED_DIVERGENCES` (10 entries, each with a mandatory
  reason naming issue #2 or #3)
- No `src/` or `Cargo.toml`/`Cargo.lock` changes in this issue
- Downstream issues this work surfaced or informs: #2 (`sort`), #3 (advanced faceting — scope
  larger than issue text implies, per above), #11 (`manifest-errors.tsv`, not yet wired in)
