# #352 — `/suggest?suggest.buildAll=true` on the default cron path

**Date:** 2026-08-04. **Branch:**
`markdlabrecque/issue-352-suggest-suggest.buildall-true-default`.
**Spec:** `docs/specs/352-suggest-buildall.md`.

`search_api_solr`'s `SearchApiSolrHooks::cron` fires
`GET /<core>/suggest?suggest.buildAll=true` via `fireAndForget`
(`nowaitforresponserequest`) on every cron run when the server is
Drupal-only-writeable, the index saw updates since the last build, and the last
build was more than 1800s ago. Issue #352 corrects the #291 descope premise
("no `/suggest` route is built") — that premise held for a three-file snapshot
but is false of the full 4.4.0 source.

## Premise verified against the vendored source

`coverage/search_api_solr_4.4.0_source/` (PREP-1, #368):

- **The cron gate and fire** — `src/Hook/SearchApiSolrHooks.php:143-164`. The
  gate (`:145-147`): `$is_drupal_only_writeable` AND `last_build <
  last_update_on_server` AND `last_build < request_time - 1800`. The only
  opt-out is a `request_handler_suggest_*` entity in `getDisabledRequestHandlers()`
  (`:152-153`). The fire (`:159-161`): `$connector->getSuggesterQuery()` →
  `addParam('suggest.buildAll', TRUE)` → `fireAndForget($query)`. `optimize` is
  fired on the same cron pass (`:136-140`).
- **`getSuggesterQuery`** → Solarium's `createSuggester()` (handler `/suggest`):
  `src/Plugin/SolrConnector/StandardSolrCloudConnector.php:355`,
  `src/SolrConnector/SolrConnectorPluginBase.php:935-937`.
- **`fireAndForget`** loads `nowaitforresponserequest`, `execute`s, removes the
  plugin — the client closes without reading the response:
  `src/SolrConnector/SolrConnectorPluginBase.php:1154-1159`.

## What changed

- **`src/lib.rs`** — register `/wayfinder/{core}/suggest` in `search_api_routes!`;
  add `SUGGEST_PARAMS` (`suggest`, `suggest.buildAll`, `suggest.build`,
  `suggest.reload`, `suggest.dictionary`, `suggest.count`, `wt`, `omitHeader`);
  add the `suggest` handler. `suggest.buildAll`/`build`/`reload` echo Solr's
  matching `command` field and do no work; a bare `/suggest` returns just the
  header; `omitHeader=true` drops it. The handler is synchronous and touches no
  index state, so `fireAndForget`'s "does not error, hang, or leak a task or
  connection per cron run" bar is met by construction.
- **`tests/suggest.rs`** (new) — 9 focused tests: the captured envelope
  byte-for-byte (modulo `QTime`), the `command` variants, `buildAll`-over-`build`
  precedence, bare/header-only, `omitHeader`, `strict_params` accepts the
  handler's routine params, `strict_params` 400s an unknown `suggest.*` param
  (mutation-tested), and inertness across repeated cron runs.
- **`solr-ref/responses/suggest_build_all.json`** + **`manifest-errors.tsv`**
  row — captured against real `solr:9` with the canonical Drupal configset
  (`search-api/configset/solrconfig_extra.xml` already carries the `/suggest`
  requestHandler and its `suggest` SuggestComponent). The `--only '^suggest_'`
  capture block is appended at the end of `solr-ref/capture.sh`.
- **`tests/differential.rs`** — route the `suggest/…` manifest-errors segment to
  the tracer-bullet `content` app (the handler is schema-agnostic/inert, so no
  dedicated app or corpus is needed).
- **`docs/PRD.md` §5** — the `suggest` path is no longer "remains v3"; it is
  served (#352), inertly.

## The wire detail that almost tripped this up

Solr's SuggestComponent **short-circuits a build command** and emits
`{"responseHeader":{status,QTime},"command":"buildAll"}` — no `suggest` block
(that appears only for a `suggest.q` lookup) and, unlike `/select`, **no `params`
under `responseHeader`**: the component does not echo them. The Wayfinder handler
omits `params.echo()` for exactly this reason; `suggest_build_all_returns_captured_envelope`
asserts the absence of `params`, which is what stops a future edit from adding it
and silently diverging from the fixture.

## No ratified divergence — the spec's premise did not hold

The #352 spec's divergence section asked to ratify "Solr returns empty until
built; Wayfinder, reading live, returns results immediately." That premise is
wrong on both halves, verified by capture against real `solr:9`:

1. **Solr does not return empty pre-build — it 500s.** With the shipped
   `AnalyzingInfixLookupFactory` suggester (`buildOnStartup=false`,
   `buildOnCommit=false`), a `GET /suggest?suggest.q=…` *before* a build returns
   HTTP 500 `"suggester was not built"` (`IllegalStateException` in
   `AnalyzingInfixSuggester.lookup`), not an empty 200. The cron `buildAll` is
   precisely what prevents that 500 on a real install.
2. **Wayfinder does not serve `suggest.q` via `/suggest`.** Per the architecture
   decision the spec itself records ("suggestions are served live from the
   index"), the live token-prefix read path is `/autocomplete` → `/terms` over
   `twm_suggest` with `terms.prefix` (findings 154-156), not `/suggest`.
   `suggest.q` is therefore deliberately **not** admitted to `SUGGEST_PARAMS`
   (a `ponytail:` names that ceiling).

So on the one path `search_api_solr` actually exercises — the `buildAll` cron
request — Wayfinder's response matches Solr's byte-for-byte. There is no wire
divergence to ratify, and no entry was added to `EXPECTED_DIVERGENCES`. This is
documented in PRD §5 rather than hidden.

## Pre-existing `capture.sh` merge damage repaired

`capture.sh` on `main` does not parse (`bash -n` fails) — concurrent #340/#341/
#343 branches that all append to the file lost two pieces in merge resolution,
which blocked this issue's `--only '^suggest_'` capture (its block is appended
after the damage). Both are restored verbatim from their introducing commits:

- the `cappls` function's three-line tail (`>> "$MANIFEST_ERRORS"`, `rm -f`,
  `}`), dropped by the #341 merge — `git show a9e0704:solr-ref/capture.sh:4077-4079`;
- the `jf343` release block's `fi`, dropped by the #343 merge — same shape as
  the `dr341` release block immediately above it.

These are exactly the `solr-ref/capture.sh` conflicts CLAUDE.md flags as routine
for concurrent branches. No captures were re-run as a side effect; only the two
missing tokens and the new appended block are touched.

## Gates

`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and the full
`cargo test` suite are all green. `tests/suggest.rs` was confirmed red first
(route absent → 404), then green after the handler landed. The
`strict_params_accepts_handler_routine_params` guard was mutation-tested
(removing `suggest.count` from `SUGGEST_PARAMS` turns it red).

The differential harness's `manifest_errors_every_row_runs_against_the_matching_hermetic_app`
runs `suggest_build_all` against the `content` app through the new `suggest/…`
routing case and diffs it against the committed fixture — zero diffs.

## Coverage

This adds an endpoint to `ROUTES` (the coverage numerator). The coverage
denominator is recomputed once at the end by #354; this PR does not change the
contract.
