# Facets, statistics, highlighting, grouping, and spatial results

These `/select` components are supported only within their documented field and
parameter constraints. Start with the [parameter inventory](../reference/parameters.md)
and [Compatibility](../../docs/COMPATIBILITY.md); worked request shapes are
hermetically tested in [`tests/faceting.rs`](../../tests/faceting.rs),
[`tests/highlighting.rs`](../../tests/highlighting.rs), and
[`tests/grouping.rs`](../../tests/grouping.rs).

## Classic and JSON facets, and stats

Classic field, query, and range facets support the documented limit, mincount,
sort, missing, local-key, exclusion, and per-field overrides. A classic field
facet is a flat alternating array by default, or an object with `json.nl=map`;
`facet.missing=true` uses literal `null`. Existing but unfacetable fields are
400. JSON facets support terms facets, nesting, and bounded `max()` aggregation,
not an open-ended aggregation language. Statistics are the supported `stats`
component, not a promise of all Solr statistics.

Facets and stats observe the committed searcher view. Before relying on a count,
commit pending updates, use compatible fast fields, and validate bucket/result
counts on representative data. Oversized facet limits are clamped, not rejected;
retry a corrected constrained request if the result is insufficient. As
read-only requests, rollback is simply not consuming an invalid report.

## Highlighting and grouping

Highlighting adds a `highlighting` block for supported stored/analyzed fields.
Grouping returns the documented group response rather than a general distributed
collapse feature. Both are read-only presentation features and use the same
committed visibility as search. Validate component presence and selected IDs;
correct bad field/parameter choices and retry. Do not infer support for SolrCloud
or distributed grouping.

## Spatial, heatmap, and date-range

`location` supports documented spatial queries. `location_rpt` has the bounded
heatmap behavior, and `date_range` supports interval predicates through its
synthetic start/end columns; it is not a scalar date. Design the schema first,
then index valid coordinates/intervals and commit before querying. Validate a
known point or interval and response shape; malformed values are 400 and can be
corrected/retried without state change. Changing these field types is a
structural reindex migration, with rollback through the old schema/data set as
explained in [index lifecycle](../schema-and-indexing/index-lifecycle.md).
