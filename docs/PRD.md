# Wayfinder — PRD

A Solr-compatible search backend in Rust, built on Tantivy.

Status: draft
Date: 2026-07-27

---

## 1. Problem & motivation

Every site that needs real search runs Solr, and every Solr instance means a JVM in
production: 1–2 GB of resident memory before the first document is indexed, GC tuning, a
10–30 second cold start, a separate deployment lifecycle, and an XML config set that has to
be regenerated and re-uploaded whenever the index definition changes.

**Wayfinder** is a single static Rust binary backed by Tantivy, configured by TOML, with one
schema file and one data directory.

### Primary goals

1. **Speed** — sub-10 ms p95 for typical facet+filter queries; near-instant cold start.
2. **Simplicity of operation** — one binary, one config file, one data directory. No JVM,
   no ZooKeeper, no config-set upload dance, no GC tuning.

### Secondary goals

3. Memory footprint an order of magnitude below Solr for the same corpus.
4. A stable wire for existing clients.

### Existing-client wire

Wayfinder retains its Solr-compatible wire because that is how Wayfinder was built and how
existing clients reach it. The wire is not an ongoing parity goal. It supplies the current
request parameter names and JSON envelope without adopting Solr's configuration format or
becoming a general Solr replacement.

### Non-goals

- SolrCloud, ZooKeeper, distributed/sharded search.
- Data Import Handler, Streaming Expressions, SQL interface.
- XML anything — not `schema.xml`, not `solrconfig.xml`, not `wt=xml`. See §3.
- OCR. Image-only documents may validly yield no text.

Document extraction is an in-process Rust capability; it never requires a JVM, Tika server,
sidecar, or separately operated service. Well-maintained Rust parsing crates are ordinary
implementation dependencies, not third-party runtime services.

---

## 2. Compatibility contract

Wayfinder's current supported behavior uses Solr's **wire format** — request parameter names
and response JSON envelopes — and deliberately not its configuration format. The retained wire
is an existing-client entry point, not an ongoing parity program.

The fixtures in `solr-ref/responses/` are the frozen regression baseline. Expected values come
from fixtures, never implementation output. They record factual historical Solr and client
behavior; fixture existence does not itself broaden Wayfinder's supported surface.

### What must match exactly

For current request and response paths, the shipped wire retains these shapes:

- **Response envelope.** `responseHeader{status, QTime, params}`,
  `response{numFound, start, numFoundExact, docs}`, `facet_counts{facet_queries,
  facet_fields, facet_ranges}`, `highlighting{}`, and `stats{}`. This includes flat alternating
  `facet_fields`, `json.nl` handling, and the shapes of empty results and facets.
- **Error shape.** `{"responseHeader": {...}, "error": {"msg": ..., "code": ...}}` with the
  returned HTTP status.
- **Parameters.** The current supported parameter names and semantics in §5.

### Verified envelope facts

1. `facet_fields` defaults to a **flat alternating array** and switches to an object under
   `json.nl=map`.
2. `facet.missing=true` appends a literal `null` key to that array.
3. `facet_counts` carries `facet_queries`, `facet_fields`, `facet_ranges`, `facet_intervals`, and
   `facet_heatmaps` when faceting is requested, and is absent otherwise.
4. `numFoundExact` is present. Wayfinder always returns `true` because its `Count` collector is
   exact.
5. Parameter echoes retain raw string values, including numerics; their object key order is not
   semantically meaningful.
6. An unknown `fl` field is silently omitted from a document, and unknown request parameters are
   ignored unless `strict_params` is enabled.
7. With no `fl`, Solr fixtures include `_version_` and `_root_`; Wayfinder's documented response
   behavior is described with the relevant feature.
8. Error HTTP status, `error.code`, and `responseHeader.status` agree. `error.metadata` is a flat
   alternating array; message text is not treated as a general equality contract.
9. Sorting a non-`docValues` field is a hard 400. Wayfinder's equivalent requirement is
   `fast = true`.
