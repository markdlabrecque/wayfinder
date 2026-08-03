# #289-#302: the remaining Search API parity batch — sequencing plan

**Date:** 2026-08-03. Covers the fourteen issues opened after the parity review of
2026-08-03: #289-#297 (server) and #298-#302 (the `search_api_wayfinder` module),
plus #308, split out of #291 by the wave 0b sweep. **Updated 2026-08-07** for the
completion of wave 2.

This document is the batch's execution order, not its scope — each issue body carries
its own scope and evidence requirements, and this plan does not restate them. Read it
before starting any branch in the batch, because several of the sequencing constraints
below are invisible from inside a single issue.

**Status:** waves 0, 1 and 2 are complete — #306, #307, #310; #297, #299, #308, #295;
#298, #289, #294, #290. **Wave 3 is current: #296 and #300** — #301 is closed
documentation-only (one core per site, see wave 3). #300 is free of open questions;
#296 needs its premise settled with fixtures first.

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

## Wave 2 — done

Four branches, all merged. The `SELECT_PARAMS` discipline held: each branch added its
own block in the same commit as its implementation, and #290 went further by leaving
`group.format`/`group.main` **out** of the list so they 400 under `strict_params`
rather than being accepted-and-ignored — the #232 shape, pinned by two tests.

| Branch | Issue | Merged | Report |
|---|---|---|---|
| K | #298 OR facets | `d4cd89e` (#317) | `docs/reports/2026-08-03-298-or-facets.md` |
| F | #289 function queries | `753e1d8` | `docs/reports/2026-08-04-289-function-queries.md` |
| B' | #294 PDF extraction | `f7ecec6` (#318) | `docs/reports/2026-08-07-pdf-extraction-implementation.md` |
| G | #290 result grouping | `7760e4c` (#321) | `docs/reports/2026-08-07-290-result-grouping.md` |

**What wave 2 leaves for the branches behind it:**

