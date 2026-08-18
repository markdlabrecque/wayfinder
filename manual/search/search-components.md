# Search components

All components read the committed searcher and are bounded by their documented
field requirements. Start from a known query and choose the component by the
result needed; they are not interchangeable or a promise of Solr parity.

## Aggregate and present

Classic facets provide field, query, and range facets with documented limits,
mincount, sort, missing, local keys/exclusions, and per-field overrides. A
classic `facet_fields` result is a flat alternating array unless `json.nl=map`;
missing uses literal `null`. Existing unfacetable fields are 400, and
`facet.method=enum` is unsupported. JSON facets provide terms, nesting, and
bounded `max()` aggregation—not arbitrary JSON Facet functions. Stats are the
supported stats component, not a general statistics engine. Oversized facet
limits clamp to configuration.

Highlighting correlates snippets by unique-key string in the `highlighting`
object. Stored/analyzed fields can return bounded fragments; multi-valued fields
and `hl.preserveMulti` have method-specific behavior. `hl.snippets` caps rather
than pads, while `hl.maxAnalyzedChars`, `hl.usePhraseHighlighter`, and
`hl.highlightMultiTerm` are accepted but inert. Grouping returns `grouped`
envelopes with group-level offset/limit/sort; `group.truncate` changes the
facet/stats domain and `group.facet` requests grouped facet counts. Component
paging is not ordinary top-level paging, and grouping is not distributed
collapse.

Use `{!geofilt}` for a radius and `{!bbox}` for a bounding box with static
`location`/`location_rpt`, `sfield`, `pt`, and radius `d`; distance fields/sorts
use `geodist()`. Input coordinates are latitude,longitude. Heatmaps require
`location_rpt`; `facet.heatmap.geom` bounds geometry, `gridLevel` or distance
error controls resolution, `maxCells` guards allocation, and `ints2D` is the
only supported format (`png` is unsupported). A `date_range` supports default
`Intersects`, `Contains`, `Within`, and alias `IsWithin`; open bounds, exclusive
braces, year/month/second precision expansion, and `NOW` date math have bounded
test-backed behavior. Multi-values are a union with relation-specific rules,
not one min-to-max span; disjoint operations are unsupported.

## Choose a helper

Choose **MLT** for a seed document selected by `q` and configured `mlt.fl`;
filters apply to similar results rather than choosing a different seed. Choose
**terms** for literal indexed terms/prefixes (analysis is not rerun),
**spellcheck** for alternatives from field-backed indexed dictionaries, and
**suggest** for configured dictionary lookup. Repeated `suggest.dictionary`
inputs get separate keys and `suggest.highlight=false` removes server
highlighting; accepted build/reload/buildAll commands only echo command state
and are inert. None is a general autocomplete endpoint or creates an external
dictionary. In particular, use a Drupal-specific supported terms/suggest flow
rather than stock autocomplete.

## Read-only lifecycle

Prerequisites: commit representative source data and use compatible fast,
stored, analyzed, spatial, or interval fields as applicable. Visibility and
durability are exactly the current committed index; no component makes state
visible or durable. Failure is normally an invalid field/grammar 400 or a
clamped result: correct/constrain it and retry. Validation is buckets, selected
IDs, highlight/group shape, or known point/interval. Rollback is not consuming
an invalid report because requests are read-only.

Hermetic advanced request evidence: `tests/faceting.rs`, `tests/json_facet.rs`,
`tests/stats.rs`, `tests/highlighting.rs`, `tests/grouping.rs`,
`tests/spatial.rs`, `tests/heatmap.rs`, `tests/date_range.rs`, `tests/mlt.rs`,
`tests/terms.rs`, `tests/spellcheck.rs`, and `tests/suggest.rs`.