10. A numeric or date `facet.field` can raise `facet.mincount` to 1 and report the warning in
    `responseHeader.warnings`.

### What deliberately differs

- Configuration is TOML (§3), not `schema.xml` or `solrconfig.xml`.
- `wt=json` is the only response writer; XML, javabin, and `wt=phps` are unsupported.
- Wayfinder serves one configured core per process rather than Solr's core-admin model.

#### Ratified divergences from captured Solr behaviour

This is the descriptive record of current supported behavior that intentionally differs from
historical Solr fixture evidence. The numbering is stable for existing citations.

1. **Unknown core errors are JSON, not Solr's HTML 404 page.** Wayfinder preserves the 404 status
   and returns its normal JSON error envelope.

2. **Faceting an existing but unfacetable field is a 400.** Solr's historical fixtures return 200
   with an empty count list; Wayfinder rejects a request it cannot aggregate rather than making an
   impossible field indistinguishable from an empty one.

3. **Unknown document fields are rejected.** Solr's `_default` configuration can add such fields
   schemalessly; Wayfinder uses a fixed schema. `[[dynamic_fields]]` is the supported way to
   admit matching fields.

4. **`fl=score` preserves score keys and ranking order, not Solr's BM25 float magnitude.** Tantivy
   and Lucene use different scoring internals; `score` and `response.maxScore` retain their
   current wire positions and types.

5. **System-admin responses report Wayfinder values.** Configured version values and Wayfinder
   placeholders replace Solr's JVM, host, timestamp, path, and build-specific data; the reported
   schema string keeps the `search_api_solr`-consumed segments while naming Wayfinder.

7. **Colliding `facet.field` response labels are a hard 400.**
   `facet_collision_field_flat.json` and `facet_collision_field_map.json` show Solr emitting
   duplicate JSON object members for the same response label; Wayfinder rejects that ambiguous
   response shape. Repeated identical `facet.query` values still coalesce.

8. **Invalid `omitHeader` values receive Wayfinder's JSON 400 rather than Jetty HTML.** Valid
   `omitHeader=true`/`yes`/`on` suppresses the header on success and error responses.

9. **Authentication failures are JSON.** Wayfinder returns its JSON `WfError` envelope and
   `WWW-Authenticate: Basic realm="wayfinder"` rather than Solr's Jetty HTML and `solr` realm.

10. **`/update/extract` uses Wayfinder extraction rather than Tika.** It omits Tika's
    `X-Parsed-By` classes and fabricated link `shape="rect"`, indexes Wayfinder-extracted content,
    and returns 200-empty for a malformed content stream that Tika rejects.

11. **`{!payload_score}` on a field without payloads is a 400.** Solr's historical response is an
    uncaught 500; Wayfinder returns a diagnosable validation error.

