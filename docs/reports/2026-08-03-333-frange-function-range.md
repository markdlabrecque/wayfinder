# #333 — `{!frange}` function-range filter

**Date:** 2026-08-03. Branch `markdlabrecque/issue-333-frange-function-range`
off `main`. Closes #333 (split from #292). Depends on #289 (✅ landed,
function evaluator). The other named dependency, #332 (`geodist()`), is
**not started** — read on for why the core of #333 landed without it.

## Scope read, and a dependency that did not block

#333's title bundles two things: a general `{!frange l=.. u=..}<func>`
range filter, and the Drupal `setSpatial()` distance-facet rewrite. The
sizing report (`docs/reports/2026-08-08-292-spatial-sizing.md`) and the
issue body itself frame the filter as deliberately **non-geo** —
"`geodist()` is simply the function that flows through it" — so it depends
only on #289's landed evaluator, not on #332.

That framing is what made the work landable now: `{!frange}` is a filter
over **any** function the evaluator knows (`rating`, `product(rating,2)`,
constants, …). `{!frange}geodist()` composes for free once #332 adds a
`GeoDist` variant — frange calls the evaluator generically, so no frange
change is needed. The distance-facet rewrite itself is client-side
(`SearchApiSolrBackend::setSpatial()`), so Wayfinder only needs
`{!frange}<func>` (this PR) plus `geodist()` (#332). The `<`/`<=`
`{!geofilt}`/`{!bbox}` clauses are #332's own scope.

Deferred to #332, then: the `{!key=spatial-…}{!frange}geodist()` end-to-end
(the labelled facet key, and the geodist body). The verbatim-key
`facet.query={!frange …}field` form is covered here.

## What was verified against `solr:9`

Read straight off `FunctionRangeQParserPlugin.java` (9.10.1) and confirmed
fixture-by-fixture, because the wire form is not what memory or finding 133's
shorthand suggested:

- The inclusivity local params are **`incl` and `incu`** (booleans, default
  `true`), *not* `incl`/`excl` with the tokens `"lower"`/`"upper"` — those
  400 as `invalid boolean value`. Finding 133's `{!frange l=..[ u=..]}` was
  shorthand, not literal.
- A doc matches iff the function value **exists** for that doc and falls in
  the (half-open) range. This is the load-bearing difference from `{!func}`:
  `{!func}field` *scores* every document (missing → `0.0`), but `{!frange}`
  *filters*, and `ValueSourceRangeFilter` excludes any document whose
  function does not exist — so `{!frange l=0 u=100}price` drops the doc with
  no `price` even though its evaluated `0.0` would be in range.
- A compound function exists iff every referenced field does
  (`MultiFunction` all-exist): `{!frange l=0 u=15}sum(price,1)` drops the
  no-`price` doc even though `0+1=1` would be in range.
- Constant score `1.0` as the main `q` (`q={!frange …}field` → each match
  scores `1.0`, Solr's `ConstantScoreScorer`).
- Works in `q`, `fq`, and `facet.query`. A non-numeric bound Solr 500s on
  (a leaky `NumberFormatException`); Wayfinder returns the correct 400 — not
  client-exercised, so it carries no fixture and no divergence entry.

## Change

`src/function_query.rs` — the filter dual of `FunctionScoreQuery`:

- `FuncQuery::exists` + `FieldColumns::exists` — Solr's `exists()` over a
  `ValueSource`. A constant always exists; a field exists iff the doc has a
  value; a compound exists iff every argument does. `Missing` column (field
  declared but no numeric fast column this segment) → `false` for every doc.
- `frange_matches` — pure predicate (value, exists, lower, upper, incl, incu)
  so the boundary logic is unit- and mutation-testable without an index.
- `FunctionRangeQuery` — a `Query` matching exactly the documents whose
  function value exists and is in range, constant-score `1.0`. `AllQuery`
  drives the doc set (as `FunctionScoreQuery::all` does); the scorer is
  pre-positioned at its first match in `Weight::scorer` because Tantivy's
  `DocSet` iteration is `doc()`-first (`fill_buffer`, `seek`, the default
  `for_each` all read `doc()` before `advance()`).

`src/core_index.rs`:

- A `{!frange}` arm in `parse_function_query_q` (`l`/`u`/`incl`/`incu`,
  body parsed + field-validated like `{!func}`'s).
- A delegation at the top of `parse_query` so `fq` and `facet.query` —
  which route through `parse_query`, unlike `q` — recognise `{!frange}`.
  This also makes `{!func}`/`{!boost}` usable as `fq`/`facet.query`, a
  correct Solr-parity improvement that came for free (no existing fixture
  asserted the prior 400).

`tests/differential.rs` — the 18 `frange` rows route to the existing `fnq`
app (byte-identical numeric `docValues` corpus; no frange fixture queries
`body`, so the text analyzer is irrelevant).

## TDD

18 fixtures captured against real `solr:9` (own `frange` core, reusing the
fnq corpus) in a self-contained appended `capture.sh` block: inclusive/
exclusive bounds, `l`/`u`-only, no bounds, float bounds, missing-value
exclusion on a field and on a compound function, a constant function,
frange as `q` (score), as `fq`, and as `facet.query` (verbatim key), plus
three error shapes.

The differential went red for the right reason first (15 rows "HTTP status
400 vs expected 200" — `{!frange}` was an unrecognised local-param block),
then green. The load-bearing pieces are mutation-tested against both the new
unit tests and the differential:

- Inverting the `exists` guard → unit test fails; `frange_missing_excluded`
  and `frange_compound_missing` go red (the missing-field doc wrongly
  included).
- Inverting the lower-bound inclusivity term → unit test fails;
  `frange_inclusive`/`frange_incl_false`/`frange_incu_false` go red.

`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and the
full `cargo test` suite are green.
