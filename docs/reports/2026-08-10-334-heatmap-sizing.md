# #334 — spatial heatmap facets: sizing, encoding decision, extension map

**Date:** 2026-08-10. Issue #334 (open). Branch
`markdlabrecque/issue-334-heatmap-facets-location` off `main`. Parent #292
(sized, closed). This report resolves the feasibility/encoding gate the issue
body names — *"the encoding (geohash/Morton `i64` for prefix-tree grid
bucketing) should be decided in this ticket's own sizing"* — and maps the wire
surface and extension points. **It implements no feature.** Fixtures and TDD
land with the implementation (§"Recommended next step").

Sister slices from the same parent, all open and all listed here only to draw
the boundary: #331 (`location` field + `geodist()`), #332 (`{!geofilt}`/`{!bbox}`
+ `geodist()` sort), #333 (`{!frange}` function-range filter + distance-facet
rewrite). #334 is feature-independent of all three (different field type,
`location_rpt`), but shares one storage contract with #331 — see §"Sequencing
with #331".

## Premise correction — the issue's encoding suggestion does not hold on Tantivy

The issue (and #292's sizing, in passing) frames Morton/geohash `i64` as the
encoding that "actually pays off" here because *"the heatmap is the one place
the locality/grid structure actually pays off."* That framing is worth
challenging before committing to it, per CLAUDE.md's "don't paper over a wrong
ticket premise." **It does not hold**, for a precise reason:

A prefix tree serves two roles in Solr, and only one of them is the "grid":

1. **Index acceleration of spatial predicates and counting.** Lucene-spatial
   indexes the prefix-tree *cells as terms*. A spatial query becomes a term
   enumeration; and — the part that matters for the heatmap —
   `HeatmapFacetCounter` counts docs-per-cell by *walking those cell terms and
   reading their document frequencies*, not by scanning every document. The
   locality (Morton/geohash interleaving) is what makes that term walk prune
   whole subtrees outside the query geometry. **This is the only place a Morton
   code buys anything**, and it requires a spatial term index.
2. **Defining the grid geometry.** The prefix tree determines, for a given
   level, how the world is subdivided into the `columns × rows` grid the
   heatmap emits.

Tantivy 0.26 ships **no spatial term index** (no geo / hilbert / s2 / h3; this
is the same fact #292's sizing confirmed against `Cargo.lock`). So role (1) is
unavailable to Wayfinder regardless of encoding: a heatmap here must
**scan the filtered doc set and bucket each point into a cell by arithmetic**
(O(N)), exactly as #292's point-distance filters scan. And role (2) — the grid
geometry — is *pure arithmetic over lat/lon*; it does not require the point to
be *stored* as a prefix-tree code. The two roles are separable, and Solr only
couples them because it exploits the term index. Wayfinder has no term index to
exploit, so it decouples them.

The decisive observation is that **both candidate prefix trees produce a
uniform rectangular grid at a given level**:

- **Geohash tree** (`geo=true` default for `SpatialRecursivePrefixTreeFieldType`,
  configset has no explicit `prefixTree`): each axis is halved independently per
  its bit. At level `L` (bits), longitude carries `ceil(L/2)` bits and latitude
  `floor(L/2)` → `columns = 2^ceil(L/2)`, `rows = 2^floor(L/2)`, each axis
  uniformly subdivided over `[-180,180] × [-90,90]`.
- **Quad tree**: each level halves both axes → `columns = rows = 2^L`, again
  uniform.

A uniform rectangular grid means bucketing a point is
`col = floor((lon − minX) / cellW)`, `row = floor((lat − minY) / cellH)` —
**arithmetic on the point's lat/lon, with no dependence on how the point is
stored.** A Morton/geohash `i64` storage encoding would add an encoder on the
way in, a decoder (or a parallel `floor`-on-the-code) on the way out, and
**rounding** (geohash `i64` quantizes; two `f64` columns do not) — and it would
buy nothing, because the thing it exists to accelerate (the term-index cell
walk) is not available.

**Decision: store an `rpts_*`/`rptm_*` point as two synthetic `f64` fast columns
— `<field>__lat` / `<field>__lon` — exactly #292/#331's point-distance encoding
— and compute the heatmap grid by arithmetic over those columns, replicating
Solr's prefix-tree subdivision and level-selection as pure math.**

This reverses the encoding the issue expected, but on every axis it is
strictly better here:

| Axis | Morton/geohash `i64` | Two `f64` columns |
|---|---|---|
| Precision | quantized (rounding to justify vs. fixtures) | exact, full precision |
| Matches Solr counts | only via exact grid math | only via exact grid math (same requirement) |
| Acceleration available | **none** — no spatial term index in Tantivy | none — O(N) scan (same) |
| New code | encoder + decoder + rounding model | none — reuses #331's machinery |
| Site-scale cost | O(N) scan-bucket anyway | O(N) scan-bucket |

The grid-math requirement is *identical* under either encoding; storage choice
changes nothing about correctness and only adds cost on the Morton side. So the
prefix tree survives in this ticket — **as grid math, not as a storage
encoding** — and that is the precise separation to keep in the implementation.

## What the grid math must replicate (the real risk; fixtures are ground truth)

The encoding is settled; the `columns`/`rows`/`minX`/`maxX`/`minY`/`maxY`
semantics and the level-selection algorithm are **not** guessable and must be
pinned from captured `solr:9` fixtures (PRD: fixtures are ground truth, never
the implementation's output). The quantities to derive, with the a-priori
expectation the fixtures will confirm or correct:

1. **Prefix-tree type.** The configset's `location_rpt` declares no
   `prefixTree`/`prefix`, so it is Solr's default for
   `SpatialRecursivePrefixTreeFieldType` with `geo=true`. Expectation: **geohash**.
   Fixture tell: at a fixed `gridLevel`, a geohash grid has
   `columns ≈ 2 × rows` (lon carries the extra bit at odd levels); a quad grid
   is square. The captured `columns`/`rows` settles it.
2. **World bounds.** For `geo=true`, `[-180,180] × [-90,90]`.
3. **`columns`/`rows` as a function of level.** Expectation: geohash
   `columns = 2^ceil(L/2)`, `rows = 2^floor(L/2)`; quad `columns = rows = 2^L`.
4. **`maxCells` default and level selection.** Solr default `maxCells = 100000`.
   Selection: the highest level `L` such that the grid covering `geom` has
   `columns × rows ≤ maxCells`. When `gridLevel` is supplied, it is used
   directly (clamped to the tree's `maxLevels`).
5. **Grid extent (`minX`/`maxX`/`minY`/`maxY`) for a bounded `geom`.** Open
   question the fixture must settle: does Solr report `geom`'s raw bounds, or
   the cell-aligned bounds of the cells overlapping `geom`? The
   default-`geom` (whole-world) case is unambiguous (the world bounds are
   exactly cell-aligned at every level). The bounded case needs a fixture.
6. **`counts_ints2D` shape.** `int[rows][columns]`; expectation (to confirm):
   rows whose counts are all zero are emitted as JSON `null` to save space.
7. **`format` param.** Default `ints2D`; other values (`png`) are emitted by
   Solr but have no client evidence (finding 133's extraction sums
   `counts_ints2D`). Descope non-`ints2D` formats with a guard.
8. **Precision/indexing params (`distErrPct`, `maxDistErr`).** These configure
   the RPT's *indexing* precision and default grid level in Solr. Under
   Wayfinder's scan-bucket they have no storage effect (points are full
   precision) and only influence default level selection when `gridLevel` and
   `maxCells` are both absent — a case the client path does not exercise.
   Accept them in the type definition (config parity) but treat as no-ops;
   guard with a test asserting full-precision storage.

## Wire surface (fixed by Solr; listed for the split)

From finding 133 and the configset
(`solr-ref/search-api/configset/schema.xml:240-241, 435-438`):

- **Field type.** `location_rpt` (`solr.SpatialRecursivePrefixTreeFieldType`,
  `geo=true`, `distErrPct=0.025`, `maxDistErr=0.001`,
  `distanceUnits=kilometers`); dynamic rules `rpts_*` (single) / `rptm_*`
  (multi).
- **Request params** (all `facet.heatmap.*`): `facet.heatmap=<field>`;
  `facet.heatmap.geom` (default `["-180 -90" TO "180 90"]`); `.format`
  (default `ints2D`); `.maxCells` (default `100000`); `.gridLevel`.
- **Filter.** An `fq` of `<field>:<geom>` constraining the doc set to the
  region. The client-evidenced `geom` is a rectangle
  `["minLon minLat" TO "maxLon maxLat"]`; WKT/CIRCLE/ENVELOPE shapes and
  `Intersects`/`IsWithin`/`Contains` operators have no client evidence (see
  descopes).
- **Response.** `facet_counts.facet_heatmaps.<field>` (the literal field name,
  e.g. `rpts_geo`) → `{ gridLevel, columns, rows, minX, maxX, minY, maxY,
  counts_ints2D }`. The client sums `counts_ints2D`
  (`SearchApiSolrBackend.php:3263-3286`, finding 133). When `facet.heatmap` is
  absent, `facet_heatmaps` stays `{}` — the placeholder `src/facet.rs:201`
  already emits.

## Codebase extension points

| Site | Change |
|---|---|
| `src/schema.rs` | `ValueKind::Location` + `ResolvedType::Location`; `"location"` **and** `"location_rpt"` arms in `resolve_type` (both resolve to the same point ValueKind — see §"Sequencing with #331"); add both names to `NON_LANGUAGE_BUILTIN_TYPES`. Build path creates two synthetic `f64` fast fields per such field + a `location_fields(name) -> Option<(Field, Field)>` accessor. |
| `tests/schema_layer.rs` | The `location` unresolvable tripwire (`src/schema.rs`'s deliberate guard) flips to resolvable; add a `location_rpt` resolvable assertion. *(Shared with #331 — coordinate; whoever lands first owns the flip.)* |
| `src/core_index.rs::add_values` | A `ValueKind::Location` arm parsing Solr's `"lat,lon"` JSON form into the two columns. Non-point RPT values (WKT/polygon) → 400 (point-only descope guard). |
| `src/facet.rs` | A `facet_heatmaps(index, params, base)` producing the per-field grid objects; the `facet_heatmaps` entry in `facet_counts` (line ~201) becomes the computed map when `facet.heatmap` is present, `{}` otherwise. Grid math (§ above) lives here. |
| `src/lib.rs` `SELECT_PARAMS` | `facet.heatmap`, `facet.heatmap.geom`, `facet.heatmap.format`, `facet.heatmap.maxCells`, `facet.heatmap.gridLevel` — else `strict_params = true` 400s them. |
| q/fq parsing (`src/core_index.rs::parse_query`, `src/local_params.rs`) | The `<field>:<geom>` `fq` as a **rectangle-containment** predicate over the two-column point (`minLon ≤ lon ≤ maxLon && minLat ≤ lat ≤ maxLat`). The `["minLon minLat" TO "maxLon maxLat"]` form only; other shapes/ops → 400 (guard). |
| `presets/search-api.toml` | `rpts_*` / `rptm_*` dynamic rules, `type = "location_rpt"`, `stored = true`, fast (the scan needs the columns). |
| `solr-ref/capture.sh` | Append a `heatmap` block **at the end** (concurrent-branch rule): own core, an `rpts_geo` field, ~12 docs at known lat/lon chosen so cells are individually distinguishable, plus the edge cases that pin the grid math — a point on a cell boundary, a point outside `geom`, and a bounded-`geom` request to settle item 5. |
| `tests/differential.rs` | Red `heatmap_*` rows in `manifest.tsv` + matching `EXPECTED_DIVERGENCES` entries; **delete** those entries on green (per the compatibility contract). |

## Descopes (each with a failing-when-reason-stops guard)

- **Non-`ints2D` `facet.heatmap.format`** (`png`, …): no client evidence. 400 on
  an unknown format; guard test.
- **Non-point `rpts_*` values** (WKT `POLYGON`/`POINT(…)`, etc.): the client
  writes `"lat,lon"` points only. 400; guard test.
- **Non-rectangle `<field>:<geom>` `fq` shapes/operators** (WKT, `CIRCLE`,
  `ENVELOPE`, `Intersects`/`IsWithin`/`Contains`, `[..]` beyond a 2-corner
  rectangle): no client evidence. 400; guard test.
- **RPT precision params** (`distErrPct`, `maxDistErr`) as actual indexing
  behaviour: no-ops under scan-bucket (full-precision storage). Guard: a test
  that a value indexed under an `rpts_*` field round-trips at full `f64`
  precision regardless of those params.
- **`location_rpt` as a `geodist()`/`{!geofilt}` field.** Those belong to
  #331/#332's `location` type. `location_rpt` is heatmap-only in this slice; a
  `geodist()`/`{!geofilt}` against an `rpts_*` field is out of scope (note, not
  necessarily a hard 400 — coordinate with #331/#332 if they want `location_rpt`
  to share the distance path later).

## Sequencing with #331

#334 is *feature*-independent of #331, but the two **share the point-storage
contract**: both store a lat/lon point as two synthetic `f64` fast columns. To
avoid forking that contract across the merge (both touch the `src/schema.rs` /
`src/core_index.rs` hot files), coordinate on a single owner:

- The shared pieces are `ValueKind::Location`, `ResolvedType::Location`, the
  `"location"`/`"location_rpt"` arms in `resolve_type`, the two-column build
  path, `location_fields(name)`, and the `"lat,lon"` parse in `add_values`.
- **If #331 lands first** (the #292 sizing's "292a first" sequencing): #334
  reuses all of the above verbatim and only adds the `"location_rpt"` type arm,
  the `rpts_*`/`rptm_*` rules, and the heatmap computation.
- **If #334 lands first:** #334 creates the shared encoding (resolving both
  `"location"` and `"location_rpt"` to `ValueKind::Location`); #331 then adopts
  it and drops its own copy. This inverts #292's "292a first" plan but is
  mechanically clean — `location` and `location_rpt` are two type names over one
  ValueKind.

Either way the reconciliation is a rebase + full-gate rerun on the merged tip
(CLAUDE.md: green branch + green base ≠ green merge). The `tests/schema_layer.rs`
tripwire flip is the single most likely conflict point; whoever lands second
takes it.

## Feasibility — resolved

Spatial is feasible with **no new dependency**, for the same reason #292 found:
Tantivy has `f64` fast fields and an O(N) scan, which is exactly what a uniform
grid bucket needs. The heatmap adds no storage primitive beyond what #331
already requires, and the grid math is closed-form arithmetic once the fixtures
pin the prefix-tree type and the bounded-`geom` extent semantics. The only real
risk is item 5 (cell-snapping for a bounded `geom`) and item 6 (the all-zero-row
shape) — both resolved by the capture, not by guessing.

## Recommended next step

Build as a retained tracer bullet under TDD, per CLAUDE.md and the #292
pattern:

1. **Capture** the `heatmap` fixtures against real `solr:9` (append at the end
   of `capture.sh`); commit fixtures + `manifest.tsv` rows. Derive items 1–6 of
   the grid math from them *before* writing the computation, the way the PRD
   requires (expected values come from fixtures, never the implementation).
2. **Red:** `heatmap_*` differential rows fail; add `EXPECTED_DIVERGENCES`
   entries naming this issue.
3. **Implement** per the extension-point table: schema + `add_values` (shared
   with #331), `facet_heatmaps` + grid math, the rectangle `fq`, `rpts_*`/`rptm_*`
   rules, `SELECT_PARAMS`.
4. **Green:** delete the `heatmap_*` entries from `EXPECTED_DIVERGENCES`.
5. **Report:** `docs/reports/<date>-334-heatmap.md` with the captured grid-math
   table and the PR.

**Sizing verdict:** medium-to-large. The storage is settled and small (shared
with #331); the work and the risk are in the grid-math derivation from fixtures
(items 1–6) and the rectangle `fq`. Comparable in size to #290 (grouping) once
the capture pins the semantics.
