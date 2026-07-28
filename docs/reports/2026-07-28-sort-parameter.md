# Report: `sort` request parameter (issue #2)

- Branch: `2-sort-parameter`
- Scope: PRD §5 sort row — `sort=<field> <asc|desc>`, comma-separated multi-clause,
  `score desc` (the default) and `score asc`, ordering holding under `start`/`rows`
  pagination, and a hard 400 (never a silent fallback) for a clause Solr rejects.
- Spec: task spec supplied to the test-writer (temp file, not committed). It fixed
  ownership against issue #3 running concurrently — `src/collector.rs`, the
  hit-collection portion of `src/core_index.rs`, and `check_sort` in `src/lib.rs`
  are #2's; facet counting and facet envelope construction are #3's and were not
  touched. #2 merges before #3, so the `core_index.rs` diff was kept deliberately
  small (7 added / 4 removed lines).
- **Docker was available**, so every assertion in this work is derived from a real
  `solr:9` capture. Nothing is marked "needs fixture confirmation".

## What was built

Ordering lives in `src/collector.rs` (63 → 295 lines). `AllScoredHits` gained
multi-clause fast-field ordering: `SortClause`/`SortKey`, per-segment column
readers, Lucene's min/max selector for multi-valued fields, and missing-values-last.
The unsorted path is now expressed as the single implicit clause `score desc` plus
the pre-existing ascending-`DocAddress` tie-break rather than as a separate code
path, which is what keeps the 12 tracer-bullet pagination tests passing unchanged.

- `src/core_index.rs` — `search()` takes `&[SortClause]` and forwards it to
  `AllScoredHits::new(...)`. That is the whole diff; faceting is untouched.
