# Report: Harness and test-infra follow-up debt

- Branch: `31-harness-debt`
- Issue: [#31](https://github.com/markdlabrecque/wayfinder/issues/31) — "v1: harness and test-infra
  follow-up debt", 4 commits on top of `origin/main` at `237c8a6`.
- Pipeline: test-writer -> implementor (2 bounce rounds against the reviewer, plus 1 round adapting
  to a rebase after concurrent branches landed) -> reviewer (2 rounds: round 1 BOUNCE with 2
  must-fix, round 2 APPROVED) -> reporter (this report). Spec was an orchestrator scratchpad file
  and was not committed to the repo.

## What was built

Scope items, per the issue:

1. **`manifest-errors.tsv` wired into the differential harness.** `load_manifest_errors`
   (6-column format) added to `tests/common/diff.rs`. Hermetic runner
   `manifest_errors_every_row_runs_against_the_matching_hermetic_app` routes each row to the
   correct in-process app per core (`app_and_request_url`: `content`/`facets`/`keyorder`/`sortdebt`/
   `facets33` schemas and corpora are mirrored locally; unknown cores such as `nosuchcore` and
   `schemaless_probe` are deliberately left unrewritten and hit the `content` app). An env-gated
   live counterpart, `live_solr_matches_committed_manifest_errors`, mirrors this against a running
   Solr, with reachability probes that print a skip line rather than failing when a container is
   down. Both runners use non-tautological `ran`/`diffed`/`skipped` anti-vacuity counters (a
   round-1 reviewer must-fix — see Review outcome). Those counters proved load-bearing within this
   same session: after the `#32`/`#33` merges landed 44 more manifest-errors rows mid-pipeline, the
   counter caught the mismatch (diffed 22 != 66) before the implementor adapted the routing.

2. **`ACCEPTED_DIVERGENCES` (permanent) vs `EXPECTED_DIVERGENCES_MANIFEST_ERRORS` (self-expiring).**
   `ACCEPTED_DIVERGENCES` is a new, deliberately permanent, ratified list — distinct in kind from
   the self-expiring `EXPECTED_*` lists — for divergences that are not owned by any future fix:
   `err_missing_core` (finding 15, an HTML fixture, status-only comparison),
   `update_unknown_field_schemaless` (per PRD §3), and the finding-16 facet trio.
   `EXPECTED_DIVERGENCES_MANIFEST_ERRORS` carries 5 self-expiring entries, all under issue #35:
   `facet_unknown_field` plus 4 `facet_err_*` rows (`facet_err_query_single`,
   `facet_err_field_single`, `facet_err_query_field`, and one more range-triggered row — see the
   issue-tracker comment below for exact names). Root cause: Wayfinder omits the `response` block
   that Solr's fixtures carry alongside `error` on query/field-triggered facet errors. The reviewer
   independently verified the root cause is byte-identical across all 4 `facet_err_*` fixtures, and
   that they genuinely diff to zero once that one field is excused.

3. **Score tolerance is no longer dead code.** `diff_ranked_ids` now takes `RankedDoc` (id + optional
   score) instead of a bare id list, and compares per-position scores under `score_tolerance()`,
   with present-vs-missing treated as a diff, all logged in `touched`. Two new fixtures,
   `select_term_scored` and `select_quick_scored` (`fl=id,score`), were captured incrementally on
   the canonical `wayfinder-solr-ref` container against `content`/`manifest.tsv`, append-only —
   verified three times that pre-existing fixtures stayed byte-identical. Both are listed in
   `RANKED_RELEVANCE_ENTRIES` and carry a self-expiring `EXPECTED_DIVERGENCES` entry under new issue
   #34 (per-doc `score` and `response.maxScore` are not yet rendered by Wayfinder). A test perturbs
   the real `select_term_scored` fixture's scores by `tol/2` (passes, logged as touched) and by 10x
   tolerance (fails, naming the score path in the diff). Live mode's ranked entries now go through
   the same ranked+score path.

4. **`request()` consolidated.** Moved out of `tests/error_shapes.rs` into `tests/common/mod.rs` as
   `request_full` plus a thin wrapper, removing the duplicate.

5. **`key_order` scoping and derivation.** `IGNORED_KEYS` narrowed to apply only under
   `response.docs[<i>]` rather than globally, with a regression test pinning the narrower scope
   (mutation-verified — see Test evidence). The three `json_key_order` query constants are now
   derived at runtime from `manifest-errors.tsv` instead of hand-duplicated. A docs-non-empty
   vacuity guard was added to the `select_all` whole-envelope test (round-1 reviewer must-fix — the
   prior version had a negation bug that let an *absent* `docs` key pass the assertion).

6. **Docs and findings.** Finding 24's `fl`-order half is now pinned by a new fixture
   `select_fl_reversed` (`fl=body,id` comes back as `id,body` — input order, not request order).
   Finding 26 is completed to reflect that Wayfinder's params echo now follows request order
   (landed under issue #25). Finding 31 is claimed and records the per-doc `score` and
   `response.maxScore` facts; findings 32/33 are reserved in the numbering but **not used** by this
   work. The harness section of `docs/solr-ref-findings.md` was rewritten to describe the
   manifest-errors wiring, the `ACCEPTED` vs `EXPECTED` distinction, and a caveat that live
   error-mode **writes** to the reference container via the `schemaless_probe` re-POST.

## Issues filed by this pipeline

- **#34** — `fl=score`: render per-doc `score` and `response.maxScore`. Owns the expiry of the two
  scored `EXPECTED_DIVERGENCES` entries.
- **#35** — facet errors omit the `response` block Solr carries alongside `error`. Owns all 5
  entries in `EXPECTED_DIVERGENCES_MANIFEST_ERRORS`.

## Test evidence

Final gates, all re-verified independently by the reviewer (not just claimed by the implementor):

- `cargo test`: **248 passed, 0 failed**, across 11 suites. (`main` stood at 235 at the rebase
  point; the difference is this branch's new tests plus concurrently-merged work absorbed via
  rebase.)
- `cargo fmt --check`: clean.
- `cargo clippy --all-targets -- -D warnings`: clean.
- `git diff` against `main` under `solr-ref/`: pure appends — 3 new fixtures added
  (`select_term_scored`, `select_quick_scored`, `select_fl_reversed`), nothing re-captured. A backup
  of `solr-ref/responses/` and `manifest.tsv` was taken before any capture run as a safety net, per
  repo convention, but was not needed.
- **Live modes exercised end-to-end** against the canonical `wayfinder-solr-ref` container on
  `:8983`: both the whole-query-set live test and the manifest-errors live test ran green against a
  real, reachable Solr. Rows tied to `:8985`/`:8986`/`:8987`/`:8988` genuinely exercised the
  printed-skip path (those containers were down during the run) — documented as `EXPECTED` skips in
  the output, not silently passed and not failed.
- **Mutation evidence**, each deliberately broken, confirmed to turn a test red for the right
  reason, then reverted:
  - Corrupted a manifest-errors row's expected status -> red.
  - Planted a bogus `ACCEPTED_DIVERGENCES`/`EXPECTED_DIVERGENCES` entry -> red, with a
    "stopped diverging, remove this entry" message.
  - Unscoped `IGNORED_KEYS` back to global -> the new regression test went red.
  - Silently skipped a row in the manifest-errors loop -> the `diffed` counter caught it, re-run
    against the merged 71-row file after the `#32`/`#33` rebase.
  All four were reverted before handoff.

## Review outcome

**Round 1: BOUNCE**, two must-fix items, both returned to the original implementor (same agent, via
handoff) and fixed:
1. The `ran`/`diffed`/`skipped` anti-vacuity counters were tautological as first written (they could
   not fail even if the loop body silently did nothing) — rewritten to be a real, independent check
   against the manifest row count.
2. The `select_all` docs-non-empty vacuity guard had a negation bug: an *absent* `docs` key passed
   the assertion it was meant to catch.

**Round 2: APPROVED.** Both fixes were re-verified by the reviewer against the mutation tests
described above.

The **2-round cap was reached** (round 1 bounce, round 2 approve, no rounds remaining). Per the
pipeline's own rule, this report records that **the work could use a further review pass**,
particularly around the live-mode paths, which CI never runs and which this pipeline's review
depth did not get a second independent look at beyond the one live run captured above.

## Follow-ups (deferred, not yet actioned)

1. `docs/solr-ref-findings.md:228` is stale: it describes a single expected-divergence entry, but
   the expected-divergence list (`EXPECTED_DIVERGENCES_MANIFEST_ERRORS`) now carries five.
2. `docs/solr-ref-findings.md:242-243`'s stale container list omits the `:8987`/`:8988` rows that now
   exist.
3. Issue #35's body and done-when criteria name only `facet_unknown_field`, but all five entries in
   `EXPECTED_DIVERGENCES_MANIFEST_ERRORS` share the root cause and must be deleted together when
   #35 lands. (A comment was posted to issue #35 recording this — see below.)
4. In the manifest-errors live/hermetic loops, the counter assertions run *before*
   `assert!(failures.is_empty())`, which masks the informative per-row diff list on a real
   regression. Should be reordered so the counters check after the diff failures are reported.
5. In the live loop, an unreachable default base pushes a failure but increments neither `ran` nor
   `skipped`, so the counter assertion misfires first during a canonical-container outage rather
   than reporting the real cause. The `:1643` inline comment claiming rows are "accounted for
   exactly" is incorrect as written.
6. Carried from round 1, not re-litigated in round 2: the finding-16 facet trio is pinned
   status-only and could be tightened to pin the exact `400` + `error.code`; `err_missing_core`'s
   Wayfinder-side response body is never asserted to actually be JSON (only the Solr-side fixture is
   asserted non-JSON); the live self-expiry exemption is keyed on `RANKED_RELEVANCE_ENTRIES`
   membership, which is a proxy for "this is a Wayfinder-side divergence" rather than a direct check;
   and `response.maxScore` is never compared for ranked rows, even once issue #34 lands (issue #34's
   own territory, noted here so it isn't lost).

## Pointers

- Tests (modified/new): `tests/common/diff.rs` (`load_manifest_errors`, `RankedDoc`,
  `score_tolerance` wiring), `tests/differential.rs` (`manifest_errors_every_row_runs_against_the_matching_hermetic_app`,
  `live_solr_matches_committed_manifest_errors`, `ACCEPTED_DIVERGENCES`,
  `EXPECTED_DIVERGENCES_MANIFEST_ERRORS`), `tests/common/mod.rs` (`request_full` consolidation),
  key-order test file (`IGNORED_KEYS` scoping, runtime-derived query consts, docs-non-empty guard).
- Fixtures (new, append-only): `solr-ref/responses/select_term_scored.json`,
  `select_quick_scored.json`, `select_fl_reversed.json`; `solr-ref/manifest.tsv` extended to match.
- Docs (modified): `docs/solr-ref-findings.md` — harness section rewritten, findings 24/26 updated,
  finding 31 added.
- Issues filed: #34 (`fl=score` / `response.maxScore`), #35 (facet errors omit `response`).
- Issue-tracker action taken by this reporter: comment posted on #35 recording that the #32/#33
  merges surfaced the same root cause in the four `facet_err_*` fixtures, and that all five
  `EXPECTED_DIVERGENCES_MANIFEST_ERRORS` entries must be deleted together as #35's done-when, not
  just `facet_unknown_field`.
- Review depth: 2 of 2 rounds used (bounce, then approve). This report explicitly flags that the
  work could use a further, independent review pass, particularly on the live-mode paths.