12. **`/suggest` honors `suggest.highlight=false`.** The historical
    `suggest_q_hl_off_en.json` and `suggest_q_hl_on_cfq_en.json` fixtures record Solr's
    handler behavior, while Drupal's `QueryBuilder.php` sets the parameter false and
    `search_api_autocomplete` highlights suggestions itself in `Suggester.php`. Wayfinder
    therefore disables highlighting when it is false, while an absent or true value highlights
    unless `suggest.cfq` engages a context filter (issue #400).

13. **Repeated `suggest.dictionary` values each receive a response key.** This is a Wayfinder
    client contract rather than captured Solr behavior: the multilingual suggester request sends
    one value per resolved language, and `WayfinderBackend::getSuggesterAutocompleteSuggestions()`
    consumes every dictionary key under `suggest` (issue #398).

14. **Search API language dictionaries without Tantivy stemmers use an unstemmed chain.** Each
    shipped Search API language field-type code remains its own `suggest.dictionary` response key
    and uses tokenize, accent-fold, word-delimit, and lowercase analysis without stemming. This
    serves languages such as `ja`, `pl`, and `zh-hans` instead of rejecting them or silently
    substituting `und`; genuinely unconfigured dictionaries such as the frozen fixture's `xx`
    remain a 400 (issue #397).

---

## 3. Configuration & schema

### One format, TOML, at runtime

```toml
[core]
name = "content"
unique_key = "id"
default_field = "text"

[[fields]]
name = "id"
type = "string"
stored = true
required = true

[[fields]]
name = "title"
type = "text_en"
stored = true
boost = 2.0

[[fields]]
name = "text"
type = "text_en"

[[fields]]
name = "category"
type = "string"
fast = true              # required for facet / sort
multi_valued = true

[[fields]]
name = "created"
type = "date"
fast = true

[[dynamic_fields]]
pattern = "*_i"
type = "int"
fast = true

[[copy_fields]]
source = "title"
dest = "text"
```

Per-field options map directly onto Tantivy's schema options: `stored` → `STORED`,
`indexed` → `INDEXED`, `fast` → `FAST`. `fast` is stated explicitly on the field rather than
being an implicit consequence of `docValues`, because it is the option people forget and then
cannot sort or facet.

### Analyzer chains: named presets first

The only genuinely nested part of a schema is the analyzer chain — an ordered list of filters
with per-filter parameters. Ship presets covering the common cases (`string`, `keyword`,
`text_general`, `text_en` and the other major languages) so nobody writes a chain by hand:

```toml
[[fields]]
name = "title"
type = "text_en"         # preset
```

The explicit chain is the escape hatch, not the default:

```toml
[[field_types]]
name = "text_en_custom"
tokenizer = "simple"
[[field_types.filters]]
kind = "lowercase"
[[field_types.filters]]
kind = "stopwords"
language = "english"
[[field_types.filters]]
kind = "stemmer"
language = "english"
```

This matches how Tantivy works — a `TextAnalyzer` registered under a name in the tokenizer
manager, referenced by name from the field. The preset *is* the registration.

### Why no `schema.xml`, even for Search API

An earlier draft proposed an `import-solr-config` converter for Search API use. On inspection it
is not needed, and shipping it would be building for a problem we do not have.

`search_api_solr` does not declare a field per Drupal field. It encodes type and cardinality
into the field name — `ss_`, `sm_`, `tm_`, `its_`, `ds_`, `bs_`, and so on — and relies on
Solr's dynamic-field matching. That scheme is **fixed and mechanical**: it is a property of
the module, not of any particular site. So the entire Search API field convention expresses
as a set of `[[dynamic_fields]]` rules in one TOML preset, hand-written once, shipped in the
repo, and reused by every Drupal site.

Same for the language-specific text types the module generates — a fixed set per language,
shipped as analyzer presets.

Wayfinder ships a `search-api.toml` preset rather than a converter. It remains a
setup-time TOML description, never a runtime XML parser.

### Server config

A second TOML file for the tuning knobs in §5. Cores are subdirectories of a data directory,
each holding its Tantivy index and the schema it was built from. On startup, compare the
on-disk schema against the configured one and refuse to open on an incompatible change rather
than silently returning wrong results.

---

## 4. Architecture

```
                HTTP (axum)
                     │
        ┌────────────┴────────────┐
        │   Solr protocol layer   │   param parsing, response envelope,
        │                         │   JSON writer, error shapes
        └────────────┬────────────┘
                     │
   ┌─────────────────┼─────────────────┐
   │                 │                 │
┌──┴───────┐  ┌──────┴──────┐  ┌───────┴────────┐
│  schema  │  │    query    │  │     update     │
│  layer   │  │   pipeline  │  │    pipeline    │
│          │  │             │  │                │
│  TOML →  │  │ parse → fq  │  │ doc mapping,   │
│ tantivy  │  │ → collect → │  │ delete, commit │
│  schema  │  │ post-proc   │  │ policy         │
└──┬───────┘  └──────┬──────┘  └───────┬────────┘
   │                 │                 │
   └─────────────────┼─────────────────┘
                     │
              ┌──────┴──────┐
              │   Tantivy   │
              └─────────────┘
```

**Query pipeline.** `parse → filter → collect → post-process`, all Tantivy-native: `q`/`fq`
to `Query` trees, collectors for hits + count + aggregations, then highlighting and response
assembly.

**Update pipeline.** JSON document add → `Document`; delete-by-id → `delete_term` on the
unique key; delete-by-query → resolve and delete; `copy_fields` applied at index time.

**Concurrency.** One `IndexWriter` per core (Tantivy permits only one), fronted by a queue.
Readers are lock-free via `IndexReader` with a reload policy. This is the natural shape of
the library, not a compromise.

---

## 5. Feature scope

This section records the current product surface. The Solr-compatible wire is retained for
existing clients, not expanded as an ongoing parity goal.

### Current search and indexing capabilities

| Feature | Current implementation |
|---|---|
| Schema, field types, analyzer chains, multi-valued fields | Tantivy `Schema`, `TextAnalyzer`, tokenizer/filter stack |
| BM25 relevance | Tantivy's native default scorer |
| `q` | Boolean, term, phrase, prefix, range, fuzzy, and regex queries |
| `fq` | Repeatable non-scoring Boolean `Must` clauses |
| `fl`, `start`, `rows` | Stored-field retrieval and `TopDocs` pagination |
| `sort` | Custom fast-field collector, including multi-clause sorting |
| Result counts | Exact Tantivy `Count` collection |
| Classic facets | `facet.field`, `facet.query`, `facet.range`, limit, mincount, sort, and missing buckets |
| JSON facets | Terms facets, nesting, and `max()` aggregations over supported fields |
| Stats | Metric aggregations for supported stat fields |
| Highlighting | `hl`, `hl.fl`, snippets, fragment size, and simple tags |
| MoreLikeThis | `/mlt` with its supported term and scoring parameters |
| Updates | Add, delete-by-id, delete-by-query, commit, `commitWithin`, `softCommit`, and `overwrite` |
| Search helpers | `/terms`, spellcheck, `/suggest`, `/admin/ping`, and the read-only `/ui` core view |
| Extraction | Multipart `/update/extract` for the shipped extraction formats and envelopes |
| Spatial and grouping | Shipped point, heatmap, date-range, grouping, and function-query capabilities |

`sort` uses Wayfinder's own collector over per-segment fast-field column readers. A field must
be `fast`; multi-valued selectors and missing-value behavior are handled by that collector rather
than `TopDocs::order_by_fast_field`.

**Classic-facet boundary.** `facet.method=enum` is permanently unsupported. Wayfinder exposes its
fast-field aggregation path and returns 400 rather than attempting Solr's term-dictionary method.

### v1 exception — edismax

Wayfinder supports `defType`, `q`, `qf`, `pf`, `mm`, `tie`, `bq`, quoted phrases, `+`/`-`, and
`boost`/`bf` over constants, numeric field references, `sum`, `product`, `max`, `min`, and
`recip`. `boost` multiplies the composed score per document and **`bf` is supported** as an
additive function-query boost. `{!func}`, `{!boost b=...}`, and `{!payload_score}` use the
shipped evaluator and payload field type.

`pf2`, `pf3`, `ps`, `stopwords`, and `lowercaseOperators` are permanently unsupported edismax
parameters. The analyzer `stopwords` filter is separate and remains supported where configured.
Within `{!payload_score}`, `includeSpanScore=true` and multi-term `v` values are unsupported;
the shipped evaluator supports a single payload-bearing term with `includeSpanScore=false`.

A local-params block in `q` names only the supported `edismax`, `func`, and `boost` parsers;
other parser types receive a `SyntaxError` 400, although Solr parses registered types such as
`{!lucene}quick`. This is **not a regression**: before issue #137, Tantivy's grammar already
rejected the raw `{!` string; #137 changed the error message. `{!func}` and `{!boost}` landed
through the real evaluator in #289.

**Issue #137 Shape B.** `search_api_solr` sends
`q=({!edismax qf='...'}+"quick" +"rocket")`; its captured handler defaults are
`defType=lucene` and `df=id`. The outer lucene parser binds only the next run after the local
params block, so traces 00004 and 00008 return `numFound: 0` although the document has both
terms. Findings 90, 91, and 92 record the fixture and client evidence.

This is deliberate **fidelity** for a current supported path: the high-recall interpretation is
a **divergence** from the shipped behavior. Issue #137's “so keyword search works” title is
wrong-premised; the Shape-B request does not make keyword search work, it preserves the observed
low-recall result.

### Permanent unsupported boundaries

`q.op` and `qt` are permanently unsupported. Wayfinder serves its own configured core and select
handler; it does not accept the foreign-core `solr_document` datasource path that emits them.

`search_api_solr_admin` is permanently unsupported. Its core reload, field-analysis, and
configset-file routes are unreachable through a Wayfinder backend and have no Wayfinder route.

The open-ended `solr_text_custom` and `solr_text_custom:<code>` analyzer families are permanently
unsupported. Wayfinder supports its fixed Search API field-type presets rather than importing
site-defined Solr analyzer chains.

Atomic updates and optimistic concurrency are permanently unsupported. `/update` accepts whole
documents, not field modifiers such as `set` or `inc`, `versions=true`, or stale-write conflict
handling.

SolrCloud, ZooKeeper, distributed or sharded search, streaming expressions, the SQL interface,
and XML/javabin response writers remain unsupported non-goals.

### Current `_version_`, administration, and extraction behavior

`_version_` is an internal `i64` fast field populated from a per-core counter. It is not declared
in `schema.toml` and is omitted from ordinary response documents. It remains available to the
shipped stats and JSON-facet behavior where those routes support it.

The read-only `/ui` page reports the configured core's live document count and on-disk size. It
runs in the same binary and reads the same in-process index and schema state as the wire routes.

`/update/extract` is an in-process multipart endpoint. It retains the documented extract-only and
indexing response shapes, applies the configured resource limits, and uses Wayfinder's own
extraction rather than Tika. OCR and external extraction services are unsupported.

---

## 6. Tuning knobs

Current server TOML and per-request parameters where Solr has them.

**Relevance (per-request):** `qf`, `pf`, `mm`, `tie`, `boost`, `bq`, `sort`.

**Commit / visibility (config + per-request):**
- `autocommit_max_docs`, `autocommit_max_time` — hard commit thresholds.
- `commitWithin` — per-request, as Solr.
- `softCommit` → maps to reader reload, **not** a true soft commit. Tantivy has no
  in-memory-searchable uncommitted segment; near-real-time behaviour comes from frequent
  cheap commits plus `ReloadPolicy::OnCommitWithDelay`. Document the difference explicitly
  rather than implying equivalence.

**Indexing:**
- `writer_heap` — `IndexWriter` arena size in bytes. The main indexing-throughput lever.
- `writer_threads` — indexing thread count.
- `merge_policy` — `LogMergePolicy` parameters (min layer size, level log size), or `NoMerge`
  for bulk load.

**Query execution:**
- `time_allowed` — query time budget.
- `rows_limit`, `facet_limit_max` — hard caps, so a bad client cannot ask for 1M rows.
- `facet.mincount`, `facet.limit`, `facet.sort` — per-request, as Solr.

**Resources:**
- `doc_store_compression`, `doc_store_blocksize` — stored-field compression trade-off.
- `searcher_pool_size`.
- `max_body_size` — hard cap, in bytes, on an incoming request body (issue #64), wired to an
  axum `DefaultBodyLimit` layer. Default `10_000_000`, a round headroom figure over the largest
  known captured fixture; axum's own bare default is 2MB, too small for a realistic bulk
  `/update`. Could not be derived from a verified Solr-side max-request-size default (finding 79,
  `docs/solr-ref-findings.md`) — the closest Solr knobs (`requestParsers`'s
  `formdataUploadLimitInKB`/`multipartUploadLimitInKB`) govern form/multipart uploads, not raw
  JSON bodies.
- No heap tuning, by design. Tantivy is mmap-based; the OS page cache does the work Solr's
  heap sizing does. The absence is a feature and should be documented as one.
- No native TLS termination. Wayfinder serves HTTP on loopback or a trusted private network;
  non-colocated deployments terminate TLS at an established reverse proxy. Certificate issuance,
  renewal, reload, and protocol policy stay outside the search process, preserving operational
  simplicity without inventing a second certificate lifecycle. See `docs/deployment.md`.

**Admin (config only, no per-request equivalent):**
- `reported_server_version` — the `lucene.wayfinder-spec-version` served by `/admin/info/system`
  and `<core>/admin/system`, default `"9.0.0"` (issue #59, §2 ratified divergence 5, §10 open
  question 2). Issue #325 renamed the key from `reported_solr_version`, which is still accepted
  as a serde alias, so existing config files keep parsing. Unclamped: an operator who overrides
  it owns the compatibility risk.

---

## 7. The tracer bullet (POC)

One thin vertical slice through every layer the finished product needs — real code, kept and
iterated on, not a spike. No Drupal in it.

**Schema:** three fields — an ID (`string`, stored), a body (`text_en`), and a `category`
(`string`, `fast`, `multi_valued`).

1. TOML schema file → Tantivy schema.
2. `POST /wayfinder/<core>/update` — JSON add, `commit`.
3. `GET /wayfinder/<core>/select` — `q`, `fq`, `fl`, `rows`, `start`, one `facet.field`, correct
   `numFound`, correct Solr JSON envelope.
4. `GET /wayfinder/<core>/admin/ping`.

**Done when:** `curl` a document in and `curl` a query out through the shipped wire,
with the response shape asserted by the frozen regression fixtures.

The one non-obvious inclusion is the single `facet.field`. Faceting goes through a completely
different Tantivy path — the aggregation API over fast fields — and whether a field is `FAST`
is a *schema* decision. Leave faceting out and the slice can produce a schema layer with no
way to express it, which is exactly the layer a tracer bullet exists to prove. One facet field
forces that decision on day one for very little code.

The initial slice excluded highlighting, edismax, stats, MLT, and sort. Their current shipped
behavior is recorded in §5.

---

## 8. Benchmarking

Benchmark current Wayfinder behavior against the operational goals rather than treating Solr
parity as a product target. Two corpus sizes are useful: 50 k documents and 2 M documents.

| Metric | Solr baseline | Wayfinder target |
|---|---|---|
| Resident memory, idle | ~1 GB | < 50 MB |
| Resident memory, 2 M docs under query load | 2–4 GB | < 500 MB |
| Cold start to first query served | 10–30 s | < 1 s |
| p95 query latency (facet + filter + highlight, 50 k docs) | baseline | ≤ baseline |
| Container image size | ~500 MB | < 30 MB |
| Index size on disk | baseline | ≤ 1.2× baseline |

The frozen fixture baseline supplies regression assertions for the current wire. Performance work
measures Wayfinder's own latency, memory, startup, and operational simplicity.

---

## 9. Risks

| Risk | Mitigation |
|---|---|
| Response-envelope fidelity is harder than it looks — Solr has accreted odd shapes (`facet_fields` alternating arrays, `json.nl`, inconsistent empty-value handling) | Frozen fixture-derived regression assertions for the current wire |
| Relevance drifts visibly from Solr on migrated corpora | Both are BM25; drift comes from analyzer chains. Test ranked ID lists explicitly, not just membership. |
| `softCommit` semantics differ enough to surprise clients that expect NRT | Document plainly; test the reindex/update flows a real client performs |
| A client needs something structurally absent (a component or response field with no Tantivy-side source) | Scope any Wayfinder-specific design on its own merits; the retained wire creates no parity commitment. |
| Scope creep toward being a general Solr replacement | The non-goals in §1 and the current boundaries in §5. |
| One `IndexWriter` per core becomes an indexing bottleneck | It is a Tantivy constraint, not a choice. Measure at 2 M docs; if it binds, the answer is batching, not architecture. |

---

## 10. Open questions

1. ~~**Multi-core vs single-core-per-process.**~~ **Resolved: single-core-per-process.** Multi-core
   is more Solr-like, but one process per core is simpler and matches the operational-simplicity
   goal, and it is what the codebase already does — `src/lib.rs`'s module doc states it as the
   current architecture, and `app()` takes exactly one schema/data-dir pair with no `CoreRegistry`
   anywhere. Issue #94 confirmed this the hard way: a ticket drafted against a "list all cores"
   premise had to be corrected once the code was read, because there is no registry to list. The
   admin UI (§5) is one core view, not a core list.
2. ~~**Which Solr version to report** from `/admin/system`.~~ **Resolved by issue #59:**
   `[admin] reported_server_version` (issue #325's rename of `reported_solr_version`, still
   accepted as an alias) defaults to `"9.0.0"` — the lowest version in the 9.x branch
   the Search API capture's generated `schema.xml` already targets (finding 78), and every
   `search_api_solr` `version_compare()` condition sits at or below Solr 8.x, so the reported
   value does not imply an unsupported feature. See ratified divergence 5 below.
3. ~~**Unknown parameters: reject or ignore?**~~ **Resolved by the reference capture:** Solr
   returns `status: 0` and ignores them. Rejecting would 400 on requests real Solr serves,
   which breaks the compatibility claim — and Solr clients do routinely send extra params.
   **Ignore by default, log at debug, offer `strict_params = true`** for deployments that want
   unknown request parameters rejected.
4. ~~**Schema evolution.**~~ **Resolved as written, and implemented in issue #10:** refuse to
   start and require a reindex. Worth recording *why* there was never a softer option — Tantivy
   fixes a schema at index creation and `IndexBuilder::open_or_create` does a strict equality
   check, so **adding** a field is no more compatible than removing or retyping one; all three
   need a reindex. Wayfinder persists the schema an index was built with and refuses on any
   change, naming the field, rather than letting Tantivy fail later with "An index exists but the
   schema does not match". The comparison covers `type`/`stored`/`fast`/`multi_valued` plus
   whether `[[dynamic_fields]]` is empty (an empty-to-non-empty transition adds the catch-all
   JSON fields, so it changes the real schema); `required` is input validation and not part of
   the Tantivy schema, so toggling it is compatible.
5. ~~**Analyzer preset coverage.**~~ **Resolved by issues #10, #51, and #205:** every language
   in `tantivy::tokenizer::Language` ships, 18 in total, as `text_<code>` presets alongside
   `string`/`keyword`/`text_general`/`text_en`. Built-in `text_en` removes English stopwords
   before stemming and applies Solr's captured Porter terminal-`y` rule (`day` → `dai`, while
   `sky` remains `sky`) on static fields. Indexes built before that static-`text_en` analyzer
   change refuse startup and require a reindex. The shared
   `_dynamic_text` catch-all intentionally retains v1 Snowball behavior because the captured
   Drupal Search API configset preserves singular `day`; normal v1 dynamic indexes remain
   compatible, while pre-v1/legacy-dynamic `en_stem` indexes still fail closed before an analyzed
   rule can use them. Unaffected raw-only indexes remain adoptable. The other language presets remain stem-only,
   and custom `[[field_types]]` chains remain available for operator-selected stopword removal. Tantivy ships
   no stopword list for Arabic, Greek, Romanian, Tamil or Turkish, so a `stopwords` filter in
   those languages is a load-time error rather than a silent no-op.