- **`src/function_query.rs` exists and is the one evaluator.** Parser, AST and a
  bespoke Tantivy `FunctionScoreQuery`, covering constants, numeric field references and
  `sum`/`product`/`max`/`min`/`recip`, with a missing value resolving to `0.0` (Solr's
  default). L2 (#292) extends it for `geodist()`; it does not fork a second evaluator.
  `ms`/`rord` were split off as a follow-up increment and need date/ordinal field types.
- **Grouping returns `grouped` alone.** A request setting both `group=true` and
  `facet=true` gets no `facet_counts` block, and `group.truncate`/`group.facet` are
  no-ops with a `ponytail:` naming the ceiling. Correct for every captured request, but
  unfixtured — see H's requirement 6 below.
- **`QueryBuilder.php` and `ResponseParser.php` now carry a grouping half too**, on top
  of the facet halves from C and K. They are the module's two hot files; every remaining
  Drupal branch touches at least one.
- **`getSupportedFeatures()` was designed out as a collision.** K converted it to one
  feature per line, and G's `search_api_grouping` then landed on its own line with no
  conflict — the prediction and the remedy both held, which is the pattern worth
  repeating on the next shared list.

**K (#298) took the fourth slot ahead of H (#296).** The plan's old
`E -> H -> K` ordering was file contention in `buildFacets()`, not a dependency: OR
facets need *tagging*, which E shipped, not per-field settings. K was PHP-only plus one
hermetic parser pin, needed no capture, and closed the most user-visible bug in the
module.

What K settled, for the branches that follow it:

- **The tag string is `facet:<search_api_field_id>`**, built from the Search API field
  id and not the mapped Solr field, because the `facets` module puts that same string on
  the `fq` (`SearchApiSolrBackend.php:3928-3935`). It carries a colon;
  `local_params::read_value` keeps a colon in a bare value, now pinned by
  `bare_local_param_value_keeps_its_colon` and mutation-tested.
- **`{!ex=…}` precedes `{!key=…}`** in the `facet.field` prefix, matching
  `facet_extag_both_facets`. A delta failing `[A-Za-z0-9_:-]+` drops the `key` half only;
  the `ex` half is unaffected because it is built from the field id, not the delta.
- **`getSupportedFeatures()` is one feature per line now**, so G (#290) adds
  `search_api_grouping` on its own line and merges mechanically. The collision the plan
  predicted was designed out rather than sequenced around.
- **Condition-group tags reach the wire through `QueryBuilder::tagFilterQuery()`**, which
  prefixes `{!tag=…}` on the `fq` of a *top-level* tagged group. Two ceilings came out of
  the review and are not yet guarded, both silent when hit: an arbitrary tag value is
  interpolated without the safe-character check the sibling delta path applies, and a
  tagged group nested below the top level — or any group under an OR root — loses its
  tag, dropping the facet back to filtered counts with no signal. See the follow-up note
  under wave 3.

## Wave 3 — current, disjoint

| Branch | Issue | Note |
|---|---|---|
| H | #296 per-field `f.<field>.facet.*` | Unblocked by E, but **its scope is in question — settle the premise below before starting.** Extends `FacetFieldPlan`; keeps mincount/missing post-exclusion |
| J | #300 non-default data types | `FieldMapper`, `supportsDataType()`, `presets/search-api.toml`. Ten closed types; `solr_text_custom*` is a named descope (finding 134) |
| M1 | #301 site hash | **Closed documentation-only** — the hash is not being built; one core per site is the supported topology. README + `DocumentBuilder` `ponytail:`. See below |

J moves earlier than its original wave-3 slot only in that it now gates #291; the work
itself is unchanged. It is the only one of the three that is startable with no open
question in front of it.

### M1 (#301) — decided: one core per site

The hash is **not** being ported, so `Utility::getSiteHash()` no longer needs fetching
and the ticket's first bullet ("decide whether to support it at all") is answered. The
restriction is provisional and matches what the server already enforces: PRD open
question 1 is resolved as **single-core-per-process** (`docs/PRD.md:1114-1121`,
enforced at `src/lib.rs:1002`), so several sites on one host is several Wayfinder
processes, one core each — available today, no work. One *process* serving several
cores is what would reopen that PRD line, and nothing here needs it.

The remaining sub-choice is settled: **document-only, no guard, no follow-up issue** —
the marker-document check was weighed against a use case nobody could name. So the
restriction ships as documentation and the residual risk is accepted explicitly: two
sites pointed at one core overwrite each other silently, and nothing detects it.

Closed by the README "Not supported" rewrite plus the `DocumentBuilder` `ponytail:`
comment, both of which now state one-core-per-site as a decision rather than a
simplification awaiting work. No expiry guard, because nothing here expires — this is
a supported-topology statement, not a deferred task.

### H (#296) — requirements, updated after #298 landed

**1. The premise is settled — captured, and it went the other way.** 26 fixtures
(`facet_perfield_*` on `content`, `pf296_sort_*` on a dedicated core), findings 147-150:

- `f.<X>.facet.*` resolves `X` against the **field name**, never against the `{!key=}`
  label. `{!key=cat}category` + `f.category.facet.limit=1` limits; `f.cat.facet.limit=1`
  does nothing (finding 147). So `f.<field>.facet.*` alone does **not** fix the case the
  README bullet describes.
- The mechanism that does is **facet settings carried as local params on `facet.field`**:
  `{!key=cat facet.limit=1}category`, and likewise for `mincount`/`missing`/`sort`, with
  or without a `key` (finding 148). Two facets on one field with different settings are
  expressible this way and no other (finding 149).
- `facet.limit` joins `mincount`/`missing` as post-exclusion (finding 150), so an OR
  facet's limit truncates the wider list, not the filtered one.
- Wayfinder already implements one per-field setting — `f.<field>.facet.missing`
  (issue #140) — keyed by field name, which is the same resolution rule. The real gap is
  `limit`/`mincount`/`sort`, plus the whole local-param form.

So #296 is **two** pieces of work, not one: `f.<field>.facet.{limit,mincount,sort}` on
the server, and local-param facet settings on `facet.field` — and it is the second that
the module actually needs, because #299 keys facets by the delta. The README bullet at
`drupal/search_api_wayfinder/README.md` still claims there is no `f.<field>.facet.*`
override at all; correct it when this lands.

This is the same shape as the #297 and #308 premise corrections — the third and fourth
times a ticket's stated premise did not survive contact with a fixture.

**2. The capture needs an exclusion row, not just precedence rows.** #296 already
required fixtures for global-set / per-field-set / both-set / per-field-on-an-unfaceted-
field. Since E and K landed, add **an OR facet carrying a per-field setting** —
`{!ex=cat}` plus a per-field `limit` on the same facet. Finding 140 pins that
`facet.mincount`/`facet.missing` apply *post*-exclusion; whether a per-field `limit`
does the same is unverified, and it is exactly the combination a real facet block
produces.

**3. `buildFacets()` now has one prefix-construction site — extend it, don't add a
second.** K rewrote the `facet.field` prefix into a single `$prefix` string assembled
from `ex=` then `key=`. Per-field settings either become more params in that same block
(if the premise resolves that way) or stay separate `f.<field>.facet.*` params, but
either way the prefix is built in one place. Preserve both invariants K established:
`ex` precedes `key`, and a hostile delta drops the `key` half while keeping `ex`.

**4. The rebase conflict is known and localised.** K's report predicts it: whichever of
#296/#298 landed second needs a local resolution inside `buildFacets()`. #296 is the one
landing second, and the conflict is the ~10-line `$prefix` block, not the method.

**5. Its capture session is the cheapest chance at the grouping+facet gap.** G (#290)
left `group=true` + `facet=true` unfixtured, with `group.truncate`/`group.facet` as
documented no-ops. H is the next branch to stand up a Solr container for facet work, so
adding those rows costs a capture block rather than a whole session. Optional, and
strictly a capture — implementing collapsed-group facet counts is #290's follow-up, not
H's.

**6. Do not fix the two open `tagFilterQuery()` ceilings here.** The #298 review found
an unguarded tag value and tag loss below the top level; both belong in their own issue
against `build()`, not folded into a facet-settings branch.

H is also **the only near-term branch that needs `capture.sh`**, so it can be scheduled
purely on capture contention: keep it clear of L1, L2 and I, which each need their own
configset and capture block.

## Wave 4

| Branch | Issue | Note |
|---|---|---|
| L | #292 spatial | **three server pieces, not two, plus the Drupal half** — see below |
| M2 | #302 multi-valued text sort | `DocumentBuilder`, after M1. May close as a verified no-op |
| I | #291 suggest / SuggestComponent | after J (#300) — `solr_text_suggester` is the field the suggester is built from. Its own configset and capture prep |
| N | #293 `_version_` via JSON facets | last. Needs `json.facet` with aggregations and nesting; its only client is an admin screen |

**#292's pieces** (finding 133). L1 and L2 each need their own field type and therefore
their own configset:

- **L1 — heatmap.** `rpt` type, `facet.heatmap.*`, the `counts_ints2D` response.
- **L2 — point distance.** `location` type, `sfield`/`pt`/`d`, `{!geofilt}`/`{!bbox}`,
  `{!frange l= u=}geodist()`, `geodist()` in `fl` and in `sort`. **F has landed**, so the
  evaluator it depends on exists: extend `src/function_query.rs` with `geodist()` rather
  than forking a second one.
- **L3 — the distance-facet rewrite.** N `facet.query` entries shaped
  `{!key=spatial-<field>__distance-<min>-<max>}{!frange l=<min> u=<max>}geodist()`.
  Depends on L2 and on `facet.query`.
- **L4 — the Drupal half.** `location`/`rpt` Search API types, after J and L2.

## Critical path

- **Longest chain:** `F (#289) -> L2 -> L3 -> L4`, and **F has landed** — the head of
  the batch's longest chain is off the board. Spatial (#292) is now the schedule on its
  own, and L2 is startable whenever a slot frees; it does not wait on wave 3.
- **Second chain: dissolved.** It was `E -> H -> K`; E landed, and K turned out not to
  depend on H, so both are now single branches rather than a chain. Nothing behind
  them.
- **Third chain:** `J (#300) -> I (#291)`.
- **Off the path entirely:** #301, #302. Everything else off the path — #308, #297,
  #299, #294, #298 — is done.

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
- **The OR-facet tag string is `facet:<search_api_field_name>`.** Not our choice — the
  facets module defines it and `search_api_solr` reproduces it verbatim. Set by K
  (#298), and the label L3 (#292's distance facets) must not collide with.
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
