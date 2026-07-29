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

Stats, MLT, edismax, `/update` responses, `commitWithin`/`softCommit` behaviour.
Range facets were captured by issue #3 (findings 16-18) — what is still missing there is
`facet.range.other` / `.include` / `.hardend`, month/year date-math gaps, `facet.prefix`,
`facet.method`, `facet.pivot`, interval and heatmap faceting, `f.<field>.facet.*` per-field
overrides, and `json.nl=map` combined with `facet.missing` (a `null` key in an object).

Add to `capture.sh` when those features come into scope — the script is meant to grow with the
feature set.

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

---

## Findings from the issue #3 faceting capture

Twelve new `manifest.tsv` rows on the untouched main core, plus seven new `manifest-errors.tsv`
rows. (That file gained nine rows in this run, but two of them —
`update_unknown_field_schemaless` and `update_unknown_field_strict` — belong to issue #10, whose
fixtures were committed while their manifest rows never were; re-running `capture.sh` backfilled
them.)
`facet.range` needs a numeric or date field, and adding one to the `content` core would rewrite
ground truth for every doc-returning fixture, so the range corpus lives in a **second core**
(`facets`, 4 docs, `views` pint + `created` pdate + `note` string-stored-only) on the same
container. Anything not a main-core GET is a `manifest-errors.tsv` row.

16. **The issue-#3 premise was wrong: Solr never errors on a facet it cannot build.**
    `facet.field` on `body` (text_en, indexed, stored, **no** docValues) returns HTTP 200 with
    `"body":[]` (`facet_non_docvalues_text.json`). So does a stored-only field that is neither
    indexed nor docValues (`facet_stored_only_field.json`, `note` on the `facets` core). Both are
    `status: 0`, present-and-empty, no warning anywhere in the response.

    **Corrected by issue #26.** This finding originally claimed a third case — that a field which
    does not exist at all also returns 200 with `"nosuchfield":[]`. That was wrong, and wrong for
    an instructive reason: the fixture was captured against a container whose `content` schema had
    already been polluted by this same script's schemaless probe, which auto-adds `nosuchfield`
    and cannot remove it. So `nosuchfield` *did* exist when the fixture was taken. On a clean
    container Solr answers `facet.field=nosuchfield` with a **400**, which is what Wayfinder does,
    so unknown facet fields are **not** a divergence at all. `capture.sh` now runs the probe on its
    own throwaway core, and the fixture is re-captured at 400.

    That is precisely the silent-empty-counts behaviour tracer-bullet review follow-up 1 names as
    a bug: the client cannot tell "this field has no values" from "I asked for something
    impossible". **Deliberate divergence:** Tantivy cannot aggregate a non-`fast` column at all,
    and Wayfinder answers all three with a hard 400 in the Solr error envelope
    (`can not facet on undefined field: <name>` / `can not facet on a field w/o fast values
    (docValues): <name>`, mirroring finding 11's `sort` wording). **Wants PRD ratification**, the
    same treatment as finding 15's unknown-core divergence — it is a knowing mismatch with
    captured behaviour, chosen because the captured behaviour is wrong.

    These three fixtures are therefore in `manifest-errors.tsv` rather than `manifest.tsv`, even
    though they *are* core-relative GETs: a `manifest.tsv` row would demand a permanent
    `EXPECTED_DIVERGENCES` entry, and that list is a self-expiring to-do list, not a home for
    accepted divergences.

17. **The empty array is a property of the default `fc` facet method, not of the field.**
    `facet.field=body&facet.method=enum` on that same non-docValues field **does** enumerate the
    term dictionary — `["dog",2,"lazi",2,"quick",2,"afternoon",1,...]`, the stemmed `text_en`
    tokens (`facet_non_docvalues_text_enum.json`). Solr's `enum` method walks the inverted index
    instead of the uninverted field, so the data is reachable; the default just declines to reach
    it. `facet.method` is out of scope for #3 (Wayfinder has one implementation, the fast-field
    aggregation), recorded so nobody later concludes from finding 16 that Solr *cannot* facet a
    non-docValues field.

