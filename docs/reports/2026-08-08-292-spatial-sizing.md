# #292 — spatial sizing: feasibility, encoding, three-way split

**Date:** 2026-08-08. Parent issue #292 (still open). Branch
`markdlabrecque/issue-292-geo-field-type` off `main`. This report resolves the
feasibility gate the issue body names — *"Tantivy has no spatial primitive, so
establish feasibility (or the encoding choice) before committing to the wire
surface"* — commits the encoding decision for the foundational slice, and
formalizes finding 133's three-way split into proposed sub-issues. **It
implements no feature.** Fixtures and TDD land with whichever slice is picked up
first (§"Recommended next step").

## Feasibility gate — resolved

Confirmed against `Cargo.lock`: Tantivy 0.26 ships **no spatial primitive** (no
geo / lat-lon / hilbert / s2 / h3 dependency). So a geo point must be encoded
from existing primitives, exactly as the issue anticipated.

The point-distance operations finding 133 attributes to `setSpatial()`
(`SearchApiSolrBackend.php`) are four, and all four reduce to the same shape:

1. store a lat/lon point per document (`location` field, `locs_*`/`locm_*`);
2. filter by circle — `{!geofilt}` with `sfield`/`pt`/`d` (haversine ≤ `d` km);
3. filter by bounding square — `{!bbox}` (the lat/lon rectangle of the
   `d`-circle);
4. compute haversine distance — `geodist()` in `fl` (`fl=dist:geodist()`),
   `sort` (`sort=geodist() asc`), and `{!frange l=.. u=..}geodist()`.

(1)–(4) each need: *two f64 per doc, readable as a fast column, and an O(N)
predicate scan over them.* Tantivy has exactly that primitive — f64 fast fields
plus a hand-written `Query`/`Scorer`. **Spatial is feasible with no new
dependency**, and no spatial *index* is available to miss: there is no BKD/2D
point type in Tantivy to accelerate a circle test, so a column scan is the only
option regardless of encoding.

## Encoding decision — point-distance slice

**Decision: two synthetic f64 fast columns per `location` field —
`<field>__lat` and `<field>__lon`.**

A `location` value arrives in the Solr JSON form `"lat,lon"`
(`solr-ref/search-api/configset/schema.xml:229-233`); on `/update` it splits on
the comma into two f64s written to both columns. `geodist(D)` reads
`<sfield>__lat`/`<sfield>__lon` for doc `D` and haversines against the request
param `pt`. `{!geofilt}`/`{!bbox}` are custom `Query` impls whose scorer
enumerates the alive doc set and includes docs within `d` km (geofilt) or inside
the `d`-circle's lat/lon rectangle (bbox).

Why two columns, not one encoded (Morton/geohash) `i64`:

- **Exact precision, no rounding.** Solr's own `LatLonPointSpatialField` is
  full-precision; a geohash interleaving (32 bits/dim → ~0.7 m) would introduce
  rounding Wayfinder would then have to justify against captured fixtures.
- **Smallest blast radius.** It extends the existing `FieldColumns` fast-field
  machinery the function-query scorer already opens (`src/function_query.rs`);
  no encoder/decoder to write or test.
