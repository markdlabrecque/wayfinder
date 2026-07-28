# Solr reference capture — findings

Source: `solr:9` (`_default` configset), core `content`, 5 docs.
Regenerate with `solr-ref/capture.sh` (gitignored). Fixtures in `solr-ref/responses/`, index in `solr-ref/manifest.tsv`.

Schema: `id` (string, uniqueKey), `body` (text_en, stored), `category` (string, stored,
docValues, multiValued). `doc5` has no `category` — that is what makes `facet.missing`
observable.

## Envelope facts the tracer bullet must reproduce

1. **`facet_fields` defaults to a flat alternating array**, not an object:
   `"category":["animals",2,"classic",2,"garden",1,"misc",1]`.
   `json.nl=map` switches it to `{"animals":2,...}`. Both shapes are in scope.

2. **`facet.missing=true` appends a literal `null` key** to that array:
   `[...,"misc",1,null,1]`. So the array is heterogeneous — string|null alternating with int.
   Serde modelling needs to handle that; an untyped JSON value array is the honest
   representation.

3. **`facet_counts` always carries all five sub-objects**, empty when unused:
   `facet_queries`, `facet_fields`, `facet_ranges`, `facet_intervals`, `facet_heatmaps`.
   Omitting the empty ones is a diff.

4. **`facet_counts` is absent entirely** when `facet` is not requested — not present-and-empty.

5. **`numFoundExact: true`** is present in Solr 9 responses. Wayfinder always knows the exact
   count (`Count` collector), so it is always `true`, but the key must be there.

6. **`params` echo reflects the raw request**: values are strings even for numerics
   (`"rows":"0"`), and key order is not request order. The differential normaliser must be
   order-insensitive on this object.

7. **Unknown field in `fl` is silently dropped.** No error, the key is just absent from the doc.

8. **Unknown request parameters are silently ignored** — `status: 0`, normal response.
   This contradicts PRD open question 3, which leaned toward rejecting them. See below.

9. **Internal fields leak into unrestricted results.** With no `fl`, docs include `_version_`
   and `_root_`. Wayfinder needs an explicit decision about what its default `fl` returns.

10. **Error shape**: HTTP status matches `error.code`, `responseHeader.status` mirrors it, and
    `error.metadata` is *also* a flat alternating array
    (`["error-class","...","root-error-class","..."]`). `error.msg` is free text — not worth
    matching verbatim; the differential harness should compare code and status only.

11. **Sort on a non-docValues field is a 400**, not a silent fallback. Matches the PRD's
    decision to require `fast = true` for sortable fields — same constraint, better error
    message available to us.

## Decision this forces

**Open question 3 (reject vs ignore unknown params) now has real evidence against rejecting.**
Solr ignores them. A strict Wayfinder would 400 on requests real Solr serves, which breaks the
compatibility claim for any client that sends extra params — and Solr clients do, routinely.

Recommendation: **ignore unknown params by default, log them at debug, and offer a
`strict_params = true` config flag** for development. Keeps the compatibility promise while
still giving a way to discover gaps during the Search API phase.

## Not yet captured

Highlighting, stats, MLT, edismax, `/update` responses, `commitWithin`/`softCommit` behaviour,
range facets. Add to `capture.sh` when those features come into scope — the script is meant to
grow with the feature set.
