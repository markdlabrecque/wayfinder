# #352 — `/suggest?suggest.buildAll=true` on the default cron path

Branch: `352-suggest-buildall`. **Group C, lands first** — #351 depends on it.

## Decision already made — build to this, do not relitigate it

**Suggestions are served live from the index. This is the permanent
architecture, not a stub.** `suggest.buildAll` is accepted and inert.

The reasoning, recorded so it is not rediscovered: Solr's suggester is a separate
FST dictionary that must be explicitly built and goes stale between builds.
Tantivy's term dictionary is already an FST, so a side structure would cost an
index-time build and a staleness window for no query-time gain. The capabilities
a real suggester adds over a term dictionary — weighted top-N, whole-phrase
completion across spaces, fuzzy completion — are ones `search_api_solr` does not
use: it reaches `twm_suggest` as a `terms.fl` value with `terms.prefix`
(findings 154-156), token-prefix only, and Solr declares `twm_suggest` as
`text_ws`, so even on real Solr that path completes single tokens.

## The premise to verify first — and it is not optional

This issue **overturns a conclusion committed to this repo.** The #291 report
(`docs/reports/2026-08-03-suggestcomponent-autocomplete.md`) states that
`getSuggesterQuery()` is "defined but never called anywhere in the backend" and
concludes "no `/suggest` server route is built". That was true of the three-file
snapshot.

Before writing any code, open `src/Hook/SearchApiSolrHooks.php` in the
**now-vendored full source** (PREP-1) and confirm the cron path exists: a
`GET /<core>/suggest?suggest.buildAll=true` via `fireAndForget`
(`SolrConnectorPluginBase.php:1154`), gated on Drupal-only-writeable + index
updates since last build + >1800s since the last one. Record the real line
numbers.

**If it is not there, stop and report.** The whole issue rests on it.

## Scope

Serve `GET /<core>/suggest`.

- Accept `suggest.buildAll`, plus the shipped handler defaults so
  `strict_params` does not 400 params the handler config makes routine:
  `suggest`, `suggest.dictionary` (default `und`), `suggest.count` (default 10).
  Check `config/install/search_api_solr.solr_request_handler.request_handler_suggest_default_7_0_0.yml`
  in the vendored source for the authoritative list, and consider
  `suggest.build`/`suggest.reload`/`suggest.q` — if the component makes them
  routine, admit them too.
- `suggest.buildAll` does no work. Return the captured envelope.
- Mark the inertness with a **`ponytail:` comment naming the ceiling** —
  "token-prefix completion served live from the index; no weights, no
  phrase completion across whitespace, no fuzzy". That comment is what a future
  reader finds instead of rediscovering the architecture.

**Do not add a self-expiring guard here.** Guards watch the source channel and
fire when an upstream premise stops holding. Nothing upstream will change this —
what would change is Wayfinder growing a materialized suggester, which is our own
code. A guard would be noise pretending to be rigour.

`fireAndForget` loads the `nowaitforresponserequest` plugin: the client closes
without reading the response. So the acceptance bar is **does not error, does not
hang, does not leak a task or a connection per cron run**. A synchronous
immediate return clears all three — which is a further argument for inert over
any async-rebuild design. Test the client-disconnects-early case explicitly; a
broken-pipe write must not produce an error log per cron run on every site.

## The divergence to ratify — this is real work, not a footnote

Solr's suggester returns **empty** until built. Wayfinder, reading live, returns
results immediately. So a fixture captured against real `solr:9` *before* its
build runs shows empty where Wayfinder shows suggestions.

Wayfinder is behaving better and still diverging. Per the compatibility contract
that must be **ratified, not hidden**:

- add the entry to `EXPECTED_DIVERGENCES` in `tests/differential.rs`
- document it in PRD §5 as deliberate, with this reasoning
- do **not** widen a normaliser or relax an assertion to make it disappear

## Fixtures

`/suggest` is not in the base corpus and needs its own configset with a
configured suggester. Capture `/suggest?suggest.buildAll=true` against real
`solr:9`. Capture the pre-build query response too — that is the evidence for the
ratified divergence above.

Append the block at the **end** of `solr-ref/capture.sh`, run with
`capture.sh --only <prefix>`, add core-relative GET rows to
`solr-ref/manifest.tsv`, and commit the fixtures before anything else.

## Testing

Tests first, red. Cover: the route exists and returns the captured envelope;
each admitted param passes `strict_params`; an unknown `suggest.*` param still
400s; the client-disconnects-early case does not error or leak.

## Files

**You own:** `src/lib.rs` (route registration near `:645`, `SELECT_PARAMS` or a
suggest-specific param list — add only, never reorder), the new suggest module
and its tests, `solr-ref/capture.sh` (append at end), `solr-ref/manifest.tsv`,
`tests/differential.rs` (`EXPECTED_DIVERGENCES`), PRD §5.

**Coverage denominator:** this adds an endpoint. Say so in the PR; **#354 owns
the number** and recomputes once at the end.

## Definition of done

- The cron-path premise verified against the vendored source, with real line
  numbers reported.
- `/suggest` served, `buildAll` inert with a `ponytail:` naming the ceiling.
- The pre-build divergence ratified in `EXPECTED_DIVERGENCES` and PRD §5.
- Disconnect-early behaviour tested.
- Rust gates clean.
