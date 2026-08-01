# Issue #158 -- `GET /solr/{core}/admin/mbeans`

- Branch: `158-admin-mbeans`
- Worktree: `/Users/mark/Projects/wayfinder-158`
- Commits: `1dc9893` (red tests), `6df6711` (implementation), `534ce25` (test: pin
  `softAutoCommitMaxTime` to the trace's string form), `9539a69` (fix: render
  `softAutoCommitMaxTime` as Solr's `"<N>ms"` string), `425e35a` (test: mutation-proof
  unknown-core and delete-by-query guards -- round-1 review fixes), `490abeb` (docs: note
  `autoCommitMaxTime` is captured but unserved)
- Base: rebased onto `origin/main` `50c3252`

## What was built

`GET /solr/{core}/admin/mbeans?stats=true` now serves the MBeans status report. This is the
fourth and last of the #155/#156/#157/#158 batch closing coverage-contract gaps against
`search_api_solr` 4.4.0. The handler is `admin_mbeans` in `src/lib.rs`, backed by three new
`CoreIndex` counters (`deletes_by_id`, `deletes_by_query`, `pending_docs`) in `src/core_index.rs`.

**Ground truth** is `solr-ref/search-api/trace/00025.json`. Verified leaf values: three
integer counters (`docsPending`/`deletesById`/`deletesByQuery` = 0), and two Solr-side string
figures: `softAutoCommitMaxTime = "5000ms"` and `autoCommitMaxTime = "15000ms"` -- both
unit-suffixed strings, not bare integers. `INDEX.sizeInBytes = 21607`, `INDEX.size = "21.1 KB"`
(the same figure rounded), `INDEX.segments = 1`. The mixed typing (bare-integer counters beside
string-typed time budgets) is Solr's own on the wire, matched deliberately rather than tidied
into consistency.

**Real vs placeholder.** Follows the honest-subset precedent already set by
`admin_info_jvm_system_security()` in `src/lib.rs`: real values where a real consumer exists
(`docsPending`/`deletesById`/`deletesByQuery` from the live counters, `INDEX.sizeInBytes`/
`INDEX.size` from the live on-disk size, `CORE.coreName` from the real configured core name,
`softAutoCommitMaxTime` from the real configured autocommit interval), static placeholders
elsewhere (`class`/`description` on the `CORE`/`UPDATE` beans). The round-1 reviewer's
refinement on this precedent: the `class`/`description` strings here are not merely "plausible
placeholders" the way the doc comment describing the precedent implies -- they are verbatim
values lifted from the trace, which is a stronger claim than the precedent it cites, and the
code comment was corrected to say so.

**The arbitration that mattered.** The test-writer flagged `softAutoCommitMaxTime` as a
judgment call and proposed serving it as a bare integer, `-1` when unset -- mirroring
`search_api_solr`'s own default. The orchestrator overruled it: the trace has the string
`"5000ms"`, and `SolrConnectorPluginBase.php:781-798`'s
`isset($update_handler_stats['UPDATE.updateHandler.softAutoCommitMaxTime'])` guard proves `-1`
is the Drupal module's *own* initialiser for a key it did not find, not a value Solr ever put on
the wire -- so the correct behaviour is to omit the key entirely when unset, not to serve `-1` as
though Solr had reported it. Both the test (`534ce25`) and the implementation (`9539a69`) were
corrected to the string-and-omit form before this went further. `autoCommitMaxTime` is captured
in the trace but deliberately left unserved: no `search_api_solr` consumer reads it.

**Coverage measurement, and one item that overstates what landed.** `cargo run -- coverage
--format json` on the rebased branch: `53/75 -> 57/75`. Four items flip, only three of which the
ticket names: the `GET /solr/{core}/admin/mbeans` endpoint itself, `admin.mbeans.stats`, and
`admin.mbeans.solr-mbeans`. The fourth, unnamed by the ticket, is
`request.json-nl.repeated-map-and-flat` -- its probe (`src/coverage.rs:655-659`) only checks
for an HTTP 200 on a request that happens to be this endpoint's own malformed-glued-query
request, so it flips as a side effect of the route existing at all, not because anything about
repeated `json.nl` handling was actually verified. **This overstates what was implemented here**:
no test or code path in this branch asserts anything about `json.nl=map`+`json.nl=flat`
repetition beyond "the endpoint doesn't 500." A follow-up to tighten that probe is listed below,
not fixed on this branch.

