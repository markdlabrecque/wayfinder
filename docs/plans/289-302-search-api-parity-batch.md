# #289-#302: the remaining Search API parity batch — sequencing plan

**Date:** 2026-08-03. Covers the fourteen issues opened after the parity review of
2026-08-03: #289-#297 (server) and #298-#302 (the `search_api_wayfinder` module),
plus #308, split out of #291 by the wave 0b sweep. **Updated 2026-08-03** for the
completion of wave 1.

This document is the batch's execution order, not its scope — each issue body carries
its own scope and evidence requirements, and this plan does not restate them. Read it
before starting any branch in the batch, because several of the sequencing constraints
below are invisible from inside a single issue.

**Status:** waves 0 and 1 are complete (#306, #307, #310; #297, #299, #308, #295 merged).
Wave 2 can start now, and all three of its branches are unblocked — #278 landing freed
#294. #296 (H) is also startable, having been gated only on #295.

## Where parity actually stands

`tests/search_api_coverage.rs` is green on the full 75/75 captured `search_api_solr`
4.4.0 denominator: every request shape, endpoint, parameter and client-consumed
response field in the capture. A stock Search API site indexes, queries, facets,
highlights, runs MoreLikeThis, and extracts attached and linked files today.

**One exception the coverage claim did not catch has been fixed** — #308, autocomplete's
`terms.prefix`/`terms.limit`. The lesson stands and is the reason to keep reading the
client rather than the capture: the contract is a floor over what the capture
*recorded*, not over what the client can *send*, and the capture session never typed a
partial word into an autocomplete box. Assume other components have the same blind
spot; the 0b sweep is the tool that finds them (findings 129-135 came from client
source, not from a capture).

Otherwise these issues are the delta between the wire-contract claim and
feature-completeness — PRD §5's v3 and v4 lines, plus the module-side descopes
recorded in `drupal/search_api_wayfinder/README.md`, each of which traces back to a
missing server capability.

## Findings that reshaped the order

**#299 is not blocked by #295** — confirmed by execution: #299 landed first, and its
green suite needed no server change. The server already uses the `{!key=...}` local-params
prefix as the facet response label (#138, `tests/facet_local_params_key.rs`);
`ResponseParser::parseFacets()` simply maps buckets back to deltas by *field name*
(`drupal/search_api_wayfinder/src/ResponseParser.php:107-142`). Emitting
`{!key=<delta>}` from `QueryBuilder::buildFacets()` and matching on the key closes it
with no server work. It moves to wave 1, and it is the smallest edit to the two PHP
files that #296 and #298 both want, so it goes first among them.

**Every fixture-needing server issue wants the same three shared files** —
`solr-ref/capture.sh`, `solr-ref/manifest.tsv`, and `docs/solr-ref-findings.md`. Five
branches each appending a numbered finding and each re-running `capture.sh` is exactly
the churn hazard `CLAUDE.md` warns about, and numbered findings conflict by
construction. That work was hoisted into wave 0 and is done.

**Three more, from the wave 0b source sweep** (findings 129-135):

- **#291 split.** Stock `search_api_autocomplete` uses the `terms` component only, and
  sends `terms.prefix` and `terms.limit` — neither in `TERMS_PARAMS`. That half is a
  live bug and became **#308**, in wave 1. #291 keeps the SuggestComponent, which is
  gated on the `solr_text_suggester` data type (#300) and drops to wave 4.
- **#293 rescoped.** `_version_` is only ever *read*, through a `json.facet`
  `max(_version_)` aggregation on an admin diagnostics screen. Its real dependency is
  JSON facets with aggregation functions and nesting — bigger than the issue assumed,
  and serving nothing a site searches. It leaves wave 1 for wave 4.
- **#292 splits three ways, not two,** and #301 has no server work at all.

## Wave 0 — done

**0a — `capture.sh --only <regex>`** (#306). The one capture run 0a was going to
perform turned out to need the tooling first: `capture.sh` was all-or-nothing, so any
new fixture rewrote all 408 and churned `QTime`/`_version_` into every branch's diff.
`--only` scopes the writes; a filtered run still walks the whole script and still
starts the containers. It also uncovered an SC2318 bug in `capspell` that meant
`main`'s `capture.sh` could not complete a full run at all.

Three constraints that came out of it, binding on every capture in this batch:

1. **The base corpus cannot serve this batch.** Core `content` is 5 docs with `body`
   (text_en) and multiValued `category`. Grouping needs a single-valued non-text field
   (#290 refuses to group on anything else), function queries need numeric fields to
   score on, and spatial needs geo types. Each wants its own container, port and core
   — the pattern already established by `wayfinder-solr-24`/`-25`/`-171`.
2. **Those captures belong in `manifest-errors.tsv`, not `manifest.tsv`.**
   `manifest.tsv` holds core-relative GETs against `content`, replayed verbatim by the
   differential harness. Anything on another core needs a dedicated app in
   `tests/differential.rs` alongside `facets_app`/`keyorder_app`.
3. **Append your block at the end of `capture.sh`** and re-capture with `--only`, so
   concurrent branches merge mechanically and no sibling's fixtures churn.

**0b — one source sweep** (#307). Findings 129-135 in `docs/solr-ref-findings.md`,
each also posted on the issue it affects. Recorded there rather than restated here:
what `{!boost}` actually emits (#289), the six `group.*` params (#290), the terms path
(#291/#308), `facet.heatmap` and `setSpatial()` (#292), `max(_version_)` (#293), the
twelve data types (#300), and the site-hash contract (#301).

## No longer in flight

Both items that gated this batch have landed. **#278** (`&mut Budget` encapsulation,
`a0b808a`) was the blocker on **#294**; #294 now has `src/extract.rs` to itself and is
free to start in wave 2. **#274** (`json.nl` on `/update/extract`) is closed and gated
nothing here.

## Wave 1 — done

All four ran concurrently as planned and merged in this order. Reports are linked, not
restated; each carries its own premise verification and gate evidence.

| Branch | Issue | Merged | Report |
|---|---|---|---|
| A | #297 `/mlt` scoped to the index | `648ba63` (#311) | `docs/reports/2026-08-03-297-mlt-index-scope.md` |
| C | #299 duplicate facets collapse | `8d36fc1` | `docs/reports/2026-08-03-duplicate-facets.md` |
| B | #308 `terms.prefix` / `terms.limit` | `0841712` | `docs/reports/2026-08-03-terms-prefix-limit.md` |
| E | #295 `{!tag}` on `fq`, `{!ex}` on `facet.field`/`facet.query` | `96f03d9` (#314) | `docs/reports/2026-08-03-295-facet-tag-ex.md` |

All 31 wave-1 `EXPECTED_DIVERGENCES` entries from the #310 capture prep are deleted;
`tests/differential.rs` wire-matches all 34 fixtures. Two of the README's eight
descope bullets are gone (MLT scope, duplicate facets); six remain.

**What wave 1 leaves for the branches behind it:**

- **The exclusion machinery is now the facet chain's shared surface.**
  `FacetFieldPlan.ex`, `FacetFieldsPlan.exclusion_active`, and
  `excluded_base_clauses` in `src/facet.rs` are what H (#296) and K (#298) extend
  rather than reimplement. Two ordering facts are load-bearing and fixture-backed
  (finding 140): `facet.mincount`/`facet.missing` run **post-exclusion**, and
  `excluded_base_clauses` assumes `base[0]` is the non-excludable `q` with `base[1..]`
  aligned 1:1 to `fq_tag_lists(params)`. H adds per-field settings *inside* that
  ordering; it does not reorder it.
- **`{!ex=…}` disables the #246 fused aggregation for the whole request.** A named
  ceiling, not a bug (`exclusion_active`'s doc comment). H and K inherit it: a
  per-field or OR-facet request that carries an exclusion takes the unfused path.
- **Type-less local-params blocks are now inert prefixes, not errors.** A block whose
  params are all in `{tag, ex, key}` is stripped by `extract_nested_queries` instead of
  400ing. Typed blocks are unchanged. This is the mechanism L3 (#292's distance facets)
  needs for `{!key=…}{!frange l= u=}geodist()` — but L3 chains an inert block *and* a
  typed one, which no wave-1 fixture exercises. **Verify chained blocks before building
  L3 on the assumption.**
- **`/terms` grew the error-sibling pattern.** `ErrorExtra::terms` + `WfError::with_terms`,
  the analogue of `with_response`, rendering a block next to `error` only when set.
  I (#291) is in the same handler family; extend that pattern rather than hand-building
  an envelope.
- **Two `/terms` premises inverted, and the guard that hid them is gone.** An undefined
  `terms.fl` now answers 200 with an empty list (it was a 400 inferred with no fixture);
  the defined-but-non-text 400 is unchanged. `check_terms_json_nl` was deleted in favour
  of real `json.nl` rendering. Four tests were inverted rather than dropped.
- **`QueryBuilder::indexScopeFilter()` exists** — one helper, both call sites (`build()`
  and `buildMlt()`). G (#290) and L4 (#292) use it rather than open-coding the scope.

The predicted collision points all held: `src/lib.rs` (#308's terms handler vs #295's
`fq` path), the `EXPECTED_DIVERGENCES` block, `README.md`'s bullets, and
`QueryBuilder.php`'s two methods each merged mechanically.
`QueryBuilderTest.php` — the one genuine collision — was avoided by insertion-point
discipline (facet tests beside facet tests, MLT tests beside MLT tests). **Carry that
rule into wave 2**: G and C's successors land in the same two PHP test classes.

## Wave 2 — next, contended on `src/lib.rs`

| Branch | Issue | Owns |
|---|---|---|
| F | #289 function queries | new `src/function_query.rs`, `src/query.rs`, `src/edismax.rs`, the warning sites at `src/lib.rs:2880` |
| G | #290 grouping | `src/collector.rs`, plus the Drupal half (`QueryBuilder`, `ResponseParser`, `WayfinderBackend`) |
| B' | #294 PDF extraction | `src/extract.rs` — fully isolated; **#278 has landed, so this is free** |

Both F and G add to `SELECT_PARAMS`. **Do not prep-land the param names.** An
accepted-but-unimplemented param is precisely the silent-wrong-answer shape #232 exists
to prevent, so each branch adds its own contiguous block in the same commit as its
implementation, and the rebase order is **F -> G** (E has landed). Those conflicts are
mechanical.

A fourth slot is open: **H (#296)** was gated only on E and can run alongside F, G and
B'. It contends with none of them — F and G own `SELECT_PARAMS` and `src/query.rs`, H
owns the facet parser E just changed.

G's Drupal half touches the two files C owns, so C must have landed.

F is the batch's long pole (see the critical path below) — start it as soon as a slot
frees.

Per finding 129, F is a function-query *parser and evaluator* reached through
`{!boost b=...}` on `q`, not a fixed list of functions reached through `bf=`. Its first
targets are `sum`, a bare field reference (`boost_document`), and `payload_score`.

## Wave 3 — disjoint

| Branch | Issue | Note |
|---|---|---|
| H | #296 per-field `f.<field>.facet.*` | **E has landed — startable now**, and can be pulled into wave 2's fourth slot. Extends `FacetFieldPlan`; keeps mincount/missing post-exclusion |
| J | #300 non-default data types | `FieldMapper`, `supportsDataType()`, `presets/search-api.toml`. Ten closed types; `solr_text_custom*` is a named descope (finding 134) |
| M1 | #301 site hash | `DocumentBuilder` only, no server work. **Fetch `search_api_solr`'s `Utility::getSiteHash()` first** — it is not in the three-file snapshot, and bug-compatibility means matching it exactly |

J moves earlier than its original wave-3 slot only in that it now gates #291; the work
itself is unchanged.

## Wave 4

| Branch | Issue | Note |
|---|---|---|
| K | #298 OR facets | E (server) has landed; still after H (client facet code) |
| L | #292 spatial | **three server pieces, not two, plus the Drupal half** — see below |
| M2 | #302 multi-valued text sort | `DocumentBuilder`, after M1. May close as a verified no-op |
| I | #291 suggest / SuggestComponent | after J (#300) — `solr_text_suggester` is the field the suggester is built from. Its own configset and capture prep |
| N | #293 `_version_` via JSON facets | last. Needs `json.facet` with aggregations and nesting; its only client is an admin screen |

**#292's pieces** (finding 133). L1 and L2 each need their own field type and therefore
their own configset:

- **L1 — heatmap.** `rpt` type, `facet.heatmap.*`, the `counts_ints2D` response.
- **L2 — point distance.** `location` type, `sfield`/`pt`/`d`, `{!geofilt}`/`{!bbox}`,
  `{!frange l= u=}geodist()`, `geodist()` in `fl` and in `sort`. Depends on F for the
  function evaluator.
- **L3 — the distance-facet rewrite.** N `facet.query` entries shaped
  `{!key=spatial-<field>__distance-<min>-<max>}{!frange l=<min> u=<max>}geodist()`.
  Depends on L2 and on `facet.query`.
- **L4 — the Drupal half.** `location`/`rpt` Search API types, after J and L2.

## Critical path

- **Longest chain:** `F (#289) -> L2 -> L3 -> L4`. Spatial is the schedule, so
  starting #289 early matters more than its own size suggests. F is behind nothing now
  that 0b has landed.
- **Second chain:** `H (#296) -> K (#298)` — two links left; E is done.
- **Third chain:** `J (#300) -> I (#291)`.
- **Off the path entirely:** #301, #302, #294 (now unblocked). #308, #297 and #299 are
  done.

## Concurrency bound

**Cap at four concurrent branches.** The binding constraints are `src/lib.rs` (six
issues want it) and `drupal/search_api_wayfinder/src/QueryBuilder.php` (five). Beyond
four, branches queue on rebase rather than on work.

## Named contracts

Decisions the siblings would otherwise each invent, which is where cross-branch
conflicts actually survive:

- **Facet response labels are always the Search API delta id, never the field name.**
  Set by C (#299) — now landed, so this is code, not a proposal — and consumed by
  H (#296), K (#298) and L3 (#292's distance facets, whose `{!key=spatial-...}` label
  carries the min and max). Two details C settled that its consumers must preserve:
  a delta failing `[A-Za-z0-9_:-]+` **falls back to the bare mapped field name** (a
  `}` or a space in a delta would break out of the local-params block), and
  `ResponseParser::parseFacets()` registers **both** the delta and the field name as
  keys for every delta, so either shape resolves. Do not "simplify" that to one key.
- **The `{!tag}`/`{!ex}` exclusion machinery in `src/facet.rs` has one implementation.**
  Set by E (#295): `FacetFieldPlan.ex`, `FacetFieldsPlan.exclusion_active`,
  `excluded_base_clauses`. H, K and L3 extend it; none forks a second exclusion path,
  and none reorders mincount/missing out of post-exclusion (finding 140).
- **Index scoping in the Drupal module goes through
  `QueryBuilder::indexScopeFilter()`.** Set by A (#297) so `build()` and `buildMlt()`
  cannot drift; G (#290) and L4 (#292) use it rather than re-emitting `index_id:"..."`.
- **An error carrying a response block next to `error` uses the `ErrorExtra` sibling
  pattern** — `with_response` (#35), now `with_terms` (#308). I (#291) extends it;
  nothing hand-assembles an envelope.
- **The function-query AST lives in `src/function_query.rs`, and F (#289) owns its
  public signature.** L2 (#292) calls it for `geodist()`; it does not fork a second
  evaluator.
- **A new request param goes into `SELECT_PARAMS`/`UPDATE_PARAMS` in the same commit as
  its implementation** — never earlier, per #232. This is what #308 is a live example
  of getting wrong in the other direction: a param the client always sends, absent from
  the list, silently dropped.
- **New captures get their own container, port and core, and land in
  `manifest-errors.tsv` with a dedicated app in `tests/differential.rs`** — the base
  corpus cannot serve this batch (see wave 0a).
- **Re-capture with `capture.sh --only <your fixture prefix>`** so no sibling's
  fixtures churn.
- **Prefer additive interfaces over editing a signature every sibling calls**, per the
  global parallel-batch rule.

## Standing gates

Unchanged from `CLAUDE.md`, restated because this batch will hit all three:

- Rebase onto `main` and re-run the gates locally before every merge. A green branch
  plus a green `main` does not imply a green merge, and these branches contend in
  `src/lib.rs` by design.
- When a feature lands, **delete** its `EXPECTED_DIVERGENCES` entry in
  `tests/differential.rs` rather than leaving it in place.
- Fixtures are ground truth. `--only` removes the need for the old back-up-and-restore
  dance, but not the rule: never re-capture existing fixtures as a side effect, and
  commit new fixtures before any full re-run.
- Findings 129-135 come from reading the client, not from a capture. They are ground
  truth for **scope** only — anything implemented from them still needs a real `solr:9`
  fixture for the response shape.
