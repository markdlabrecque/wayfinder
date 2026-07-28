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

Also uncaptured, and a **known Wayfinder divergence** rather than just a gap: `facet.field` on a
*numeric or date* field. Tantivy 0.26.1 only walks the term dictionary to fill zero-count buckets
for string columns (`aggregation/bucket/term_agg.rs:1024-1053`); its numeric/date branches
(`:1054-1112`) map only the values the hit set produced. So Wayfinder reports numeric/date facet
values present in the hit set and silently drops a value reachable only through a non-matching
document, where Solr would report it at 0. Capture a numeric `facet.field` on the `facets` core
before relying on it — see the `ponytail:` on `CoreIndex::term_facet`.

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
