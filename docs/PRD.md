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

For the overwhelming majority of these deployments — tens of thousands to low millions of
documents, single node, one or two indexes — that is a large operational tax for a small
search problem.

**Wayfinder** is a single static Rust binary that speaks Solr's HTTP API, backed by Tantivy
for indexing and retrieval, configured by a TOML file.

### Primary goals

1. **Speed** — sub-10 ms p95 for typical facet+filter queries; near-instant cold start.
2. **Simplicity of operation** — one binary, one config file, one data directory. No JVM,
   no ZooKeeper, no config-set upload dance, no GC tuning.

### Secondary goals

3. Memory footprint an order of magnitude below Solr for the same corpus.
4. Wire-compatible enough that existing Solr clients work against it unmodified.

### Eventual target: Drupal Search API

The motivating consumer is Drupal's Search API via `search_api_solr`, but that integration
is **deliberately deferred to Phase 2** (§4). Building the engine against Solr's own
documented behaviour first keeps the design honest and avoids contorting the core around one
client's quirks. §2 explains how the two contracts relate.

### Non-goals

- SolrCloud, ZooKeeper, distributed/sharded search.
- Data Import Handler, Streaming Expressions, SQL interface, Tika/`/extract`.
- XML anything — not `schema.xml`, not `solrconfig.xml`, not `wt=xml`. See §3.

---

## 2. Compatibility contract

Wayfinder is compatible with Solr's **wire format** — request parameter names and response
JSON envelope — and deliberately *not* with its configuration format.

That split is the central design decision. The wire format is what clients depend on and is
therefore worth matching precisely. The configuration format is what *operators* deal with,
it is the single worst part of running Solr, and matching it would import the project's
largest source of complexity for no gain.

### What must match exactly

- **Response envelope.** `responseHeader{status, QTime, params}`,
  `response{numFound, start, numFoundExact, docs}`, `facet_counts{facet_queries,
  facet_fields, facet_ranges}`, `highlighting{}`, `stats{}`. Including the awkward parts —
  `facet_fields` as a flat alternating name/count array, `json.nl` handling, the shape of an
  empty result and an empty facet.
- **Error shape.** `{"responseHeader": {...}, "error": {"msg": ..., "code": ...}}` with the
  HTTP status Solr would return.
- **Parameter names and semantics**, per §4.

### Verified envelope facts

Captured from a real `solr:9` against the tracer-bullet schema (`solr-ref/capture.sh`,
gitignored — regenerate rather than trust this list if it ages):

1. `facet_fields` defaults to a **flat alternating array** — `["animals",2,"classic",2]` —
   and switches to an object under `json.nl=map`. Both shapes in scope.
2. `facet.missing=true` appends a literal `null` key to that array: `[...,"misc",1,null,1]`.
   The array is heterogeneous; model it as untyped JSON values.
3. `facet_counts` always carries all five sub-objects — `facet_queries`, `facet_fields`,
   `facet_ranges`, `facet_intervals`, `facet_heatmaps` — empty when unused. But the whole
   `facet_counts` key is **absent** when `facet` was not requested.
4. `numFoundExact` is present in Solr 9. Always `true` for Wayfinder (exact `Count`
   collector), but the key must exist.
5. `params` echoes raw request values as **strings**, even numerics (`"rows":"0"`), in
   non-request order. The differential normaliser must be order-insensitive here.
6. An unknown field in `fl` is **silently dropped** — no error.
7. Unknown request parameters are **silently ignored**, `status: 0`. See open question 3.
8. With no `fl`, docs include internal fields (`_version_`, `_root_`). Wayfinder needs its
   own explicit default-`fl` decision.
9. Errors: HTTP status matches `error.code`, `responseHeader.status` mirrors it, and
   `error.metadata` is *also* a flat alternating array. `error.msg` is free text — compare
   code and status, not the message.
10. Sorting on a non-`docValues` field is a hard 400, not a silent fallback. Same constraint
    as Wayfinder's `fast = true` requirement.
11. `responseHeader.warnings` (a JSON array of strings) appears when Solr raises the effective
    `facet.mincount` from 0 to 1 for a `facet.field` on a Points-based (numeric/date) column —
    `"Raising facet.mincount from 0 to 1, because field <name> is Points-based."`
    (`facet_field_numeric_all.json`). Absent from every other fixture, including `facet.range`
    on the same fields and `facet.field` on a string column; Wayfinder emits the same key,
    verbatim wording, under the same gate. Not a divergence — see findings 21-24.

