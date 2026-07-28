# Report: facet debt — error precedence, float/date key rendering, unpinned semantics (issues #33, #30)

- Branch: `33-facet-debt` (worktree)
- Issues: #33 (v1 milestone, facet debt sweep), #30 (facet error precedence — closed by this
  branch). Read first: `CLAUDE.md`, `docs/solr-ref-findings.md` findings 38-41,
  `docs/reports/2026-07-28-numeric-facet-enumeration.md` (issue #24 — the precedent this issue
  extends and, in one place, corrects).
- Three commits: `9d156ba` (fixtures + capture script block + findings), `1cdd781`
  (implementation + tests, including round-1 fixes baked in at handoff), `103e52f` (round-1
  review fixes: naming two unfixtured ceilings, tightening a silent-`0.0` fallback into a loud
  `unreachable!`).

## The headline

Issue #24 (numeric facet enumeration) hoisted `facet_fields` ahead of `facet_queries`/
`facet_ranges` in `src/facet.rs::facet_counts` so the warnings vector would be known before
`responseHeader` was built. That hoist silently inverted Solr's error precedence — a fact issue
#24's own report flagged as an unobserved follow-up (its follow-up 2), and issue #30 was filed
against. This branch's capture settled Solr's actual precedence (finding 38:
`facet.range` > `facet.query` > `facet.field`, exactly one error per response) and the fix
re-orders *evaluation* back to range-first while leaving the *emitted* key order of
`facet_counts` (`facet_queries`, `facet_fields`, `facet_ranges`, `facet_intervals`,
`facet_heatmaps`) untouched — those are two separate contracts, and `tests/json_key_order.rs`
was left unmodified as the guard on the second one.

Alongside that, three more real divergences (`pdouble`/`pfloat` key rendering, millisecond-date
bucket collapse, non-gap-aligned range-end echo) and four previously-unpinned-but-correct
semantics (missing-bucket limit/mincount exemption, `json.nl=arrarr`/`arrmap`, `json.nl=map` +
`facet.missing`) were fixed and pinned respectively, per findings 39-41.

## Capture

22 new `manifest-errors.tsv` rows against a self-contained `facets33` core
(`wayfinder-solr-33`, port 8988) — the block is appended to the end of `solr-ref/capture.sh`,
per convention. The canonical `wayfinder-solr-ref` container was left untouched; fixtures were
backed up outside the repo before the run per `CLAUDE.md`'s re-capture warning. All 22 rows
landed in `manifest-errors.tsv` only (non-`facets` core, or otherwise not a plain core-relative
GET) — confirmed by `git diff --stat main...HEAD`, which shows 22 new lines in
`manifest-errors.tsv` and zero in `manifest.tsv`. The differential harness
(`cargo test --test differential`, 18 tests) is unaffected, and `EXPECTED_DIVERGENCES` in
`tests/differential.rs` gained and lost no entries.

