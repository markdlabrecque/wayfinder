# #351 — autocomplete: the client's real path is `/autocomplete`, not `/terms`

Found by the 2026-08-04 full-source sweep of `search_api_solr` 4.4.0. This report
corrects a premise the PRD and the findings doc had been relying on, records the
decision that governs #351, and scopes the one server-side prerequisite for full
autocomplete-plugin coverage (the suggester read path).

## The premise correction

The PRD (§5) and finding 76 cited the capture to justify the `/terms` endpoint
(#155): "the capture confirmed the module does call `terms`." That conflates two
different things:

- the **terms *component*** — real, and used by autocomplete; and
- the **`/terms` *endpoint/path*** — never requested by stock `search_api_solr`.

Re-reading the source end to end (`SearchApiSolrBackend.php:3973-4045`, the
`search_api_solr_autocomplete` submodule's three plugins, and
`SolrConnectorPluginBase`) pins the actual mechanism:

- Stock autocomplete **always hits the `/autocomplete` handler** via
  `$connector->autocomplete($solarium_query, ...)`. The Solarium query type is
  the custom `autocomplete` handler (`src/Solarium/Autocomplete/Query.php`),
  which carries three components: terms, spellcheck, suggest.
- The **Terms** path (`Terms.php` extends `Server`, delegates to
  `SearchApiSolrBackend::getAutocompleteSuggestions()`) sets `terms.fl` /
  `terms.prefix` / `terms.limit` on the handler's **terms component**
  (`setAutocompleteTermQuery`, `:4033-4044`) — it does **not** issue a `/terms`
  request.
- The **Suggester** plugin sets `suggest.q` / `suggest.dictionary` /
  `suggest.cfq` / `suggest.count` on the handler's suggest component.
- The **Spellcheck** plugin sets `spellcheck.q` / `spellcheck.dictionary` /
  `spellcheck.count` on the handler's spellcheck component.
- **Decisive negative:** `getTermsQuery()` (the only thing that would emit a
  standalone `/terms` request) has **zero callers** in 4.4.0 — its only
  reference is `StandardSolrCloudConnector.php:340` overriding itself to add
  `distrib`. `getSuggesterQuery()` (a standalone `/suggest` request) likewise
  has zero callers — already recorded as finding 154.

So `/terms` (#155/#308) and `/suggest` (#352) are **not evidenced by stock
`search_api_solr`**. They are reached only by our `search_api_wayfinder`
connector module: it reimplements the Terms-plugin path as a direct
`/terms?terms.prefix=` GET (`buildAutocompleteTerms` → `WayfinderClient::terms`),
and it accepts the `/suggest?suggest.buildAll=true` cron envelope inertly. The
capture could not have settled this either way — `search_api_autocomplete` was
not installed on the captured site — so this was always a source-only question,
and the source answer is the one above.

Finding 154's *mechanism* ("autocomplete reads the dictionary through the terms
component") is correct; its use as justification for the `/terms` *endpoint* is
the part that was wrong.

## The decision (governs #351)

**Our-module reading, extended to all three plugins.** `search_api_solr`
integration is via the `search_api_wayfinder` connector module (#57); goal 4's
"existing clients work unmodified" is operated as "via the thin connector," not
"stock client, zero changes" (which is why the connector module exists). Under
that reading `/terms` works today (#291), and `/autocomplete` is **not** built —
stock `search_api_solr` still 404s on `/<core>/autocomplete`, accepted.

Going beyond the issue's minimal our-module reading: the connector module will
**reimplement all three autocomplete plugins**, each against an existing or
planned server endpoint, mirroring the Terms→`/terms` precedent:

| Plugin | Server endpoint | Server work needed? |
|---|---|---|
| Terms | `/terms` (done, #155/#308) | none — shipped |
| Spellcheck | `/select` (spellcheck shipped, #222/#228/#342) | none — `fn spellcheck` returns 1 correction/token, exactly the `/autocomplete` handler's `spellcheck.count=1` default the plugin uses |
| Suggester | `/suggest` lookup path | **yes — the prerequisite below** |

The Suggester plugin is the sole blocker: no endpoint answers `suggest.q` today
(`SUGGEST_PARAMS` omits it by design; `/suggest?suggest.q=` 400s).

## Scope: the suggester read path (the prerequisite)

**Objective.** Serve real `suggest.q` lookups on `/suggest` so the Suggester
plugin can be reimplemented in `search_api_wayfinder`. This is the single
server-side prerequisite for full autocomplete-plugin coverage.

**What the Suggester plugin sends** (`Suggester.php::setAutocompleteSuggesterQuery`):

- `suggest=true` (gate)
- `suggest.q=$user_input` (the lookup string)
- `suggest.dictionary=$langcode|und` (selects the per-language dictionary)
- `suggest.count=$query->getOption('limit') ?? 10`
- `suggest.cfq=<context filter query>` — `Utility::buildSuggesterContextFilterQuery()`
  from site-hash / index / langcode tags, encoded as Solr names
  (e.g. `+ss_search_api_solr_site_hash …`), matched against `sm_context_tags`
- `suggest.highlight=false` (the module highlights itself)

**What the shipped Solr config does** (`solrconfig_extra.xml:32-53`):

- `solr.SuggestComponent` with two dictionaries: `en` (analyzer `text_en`) and
  `und` (analyzer `text_und`).
- `lookupImpl=AnalyzingInfixLookupFactory` — analyzed **infix (substring)**
  matching, not prefix.
- `dictionaryImpl=DocumentDictionaryFactory` — dictionary built from a field,
  per-document.
- `field=twm_suggest` — the suggestion-phrase source (the `solr_text_suggester`
  sink).
- `suggestAnalyzerFieldType=text_<lang>` — the matching analyzer.
- `contextField=sm_context_tags` — the context-filter field (`strings`, stored,
  not docValues); `cfq` matches stored values here.
- `buildOnCommit=false`, `buildOnStartup=false` — built on demand (the cron
  build already served inertly, #352).

**Wire response shape** (what the module parses via Solarium's Suggester Result,
`Suggester.php::getAutocompleteSuggesterSuggestions`):

```json
{"suggest":{"<dictionary>":{"<suggest.q>":{"numFound":N,"suggestions":[
  {"term":"<phrase>","weight":<int>,"payload":"<str>"}, …]}}}}
```

Each `phrase['term']` becomes a suggested search key.

**The hard parts (design decisions to make in the implementing issue):**

1. **Infix (substring) matching.** `AnalyzingInfixLookup` matches the analyzed
   query anywhere in the phrase, not just as a prefix. Tantivy's term dictionary
   does prefix/range scans natively but not arbitrary infix. Options: (a) index
   an n-gram-analyzed sidecar of `twm_suggest` and prefix-scan it; (b) scan the
   live `twm_suggest` term dictionary and filter by infix in memory (feasible at
   autocomplete scale, bounded by `suggest.count`). Capture-informed decision.
2. **Context filtering (`cfq`).** `DocumentDictionaryFactory` pairs each phrase
   with its source document's `sm_context_tags` values and weight; `cfq`
   filters by those tags. `twm_suggest` is a flattened multi-value sink, so the
   phrase→contexts association is lost when flattened — replicating `cfq` means
   retaining, per source document, the `(phrase, contexts, weight)` triple and
   intersecting `cfq` tags. This is the genuinely novel server work; it likely
   needs a stored-sidecar or a scan over stored `sm_context_tags`.
3. **Per-language dictionaries.** `en`/`und` map to analyzer + sink per
   language; `suggest.dictionary` selects. Wayfinder's per-language field naming
   gives the routing; the dictionary's analyzer choice must match the shipped
   `text_<lang>`.

**Fixtures needed.** `search_api_autocomplete` was not installed in the v1.5
capture, so there is **zero** wire evidence for `suggest.q` / `suggest.cfq`
today. New captures against a real `solr:9` with the shipped suggester configured
(and `search_api_autocomplete` + the Suggester plugin enabled) are required
before implementation — extend `solr-ref/capture.sh` (append at the end), commit
fixtures + manifest rows, derive the differential test. Cover: infix hit,
prefix-only miss, `cfq` scoping (site-hash / index / langcode), multi-dictionary,
empty result.

**Descopes / open questions.**

- `payload` / weight ranking: `DocumentDictionaryFactory` supports a weight
  field; whether `search_api_solr` populates it (and whether ranking matters for
  autocomplete correctness) is a capture question.
- `suggest.buildOnCommit` stays `false`; the **live read path** is the
  deliverable. The build envelope is already served (#352).
- Endpoint home: expose the lookup on `/suggest` (admit `suggest.q` + `cfq`),
  so the module reimplements the plugin against the same endpoint shape —
  consistent with Terms→`/terms`.

**Sequencing.** This lands before the module's Suggester-plugin support.
Spellcheck-plugin support needs **no** server work (served on `/select` today).
Terms-plugin support already works (#291).

## Follow-up issues

This issue (#351) closes with the doc corrections only (finding 76 amended,
finding 192 added, PRD §5 corrected, the stale `src/lib.rs:634` comment fixed).
Two follow-up issues carry the building:

1. **Server — suggester read path** (the scope above). Blocks #2.
2. **Module — Suggester + Spellcheck autocomplete plugins** in
   `search_api_wayfinder`. Blocked by #1; Spellcheck half is unblocked today.