18. **`facet_ranges` envelope — the part that is not guessable.** From
    `facet_range_numeric.json` / `facet_range_date.json` / `facet_range_json_nl_map.json`:
    - `facet_ranges.<field>` is `{"counts": ..., "gap": ..., "start": ..., "end": ...}` and
      nothing else when `facet.range.other` is unset — no `before`/`after`/`between` keys.
    - **Bucket keys are strings even for a numeric field**: `"counts":["0",1,"10",1,...]`.
    - **`gap`/`start`/`end` are JSON numbers for a numeric field** (`"gap":10, "start":0,
      "end":40`) **and strings for a date field**, with the gap echoed *verbatim as the date-math
      expression* rather than normalised (`"gap":"+1DAY"`).
    - `json.nl=map` turns `counts` into an object (`{"0":1,"10":1,...}`) and leaves
      `gap`/`start`/`end` untouched.
    - **Empty interior buckets are emitted, at 0**: the date capture has
      `"2020-01-01T00:00:00Z",0` and `"2020-01-04T00:00:00Z",0` between populated buckets.

    Only day-granularity date gaps are in scope: `+1MONTH`/`+1YEAR` need a calendar-aware
    DateMathParser (month lengths vary), so Wayfinder refuses them by name rather than silently
    rounding. Follow-up.

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
  Docker itself. **This mode never queries Wayfinder** — it re-fetches from Solr itself and
  diffs that against the committed fixture, i.e. a fixture-staleness check against the live
  container, not a Wayfinder compatibility check (that is the hermetic mode's job). This matters
  for the self-expiring list below: a divergence that only exists because Wayfinder lacks a
  feature trivially "matches" in live mode on every run, since real Solr is internally
  consistent with its own past capture — so those entries' self-expiry is decided by the
  hermetic run only, and live mode just logs them. (`fl=score`, issue #34, used to be this
  list's example of that; it no longer is, since it landed as a ratified permanent divergence —
  PRD ratified-divergence 4 — rather than an unbuilt-feature one. See finding 31.)

### `manifest-errors.tsv` wiring (issue #31)

`manifest-errors.tsv` (added by issue #11 for the non-core-relative-GET error fixtures — other
cores, POST/PUT/DELETE, request bodies) is now run by the same two-mode harness, in
`tests/differential.rs::manifest_errors_every_row_runs_against_the_matching_hermetic_app` /
`live_solr_matches_committed_manifest_errors`. `tests/common/diff.rs::load_manifest_errors`
parses its 6-column format (`name, status, method, url-after-/solr/, body, [base-url]`);
`common::request_full`/`request` (moved from `tests/error_shapes.rs`, issue #31 item 3) issue the
requests so POST/PUT/DELETE and bodies work. Per-row app selection is by the URL's leading core
segment (`content/...`, `facets/...`, `keyorder/...`), rewritten to `content` before the request
is issued, since every Wayfinder test app names its core `content` (`KEYORDER_SCHEMA_TOML`'s
comment documents the same rewrite for `tests/json_key_order.rs`'s own copy of that core). A
segment naming neither — `nosuchcore/...`, `schemaless_probe/...` — is issued unrewritten against
the default content app, since that mismatch (a core Wayfinder genuinely does not have) is exactly
the shape of the `ACCEPTED_DIVERGENCES` rows below.

### `ACCEPTED_DIVERGENCES` vs `EXPECTED_DIVERGENCES`

Two distinct lists, both self-documenting, printed during their runs, never silent:

- **`ACCEPTED_DIVERGENCES`** (`tests/differential.rs`, manifest-errors runner only) — *ratified,
  permanent* divergences from captured Solr behaviour, each citing the PRD/findings section that
  ratifies it: `err_missing_core` (finding 15, HTML vs JSON 404 body — checked as a raw-text
  non-JSON assertion, since `common::fixture` would panic parsing it), `update_unknown_field_schemaless`
  (PRD ratified-divergence 3, no schemaless mode), and the three unfacetable-field rows
  (finding 16, `facet_non_docvalues_text[/_enum]`, `facet_stored_only_field`). These never
  expire — there is nothing to build, the PRD says this is Wayfinder's intended shape — so they
  are checked by a narrower, row-specific assertion instead of the generic differ, and are not
  expected to ever leave the list.
- **`EXPECTED_DIVERGENCES`** (`manifest.tsv` loop) / **`EXPECTED_DIVERGENCES_MANIFEST_ERRORS`**
  (`manifest-errors.tsv` loop) — a *self-expiring to-do list* for an unbuilt feature or an
  as-yet-unfixed bug, not a harness bug and not ratified. Every entry's diff is still computed;
  the moment it comes back empty the suite goes red, naming the entry to delete. See "Expected-
  divergence list" below for the `manifest.tsv` history; `EXPECTED_DIVERGENCES_MANIFEST_ERRORS`
  is now **empty** — issue #35 closed the gap it tracked (`facet_unknown_field` and four more
  rows surfaced by issue #33: Wayfinder's facet-field/facet-query errors omitted the `response`
  block Solr's fixture carries alongside `error`). The fix builds `response` before
  `facet::facet_counts` runs and attaches it to a `facet.query`/`facet.field` error, while a
  `facet.range` error — detected before the base query ever runs — is marked with
  `facet::PreQueryFacetError` so it is deliberately excluded and still renders with no
  `response` key, matching `facet_err_range_single.json` and friends (see finding 43).

### Running the live error mode

`WAYFINDER_DIFF_SOLR=1 cargo test --test differential` runs `live_solr_matches_committed_manifest_errors`
alongside the others. Each row uses its own effective base URL (column 6 of `manifest-errors.tsv`,
defaulting to the canonical `http://localhost:8983/solr`) and its own method/body via
`common::diff::fetch_live_full`/`fetch_live_status` (the latter is a status-only fetch, used for
`ACCEPTED_DIVERGENCES` rows, since `err_missing_core`'s HTML body would fail `fetch_live_full`'s
JSON parse). A row whose base URL fails a quick reachability probe
(`common::diff::live_reachable`) is a printed, named skip — the per-issue containers on
8984/8985/8986 are not guaranteed to be up — except the canonical 8983 base, which must always
answer. Running this mode **writes** to the reference container: `update_unknown_field_schemaless`
re-POSTs `probe_unknown_field` with `commit=true` against the canonical 8983 container, exactly
as `manifest-errors.tsv`'s own row does — idempotent in practice (same doc `id`, so a re-POST
just re-indexes it), but not a read-only probe.

### Adding a query

The query set is `solr-ref/manifest.tsv`, generated by `solr-ref/capture.sh` — it is the single
source of truth. To add a query: add a `cap` line to `capture.sh`, re-run it (needs Docker), and
commit both the new fixture under `solr-ref/responses/` and the updated `manifest.tsv`. Do not
hand-edit the manifest or fixtures.

### Expected-divergence list

`tests/differential.rs::EXPECTED_DIVERGENCES` names manifest entries with a *known, currently
real* Wayfinder-vs-Solr divergence caused by an unbuilt feature (not a harness bug) — currently
just `ping` (reason below). `sort` *ordering* (issue #2) and the seven faceting entries this list
used to carry (`facet.mincount`/`limit`/`missing`/`query`, `json.nl=map`, term-dictionary
enumeration for the zero/all-filtered facets) were deleted when issues #2/#3 landed and they
stopped diverging — the mechanism working exactly as designed. `select_term_scored`/
`select_quick_scored` (issue #31/#34) were here too, for the same reason (no `fl=score`
support); issue #34 landed `fl=score`, and their remaining divergence (BM25 score magnitude)
turned out to be a permanent scoring-formula difference rather than an unbuilt feature, so they
moved to `RANKED_SCORE_VALUE_RATIFIED` (PRD ratified-divergence 4, finding 31) instead of being
deleted outright. Each entry carries a mandatory reason naming the owning issue.

Note what does **not** belong here: an *accepted, permanent* divergence (finding 15's unknown
core, finding 16's unfacetable-field 400). Those get their fixtures in `manifest-errors.tsv` and
a numbered finding, because this list is a to-do list that fails when an entry stops diverging —
an accepted divergence parked here would sit unexpiring forever.

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

---

## Findings from the issue #2 `sort` capture

Sixteen new fixtures, captured against the same `solr:9` container and 5-doc corpus, all
core-relative GETs and so all in `solr-ref/manifest.tsv`. Thirteen came with the feature; the
last three were added over two review rounds to settle the check-order claims in finding 18,
each because the claim had been inferred rather than captured (see finding 20).

16. **Sorting on a `multiValued` docValues field is a 200, not an error** — the issue's "Out"
    scope premise ("error like Solr") was wrong. `select_sort_mv_asc.json` /
    `select_sort_mv_desc.json` show Solr 9 applying Lucene's `SortedSetSortField` selector
    semantics:
    - `asc` orders by each document's **minimum** value (doc1 `[animals, classic]` -> `animals`),
    - `desc` orders by each document's **maximum** (doc3 `[misc, classic]` -> `misc`),
    - a document with **no value sorts last in both directions** (doc5 has no `category` and is
      last under `asc` *and* under `desc`, so "missing last" is not a consequence of direction).

    Wayfinder implements the selector rather than rejecting, in `src/collector.rs`.

17. **A sort clause with a missing or unrecognised direction token is a 400.** `sort=id`
    (`err_sort_no_direction.json`) and `sort=id sideways` (`err_sort_bad_direction.json`) both
    fail with `Can't determine a Sort Order (asc or desc) in sort spec '<spec>', pos=2` — Solr
    does **not** default the direction. `pos` is the parser offset just past the clause's field
    name: `pos=2` past `id`, and `pos=5` past `score`
    (`err_sort_score_bad_direction.json`), which is what confirms the offset's meaning rather
    than it being a coincidence of two same-length field names.

18. **Solr validates a sort spec clause by clause, left to right, stopping at the first bad
    clause; within a clause it checks the direction *before* resolving the field.** These are
    two independent claims and each needs its own fixture — issue #2 got the order wrong twice
    by inferring one from evidence for the other, so the scope of each fixture is spelled out:

    *Across clauses (left to right, first bad clause wins):*
    - `sort=id asc,body desc` is a 400 (`err_sort_bad_clause_among_good.json`) — one bad clause
      rejects the whole spec, rather than sorting on the valid prefix.
    - `sort=body desc,id sideways` is a 400 (`err_sort_field_before_direction.json`) reporting
      the *field* error for the earlier clause, not the direction error for the later one. This
      rules out validating every direction in a global first pass. It says **nothing** about
      within-clause order: clause 1's direction (`desc`) is valid, so clause ordering alone
      explains the result.

    *Within one clause (direction before field):*
    - `sort=body sideways` is a 400 (`err_sort_direction_before_field.json`) reporting the
      *direction* error, even though `body` is also a non-docValues field. One clause, bad in
      both ways at once — the **only** captured spec that separates the two within-clause
      orders, because every other one answers identically under either. `pos=4`.

    *And `score`:*
    - `sort=score sideways` is a 400 (`err_sort_score_bad_direction.json`) reporting the
      direction error. This establishes only that `score` is **not exempt from the direction
      check** — under direction-first, a bad direction errors whether or not `score` is
      special-cased, so this fixture cannot speak to field resolution. That `score` is never
      resolved as a field is established instead by `select_sort_score_{all,asc,desc}`
      returning 200 and ranking by score, which an unresolvable field could not do.

    Only `error.code`/HTTP status is part of the compatibility contract (finding 10), so every
    one of these is a 400 whichever order an implementation picks; the discriminating evidence
    is `error.msg` alone. That is why the order needs fixtures rather than reasoning, and why
    `tests/sort.rs` asserts the error *class* against the fixture and pins the direction
    message verbatim (`pos` included) — see finding 20.

    `check_sort` in `src/lib.rs` implements exactly this: one pass over the clauses, per clause
    checking the direction and then resolving the field, returning on the first bad clause.

19. **`sort=score desc` is exactly the default order.** `select_sort_score_all.json` has the
    same document order as the no-`sort` `select_all.json`, which is why Wayfinder expresses the
    unsorted path as the single implicit clause `score desc` plus the ascending-doc-order
    tie-break, rather than as a separate code path.

20. **A divergence the compatibility contract cannot see — kept as a worked example.** Issue #2
    first shipped `check_sort` as two passes (validate every clause's direction, then resolve
    fields), built to a task-spec premise that Solr "parses the whole spec before resolving any
    field". For `sort=body desc,id sideways` that answered

    ```
    Can't determine a Sort Order (asc or desc) in sort spec 'body desc,id sideways', pos=12
    ```

    where Solr answers the earlier clause's field error (finding 18). Both are HTTP 400 with
    `error.code: 400`, and the differential harness drops `error.msg` (finding 10), so
    **`err_sort_field_before_direction` showed 0 diffs while the behaviour was wrong.** The
    normaliser output says so out loud:

    ```
    err_sort_field_before_direction: normaliser touched ["responseHeader.QTime", "error.msg", "error.metadata"]
    err_sort_field_before_direction: 0 diffs
    ```

    It then happened a **second** time, one level down. The replacement was one pass, per clause
    resolving the field and *then* checking the direction — and that within-clause half was
    itself never captured, only inferred from `err_sort_field_before_direction`, which cannot
    establish it. Review swapped the two checks and the whole suite passed. Capturing
    `sort=body sideways` settled it: Solr checks the **direction first**, so that inference was
    backwards too, and the code was corrected again.

    Three lessons, all earned the hard way in one issue:

    - **The premise, not the code, was the bug** — both times. Each wrong design was consistent
      with every fixture that existed when it was written. When a design rests on an inferred
      mechanism rather than a captured one, capture the discriminating case *before* writing the
      prose that claims it.
    - **Evidence for one claim is not evidence for a neighbouring claim.** "Clause by clause,
      left to right" and "direction before field within a clause" look like one fact and are
      two. `err_sort_field_before_direction` proves only the first; the second needed a spec bad
      in both ways at once, in a single clause.
    - **0 diffs is not proof for anything the normaliser drops.** Where the class of an error
      matters, assert the class against the fixture — `tests/sort.rs::sort_error_class` compares
      "direction error" vs "field error" between response and fixture, pinning the ordering
      without freezing either side's wording. Where Wayfinder claims *verbatim* equality, as it
      does for the direction message, freeze the whole string:
      `direction_error_messages_match_solr_verbatim_including_pos` covers four fixtures with
      four different field-name lengths, because `pos` is arithmetic and a deliberately wrong
      `pos` otherwise passed the entire suite.

## Findings from the issue #25 JSON key-order capture

Solr serialises `SimpleOrderedMap`/`NamedList`, so **every object in its envelope has a
meaningful, insertion-defined key order**, and almost none of it is alphabetical. The facts below
are read out of the committed fixtures with an order-preserving parse; `serde_json::from_str::<Value>()`
cannot see any of them unless the `preserve_order` feature is on, which is why the guard suite
(`tests/json_key_order.rs`, helper `tests/common/key_order.rs`) reads key order out of the document
*bytes* via `MapAccess` instead of out of a parsed `Value`.

21. **The envelope's per-object key orders.** Every one of these is captured, and every one except
    `facet_field`-under-`index`-sort differs from alphabetical:

    | Object | Order | Alphabetical would be | Fixture |
    |---|---|---|---|
    | top level, plain select | `responseHeader, response` | same | `select_all.json` |
    | top level, with facets | `responseHeader, response, facet_counts` | `facet_counts, responseHeader, response` | `facet_json_nl_map.json` |
    | top level, error | `responseHeader, error` | `error, responseHeader` | `err_bad_syntax.json` |
    | top level, bare error | `error` | same | `err_update_put.json` |
    | top level, ping | `responseHeader, status` | same | `ping.json` |
    | `responseHeader` | `status, QTime, params` | `QTime, params, status` | `select_all.json` |
    | `response` | `numFound, start, numFoundExact, docs` | `docs, numFound, numFoundExact, start` | `select_all.json` |
    | `error` | `metadata, msg, code` | `code, metadata, msg` | `err_bad_syntax.json`, and every other `err_sort_*` |
    | `facet_counts` | `facet_queries, facet_fields, facet_ranges, facet_intervals, facet_heatmaps` | `facet_fields, facet_heatmaps, facet_intervals, facet_queries, facet_ranges` | `facet_json_nl_map.json` |
    | `facet_ranges.<field>` | `counts, gap, start, end` | `counts, end, gap, start` | `keyorder_range_wide_map.json`, `facet_range_json_nl_map.json` |

    Note `ping.json`'s `responseHeader` is `zkConnected, status, QTime, params` — the standalone
    `zkConnected` leads. Wayfinder deliberately omits `zkConnected` (it is not SolrCloud), so this
    is a membership divergence already covered elsewhere, not an ordering one.

22. **Under `json.nl=map`, the object's key order *is* the facet order** — the map form carries the
    same information as the flat alternating array (finding 1), including the ordering, which the
    array expresses positionally and the map expresses as key order. Solr does not re-sort it. For
    `facet.field` with the default `facet.sort=count`, that is count-descending with an index-order
    tie-break: `keyorder_facet_field_map.json` gives `apple, zebra, mango, banana` for the counts
    5, 5, 2, 1 — `apple` takes the 5-5 tie on term order, and the whole sequence is neither
    alphabetical (`apple, banana, mango, zebra`) nor the reverse. Under `facet.sort=index` it is
    term order, `apple, banana, mango, zebra` (`keyorder_facet_field_map_index.json`) — which
    *happens* to be alphabetical, so that fixture alone can never detect the alphabetising bug and
    the guard suite marks it as such.

23. **`facet_ranges.<field>.counts` under `json.nl=map` is ascending *numeric* bucket order**, and
    the bucket keys are strings, so this is exactly where a sorted map goes wrong.
    `keyorder_range_wide_map.json` (0-200 by 10 over `views`) gives
    `0, 10, 20, ... 90, 100, 110, ... 190`; alphabetised that becomes
    `0, 10, 100, 110, ... 190, 20, 30, ...`. This is the single most decisive captured case for
    key order, because the divergence is 18 keys wide and visible on the wire.

24. **Doc field order is index-time input order — not `fl` order and not schema declaration
    order.** `select_all.json`'s docs come back `id, body, category, _version_, _root_`, which is
    the order the fields appeared in the indexing request, with Solr's internal fields appended.
    It is *not* the managed-schema declaration order (where `_version_` and `_root_` are declared
    up front, before the dynamically-added `body`/`category`), so the fixtures rule schema order
    out directly. That `fl` order does not drive it was established by a live probe during the
    issue-#25 investigation; the only multi-field `fl` capture at the time, `select_term`
    (`fl=id,body`), listed its fields in input order anyway, so it could not discriminate.
    **Update (issue #31):** the `fl` half is no longer inferred-only — `select_fl_reversed`
    (`fl=body,id`, deliberately reversed) is a committed fixture whose docs come back `id, body`
    (input order), not `body, id` (`fl` order), pinning this half the same way `select_all` pins
    the other. See `tests/json_key_order.rs::select_fl_reversed_doc_key_order_matches_solr` and
    its vacuity guard.

25. **Wayfinder's doc field order is *schema* order (`render_doc` in `src/core_index.rs`) and
    coincides with Solr's for every committed corpus**, because each corpus was indexed with its
    fields in schema order. So there is no divergence today, and the whole-envelope order
    assertions pass over the docs as well — but the two rules are not the same rule, and a corpus
    indexed out of schema order would separate them. Recorded so the next person does not read a
    green suite as evidence that Wayfinder reproduces Solr's rule.

26. **`responseHeader.params` order is not reproducible and is not a contract.** It is Java
    `HashMap` iteration order: neither the request order nor alphabetical.
    `facet_range_json_nl_map.json` echoes `facet.range, q, facet.range.gap, json.nl, rows, facet,
    wt, facet.range.start, facet.range.end` for a request that sent them in a different order
    entirely. This is the ordering half of finding 6, and it is why the differential normaliser is
    order-insensitive on that object and why `params` is the one permanently exempt path in
    `key_order::EXEMPT_PATHS`. Every *other* object in the envelope is ordered on purpose and is
    compared. Issue #25's own change moved Wayfinder's echo of this object from alphabetical to
    request order — neither matches Solr's `HashMap` order, so the exemption stands regardless.

## Findings from the issue #24 `facet.field` numeric/date capture

Claiming findings 27-30 (issue #25 landed concurrently and took 21-26 above).

Eleven new `manifest-errors.tsv` rows against the `facets` core (a `facets`-core GET, so not
`manifest.tsv`), on the same 4-doc corpus `facet_range_*` uses (`r1..r4`, `views` pint,
`created` pdate).

27. **The issue-#24 premise was wrong: Solr does not enumerate a numeric/date term dictionary
    for `facet.field` either.** `q=id:r1&facet.field=views` returns `"views":["5",1]` only —
    `15`/`25`/`35` are absent, not present at 0 (`facet_field_numeric_subset.json`,
    `facet_field_date_subset.json`). `pint`/`pdate` are Points-based fields with no term
    dictionary to walk, so there is nothing for Solr to enumerate from. Wayfinder's existing
    hit-set-only behaviour for numeric/date `facet.field` is therefore a match, not a gap.

28. **The control that makes finding 27 trustworthy.** `facet_field_string_control_subset.json`
    is the *same* container, core, corpus and hit set as finding 27, with `facet.field=id` (a
    string field) instead: it still enumerates the whole dictionary,
    `["r1",1,"r2",0,"r3",0,"r4",0]`. Same everything except the field's type, different
    behaviour — proof finding 27 is field-type-driven and not an artifact of how the fixture was
    captured.

29. **Solr raises `facet.mincount` from 0 to 1 for a Points-based `facet.field`, and says so.**
    Every fixture where a `facet.field` names `views`/`created` (numeric/date) *and* the
    effective `facet.mincount` is 0 carries a `responseHeader.warnings` array with exactly one
    string: `"Raising facet.mincount from 0 to 1, because field <name> is Points-based."`
    (`facet_field_numeric_all.json` et al.). It is absent when `facet.mincount=1` is given
    explicitly (`facet_field_numeric_mincount_one.json`) and absent for the string control
    (`facet_field_string_control_subset.json`) and for every `facet_range_*` fixture — the raise
    is specific to `facet.field`, not `facet.range`. The raise has no observable effect on the
    counts in these fixtures (no zero-count numeric bucket exists for `min_doc_count: 0` to
    introduce), so this is a header-honesty fact rather than a counting one. Wayfinder now emits
    the same `responseHeader.warnings` key, verbatim wording, under the same gate (a `facet.field`
    naming a non-string fast field, effective `facet.mincount == 0`). Per finding 21 above,
    `warnings` leads `responseHeader` (`warnings, status, QTime, params`), not trails it.

30. **Numeric/date facet terms order by value, not by the rendered string.**
    `facet_field_numeric_sort_index_all.json` (`facet.sort=index`, whole corpus) orders `views` as
    `5, 15, 25, 35` — value order, not the lexical `15, 25, 35, 5` a naive string sort on the
    rendered term produces. The `facet.sort=count` tie-break
    (`facet_field_numeric_sort_index.json`/`facet_field_numeric_sort_count.json`, four counts of 1)
    is also value-ascending. This was a real bug in `src/facet.rs::facet_fields`, fixed by
    carrying a typed sort key (`CoreIndex::FacetOrderKey`) out of `term_facet` alongside the
    rendered term, rather than sorting the rendered string. `facet_field_date_sort_index_all.json`
    does not by itself distinguish value order from lexical order (RFC3339 lexical order and
    chronological order coincide for these dates), so it is asserted but not load-bearing for the
    fix the way the numeric fixtures are.

## Findings from the issue #32 sort-debt capture

Claiming findings 34-37 (issue #31 reserved 31-33). Twenty-two new `manifest-errors.tsv` rows
against a new `sortdebt` core (own container `wayfinder-solr-32`, port 8987 — not the
`content` core, so none of these are `manifest.tsv` rows). Corpus `s1..s6`: `category`
string, `views` pint, `weight` pfloat, `created` pdate, `nums` multiValued pint, with
per-field gaps (`s4` has no `views`; `s5` has only `id`/`category`/`views`) and **negative
values plus a pre-epoch date on `s6`** — the negatives are what make finding 36
discriminable at all.

34. **The comma between sort clauses is mandatory, and extra tokens after a direction are a
    400, not ignored.** Within one comma-delimited clause, the field is the first
    whitespace-delimited token and *everything from there to the next comma or end of spec*,
    trimmed, must be exactly `asc` or `desc`; otherwise Solr answers the direction error with
    `pos` just past the field token. `sort=id asc category desc`, `sort=id asc garbage`, and
    `sort=id asc category` all 400 with
    `Can't determine a Sort Order (asc or desc) in sort spec '<spec>', pos=2`
    (`sort_clause_space_separated.json`, `sort_clause_trailing_garbage.json`,
    `sort_clause_trailing_valid_field.json`). This kills Wayfinder's previous
    split-on-whitespace-and-drop-the-rest reading: `sort=id asc garbage` was a silent 200
    here and is a 400 in Solr — exactly the "never a silent fallback" property. Comma
    handling is asymmetric: a trailing comma is fine (`sort=id asc,` → 200,
    `sort_clause_trailing_comma.json`), as is whitespace around the separating comma
    (`sort_clause_space_before_comma.json`, `sort_clause_space_after_comma.json`) and an
    empty `sort=` (`sort_clause_empty.json`, default order) — but a comma *starting* a
    clause glues onto the following token and fails **field resolution**: `sort=,id asc` and
    `sort=id asc,,category desc` 400 with
    `sort param could not be parsed as a query, and is not a field that exists in the
    index: ,id` (resp. `,category`) — a third field-error wording, still classified as a
    field error by `tests/sort.rs::sort_error_class` (`sort_clause_leading_comma.json`,
    `sort_clause_double_comma.json`). So "skip empty clauses" is right only for the
    *trailing* position; Wayfinder previously 200'd the leading-comma case.

35. **The direction error's `pos` is absolute within the whole sort spec, not
    clause-relative — the inference flagged in `src/lib.rs` was right.**
    `sort=id asc,id sideways` → `pos=9` (`err_sort_second_clause_bad_direction.json`),
    exactly what Wayfinder's arithmetic predicted; `sort=id asc,category` → `pos=15`
    (`err_sort_second_clause_no_direction.json`), i.e. past the *second* clause's
    whole field token; and leading whitespace counts — `sort='  id sideways'` → `pos=4`
    (`err_sort_leading_whitespace.json`).

36. **A document missing a numeric, float, or date sort value sorts as the value 0 — not
    first, not last.** Lucene's default `missingValue` for numeric sorts is 0, and the
    fixtures pin it: under `views asc` the missing doc lands *between* `-5` and `10`
    (`s6, s4, s2, s3, s1, s5` — `sort_int_asc.json`), under `weight asc` between `-1.5`
    and `0.5`, and under `created asc` between the pre-epoch `1969-06-01` and `2021-01-01`
    (missing date = epoch). Descending is the exact mirror. Without `s6`'s negatives this
    is indistinguishable from "missing sorts first under asc" — which is what a corpus of
    positive values would have wrongly suggested. **This is a per-type divergence from
    strings**: a missing `SortedSet` (string) value sorts last in *both* directions
    (finding 16), so missing-value placement is a property of the column type, not of the
    sort machinery. Wayfinder's previous missing-last-for-everything comparator was wrong
    for the numeric/float/date arms.

37. **The min/max selector applies to multiValued numerics exactly as to strings, composed
    with finding 36's missing-as-zero.** `nums asc` orders by per-doc minimum
    (`s6(-10), s5(missing→0), s1(10), s3(20), s2(50), s4(70)` — `sort_mv_int_asc.json`);
    `nums desc` by per-doc maximum (`s1(90), s3(80), s4(70), s2(60), s6(5), s5(missing→0)`
    — `sort_mv_int_desc.json`). The desc order is not the reverse of the asc order (the
    corpus was arranged so min-order and max-order disagree), which is what proves a
    selector rather than a direction flip; and `s6` (max `5`) sorting *above* the missing
    `s5` under desc while `s5` sits between `-10` and `10` under asc is the multiValued
    confirmation of missing-as-zero.

## Findings from the issue #33 facet-debt capture (error precedence, float/date rendering, unpinned semantics)

Claiming findings 38-41 (issues #31 and #32 have 31-33 and 34-37 reserved, per issue #33).

Twenty-two new `manifest-errors.tsv` rows against a new self-contained `facets33` core
(`wayfinder-solr-33`, port 8988), on a 5-doc corpus (`r1..r5`) with `views` pint, `price`
pdouble, `rating` pfloat, `stamp` pdate (millisecond values), `tag` string (docValues, absent
from r4/r5), `note` string stored-only. The container is the issue-33 block's own, so no
schemaless probe ever touched it; `facet_err_field_single.json`'s clean
`undefined field: "nosuchfield"` is the proof the schema is unpolluted (the issue-#26 lesson).

38. **Error precedence among broken facet params is `facet.range` > `facet.query` >
    `facet.field`, one error per response.** Singles first
    (`facet_err_{query,field,range}_single.json`): an unparseable `facet.query` is a 400
    SyntaxError, an undefined `facet.field` is a 400 `undefined field`, and a `facet.range` on a
    string field is a 400 `Unable to range facet on field:tag{...}`. Every pair and the
    all-three combo report exactly one error: query+field -> the query error
    (`facet_err_query_field.json`), query+range -> the range error
    (`facet_err_query_range.json`), field+range -> the range error
    (`facet_err_field_range.json`), all three -> the range error (`facet_err_all_three.json`).
    Wayfinder evaluated field -> query -> range (the issue-#24 hoist), which is exactly
    backwards; fixed to range -> query -> field. Also captured, the #30 shape verbatim:
    an invalid `facet.query` plus a stored-only (unfacetable-in-Wayfinder) `facet.field`
    reports the query SyntaxError (`facet_err_query_vs_unfacetable.json`) — in Solr the
    unfacetable half is not an error at all (ratified divergence 2), so the query error
    surfacing over it is consistent with both engines and needs no divergence entry.

39. **A `pdouble`/`pfloat` facet key renders as Java `Double.toString`, so an integral double is
    `"5.0"`, never `"5"`.** `facet_field_double_all.json` keys `price` as
    `"5.0", "0.25", "7.5", "12.0"`; `facet_field_float_all.json` renders `pfloat` identically
    (`"5.0"`, `"7.5"`). Wayfinder rendered the Tantivy aggregation key via `f64::to_string()`,
    which emits `"5"` for integral values — and worse, Tantivy normalises an exactly-integral
    double to a `U64`/`I64` *key variant* (`NumericalValue::normalize`), so the fix must be
    driven by the schema's declared kind (`ValueKind::F64`), not by sniffing the key's variant
    or value. Ordering is by value throughout: `facet.sort=index` gives
    `0.25, 5.0, 7.5, 12.0` (`facet_field_double_sort_index_all.json`), and the default count
    sort tie-breaks value-ascending (`facet_field_double_all.json`: 5.0 at 2, then
    0.25/7.5/12.0 at 1 each) — finding 30 extended to doubles.

40. **A millisecond-precision `pdate` facet keeps distinct millisecond buckets and renders the
    fraction only when non-zero.** `facet_field_date_ms_all.json` keys `stamp` as
    `"2020-01-02T00:00:00.123Z"` and `"2020-01-02T00:00:00.456Z"` (two distinct buckets inside
    the same second) and renders the whole-second value as `"2020-01-05T00:00:00Z"` — no
    trailing `.000`. Order is chronological (`facet_field_date_ms_sort_index_all.json`).
    Wayfinder's fast date column was `DateOptions::default()` = seconds precision, which would
    have collapsed the two same-second values into one bucket of 2 — a real divergence, fixed by
    declaring millisecond precision (Solr's `pdate` precision) on the fast column.

41. **Four previously-unpinned semantics, now pinned.** (a) The `facet.missing` null bucket is
    exempt from both `facet.limit` and `facet.mincount`: at `facet.limit=1` it survives after
    the one kept term (`facet_missing_with_limit.json`), and at `facet.mincount=3` — above its
    own count of 2 — it still appears, alone (`facet_missing_with_mincount_three.json`,
    with `facet_missing_with_mincount_two.json` as the intermediate control). Wayfinder already
    matched. (b) A `facet.range` span not divisible by the gap (start 0, end 22, gap 10,
    `hardend` unset) extends the last bucket to the gap boundary — `[20,30)` counts the 25 —
    and echoes the *gap-aligned* `end: 30`, not the requested 22
    (`facet_range_end_not_gap_aligned.json`). Wayfinder's bucket walk already extended; the
    echoed `end` was the requested value verbatim, fixed. (c) `json.nl=arrarr` renders each
    bucket as a two-element array `[["apple",2],["banana",1]]` and `json.nl=arrmap` as a
    one-entry object `[{"apple":2},{"banana":1}]` (`facet_json_nl_arrarr.json`,
    `facet_json_nl_arrmap.json`); Wayfinder rendered both flat, fixed. (d) Under
    `json.nl=map` + `facet.missing`, the null bucket's object key is the empty string
    (`facet_json_nl_map_missing.json`: `{"apple":2,"banana":1,"":2}`) — exactly what Wayfinder
    already emitted; the `ponytail:` guess in `render_buckets` is now a captured fact.

## Findings from the issue #31 harness follow-up capture

Claiming finding 31 (32/33 not needed — no further new Solr facts surfaced).

Three new `content`-core captures against the canonical container, added to `manifest.tsv`:
`select_term_scored`/`select_quick_scored` (`fl=id,score` on the two existing free-text
relevance queries) and `select_fl_reversed` (`fl=body,id`, see finding 24's update above).

31. **`fl=score` puts a per-doc `score` float on each doc and a `response.maxScore` key
    alongside `numFound`/`start`, not just a bare per-query relevance number.**
    `select_term_scored.json`: `response.maxScore` equals the top doc's own `score`
    (`0.45748958`), and every doc in `response.docs` carries its own `score` — the docs are still
    ordered by descending score exactly as they are without `fl=score`, `fl` only adds which
    fields render, never reorders anything (finding 24's `fl`-order lesson generalises: `fl`
    governs field selection, not document order or field order for the fields it does select
    when unreversed — see the reversed case above). **Update (issue #34):** `fl=score` is now
    implemented — per-doc `score`, correct key position, and `response.maxScore` all render when
    `fl` requests `score`, gated the same way `fl=*` field selection is. Doc ranking order
    matches Solr exactly for both fixtures. What does *not* match is the BM25 float's magnitude:
    Tantivy's own BM25 disagrees with Solr's BM25Similarity by a non-constant ~1.9x-2.3x ratio
    (an internal scoring-formula difference — idf/norm-encoding — not a wiring gap), so this was
    ratified as a permanent divergence rather than left as an unbuilt-feature to-do: PRD "Ratified
    divergences from captured Solr behaviour" entry 4. `tests/differential.rs` reflects the
    split — `RANKED_SCORE_VALUE_RATIFIED` waives only the `response.docs[*].score` *value*
    for `select_term_scored`/`select_quick_scored`; doc ranking order and `response.maxScore`'s
    presence/type are still checked for real, and both entries are gone from
    `EXPECTED_DIVERGENCES` (which is `ping`-only now). The ranked-ID-list differ's
    score-tolerance path (`diff_ranked_ids` in `tests/common/diff.rs`) is exercised against these
    real fixtures, not just synthetic id/score pairs.

42. **`response.maxScore` sits between `start` and `numFoundExact` in Solr's key order.**
    `select_term_scored.json`/`select_quick_scored.json` show the `response` object as
    `numFound, start, maxScore, numFoundExact, docs` when `fl` requests `score` — `maxScore`
    is not appended at the end or led at the front. Wayfinder builds `response` as a `Map`
    (rather than a `json!` object literal, which can't express a conditional key mid-object) to
    match this exactly (`src/lib.rs`, around the `response.insert("maxScore", ...)` call).

---

## Findings from the issue #9 update-pipeline capture

Claiming findings 46-49 (31-41 are #31/#32/#33's; #8 has 56-59 — supersedes the earlier
"claim from 42 up" note on issue #9).

Thirty-four new `manifest-errors.tsv` rows against a new self-contained `update9` core
(`wayfinder-solr-9`, port 8989) — POSTs and deliberately-non-GET requests, so nothing here
touches `manifest.tsv`. Deletes mutate the corpus, so the block is idempotent-by-reset:
every run starts with an uncaptured delete-by-query `*:*` + reseed of the same 5-doc corpus
(`u1..u5`), and the captures are strictly ordered with the corpus state tracked in comments
(the issue-#26 lesson, applied to a probe whose whole *purpose* is mutation).

The corpus-state selects (`update_select_after_delete_list`/`_query`/`_mixed`) carry an
explicit `sort=id asc`, added on a re-capture: their first capture used bare `q=*:*`, whose
equal-score tie order is Lucene segment-merge history — the mixed-commands select came back
newest-doc-first purely as a merge artifact, which is not a wire contract and is not even
reproducible across Tantivy runs (background merges race `commit()`). What these fixtures pin
is *which* docs survive each mutation; the sort makes that deterministic on both sides.

`update_select_overwrite_false` cannot take that fix: its two docs share uniqueKey `u7`, so no
`sort` can discriminate them, and the tie falls to internal doc order on both engines. On the
Wayfinder side that order is per-process random — tantivy 0.26.1's `SegmentRegister`
(`src/indexer/segment_register.rs`) holds segments in a std `HashMap`, so segment ordinals
(and with them cross-segment `DocAddress` order) change run to run — and on the Solr side it
is equally a Lucene merge-internals accident, not a wire contract. The comparison for exactly
this fixture (and only it) is therefore order-insensitive over the duplicate-id pair: it
asserts both docs survive with their distinct bodies, not which internals-accident order they
arrive in. Every other doc-order assertion stays strict. Ruled by the orchestrator during
issue #9; recorded here so the next issue does not re-litigate it.

A warm re-run also exposed a second #26-class trap, fixed in the block: Solr's
`add-copy-field` is not idempotent (unlike `add-field`'s tolerated "already exists"), so a
re-run against a warm core duplicated the `nick`->`alias` directive and flipped
`update_copyfield_single_ok` from 200 to 400. The block now deletes and recreates the core
(schema included) at its top; idempotency was verified by two consecutive warm runs producing
identical statuses.

46. **The `/update` success envelope is the bare `responseHeader` and nothing else — for every
    command shape.** Add-without-commit, add-with-commit, delete-by-id (object and list forms),
    delete-by-query, delete of a nonexistent id, and a mixed-command body all return exactly
    `{"responseHeader":{"status":0,"QTime":N}}` (`update_add_nocommit.json`,
    `update_add_commit.json`, `update_delete_id_obj.json`, `update_delete_id_list.json`,
    `update_delete_query.json`, `update_delete_id_missing.json`, `update_mixed_commands.json`).
    No `params` echo ever (finding 13's error-shape rule holds for successes too), no per-command
    keys, and deleting an id that matches nothing is still a 200. Mixed-command bodies
    (`{"add":{"doc":{...}},"delete":{"id":...},"commit":{}}`) are accepted and executed —
    add, delete and commit all took effect in one request (`update_select_after_mixed.json`) —
    so they are in scope per the issue's "capture decides".

47. **`GET /update` is not a method error — Solr 400s an empty *content stream*, and a GET that
    only commits is a 200.** Bare `GET /update?wt=json` answers 400 `missing content stream`
    with the `/update` (no-params) error envelope (`update_get.json`) — a body problem, not a
    method problem, unlike PUT's bare-envelope `Unsupported method` (finding 13/14). And
    `GET /update?commit=true&wt=json` is a **200** that really commits (`update_get_commit.json`).
    Wayfinder previously rejected every non-POST as an unsupported method by analogy; that was
    wrong for GET on both counts (error-shapes follow-up 2, settled).

48. **Overwrite, delete and commit semantics, pinned.** (a) Default `overwrite=true` replaces:
    re-adding an existing id keeps `numFound` at 1 with the new body
    (`update_select_overwritten.json`). (b) `overwrite=false` really duplicates: two live docs
    with the same uniqueKey (`update_select_overwrite_false.json`). (c) Delete-by-id is a term
    delete on the uniqueKey and removes **all** docs with that key — both `overwrite=false`
    duplicates went in one `{"delete":{"id":"u7"}}` (`update_select_after_delete_id.json`).
    (d) Delete-by-query goes through the same analyzed query semantics as `/select`:
    `{"delete":{"query":"body:lazy"}}` on a `text_en` field deleted both `lazy dog` and
    `lazy afternoon` (`update_select_after_delete_query.json`). (e) A one-element array into a
    single-valued field is unwrapped and accepted (`update_single_valued_array_one.json`,
    stored as the scalar per `update_select_single_valued_array_one.json`); **more** than one
    value is the 400 `multiple values encountered for non multiValued field`
    (`update_single_valued_array.json`), and a copy-field landing a second value in a
    single-valued destination is the same 400 family (`update_copyfield_single_valued.json`,
    with `update_copyfield_single_ok.json`/`update_select_copyfield_dest.json` as the
    one-copied-value control). A dynamic `*_dt` date round-trips: stored RFC3339-Z in, identical
    string out, range-queryable (`update_select_dynamic_date.json`).

49. **Visibility: an uncommitted add is invisible; `commitWithin` and `softCommit` both end
    visible; unknown-core behaviour is endpoint-agnostic for GET/POST.** The `_default`
    configset's hard autocommit has `openSearcher=false`, so an add with no commit param stays
    unsearchable (`update_select_uncommitted.json`: `numFound: 0`). `commitWithin=500` makes the
    doc searchable once the window has passed (`update_select_commitwithin_visible.json`,
    captured after a 3 s settle — an immediate select would race the window, so only the settled
    state is pinned). `softCommit=true` with no `commit` param commits at request end and the doc
    is immediately visible (`update_select_softcommit_visible.json`). Unknown core on POST
    `/update` and GET `/admin/ping` is the same 404 HTML easter egg as finding 15's `/select`
    (`update_unknown_core.json`, `ping_unknown_core.json`) — Wayfinder's ratified
    JSON-instead-of-HTML divergence extends to those endpoints unchanged (error-shapes
    follow-ups 3-4, settled). One wrinkle: `DELETE` on an unknown core's `/admin/ping` is a
    Jetty-level **405 with an empty body** (`ping_unknown_core_delete.json`), not the 404 page;
    Wayfinder stays method-agnostic and serves its JSON 404 there too — same divergence family,
    noted rather than matched.

## Findings from the issue #35 facet-error-response fix

Claiming finding 50 (46-49 are issue #9's, landed first).

50. **A `facet.query`/`facet.field` error carries the base query's `response` block; a
    `facet.range` error does not — Solr detects the two at different points in request
    processing.** `facet_unknown_field.json` (an undefined `facet.field`) and
    `facet_err_query_single.json` (an unparseable `facet.query`) both 400, and both still carry
    a real `response` block alongside `error` — `numFound`/`start`/`numFoundExact`/`docs` for
    the base `q`/`fq`, computed before the facet param is ever validated. `facet_err_range_single.json`
    (a `facet.range` on an unfacetable field) 400s the same way but has **no** `response` key at
    all: Solr validates `facet.range` before running the base query, so there is no query result
    yet to attach. This is the same precedence order finding 38 pins (`facet.range` >
    `facet.query` > `facet.field`) surfacing a second, independent fact — not just which error
    wins when several are broken, but that the range check runs at a genuinely earlier point in
    the pipeline than the query/field checks, before vs. after the base query. Wayfinder's
    `src/lib.rs::select` now builds `response` before calling `facet::facet_counts`, and
    `facet::facet_counts` marks a `facet_ranges` failure with the `facet::PreQueryFacetError`
    wrapper (`Display` forwards to the original error so no message changes) so `select` can
    tell the two cases apart via `downcast_ref` and only attach `response` to the
    query/field case, matching both fixtures exactly. Fixed under issue #35; closes the
    `EXPECTED_DIVERGENCES_MANIFEST_ERRORS` entries it tracked (`facet_unknown_field`,
    `facet_err_query_single`, `facet_err_field_single`, `facet_err_query_field`,
    `facet_err_query_vs_unfacetable`).

## Findings from the issue #5 stats-component capture

51. **`stats.field`'s `min`/`max` render as JSON floats even for an integer field, and zero
    matching docs makes `mean` the literal JSON string `"NaN"`, not `null` and not a bare `NaN`
    token.** Captured against a dedicated `stats` Solr core/corpus (`solr-ref/capture.sh`'s
    issue-#5 block, container `wayfinder-solr-5`, port 8992, six docs `st1..st6` with `views`
    missing on `st6` and `price` missing on `st5`, so `missing`/`min`/`max`/`sum`/etc. are
    provably computed over present-only docs, not defaulted to 0). Four fixtures:
    `stats_views.json` (single `stats.field`, real gap), `stats_multi_fields.json` (repeated
    `stats.field=views&stats.field=price`, two independent gaps), `stats_zero.json` and
    `stats_zero_fq.json` (zero matching docs via `q` and via `fq` respectively). Three facts
    worth naming: (a) `min`/`max` for `views` (schema type `int`/`pint`) come back as `10.0`,
    `50.0` — floats, not integers, matching Solr's stats-component convention of computing all
    metrics in double precision regardless of the field's declared type; (b) `stats.field` in
    the echoed `responseHeader.params` is a bare string when given once
    (`"stats.field":"views"`) and a JSON array when repeated
    (`"stats.field":["views","price"]`) — the same singular/array split other repeatable params
    already show; (c) on zero matching docs, `min`/`max` are JSON `null`, `count`/`missing` are
    `0`, `sum`/`sumOfSquares` are `0.0`, `stddev` is `0.0`, but **`mean` is the string `"NaN"`**
    — Solr computes `mean = sum/count` in Java double arithmetic (`0.0/0`), gets a real
    floating-point NaN, and its JSON writer renders that as a quoted string since bare `NaN` is
    not valid JSON. A naive Rust implementation using `f64::NAN` would serialize via `serde_json`
    as `null` instead and silently diverge — `tests/stats.rs` asserts this field literally rather
    than only diffing against the fixture, for exactly that reason. Implemented under issue #5
    (`src/stats.rs`, wired into `src/lib.rs::select`); closes the four
    `EXPECTED_DIVERGENCES_MANIFEST_ERRORS` entries the four fixtures above tracked
    (`stats_views`, `stats_multi_fields`, `stats_zero`, `stats_zero_fq`).

## Findings from the issue #4 highlighting capture

Claiming findings 52-55 (51 already claimed by issue #5's stats-component capture, above). Nine
new `manifest.tsv` rows (`hl_*`) against a new self-contained `content` core on its own container
(`wayfinder-solr-4`, port 8991), same schema and 5-doc "quick brown fox" corpus as the canonical
container at the top of `capture.sh`. All core-relative GETs, so all in `manifest.tsv`, not
`manifest-errors.tsv`.

52. **The `highlighting` envelope is a top-level object keyed by the unique key, each value
    `{field: [snippet, ...]}` — and critically, a doc that matched the query through a field
    other than the one(s) named in `hl.fl` still gets a key, but that key's value is an *empty
    object*, not an absent key and not `{"body":[]}`.** `hl_no_field_match.json`
    (`q=*:*&fq=category:animals`, `hl.fl=body`, matching doc1 and doc4 through the non-scoring
    `fq` while neither doc's `body` contains "animals") shows exactly
    `"highlighting":{"doc1":{},"doc4":{}}`. This is the single fact this issue most needed pinned
    rather than guessed: Solr does **not** omit the doc, and does **not** emit an empty-array
    field placeholder — it emits the doc key with an empty snippet object. (The fixture uses
    `q=*:*&fq=category:animals` rather than the more obvious `q=category:animals` directly:
    the latter was tried first and surfaced an unrelated, real divergence — Wayfinder orders
    doc4 before doc1 for a bare category-field term query where Solr orders doc1 before doc4, a
    BM25/norm ranking difference orthogonal to highlighting. `q=*:*` gives every matching doc an
    identical score, making the ascending-doc-order tie-break (finding 19) deterministic on both
    engines, so the fixture isolates the highlighting fact without tripping over that separate,
    unfixed ranking gap.) `hl_multi_field_comma.json`/`hl_multi_field_space.json` show the same rule at the
    *field* level within a doc that does have a match elsewhere: `hl.fl=body,category` on a query
    that matches `body` but never `category` (category's values are `garden`/`animals`/`classic`,
    none of which is `lazy`) renders `"doc1":{"body":[...]}` — no `"category"` key at all, not
    `"category":[]`. So the rule is uniform at both levels: a field/doc with no match is *absent*
    from its parent object, and the parent object itself is *never* absent, only ever present
    (possibly empty).

53. **`hl.snippets` does not fabricate snippets that do not exist — one field occurrence still
    yields a one-element array even when `hl.snippets` asks for more.** Neither `hl_basic.json`
    nor `hl_snippets_two.json`'s corpus has a doc where the query term appears twice in `body`
    (the corpus's one repeated word, "the", is an English stopword and never indexed), so
    `hl_snippets_two.json` (`hl.snippets=2`) renders identically to the `hl.snippets`-unset case
    apart from the params echo: `"body":["<em>quick</em> thinking saves the day"]`, a single-
    element array. `hl.snippets` bounds the *maximum* snippet count per field, it does not pad to
    it — there is nothing in scope here proving Solr *can* return more than one, only that it
    never invents one, which is the property Wayfinder's implementation must not violate either.

54. **`hl=true` with no `hl.fl` at all defaults to highlighting the search's default field
    (`df`), not every stored field and not nothing.** `hl_default_fl.json` (`df=body`, no
    `hl.fl`) renders identically to `hl_basic.json` (`hl.fl=body` given explicitly) — only
    `body` is highlighted, `category` never appears in any doc's `highlighting` entry despite
    being stored and docValues. This settles the PRD task spec's "capture it rather than
    assuming" instruction: the default is `df`, not `*` (all fields) and not an empty/absent
    `highlighting` block.

55. **`hl.fragsize` under Solr's default `hl.method=unified` does not truncate a short,
    punctuation-free field at all — not even at `hl.fragsize=1`, verified interactively down to
    that value though only `hl.fragsize=18` is a committed fixture (`hl_fragsize_small.json`).**
    The unified highlighter's sentence `BreakIterator` refuses to cut inside what it considers a
    single sentence, and this corpus's fields have no internal punctuation, so every field comes
    back whole regardless of `hl.fragsize`. This contradicted the issue's premise that a small
    `hl.fragsize` value "forces truncation" under Solr's actual default configuration. Truncation
    *is* observable under `hl.method=original` (the pre-unified classic Highlighter with a
    `GapFragmenter`), captured as `hl_fragsize_truncated.json`
    (`hl.method=original&hl.fragsize=10`): `"quick thinking saves the day"` (29 chars) becomes
    `"<em>quick</em>"` and `"the quick brown fox jumps over the lazy dog"` becomes
    `"the <em>quick</em>"` — truncated hard around the match, no trailing context, no sentence
    awareness. Tantivy's `SnippetGenerator` truncates by a character budget the same
    match-centered way the classic highlighter does, not by sentence boundaries, so
    `hl_fragsize_truncated.json` is the fixture the fragsize *truncation* test derives its
    assertion from; `hl_fragsize_small.json` is kept as the documented default-method surprise
    (a no-truncation control), not as a truncation assertion's source.

Not yet captured for highlighting: `hl.maxAnalyzedChars`, `hl.requireFieldMatch`,
`hl.highlightMultiTerm`, per-field `f.<field>.hl.*` overrides, and any field type other than
`text_en` (a highlighted `string`/numeric/date field was not exercised — `category` above is
requested but never actually matched, so its shape when it *does* highlight remains unseen).
Also not captured: an `hl.fl` naming an undefined or non-text field. Wayfinder renders that as a
400 with the base query's `response` block attached (`WfError::with_response`), by inference from
`facet.field`'s own unknown-field precedent (`facet_unknown_field.json`, issue #35) rather than
from a captured `hl_*` fixture — flag for correction if a real capture ever shows a different
shape.

## Findings from the issue #8 query-types capture

Claiming findings 56-59 (issue #4's highlighting capture has 52-55, above). Forty-six new
`manifest.tsv` rows (content-core GETs, replayed by the differential harness) plus twelve
`manifest-errors.tsv` rows (read-only GETs against the existing `facets` core on the SAME
canonical container — its schema and corpus untouched, so no pre-existing fixture moved).
Capture block appended to `capture.sh`; `animols`/`animblz` are edit distance 1/2 from the
indexed `animals`, and the `content` corpus's stemmed `text_en` field (`lazy` indexed as `lazi`)
is what makes the multi-term-analysis questions answerable at all.

56. **Fuzzy: default distance 2; explicit `~0`/`~1`/`~2` are exact edit distances; out-of-range
    distances are NOT syntax errors; the fuzzy term is lowercased but never stemmed.**
    `category:animblz~` (distance 2) hits with a bare `~` (`fuzzy_default_dist2.json`), `~1`
    misses it and hits the distance-1 `animols` (`fuzzy_dist1_{hit,miss}.json`), `~2` hits, `~0`
    is exact (`fuzzy_dist2.json`, `fuzzy_dist0_exact.json`). `animals~3` and `animals~0.8` are
    both **200s**, not 400s (`err_fuzzy_dist3.json`, `err_fuzzy_fractional.json` — named `err_`
    for the intent of the probe, both came back 200 with the exact-term match set). Against the
    stemmed `body`: `lazy~0` misses (the index holds `lazi`, so the query term was not stemmed)
    while `lazy~1` and `LAZY~1` both hit (`fuzzy_analyzed_{dist0,dist1,case}.json`) — multi-term
    analysis lowercases but does not stem. **Fuzzy matches are scored, not constant-score**:
    `fuzzy_analyzed_dist1.json` returns `doc2, doc1` — NOT insertion order — so a
    constant-score fuzzy (Tantivy's `FuzzyTermQuery` default) diverges on ordering even when the
    match set is right. On a Points-based field, `views:15~1` is a 200 with 0 hits, not an error
    (`qfuzzy_int.json`).

57. **Wildcard and regex are anchored whole-term automata over the indexed terms, lowercased
    (wildcard) / verbatim (regex), never stemmed, constant-score (doc-order results).**
    Trailing `anim*`, single-char `anima?s`, leading `*mals` and infix `an*ls` all work
    (`wildcard_{prefix,qmark,leading,infix}.json` — leading wildcards need no opt-in in Solr 9).
    `body:laz*` hits the stemmed `lazi` and `body:lazy*` misses it
    (`wildcard_analyzed_{hit,stem}.json`) — same not-stemmed rule as fuzzy; `LAZ*` hits, so
    wildcards are lowercased (`wildcard_analyzed_case.json`). `category:*` is the field-exists
    idiom, 4 docs (`wildcard_field_exists.json`); on a pint field `views:1*` is a **400**
    `Can't run prefix queries on numeric fields` (`qwild_int.json`). Regex: `/animals/` hits,
    `/anim/` misses — **anchored full-term match, not substring** (`regex_{full,substring}.json`);
    `/anim.*/` and `/anim[a-z]ls/` hit; `/ANIMALS/` misses — **case-sensitive, no analysis at
    all** (`regex_{dotstar,charclass,uppercase}.json`); `body:/laz./` hits the stemmed `lazi`
    (`regex_analyzed.json`). Every wildcard/regex/range fixture returns docs in insertion order —
    Lucene's constant-score multi-term rewrite — unlike fuzzy (finding 56).

58. **Ranges: `[..]`/`{..}`/mixed `[..}` endpoints behave classically on string, numeric and
    date fields; `*` is the open endpoint; a reversed range is a 200 with 0 hits; `TO` is
    case-sensitive; numeric endpoints must parse as the field's type.**
    String: `category:[animals TO garden]` = 4 docs, `{animals TO garden}` = the strictly-between
    `classic` docs, `[animals TO garden}` = 3 (`range_str_{incl,excl,half_open}.json`), `*` at
    either or both ends works (`range_str_star_{upper,lower,both}.json`), `[garden TO animals]`
    is 200/0 (`range_str_reversed.json`). Lowercase `to` is a 400 SyntaxError
    (`err_range_lowercase_to.json`), as is an unclosed range (`err_range_unclosed_q.json`).
    pint: inclusive/exclusive/half-open/star all value-typed (`qrange_int_*.json`); a float
    endpoint on a pint (`[10.5 TO 30]`) and an alphabetic endpoint are both 400
    `Invalid Number: <token> for field views` (`qrange_int_{float,alpha}_endpoint.json`) — no
    truncation, no lexical fallback; `views:015` matches 15, so bare numeric terms parse
    numerically too (`qterm_int_leading_zero.json`). pdate: RFC3339 endpoints, inclusive and
    exclusive, behave as values (`qrange_date_{incl,excl}.json`).

59. **Boosts reorder scoring exactly as expected and a non-numeric boost is a 400; a bad regex
    is Solr's one captured 500.** Baseline `q=quick garden&df=body` ranks the rarer `garden`
    doc first (`doc2, doc3, doc1`, `boost_baseline.json`); `quick^10 garden` flips it to
    `doc3, doc1, doc2`, and the fielded (`body:quick^10 body:garden`) and float (`^2.5`) forms
    agree (`boost_{term,fielded_term,float}.json`). Boost composes with phrases
    (`boost_phrase.json`) and fuzzy (`boost_fuzzy_combo.json`). `body:quick^bad` is a 400
    SyntaxError (`err_boost_bad.json`). `category:/anim[/` — a regex that parses as a query but
    fails automaton compilation — is a **500** whose error object is `msg, trace, code` with a
    Java stack trace and **no `metadata`** (`err_regex_bad_class.json`); an unclosed `/regex` is
    an ordinary 400 SyntaxError (`err_regex_unclosed.json`). The `trace` key is free text no
    other engine can reproduce, so the differential normaliser drops `error.trace` the way it
    already drops `error.msg` (finding 10). Also pinned while here: a colon inside a quoted
    phrase is NOT a field query — `q="category:animals"&df=body` is a phrase on `body` (0 hits)
    where the unquoted control matches 2 (`phrase_with_colon.json`, `select_q_field_term.json`)
    — which retires schema-layer follow-up 5's open question about what the dynamic-field
    rewrite scan must not rewrite.

    **Round-1 review addendum (seven more `manifest.tsv` rows, same block).** The reviewer named
    three inference gaps and each got its discriminating capture rather than an argument:
    (a) **Fuzzy distance is Damerau — a transposition counts as ONE edit.** `animasl` (last two
    chars of `animals` swapped) is plain-Levenshtein distance 2 but `category:animasl~1` hits
    both `animals` docs (`fuzzy_transposition_dist1.json`; `~2` control captured too), pinning
    Lucene's `transpositions=true` FuzzyQuery default. (b) **Wildcard/fuzzy clauses compose as
    ordinary boolean clauses.** `category:animals OR body:laz*` = 3 docs,
    `body:laz* AND category:animals` = doc1, `(body:laz*)` = 2, and
    `category:animols~1 OR body:garden` = `doc2, doc1, doc4` — scored composition, not a
    whole-query glob and not a silently dropped suffix (`compound_*.json`,
    `grouped_wildcard.json`). (c) **`field:*` works on a plain indexed field with no docValues**:
    `body:*` is a 200 with all five docs (`exists_non_docvalues.json`) — Solr answers exists
    from the postings, so an implementation must not require a fast/docValues column for it.

    **Round-2 review addendum (five more `manifest.tsv` rows, same block).** Purely negative
    queries: Solr answers the **complement** (an implicit match-all MUST clause), never a
    silent 0 and never a 400 — `q=-lazy&df=body` = doc3,doc4,doc5, `NOT lazy` identical,
    `-lazy -dog` = doc3,doc5 (`negative_only.json`, `negative_not_keyword.json`,
    `negative_two_clauses.json`). Mixed positive/negative composes as expected:
    `lazy AND NOT dog` = doc2, `category:animals AND NOT body:garden` = doc1,doc4
    (`negative_and_not.json`, `negative_fielded_and_not.json`). Pins the all-negative
    regression fix (a `(Should, AllQuery)` companion clause when every built clause is
    `MustNot`) to captured behaviour rather than prose.