### What deliberately differs

- Configuration is TOML (§3), not `schema.xml` / `solrconfig.xml`.
- `wt=json` only. No XML, no javabin, no `wt=phps`.
- No core admin API in v1; cores are directories.

#### Ratified divergences from captured Solr behaviour

The rule elsewhere is that any difference from a captured fixture is a bug. These are the
exceptions — knowing mismatches, each with the fixture that documents it and the reason it was
chosen. Nothing may be added here without the same two things.

1. **An unknown core returns a JSON error envelope, not Solr's 404 HTML page.**
   `err_missing_core.json` shows real Solr answering with its "Searching for Solr? You must type
   the correct path." easter egg. Wayfinder matches the 404 status and returns its normal JSON
   error. Clients parse JSON; none depends on that HTML, and serving it would mean carrying a
   second response format solely to reproduce a joke. (findings 15, issue #11)

2. **A facet on an existing but unfacetable field is a 400, where Solr returns 200 with empty
   counts.** `facet_non_docvalues_text.json` and `facet_stored_only_field.json` show Solr
   answering `status: 0` with `"<field>":[]` for a non-docValues field and for a stored-only
   field — no warning anywhere in the response. That is the one captured behaviour this project
   rejects on the merits: the client cannot distinguish "this field has no values" from "you asked
   for something impossible", which is exactly the silent-empty-counts failure the tracer-bullet
   review flagged. Tantivy cannot aggregate a non-`fast` column at all, so the honest answer is a
   hard 400 in the Solr error envelope, worded to mirror the `sort` equivalent in finding 11.
   (issue #3's finding 16, narrowed by issue #26)

   **Scope note, and a caution about how this list gets built.** As first ratified this divergence
   also covered a facet on a field that *does not exist*, on the strength of a fixture showing
   Solr returning 200 with an empty array. That fixture was captured against a container whose
   schema `capture.sh`'s own schemaless probe had polluted, so the "unknown" field existed. On a
   clean container Solr 400s, exactly as Wayfinder does, so unknown facet fields were never a
   divergence. A ratified divergence is only as good as the cleanliness of the fixture behind it —
   which is why every entry here must name its fixture, and why `capture.sh` must leave the
   reference core able to reproduce its own captures.

3. **An unknown field in an incoming document is rejected, where Solr's `_default` configset
   auto-adds it.** That configset is *schemaless* (`update.autoCreateFields` defaults to true), so
   Solr silently adds the field as `text_general` and returns 200
   (`update_unknown_field_schemaless.json`). Wayfinder matches non-schemaless Solr instead
   (`update_unknown_field_strict.json`, 400), because auto-adding is runtime schema mutation,
   which §3 rules out and Tantivy cannot do in place regardless. `[[dynamic_fields]]` is the
   supported way to accept fields not named individually. (issue #10)

Note that divergence 3 is a difference from the *configset* the reference fixtures were captured
against, not from Solr itself — a strict Solr agrees with Wayfinder. Divergences 1 and 2 are
differences from Solr proper.

### How this is verified

Differential testing against a real Solr, which needs no client and no trace — just a corpus
and a query set. Run both, diff the JSON, fail on any difference outside a normaliser for
legitimately variable fields (`QTime`, timestamps, float tolerance on scores). Details in §7.

### The Search API contract, later

`search_api_solr` generates a Solr config set and issues a bounded set of requests. When
Phase 2 begins, capture both — the generated `schema.xml` and an HTTP trace of a real site
against a real Solr — and freeze them as fixtures. That is a discovered contract rather than
a guessed one, and it becomes the Phase 2 conformance suite.

Importantly, that capture is expected to require **no XML parsing in Wayfinder** (§3).

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

An earlier draft proposed an `import-solr-config` converter for Phase 2. On inspection it is
not needed, and shipping it would be building for a problem we do not have.

`search_api_solr` does not declare a field per Drupal field. It encodes type and cardinality
into the field name — `ss_`, `sm_`, `tm_`, `its_`, `ds_`, `bs_`, and so on — and relies on
Solr's dynamic-field matching. That scheme is **fixed and mechanical**: it is a property of
the module, not of any particular site. So the entire Search API field convention expresses
as a set of `[[dynamic_fields]]` rules in one TOML preset, hand-written once, shipped in the
repo, and reused by every Drupal site.

Same for the language-specific text types the module generates — a fixed set per language,
shipped as analyzer presets.

Phase 2 therefore ships a `search-api.toml` preset, not a converter. Revisit only if a real
config set turns out to contain genuinely per-site content the preset cannot express; the
fallback then is a converter, still a setup-time tool and never a runtime XML parser.

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

Scoping rule for v1: **ship what Tantivy supports natively; roadmap the rest.** One
deliberate exception, called out below.

### v1 — native Tantivy capability

| Feature | Tantivy primitive |
|---|---|
| Schema, field types, analyzer chains, multi-valued fields | `Schema`, `TextAnalyzer`, tokenizer/filter stack |
| BM25 relevance | native, default scorer |
| `q` — boolean / term / phrase / prefix / range / fuzzy / regex | `QueryParser`, `BooleanQuery`, `PhraseQuery`, `RangeQuery`, `FuzzyTermQuery`, `RegexQuery` |
| `fq` filter queries (repeatable) | `BooleanQuery` with a non-scoring `Occur::Must` clause |
| `fl`, `start`, `rows` | stored-field retrieval, `TopDocs::with_limit(..).and_offset(..)` |
| `sort` | custom collector over fast-field column readers (field must be `fast`) — **not** `TopDocs::order_by_fast_field`, see below |
| `numFound` | `Count` collector |
| **Faceting** — `facet.field`, `facet.query`, `facet.range` + `.start`/`.end`/`.gap`, `facet.limit`, `facet.mincount`, `facet.sort`, `facet.missing` | aggregation API: `terms`, `range`, `histogram`, `date_histogram` |
| **Stats** — `stats`, `stats.field` | metric aggregations: `min`/`max`/`sum`/`avg`/`count`/`stats`/`extended_stats`/`percentiles`/`cardinality` |
| **Highlighting** — `hl`, `hl.fl`, `hl.snippets`, `hl.fragsize`, `hl.simple.pre`/`post` | `SnippetGenerator` |
| **MoreLikeThis** — `/mlt` | `MoreLikeThisQuery` + builder (min/max doc freq, min term freq, max query terms, word length, stop words, boost) |
| Field and document boosts | `BoostQuery` |
| `/update` — add, delete-by-id, delete-by-query, `commit`, `commitWithin`, `softCommit`, `overwrite` | `IndexWriter` |
| `/admin/ping` | trivial |
| Commit / merge behaviour | `IndexWriter::commit`, `IndexReader` reload policy, merge policy |

Tantivy's aggregation API is Elasticsearch-shaped, which is a good sign for this project: the
bucket/metric model is a superset of what Solr's facet component needs, so the work is
parameter translation rather than building an aggregation engine.

**Correction from issue #2 (`sort`).** This table originally paired `sort` with
`TopDocs::order_by_fast_field`. That primitive cannot implement the feature: it orders by exactly
one fast field, so it cannot express multi-clause sort, cannot mix `score` into a clause list, and
gives no control over where missing values land. `sort` is implemented instead as ordering inside
Wayfinder's own collector (`src/collector.rs`), with per-segment fast-field column readers, the
Lucene min/max selector for multi-valued fields, and missing values last in both directions. The
`fast = true` requirement is unchanged — that part was right.

### v1 exception — edismax

Not a Tantivy feature, but *composition* of Tantivy primitives rather than missing capability:
`qf` is a set of per-field `BoostQuery` clauses, `pf` a phrase clause over the same fields,
`tie` dis-max tie-breaking across the per-field scorers, `boost` a multiplicative wrapper.

- **In:** `defType`, `q`, `qf`, `pf`, `mm`, `tie`, `boost`, `bq`, quoted phrases, `+`/`-`.
- **Out:** `bf` function queries, `pf2`/`pf3`, `ps`, `stopwords`, `lowercaseOperators`, the
  full Solr function-query syntax.

`mm` is the hardest single piece — the grammar accepts absolute counts, percentages, and
conditional lists (`2<-1 5<80%`). Implement it fully; it is a small self-contained parser.

### Phases

| Phase | Contents |
|---|---|
| **POC** | The tracer bullet, §6 |
| **v1** | The table above + edismax + the differential harness |
| **v2 — Search API** | Contract capture (§2), `search-api.toml` preset, `/admin/system` version handshake, `/admin/luke`, `/terms`, `/admin/mbeans`, whatever the trace turns up |
| **v3** | Result caches + autowarm, spellcheck/suggester, grouping/collapse, atomic updates + `_version_` optimistic concurrency |
| **v4** | Function queries (`bf`, `{!func}`), spatial, snapshot-based read replicas |
| **Deep roadmap** | Distributed / sharded search, SolrCloud. The majority of Solr's complexity and directly opposed to the operational-simplicity goal. |

Notes on deferred items:

- **Caches** — Tantivy has none. Measure before building; Tantivy may be fast enough that a
  filter cache is unnecessary. If v1 latency lands materially worse than Solr, that is the
  signal to build it.
- **Spellcheck / suggester** — needs a build step over the term dictionary. Tantivy's FST and
  fuzzy support give a foundation, not the component.
- **Atomic updates** — needs stored-field read-modify-write plus a version field. Most
  clients reindex whole documents.
- **Grouping / collapse** — no native equivalent; needs a custom collector.

---

## 6. Tuning knobs

Scoped to v1 features. Server TOML plus per-request params where Solr has them.

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
- No heap tuning, by design. Tantivy is mmap-based; the OS page cache does the work Solr's
  heap sizing does. The absence is a feature and should be documented as one.

---

## 7. The tracer bullet (POC)

One thin vertical slice through every layer the finished product needs — real code, kept and
iterated on, not a spike. No Drupal in it.

**Schema:** three fields — an ID (`string`, stored), a body (`text_en`), and a `category`
(`string`, `fast`, `multi_valued`).

1. TOML schema file → Tantivy schema.
2. `POST /solr/<core>/update` — JSON add, `commit`.
3. `GET /solr/<core>/select` — `q`, `fq`, `fl`, `rows`, `start`, one `facet.field`, correct
   `numFound`, correct Solr JSON envelope.
4. `GET /solr/<core>/admin/ping`.

**Done when:** `curl` a document in, `curl` a query out, and the response matches Solr's for
the same corpus and query, modulo `QTime`.

The one non-obvious inclusion is the single `facet.field`. Faceting goes through a completely
different Tantivy path — the aggregation API over fast fields — and whether a field is `FAST`
is a *schema* decision. Leave faceting out and the slice can produce a schema layer with no
way to express it, which is exactly the layer a tracer bullet exists to prove. One facet field
forces that decision on day one for very little code.

Deliberately out: highlighting, edismax, stats, MLT, sort. All are post-processing or query
composition on a pipeline the slice already proves.

---

## 8. Conformance & benchmarking

**Differential harness.** Same corpus, same query set, real Solr vs Wayfinder, diff the JSON.
Normalise `QTime`, timestamps, and float tolerance on scores — and log every field the
normaliser touched, because an over-eager normaliser turns a green suite into a lie.

**Known limit, learned the hard way (issues #2, #11).** The harness proves *envelope*
equivalence, not *semantic* equivalence. `error.msg` is deliberately normalised away — it is free
text and outside the compatibility contract (findings 10) — so two genuinely different errors that
share an HTTP status and `error.code` diff to **zero**. Issue #2 shipped a wrong error
classification twice under a fully green harness for exactly this reason: reverting a real bug in
sort-clause validation produced no diffs at all. A green run is therefore necessary but not
sufficient for anything error-shaped, and every issue that produces errors (#4–#9) inherits this.
The mitigation is per-feature: reduce a message to its *class* and compare that class against the
fixture, so the fixture decides which error is correct without freezing either side's wording —
see `sort_error_class()` in `tests/sort.rs`.

**The matching trap in the other direction, now closed (issue #25).** Comparing parsed `Value`s
cannot detect key-order divergence: parsing discards object order, and `serde_json` was originally
built *without* `preserve_order`, so every object Wayfinder emitted was alphabetised. Solr's order
is meaningful throughout — it serialises `SimpleOrderedMap`/`NamedList`, giving
`responseHeader, response, facet_counts` at the top, `status, QTime, params` in the header,
`numFound, start, numFoundExact, docs` in the response, `metadata, msg, code` in an error,
`counts, gap, start, end` in a range facet, and under `json.nl=map` the facet order itself as the
object's key order. `serde_json` is now built with `preserve_order`, every construction site already
lists its keys in Solr's order, and the emitted order reproduces Solr's (findings 21–25).
Enabling the feature does not weaken any existing assertion: `IndexMap`'s `PartialEq` compares as a
map, so `Value == Value` — and therefore `assert_matches_fixture` and `tests/common/diff.rs` —
stays order-*insensitive* and keeps exactly its previous meaning. The guard is consequently a
separate, order-*sensitive* suite, `tests/json_key_order.rs`, which reads key order out of the
document bytes via a hand-written `Deserialize` over `MapAccess` (`tests/common/key_order.rs`) so
it cannot be neutered by a feature-flag change; it carries self-tests pinning that property and a
tripwire test that names `preserve_order` if someone drops it. One path is permanently exempt:
`responseHeader.params`, whose order in Solr is Java `HashMap` iteration order — neither request
order nor alphabetical, and not reproducible by any implementation (findings 6, 26). The
differential normaliser is order-insensitive there for the same reason.

Without a captured client trace, the query set is written deliberately rather than observed,
so weight it toward the edges: zero results, empty facets, pagination past the end,
multi-valued facets, facet with `mincount` filtering everything out, sort on each field type,
missing field in `fl`, malformed params. Those are where a hand-rolled response envelope
diverges from Solr's, and where a strict client throws a fatal error instead of showing an
empty result set.

Build this early. It is not in the tracer bullet, but it is the thing that tells you the
envelope is right, and retrofitting it is more expensive than starting with it.

**Relevance check.** Both engines use BM25, so ranking should be close; drift comes from
analyzer-chain differences. Compare ranked ID lists, not just result sets.

**Benchmark.** Two corpus sizes: 50 k documents and 2 M. Targets, to be revised once real
numbers land:

| Metric | Solr baseline | Wayfinder target |
|---|---|---|
| Resident memory, idle | ~1 GB | < 50 MB |
| Resident memory, 2 M docs under query load | 2–4 GB | < 500 MB |
| Cold start to first query served | 10–30 s | < 1 s |
| p95 query latency (facet + filter + highlight, 50 k docs) | baseline | ≤ baseline |
| Container image size | ~500 MB | < 30 MB |
| Index size on disk | baseline | ≤ 1.2× baseline |

Latency parity is the honest v1 target. Solr is mature and fast; memory, startup, and
operational simplicity are where this project wins, and those are the primary goals.

---

## 9. Risks

| Risk | Mitigation |
|---|---|
| Response-envelope fidelity is harder than it looks — Solr has accreted odd shapes (`facet_fields` alternating arrays, `json.nl`, inconsistent empty-value handling) | The differential harness, built early, with edge-weighted queries |
| Relevance drifts visibly from Solr on migrated corpora | Both are BM25; drift comes from analyzer chains. Test ranked ID lists explicitly, not just membership. |
| `softCommit` semantics differ enough to surprise clients that expect NRT | Document plainly; test the reindex/update flows a real client performs |
| Phase 2 finds Search API needs something structurally absent (a component, a response field with no Tantivy-side source) | Capture the contract at the *start* of Phase 2, before building against it. The v1 engine is useful regardless. |
| Scope creep toward being a general Solr replacement | The non-goals in §1 and the phase table in §5. Every request gets asked: does the target use case need this? |
| One `IndexWriter` per core becomes an indexing bottleneck | It is a Tantivy constraint, not a choice. Measure at 2 M docs; if it binds, the answer is batching, not architecture. |

---

## 10. Open questions

1. **Multi-core vs single-core-per-process.** Multi-core is more Solr-like; one process per
   core is simpler and better matches the operational-simplicity goal. Lean
   single-core-per-process unless something forces otherwise.
2. **Which Solr version to report** from `/admin/system`. Phase 2 question — determined by
   which feature gates in `search_api_solr` are cheapest to satisfy. Report the lowest version
   whose feature set is fully implemented rather than claiming a high one and failing on
   unsupported parameters.
3. ~~**Unknown parameters: reject or ignore?**~~ **Resolved by the reference capture:** Solr
   returns `status: 0` and ignores them. Rejecting would 400 on requests real Solr serves,
   which breaks the compatibility claim — and Solr clients do routinely send extra params.
   **Ignore by default, log at debug, offer `strict_params = true`** for development, so gaps
   are still discoverable during the Search API phase.
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
5. ~~**Analyzer preset coverage.**~~ **Resolved by issue #10:** every language in
   `tantivy::tokenizer::Language` ships, 18 in total, as `text_<code>` presets alongside
   `string`/`keyword`/`text_general`/`text_en`. Presets stem but do not strip stopwords, matching
   `en_stem`'s shape; stopword removal is available through a custom `[[field_types]]` chain.
   Tantivy ships no stopword list for Arabic, Greek, Romanian, Tamil or Turkish, so a `stopwords`
   filter in those languages is a load-time error rather than a silent no-op.