- `src/lib.rs` — `check_sort` went from validate-only (landed by #11) to
  parse-and-validate, returning `Vec<SortClause>`. It was extended, not replaced,
  and the #11 error envelope was not re-derived.
- `tests/differential.rs` — `select_sort` **deleted** from `EXPECTED_DIVERGENCES`.
  That entry failing on purpose is the designed signal per `CLAUDE.md`; the
  surrounding comment now records that both `err_bad_sort` (#11) and `select_sort`
  (#2) are gone and why.
- `tests/sort.rs` — 561 lines, 25 tests, all derived from fixtures.

## Ticket-premise corrections (three)

Per `CLAUDE.md` ("Don't paper over a wrong ticket premise"). Each was captured
rather than argued, and each is now a numbered finding in
`docs/solr-ref-findings.md`.

1. **Sorting on a multi-valued field is not an error** (finding 16). Issue #2's
   "Out" scope said it should error like Solr. Captured Solr 9 returns **200**
   with Lucene `SortedSetSortField` selector semantics: `asc` orders by each
   doc's minimum value, `desc` by its maximum, and a doc with no value sorts
   **last in both directions** (so missing-last is not a consequence of
   direction). Implemented as captured. **Issue #2's text should be corrected
   when it closes** so the next reader is not misled.
2. **`TopDocs::order_by_fast_field` cannot implement this feature.** It orders by
   exactly one fast field — no composition for `score desc,id asc`, no `score
   asc`, no multi-valued selector. PRD §5's sort row names it as the mechanism and
   needs correcting to point at `src/collector.rs`.
3. **`id` needed `fast = true`** in the test schema (`tests/common/mod.rs`).
   Solr's `_default` configset gives its `string` type `docValues="true"`, so real
   Solr sorts on `id` — `select_sort.json` is exactly that query. Without `fast`,
   the mirror schema would 400 a query the fixture answers 200, which is a
   divergence in the *test schema*, not in Wayfinder. `body` stays non-fast so
   `err_bad_sort.json`'s 400 still reproduces.

## Fixtures

16 new fixtures against the same `solr:9` container and 5-doc corpus, all
core-relative GETs and so all in `manifest.tsv` (+16 rows). `capture.sh` gained an
appended-only block (+57 lines, no existing line changed) and the pre-existing
`solr-ref/responses/*.json` show **zero churn** — no accidental re-capture.

- Ordering: `select_sort_asc`, `select_sort_multi_asc`, `select_sort_multi_desc`,
  `select_sort_score_all`, `select_sort_score_asc`, `select_sort_score_desc`,
  `select_sort_paged`, `select_sort_paged_past_end`.
- Multi-valued selector: `select_sort_mv_asc`, `select_sort_mv_desc`.
- Errors: `err_sort_no_direction`, `err_sort_bad_direction`,
  `err_sort_score_bad_direction`, `err_sort_bad_clause_among_good`,
  `err_sort_field_before_direction`, `err_sort_direction_before_field`.

13 came with the feature; the last three were captured during review to settle
check-order claims that had been *inferred* rather than captured.

## Review outcome — 2 rounds, and round 1 found a real bug

Round 1 ruled out both weaknesses it was pointed at: cross-segment term ordinals
are resolved to an owned `String` inside the segment before any merge, and
`descending` is applied only in the `(Some, Some)` arm of the comparator, so
missing-last survives direction reversal and applies to non-leading clauses too.

Its one must-fix was that finding 18 overclaimed an uncaptured Solr fact as
reproduced — and **capturing the discriminating fixture disproved the
implementation.** Solr processes clauses left to right and stops at the first bad
clause; `err_sort_direction_before_field.json` (`sort=body sideways`, a single
clause bad in both ways at once, the only spec that separates the two
within-clause orders) shows the **direction is checked before the field** within a
clause. `score` 400s on a bad direction because it is special-cased out of *field
resolution*, not because parsing precedes resolution. The original two-pass design
was built to the wrong mechanism.

That was fixed in this issue rather than deferred: `CLAUDE.md` treats divergence
from captured Solr as a bug, and this divergence sat inside the exact feature #2
exists to make compatible. `check_sort` is now a single pass, per clause checking
the direction and then resolving the field, returning on the first bad clause.

**Round 2 was the capped final round, and its written verdict did not reach the
orchestrator through the normal channel.** The gates were therefore re-verified
independently by the orchestrator before proceeding, and again by this reporter
(below). Because round 2 was the cap, follow-ups 3-9 are tracked leftovers rather
than reviewed-and-accepted decisions: **this work could use more review passes.**

## The differential harness is blind to error-classification divergences

This is the finding most likely to matter to someone who is not working on sort,
and it is project-wide, not issue-#2-specific. **Issues #4-#9 need to know it.**

Reverting to the buggy two-pass `check_sort` **passed the entire sort suite and the
whole differential run.** HTTP 400 plus `error.code: 400` cannot discriminate two
genuinely different error classifications, and the harness's normaliser drops
`error.msg` by design (finding 10). The normaliser output says so out loud:

```
err_sort_field_before_direction: normaliser touched ["responseHeader.QTime", "error.msg", "error.metadata"]
err_sort_field_before_direction: 0 diffs
```

So two different errors with the same status and code diff to zero. **A green
differential run is evidence of envelope equivalence, not semantic
equivalence.** Any issue whose feature can produce more than one class of 400 —
query parsing, faceting params, update handling — cannot rely on the harness to
catch getting the classification wrong.

The local fix is a `sort_error_class()` helper in `tests/sort.rs` that reduces a
message to `direction` or `field` and compares the response against the fixture:

```rust
fn sort_error_class(msg: &str) -> &'static str {
    if msg.starts_with("Can't determine a Sort Order") {
        "direction"
    } else {
        "field"
    }
}
```

The fixture decides which is right and neither side's wording is frozen, so
Wayfinder keeps its own field-error wording from #11 while matching Solr's
direction message byte for byte including `pos`. This is a test-side mitigation
for one feature, not a harness fix; the harness limitation is unchanged.

## Test evidence

Independently re-run by this reporter with `command cargo test` (bypassing shell
aliases), hermetic — no network, no Docker. Live-Solr paths stay behind
`WAYFINDER_DIFF_SOLR=1`. **118 tests across 9 binaries, all green:**

```
     Running unittests src/lib.rs
running 8 tests
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
     Running unittests src/main.rs
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
     Running tests/differential.rs
running 18 tests
test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
     Running tests/error_shapes.rs
running 12 tests
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
     Running tests/schema_layer.rs
running 25 tests
test result: ok. 25 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
     Running tests/server_config.rs
running 18 tests
test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
     Running tests/sort.rs
running 25 tests
test result: ok. 25 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
     Running tests/tracer_bullet.rs
running 12 tests
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
   Doc-tests wayfinder
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

`command cargo fmt --check` — clean. `command cargo clippy --all-targets -- -D
warnings` (CI's exact command) — clean, zero warnings.

The sort suite was additionally run 8 consecutive times and was green 8/8, after
an earlier red turned out to be a mid-edit artifact rather than flake.

## Mutation testing

`CLAUDE.md` requires it for code whose whole value is failing correctly. Both
review rounds combined:

| Deliberate break | Tests that caught it |
|---|---|
| Direction defaults to `asc` instead of 400 | 3 |
| `score` short-circuits before the direction check | 3 |
| Direction applied to missing values (breaks missing-last) | 2 |
| Min/max multi-valued selector dropped | 1 |
| `DocAddress` tie-break reversed | 9, across three suites plus the hermetic differential |
| Two-pass `check_sort` restored | 1 — and **zero** before `sort_error_class()` existed |

That last row is the evidence for the blind-spot section above.

## Also fixed on this branch (pre-existing, from issue #1)

`fetch_live` in `tests/common/diff.rs` invoked `curl` without `-g`, so live mode
died on the unquoted `[` in `err_bad_syntax`'s `fq=category:[unclosed` — curl read
it as a glob range and exited non-zero before issuing a request. `capture.sh`'s
`cap()` has always passed `-sg`; this side was missing it, which broke live mode
for the whole manifest. With `-g`, **40 of 41 entries fetch and diff clean against
live Solr** (the 41st is follow-up 7). Issue #1's report listed "live mode never
exercised end-to-end" as a follow-up; this is that follow-up arriving.

## Follow-ups (deferred, not actioned here)

1. **PRD §5's sort row** names `TopDocs::order_by_fast_field` as the mechanism;
   correct it to point at `src/collector.rs`.
2. **Issue #2's "Out" scope** on multi-valued sort contradicts captured Solr
   (finding 16); correct the issue text when it closes.
3. **Clause splitting is comma-only, and tokens after the direction are silently
   dropped** — `sort=id asc garbage` returns 200. Solr's parser treats the comma
   as optional and reads a token stream, so `sort=id asc category desc` may be two
   clauses in Solr and one here: a silently dropped clause, which is exactly the
   "never a silent fallback" property the spec cares about. Uncaptured — capture
   `sort=id+asc+category+desc` and `sort=id+asc+garbage` and build to the fixture
   rather than guessing.
4. **Zero coverage of the `I64`/`F64`/`Date`/`Absent` sort-column arms** — the
   shared 5-doc corpus has no numeric or date field. Needs a new *captured*
   corpus, not an invented one. Worth its own issue: missing-last and the min/max
   selector on numerics are currently assumed, not verified.
5. **No multi-segment sort test** — `indexed_app` commits once. Correct today, but
   two commits before searching would pin it cheaply, and would catch a future
   "defer ordinal resolution" optimisation, the one change that would silently
   break the string sort.
6. **Bounded-heap ordering** — the `ponytail:` ceiling at `src/collector.rs:22`:
   sort keys are materialised for every match and the whole match set is sorted in
   a `Vec`.
7. **`live_solr_matches_committed_query_set` doesn't consult
   `EXPECTED_DIVERGENCES`**, so `ping`'s unreproducible `rid` fails live-vs-captured.
   Pre-existing from #1, one line, deliberately not fixed here.
8. **`partial_cmp().unwrap_or(Equal)` on `f64`** in `SortValue::cmp_value` would be
   non-transitive given a NaN, and `sort_by` can panic on a non-total order.
   Currently unreachable — scores are never NaN and `serde_json` cannot parse a
   NaN literal into a fast field. Worth a note at the call site rather than code.
9. **Multi-clause `pos=` arithmetic** is only verified for the single-clause case
   the fixtures cover. Acceptable — `error.msg` is outside the compatibility
   contract — but a later reader should not assume it was checked.
10. **`CLAUDE.md`'s fixture-restore advice is incomplete**: "restore with `git
    checkout -- solr-ref/`" only works for *tracked* files, so newly captured
    (untracked) fixtures survive as freshly-churned versions. The correct procedure
    is to commit or back up new fixtures before re-running `capture.sh`. The
    orchestrator is handling this amendment with the user directly and it appears
    to have already landed on `main`, ahead of this branch — a rebase onto `main`
    (`CLAUDE.md` §Workflow item 5) will pick it up. Recorded here, not edited by
    this reporter.

## Pointers

- Production code: `src/collector.rs` (`AllScoredHits`, `SortKey`, `SortClause`,
  `SortValue`), `src/core_index.rs` (`search()`), `src/lib.rs` (`check_sort`)
- Tests: `tests/sort.rs`, `tests/differential.rs`, `tests/common/mod.rs`
  (`SCHEMA_TOML`), `tests/common/diff.rs` (`fetch_live`)
- Fixtures as ground truth: `solr-ref/responses/select_sort*.json`,
  `solr-ref/responses/err_sort_*.json`, `solr-ref/manifest.tsv`,
  `solr-ref/capture.sh`
- Solr facts learned: `docs/solr-ref-findings.md` findings 16-20 (20 is the
  harness blind spot, kept as a worked example)
- PRD scope: `docs/PRD.md` §5 (sort row — see follow-up 1), §2 (compatibility
  contract)
