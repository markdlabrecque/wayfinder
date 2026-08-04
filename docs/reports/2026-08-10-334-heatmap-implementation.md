# Issue #334 — `facet.heatmap` over `location_rpt` (implementation)

Implements Solr's `facet.heatmap` facet over `location`/`location_rpt` fields.
Builds on the shared two-column point encoding (#331) and the captured-Solr
grid model (finding 159). All 15 captured fixtures reproduce byte-for-byte;
the differential manifest-errors runner covers the four 400 cases.

## What changed

**Encoding (shared with #331, replicated here so #334 is self-contained).**
`location` and `location_rpt` both resolve to a new `ValueKind::Location`,
stored as two synthetic f64 fast columns `<field>__lat`/`<field>__lon`
(`src/schema.rs`), reachable only via `WayfinderSchema::location_fields`. A
dynamic `rpts_*`/`locs_*` field resolves to `Location` too but has no columns
(the name is unknown at build time), so the heatmap (and `geodist()`) require a
*declared static* field — a documented ceiling, consistent across #331/#334.
`add_values` parses the `lat,lon` point form (`as_location`, `src/core_index.rs`).

**Grid math (`src/heatmap.rs`, new).** Verified cell-for-cell against every
fixture: geohash tree, `gridLevel` = char count, `columns = 2^ceil(5L/2)`,
`rows = 2^floor(5L/2)`; right-closed longitude cells, north-anchored rows;
bounded `geom` subsets the world grid snapped out to cell edges. Default level
is `distErrPct`-derived (`0.1 * diagonal`); `maxCells` is a pure ceiling guard
(`32^L > maxCells → 400`, never lowering the level — finding 159). A
`HeatmapCollector` reads the `__lat`/`__lon` columns per matching doc and
tallies cells (site-scale O(N) pass — Tantivy has no spatial term index, the
ponytail the sizing report names).

**Rectangle `fq`.** `rpts_geo:["minLon minLat" TO "maxLon maxLat"]` is
intercepted in `parse_query` (`src/core_index.rs`) and built as the
intersection of two inclusive `RangeQuery`s over the columns (the columns are
indexed, so no custom Scorer is needed).

**Wiring.** `facet_heatmaps` replaces the `json!({})` stub in
`facet_counts_inner`; its errors are post-query (the base query already ran),
so they get the `response` block attached like `facet.field`/`facet.query`
errors, matching `heatmap_unknown_field.json`. `facet.heatmap{,gridLevel,geom,
maxCells,distErrPct,distErr,format}` join `SELECT_PARAMS` (`src/lib.rs`).
`rpts_*`/`rptm_*` dynamic rules join the Search API preset.

**Tests.** 7 grid-math unit tests (`src/heatmap.rs`); a differential
`heatmap_grid_outputs_match_captured_fixtures` test reproducing all ten 200
fixtures' `facet_heatmaps` blocks + the `facet=true`-gate no-op; four
manifest-errors rows run through the hermetic runner via a new `heatmap_app`
fixture core + dispatch arm. The `field_class_for_builtin` catch-all no longer
mis-reports the spatial types as `TextField`. Schema-layer tripwires pin the
two-column encoding and add `location`/`location_rpt` to the reserved-name set.

## Evidence

- `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean.
- `cargo test` all green.
- Mutation: flipping the right-closed-longitude cell formula (the `ceil(...)-1`
  index) fails `world_grid_l1_extents...`; disabling the `maxCells` guard fails
  the manifest-errors 400 cases. Both reverted.
- No `EXPECTED_DIVERGENCES` entries: every heatmap fixture matches cleanly
  (`normalize` strips `error.msg`/`metadata`, so the Java-class-name messages
  Solr emits for the non-spatial/maxCells cases are not compared — only the
  400 status + `error.code`).

## Coordination with #331

#331 owns the shared `ValueKind::Location` encoding (it has an open PR, #344).
#334 replicates the *storage* encoding verbatim (not geodist/function-query)
and resolves `location_rpt` to the same variant, so the PR is self-contained.
When #331 lands first: the `location` arm is identical (rebase keeps main's),
`location_rpt` is net-new, and the duplicate `__lat`/`__lon` storage lines
drop out mechanically. Finding numbers are stable ids: #331 is 157, #332
(`{!bbox}`) independently grabbed 158, so #334's heatmap finding is **159**
(renumbered on rebase once #332 landed — no collision).

## Descopes (documented, guard-backed)

- `facet.heatmap.format=png` is accepted but emits `ints2D` (Solr's png is a
  base64 blob over a different numeric encoding — no fixture, descope).
- `facet.heatmap.distErr` is accepted but ignored (the level stays
  distErrPct-derived); a descope, safe because it only affects the default level.
- WKT/circle geoms return a clear error naming the supported rectangle form.
- An explicit level that overflows `maxCells` 400s (finding 159: maxCells never
  lowers the level); a malformed `maxCells`/`distErrPct`/`gridLevel` 400s too.
- Per-field heatmap params via `{!key=…}` local params are not supported (the
  fixtures use global params).
