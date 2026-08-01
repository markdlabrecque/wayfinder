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

The motivating consumer is Drupal's Search API via `search_api_solr`, and the *integration*
is deliberately sequenced after v1 (§5). Building the engine against Solr's own documented
behaviour first keeps the design honest and avoids contorting the core around one client's
quirks.

The module's request set, however, is more than one client's quirks: it is the best available
evidence of which Solr features real applications depend on, so capturing it is phase v1.5 and
feeds the roadmap rather than only the test suite. §2 explains how the two contracts relate.

### Non-goals

- SolrCloud, ZooKeeper, distributed/sharded search.
- Data Import Handler, Streaming Expressions, SQL interface.
- XML anything — not `schema.xml`, not `solrconfig.xml`, not `wt=xml`. See §3.

Document extraction was originally part of the second bullet as “Tika/`/extract`.” Issue #171
revisited that decision because the vendored `search_api_solr` client has a concrete
`extractContentFromFile()` path to `/update/extract`, even though the captured site did not have
an attachments integration configured to exercise it. The revised boundary admits extraction to
the roadmap **only as an in-process Rust feature with no JVM, Tika server, sidecar, or separately
operated service**. Well-maintained Rust parsing crates are ordinary implementation dependencies,
not third-party runtime services; hand-writing PDF and Office parsers is not a goal. OCR remains
out of scope: image-only documents may validly yield no text.

---

## 2. Compatibility contract

Wayfinder is compatible with Solr's **wire format** — request parameter names and response
JSON envelope — and deliberately *not* with its configuration format.

That split is the central design decision. The configuration format is what *operators* deal
with, it is the single worst part of running Solr, and matching it would import the project's
largest source of complexity for no gain.

### The goal is coverage, not identity

Wire compatibility is **an adoption mechanism, not the product**. The target is to serve the
great majority of what real Solr deployments actually do — call it 75-80% of use cases — so
that an existing user switches without touching their client. It is not to reproduce Solr's
response bytes for their own sake.

The distinction matters because two different things get called "Solr compatibility":

- **Solr as a design teacher.** Twenty-one years of accumulated judgment about what a search
  backend needs: the field-type and analyzer model, what faceting has to return, `mm`'s
  conditional grammar, commit-visibility semantics, missing-values-last on sort. This is the
  valuable inheritance, it is why the engine is built against Solr's documented behaviour
  first, and Wayfinder keeps it regardless of what happens to the wire.
- **Solr as a wire protocol.** Param names and envelope shape. Much less of this is wisdom
  than it appears — `facet_fields` as a flat alternating name/count array is a 2006 decision
  nobody could undo, not an insight. Reproducing it buys client compatibility and nothing else.

Consequences, and they are the point of this section:

1. **Fidelity is weighted by what clients exercise.** A response path a real client generates
   is held exactly. A path no client reaches is worth matching only if it is cheap. The
   differential harness (§7) is evidence that the exercised paths work — not a claim of
   byte-identity across Solr's whole surface.
2. **Divergence on the merits is policy, not apology.** Where Solr's captured behaviour is
   actively worse for clients, Wayfinder may depart from it — recorded in the ratified list
   below, with its fixture and its reason. That list is a design record, not a confession.
3. **Coverage has to be measurable or it will drift.** "75-80%" is not an assertion, it is a
   number to compute; the instrument is in §5's Phases notes.

None of this loosens the rule that an *unintended* difference from a captured fixture is a bug.
Divergence is a decision someone makes and writes down, never something a normaliser absorbs.

### What must match exactly