## Test evidence (re-run for this report, not copied)

- `cargo fmt --check` -- clean.
- `cargo clippy --all-targets -- -D warnings` -- clean (CI's exact invocation).
- `cargo test` -- 716 passed, 38 suites, 0 failed.
- Coverage: `cargo run -- coverage --format json` and `tests/search_api_coverage.rs` both show
  `57/75` (up from `53/75`), re-measured by the orchestrator after the rebase onto `50c3252`.
  Endpoint-coverage sub-fraction is now `9/9` -- every endpoint in the coverage contract is
  covered (see "Closing the batch" below).
- Ground truth: `solr-ref/search-api/trace/00025.json`. No `solr-ref/manifest.tsv` row exists
  for `admin/mbeans` -- see follow-ups.

## Review outcome

Two rounds (the pipeline's default cap), both by an independent Opus reviewer. This work could
use further review passes beyond the two the cap allowed -- nothing in either round certified
the diff as exhaustively checked, only that the specific attacks made came back clean.

**Process note ahead of round 1.** The implementor edited two tests it did not author,
`mbeans_six_leaves_resolve_by_exact_key_strings` and
`mbeans_leaves_do_not_resolve_at_plausible_but_wrong_paths`, giving both an explicit soft-autocommit
config because as written they asserted all six leaves present under a `None` autocommit config
-- contradicting the arbitrated absent-when-unset behaviour above. The implementor self-reported
this edit rather than silently overriding the red-phase tests. The orchestrator named this as the
round-1 reviewer's first target, specifically asking whether any test still covered the other
five leaves under the default (`None`) config after the edit. The reviewer acquitted the edit by
mutation: misspelling `CORE.coreName`, `INDEX.size`, and `UPDATE.updateHandler.deletesById` in the
handler each independently failed one of five other `None`-config tests, so a dropped leaf would
still be caught even with the two edited tests no longer covering that combination.

**Round 1** bounced three must-fix items, two of them mutation-proven silent holes:

1. **`docs/PRD.md` still described reverted behaviour.** It stated the bare-integer,
   `-1`-when-unset form for `softAutoCommitMaxTime` that commit `9539a69` had already reverted
   in code -- the fourth PRD-vs-code mismatch caught across this four-branch batch. Rewritten to
   state the `"<N>ms"` string form and the omit-when-unset rule, citing both the trace and the
   `isset` guard.
2. **No unknown-core test.** Deleting `check_core(...)` from `admin_mbeans` left the entire
   690-test suite green -- the same bug class that shipped in #156's first round (schema
   fieldtypes). Added `mbeans_unknown_core_is_a_json_404` in `425e35a`.
3. **An untested comment-only guard.** The rule that "a query that failed to parse must not
   count" toward `deletesByQuery` was asserted only in a code comment above the counter. Hoisting
   `deletes_by_query.fetch_add(1, ...)` above the `parse_query(...)?` call left the suite green.
   Added `mbeans_deletes_by_query_does_not_count_a_query_that_failed_to_parse` in `425e35a`.

**Round 2** re-verified each fix by re-applying the exact mutation itself rather than reading the
diff -- confirmed all three now fail the suite. It additionally checked the wrong-reason trap on
item 3: it inserted a `fetch_add(99, ...)` probe immediately after a *successful* `parse_query`
call and confirmed the test stayed green, which pins the 400 specifically to the parse step
rather than to something earlier in the request path. It confirmed the unknown-core test asserts
the full error envelope, not just a status code -- `responseHeader.status`, `responseHeader.params`
present, `error.code`, `error.msg` naming the core, and `solr-mbeans` absent from the body -- so it
is a leak test rather than a status-only test. It confirmed the `src/core_index.rs` change from
this round was comment-only (+3/-0), and that the round's commit was purely additive on tests
(zero deletions, no `#[ignore]`). Approved with no must-fix items outstanding.

## Follow-ups deferred by the reviewer -- filed as issues (from this batch's reviews)

1. **#160** -- the schema loader accepts duplicate `[[field_types]]` names, silently leaving the
   second definition dead. Pre-existing, surfaced (not caused) by this batch's review.
2. **#162** -- three coverage response-field probes (including this endpoint's neighbours in the
   batch) accept an empty container as "covered," with no content assertion.
