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

---

## Findings from the issue #11 error-shape capture

Four new fixtures, captured against the same `solr:9` container and corpus. Three of them are not
core-relative GETs (other core, POST body, non-GET method), so they are indexed in
`solr-ref/manifest-errors.tsv` (`name`, `status`, `method`, `url-after-/solr/`, `body`) rather than
`manifest.tsv`, whose "core-relative GET" contract the differential harness (#1) depends on.

12. **Missing `q` is not an error — and does not mean `*:*`.** `select?wt=json` returns HTTP 200,
    `status: 0`, `numFound: 0`, `docs: []` (`err_missing_q.json`). This settles tracer-bullet review
    follow-up 2: Wayfinder's `q`-defaults-to-`*:*` was a real divergence and is now fixed to match.

13. **The error envelope has three shapes, and the difference is visible to clients:**
    - `/select` errors: `responseHeader` *with* the `params` echo, plus `error`
      (`err_bad_syntax.json`).
    - `/update` errors: `responseHeader` with `status` + `QTime` but **no `params`** — `/update`
      never echoes params (`err_update_bad_json.json`, confirming tracer-bullet follow-up 3).
    - Unsupported HTTP method: **no `responseHeader` at all**, just the bare `error` block
      (`err_update_put.json`).

14. **Solr's request handlers are method-agnostic.** `DELETE /select?q=*:*` is served as a normal
    query, 200 and all five docs (`err_select_delete.json`) — a 405 would be a divergence, so the
    routes are registered with `any` rather than `get`/`post`. `/update` is the exception: `PUT`
    returns 400 with the bare envelope above. `GET /update` was not captured.

15. **An unknown core 404s with an HTML page, not JSON.** `err_missing_core.json` is Solr's
    "Searching for Solr? You must type the correct path." easter egg.
    **Deliberate divergence:** Wayfinder matches the 404 status but returns its normal JSON error
    envelope, on the grounds that clients parse JSON and no client depends on the HTML. Wants PRD
    ratification — it is the one place this branch knowingly does not match captured behaviour.
## Differential harness (issue #1)

`tests/differential.rs` + `tests/common/diff.rs` run the query set in `solr-ref/manifest.tsv`
against Wayfinder and diff the response against a known-good side, per the normaliser rules
above (PRD §8). Two modes, one differ:

- **Hermetic (default):** `cargo test --test differential`. Every manifest entry runs against
  an in-process Wayfinder and is diffed against the committed fixture in
  `solr-ref/responses/<name>.json`. No network, no Docker — this is the command CI runs.
- **Live:** `WAYFINDER_DIFF_SOLR=1 cargo test --test differential`. Same query set, same differ,
  but the expected side comes from a live Solr over HTTP (`WAYFINDER_DIFF_SOLR_URL`, default
  `http://localhost:8983/solr/content`). Run `solr-ref/capture.sh` first — it leaves the
  container up with the schema and corpus already loaded; this test does not orchestrate
  Docker itself.

### Adding a query

The query set is `solr-ref/manifest.tsv`, generated by `solr-ref/capture.sh` — it is the single
source of truth. To add a query: add a `cap` line to `capture.sh`, re-run it (needs Docker), and
commit both the new fixture under `solr-ref/responses/` and the updated `manifest.tsv`. Do not
hand-edit the manifest or fixtures.

### Expected-divergence list

`tests/differential.rs::EXPECTED_DIVERGENCES` names manifest entries with a *known, currently
real* Wayfinder-vs-Solr divergence caused by an unbuilt feature (not a harness bug) — e.g.
`sort` (issue #2) and advanced faceting: `facet.mincount`/`limit`/`missing`/`query`,
`json.nl=map`, full term-dictionary enumeration for zero/all-filtered facets (issue #3). Each
entry carries a mandatory reason naming the owning issue.

This list is a to-do, not a permanent skip. The whole-query-set test still runs every one of
those entries and computes their real diff; it just doesn't count a listed entry's diff as a
failure. But if a listed entry ever stops diverging — i.e. its diff comes back empty — that
means the underlying feature landed, and the test **fails** telling you to delete the entry.
There is no way for an entry to sit in this list after the fix has shipped without the suite
turning red first.

`ping` is in this list too, not given a normaliser carve-out: its fixture's
`responseHeader.params` includes Solr ping-handler internals such as a per-run `rid` counter
that no implementation can reproduce byte-for-byte (the same reason `tracer_bullet.rs::ping_reports_ok`
only asserts ping's essential shape rather than diffing the full envelope). Normalising `rid`
away generically would risk quietly swallowing a real `params` diff on every other manifest
entry, so it is handled as a named, reasoned exclusion instead.

When you fix the owning feature: remove the corresponding entry (or entries) from
`EXPECTED_DIVERGENCES` in `tests/differential.rs`. If you forget, the test fails with a message
telling you exactly which entry to remove.
