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

Highlighting, stats, MLT, edismax, `/update` responses, `commitWithin`/`softCommit` behaviour.
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
  Docker itself.

### Adding a query

The query set is `solr-ref/manifest.tsv`, generated by `solr-ref/capture.sh` — it is the single
source of truth. To add a query: add a `cap` line to `capture.sh`, re-run it (needs Docker), and
commit both the new fixture under `solr-ref/responses/` and the updated `manifest.tsv`. Do not
hand-edit the manifest or fixtures.

### Expected-divergence list

`tests/differential.rs::EXPECTED_DIVERGENCES` names manifest entries with a *known, currently
real* Wayfinder-vs-Solr divergence caused by an unbuilt feature (not a harness bug) — currently
`sort` *ordering* (issue #2), plus `ping` for the reason below. The seven faceting entries it
used to carry (`facet.mincount`/`limit`/`missing`/`query`, `json.nl=map`, term-dictionary
enumeration for the zero/all-filtered facets) were deleted when issue #3 landed and they stopped
diverging. Each entry carries a mandatory reason naming the owning issue.

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
    issue-#25 investigation and is **not** pinned by any committed fixture — the only multi-field
    `fl` capture, `select_term` (`fl=id,body`), lists its fields in input order anyway, so it
    cannot discriminate. Treat the `fl` half as inferred, per finding 20's lesson.

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
    compared.

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