- **The single-column trade buys nothing here.** Interleaved locality matters
  only for spatial-*index* range pruning (Tantivy exposes none) or for a
  *prefix-tree grid* (heatmap bucketing). The heatmap is a different field type
  (`location_rpt`) and a different slice (#292c), where the grid is the whole
  point — that slice can choose the geohash encoding in its own sizing without
  being coupled to this decision. For point distance the locality buys nothing
  and costs rounding + an encoder.
- **Acceleration is not needed at site scale.** A per-site Drupal index is small
  enough that an O(N) fast-column scan per geo filter is cheap; this is the
  "ponytail" ceiling CLAUDE.md sanctions — dropping acceleration the use case
  does not need, called out by name.

## Wire surface committed by the decision

The Solr API is fixed by Solr, not by us; the encoding decision is what makes it
implementable. Listing for the split:

- **`location` field type** + the four dynamic rules the configset ships
  (`locs_*`/`locm_*`/`geos_*`/`geom_*`, all `type="location"`).
- **`{!geofilt}`** / **`{!bbox}`** as `fq` filters, driven by request params
  `sfield`/`pt`/`d` (the client-evidenced form, finding 133).
- **`geodist()`** in `fl` (`<alias>:geodist()`), `sort` (`geodist() asc`), and
  inside `{!frange l=.. u=..}`.

## Codebase extension points (map for #292a)

| Site | Change |
|---|---|
| `src/schema.rs` | `ResolvedType::Location` + `ValueKind::Location`; a `"location"` arm in `resolve_type`; add `"location"` to `NON_LANGUAGE_BUILTIN_TYPES`. The build path creates **two** synthetic f64 fields and a `location_fields(name) -> Option<(Field, Field)>` accessor. |
| `tests/schema_layer.rs` | The guard `type_names_absent_from_the_reservation_list_are_still_unresolvable` currently asserts `location` is unresolvable — it is the deliberate tripwire for this exact change (`src/schema.rs:726-733`). Update it to assert `location` is now resolvable. |
| `src/core_index.rs::add_values` | A `ValueKind::Location` arm parses `"lat,lon"` and writes both columns via `location_fields`. |
| `src/function_query.rs` | A `GeoDist { sfield }` variant; `fields()` reports `sfield__lat`/`sfield__lon`; `eval` reads them plus a captured request `pt` and returns haversine km. The argless `geodist()` (request-param `sfield`/`pt`) is the client-evidenced form; the explicit-args `geodist(sfield, pt, …)` form is Solr parity with no client evidence — documented descope. |
| `q`/`fq` parsing (`src/lib.rs` ~3005, `src/core_index.rs::parse_query`) | Recognise `{!geofilt}`/`{!bbox}` position-0 local-param blocks (joining `{!func}`/`{!edismax}`), building the filter `Query` from `sfield`/`pt`/`d`. |
| `sort` (`parse_sort_spec`) + `fl` | A `geodist()` sort key and a `fl=<alias>:geodist()` computed column. |
| `presets/search-api.toml` | The four `location` dynamic rules. |
| `solr-ref/capture.sh` | Append a `geo` block (own core, `location` field, ~7 docs at known offsets along + off a meridian so circle-vs-bbox is observable). |

## The three-way split (formalising finding 133)

| Sub-issue | Field type | Encoding | Scope | Depends on |
|---|---|---|---|---|
| **#292a — point distance (foundation)** | `location` | two f64 cols | `geodist()` (fl/sort), `{!geofilt}`, `{!bbox}` | nothing — build first |
| **#292b — function-range filter + distance-facet rewrite** | — (reuses #292a) | — | `{!frange l=.. u=..}geodist()` as a *general* range-filter-over-function (Solr `FunctionRangeQuery`), and the `facet.query` distance rewrite into N `{!frange}geodist()` entries | #292a + #289 (✅ landed) + `facet.query` (✅) |
| **#292c — heatmap facets** | `location_rpt` | Morton/geohash `i64` (decided in its own sizing) | `facet.heatmap.*` params, `counts_ints2D` response | nothing — independent; different field type |

Note on #292b: `{!frange l=.. u=..}<func>` is Solr's general "range filter over a
function" (`FunctionRangeQuery`), not a geo-specific construct — `geodist()` is
just the function that happens to flow through it on the client path. So #292b is
worth framing as *function range filters* generally, with the distance-facet
rewrite riding on top. That keeps it honest as a foundation other functions
(`ms`/`rord` if they ever land) would reuse.

**Sequencing:** #292a first — it owns the `location` encoding and `geodist()`
that both other slices lean on. #292c is independent (different field type,
different encoding, no shared filter code) and can be sized/run in parallel.
#292b follows #292a.

## Recommended next step

Build **#292a** as a retained tracer bullet under TDD, exactly per CLAUDE.md:
capture the `geo` fixtures against real `solr:9` (append at the end of
`capture.sh`) → red differential rows → schema encoding + `geodist()` +
`{!geofilt}`/`{!bbox}` + sort/fl → green → delete the `geo_*` entries from
`EXPECTED_DIVERGENCES`. The bulk of the work is schema plumbing (a new
`ValueKind` and the two-column field-handle map); the distance math is a single
haversine. Medium-sized slice.

Open question for the parent: create the three sub-issues now (with #292 as
parent), or keep them as headings here until #292a lands and split on demand?