3. **#164** -- a dynamic field name containing a dot resolves but never matches: the read path
   splits on `.`, the write path does not. Pre-existing, shared with `/select`, not caused by this
   branch.

## Follow-ups NOT yet filed -- open gaps

1. **The coverage-probe overstatement noted above.** `request.json-nl.repeated-map-and-flat`'s
   probe (`src/coverage.rs:655-659`) should assert `solr-mbeans` is an object, not just that the
   request 200s -- today it flips on a bare 200 while the sibling `request.json-nl.flat` probe
   stays uncovered, and nothing in this branch actually exercises repeated `json.nl` semantics.
2. **`mbeans_strict_params_accepts_the_documented_allowlist`** asserts 200 for `cat` and `key`
   but not that they are *ignored* -- a future filtering implementation of those params could
   narrow the mbean dump and still pass this test.
3. **No test pins `human_size(21607) == "21.1 KB"`**, the one spelling claim in this endpoint
   tied directly to the trace. `src/admin_ui.rs:360-369`'s existing tests cover other magnitudes
   only, not this one.
4. **No `solr-ref/manifest.tsv` row exists for `admin/mbeans`**, so the differential harness does
   not cover this endpoint -- the same gap #155 (`terms`) closed its report noting for itself.

## Closing the batch

All four endpoints have landed: #156 as `e5f75a8` (`schema/fieldtypes`), #157 as `a1b637b`
(`admin/luke`), #155 as `50c3252` (`terms`), #158 (`admin/mbeans`) on this branch, pending
merge. Coverage moved 46/75 -> 57/75 across the batch. PRD open question #142 is resolved as
"In." The rebase of this branch onto `50c3252` conflicted in `tests/search_api_coverage.rs` at
all three of the usual edit points (the endpoints-uncovered list, the response-fields-uncovered
list, and the fraction comment plus assertion) -- exactly the collision each sibling report
flagged as expected. Resolving it left the endpoints-uncovered list empty: with mbeans landed,
every endpoint in the coverage contract is now covered.

The batch's most transferable lesson is not endpoint-specific: four separate PRD-vs-code
mismatches were caught by review across the four branches (#156's language count, #157's
real-field-count undercount plus a stale doc comment plus an unbacked test-module claim, #155's
fabricated stopword finding, and this branch's reverted-`softAutoCommitMaxTime` description) --
docs and code comments drifted from the implementation inside almost every branch in this batch,
and every one of the four review passes caught it rather than any pre-merge check.

## Bottom line

`GET /solr/{core}/admin/mbeans?stats=true` lands, closing the #155/#156/#157/#158 batch. All
local gates green (fmt/clippy clean, 716/38 tests passing), coverage 53/75 -> 57/75 -- three of
the four flipped items are genuinely earned (the endpoint, `admin.mbeans.stats`,
`admin.mbeans.solr-mbeans`); the fourth, `request.json-nl.repeated-map-and-flat`, flips as a
side effect of the route existing and overstates what was actually verified, with a follow-up
to tighten that probe left open. Two review rounds: round 1 caught a PRD/code mismatch (the
fourth in this batch), a missing unknown-core guard, and a comment-only delete-by-query guard,
all mutation-proven and all fixed on this branch; round 2 re-verified every fix by re-applying
the exact mutations, checked the wrong-reason trap on the delete-by-query fix, confirmed the
unknown-core test is a full envelope leak test, and approved with no must-fix items outstanding.
Four follow-ups deferred, none blocking: the overstated coverage probe, a loose
strict-params-allowlist test, an unpinned `human_size` spelling claim, and no differential
manifest row for this endpoint. Closing the batch: coverage moved 46/75 -> 57/75 across all four
branches, endpoint coverage is now 9/9, PRD open question #142 is resolved as "In," and four
separate PRD-vs-code mismatches were caught by review across the four branches -- the batch's
most transferable lesson.
