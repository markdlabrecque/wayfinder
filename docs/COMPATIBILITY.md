# Compatibility

Wayfinder retains the Solr-shaped JSON wire through which existing clients already reach it. This
is a bounded compatibility contract, not an ongoing Solr parity project. Wayfinder uses Solr
request names and response envelopes on the routes below; it does not use Solr configuration files
or aim to become a general Solr replacement.

## Routes

| Route | Purpose |
|---|---|
| `/wayfinder/{core}/select` | Search, facets, stats, highlighting, grouping, and spatial queries |
| `/wayfinder/{core}/update` | Add, delete, and commit whole documents |
| `/wayfinder/{core}/update/extract` | Extract or index supported document formats |
| `/wayfinder/{core}/mlt` | MoreLikeThis |
| `/wayfinder/{core}/terms` | Terms lookup and prefix completion |
| `/wayfinder/{core}/suggest` | Dictionary suggestions |
| `/wayfinder/{core}/admin/ping` | Core health |
| `/wayfinder/admin/info/system` | Server information |
| `/wayfinder/{core}/admin/system` | Core-scoped server information |
| `/wayfinder/{core}/schema/fieldtypes` | Field-type metadata |
| `/wayfinder/{core}/admin/luke` | Schema/index metadata |
| `/wayfinder/{core}/admin/mbeans` | Selected runtime metrics |

Wayfinder also serves its own read-only `/ui` pages. Those are not part of the retained wire.

## Frozen baseline

Fixtures in `solr-ref/responses/` are the frozen regression baseline. Expected values come from
fixtures, never from implementation output. They are factual evidence for shipped behavior and do
not expand product scope. No new Solr-parity captures are planned.

Historical capture and client observations remain in
[`solr-ref/FINDINGS.md`](../solr-ref/FINDINGS.md). Numbering gaps are intentional. The findings are
supporting evidence rather than instructions or roadmap items.

## Response contract

Supported paths retain these shapes:

- `responseHeader` with `status`, `QTime`, and echoed `params`
- `response` with `numFound`, `start`, `numFoundExact`, and `docs`
- `facet_counts`, `highlighting`, and `stats` where requested
- errors shaped as `{"responseHeader": ..., "error": {"msg": ..., "code": ...}}`, with matching
  HTTP status, `error.code`, and header status

Notable details:

- Classic `facet_fields` is a flat alternating array by default and becomes an object under
  `json.nl=map`; `facet.missing=true` appends a literal `null` key.
- Parameter echoes preserve raw strings. Object key order is not semantic.
- Unknown `fl` fields are omitted. Unknown request parameters are ignored unless `strict_params`
  is enabled.
- `numFoundExact` is always true because Wayfinder uses exact counting.
- Sorting and faceting require compatible fast fields.
- `wt=json` is the only response writer.

## Current capabilities

| Area | Supported behavior |
|---|---|
| Query syntax | Boolean, term, phrase, prefix, range, fuzzy, and regex queries; repeatable `fq` filters |
| Retrieval | `fl`, `start`, `rows`, exact counts, multi-clause fast-field sorting |
| Classic facets | Field, query, and range facets; limit, mincount, sort, missing, local keys, exclusions, and supported per-field overrides |
| JSON facets | Terms facets, nesting, and supported `max()` aggregations |
| Relevance | BM25 plus the edismax and function-query subset below |
| Presentation | Highlighting, stats, grouping, and supported spatial/date-range responses |
| Updates | Whole-document add, delete by ID/query, commit, `commitWithin`, `softCommit`, and `overwrite` |
| Helpers | MoreLikeThis, terms, spellcheck, suggest, ping, and selected admin metadata |
| Extraction | In-process multipart extraction and indexing for shipped formats |

The source allowlists in `src/lib.rs` are the exhaustive authority for accepted parameter names.
`strict_params` validates against those per-route allowlists; acceptance does not imply general
Solr behavior beyond what the handler documents and tests.

## Edismax and function queries

Wayfinder supports `defType`, `q`, `qf`, `pf`, `mm`, `tie`, `bq`, `bf`, quoted phrases,
`+`/`-`, and `boost`. Function expressions support constants, numeric field references, `sum`,
`product`, `max`, `min`, and `recip`. `{!func}`, `{!boost b=...}`, and `{!payload_score}` use the
shipped evaluator.

`pf2`, `pf3`, `ps`, `stopwords`, and `lowercaseOperators` are permanently unsupported edismax
parameters. Analyzer stopword filters are a separate supported schema feature.

Within `{!payload_score}`, only one payload-bearing term with `includeSpanScore=false` is
supported. A payload query against a field without payloads returns 400.

A local-params block in `q` supports only the `edismax`, `func`, and `boost` parser names. Other
parser types such as `{!lucene}` receive a `SyntaxError` 400. This is not a regression: before
issue #137, Tantivy's grammar already rejected the raw local-params string; issue #137 improved the
error. `{!func}` and `{!boost}` became supported through the real evaluator in issue #289.

The historical issue #137 Shape-B request
`q=({!edismax qf='...'}+"quick" +"rocket")` deliberately retains its captured low-recall outcome.
With outer defaults `defType=lucene` and `df=id`, traces 00004 and 00008 return `numFound: 0` even
though the document contains both terms. Findings 90–92 preserve the evidence. Reproducing that
low recall is deliberate fidelity. Issue #137's “so keyword search works” premise is wrong-premised;
treating the entire expression as one high-recall edismax query would be a divergence from the
existing-client behavior.

## Deliberate differences

Wayfinder intentionally differs from captured Solr behavior in these areas:

1. Unknown cores return Wayfinder's JSON 404 rather than HTML.
2. Faceting an existing but unfacetable field returns 400 rather than an empty list.
3. Unknown document fields are rejected; dynamic fields must be configured.
4. `fl=score` preserves score keys and ranking order, not Lucene's exact float magnitude.
5. Admin responses report configured Wayfinder values rather than JVM/build details.
6. Colliding `facet.field` response labels are a hard 400. The raw
   `facet_collision_field_flat.json` and `facet_collision_field_map.json` fixtures show why:
   Solr emits duplicate object members that ordinary JSON models cannot retain safely.
7. Invalid `omitHeader` values return a JSON 400 rather than server HTML.
8. Authentication failures use Wayfinder's JSON 401 and `Basic realm="wayfinder"`.
9. `/update/extract` uses Wayfinder's in-process extraction rather than Tika metadata and errors.
10. Repeated `suggest.dictionary` values each receive a response key.
11. `suggest.highlight=false` disables server-side suggestion highlighting.
12. Search API language dictionaries without Tantivy stemmers use their configured unstemmed
    analysis chain rather than being rejected or substituted.

## Permanent unsupported boundaries

- `q.op` and `qt`
- `search_api_solr_admin` core reload, field-analysis, and configset-file routes
- Open-ended `solr_text_custom` analyzer families and Solr XML analyzer imports
- Atomic field modifiers, optimistic concurrency, `versions=true`, and stale-write conflicts
- Classic `facet.method=enum`
- SolrCloud, ZooKeeper, distributed/sharded search, streaming expressions, and SQL
- XML, javabin, PHP, and other non-JSON response writers
- OCR and external extraction services

Wayfinder serves one configured core per process. It does not expose Solr's core-admin or configset
model.
