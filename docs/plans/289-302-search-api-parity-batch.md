# #289-#302: the remaining Search API parity batch — sequencing plan

**Date:** 2026-08-03. Covers the fourteen issues opened after the parity review of
2026-08-03: #289-#297 (server) and #298-#302 (the `search_api_wayfinder` module),
plus #308, split out of #291 by the wave 0b sweep.

This document is the batch's execution order, not its scope — each issue body carries
its own scope and evidence requirements, and this plan does not restate them. Read it
before starting any branch in the batch, because several of the sequencing constraints
below are invisible from inside a single issue.

**Status:** wave 0 is complete (#306, #307 merged). Wave 1 can start now.

## Where parity actually stands

`tests/search_api_coverage.rs` is green on the full 75/75 captured `search_api_solr`
4.4.0 denominator: every request shape, endpoint, parameter and client-consumed
response field in the capture. A stock Search API site indexes, queries, facets,
highlights, runs MoreLikeThis, and extracts attached and linked files today.

**With one exception the coverage claim does not catch** — see #308 below. The
contract is a floor over what the capture *recorded*, not over what the client can
send, and the capture session never typed a partial word into an autocomplete box.
Worth assuming other components have the same blind spot.

Otherwise these issues are the delta between the wire-contract claim and
feature-completeness — PRD §5's v3 and v4 lines, plus the module-side descopes
recorded in `drupal/search_api_wayfinder/README.md`, each of which traces back to a
missing server capability.

## Findings that reshaped the order

**#299 is not blocked by #295.** The server already uses the `{!key=...}` local-params
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

## In flight, and what it gates

- **#278** (`&mut Budget` encapsulation) is in flight and touches `src/extract.rs`.
  **#294 (PDF extraction) must wait for it** — same file, and #294 is a large rewrite
  in it. #294 therefore moves out of wave 1 into wave 2, where it is still contended
  with nothing else.
- **#274** (bound total resident upload memory) is in flight and shares the capture
  machinery `--only` (#306) now provides. Nothing in this batch depends on it.

## Wave 1 — four branches, no shared decisions

| Branch | Issue | Owns |
|---|---|---|
| A | #297 `/mlt` accepts `fq` | the `MLT_PARAMS` block in `src/lib.rs`, the mlt path in `src/core_index.rs` |
| B | #308 `terms.prefix` / `terms.limit` | `TERMS_PARAMS`, `TERMS_DEFAULT_LIMIT`, the `/terms` handler — touched by nothing else in the batch |
| C | #299 duplicate facets collapse | `QueryBuilder::buildFacets()`, `ResponseParser::parseFacets()` |
| E | #295 `{!tag}` on `fq`, `{!ex}` on `facet.field` | `src/facet.rs`, `src/local_params.rs` |

**#308 first if you only start one.** It is the only item in the batch that is broken
in a stock configuration today rather than merely absent, and it is small.

E is promoted into wave 1: #293 vacated its slot, E heads the four-deep facet chain
(`E -> H -> K`), and its files do not overlap A, B or C.

## Wave 2 — contended on `src/lib.rs`

| Branch | Issue | Owns |
|---|---|---|
| F | #289 function queries | new `src/function_query.rs`, `src/query.rs`, `src/edismax.rs`, the warning sites at `src/lib.rs:2880` |
| G | #290 grouping | `src/collector.rs`, plus the Drupal half (`QueryBuilder`, `ResponseParser`, `WayfinderBackend`) |
| B' | #294 PDF extraction | `src/extract.rs` — fully isolated, but **only after #278 lands** |

Both F and G add to `SELECT_PARAMS`. **Do not prep-land the param names.** An
accepted-but-unimplemented param is precisely the silent-wrong-answer shape #232 exists
to prevent, so each branch adds its own contiguous block in the same commit as its
implementation, and the rebase order is fixed **E -> F -> G**. Those conflicts are
mechanical.

G's Drupal half touches the two files C owns, so C must have landed.

F is the batch's long pole (see the critical path below) — start it as soon as a slot
frees.

Per finding 129, F is a function-query *parser and evaluator* reached through
`{!boost b=...}` on `q`, not a fixed list of functions reached through `bf=`. Its first
targets are `sum`, a bare field reference (`boost_document`), and `payload_score`.

## Wave 3 — disjoint

| Branch | Issue | Note |
|---|---|---|
| H | #296 per-field `f.<field>.facet.*` | **Strictly after E** — same parser, same tests, not parallelisable with it |
| J | #300 non-default data types | `FieldMapper`, `supportsDataType()`, `presets/search-api.toml`. Ten closed types; `solr_text_custom*` is a named descope (finding 134) |
| M1 | #301 site hash | `DocumentBuilder` only, no server work. **Fetch `search_api_solr`'s `Utility::getSiteHash()` first** — it is not in the three-file snapshot, and bug-compatibility means matching it exactly |

J moves earlier than its original wave-3 slot only in that it now gates #291; the work
itself is unchanged.

## Wave 4

| Branch | Issue | Note |
|---|---|---|
| K | #298 OR facets | after E (server) and H (client facet code) |
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
- **Second chain:** `E (#295) -> H (#296) -> K (#298)` — three serial waves, all
  facets, all the same parser. E starts in wave 1 for this reason.
- **Third chain:** `J (#300) -> I (#291)`.
- **Off the path entirely:** #308, #297, #299, #301, #302, #294 (once #278 lands).

## Concurrency bound

**Cap at four concurrent branches.** The binding constraints are `src/lib.rs` (six
issues want it) and `drupal/search_api_wayfinder/src/QueryBuilder.php` (five). Beyond
four, branches queue on rebase rather than on work.

## Named contracts

Decisions the siblings would otherwise each invent, which is where cross-branch
conflicts actually survive:

- **Facet response labels are always the Search API delta id, never the field name.**
  Set by C (#299), consumed by H (#296), K (#298) and L3 (#292's distance facets, whose
  `{!key=spatial-...}` label carries the min and max).
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