**Findings claimed: 38-41** in `docs/solr-ref-findings.md` (confirmed by reading the file: the
section header reads "Claiming findings 38-41 (issues #31 and #32 have 31-33 and 34-37
reserved, per issue #33)" — a numbering reservation, not a collision, this time).

Corpus: 5 docs (`r1..r5`) on `views` (pint), `price` (pdouble), `rating` (pfloat), `stamp`
(pdate, millisecond values), `tag` (string, docValues, absent from r4/r5), `note` (string,
stored-only, unfacetable). The clean `undefined field: "nosuchfield"` wording in
`facet_err_field_single.json` is the schema-cleanliness check (the issue-#26 lesson) — this
container never saw a schemaless probe.

Findings, verified by reading `docs/solr-ref-findings.md` directly:

- **38 — error precedence.** Singles (`facet_err_{query,field,range}_single.json`) establish
  each error in isolation; every pairing and the all-three combo report exactly one error at
  `facet.range` > `facet.query` > `facet.field` precedence (`facet_err_query_field.json` ->
  query error; `facet_err_query_range.json` / `facet_err_field_range.json` /
  `facet_err_all_three.json` -> range error). The #30 shape verbatim
  (`facet_err_query_vs_unfacetable.json`): an invalid `facet.query` plus a stored-only
  `facet.field` reports the query `SyntaxError` — in Solr the unfacetable half isn't an error
  at all (ratified divergence 2), so the query error surfacing over it needs no divergence
  entry, just a test pinning it.
- **39 — pdouble/pfloat rendering.** Java `Double.toString` semantics: an integral double
  renders `"5.0"`, never `"5"` (`facet_field_double_all.json`, `facet_field_float_all.json`).
  Ordering by value throughout, including count-sort's value-ascending tie-break — finding 30
  (issue #24) extended to doubles.
- **40 — millisecond pdate.** Two values inside the same second stay distinct buckets
  (`facet_field_date_ms_all.json`: `.123Z`/`.456Z`); a whole-second value renders without a
  trailing `.000` (`...00:00:00Z`). Order chronological.
- **41 — four previously-unpinned semantics.** (a) `facet.missing` exempt from `facet.limit`
  and `facet.mincount`. (b) A range span not divisible by the gap extends the last bucket to
  the gap boundary and echoes the *gap-aligned* `end` (30), not the requested value (22) —
  `facet_range_end_not_gap_aligned.json`. (c) `json.nl=arrarr`/`arrmap` nested shapes. (d)
  `json.nl=map` + `facet.missing` keys the null bucket `""`.

## What was built

- **`src/facet.rs`** — evaluation reordered to range -> query -> field (fixes #30). Results are
  hoisted into local bindings and evaluated in that order before being placed into the `json!`
  object in the unchanged emitted key order (`tests/json_key_order.rs` untouched and green — the
  contract it guards was never meant to move). `JsonNl` enum (`Flat`/`Map`/`ArrArr`/`ArrMap`)
  replaces the old boolean `as_map`, feeding a rewritten `render_buckets` that produces all four
  shapes; applies identically to `facet_fields.<name>` and `facet_ranges.<name>.counts`. New
  `echo_range_end` echoes the walked bucket boundary (gap-aligned) with a fallback to the
  requested `end` string when the walk produces zero buckets (unfixtured fallback, called out as
  such in-code).
- **`src/core_index.rs`** — `render_double` renders Java-`Double.toString`-style output, driven
  by the schema's declared `ValueKind::F64` — *not* by sniffing the aggregation bucket's key
  variant or value, because Tantivy's own aggregation normalises an exactly-integral double to a
  `U64`/`I64` key variant (`NumericalValue::normalize`), so variant-sniffing would misrender an
  `I64` column's identical values (`views`) the same wrong way it was trying to fix for `price`/
  `rating`. `FacetOrderKey::Nanos(i128)` replaces the old f64-seconds-plus-fraction sort key with
  an exact nanoseconds-since-epoch carrier (issue #24's own follow-up 1, closed here against real
  ms fixtures). Both of the issue-#24 "ponytail" comments this branch's fixtures were captured to
  settle are closed. A `Key::Str` on an `F64`-kind column — previously a silent `0.0` fallback —
  is now a loud `unreachable!()` (round-1 fix, `103e52f`).
- **`src/schema.rs`** — one line: the fast date column's precision is set to milliseconds
  (Solr's `pdate` precision). Reviewer verified `set_precision` affects only the fast field (the
  indexed date term is unaffected), so `tests/sort.rs` — read-only, untouched — still passes
  unmodified with strictly finer tie-breaking available to it.
- **`tests/faceting.rs`** — sections 17-22, 25 new tests (79 total in the suite, up from 54 at
  issue #24's handoff). 17 were red-first (precedence combos, "5" vs "5.0", same-second date
  collapse, end-echo 22-vs-30, arrarr/arrmap flat-vs-nested); 8 were green from birth, pinning
  already-correct behaviour, each test comment saying so explicitly (missing-bucket exemptions,
  map+missing empty-string key, the `ValueKind::Text` range-facet bail, and the query-vs-
  unfacetable #30 shape once verified red-for-precedence-not-for-lack-of-a-check). A local
  `facet_error_class(msg) -> FacetErrorClass` (`RangeUnfacetable`/`QuerySyntax`/`UndefinedField`)
  maps both Solr's and Wayfinder's wording to a class, per the `sort_error_class` pattern in
  `tests/sort.rs` (read-only, not touched) — the fixture decides which class wins, no message
  text is frozen. The `ValueKind::Text` range bail is exercised via a fast string field (`tag`),
  the one path that reaches `src/facet.rs:386-390` at all (a non-fast text field is caught by
  `check_facetable` first).

## Issue #30 verdict

#30's own done-when, checked against what actually landed:

- **(a) Fixture capturing Solr's behaviour for >= 2 broken facet params, across all three
  families.** Done — `facet_err_query_field`/`query_range`/`field_range`/`all_three` cover every
  pairing and the triple, plus the exact #30 shape
  (`facet_err_query_vs_unfacetable.json`).
- **(b) Wayfinder matches whichever error Solr reports, with a test that fails if precedence
  changes.** Done — evaluation reordered to range > query > field; six class-level tests, each
  mutation-verified (see below) to fail if the reorder is reverted.
- **(c) Escalation path.** Not needed — there is no divergence to ratify. The unfacetable-field
  half of the #30 shape is consistent with already-ratified divergence 2 (Wayfinder 400s on an
  unfacetable field where Solr silently no-ops), and the query-syntax error surfaces identically
  on both engines regardless of precedence, so nothing new needed a decision.

**Conclusion: #30's done-when is fully met.** The PR for this branch carries `Closes #30`
alongside its own issue-#33 closure.

## Mutation testing

Per the implementor's own account, re-verified against the diff rather than taken on faith:

1. **Precedence reverted** (range/query/field back to field/query/range) — caught by all 5
   combo tests plus the #30-shape test.
2. **F64 rendering switched to key-variant sniffing** instead of schema-driven — caught by the
   double/float fixture-match tests and the targeted regression test pinning that the *same*
   integral value renders `"5.0"` on `price` (F64) but `"5"` on `views` (I64).
3. **Millisecond date precision reverted** to `DateOptions::default()` (seconds) — caught by the
   three `date_ms` tests; `tests/sort.rs` (25 tests, read-only) confirmed unaffected by the same
   mutated build.
4. **Gap-aligned end echo reverted** to the requested value — caught.
5. **`arrarr`/`arrmap` mapped back to `Flat`** — caught.

## Review: two rounds, the pipeline's cap

**Round 1 (bounce).** No correctness defects found. Two `ponytail:` comments the reviewer
required before approval, both landed in `103e52f`:

- The `F64` end-echo accumulation drift (`facet.range` walking `lower + gap` bucket by bucket on
  a double field, so an aligned `start=0/end=0.3/gap=0.1` request now echoes the walked
  `0.30000000000000004` rather than the requested `0.3`) and the date start-vs-end render
  asymmetry (`end` now routes through `echo_range_end`/`format_date`, `start` still echoes the
  raw request string via `echo_bound` — the same kind of value rendered by two different rules).
  Neither is fixtured; both are now named in-code as open ceilings, not silently shipped.
- The same "5" vs "5.0" divergence (finding 39), unfixed on the `facet.range` bucket-boundary
  path (`range_buckets`'s plain `f64::to_string()`), named as the identical bug to `render_double`
  rather than a new one.
- `Key::Str` on an `F64`-kind column: a silent `0.0` fallback tightened to a loud `unreachable!()`.

**Round 2 (approve, final).** The reviewer independently re-verified `cargo test`, `cargo fmt
--check`, and `cargo clippy --all-targets -- -D warnings`, then traced the `unreachable!()`'s
soundness through Tantivy's whole key-variant dispatch: a `Key::Str` can only originate from
`ColumnType::Str` or the `DateTime` branch in `term_agg.rs`, neither reachable for a column
whose schema-declared kind is `ValueKind::F64`; `ValueKind::Date` routes through the `else` arm
instead; a dynamic/JSON field resolves `value_kind` to `None` and also takes the `else` arm — so
the panic path is genuinely unreachable for every route into `term_facet`, not merely
unreached-in-practice.

The reviewer additionally found a sharp edge beyond what the implementor's own comment claimed:
`tantivy`'s `agg_data.rs:964` builds one terms-aggregation node per column type for a given field
name, so an on-disk index whose physical schema disagrees with the current `wayfinder-schema.toml`
could in principle mix a `Str`-keyed bucket into what the TOML now calls an `F64` column. That
path is blocked by `schema::check_compatible` at startup and is reachable only through operator
error (deleting `wayfinder-schema.toml` while keeping the index around). Blast radius if it did
fire: the `unreachable!()` unwinds the request-handling task, the process survives; the pre-fix
behaviour in that same scenario was a silently wrong `"0.0"` key, so the change is a net strict
improvement even accounting for the edge case, not a new risk.

**Per the pipeline's own rule: this work has used its full 2-round allotment. This work could
use more review passes** if anything else surfaces later, specifically on the two unfixtured
`facet.range` numeric/date end-echo paths named above — those are the only places where behaviour
changed without a captured fixture behind it, and there is no built-in headroom for a third round
without escalating to the orchestrator.

## Test evidence

Verified independently by this reporter (ran the commands myself in the worktree, not copied
from the handoff).

`cargo test` — full run, by suite:

```
     Running unittests src/lib.rs        -> 8 passed; 0 failed
     Running unittests src/main.rs       -> 0 passed; 0 failed
     Running tests/differential.rs       -> 18 passed; 0 failed
     Running tests/error_shapes.rs       -> 12 passed; 0 failed
     Running tests/faceting.rs           -> 79 passed; 0 failed
     Running tests/json_key_order.rs     -> 14 passed; 0 failed
     Running tests/schema_layer.rs       -> 25 passed; 0 failed
     Running tests/server_config.rs      -> 18 passed; 0 failed
     Running tests/sort.rs               -> 25 passed; 0 failed
     Running tests/tracer_bullet.rs      -> 12 passed; 0 failed
   Doc-tests wayfinder                   -> 0 passed; 0 failed
```

Total: **211 passed, 0 failed** across 11 suites (8+18+12+79+14+25+18+25+12 = 211). `faceting.rs`
grew from 54 (issue #24's handoff) to 79 — exactly the 25 new tests this branch's spec asked for.

`cargo fmt --check` — clean, exit 0, no output.

`cargo clippy --all-targets -- -D warnings` — CI's exact command:

```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in ...s
```

Clean, zero warnings, exit 0.

Both gates match the handoff's claims exactly; nothing to flag as a discrepancy.

## Follow-ups to record (from the reviewer, verbatim in substance)

1. **`facet.range` bucket keys on double/float still render `"0"`, not `"0.0"`-style**
   (`src/facet.rs`, `range_buckets`, ~line 413 area, `ponytail:`'d) — needs a capture against
   `price`/`rating` plus a fix that routes through `render_double`, mirroring what `term_facet`
   already does for `facet.field`.
2. **No fixture for a non-gap-aligned f64/date range end, nor for the raw-vs-normalised
   start/end asymmetry.** The reviewer's belief that Solr's `FacetRangeProcessor` matches this
   is a reading of Solr's own source, not a captured fact — stated explicitly so it isn't
   mistaken for ground truth later.
3. **`render_double` folds `pdouble` and `pfloat` together.** 32-bit `Float.toString` will
   diverge from 64-bit `Double.toString` outside the values this capture happened to pin;
   capture a `pfloat` value like `0.1` (where the 32-bit and 64-bit shortest-round-trip strings
   differ) before trusting the current shared implementation more broadly.
4. **`FacetOrderKey::Nanos` ordering is only indirectly covered** (via the millisecond-date
   fixtures, not a direct ordering test). Add a `#[cfg(test)]` unit test inside
   `src/core_index.rs` next time that file is touched — `FacetOrderKey` is `pub` but
   `core_index` is a private module with no re-export, so it is unreachable from integration
   tests today.
5. **Add a clause to the `unreachable!()` comment** citing the `agg_data.rs:964` /
   `schema::check_compatible` dependency the round-2 reviewer traced, next time the file is
   edited — the current comment doesn't yet name that specific safety net.
6. **`json.nl` gaps still unfixtured:** `arrarr`/`arrmap` + `facet.missing`'s null bucket;
   `arrarr` applied to `facet_ranges.<name>.counts` (the current test mirroring `map`'s treatment
   there is Wayfinder-only, said so in its own comment); unknown `json.nl` values falling back to
   `Flat` is Wayfinder's own choice, not a captured one.

## Other bookkeeping, confirmed by reading the diff

- **`docs/PRD.md` §2 ratified divergences** — unchanged. Nothing was added because nothing new
  diverges; the #30 query-vs-unfacetable shape is consistent with the *already*-ratified
  divergence 2, not a new one.
- **`tests/differential.rs`'s `EXPECTED_DIVERGENCES`** — unchanged; no entries added or removed.
  Confirmed: `git diff main...HEAD -- tests/differential.rs` is empty (the file is not in this
  branch's diff at all).
- **`solr-ref/manifest.tsv`** — untouched (all 22 new rows are in `manifest-errors.tsv`); the
  differential harness (18 tests) is unaffected by this branch, confirmed by its unchanged pass
  count above.

## Review depth statement

Round 2 was the second and final round permitted by the pipeline's 2-round cap. It closed as an
approval with six follow-ups, not a bounce, but the work has now used its full allotment of
review passes — **this work could use more review passes**, specifically on the two unfixtured
`facet.range` numeric/date end-echo paths (accumulation drift on doubles, start/end rendering
asymmetry) that changed behaviour with no ground truth behind them yet, and there is no built-in
headroom left for a third round without escalating to the orchestrator.

## Pointers

- Precedence fix (#30): `src/facet.rs` (`facet_counts` — range/query/field evaluation order,
  key order in the `json!` object unchanged)
- `json.nl` shapes: `src/facet.rs` (`JsonNl`, `render_buckets`)
- Gap-aligned range-end echo: `src/facet.rs` (`echo_range_end`, `facet_ranges`)
- Double/float rendering: `src/core_index.rs` (`render_double`, `term_facet`)
- Exact date sort key: `src/core_index.rs` (`FacetOrderKey::Nanos`)
- Millisecond date precision: `src/schema.rs` (fast date column, one line)
- Tests: `tests/faceting.rs` sections 17-22 (`FacetErrorClass`/`facet_error_class` helper, 25
  new tests)
- New fixtures: `solr-ref/responses/facet_err_*.json`, `facet_field_double_*.json`,
  `facet_field_float_all.json`, `facet_field_date_ms_*.json`, `facet_missing_with_*.json`,
  `facet_range_end_not_gap_aligned.json`, `facet_json_nl_arrarr.json`,
  `facet_json_nl_arrmap.json`, `facet_json_nl_map_missing.json` (22 files), captured against a
  self-contained container/core appended to `solr-ref/capture.sh`
- Capture rows: `solr-ref/manifest-errors.tsv` (22 new rows; `manifest.tsv` untouched)
- Findings: `docs/solr-ref-findings.md`, "Findings from the issue #33 facet-debt capture" (38-41)
- Review fix commit: `103e52f` (ponytail naming + `unreachable!()` tightening)
