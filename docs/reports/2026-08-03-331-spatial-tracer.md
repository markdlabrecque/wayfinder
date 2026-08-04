# #331 — spatial tracer: `location` field type + `geodist()` in `fl`

**Date:** 2026-08-03. Issue #331 (open), split from #292 (parent, closed). Branch
`markdlabrecque/issue-331-tracer-location` off `main`. This is the retained
tracer bullet #292's sizing report (<https://github.com/markdlabrecque/wayfinder/blob/markdlabrecque/issue-292-geo-field-type/docs/reports/2026-08-08-292-spatial-sizing.md>)
recommends: the thinnest vertical slice that de-risks the spatial encoding
end-to-end before the filters land.

## What landed

A `location` field stores a point as **two synthetic f64 fast columns**
(`<field>__lat` / `<field>__lon`), and argless `geodist()` in `fl` returns each
document's haversine distance (km) from the `pt` request param. Encoding per
the sizing report (two columns, exact precision; the Morton/geohash alternative
buys nothing for point distance).

| Site | Change |
|---|---|
| `src/schema.rs` | `ResolvedType::Location` + `ValueKind::Location`; a `"location"` arm in `resolve_type`; `"location"` in `NON_LANGUAGE_BUILTIN_TYPES`; the build path creates two synthetic f64 fast columns (kept out of `field_handles`, like `VERSION_FIELD`) + a `location_fields(name) -> Option<(Field, Field)>` accessor. |
| `tests/schema_layer.rs` | `location` moved off the unresolvable-name tripwire into `MUST_BE_RESERVED_TYPE_NAMES`; new test `location_field_is_two_synthetic_f64_columns`. |
| `src/core_index.rs` | `as_location` parses Solr's `"lat,lon"` form; an `add_values` `Location` arm writes both columns; a `coerce_json` `Location` arm keeps a dynamic point as its string; `eval_function` evaluates a `FuncQuery` per doc. |
| `src/function_query.rs` | `FuncQuery::GeoDist { sfield, pt }`; `fields()` reports `sfield__lat`/`sfield__lon`; `eval` haversines; `haversine_km`; public `eval_doc` helper. |
| `src/lib.rs` | `sfield`/`pt` in `SELECT_PARAMS`; `computed_fl_fields` parses `<alias>:geodist()` from `fl` against the request params and the `/select` handler evaluates + appends each; `field_class_for_builtin` reports `wayfinder.LatLonPointSpatialField`. |
| `presets/search-api.toml` | the four `location` dynamic rules (`locs_*`/`locm_*`/`geos_*`/`geom_*`) from `schema.xml:229-237`. |
| `solr-ref/capture.sh` | appended a `geo` block (own core, 7-doc grid around NYC); two `geo_*` rows in `manifest-errors.tsv` + fixtures. |
| `tests/differential.rs` | `geo_app` (schema/corpus/routing); documented meter-granularity tolerance for `geo_*` distances. |

## Wire-compat evidence

Two `geo_*` rows captured against real `solr:9`, run hermetically against a
matching `geo_app`:

- `geo_geodist_fl` — `fl=id,dist:geodist()` with `sfield=loc&pt=40,-74` (the
  grid origin is `g1`, distance `0.0`).
- `geo_geodist_fl_pt` — the same with `pt=41,-73` (the NE corner is `g6`,
  distance `0.0`).

Both pass (0 diffs). The haversine is mutation-tested: doubling the distance
fails both rows, so the values are genuinely checked.

## Correction to the issue's "green" premise (flagged, not papered over)

The issue's TDD flow reads "implement → green → delete divergence entries,"
implying an exact byte-match. Solr's `geodist()` does **not** return an exact
distance: it computes Lucene's `SloppyMath.haversinMeters` (which Lucene's own
javadoc gives a ~40 cm error budget) on lat/lon re-read through 32-bit
`GeoEncodingUtils` quantisation. The tell is that a symmetric pair of points
1° north and 1° south of the origin return *different* km values
(`111.195076` vs `111.19508`) — an exact haversine on full-precision coords is
symmetric, so the asymmetry is the lossy read path. Recorded as **finding 157**
in `docs/solr-ref-findings.md`.

Wayfinder stores a point as two full-precision f64 fast columns (the encoding
decision is unchanged) and computes the exact haversine. The two agree to well
under a centimetre on the captured corpus, so the differential harness compares
`geo_*` distances at **meter granularity (3 dp of km)** — the same
float-magnitude category it already tolerates for BM25 `score` and stats
`sum`/`mean` (`score_tolerance`). Structural fields (`numFound`, doc ids, the
presence of the alias key) are still compared exactly; only the float magnitude
relaxes. This is the "escalate it with the diff" path CLAUDE.md prescribes, not
a silent normaliser widening.

## Descopes (named, per the issue)

- `{!geofilt}`/`{!bbox}` filters and `geodist()` in `sort` — split from #292,
  out of scope here. (`d`, the geofilt radius param, deliberately stays absent
  from `SELECT_PARAMS`.)
- `fl=<locationfield>` reconstruction of the stored point: a static `location`
  field's two columns are fast-only and not reconstructed into `fl` in this
  tracer. A dynamic `locs_*` point does round-trip as its `lat,lon` string.
- `geodist()` on a dynamic `sfield` is unsupported (the two-column encoding
  needs the field name at build time) — `location_fields` returns `None` for a
  dynamic name, and `computed_fl_fields` 400s on it.
- The explicit-args `geodist(sfield, pt, …)` form: the argless
  request-param-driven form is the client-evidenced one (finding 133).

## Commands

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test                      # all green
```

Live capture (own `solr:9` container, not part of CI): `bash solr-ref/capture.sh --only '^geo_'`.
