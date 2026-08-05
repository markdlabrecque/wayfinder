> **Historical implementation record.** This completed spec does not define current requirements or future work.

# #351 — serve `/autocomplete`; `/terms` has no upstream caller

Branch: `351-autocomplete-endpoint`. **Group C. Depends on #352 (suggest
component), #359 and #360 (spellcheck component). Land all three first.**

## Decisions already made — build to these

**1. `/autocomplete` is in.** PRD secondary goal 4 ("existing Solr clients work
against it unmodified") governs. Stock `search_api_solr` pointed at Wayfinder
currently 404s on every autocomplete keystroke; that is the failure being fixed.

**2. `/terms` stays.** It is not being retired. `search_api_wayfinder` — our own
Drupal module — GETs it at
`drupal/search_api_wayfinder/src/WayfinderClient.php:80`, and it is the only
client that does. Removing the route would break our own module.

What changes is the *justification*: `/terms` was built citing upstream client
evidence that does not exist. Its warrant is now our own module, and finding 76
must say so.

## What the client actually does

`src/Solarium/Autocomplete/Query.php:33` defines a custom Solarium query type
whose handler is **`autocomplete`**, carrying three components in one request
(`:42-44`): `COMPONENT_SPELLCHECK`, `COMPONENT_SUGGESTER`, `COMPONENT_TERMS`.

All three suggester plugins in `search_api_solr_autocomplete` build that query
type. `getAutocompleteSuggestions()` (`SearchApiSolrBackend.php:3973-3995`) sets
`terms.fl`/`terms.prefix`/`terms.limit` via `setAutocompleteTermQuery()`
(`:4031-4038`) and executes through `$connector->autocomplete(...)`.

The handler ships as a config entity —
`config/install/search_api_solr.solr_request_handler.request_handler_autocomplete_default_7_0_0.yml`:

```
name: /autocomplete
class: solr.SearchHandler
components: [terms, spellcheck, suggest]
defaults:
  terms: false            distrib: false
  spellcheck: false       spellcheck.onlyMorePopular: true
  spellcheck.extendedResults: false   spellcheck.count: 1
  suggest: false          suggest.count: 10
```

The decisive negative: **`getTermsQuery()` has no caller anywhere in the
module.** The only reference is `StandardSolrCloudConnector.php:340` overriding
itself to add `distrib`. Nothing in 4.4.0 requests `/terms`.

## Verify before implementing

All of the above comes from the sweep, not from a read of this tree. With the
full source now vendored (PREP-1, #368), confirm and report real line numbers for:

1. the `autocomplete` handler name on the Solarium query type
2. the three components it carries
3. the config entity's exact defaults — build to the file, not to the table above
4. that `getTermsQuery()` genuinely has no caller (grep the whole vendored tree)

If (4) is wrong — if something does call it — say so. It changes `/terms`'s
warrant back and the finding 76 amendment with it.

## Scope

Serve `GET /<core>/autocomplete` as a select-like route running the three
components with the config entity's defaults.

- **terms** — the same machinery `/terms` already uses (`src/lib.rs:4075`).
  Reuse it; do not fork a second implementation. `terms.fl`, `terms.prefix`,
  `terms.limit` are the params that matter.
- **spellcheck** — the real spellcheck path, after #359 and #360.
- **suggest** — served live from the index per #352's architecture decision.
  Read that spec's reasoning section before building this component.
- Admit every param the three components make routine, or `strict_params` 400s a
  legitimate request. The config entity's defaults list is the starting point.
- Components default to **off** (`terms: false`, `spellcheck: false`,
  `suggest: false`) and are switched on per request. A request with none enabled
  is legitimate and must return the right empty envelope, not an error.

## Fixtures

`/autocomplete` needs its own configset — the handler is not in the base corpus.
Capture against real `solr:9`: each component alone, and all three together in
one request, since the combined envelope is the thing no single-component
fixture pins.

Append the block at the **end** of `solr-ref/capture.sh`, run with
`capture.sh --only <prefix>`, add core-relative GET rows to
`solr-ref/manifest.tsv`, commit the fixtures before anything else.

## The documentation half — not optional, does not wait

Independent of the implementation:

- **Amend finding 76.** Per the batch convention (`docs/specs/README.md`): leave
  the original text, append `**Amended by finding N (YYYY-MM-DD):**` to it, and
  write the correction as a new numbered finding. The correction says: the terms
  *component* is real, the `/terms` *path* is not requested by 4.4.0, the capture
  could never have settled it (the captured site had no `search_api_autocomplete`
  installed), and `/terms`'s warrant is `search_api_wayfinder`.
- **Update PRD §5**, which cites finding 76 to justify `/terms` (#155).
- Run `tests/finding_citations.rs` — amending a finding can break citations.

## Testing

Tests first, red, from fixtures. Cover each component alone, all three together,
none enabled, and `strict_params` acceptance of every param the handler config
makes routine. Add a regression test that `/terms` still works unchanged — this
issue's premise is that it has a real client, and that must stay true.

## Files

**You own:** `src/lib.rs` (route registration, param lists — add only, never
reorder), the autocomplete module, `docs/solr-ref-findings.md` (append-plus-
amend per convention), PRD §5, `solr-ref/capture.sh` (append at end),
`solr-ref/manifest.tsv`.

**Sibling conflict:** #355 also amends a finding in
`docs/solr-ref-findings.md`. Follow the shared convention exactly so the two
merge mechanically.

**Coverage denominator:** adds an endpoint. Report it; **#354 owns the number.**

## Definition of done

- `/autocomplete` served with all three components, against captured fixtures.
- `/terms` unchanged and still passing.
- Finding 76 amended, PRD §5 updated, citations test green.
- The four verification results reported in the PR body.
- Rust gates clean.