For the request and response paths real clients exercise:

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
    verbatim wording, under the same gate, and leads `responseHeader` with it (`warnings, status,
    QTime, params`), matching the fixture's key order. Not a divergence — see findings 27-30.

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
   (issue #3's finding 105, narrowed by issue #26)

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

4. **`fl=score`'s BM25 float magnitude does not match Solr's; doc ranking order and the wire
   shape do.** `select_term_scored.json` and `select_quick_scored.json` show Tantivy's BM25
   score for a doc diverging from Solr's captured BM25 score by a non-constant ~1.9x-2.3x
   ratio (e.g. `select_term_scored`'s `doc2`: Tantivy ~0.875 vs. Solr's captured 0.457;
   `select_quick_scored`'s `doc3`: Tantivy ~0.940 vs. Solr's captured 0.413) — the ratio moving
   per-doc rules out a missing constant factor and points at an internal scoring-formula
   difference (idf / norm-encoding) between Tantivy's and Solr/Lucene's BM25Similarity, not a
   wiring bug. Doc *ranking order* already matches Solr exactly for both fixtures. Per
   `CLAUDE.md`'s compatibility contract ("Wire format only... Never Solr's config format"),
   Wayfinder's compatibility obligation is to Solr's wire semantics, not to reproducing another
   engine's internal ranking math bit-for-bit, so `fl=score`'s obligation is: the `score` key is
   present/positioned/typed correctly and `response.maxScore` is present/typed correctly (both
   gated on `fl` requesting `score`), and ranking order matches — not that the float values
   themselves match. (issue #34)

5. **`/admin/info/system` and `/admin/system` report a configured Solr version, not the
   captured Solr's own `9.10.1`, and serve static `jvm`/`system` placeholders instead of host
   introspection.** `admin_info_system.json` and `admin_system.json` are verbatim copies of the
   captured envelopes; the differences are `lucene.solr-spec-version`/`-impl-version` (config
   choice, default `"9.0.0"` — resolves open question 2 above) and `jvm.*`/`system.*` (real host
   JVM/OS stats with no Wayfinder equivalent to introspect). `core.schema` is **not** a
   divergence: it is compared exactly against `"drupal-4.4.0-solr-9.x-0"`, because
   `search_api_solr`'s `SolrConnectorPluginBase.php` `explode('-', $schema)`s that value and
   indexes into it (finding 78) — getting it wrong breaks the client, not just cosmetics.
   (issue #59)

6. **A local-params block in `q` naming any query parser other than `edismax` is a hard 400, where
   Solr parses it.** Real Solr registers a parser per type, so `{!lucene}quick`, `{!term f=id}doc1`
   and `{!func}...` all parse; Wayfinder recognises `{!edismax ...}` only and answers everything
   else with a `SyntaxError` 400 in the Solr error envelope. The evidence is the v1.5 capture rather
   than a per-shape fixture, and that is stated plainly here because the rule above demands it: the
   only local-params types the captured client ever sends are `{!edismax qf='...'}` in `q` (7 traces)
   and `{!key=...}` in `facet.field` (2 traces) across the 28 committed traces in
   `solr-ref/search-api/trace/`, so no fixture exercises `{!lucene}`/`{!term}`/`{!func}` at all and
   real Solr's answers for them are documented, not captured. Two reasons this is the right call
   anyway. First, **it is not a regression and this PR did not introduce it**: before issue #137,
   `q={!lucene}quick` already 400d, because `tantivy::query_grammar::parse_query` rejects the raw
   `{!` string outright (`{` opens an exclusive range in Tantivy's grammar) — issue #137 changed the
   error *message*, not the status. Second, `{!func}` is **v4** scope (§5's phase table, "Function
   queries (`bf`, `{!func}`)") and v1 has no function-query evaluator, so a `{!func}` block that
   parsed into something would silently half-work — the accept-and-ignore treatment `bf` gets is
   defensible for an optional relevance-tuning param and indefensible for the query itself. A 400
   is the honest answer until the parser it names exists. (issue #137's open question 5; findings
   90-92)

7. **Colliding `facet.field` response labels are a hard 400, where Solr returns 200 with duplicate
   JSON object members.** `facet_collision_field_flat.json` and
   `facet_collision_field_map.json` show `{!key=x}category` plus `{!key=x}id` producing two literal
   `"x"` members in request order; `json.nl=map` changes only each member's bucket shape. That
   response cannot be represented by a normal JSON object model, and common parsers silently keep
   only one member — recreating the data loss this edge case is meant to prevent. Wayfinder
   therefore rejects the ambiguous request in its normal 400 error envelope rather than emitting
   duplicate keys or silently choosing a facet. This is deliberately narrow: the companion
   `facet_collision_query_flat.json` and `facet_collision_query_map.json` fixtures show Solr itself
   coalescing duplicate identical `facet.query` values, which Wayfinder matches. (issue #149;
   finding 102)

8. **An invalid `omitHeader` value returns Wayfinder's JSON error envelope, where Solr returns
   Jetty HTML.** `omit_header_invalid_one.html` shows `omitHeader=1&wt=json` failing before Solr's
   JSON response writer runs, with HTTP 400 and `invalid boolean value: 1` embedded in an HTML
   page. Wayfinder matches the 400 and rejects the same vocabulary, but keeps the response in its
   normal headerless JSON error shape. This is the same client-facing choice as divergence 1:
   clients parse JSON, Wayfinder supports only `wt=json`, and reproducing a servlet container's
   fallback HTML would add a second error format solely for parser failures. Valid
   `omitHeader=true`/`yes`/`on` still suppresses `responseHeader` on success and error envelopes,
   matching `omit_header_error_true.json`, `omit_header_error_yes.json`, and
   `omit_header_update_error_true.json`. (issue #179; finding 109)

Note that divergence 3 is a difference from the *configset* the reference fixtures were captured
against, not from Solr itself — a strict Solr agrees with Wayfinder. Divergences 1, 2, 4, 6, 7,
and 8 are differences from Solr proper. Divergence 5 is a deliberate config choice plus inherent
host non-reproducibility, not a Solr-behaviour disagreement.

### How this is verified

Differential testing against a real Solr, which needs no client and no trace — just a corpus
and a query set. Run both, diff the JSON, fail on any difference outside a normaliser for
legitimately variable fields (`QTime`, timestamps, float tolerance on scores). Details in §7.

### The Search API contract, next

`search_api_solr` generates a Solr config set and issues a bounded set of requests. Capture
both — the generated `schema.xml` and an HTTP trace of a real site against a real Solr — and
freeze them as fixtures. That is a discovered contract rather than a guessed one, and it serves
twice over: as the v2 conformance suite, and as the coverage denominator this section's target
is measured against. It is phase v1.5 (§5) for that reason — the scope it defines is needed
before the work it scopes.

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

An earlier draft proposed an `import-solr-config` converter for the Search API phase (v2, §5).
On inspection it is
not needed, and shipping it would be building for a problem we do not have.

`search_api_solr` does not declare a field per Drupal field. It encodes type and cardinality
into the field name — `ss_`, `sm_`, `tm_`, `its_`, `ds_`, `bs_`, and so on — and relies on
Solr's dynamic-field matching. That scheme is **fixed and mechanical**: it is a property of
the module, not of any particular site. So the entire Search API field convention expresses
as a set of `[[dynamic_fields]]` rules in one TOML preset, hand-written once, shipped in the
repo, and reused by every Drupal site.

Same for the language-specific text types the module generates — a fixed set per language,
shipped as analyzer presets.

v2 therefore ships a `search-api.toml` preset, not a converter. Revisit only if a real
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

Scoping rule from v2 onward: **ship what `search_api_solr` demonstrably uses; everything else
Solr 9.x offers is unscheduled, on the Solr 9.x parity roadmap below.** The evidence base is
the v1.5 capture (`coverage/search_api_coverage_contract.json`) plus the vendored module source
(`coverage/search_api_solr_4.4.0_source`) for features the capture site did not have configured
(autocomplete, spatial, grouping). This generalises calls the PRD had been making one at a time —
the edismax six-param descope (issue #136), the `terms`/`admin/luke`/`admin/mbeans` descope
(issue #57), the `_version_` narrowing — into the default rule, so each future descope no longer
needs its own argument from first principles, only its evidence.

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
`tie` dis-max tie-breaking across the per-field scorers, `boost` a multiplicative wrapper around
the composed query — for a constant multiplier only, see below.

- **In:** `defType`, `q`, `qf`, `pf`, `mm`, `tie`, `bq`, quoted phrases, `+`/`-`, and `boost`
  restricted to a constant numeric multiplier.
- **Out:** `pf2`/`pf3`, `ps`, `stopwords`, `lowercaseOperators`. `bf` and the full Solr
  function-query syntax are **not** a second, independent v1 exclusion: their single
  disposition is the **v4** "Function queries (`bf`, `{!func}`)" line in the phase table
  below. v1 has no function-query evaluator at all, so a function-query argument is
  accepted-and-ignored rather than rejected (findings 75 and 83).

**`boost` is constant-only, deliberately.** Real Solr's `boost` is a *function-query* parameter,
not a plain float (finding 83, `docs/solr-ref-findings.md`), and Wayfinder implements no
function-query evaluator. Only the constant-numeric form (`boost=2.5`) is therefore applied; a
function-query form such as `boost=recip(rord(title),1,1000,1000)` parses to no boost and is
ignored, the same accept-and-ignore treatment `bf` gets, and lands properly with **v4**'s
function queries. Listing `boost` as flatly **In** would overstate what v1 does.

**The Out items are ratified by the v1.5 capture, not merely undemanded (issue #136).** Across
the 28 committed Drupal traces in `solr-ref/search-api/trace/`, client usage of `bf`, `pf2`,
`pf3`, `ps`, `stopwords` and `lowercaseOperators` is **zero** — the module's whole edismax
surface is `{!edismax qf='...'}` inside `q`, and it emits none of `mm`, `tie`, `pf`, `bq`, or
`boost` either, all of which v1 already implements. (The edismax *param* `stopwords` is what is
descoped; the analyzer *filter* of the same name is implemented and appears in captured schema
responses.) None of the six appears in the `captured_parameters` denominator in
`coverage/search_api_coverage_contract.json`, so building any of them moves the coverage
fraction by zero. Per the capture's role as a scoping input that decides what to build (see "Why
the capture moved to its own phase" below), zero usage plus zero coverage movement is a positive
reason to keep them out. `tests/edismax_descope_guard.rs` is the expiring guard: it fails the day
a new trace or a regenerated coverage contract mentions any of the six, which is the signal to
revisit this descope (issue #136) rather than silently keep it.

**The client's own `{!edismax}`-inside-`q` shape is reproduced bug-compatibly, low recall included
(issue #137).** `search_api_solr` does not send `defType=edismax`; it sends the whole query as
`q=({!edismax qf='...'}+"quick" +"rocket")`. The captured `/select` handler defaults
(`solr-ref/search-api/configset/solrconfig_extra.xml`) are `defType=lucene`, `df=id`, so the
**outer** parser is lucene and `{!edismax ...}` is an *inline nested query*, not a position-0
local-params block that would re-select the parser for the whole `q`. Solr binds only the **next
whitespace-delimited run** of characters after `}` to the nested parser — also terminated by a `)`
that closes a paren opened *before* the run, which matters because every captured `q` wraps the
whole query in `(...)` (finding 91) — and the remainder is parsed by the outer lucene parser
against `df=id` and matches nothing. Wayfinder reproduces that binding rule **including its
low-recall outcome**: traces 00004/00008 (`+"quick" +"fox"`) return `numFound: 0` even though the
document `entity:node/1` contains both terms, because `+"fox"` never reaches edismax. Findings 90,
91 and 92 and the per-trace `numFound` table in finding 90 carry the evidence; all seven captured
Shape-B traces fit this model and only this model.

That is **deliberate fidelity, not a defect.** Matching captured Solr is the contract (§2), and the
"obviously more useful" high-recall reading — hand the whole remainder to edismax — would be a
**divergence**, so it could only ship after being ratified in §2's list with a fixture behind it.
Issue #137's own title ("so keyword search works") is wrong-premised on this point and is recorded
as such: keyword search does not start working for a Shape-B client, it starts failing the way real
Solr fails, and the fix belongs upstream in `search_api_solr`. The two `numFound == 0` assertions in
`tests/local_params.rs` are the guard against a later well-meaning rewrite to high recall.

`mm` is the hardest single piece — the grammar accepts absolute counts, percentages, and
conditional lists (`2<-1 5<80%`). Implement it fully; it is a small self-contained parser.

### Phases

| Phase | Contents |
|---|---|
| **POC** | The tracer bullet, §6 |
| **v1** | The table above + edismax + the differential harness |
| **v1.5 — the capture** | The `search_api_solr` contract capture (§2), pulled ahead of the rest of v2: generated config set + HTTP trace of a real Drupal site, frozen as fixtures, plus the coverage denominator computed from them |
| **v2 — Search API** | `search_api_wayfinder` connector module (issue #57, done), `search-api.toml` preset (done), `/admin/system` version handshake (done). `/admin/luke`, `/terms`, `/admin/mbeans` were descoped here and are now back in scope under v2.75 — see below. |
| **v2.5 — Admin web UI** | A read-only operator dashboard, server-rendered by the same binary. Tracer bullet (core view, issue #94) done. See below. |
| **v2.75 — the contract's remaining endpoints** | The four endpoints in the coverage denominator that Wayfinder does not yet serve: `/terms` (#155), `/schema/fieldtypes` (#156), `/admin/luke` (#157), `/admin/mbeans` (#158). Completing them closes the endpoints bucket. See below. |
| **Document extraction — staged** | First the client-evidenced `extractOnly=true` path for plain text and HTML, behind request/concurrency/output limits; then DOCX/PPTX and spreadsheet/ODF/RTF families; PDF only after a separate parser-quality and cancellation decision. See below. |
| **v3** | Result caches + autowarm, spellcheck/suggester (the module's autocomplete path: `spellcheck.*`, `suggest`; `terms` moved earlier to v2.75), grouping (`group=true` — see note below on why "collapse" left this line), `_version_` (issue TBD — scope narrowed, see below) |
| **v4** | Function queries (`bf`, `{!func}`) — client-evidenced: the module's `BoostMoreRecent` processor emits `product(…,recip(ms(…)))` — spatial (`{!geofilt}`, `bbox`, `{!frange}geodist()`, heatmap facets), snapshot-based read replicas |
| **Solr 9.x parity** | Solr features with zero client evidence, deliberately unscheduled — the table below. |
| **Deep roadmap** | Distributed / sharded search, SolrCloud. The majority of Solr's complexity and directly opposed to the operational-simplicity goal. |

**Why the capture moved to its own phase.** `search_api_solr` is not merely the motivating
client — it is a fifteen-year empirical answer to *which subset of Solr real applications use*,
validated across tens of thousands of sites. Under §2's coverage framing that makes the capture
a **scoping input, not just a conformance suite**: it decides what to build, not only what to
test. Two things follow. First, it has to land before the endpoints it would justify, so v2's
endpoint list is deliberately conditional — building `/terms` or `/admin/mbeans` because Solr
has them is the old framing. Second, it informs work already in flight: the module leans on
edismax, so the trace bears on issue #7's `qf`/`pf`/`mm` handling.

Capture against **stock upstream `search_api_solr`, unmodified** — the connector module is the
extension point and must not be in the loop while ground truth is being established.

**v2's conditional endpoint clause, resolved by descoping rather than by building.** The capture
(finding 76, `docs/solr-ref-findings.md`) confirmed the module does call `terms`, `<core>/admin/luke`,
and `<core>/admin/mbeans` — so the trace answered the "whichever" question above. But issue #57's own
scoping doc (`docs/plans/57-search-api-wayfinder-backend.md`) narrowed v2 further than the trace alone
would: the connector module implements only what the Wayfinder *server* already exposes, and the
server has no `terms`/`admin/luke`/`admin/mbeans` endpoints. Building the client side of three
endpoints the server can't answer would be exactly the "stub methods for later" the plan doc rules
out, so all three are out of scope for #57, not merely deferred silently. `terms` backs the module's
autocomplete/suggester path (`search_api_autocomplete`, not installed in the capture); `admin/luke`
and `admin/mbeans` back Search API's own schema-browsing/server-stats admin screens — none of the
three sit on the query or index path v1-v2 already cover. Revisit if a later phase adds server-side
support for any of them; until then this is a recorded, deliberate gap, not an open PRD commitment.

**v2.75: that revisit condition has fired, and the gap is now being closed.** The descope above
was conditional on the server not exposing these endpoints, which is a circular reason once the
question becomes whether the server should. Four endpoints in the coverage denominator remain
unserved — `terms` (#155), `schema/fieldtypes` (#156), `admin/luke` (#157), `admin/mbeans` (#158)
— and all four are now in scope. This does not reopen the general rule: they are in because the
capture shows the module calling them, which is exactly §5's client-evidence test, and the rest
of Solr's admin and schema surface stays on the parity roadmap.

`schema/fieldtypes` is the one whose absence does active harm rather than leaving a screen blank.
The module's `getSchemaLanguageStatistics()` catches the 404 per language and degrades to
"unsupported", so a working Wayfinder currently reports every language as unsupported on Drupal's
server-status screen.

**Deliberate divergence: three of the four answer with an honest subset.** Solr's `admin/luke`
(7.5 KB), `admin/mbeans` (48 KB) and `schema/fieldtypes` (24 KB) responses are mostly Lucene and
JVM identity — analyzer chain classes, per-field index flag strings like `ITS-----OF-----`,
directory implementation names, heap accounting, per-handler timers. Wayfinder has no such
internals, and the client reads a handful of leaves from each: one field from `luke`
(`index.numDocs`), six from `mbeans`, and field-type *names* only from `fieldtypes`. Wayfinder
therefore serves real values where a real consumer exists and static plausible placeholders
elsewhere, following the precedent `/admin/info/system` already set, and omits the Lucene-internal
keys rather than fabricating them. This is a recorded divergence from captured Solr behaviour, not
a bug: none of the three carries a `solr-ref/manifest.tsv` row, because a differential-harness row
could only ever be a permanent `EXPECTED_DIVERGENCES` entry. `terms` is the exception — it is real
index data Wayfinder genuinely has, so it is expected to match, and a manifest row for it is a
legitimate follow-up once a capture against the differential core exists.

Two further `admin/mbeans` specifics, recorded here rather than left to code comments (issue
#158). First, `UPDATE.updateHandler.softAutoCommitMaxTime` is a key the client *does* read, and
Wayfinder matches Solr's wire form exactly: the string `"<N>ms"` built from the configured
millisecond value, and no key at all when soft autocommit is unset. Both halves come from the
capture and the consumer, not from convenience. `solr-ref/search-api/trace/00025.json` renders it
as the string `"5000ms"` (with `autoCommitMaxTime` alongside it as `"15000ms"`), so a bare integer
would be a divergence; and the `-1` an earlier draft of this paragraph promised for the unset case
is the *Drupal module's own* default for a missing key, never a value Solr puts on the wire — the
`isset($update_handler_stats['UPDATE.updateHandler.softAutoCommitMaxTime'])` guard around
`$max_time = -1` in `coverage/search_api_solr_4.4.0_source/src/SolrConnector/SolrConnectorPluginBase.php:781-798`
only fires because Solr omits the key entirely when soft autocommit is off, which is what Wayfinder
does too. Second, this handler treats `stats` as truthy on a
`true` *prefix* rather than the usual exact `== "true"`: the captured request
(`solr-ref/search-api/trace/00025.json`) sends `stats=true?omitHeader=false`, because the module
concatenates a handler string that already carries a query onto Solarium's params, and the
captured response shows Solr honoured it. Exact equality would answer the real client with an
empty status report.

The honesty constraint cuts both ways. `schema/fieldtypes` must list exactly the languages
Wayfinder really stems, which is the 18 in `schema.rs`'s `LANGUAGES` table (English plus 17
non-English `text_<code>` presets) -- not the 16 an earlier draft of this section and issue #156
both claimed; `LANGUAGES` also carries `ta` and `tr`, and `resolve_type` accepts them, so the
handler reports 18 and the tests assert 18. The count matters because the module turns a name in
that list into a green "supported" row. Padding it would convert today's misreport-downward into a
misreport-upward, which is worse: nobody investigates green; under-reporting `ta`/`tr` would hide
two languages Wayfinder genuinely supports. The endpoint's recorded divergences are therefore an
omission *and* an addition: it omits the Lucene analyzer chains
(`indexAnalyzer`/`queryAnalyzer`/`analyzer`), which Wayfinder cannot describe truthfully, and it
adds `indexed`/`stored`/`multiValued`/`docValues` to every entry where Solr emits them sparsely,
because those four are Wayfinder's real uniform type-level defaults and no client reads them.

`admin/luke` (#157) lands under the same rule. Its `index{}` block reports real values for the
five figures that describe the core's contents -- `numDocs`, `maxDoc`, `deletedDocs`,
`hasDeletions`, `segmentCount`, all read per request off the same searcher `/select` answers
from -- and static placeholders for the Lucene-identity keys (`version`, `current`, `directory`, `segmentsFile`,
`segmentsFileSizeInBytes`, `userData`); `indexHeapUsageBytes` and `lastModified` are omitted, as
real Solr omits them in the captured trace. Its recorded divergences in `fields{}` are again an
omission and an addition: it omits the per-field `schema`/`index` flag strings (Lucene `FieldInfo`
bits), `docs`, `topTerms` and `histogram`, and adds `indexed`/`stored`/`multiValued`/`docValues`/
`required` booleans, which carry the same information the flag string encodes but as real values
from the live `[[fields]]` schema rather than a fabricated bitmask. Dynamic-field *instances* do
not appear in `fields{}`: Wayfinder stores every dynamic value in the shared `_dynamic` container,
so there is no per-instance field in the index to enumerate.

**The coverage instrument.** The capture yields the denominator: the set of params, endpoints,
and response fields the module can emit across its configured features. Coverage is the fraction
of that set Wayfinder serves, computed from the fixtures rather than asserted. Report it
alongside the differential results (§8). A number that can be recomputed is what keeps
"75-80%" from becoming a slogan.

Notes on deferred items:

- **Caches** — Tantivy has none. Measure before building; Tantivy may be fast enough that a
  filter cache is unnecessary. If v1 latency lands materially worse than Solr, that is the
  signal to build it.
- **Spellcheck / suggester** — needs a build step over the term dictionary. Tantivy's FST and
  fuzzy support give a foundation, not the component.
- **Atomic updates + `_version_`** — narrowed by evidence to just `_version_`; see the "v3 —
  `_version_`" subsection below.
- **Grouping** — no native equivalent; needs a custom collector. This line originally said
  "grouping/collapse", but the module's `collapse`-named identifiers (`setGrouping()`'s
  `$collapse_field` loop) drive Solr *grouping* (`group=true&group.field=…`), not the
  Collapse/Expand component (`fq={!collapse}` + `expand=true`), which the client never sends —
  Collapse/Expand moved to the Solr 9.x parity table below.

### Document extraction — accepted direction, staged delivery

The evidence is source-level rather than trace-level. The vendored
`SearchApiSolrBackend::extractContentFromFile()` constructs a Solarium Extract query, sets
`extractOnly=true`, chooses XML or text extraction, uploads the file, and reads the extracted
content. None of the 28 captured Search API requests calls `/update/extract`, because the capture
site did not configure an attachments integration. This is still a real client emission path,
but it weights the initial wire scope toward `extractOnly` rather than Solr Cell's much broader
server-side indexing surface.

The retained tracer bullet is therefore a multipart `POST /solr/{core}/update/extract` for plain
text and HTML, returning the captured `extractOnly=true` envelope. A following slice may apply
`literal.<field>` and `fmap.<from>` and feed the existing update pipeline; those params are not
required by the evidenced client path. The proposed format order is:

1. Plain text and HTML, including charset decoding and a budgeted incremental HTML token sink
   rather than an unbounded DOM.
2. DOCX and PPTX through bounded ZIP + streaming XML; XLSX and ODS through a spreadsheet reader;
   then ODT/ODP and RTF.
3. PDF in its own issue. PDF text fidelity (font encodings, CMaps, ligatures, layout) and opaque
   parser CPU behaviour make it qualitatively riskier than zipped XML. Image-only PDF OCR is out.

Resource limits are an architecture prerequisite, not a post-launch hardening task. At minimum:
request bytes, concurrent extraction count, extracted-character count, archive entry count,
per-entry and cumulative uncompressed bytes, and compression ratio. Extraction must run off the
async request executor. An HTTP timeout around a blocking in-process parser is not cancellation —
the work continues after the response — so formats whose parser cannot enforce a cooperative
budget, especially PDF, do not ship until that limitation is explicitly resolved. Metadata starts
narrowly with stable keys needed by the envelope (`resourceName`, detected content type, and the
format's title/author when reliable); unknown metadata is dropped unless a later indexing slice
maps it through `fmap`/`uprefix`.

Issue #171's dependency survey recommends maintained, permissively licensed Rust crates rather
than hand-written format implementations: `chardetng` + `encoding_rs`, `html5ever`, `zip` +
`quick-xml`, `calamine`, and `rtf-parser`; `pdf-extract`/`lopdf` remain candidates for the separate
PDF issue. Exact versions, licenses, evidence, captures, and rejected alternatives are recorded in
`docs/reports/2026-08-01-text-extraction-exploration.md`.

### Solr 9.x parity roadmap — zero client evidence, deliberately unscheduled

The remainder of Solr 9.x's feature surface, checked against the same evidence base as the
descopes above: zero hits in the coverage contract's parameter denominator, and zero emission
sites in the module source — both the vendored 4.4.0 core (`coverage/search_api_solr_4.4.0_source`)
and upstream's submodules (autocomplete, admin, devel), swept for each row below. None of these
has a phase. The only reason any of them would ever be built is **Solr 9.x wire parity as a goal
in itself** — which §2 explicitly says it is not ("an adoption mechanism, not the product") — or
a new client whose capture shows real usage, the same bar every descope above already carries.

| Solr 9.x feature | Note |
|---|---|
| JSON Request API / JSON Facet API | The module speaks classic `facet.*` params exclusively; `json.facet` appears nowhere in its source. |
| `facet.pivot`, `facet.interval` | Only field, query, and heatmap faceting is emitted. `facet.range` is equally unemitted, but v1 already shipped it — kept as surplus, not unshipped. |
| Collapse & Expand (`fq={!collapse}`, `expand=true`) | The module's "collapse" identifiers drive Solr grouping (`group=true`), which stays in v3. |
| Query Elevation (`/elevate`) | Relevance tweaks travel as `bq`/`boost` function queries; no elevation params. |
| Block join / nested documents (`{!parent}`, `{!child}`) | The module indexes flat documents; even date ranges are a custom field type, not child docs. `_root_` remains envelope-shape only (§2 fact 8). |
| Realtime Get (`/get`) | |
| TermVector component (`/tvrh`) | |
| `cursorMark` deep paging | Pagination is plain `start`/`rows`. |
| Learning to Rank (`{!ltr}`) | |
| Result clustering (Carrot2) | |
| Tagger handler (`/tag`) | |
| Atomic updates & optimistic concurrency (`set`/`inc`/…, `versions=true`, 409-on-stale) | Already descoped with evidence in the v3 `_version_` subsection; listed here so the parity picture is complete in one place. |

Features that are *also* client-unused but already ruled out as §1 non-goals — SolrCloud,
streaming expressions, the SQL interface, and XML/javabin response writers — stay non-goals rather
than moving here: a non-goal is a stronger statement than unscheduled. `/update/extract` is no
longer in that list: issue #171 found a direct emission path in the vendored client source and the
staged in-process boundary is recorded above.

This table is scoped to features a single-node Solr 9.x serves on the wire. It does not
enumerate operational subsystems with no Wayfinder analogue (replication handler, CDCR, metrics
API, security plugins) — those fall under §1 non-goals or §5's deep roadmap wholesale.

### v2.5 — Admin web UI

Solr operators get `/solr/#/`, an AngularJS console bundled with the JVM process, for free. Wayfinder
has no equivalent today — the only way to see what a running instance is doing is `curl`. This phase
closes that gap, scoped deliberately small: **a read-only dashboard, nothing more,** landed after v2
because by then there are real cores (Search API sites) worth looking at, though nothing here
functionally depends on v2 or blocks on it.

**In scope (v1 of this phase):**

- **Core view** — name, doc count, on-disk size, field count for the one core this process
  serves (§10 open question 1, resolved: single-core-per-process — `app()` takes exactly one
  schema/data-dir). One page, the landing view. A multi-core *list* would need a core registry
  that does not exist; out of scope here, revisit only if open question 1 reopens toward
  multi-core.
- **Schema view** — the core's persisted TOML schema rendered read-only: fields, types, `stored`/
  `fast`/`multi_valued` flags, dynamic-field patterns, copy-fields. Sourced from the same on-disk
  schema §3 already persists and diffs against at startup — no new storage.
- **Index stats** — doc count, segment count, on-disk size, uptime. "Resident memory" is reported as
  best-effort OS-level info, with the same honesty §6 already applies to the absent heap knob: Wayfinder
  is mmap-based, so there is no JVM-heap-shaped number to show, and the page says so rather than
  faking one.
- **Query tester** — a form for `q`/`fq`/`fl`/`rows`/`start`/`facet.field`, submitted to the core's own
  `/select`, rendering the JSON response. This is a thin UI wrapper over the existing endpoint — no new
  query logic, no second code path to keep in sync with the real one.
- **Ping/health** — this process's core status, reusing `/admin/ping`.

**Out of scope, explicitly, for this phase** (each is a bigger surface than a dashboard and needs its
own scoping pass if ever pursued):

- Core create/delete, schema editing, or any config mutation from the browser. §3 already refuses to
  start on an incompatible schema change; a UI that could trigger one is a different, much larger
  feature.
- Document edit/delete from the UI (the update pipeline stays API-only).
- Authentication/authorization. Solr's own admin UI has none by default either — this phase matches
  that posture and documents it as a deployment responsibility (reverse proxy / firewall / network
  policy), not something Wayfinder arbitrates. Flagged as a risk below, not silently assumed away.
- Multi-instance/cluster views. Out of scope for the same reason SolrCloud is (§1 non-goals) — one
  process, one core.

**Architecture.** New routes under `/ui` (resolved by issue #94; `/admin` was the other
candidate), served by the same axum app, alongside the existing `/solr/*` API routes — not a
second process, not a second deployment artifact. Server-rendered HTML, compiled in via `askama`
(compile-time-checked templates, no runtime template parsing, no JS build step) rather than a
client-side framework — this keeps the "single static binary" goal intact the same way
TOML-not-XML config does. Data comes from the same
in-process index/schema state the query pipeline already reads (no core registry exists — `app()`
serves exactly one core, per open question 1); no new stats-collection subsystem, only what's
already tracked or trivially derivable (e.g. index directory size via `std::fs`).

**Tracer bullet for this phase — done (issue #94).** One page: the core view, reading real doc
count and on-disk size from the one running core, at `GET /ui`. Schema view, stats, and the query
tester are the "flesh it out" that follows, each its own follow-up issue, not part of the slice.

**Testing.** Hermetic unit/integration tests against a real in-process core (no browser automation
required at this scope — assert on rendered HTML/text content and HTTP status, the same style as the
existing route tests). A browser-driven check is a fair addition later if the UI grows enough
interactivity to need one; a static dashboard doesn't.

### v3 — `_version_`, narrowed from "atomic updates + optimistic concurrency"

The Phases table originally listed this item as full atomic updates (`{"set"/"inc"/"add-distinct":
...}` field modifiers) plus write-time optimistic concurrency (`versions=true`, a client-supplied
`_version_` on update, HTTP 409 on a stale write) — Solr's whole story around `_version_`. Checked
against the real client the same way v2's `terms`/`admin/luke`/`admin/mbeans` clause was: **the
evidence does not support building most of that.**

`search_api_solr` 4.4.0 (`SearchApiSolrBackend.php`) writes exclusively through Solarium's
`addDocument(s)` — always whole documents, never Solr's atomic-update JSON. It references
`_version_` exactly once, read-only: `stats.field=_version_&function=max(_version_)`, to compute an
incremental-indexing watermark for its own admin UI. No `versions=true`, no conflict handling. The
coverage contract (`coverage/search_api_coverage_contract.json`) confirms zero hits for
`set`/`inc`/`add-distinct`/`versions`. This is a real feature described in the Phases table only
because it's a Solr concept, the same premise gap the capture is supposed to close (§5's "coverage
instrument" note).

**In scope (v1 of this item):**

- **A real `_version_` field**, `i64`, `fast` (docValues), auto-populated per document — not
  user-schema-defined, not user-visible in `schema.toml`, the same kind of internal pseudo-field
  `_root_` already is in the response envelope (§2 envelope fact 8). Monotonic per core (a simple
  counter is sufficient; Solr's own `_version_` has no semantic meaning beyond "increases on every
  write" — see below).
- **`stats.field=_version_`** (and `function=max(_version_)`, Solr's equivalent phrasing for the same
  aggregate) working against it via the stats component that already exists (`src/stats.rs`, issue
  #5) — `_version_` just needs to be a real, statable, fast numeric field; no new aggregation code.

**Out of scope, explicitly** (each needs its own evidence before it's worth building):

- Atomic update field modifiers (`set`/`inc`/`add`/`add-distinct`/`remove`/`remove-regex`). No
  client evidence of use; would need Tantivy read-modify-write under the hood regardless (segments
  are immutable — Tantivy's `IndexWriter` has no partial-field mutation primitive, so this is never
  "atomic" at the storage layer, only at the request-response boundary), a materially bigger and
  riskier feature than a version counter.
- `versions=true` on `/update` and 409-on-stale-write optimistic concurrency. No client sends a
  `_version_` on write or checks for a conflict response. Revisit only if a client that does
  surfaces.
- Any ordering/semantic guarantee on `_version_` values beyond monotonic increase (Solr's own is an
  opaque `long` tied to its update log; clients that need `max(_version_)` as a watermark only need
  "bigger means newer," which a simple counter already gives).

**Architecture.** Add `_version_` at the same layer `_root_` is added today (`core_index.rs`, where
Solr's pseudo-fields are appended to the response — see the comment at line ~1540) — but as a real
schema field this time, not just an envelope-shape addition, since it has to be `fast` for
`stats.field` to aggregate on it. A per-core atomic counter (`AtomicI64`, bumped on every
`add_documents` call) is enough; no persistence of the counter's last value across restarts is
needed for a "bigger means newer" watermark, though restart behavior should be a documented,
deliberate choice (reset to a value that can't collide with pre-restart versions, e.g. seed from
current Unix-epoch millis) rather than an accident.

**Tracer bullet.** One field, one path: index a document, confirm `_version_` is present and
`fast`, then confirm `stats.field=_version_&function=max(_version_)`-shaped requests (both spellings
if they differ in practice — verify against a captured Solr fixture) return the right max. No
atomic-update work, no optimistic concurrency, in this slice or after it unless new evidence appears.

**Testing.** Hermetic — no live Solr needed beyond what already exists for `stats.field`. Extend
`src/stats.rs`'s existing fixture-backed test style (`solr-ref/responses/stats_*.json`) with a
`_version_`-specific case if the real Solr capture doesn't already have one; capture one if not.

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
- `max_body_size` — hard cap, in bytes, on an incoming request body (issue #64), wired to an
  axum `DefaultBodyLimit` layer. Default `10_000_000`, a round headroom figure over the largest
  known captured fixture; axum's own bare default is 2MB, too small for a realistic bulk
  `/update`. Could not be derived from a verified Solr-side max-request-size default (finding 79,
  `docs/solr-ref-findings.md`) — the closest Solr knobs (`requestParsers`'s
  `formdataUploadLimitInKB`/`multipartUploadLimitInKB`) govern form/multipart uploads, not raw
  JSON bodies.
- No heap tuning, by design. Tantivy is mmap-based; the OS page cache does the work Solr's
  heap sizing does. The absence is a feature and should be documented as one.

**Admin (config only, no per-request equivalent):**
- `reported_solr_version` — the `lucene.solr-spec-version` served by `/admin/info/system` and
  `<core>/admin/system`, default `"9.0.0"` (issue #59, §2 ratified divergence 5, §10 open
  question 2). Unclamped: an operator who overrides it owns the compatibility risk.

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
| v2 finds Search API needs something structurally absent (a component, a response field with no Tantivy-side source) | The v1.5 capture exists to surface this *before* v2 builds against it. The v1 engine is useful regardless. |
| Scope creep toward being a general Solr replacement | The non-goals in §1 and the phase table in §5. Every request gets asked: does the target use case need this? |
| One `IndexWriter` per core becomes an indexing bottleneck | It is a Tantivy constraint, not a choice. Measure at 2 M docs; if it binds, the answer is batching, not architecture. |

---

## 10. Open questions

1. ~~**Multi-core vs single-core-per-process.**~~ **Resolved: single-core-per-process.** Multi-core
   is more Solr-like, but one process per core is simpler and matches the operational-simplicity
   goal, and it is what the codebase already does — `src/lib.rs`'s module doc states it as the
   current architecture, and `app()` takes exactly one schema/data-dir pair with no `CoreRegistry`
   anywhere. Issue #94 confirmed this the hard way: a ticket drafted against a "list all cores"
   premise had to be corrected once the code was read, because there is no registry to list. v2.5's
   admin UI (§5) is scoped around this — one core view, not a core list — and stays that way unless
   a future need forces multi-core, at which point this line reopens rather than silently drifting.
2. ~~**Which Solr version to report** from `/admin/system`.~~ **Resolved by issue #59:**
   `[admin] reported_solr_version` defaults to `"9.0.0"` — the lowest version in the 9.x branch
   the Search API capture's generated `schema.xml` already targets (finding 78), and every
   `search_api_solr` `version_compare()` gate that unlocks a feature Wayfinder does not implement
   sits at or below Solr 8.x, so no 9.x value invites an unsupported feature. See ratified
   divergence 5 below.
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
5. ~~**Analyzer preset coverage.**~~ **Resolved by issues #10 and #51:** every language in
   `tantivy::tokenizer::Language` ships, 18 in total, as `text_<code>` presets alongside
   `string`/`keyword`/`text_general`/`text_en`. Built-in `text_en` removes English stopwords
   before stemming, matching Solr; the other language presets remain stem-only, and custom
   `[[field_types]]` chains remain available for operator-selected stopword removal. Tantivy ships
   no stopword list for Arabic, Greek, Romanian, Tamil or Turkish, so a `stopwords` filter in
   those languages is a load-time error rather than a silent no-op.
