# #332 — `{!geofilt}`/`{!bbox}` filters + `geodist()` in `sort`

**Date:** 2026-08-03. Issue #332 (open), split from #292 (parent, closed). Branch
`markdlabrecque/issue-332-geofilt-bbox-filters` off `main`. Builds on the #331
tracer (PR #344 — `location` field type, `geodist()` in `fl`) and composes with
#333's `{!frange}` (PR #345), both already on `main`.

## What landed

| Site | Change |
|---|---|
| `src/function_query.rs` | `GeoShape` (`Circle`/`Rectangle`), the pure `geo_matches` predicate (haversine ≤ `d` for the circle; `lat ± d/KM_PER_DEG`, `lon ± d/(KM_PER_DEG·cos lat)` for the rectangle), `GeoFilterQuery` — a constant-score Tantivy `Query` over #331's two synthetic columns, `exists`-gated so a doc with no point never matches. `FunctionColumns` opens a `FuncQuery`'s columns once for per-doc evaluation outside a scorer (the sort path). Hoisted `EARTH_RADIUS_KM` so the haversine and the bbox bounds share one constant. |
| `src/core_index.rs` | `parse_function_query_q` gains `Option<&Params>` and `geofilt`/`bbox` arms (position-0, like `{!func}`/`{!frange}`); `resolve_geo_filter` resolves `sfield`/`pt`/`d` from the block's local params with request-param fallback (Solr's QParser), validating `sfield` is a declared `location` field. The nested call from `parse_query` passes `None` — `{!geofilt}` is a top-level filter, never nested. |
| `src/collector.rs` | `SortKey::Function(FuncQuery)` + `SegmentSortColumn::Function` — the collector evaluates the function per doc via `FunctionColumns`; a doc whose function does not exist (no `location` point) sorts last. Dropped the unused `Eq` derive on `SortKey`/`SortClause` (a `FuncQuery` carries `f64`). |
| `src/lib.rs` | `d` joins `sfield`/`pt` in `SELECT_PARAMS`; the `q` and `fq` parse sites pass `Some(&params)` to the dispatcher (an `fq` pre-check, mirroring `q`, so `{!geofilt}` as `fq` resolves its params); `geodist_sort_func` resolves the argless `geodist()` a `sort=geodist() asc` clause ranks by. |
| `solr-ref/capture.sh` | 4 new `geo_*` rows (`{!geofilt}`, `{!bbox}`, a tight-boundary `{!geofilt d=70}`, `sort=geodist() asc`). **Incidental fix:** `want_any` now also compares the `--only` and block patterns as string prefixes — their leading carets defeated `[[ =~ ]]` for the exact-match case, so `capture.sh --only '^geo_'` was skipping the geo *setup* block and the captures then ran against a container that was never started (exit 7). |
| `tests/spatial.rs` (new) | Hermetic coverage no fixture exercises: `{!geofilt}`/`{!bbox}` excluding a doc with no `location` point (the `exists` guard), and a block's local params overriding the request params. |
| `docs/solr-ref-findings.md` | Finding 158: `{!bbox}` is the lat/lon rectangle of the `{!geofilt}` circle. |

## Wire-compat evidence

Four `geo_*` rows captured against real `solr:9`, run hermetically against the
`geo_app` from #331 (unchanged 7-doc NYC grid, no corpus change → #331's two
`geo_geodist_fl` fixtures are untouched):

- `geo_geofilt` — `fq={!geofilt}&sfield=loc&pt=40,-74&d=130` → 6 docs (g6 at
  ~140 km excluded).
- `geo_bbox` — `fq={!bbox}&sfield=loc&pt=40,-74&d=130` → all 7 (g6 inside the
  rectangle, outside the circle — the one doc `{!bbox}` returns that
  `{!geofilt}` does not).
- `geo_geofilt_tight` — `d=70` → g1 and g7 (~69.94 km, just inside the circle).
- `geo_geodist_sort` — `sort=geodist() asc` → ascending distance, ties
  (g3/g5, g2/g4 are equidistant E/W) broken by ascending doc id, matching Solr.

All six `geo_*` rows pass the differential harness with 0 diffs (distances at
the #331 meter-granularity tolerance; doc sets and order exact).

## Circle-vs-square design

The 7-doc grid already separates at `d=130`: g6 `(41,-73)` is ~140 km from the
origin — outside the circle, but inside its bounding rectangle (lat `41 ≤
40+1.17`, lon `-73 ∈ [-75.53,-72.47]`). `d=130` was chosen as the smallest
radius that pulls g2/g4 (~111 km) into the circle while keeping g6 out, so the
circle holds 6 and the rectangle holds 7. No new doc was needed, so #331's
captured fixtures did not need re-capture (QTime/`_version_` churn avoided).

## Mutation testing

Three mutations, each caught:

1. Invert the circle predicate (`≤` → `>`) → `geo_matches_*` unit test and the
   `geo_geofilt` differential row both fail.
2. Swap the `geofilt`/`bbox` shape dispatch → `geo_geofilt` and `geo_bbox`
   differential rows both flip (6↔7).
3. Drop the scorer's `exists` guard → `geofilt_excludes_a_doc_with_no_location_point`
   fails (a doc with no point reads back as the origin and wrongly matches).

## Descopes (named)

- `facet.query={!geofilt}`/`{!bbox}`: out of scope (the issue is `q`/`fq` only).
  A `facet.query` routes through `parse_query`, whose dispatcher gets `None`
  params, so a `{!geofilt}` there 400s on its missing params. The Drupal
  distance-facet rewrite routes through `{!frange}geodist()` (#333, already
  landed), not `{!geofilt}`, so this is not a client gap.
- Missing-/bad-`sfield`/`pt`/`d` 400s carry no fixture (the capture always sends
  all three); they are the correct 400 rather than a panic or a silent
  match-all.
- Polar `pt` (where `cos lat → 0`) degenerates the rectangle to "every
  longitude" — site-scale, no polar data (ponytail).
- `geodist()` sort on a doc with no point sorts last (`None`); unfixtured.

## Commands

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test                      # 1214 passed
```

Live capture (own `solr:9` container, not part of CI; `SOLR_PORT` shifts off
8983 if a DDEV router holds it):

```
SOLR_PORT=9090 bash solr-ref/capture.sh --only '^geo_'
```
