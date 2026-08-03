# Solr reference capture — findings

Source: `solr:9` (`_default` configset), core `content`, 5 docs.
Regenerate with `solr-ref/capture.sh` (gitignored). Fixtures in `solr-ref/responses/`, index in `solr-ref/manifest.tsv`.

Schema: `id` (string, uniqueKey), `body` (text_en, stored), `category` (string, stored,
docValues, multiValued). `doc5` has no `category` — that is what makes `facet.missing`
observable.

## Numbering

A finding is numbered once and never renumbered; a new one takes the next free number rather
than reusing a vacated one. Citations across `src/`, `tests/`, `docs/` and
`solr-ref/capture.sh` point at these numbers, and `tests/finding_citations.rs` fails on a
citation that resolves to nothing and on a number defined twice.

32, 33, 43, 44, 45, 85 and 86 were vacated by renumbers and never reused; a citation of one
of them dangles and the guard catches it.

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
Range facets were captured by issue #3 (findings 105-107) — what is still missing there is
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

Issue #3 and issue #2 both claimed 16-18 for their own findings, which made a citation of
those numbers ambiguous. These three are issue #3's, renumbered to 105-107; issue #2's `sort`
16-18 keep their numbers, being contiguous with 19/20.

105. **The issue-#3 premise was wrong: Solr never errors on a facet it cannot build.**
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

106. **The empty array is a property of the default `fc` facet method, not of the field.**
    `facet.field=body&facet.method=enum` on that same non-docValues field **does** enumerate the
    term dictionary — `["dog",2,"lazi",2,"quick",2,"afternoon",1,...]`, the stemmed `text_en`
    tokens (`facet_non_docvalues_text_enum.json`). Solr's `enum` method walks the inverted index
    instead of the uninverted field, so the data is reachable; the default just declines to reach
    it. `facet.method` is out of scope for #3 (Wayfinder has one implementation, the fast-field
    aggregation), recorded so nobody later concludes from finding 105 that Solr *cannot* facet a
    non-docValues field.

107. **`facet_ranges` envelope — the part that is not guessable.** From
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
  (finding 105, `facet_non_docvalues_text[/_enum]`, `facet_stored_only_field`). These never
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
  `response` key, matching `facet_err_range_single.json` and friends (see finding 50).

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
core, finding 105's unfacetable-field 400). Those get their fixtures in `manifest-errors.tsv` and
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
    naming a non-string fast field, effective `facet.mincount == 0`). `warnings` leads
    `responseHeader` (`warnings, status, QTime, params`), not trails it — per this finding's own
    fixture. Finding 21's `responseHeader` row is the no-warnings case (`status, QTime, params`)
    and does not cover where `warnings` sits.

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

Not yet captured for highlighting: `hl.maxAnalyzedChars`, `hl.highlightMultiTerm`,
per-field `f.<field>.hl.*` overrides, and any field type other than
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
## Findings from issue #6 (MoreLikeThis)

Claiming findings 60-67 (issue #8's query-types capture has 56-59, above).

60. **The `_default` configset does not register `/mlt` at all — a bare `solr-precreate` core
    404s every `/mlt` request with the generic "you must type the correct path" easter egg, not
    a Solr-shaped 404 or an empty result.** Unlike `/select`/`/update`/`/admin/ping`, `/mlt` is
    not a handler `_default`'s `solrconfig.xml` wires up by default (that config ships only the
    implicit `/select`, `/update`, `/get`, and admin handlers). Getting *any* `/mlt` response at
    all required registering the handler via the Config API first:
    `POST /solr/<core>/config {"add-requesthandler":{"name":"/mlt","class":"solr.MoreLikeThisHandler"}}`
    — `solr-ref/capture.sh`'s MLT block does this before indexing. This is a fact about the
    reference container's setup, not a wire-format divergence to track: once the handler exists,
    every fixture below is a plain 200. Not applicable to Wayfinder, which is expected to serve
    `/mlt` unconditionally per PRD §5.

61. **The `/mlt` envelope is `{responseHeader, match, response[, interestingTerms]}` — `match` is
    the source document as its own nested search result, not a bare doc.** `mlt_baseline.json`
    (and every other fixture here): `match` has the full `numFound`/`start`/`numFoundExact`/`docs`
    shape of an ordinary `/select` response, holding exactly the one document `q` resolved to
    (`docs` empty and `numFound:0` if `q` matched nothing — see finding 63). `response` is the
    same four-key shape holding the *similar* documents, i.e. the actual MLT result set that
    `fl`/`rows`/`start`/`sort` semantics apply to. `_version_`/`_root_` appear on stored docs in
    both blocks exactly as `/select` finding 9 describes — Wayfinder's default-`fl` decision
    presumably extends here unchanged, but that's the implementor's call to make explicit.

62. **`interestingTerms` only appears at all when `mlt.interestingTerms` is set to a truthy value
    (`details` here), and is a bare top-level array sibling to `match`/`response` — never nested
    under either.** `mlt_interesting_terms_details.json` has `interestingTerms:[]` (empty, because
    the query in that fixture also produced zero result docs — see finding 64 on why); no other
    fixture carries the key at all. This capture run never exercised a case where interesting
    terms exist *and* `mlt.interestingTerms=details` is set simultaneously — the implementor
    needs a fixture with a real (non-empty) `interestingTerms` array to pin the per-term shape
    (Solr's classic form is `"term:field:weight"` strings, but that is from memory, not this
    capture; treat it as unverified until a fixture actually shows one).

63. **A `q` that resolves to zero source documents is a 200 with `response: null` (JSON `null`,
    not an empty result object) and no `interestingTerms` key at all — not a 404 and not an
    empty-result shape.** `mlt_nonexistent_doc.json` (`q=id:nosuchdoc`): `match.numFound:0`,
    `match.docs:[]` (the ordinary empty-result shape), but `response` is the bare JSON value
    `null`. This is a genuinely different shape from every other captured case, where `response`
    is always an object even when its own `docs` array is empty (`mlt_no_interesting_terms.json`:
    `response:{numFound:0,start:0,numFoundExact:true,docs:[]}`). A response builder that always
    emits the four-key object here would diverge; `response` must be nullable independently of
    `match`.

64. **The handler's real defaults (`mlt.mintf=2`, `mlt.mindf=5`) are already too strict to
    produce any match on a 20-document corpus** — `mlt_baseline.json`/`mlt_fl_restricted.json`
    (`q=id:mlt1`, no tuning params) both get `response:{numFound:0,...,docs:[]}` despite `mlt1`
    and `mlt2` sharing "fresh", "basil", "tomatoes", "pasta" almost verbatim: those terms each
    appear in only 2 of the 20 docs, one short of Solr's default `mindf=5` floor. This is a real,
    intentional Solr default (tuned for corpora far larger than a five-or-twenty-doc fixture set,
    per Lucene's own `MoreLikeThis` javadoc), not a bug or a capture mistake — `mlt_baseline`/
    `mlt_fl_restricted` are legitimate degenerate-empty fixtures in their own right (same shape
    as finding 63's `response` object, `docs:[]`, distinct from finding 63's `response: null`
    case because `match` still finds the source doc here). Every other tuning fixture
    (`mlt_mintf_mindf_maxdf.json` and everything downstream of it) deliberately sets
    `mlt.mintf=1&mlt.mindf=1` first to get a non-empty baseline, so the *other* parameter's
    narrowing effect (`mlt.minwl`/`mlt.maxwl`, `mlt.maxqt`) is visible against real matches
    instead of against another 0.

65. **`mlt.minwl`/`mlt.maxwl` and `mlt.maxqt` measurably narrow the interesting-terms set, and
    `mlt.maxqt` can narrow it all the way to zero real matches even with `mintf`/`mindf`
    loosened.** Starting from `mlt_mintf_mindf_maxdf.json`'s `mlt.mintf=1&mlt.mindf=1` baseline
    (4 matches for `mlt11`: `mlt13, mlt15, mlt12, mlt17`), adding `mlt.minwl=6&mlt.maxwl=10`
    (`mlt_minwl_maxwl.json`) drops the match set to 1 (`mlt12`) — words shorter than 6 letters
    ("night", "sky", "bright") stop counting as interesting terms, which removes enough shared
    vocabulary to sever most of the cluster. Adding `mlt.maxqt=2` instead
    (`mlt_maxqt.json`) drops it to **0** — capping to the top 2 interesting terms by weight
    apparently picks terms too sparse/generic to match anything else in this corpus. Both are
    real, captured Solr behaviour on this corpus, not something to soften in a normaliser.

66. **`mlt.boost=true` changes both the match set and its order relative to the unboosted
    query, on the same loosened thresholds.** `mlt_boost.json` (`q=id:mlt1`,
    `mlt.mintf=1&mlt.mindf=1&mlt.boost=true`) returns 3 matches in order `mlt2, mlt6, mlt10` —
    `mlt2` (the near-duplicate) ranks first as expected, but `mlt6`/`mlt10` (weaker, partial
    vocabulary overlap via the gardening cluster) also surface, an order the differential tests
    must compare via ranked-ID comparison (`diff_ranked_ids`), not membership alone, per PRD §8.

67. **`fl`/`rows`/`start` apply to `response` (the similar-docs list) exactly as they do on
    `/select`, including per-doc `score` and top-level `maxScore` on *both* `match` and
    `response` when `fl` includes `score`.** `mlt_fl_rows_start.json`
    (`mlt.mintf=1&mlt.mindf=1&fl=id,score&rows=2&start=1`, source doc `mlt11`, 4 total matches):
    `response.start:1`, `response.docs` holds exactly 2 entries (`mlt15`, `mlt12`, matching the
    second page of the same order finding 66's `boost` case implies is real, not incidental), and
    `response.maxScore` is the corpus-wide max (1.6043521) — **not** recomputed over just the
    returned page, same as ordinary `/select` `fl=score` semantics (PRD ratified-divergence 4).
    `match` also carries `score`/`maxScore` for the one source doc once `fl` includes `score`,
    even though `match` is a single-doc lookup, not a ranked list — `match.docs[0].score` and
    `match.maxScore` are numerically identical (1.1995715) since there is only one "hit".

## Findings from issue #7 (edismax)

Claiming findings 68-75 (issue #6's MoreLikeThis capture has 60-67, above). Own container/corpus
(`solr-ref/capture.sh`'s edismax block, container `wayfinder-solr-7`, port 8994, 10 docs over
`title`/`body` text_en fields) — same rationale as the MLT block: the 5-doc tracer-bullet corpus
has one text field and no second field to reward differently via `qf`, so it cannot exercise any
edismax-specific behaviour.

68. **The `mm` grammar's exact arithmetic was verified empirically against a live Solr, not taken
    from memory of the reference-guide table** — this is the "hardest single piece" the PRD names,
    and reasoning it out by hand produces plausible-looking numbers that a real capture proves
    wrong (see the `-25%` case below). Verification corpus: a disposable 16-doc probe core (not
    committed — `d0..d15`, doc `dN` containing exactly tokens `t1..tN`) queried with
    `q=t1 t2 ... tK&defType=edismax&qf=body&mm=<spec>`; since doc `dJ` (`J<=K`) matches exactly
    `J` of the `K` optional clauses, the lowest-`J` doc present in the result set *is* the
    required-match count, read directly off real Solr rather than inferred. Confirmed algorithm,
    `calc(spec, clause_count) -> required`:
    - Default (no spec, or a plain absolute/percentage token): `result = clause_count`.
    - A `X<Y` pair: if `clause_count > X`, set `result = apply(Y, clause_count)` and continue to
      the next pair (a later pair can override again); if `clause_count <= X`, leave `result`
      unchanged and continue — **the pair is skipped, not applied**, which is the opposite of the
      naive reading of "if clause_count <= X".
    - `apply(Y, n)`: a bare positive integer is `min(Y, n)`; a bare negative integer is
      `max(0, n + Y)`; a positive percentage `P%` is `floor(P * n / 100)`; a negative percentage
      `-P%` is `n - floor(P * n / 100)` — **floor in both the positive and negative percentage
      cases**, not ceiling for the negative side (the plausible-sounding alternative). This is
      the case memory got wrong: `-25%` at `clause_count=3` is **not** 2 (which a ceiling-based
      "1 clause may be missing" reading gives) — real Solr returns 3 (`floor(0.25*3)=0` clauses
      may be missing, so all 3 are required).
    - Verified table (`spec`, `clause_count` -> `required`): `("1",3)->1`, `("-1",3)->2`,
      `("75%",3)->2`, `("-25%",3)->3`, `("5",3)->3` (clamped), `("0",3)->0`, `("-5",3)->0`
      (clamped), `("100%",3)->3`, `("-100%",3)->0`, `("50%",4)->2`, `("50%",3)->1`, `("33%",5)->1`,
      `("-25%",8)->6`, `("3<-1 10<-2",3)->3`, `("3<-1 10<-2",10)->9`, `("3<-1 10<-2",15)->13`,
      `("2<-1 5<80%",2)->2`, `("2<-1 5<80%",3)->2`, `("2<-1 5<80%",6)->4`. `tests/mm.rs` asserts
      this exact table.
    - Not independently verifiable on this corpus: whether an all-optional `BooleanQuery` floors
      an effective 0-or-negative minimum-should-match up to 1 at the Lucene execution layer.
      Every corpus doc that shares zero terms with the query also shares zero terms with *any*
      disjunction clause, so it can never appear in results regardless of `mm` — Lucene's
      disjunction scorer only iterates postings for docs that hit at least one clause, structurally
      independent of the `mm` value. Treated as out of scope for the *pure* `(spec, clause_count)
      -> required` function per the issue's own framing ("small, pure, self-contained
      function") — a floor-at-1 belongs to the `BooleanQuery`-construction layer, not the grammar
      parser, if Wayfinder needs it at all.

69. **`qf` per-field boosts change which of two same-relevance documents ranks first, and the
    reordering is real (not merely a magnitude change).** `edismax_qf_equal.json`
    (`qf=title body`, unboosted) ranks `eB, eA` (title-only vs body-only matches for the same
    three-word query); `edismax_qf_boost_title.json` (`qf=title^10 body`) ranks `eA` first;
    `edismax_qf_boost_body.json` (`qf=title body^10`) ranks `eB` first — `eA`/`eB` swap ends of
    the result list purely from which field's boost is raised, over the identical query and
    identical two documents.

70. **`pf` adds a phrase-query score on top of the `qf` bag-of-words score; it does not replace or
    re-weight it, and two documents with identical term frequencies score identically without
    it.** `edismax_pf_off.json` (`pf` absent): `pA` and `pB` — same two words, adjacent in `pA`'s
    body ("a quick fox ran away"), split apart in `pB`'s ("a fox that is quick ran away") — score
    **exactly equal** (1.2725438 each; no adjacency-sensitivity leaks in from `qf` alone).
    `edismax_pf_on.json` (`pf=body` added, same `qf`): `pA`'s score exactly **doubles**
    (2.5450876) while `pB`'s is unchanged (1.2725438) — the phrase clause's own score happened to
    equal the bag-of-words clause's score here, which is a property of this fixture's specific
    term statistics, not a general "pf always doubles" rule; what generalizes is additive-not-
    replacing and zero effect on a non-adjacent match.

71. **`tie` blends per-field scores only for a document that actually matches in more than one
    of the `qf` fields — a document matching in exactly one field is untouched by `tie` at any
    value.** `edismax_tie_0.json`/`edismax_tie_1.json` (`q=rocket`, `qf=title body`): `eC`
    ("rocket" in both title and body) scores 0.871532 at `tie=0` and 1.480436 at `tie=1` — up,
    not just different, since `tie=1` sums in the previously-ignored second field's score in
    full. `eD` ("rocket" only in its title) scores 0.72299594 at **both** `tie=0` and `tie=1`,
    bit for bit — there is no second field's score for `tie` to add in.

72. **`boost` is a pure, uniform multiplier — every document's score in the result set scales by
    exactly the same factor, and document order is therefore unaffected by `boost` alone.**
    `edismax_score_baseline.json` vs `edismax_boost_multiplicative.json` (`boost=2` added, same
    `q`/`qf`, same four docs): every one of `eA`/`eB`/`eC`/`eD`'s scores exactly doubles
    (0.5274755->1.054951, 0.71525735->1.4305147, 0.72299594->1.4459919, 0.871532->1.743064) —
    confirms `boost` composes as a multiplicative wrapper around the whole query, per PRD §5's
    v1-exception framing, not a per-field or per-clause effect.

73. **`bq` adds an independent, real (idf/tf/norm-scored) query's score on top of the base score
    — it is not a flat per-doc offset, and it leaves non-matching documents' scores byte-for-byte
    unchanged.** `edismax_bq_additive.json` (`bq=title:mission^5`, same base query/docs as finding
    72's baseline): `eA`/`eB` (no "mission" in title) keep their exact baseline scores
    (0.5274755, 0.71525735); `eC`/`eD` (both have "mission" in title) gain different amounts
    (0.871532->4.810618, a gain of ~3.94; 0.72299594->3.4152434, a gain of ~2.69) — the two
    matching docs gain *different* absolute amounts despite matching the same `bq` clause,
    because `bq`'s own score depends on each doc's own term statistics (`eD`'s title has "mission"
    among fewer/shorter tokens than the surrounding boilerplate would suggest), confirming `bq` is
    a real additive sub-query, not a bonus constant.

74. **Quoted phrases and `+`/`-` operators inside `q` work exactly as in the plain (non-edismax)
    query grammar findings already established (`compound_*`/`negative_*` in the tracer-bullet
    capture), unaffected by `defType=edismax`.** `edismax_quoted_phrase.json`
    (`q="quick fox"&qf=body`) matches only `pA` (the doc with the two words adjacent) — a bag-of-
    words match on `pB` does not qualify once the query itself is a quoted phrase, independent of
    `pf`. `edismax_operators_exclude.json` (`q=rocket -mission&qf=title body`) drops `eC`/`eD`
    (both have "mission"); `edismax_operators_required.json` (`q=+rocket +launch&qf=title body`)
    drops `eC` (the one document with no "launch" anywhere in either field) — `+`/`-` apply across
    the whole `qf` field set per term, not per individual field.

75. **`bf` (function-query boost) is a real, scoring-affecting edismax parameter in Solr itself —
    it is not something Solr silently ignores.** A probe query (`bf=recip(rord(id),1,2,3)`
    against this corpus, not committed as a fixture) changed every document's score relative to
    the no-`bf` baseline. The PRD's "unsupported edismax params are ignored like any unknown
    param (finding 8)" therefore describes **Wayfinder's own chosen behaviour for a
    deliberately-out-of-scope param**, not a claim about what real Solr does with it — this is a
    ratified, PRD-documented divergence (full function-query syntax is out of scope per PRD §5),
    not a compatibility bug, and it is why no `bf` fixture is committed here: a captured Solr
    response for `bf` would be real Solr ground truth for behaviour Wayfinder is explicitly not
    building, and comparing against it would either force scope creep or require a
    fixture-vs-fixture divergence carve-out for a param nobody asked to keep matching.

76. **Captured against a real Drupal 11.3.2 + `drupal/search_api` 1.41.0 + `drupal/search_api_solr`
    4.4.0 site, indexing/searching two node bundles through a stock, unmodified module (issue
    #55), the endpoints the module actually calls are: `update` (bulk JSON body, one POST per
    batch, `commitWithin` set rather than an explicit `commit=true` — no separate commit request
    observed), `select` (all fulltext/filter/facet/sort/spellcheck/highlight traffic — the module
    never uses a dedicated `/facet` or `/spellcheck` handler, everything rides on `select`'s
    components), `mlt` (a dedicated handler for "more like this", not a `select` component),
    `terms` (a dedicated handler, used by the module's own autocomplete/suggester code path —
    `search_api_autocomplete` was not installed for this capture, but `SolrConnector::getTermsQuery()`
    is core to `search_api_solr` itself), `schema/fieldtypes` (a Schema API introspection call
    issued mid-query, not just at config time), and the admin/handshake set: `admin/info/system`
    (server-level), `<core>/admin/system`, `<core>/admin/luke`, `<core>/admin/mbeans`. `/admin/ping`
    is also called (via `SolrConnector::pingCore()`) but landed in this capture's pre-trace setup
    noise rather than the frozen trace. Notably absent from the whole session: the module never
    called anything shard/cluster-related (no `/admin/collections`, no ZooKeeper-flavoured calls),
    consistent with PRD's non-goal of SolrCloud. See `solr-ref/search-api/manifest.tsv` for the
    full request-by-request list (28 request/response pairs, all HTTP 200).

77. **The module's own multi-term AND-conjunction edismax queries silently under-match, because of
    a Solr local-params scoping quirk, not a `search_api_solr` bug in the usual sense.** Building a
    query via the module's `edismax` parse mode with two keywords and the default `AND` conjunction
    produces `q=({!edismax qf='...'}+"term1" +"term2")` — the whole expression wrapped in a literal
    `(...)` group. Captured behaviour: `solr-ref/search-api/trace/00008.json` (`+"quick" +"fox"`,
    both words present in `entity:node/1`'s title) returns `numFound:0`, while the *same* local-params
    clause with only one term (`00006.json`, `+"quick"` alone) correctly returns 2 matches. Root
    cause, confirmed by isolated curl probes against the same core: Solr's local-params syntax
    (`{!edismax qf=...}rest`) only consumes the *entire remainder of the query string* when that
    clause **is** the whole `q` value; once it is wrapped in an enclosing `(...)` group (as the
    module always does for multi-clause queries), only the first whitespace-delimited token after
    `}` is handed to the named sub-parser — every subsequent `+"term"` clause is instead parsed by
    the *outer*, non-edismax, default query parser against Solr's configured `df` field (`id`, set
    via `solr-ref/search-api/configset/solrconfig_extra.xml:113` on the `/select` handler — a
    string field holding values like `4m8z66-capture_index-entity:node/1:en`), which does not carry
    the module's per-field-boosted content and so those clauses can never match, making the
    fall-through behaviour even more certain than a fulltext default field would. The module's
    `OR`-conjunction case (`00007.json`, `"quick" "rocket"`)
    happened to still return correct-looking results in this capture, but only because the corpus
    has overlapping vocabulary (both matching documents also contain "quick") — the second term
    ("rocket") was independently confirmed to be silently dropped there too. Net: `qf` weighting is
    real and applied (finding 70 confirms this against a hand-built query), but the module's own
    generated multi-term `q` string does not reliably deliver every keyword to `qf`-boosted fields
    once more than one keyword is present — a discrepancy any Wayfinder edismax implementation
    aiming for parity should match faithfully rather than "fixing" (PRD: divergence from captured
    Solr behaviour is a bug unless the PRD says otherwise; this is upstream-module-generated Solr
    input, not Wayfinder's own choice, so faithful parity means reproducing Solr's parse of *that*
    string, local-params quirk included) — worth flagging explicitly against issue #7/PR #53's
    edismax work for comparison, since a naive from-scratch edismax implementation is likely to
    treat multi-term `q` more literally than real Solr's local-params scoping does.

78. **Answering PRD open question 2** ("which Solr version does `/admin/system` report"): the
    module detects the Solr version via `SolrConnector::getSolrVersion()`, which reads
    `lucene.solr-spec-version` off `<core>/admin/system` (or `/admin/info/system` as fallback) and
    regex-captures the leading `major.minor.patch`. Against this capture's pinned `solr:9` image,
    that value was `9.10.1` (`solr-ref/search-api/trace/00026.json`) — so a real
    `search_api_solr` 4.4.0 site talking to today's `solr:9` detects and reports Solr 9.10.1, not a
    bare "9". Separately, the generated `schema.xml`'s `<schema name="drupal-4.4.0-solr-9.x-0" ...>`
    attribute (`solr-ref/search-api/configset/schema.xml`) encodes the *targeted* Solr branch
    (`9.x`) and module version (`4.4.0`) the config set was generated for — a second, independent
    version signal the module reads via `getSchemaVersionString()`/`getSchemaTargetedSolrBranch()`.
    Implementation of what Wayfinder itself should report is deferred to issue #59 per the issue's
    scope; this finding is the discovered ground truth that decision should be checked against.

79. **`multipartUploadLimitInKB`/`formdataUploadLimitInKB` do not govern raw `application/json`
    `/update` bodies** (issue #64). The generated `solrconfig.xml`'s `<requestDispatcher>` section
    documents these two `requestParsers` attributes as caps on `multipart/form-data` file uploads
    and `application/x-www-form-urlencoded` POST params respectively — neither content type
    `search_api_solr`'s bulk `/update` uses. Worse, the example block that names them
    (`solr-ref/search-api/configset/solrconfig.xml` lines ~500-537) is itself inside an HTML
    comment; the actual active `<requestDispatcher>` content is the entity-included
    `solr-ref/search-api/configset/solrconfig_requestdispatcher.xml`, which sets neither attribute
    at all (just `httpCaching`). So this capture gives no evidence of an active Solr-side cap on
    raw JSON update bodies, and none of `search-api`'s own captured bulk-update traffic exceeds a
    few KB (the largest fixture in `solr-ref/responses/` is ~7KB) — there is no in-repo, hermetic
    signal for what Solr's own effective raw-body ceiling is. Absent a live-Solr probe (gated
    behind `WAYFINDER_DIFF_SOLR=1`, not run here), Wayfinder's `resources.max_body_size` default
    (see `src/config.rs`) is a deliberate round headroom figure over the largest known fixture, not
    a value derived from a verified Solr default.

80. **Solr 9 accepts `stats=true&stats.field=_version_&function=max(_version_)` as
    search_api_solr's watermark request shape.** A dedicated three-document
    `version99` core captured `stats_version_max.json`: the response is HTTP
    200, echoes both `stats.field` and `function`, and returns the normal
    `stats.stats_fields._version_` metrics with `count:3`, `missing:0`, and a
    `max` equal to the newest auto-assigned version. `function=max(_version_)`
    does not create a second stats entry; `_version_` remains the sole key.
    Its numeric values are update-log/time-derived and therefore intentionally
    fixture-variable; the hermetic test derives its expected maximum from the
    indexed fast field while using this fixture to pin Solr's request and
    envelope behavior. Captured with `solr:9` (issue #99), container
    `wayfinder-solr-99`, port 8999.

## Findings from the issue #104 `hl.fragsize=0` capture

Claiming finding 81. Own container/corpus (`solr-ref/capture.sh`'s fragsize block, container
`wayfinder-solr-104`, port 8995, core `fragsize104`, one ~310-char `text_en` `body` document) —
the shared 5-doc corpus's `body` is four words long, too short for "whole field" and "one
fragment" to be different answers at all.

81. **`hl.fragsize=0` means "return the whole field, unfragmented, as a single snippet" — it is
    not a synonym for "unset", and it holds for both `hl.method` values.**
    `hl_fragsize_zero_whole_field.json` (default `hl.method`, i.e. unified) and
    `hl_fragsize_zero_whole_field_method_original.json` (`hl.method=original`) are byte-identical
    in their `highlighting` block: the entire ~310-char `body`, trailing "." included, with the
    single `quick` match wrapped in `<em>`. This corrects the reading implied by the pre-#104
    code, which filtered a parsed `0` out as if it were absent and so fell back to
    `DEFAULT_FRAGSIZE` (100) under `hl.method=original` and to Tantivy's 150-char
    `SnippetGenerator` default otherwise — both of which truncate. Wayfinder now decides the zero
    case *before* finding 55's `hl.method=original`-vs-everything-else split, mapping it to
    `core_index::WHOLE_FIELD_MAX_CHARS` (`usize::MAX`); finding 55's ponytail scope limit is
    unchanged and continues to govern `hl.fragsize > 0` exactly as before. The fixtures pin one
    detail that a `set_max_num_chars` sentinel alone does not deliver: the
    snippet keeps text *outside* the first/last token boundary (Tantivy's fragment stops at the
    last token's `offset_to`, dropping the field's final "."). Wayfinder additionally returns
    exactly one snippet for `hl.fragsize=0` regardless of `hl.snippets` — but that is an
    **inference, not a captured fact**: none of the three `capf` rows in this block sends
    `hl.snippets` at all, so what real Solr does for `hl.fragsize=0&hl.snippets=3` is uncaptured.
    It follows from "the whole field is the fragment" (there is no second fragment to return),
    which is why it was implemented that way; confirming it needs another capture.

    The same capture's control row `hl_fragsize_small_truncated.json`
    (`hl.method=original&hl.fragsize=40`) is a normal compatibility row after issue #51:
    built-in `text_en` now strips the English stopword `the` (27..30) before stemming, so
    Wayfinder and Solr both end the fragment at `"<em>quick</em> prototype notes from"` (offset
    26). The temporary accepted-divergence waiver was removed and the row now uses the ordinary
    differential assertion.

## Finding from the issue #109 in-query term boost capture

82. **`q=rocket^5` under `defType=edismax` is an exact multiplier on that term's own score
    contribution, scoped to the leaf it decorates — not a whole-query multiplier like `boost=`.**
    Issue #51 reverified this on 2026-07-30 in a clean isolated `solr:9` container using the
    exact edismax schema/corpus and two same-container requests. `q=rocket` returned
    `eC/eD/eB/eA` at `0.871532/0.72299594/0.71525735/0.5274755`; `q=rocket^5` returned the
    same order at exactly 5x: `4.3576603/3.6149795/3.5762868/2.6373773`. That evidence corrected
    the stale one-off `edismax_term_boost.json` fixture without rerunning `capture.sh`; its
    temporary captures remain outside the repository. A second capture (`q=rocket^5 mission`,
    not committed as a fixture since it only confirms scoping, not a new fact) showed `eB`
    (matching only `rocket`) scaled by the same exact 5x while `eC`/`eD` (matching both terms)
    did not scale uniformly, confirming the boost applies to the `rocket` leaf alone and not the
    composed query. Before this fix, `flatten_edismax_clauses` discarded `UserInputAst::Boost`'s
    weight entirely (the same weight the plain, non-edismax `parse_query` path already honors via
    `build_ast`'s own `UserInputAst::Boost` arm), so `q=rocket^5` had no scoring effect at all
    under edismax.

## Finding from issue #110 (`boost=<function-query>`)

83. **`boost` is a function-query parameter in real Solr, not a plain float — real Solr 500s on
    `boost=recip(rord(title),1,1000,1000)` against `title` here only because that field lacks
    `docValues`, an unrelated schema-config error, not evidence about the function-query
    machinery itself.** Confirmed by a one-off capture (500, `IllegalStateException: unexpected
    docvalues type NONE for field 'title'`) that Solr does attempt to *evaluate* a function-query
    `boost`, unlike Wayfinder, which implements no function-query evaluator at all (PRD v1 scope
    explicitly excludes "full function-query syntax", same exclusion as `bf` — finding 75). No
    capture demonstrates a clean 200 for a function-query `boost` Wayfinder could compare itself
    against, since every function needing a real field hits this schema's lack of `docValues`.
    Given the `bf` precedent (issue #108) and PRD scope, the decision this issue asked for is:
    accept-and-ignore, not implement or reject. `params.get("boost").and_then(|s|
    s.parse().ok())` already produces exactly that (`.ok()` turns an unparseable non-numeric
    value into `None`, applying no boost) — this issue's fix is a comment making that
    intentional rather than incidental, plus a test
    (`non_numeric_boost_is_ignored_like_any_unsupported_function_query_not_rejected`) locking the
    behavior in alongside the existing `bf` one.

## Finding from issue #111 (`qf` naming one undefined field among valid ones)

84. **A `qf` naming a mix of valid and undefined fields 400s on the undefined name alone, even
    when another name in the same `qf` is perfectly valid.** Confirmed by a one-off capture
    (`edismax_qf_partial_invalid.json`, not committed through `capture.sh`'s `cape` helper since
    it's an error envelope and the generic hermetic sweep compares `error.msg`/`error.metadata`
    verbatim — same narrow, non-verbatim contract as `tests/error_shapes.rs`):
    `qf=title+nosuchfield` 400s with `"Query Field 'nosuchfield' is not a valid field name"` even
    though `title` in the same `qf` is a real field. Before this fix, `resolve_field_weights`'s
    drop-unknown filtering (built for `pf`'s deliberate unknown-field leniency — a Wayfinder
    choice, not a captured Solr fact — and `qf`'s empty-spec default-field fallback) silently
    dropped `nosuchfield` and 200d using
    `title` alone — the wrong-answer bug this issue tracks. The fix validates every raw `qf` name
    up front via `field_target` (the same static-before-dynamic resolution `resolve_field_weights`
    itself uses, so a `qf` naming only a dynamic field, issue #84, is unaffected) before falling
    through to the existing empty-resolution 400 for a `qf` naming *only* undefined fields.

## Finding from issue #114 (`pf` and a negated clause)

87. **Issue #114's premise -- that `pf` "presumably" should exclude a negated (`-term`) clause's
    text from the phrase it builds -- is wrong; real Solr does exactly what Wayfinder already
    does.** Confirmed by a one-off capture against a query with a negated clause for a term
    absent from every doc (`-zzznonexistent`): `pf`'s boost, present and correctly favoring the
    adjacent-phrase doc without the negation (`edismax_pf_negation_isolated.json`,
    nA=2.03014/nB=1.01507), vanishes completely once the negation is added
    (`edismax_pf_negation_with_absent_negated_term.json`, nA=1.01507/nB=1.01507 -- identical to
    the unboosted score `nB` already carries in the isolated capture, i.e. the boost vanished
    entirely). The scoreboard is consistent with real Solr's own `pf` folding the negated
    clause's text into the phrase it builds, the same as Wayfinder's `literal_texts`
    (unconditional on `Occur`) does today: a phrase containing a term that can never appear in
    any matching doc can never match, silently dropping the boost. (The capture doesn't observe
    Solr's internal parsed query directly -- `debugQuery=true` would settle that for one more
    curl -- but for a single-term `pf` with no `pf2`/`pf3`, "folds the term in" and "skips pf
    entirely" are observationally indistinguishable, and `pf2`/`pf3` are out of PRD v1 scope.)
    This is not a Wayfinder-specific bug -- no production code change was made. Locked in by
    `pf_phrase_over_a_negated_absent_term_loses_its_boost_matching_solr` (`tests/edismax.rs`).

## Finding from issue #112 (`qf` naming validation under `q=*:*`)

88. **`qf` naming validation is independent of `q`'s shape — even `q=*:*`, which short-circuits
    to a match-everything query with no field lookups at all, still 400s on an invalid `qf`.**
    Confirmed by two one-off captures (same schema as the #111 capture above, not re-run through
    `capture.sh`): `edismax_qf_star_unknown.json` (`qf=nosuchfield` alone) and
    `edismax_qf_star_partial_invalid.json` (`qf=title+nosuchfield`, partially valid) both 400
    against real Solr with the same underlying Java exception shape as finding 84's capture, even
    though the query itself is `q=*:*`. Before this fix, `parse_edismax_query`'s `*:*`
    short-circuit (added to special-case Tantivy's `Exists { field: "*" }` parse of `*:*`, see
    finding elsewhere in this doc) returned `AllQuery` before the `qf`-validation loop added for
    issue #111 ever ran, so an invalid `qf` under `q=*:*` silently 200d instead of 400ing. The fix
    moves that existing validation loop (`src/core_index.rs`, `parse_edismax_query`) to run
    *before* the `*:*` short-circuit, not after — the short-circuit itself, and the
    `resolve_field_weights(...).is_empty()`/`default_field` lookups, still run after it and are
    unaffected. Checked directly by `star_query_with_undefined_qf_field_still_400s` and
    `star_query_with_partially_invalid_qf_still_400s` in `tests/edismax.rs`.

## Finding from issue #113 (`mm` present but empty)

89. **`mm` present but empty (`mm=`) is a malformed spec real Solr *rejects*, not one it ignores
    -- but only when `q` yields a multi-clause boolean query. Issue #113's own premise, "Solr
    ignores an empty `mm` param", is wrong.** Confirmed by one-off captures against a disposable
    `solr:9` running this block's edismax schema/corpus (not re-run through `capture.sh`, same
    precedent as findings 82-84): `q=alpha+beta+gamma&defType=edismax&qf=body&mm=` 400s with
    `"Invalid 'mm' spec. Expecting an integer."`, `root-error-class
    java.lang.NumberFormatException` (fixture `solr-ref/responses/edismax_mm_empty_string.json`).
    A bare `mm` with no `=` and a whitespace-only `mm=%20` produce byte-identical 400s -- Solr
    trims before parsing, which is why Wayfinder's guard tests `spec.trim().is_empty()` rather
    than `spec.is_empty()`.

    The second half of the finding, and the part that decides where the check belongs: Solr does
    *not* validate `mm` eagerly as a request parameter. The spec is only parsed when there is a
    multi-clause boolean query to apply it to, so the identical `mm=` 200s whenever `q` yields
    fewer than two clauses. Captured 200s: `q=*:*` (numFound 10), `q=` (numFound 0), `q=alpha`
    (numFound 3), `q=title:rocket`, `q="alpha beta"` (a phrase is one clause), and `q=-mission`
    (numFound 8). Captured 400s: `q=alpha beta`, `q=+alpha +beta`, `q=alpha -mission` -- occur
    kind is irrelevant, only the clause count. A review round that assumed the opposite
    (eager param validation, check before the `*:*` short-circuit) would have shipped a real
    divergence on `q=*:*&mm=`; the capture is what settled it. Precedence against a second bad
    param was captured too: `qf=nosuchfield` alongside `mm=` 400s on the `qf` name (finding 84's
    error), so the `qf` check stays ahead of the `mm` one.

    `mm` entirely *absent* is a different case and unchanged by this issue: no
    `minimum_number_should_match` is set at all and the normal OR default stands
    (`edismax_mm_absent.json`, a committed 200 fixture with its own `manifest.tsv` row).
    `edismax_mm_empty_string.json` is deliberately *not* `cape`d into `manifest.tsv` or
    `manifest-errors.tsv`, for exactly the reason finding 84 (#111) established: it is an error
    envelope, and the generic hermetic sweep compares `error.msg`/`error.metadata` verbatim,
    which can never match (Solr's Java exception text vs Wayfinder's own). It is checked
    directly by `mm_present_but_empty_400s_like_a_malformed_spec` against the narrow, non-verbatim
    contract `tests/error_shapes.rs` documents. The clause-count 200/400 boundary is covered by
    `empty_mm_alongside_a_single_clause_q_does_not_400` and
    `empty_mm_400s_for_every_multi_clause_shape_regardless_of_occur`.

    **Reviewer round-2 follow-up, actioned:** the boundary evidence above previously lived only
    in this prose and in test comments -- the same class of gap that let round 1's bad guard
    placement hide behind a green suite. One point on the boundary (`q=*:*`, a single-clause
    query) is now also a committed fixture + `manifest.tsv` row,
    `solr-ref/responses/edismax_mm_empty_star.json` (asserted directly by
    `empty_mm_alongside_star_all_matches_committed_fixture` and swept generically by
    `hermetic_edismax_manifest_entries_match_committed_fixtures`). That fixture's `numFound`
    (10) is confirmed against the real one-off Solr capture cited above; its exact doc-order/id
    list was reconstructed from Wayfinder's own hermetic output, not independently
    re-captured against a live Solr container (none was available for that follow-up task) --
    see the `cape edismax_mm_empty_star` comment in `solr-ref/capture.sh` for the caveat and
    the remaining step. The other boundary points (`q=alpha`, `q=-mission`, and the three
    multi-clause 400 shapes) remain prose/comment-only evidence, an open follow-up.

    Wayfinder's pre-fix behaviour was wrong in the opposite direction from the issue's premise:
    `edismax::min_should_match` had an `if spec.is_empty() { return clause_count; }` early return,
    silently reading an empty `mm` as "require every clause". That line is removed -- the general
    path returns the identical value for an empty spec anyway (`split_whitespace` yields no
    token, so the all-required default stands), so it was redundant rather than load-bearing.

## Findings from issue #137 (inline `{!edismax qf='...'}` local params in `q`)

90. **`search_api_solr`'s Shape B `q` is broken against real Solr itself, and Wayfinder now
    reproduces the breakage.** The captured `/select` handler defaults
    (`solr-ref/search-api/configset/solrconfig_extra.xml:110-118`) are `defType=lucene`,
    `df=id`, `omitHeader=true`. So in `q=({!edismax qf='...'}+"quick" +"rocket")` the outer
    parser is **lucene** and `{!edismax ...}` is an *inline nested query*, not a position-0
    local-params block that would re-select the parser for the whole `q`. The leading `(`
    is irrelevant to that: the block is never at position 0 anyway. A lucene inline nested
    query binds only the **next run of characters** after `}`; everything after that run is
    parsed by the outer lucene parser against `df`, which here is `id` and matches nothing.
    Solr says so itself: issue #147 captured the parse tree with `debugQuery=true`, in a
    container using `capture.sh`'s edismax block schema/corpus with `qf=title body` against
    `df=id` so the parsed query names the field each token resolved through.
    `solr-ref/responses/edismax_shape_b_debug_parsedquery.json`
    (`q=({!edismax qf='title body'}+"quick" +"rocket")`) parses to
    `(+(+DisjunctionMaxQuery((title:quick | body:quick)))) +id:rocket` -- only the bound run
    reached the nested edismax query, and `+"rocket"` after it went to the outer lucene parser
    against `df=id`, matching nothing.
    `solr-ref/responses/edismax_shape_b_debug_parsedquery_paren_terminated.json`
    (`q=({!edismax qf='title body'}+"quick")`) parses to
    `+(+DisjunctionMaxQuery((title:quick | body:quick)))`, `numFound=2` (`pA pB`). Neither is a
    `manifest.tsv` row -- Wayfinder emits no `debug` section, so the whole-body sweeps could only
    pass by widening a normaliser over a real capability gap (same exclusion as
    `edismax_qf_partial_invalid`, #111); the commands are commented at the end of
    `solr-ref/capture.sh`. All seven captured Shape-B traces fit that model and only that model,
    which is how the rule was originally derived:

    | trace | text after `}` | numFound | under the model |
    |---|---|---|---|
    | 00006 | `+"quick"` | 2 | edismax(`+"quick"`) -> docs 1,3 |
    | 00005, 00007 | `"quick" "rocket"` | 2 | edismax(`"quick"`) OR `id:"rocket"` (no match) |
    | 00003 | `+"quick" +"rocket"` | **0** | edismax(`+"quick"`) AND `id:"rocket"` -> 0 |
    | 00004, 00008 | `+"quick" +"fox"` | **0** | same |
    | 00021 | `+"qwick"` | 0 | typo, no match |

    00004/00008 is decisive: `entity:node/1` is titled "The quick brown fox..." and its body
    also contains "quick", so an edismax applied to the *whole remainder* would return it.
    Real Solr returns 0 because `+"fox"` never reaches edismax at all. Per the compatibility
    contract the fixtures are ground truth, so `src/local_params.rs` reproduces the low-recall
    outcome deliberately -- `tests/local_params.rs`'s two `numFound == 0` tests are the guard
    against a later "obviously more useful" whole-remainder rewrite. A site builder using
    Shape B gets a search box that returns nothing for multi-word AND queries; that is
    `search_api_solr`'s bug to fix, not Wayfinder's to paper over.

91. **The bound run's terminators are whitespace *or* an unbalanced `)`, not whitespace
    alone.** Every captured Shape-B `q` wraps the whole query in `(...)`, so trace 00006's
    `({!edismax ...}+"quick")` has no whitespace after the bound run at all -- binding purely
    on whitespace swallows the closing paren and the query fails to parse. Confirmed directly by
    issue #147's `debugQuery=true` capture of that shape,
    `solr-ref/responses/edismax_shape_b_debug_parsedquery_paren_terminated.json`: real Solr
    answers 200 with `parsedquery` `+(+DisjunctionMaxQuery((title:quick | body:quick)))` and no
    `df=id` clause, i.e. the `)` closed the query's opening paren and contributed no clause of its
    own -- so the terminator is Solr's behaviour, not an inference from `numFound`. The whitespace
    half at run-local paren depth zero has its own capture,
    `solr-ref/responses/edismax_shape_b_debug_parsedquery.json` (see finding 90). `"` is *not* a
    terminator: every captured bound run is a quoted phrase, optionally `+`-prefixed, and the
    quotes belong to the nested query's own text. This is what
    `local_params::bound_token_len` implements. **Issue #197 directly captured that whitespace
    terminates at any paren depth.**
    `solr-ref/responses/edismax_shape_b_debug_nested_paren.json` sends
    `q=({!edismax qf='title body'}(+"quick" +"fox"))`, whose first whitespace after the block is
    inside the bound run's open paren. Real Solr answers 400: the depth-one whitespace cut leaves
    the outer parser the unbalanced remainder `+"fox"))`. Had whitespace waited for depth zero,
    the nested edismax parser would have received the complete balanced expression and the query
    would have parsed. The fixture carries `debugQuery=true` but, correctly for a parse failure,
    has no `debug` section; its commented one-off command is at the end of `capture.sh`, not in a
    manifest whose whole-body comparison would require Wayfinder to implement debug output.

92. **`autoGeneratePhraseQueries` defaults to *off*, so an unquoted string that analyzes to
    several tokens is a boolean OR over those tokens, not a phrase query.** **Settled by
    capture (issue #147).** `solr-ref/responses/edismax_unquoted_multitoken.json` -- manifest row
    `edismax_unquoted_multitoken`, `q=quick%2Brocket&defType=edismax&qf=title+body&sort=id+asc`,
    against a real `solr:9` with `capture.sh`'s edismax block schema and 10-doc corpus -- answers
    `numFound=6` (`eA eB eC eD pA pB`): every document carrying *either* token, and no document in
    that corpus carries the two adjacent, so a phrase reading would have matched 0. The two
    readings are therefore distinguished, and Solr chose OR.
    `tests/edismax.rs::unquoted_multitoken_clause_matches_committed_capture` reads both `numFound`
    and the id list out of that fixture, and the
    `select.q.local-params-edismax.and` coverage probe -- whose expected `Some(2)` was authored
    speculatively in `bb44cc4` (#105) -- now derives its expectation from it (OR over `PROBE_DOCS`,
    where "quick" and "rocket" occur only in doc1 and doc2, gives 2; a phrase reading would give
    0). Before the capture this finding rested on Solr's *documented* default: the configset never
    *sets* the attribute, `solr-ref/search-api/configset/schema.xml:52` declares `version="1.6"`,
    and grepping the whole of `solr-ref/` for `autoGeneratePhraseQueries` returns exactly one hit
    — the XML comment quoted next — so the documented default for `version >= 1.4` (off) governs.
    That inference is now corroborated rather than load-bearing. That one hit -- `schema.xml:63`,
    "autoGeneratePhraseQueries attribute introduced
    to drive QueryParser behavior when a single string produces multiple tokens. Defaults to off
    for version >= 1.4" -- is **inside an XML comment** documenting the history of the
    `version` attribute, and is the line this finding used to be quoted as resting on: it
    evidences what the default is, it is not a setting, and citing it alone establishes nothing.
    Note the 21 pre-#147 `defType=edismax` rows in `solr-ref/manifest.tsv` all use either a quoted
    phrase or `+`-as-space single-token clauses, which is why the distinction went unnoticed until
    issue #137's `{!edismax}quick+rocket` probe. This matters because `+` and `-` are
    ordinary term characters *mid-token* in Lucene's
    `_TERM_CHAR` set, so `quick+rocket` is **one** clause whose analysis yields two terms --
    not two clauses. **That `+` step is captured too, not read off the grammar**: the same request
    with `debugQuery=true`, `solr-ref/responses/edismax_unquoted_multitoken_debug.json`, parses to
    `+DisjunctionMaxQuery(((title:quick title:rocket) | (body:quick body:rocket)))` -- exactly
    **one** `DisjunctionMaxQuery` spanning both analysed tokens. edismax fans each clause out over
    `qf` as its own disjunction, so a two-clause reading would have produced two of them
    (`+(DisjunctionMaxQuery((title:quick | body:quick)) DisjunctionMaxQuery((title:rocket | body:rocket)))`);
    counting them discriminates the readings directly. It also shows the OR structurally rather
    than only through a count: inside each `qf` field the two tokens are a SHOULD pair
    (`(title:quick title:rocket)`), not a `PhraseQuery`. Issue #197 separately captured the `-`
    form rather than continuing to generalise from `+` by grammar alone:
    `solr-ref/responses/edismax_midtoken_minus_debug.json` sends the motivating
    `q=state-of-the-art` and parses to
    `+DisjunctionMaxQuery(((title:state title:art) | (body:state body:art)))`. Exactly one
    `DisjunctionMaxQuery` spans every analysed token, so the hyphens are ordinary mid-token
    characters at query-clause parsing time; the `text_en` analyzer then removes the stopwords
    `of` and `the`, leaving `state` and `art` in that one clause. Like the Shape-B debug captures
    it is deliberately **not** a `manifest.tsv` row (Wayfinder
    emits no `debug` section); the command is commented at the end of `solr-ref/capture.sh` and
    `tests/edismax.rs::unquoted_multitoken_debug_parsedquery_shows_one_clause_over_both_tokens`
    asserts on it. Wayfinder's `build_field_disjunction` previously made a `PhraseQuery`
    for any multi-token clause, which is right for finding 74's fixtures (all quoted) and
    wrong for a bare multi-token string; it now takes the quoted/unquoted distinction from
    the grammar's own `Delimiter`.

## Findings from the issue #143 omitHeader/TZ capture

Claiming finding 93 (94/95 not needed -- no further new Solr fact surfaced by this issue).

No new fixtures were captured for this issue -- the fact below is read off the existing
`search_api_solr` corpus, `solr-ref/search-api/trace/`, which already contains the
evidence.

93. **`omitHeader=true` yields no `responseHeader` key at all, not a present-and-empty one.**
    Twenty of the twenty-eight `search_api_solr` traces send `omitHeader=true`
    (`00002`-`00019`, `00021` on `/select`; `00022` on `/mlt`; `00028` on `/terms`), and every
    one of those response bodies has no `responseHeader` key anywhere in the envelope -- not an
    empty object, not a subset of the usual `status`/`QTime`/`params` fields, just absent. The
    one `/update` trace that touches this param (`00001`) sends `omitHeader=false` and does
    carry a `responseHeader`, so the corpus does not by itself show what `/update` does under
    `omitHeader=true`; see the `ponytail:` on `update_success` in `src/lib.rs`. Nothing in this
    corpus is a 4xx/5xx, so this finding is silent on error envelopes -- that gap is issue #179.

## Findings from issue #139 (`hl.fl=*`, `hl.mergeContiguous`, `hl.requireFieldMatch`)

Claiming findings 94 and 95. No new capture: derived from the already-committed
`solr-ref/search-api/` trace set and configset, which is why this is an inference from
existing ground truth rather than a fixture claim.

94. **`hl.fl=*` expands against the *schema's* fields, not the query's `qf`/`df` set, and a
    field it sweeps up that cannot be analyzed is silently skipped rather than erroring.**
    Every captured `search_api_solr` search sends
    `hl=true&hl.fl=*&hl.requireFieldMatch=false&hl.snippets=3&hl.fragsize=0&hl.mergeContiguous=false&hl.simple.pre=[HIGHLIGHT]&hl.simple.post=[/HIGHLIGHT]`;
    `hl.fl=*` (`hl.fl=%2A`) appears in 19 of the 28 traces. Two things are pinned by the
    traces themselves:
    - **Not a `df` fallback.** The traced core's `/select` handler *does* set a `df`, to `id`
      (`solr-ref/search-api/configset/solrconfig_extra.xml:113`, the `<requestHandler
      name="/select">` defaults block — note it is *not* in `solrconfig_query.xml`, which is
      the file it would be natural to look in). A real `df` therefore exists on every one of
      these requests, and yet no wildcard trace ever keys `highlighting` on `id`: `00002`,
      `00005`, `00006`, `00007` and `00009` all key it on
      `tm_X3b_en_body`/`tm_X3b_en_title` only. That is *stronger* evidence than an absent
      `df` would have been — the fallback candidate is present and still unused — so `hl.fl=*`
      is resolved before, and independently of, finding 54's `df` default.
    - **Non-text fields are skipped, not rejected.** `sm_context_tags`
      (`solr-ref/search-api/configset/schema.xml:161`) is declared `type="strings"
      stored="true"`, i.e. a genuinely stored `StrField`, and it is present in the returned
      docs of every wildcard-`hl` trace that returns docs at all (`00002`, `00005`-`00007`,
      `00009`-`00017`). So Solr's stored-field expansion of `*` demonstrably swept up a
      non-analyzed field, and every one of those responses is a 200 with no `highlighting`
      entry for it. An expansion that ran each expanded name through Solr's per-field
      highlightability check and *rejected* a failure would have had to 400 these requests;
      none did.

      Care is needed with the other non-text names in these docs: `ss_type`, `its_nid`,
      `bs_sticky`, `ds_created` and the dynamic `sm_*` fields are all `stored="false"`
      (`schema.xml:185,187,191,196,202`) and reach `docs` via docValues, so their absence
      from `highlighting` is already explained by Solr's stored-only expansion set and
      evidences nothing about non-text handling. The only genuinely stored non-`tm_` fields
      here are `sm_context_tags`, `id` and `_root_`.

    Note what this bullet does *not* settle: no captured query term ever matches a
    `sm_context_tags` value, so the corpus shows only that a stored `StrField` in the
    expansion set does not error — never what Solr would emit for one on a match.

    What the traces **cannot** settle is "every text field" vs. "the query's `qf` set": the
    only text fields this corpus populates (`tm_X3b_en_body`, `tm_X3b_en_title`) are always
    also in `qf`, and every `q=*:*` wildcard trace (e.g. `00013`) has no term overlap at all,
    so each doc's entry is `{}` whichever way `*` resolved. Solr's own implementation decides
    it: `DefaultSolrHighlighter::getHighlightFields` expands `*` via
    `SolrPluginUtils.expandWildcardsInField` over field *names*, with no reference to the
    query. Wayfinder follows that (`src/highlight.rs::highlightable_fields`), narrowed to
    stored, *analyzed* text fields — see that function's doc comment for the deliberate
    `StrField` divergence and the dynamic-field ceiling.

95. **`hl.requireFieldMatch=false` and `hl.mergeContiguous=false` are Solr's own documented
    defaults and are the values in every captured Search API request.** Issue #139 initially
    allowlisted both on the mistaken premise that Wayfinder already behaved like those false
    paths. Issue #181's discriminating captures corrected that premise: Wayfinder's existing
    field-scoped term extraction actually matched `hl.requireFieldMatch=true`, while its
    original-highlighter fragments did not exactly match either merge control. Findings
    113-114 record the now-fixtured semantics and the implementation supports both values.

## Findings from issue #154 (repeated `add` command keys in one `/update` body)

Captured against a one-off `solr:9` (port 8992, `update9` core, same schema and `u1..u5`
seed as `capture.sh`'s update9 block; the block is appended at the end of `capture.sh` and
the container was removed afterwards). Fixtures `update_repeated_add_*.json` and their
`update_select_after_repeated_add_*.json` corpus states, twelve `manifest-errors.tsv` rows
(POSTs and their follow-up selects, never `manifest.tsv`).

96. **Every occurrence of a repeated top-level command key executes, in body order, and a
    malformed occurrence aborts the whole body.** Solr's JSON update format is a stream of
    commands, not a map: `search_api_solr`'s real body
    (`solr-ref/search-api/trace/00001.json`) repeats `add` once per document, six times.
    Four things the capture settles:

    - **Not last-wins.** `update_repeated_add_batch` sends two `add`s plus a `delete` and a
      `commit` key; both `r1`/alpha and `r2`/bravo are indexed. A parse that goes through
      `serde_json::Value` collapses the duplicate key to the last occurrence and silently
      drops the rest — a 200 with a wrong answer, which is what Wayfinder did before this
      issue.
    - **Body order, not grouped by kind.** A `delete` *between* two adds sees the earlier
      one (`update_repeated_add_delete_between`: `r3` is added then deleted, and is gone;
      `r4` survives). A `delete` *before* an add of the same id does not consume it
      (`update_repeated_add_delete_before`: `r4` is deleted then re-added, and survives with
      the new title `echo`). An "all adds, then all deletes" execution order — Wayfinder's,
      before this issue — loses that second doc. Wayfinder now executes the parsed command
      list in order, coalescing only *consecutive* adds into one batch.
    - **Two adds of the same id leave the last.** `update_repeated_add_same_id` leaves one
      `r5`, body `same id second`. This falls out of in-order execution plus the ordinary
      `overwrite=true` replace-by-uniqueKey, on both engines.
    - **A bad command aborts everything before it.** A doc-less `add` is a 400 ("Missing
      solr document at [66]") and the valid `add` that *preceded* it never lands
      (`update_select_after_repeated_add_missing_doc`, `numFound` 0) even though the request
      carried `?commit=true`; an unknown command key is the same ("Unknown command
      'frobnicate' at [129]", both preceding adds lost). Wayfinder matches by validating the
      whole body in `parse_update_commands` before executing any of it. Message text is
      Wayfinder's own, per the `/update` error contract.

    Ceiling: only the *top level* is duplicate-tolerant. A repeated key inside a command
    value (`{"add":{"doc":{...},"doc":{...}}}`) still collapses to the last occurrence —
    unobserved in any capture or trace, and Solr's own `JsonLoader` reads a single `doc`
    per `add`. Marked `ponytail:` on `UpdateBody` in `src/lib.rs`.

## Findings from the issue #140 `f.<field>.facet.missing` capture

Claiming finding 97 (this block was written against 94 and has been renumbered twice: issue
#139 landed 94/95 first, then issue #154 landed 96).

Captured against a one-off `solr:9` container (port 8992), same schema and 5-doc corpus as the
reference `content` core (`solr-ref/capture.sh`'s top block). No `manifest.tsv` rows -- and
*not* for issue #138's reason. This paragraph was written before the implementation and
originally read "Wayfinder does not implement `f.<field>.facet.*` yet, so a row would only buy
a mandatory `EXPECTED_DIVERGENCES` entry"; #140 implemented `f.<field>.facet.missing` on the
same branch, so that rationale expired on landing. The reason they stay out is now narrower: a
manifest row feeds the whole body to the differential harness, which compares facet bucket
*ordering* verbatim -- a separate question from the precedence semantics these captures settle,
and a deliberate follow-up rather than a side effect of this issue. The claim is still pinned:
all five bodies are asserted whole against these fixtures by `assert_matches_fixture` in
`tests/facet_field_missing_override.rs`. The other `f.<field>.facet.*` params (`.limit`,
`.mincount`, `.sort`, `.prefix`) do remain unimplemented and still 400 under `strict_params`.

97. **`f.<field>.facet.missing` always wins over the global `facet.missing`, unconditionally --
    not merely when the global is unset.** `facet.missing=true&f.category.facet.missing=false`
    drops the null bucket entirely (`facet_missing_field_override_wins_over_global_true.json`),
    and the reverse, `facet.missing=false&f.category.facet.missing=true`, adds it
    (`facet_missing_field_override_wins_over_global_false.json`). The override also works with
    no global `facet.missing` present at all (`facet_missing_field_override_alone.json`). A
    `f.<field>.facet.missing` naming a field that was never itself passed to `facet.field` is
    silently inert: `facet.field=category` alongside `f.body.facet.missing=true` returns
    `category`'s counts with no null bucket and no error/warning
    (`facet_missing_field_override_unrelated_field_no_effect.json`). Issue #138's own capture
    already settled the sibling question of whether `f.<field>.` keys off the field or the
    `{!key=...}` response label -- it is the field
    (`facet_local_params_key_f_field.json`/`_f_key.json`) -- so this issue's captures only needed
    to add the true/false precedence and the unrelated-field-name cases.

## Findings from the issue #141 MLT-refinements capture

Claiming findings 98-101. This block was written against 94-97 and has been renumbered twice:
issue #139 landed 94/95 first, then issue #154 took 96 and issue #140 took 97.

One-off `solr:9` container (port 8996, `wayfinder-solr-141`, removed after capture), same
schema and 20-doc corpus as the issue #6 MLT block (`solr-ref/capture.sh`) -- reindexed
identically, no new fields. Fixtures: `mlt_fq_scope.json`, `mlt_fq_seed_not_filtered.json`,
`mlt_fq_multiple_and.json`, `mlt_match_include_false.json`, `mlt_match_offset.json`,
`mlt_json_nl_map_empty_terms.json`, `mlt_fl_wildcard_score.json`, `mlt_maxntp_noop.json`.

98. **`fq` on `/mlt` filters only the similar-docs result set (`response`), never the seed-doc
    resolution (`match`).** `mlt_fq_scope.json` (`q=id:mlt11&fq=category:astronomy`, loosened
    `mlt.mintf=1&mlt.mindf=1`) narrows the astronomy cluster's 4 unfiltered matches
    (`mlt_mintf_mindf_maxdf.json`: mlt13, mlt15, mlt12, mlt17) to 3, dropping mlt17
    (`category:outdoors`) — `fq` genuinely restricts the similar-docs set. But
    `mlt_fq_seed_not_filtered.json` (`q=id:mlt11&fq=category:cooking` — mlt11 itself is
    `category:astronomy`, which the filter excludes) still resolves `match.docs[0]` to mlt11:
    the filter has no bearing on which document `q` picks as the seed, only on what comes back
    as "similar". `mlt_fq_multiple_and.json` confirms multiple `fq` params still AND together
    on this path, the same as `/select` (`fq=category:astronomy&fq=category:outdoors` — no doc
    is both — empties `response` to 0).
99. **`mlt.match.offset` changes *which* document is resolved as the seed, and is reflected in
    `match.start` — not cosmetic.** `mlt_match_offset.json` (`q=category:astronomy&mlt.match.offset=1`,
    5 total matches for `q`) resolves `match.docs[0]` to mlt12 (the *second* match in doc order),
    not mlt11 (the first, which is what offset 0/absent always picks) — and `match.start` is `1`,
    not the usual `0`. Every existing MLT fixture has `match.start: 0` because none of them set
    this param; this is the first evidence that field is not a hardcoded constant.
100. **`mlt.match.include=false` drops the `match` key from the envelope entirely — not an
    empty-and-present object.** `mlt_match_include_false.json`: same query as the
    `mlt_mintf_mindf_maxdf` baseline, `mlt.match.include=false` added, and the body has no
    `match` key at all (`{responseHeader, response}`), while `response` is unaffected.
101. **`json.nl` reaches `/mlt` only through `interestingTerms`'s container shape, and only when
    it is empty that the difference is visible with Wayfinder's current (always-empty)
    rendering.** `mlt_json_nl_map_empty_terms.json` (`q=id:mlt1`, real Solr defaults — genuinely
    0 interesting terms per finding 62/64 — `mlt.interestingTerms=details&json.nl=map`) renders
    `interestingTerms` as `{ }`, not the default `flat` shape's `[ ]`
    (`mlt_interesting_terms_details.json`, same query minus `json.nl`, is `[ ]`). This is why
    `json.nl` cannot be filed as purely-cosmetic accepted-and-ignore the way `TZ`/`bf` are: a
    non-empty term set under `json.nl=map` would presumably render as a real key/value map
    (unverified — no fixture here has a non-empty term set to confirm the populated shape), but
    even the empty case already diverges by container type.

Also confirmed directly from source, no capture needed: Tantivy 0.26.1's
`tantivy::query::more_like_this::MoreLikeThis` struct
(`~/.cargo/registry/src/*/tantivy-0.26.1/src/query/more_like_this/more_like_this.rs`) has
exactly these knobs — `min_doc_frequency`, `max_doc_frequency`, `min_term_frequency`,
`max_query_terms`, `min_word_length`, `max_word_length`, `boost_factor`, `stop_words` — and
nothing resembling Lucene's `maxNumTokensParsed`. `mlt_maxntp_noop.json` pins a value
(`5000`) far above this corpus's real token counts producing an identical result to the
unmodified baseline, which is the realistic case for `search_api_solr`'s Drupal field bodies.
Real Solr's `mlt.maxntp` genuinely narrows results at a low-enough value:
`mlt_maxntp_low.json`, captured on 2026-08-01 against the identical corpus in a one-off
`solr:9` container for issue #189, pins `mlt.maxntp=1` dropping the astronomy-cluster match
count from 4 to 0. Accepted-and-ignore is therefore a real capability gap, not a safe no-op in
general the way `TZ`/`bf` are.

Issue #189 closes that gap inside Wayfinder's existing reimplementation of Tantivy's private
MLT term-mining algorithm. Lucene 9.12.3's `MoreLikeThis` source establishes the exact missing
semantics: the default is 5000; the counter resets for every stored field value; every
analyzer-emitted token consumes the cap before `isNoiseWord`; zero and negative signed-Java-int
values mine no terms; malformed or out-of-range values are 400s. The committed
`mlt_maxntp_invalid.json` and `mlt_maxntp_overflow.json` error fixtures pin the latter envelope
shape. A separate live check established parse precedence: malformed `q=body:[` wins when
`mlt.maxntp=abc` is also malformed. `CoreIndex::mlt_query` now applies the cap while mining
stored values, and `MLT_PARAMS` allowlists `mlt.maxntp` only because the handler implements it.
The low/high fixture pair, error fixtures, a custom-analyzer multi-value test, signed-int edge
and parse-precedence tests, and the semantic coverage probe guard against regression.

Also confirmed: the `fl=*,score` gap is not `/mlt`-specific. `mlt_fl_wildcard_score.json`
shows real Solr returning every stored/docValues field plus `score` for `fl=*,score`, but
Wayfinder's `CoreIndex::render_doc` treats `fl` as a literal field-name allowlist with no `*`
wildcard handling at all — `fl=*,score` on **`/select` today** (verified directly, not just
inferred) returns only `score`, dropping every other field, even though a captured
`search_api_solr` fixture (`solr-ref/search-api/trace/00010.json`, `fl=*,score`) already shows
real Solr returning everything. The existing `select.fl.wildcard-plus-score` coverage probe
(`src/coverage.rs`) only asserts `score` is present, not that other fields survive, so it is a
false-positive green today. The fix belongs in `render_doc` (shared by `/select` and `/mlt`),
not `/mlt`'s handler alone, so it is **descoped from #141 to issue #188**. Two expiring guards
pin the gap meanwhile: `tests/mlt.rs::mlt_fl_wildcard_plus_score_still_drops_every_field_until_issue_188`
and the `mlt_fl_wildcard_score` entry in that file's `MLT_EXPECTED_DIVERGENCES`, both of which
fail as soon as `render_doc` learns `*`. The fixture stays committed for #188.

## Finding from issue #149 (colliding facet response keys)

One-off `solr:9` container (port 8997, `wayfinder-solr-149`, removed after capture), with the
tracer-bullet schema and five-document corpus from `solr-ref/capture.sh` recreated verbatim.
Fixtures: `facet_collision_field_flat.json`, `facet_collision_field_map.json`,
`facet_collision_query_flat.json`, `facet_collision_query_map.json`.

102. **Solr emits duplicate JSON object members for colliding `facet.field` labels, but
     coalesces duplicate `facet.query` values itself; `json.nl=map` does not change either
     result.** `{!key=x}category` plus `{!key=x}id` produces two literal `"x"` members in
     request order under `facet_counts.facet_fields`: first category's buckets, then id's.
     The default writer makes each value a flat alternating array; `json.nl=map` changes each
     value to a bucket object but leaves the duplicate outer `"x"` members intact. Ordinary
     JSON object models cannot represent that response faithfully and generally retain only
     one member. In contrast, sending `facet.query=category:animals` twice produces exactly
     one `"category:animals":2` member with either `json.nl` shape -- Solr has already
     coalesced it before writing. The field-collision fixtures stay out of `manifest.tsv`
     because its differential harness parses bodies into `serde_json::Value`, which would
     discard one duplicate and report a false-positive match; dedicated tests inspect those
     fixtures as raw text.

## Finding from issue #169 (`/terms` differential coverage)

Captured 2026-08-01 against `solr:9.10.1` on a clean `content` core with the
tracer-bullet schema and five-document corpus from `solr-ref/capture.sh`.
Fixture: `terms_body.json`.

103. **Resolved by issue #205: the canonical differential core exposed Solr's Porter
     terminal-`y` rule.** The request
     (`terms?terms=true&terms.fl=body&omitHeader=true&wt=json`) originally returned the same ten
     terms and frequencies except that Solr stemmed corpus token `day` to `dai` while Tantivy's
     English stemmer left it as `day`. This was not the Search API configset's hypothesized
     char-filter, length-filter, or word-delimiter mismatch: the differential core uses
     `_default`'s `text_en`. Wayfinder's analyzer contract v2 now applies the captured Porter
     terminal-`y` rule to static built-in `text_en`, requires affected v1 indexes to reindex,
     and compares `terms_body` normally with no exact-diff waiver. The shared `_dynamic_text`
     catch-all retains v1 Snowball behavior separately: the captured Search API update contains
     singular `day` and its terms trace preserves `day`, so applying the canonical rule globally
     would create a different compatibility bug.

## Finding from issue #150 (duplicate facet local-param keys)

Captured against a one-off `solr:9` container (port 8998, `wayfinder-solr-150`, removed after
capture), with the tracer-bullet schema and five-document corpus from `solr-ref/capture.sh`
recreated verbatim. Fixture: `facet_local_params_duplicate_key.json`.

108. **A repeated key inside a `facet.field` local-params block keeps the first value, not the
     last.** `{!key=a key=b}category` returns category's counts under `"a"`; there is no `"b"`
     member. This contradicts issue #150's source-based guess that Solr's map write would make
     the last value win. Wayfinder's ordered `LocalParams::params` plus first-match `get`
     already agrees. The fixture has a `manifest.tsv` row because it is an ordinary
     core-relative GET whose JSON the differential harness can represent faithfully.

## Findings from issue #171 (`/update/extract` exploration)

Captured 2026-08-01 against `solr:9.10.1` with the `extraction` module enabled and a
Search-API-shaped `ExtractingRequestHandler`. Fixtures: `extract_plain_text_xml.json`,
`extract_plain_text_text.json`, `extract_html_index.json`, `extract_html_select.json`, and
`extract_corrupt_pdf.json`.

109. **`extractOnly=true` returns the multipart part name as the content key plus a flat,
     nested-valued metadata NamedList; the indexing path instead returns only a header.** A
     Solarium-shaped multipart part named `file` produces `{responseHeader,file,file_metadata}`
     even when `resource.name=sample.txt`: the resource name appears in metadata but does not
     rename `file`. `file_metadata` is a flat alternating JSON array whose values are arrays,
     preserving repeated Tika metadata. The default extracted value is XHTML; adding
     `extractFormat=text` returns text with Tika's leading/trailing newlines intact. Without
     `extractOnly`, `literal.id=extract-html-captured&fmap.content=body&commit=true` answers with
     `responseHeader` only, and the companion select proves the literal ID and mapped body were
     indexed; handler-default `captureAttr=true` plus `fmap.a=links` also captures anchor
     attributes. A malformed PDF returns the normal HTTP/error-code 500 envelope. The multipart
     fixtures stay out of `manifest-errors.tsv` until its JSON-body-only runner and Wayfinder's
     absent route are extended; exact reproduction is appended to `capture.sh`.

## Finding from issue #184 (`hl.fl=*` over a stored string field)

Captured against a clean one-off `solr:9` container (port 8999,
`wayfinder-solr-184`) with the tracer-bullet schema and five-document corpus from
`solr-ref/capture.sh` recreated verbatim. Fixture: `hl_wildcard_stored_string.json`.

110. **`hl.fl=*` and an explicit `hl.fl=category` both highlight a stored `string`
     field whose value contains the query term.** For `q=category:animals`, wildcard
     expansion returns `category:["<em>animals</em>"]` under both matching documents,
     just as the explicit path does. Solr's `StrField` is therefore not merely present
     in the wildcard expansion set and then silently skipped: it produces a snippet on
     the wire. Wayfinder's issue #139 exclusion of raw strings is a real unintended
     divergence, not an equivalent implementation choice.

## Finding from issue #177 (dotted dynamic field names)

Captured against a one-off `solr:9` container (port 8999, `wayfinder-solr-177`, removed after
capture), with the tracer-bullet schema and five-document corpus plus one unstored
`tm_X3b_en_*` dynamic-field rule. Fixtures: `dotted_dynamic_basic.json`,
`dotted_dynamic_leading.json`, `dotted_dynamic_trailing.json`, and
`dotted_dynamic_consecutive.json`.

111. **Solr accepts dots as ordinary characters in dynamic field names, including empty path-like
     segments.** Fields named `tm_X3b_en_a.b`, `tm_X3b_en_.leading`,
     `tm_X3b_en_trailing.`, and `tm_X3b_en_a..b` all index successfully and each exact fielded
     query returns its one source document. Leading, trailing, and consecutive dots are not
     rejected or collapsed. These four fixtures replace issue #164's Tantivy-source-derived
     assumption with Solr wire evidence and are ordinary core-relative GETs in `manifest.tsv`.

## Findings from issue #179 (`omitHeader` on errors and boolean spellings)

Captured against a clean one-off `solr:9.10.1` container (port 9010,
`wayfinder-solr-179`, removed after capture). Fixtures:
`omit_header_error_true.json`, `omit_header_error_yes.json`,
`omit_header_update_error_true.json`, and the raw `omit_header_invalid_one.html`
response.

112. **`omitHeader` suppresses `responseHeader` on error responses, and its accepted values
     are case-insensitive `true`/`yes`/`on` and `false`/`no`/`off` — not `1` or `t`.** The
     undefined-field control with `omitHeader=false` returns the normal 400 JSON envelope with
     `responseHeader`; the otherwise identical `true`, `yes`, and `TRUE` requests return only
     the `error` block. A malformed `/update` POST likewise drops its normally header-bearing
     `NoParams` error envelope under `omitHeader=true`. Success probes gave the same result for
     `on` and every case variant.
     This settles #179's original question in favour of suppression. It also corrects the issue
     comment's premise: on Solr 9.10.1, `omitHeader=1` and `omitHeader=t` are invalid booleans,
     returning HTTP 400 Jetty HTML before the JSON response writer runs; `0`, `f`, `y`, and `n`
     are invalid likewise. The three JSON captures live in `manifest-errors.tsv`; the raw `1`
     response stays outside it because the differential harness intentionally parses that
     manifest as JSON.

## Findings from issue #181 (highlighting true paths)

Captured against a one-off `solr:9` container (port 9011, `wayfinder-solr-181`, removed after
capture) with the dedicated three-document corpus recorded at the end of
`solr-ref/capture.sh`. Fixtures: `hl_require_field_match_{false,true}.json` and
`hl_merge_contiguous_{false,true}.json`.

113. **`hl.requireFieldMatch=true` filters query terms per target field, rather than dropping
     fields whose query clauses did not contribute to the document match.** For
     `q=title:quick OR body:fox`, document `rfm1` still receives both `title` and `body`
     snippets because both fields have query clauses. The discriminating change is inside the
     body snippet: false highlights both `quick` and `fox`, while true leaves `quick` plain and
     highlights only `fox`. Document `rfm2`, which matched only through `body:fox`, likewise
     retains its body snippet with only `fox` marked. The true path is therefore field-scoped
     query-term extraction, not document-level matched-clause filtering.

114. **`hl.mergeContiguous=true` coalesces adjacent original-highlighter fragments until a
     real gap remains.** With `hl.method=original`, `hl.fragsize=20`, and three spaced query
     terms, false emits three snippets. True joins the first two into one continuous substring
     (including all intervening text) and leaves the third separate because an unselected gap
     remains. It does not concatenate snippets with a synthetic separator or merge every
     fragment indiscriminately.
## Finding from issue #187 (boolean param parsing)

Captured 2026-08-01 against a one-off `solr:9` container (port 8996), with the tracer-bullet
schema and five-document corpus from `solr-ref/capture.sh` recreated verbatim. Fixtures:
`bool_facet_missing_upper_true.json`, `bool_facet_missing_yes.json`, `bool_facet_missing_on.json`,
`bool_facet_missing_no.json`, `bool_facet_missing_prefix.json`, `bool_facet_missing_invalid.json`,
`bool_facet_on.json`, `bool_facet_invalid.json`, `bool_omit_header_yes.json` -- all nine have
`manifest.tsv` rows.

115. **Solr's boolean params are prefix-matched and case-insensitive, and an unrecognised value
     is a 400 -- but `1`/`0`/`t`/`f`/`y` are NOT recognised.** Issue #187's own premise said they
     were; captured Solr rejects all five. On the value lowercased, `StrUtils.parseBool` answers
     `true` when it *starts with* `true`, `on` or `yes` (`TRUE`, `Yes`, `oN`, `truestuff`,
     `onward`, `yesss` are all true), `false` when it *starts with* `false` or `off` or *equals*
     `no` exactly (`offside`, `falsey`, `NO` are false), and otherwise throws. The `no` arm is
     the one exact match in the rule: `noo` is invalid, not false, so it cannot be folded into
     the prefix list. The error is a 400 whose `error.msg` is `invalid boolean value: <raw
     value>` verbatim, with `error.code` and `responseHeader.status` both 400.

     **Where the 400 surfaces depends on when the param is read.** `facet` is read before the
     base query runs, so `facet=1` answers with the error-only envelope -- `responseHeader` and
     `error`, no `response` block (`bool_facet_invalid.json`). `facet.missing` is read inside
     faceting, after the base query has already produced its hits, so `facet.missing=nope`
     answers with issue #35's shape: the base query's real `response` block sits between
     `responseHeader` and `error` (`bool_facet_missing_invalid.json`). Wayfinder reproduces the
     split by reading `facet`/`stats`/`hl` at handler entry and letting `facet.missing`'s error
     out through `facet::facet_counts`'s non-`PreQueryFacetError` path.

     **Relationship to finding 112 (issue #179).** That finding probed `omitHeader` and
     concluded its accepted values are case-insensitive `true`/`yes`/`on` and `false`/`no`/`off`.
     This one refines it: the match is by *prefix*, not equality -- it simply never probed a
     value like `truestuff`. The two agree on everything they both tested, including that
     `1`/`0`/`t`/`f`/`y` are invalid. One rule, `StrUtils.parseBool`, governs every boolean
     param; `Params::validate_omit_header` now routes through the same shared parser.

     **Divergence, deliberate: an invalid `omitHeader` gets Jetty's HTML error page from Solr,
     an ordinary JSON 400 from Wayfinder.** `omitHeader=1` never reaches a JSON response writer
     in Solr, because header suppression is decided before that writer exists, so the container's
     own HTML 400 page comes back instead (captured by issue #179 as
     `omit_header_invalid_one.html`, deliberately outside `manifest-errors.tsv` since that
     harness parses bodies as JSON). Wayfinder validates `omitHeader` in `check_params` and
     answers with its normal JSON envelope. The status code matches; the body does not, and
     reproducing Jetty's page is not worth it. The accept side *is* fixtured:
     `bool_omit_header_yes.json` shows `omitHeader=yes` suppressing `responseHeader` exactly as
     `omitHeader=true` does.

## Finding from issue #196 (partial `fl` pattern capture)

116. **On the Search API corpus, `fl=ss_*` returns HTTP 200 and only the five matching `ss_*`
     fields, excluding `id` and `timestamp`.** `select_fl_ss_wildcard.json` is the captured
     evidence for this field-selection result.

## Findings from issue #223 (configured spellchecker output)

Captured against a dedicated `solr:9` Search API configset core (port 9012) with two documents
whose `spellcheck_en` dictionary contains `quick`/`rocket` and whose `spellcheck_und` dictionary
contains `quack`/`garden`. The self-contained setup is appended to `solr-ref/capture.sh`.

117. **Spellcheck named-list rendering follows `json.nl`, repeated dictionaries use the first
     requested dictionary, and a collation substitutes every misspelled token.** Under
     `json.nl=flat`, `suggestions` alternates each misspelled token with an object containing
     `numFound`, UTF-16-code-unit `startOffset`/`endOffset`, and a suggestion string array;
     `collations`
     is `["collation", "quick rocket"]`. Under `json.nl=map`, those become
     `{"qwick": {...}, "roket": {...}}` and `{"collation":"quick rocket"}`. With
     `spellcheck.dictionary=en&spellcheck.dictionary=und`, `qwick` becomes `quick`; reversing
     the repeated parameter order makes it `quack`, proving first-dictionary precedence rather
     than merging or per-dictionary output. `spellcheck.collate=true` emits one corrected query
     string and no hit count when `spellcheck.collateExtendedResults` is not requested. Fixtures:
     `spellcheck_flat.json`, `spellcheck_map.json`,
     `spellcheck_dictionary_en_first.json`, and `spellcheck_dictionary_und_first.json`.
     `spellcheck_unicode_offsets.json` resolves the offset unit directly: for `é qwick`, Solr
     reports `qwick` at `startOffset:2,endOffset:7`, i.e. Java UTF-16 code units rather than
     Rust UTF-8 byte positions (`3..8`).

## Finding from issue #229 (HTTP Basic authentication)

118. **The issue #229 premise was wrong: Solr's BasicAuthPlugin failure body was not a JSON
     envelope.** Captured 2026-08-01 from a cloud-mode `solr:9` container with BasicAuthPlugin
     enabled by `auth enable operator:secret/blockUnknown`. An unauthenticated request to
     `/solr/admin/info/system` returned HTTP 401 with Jetty HTML whose message was
     `Authentication failed, Response code: 401`; the same request with a wrong credential
     returned HTTP 401 Jetty HTML `Bad credentials`. Both responses included
     `WWW-Authenticate: Basic realm="solr"`. A request with the correct `operator:secret`
     credential returned HTTP 200.

     The ticket claimed Solr returned a JSON error envelope for auth failures. This capture
     corrects that premise: the auth filter answered before Solr's JSON response writer. Wayfinder
     deliberately matches the 401 and challenge realm but returns its JSON `WfError` envelope;
     that ratified divergence is PRD §2, divergence 9. It follows the same JSON-only client
     response-surface decision as divergences 1 and 8 rather than adding Jetty HTML solely for
     authentication failures.

## Finding from issue #251 (benchmark cold/warm split)

119. **`wt=json` alone is not enough on the admin endpoints: Solr renders a type signature,
     not data.** Observed 2026-08-01 against a live `solr:9` container with a 2M-doc core.
     `GET /solr/<core>/admin/mbeans?cat=CACHE&stats=true&wt=json` and
     `GET /solr/admin/metrics?group=core&prefix=CACHE.searcher.queryResultCache&wt=json` both
     return HTTP 200 with a body whose keys are unquoted and whose values are the literal type
     names, e.g.

         { responseHeader: { QTime: int, status: int } solr-mbeans: [string] (2) }

     That is not valid JSON and carries no statistics at all. Adding any recognised
     response-writer parameter restores real output: `indent=true`, `indent=false`, and
     `json.nl=map` each work; an unrecognised parameter (`x=1`) and `omitHeader=true` do not.
     `/select?wt=json` is unaffected and returns real JSON without any extra parameter.

     Consequences for anything reading these endpoints: the URL must carry a writer parameter,
     and `admin/metrics` is the better target than `admin/mbeans` because its body is plain
     nested JSON. `admin/mbeans`'s `solr-mbeans` is a flat alternating
     `[name, value, name, value, ...]` array by default and a map only under `json.nl=map`, so a
     parser must pick one and cannot straddle both. Note also that `admin/metrics` is
     server-level, not core-relative: the core appears as a registry key `solr.core.<core>`
     inside `metrics`. Mind the shape difference between the two endpoints. Under
     `admin/metrics` the counters are a **nested bean**: `metrics` -> `solr.core.<core>` ->
     `CACHE.searcher.queryResultCache` -> `{lookups, hits, inserts, hitratio, size, ...}`.
     Under `admin/mbeans` the same counters appear as **flat, fully-qualified string keys**
     inside a bean's `stats` map, e.g. `CACHE.searcher.queryResultCache.hits`. Either way the
     `.searcher` scope is part of the path -- an unscoped `CACHE.queryResultCache` matches
     nothing.

     Both halves of this cost a full bad measurement round: a benchmark harness built to the
     unparameterised URL and the unscoped key aborted an hour into a 2M-document run.

## Findings from issue #258 (`/update/extract` extractOnly tracer)

Captured 2026-08-02 against `solr:9.10.1` (container `wayfinder-solr-258`, port 9020, core
`extract258`, removed after capture) with the `extraction` module and the same
Search-API-shaped `ExtractingRequestHandler` as the #171 block. Fixtures:
`extract_html_only_xml.json`, `extract_html_only_text.json`, `extract_latin1_text.json`,
`extract_utf8_bom_text.json`, `extract_declared_charset_text.json`. Inputs:
`extract-inputs/sample.html`, `sample-latin1.txt` (raw ISO-8859-1 bytes),
`sample-utf8-bom.txt` (UTF-8 with a BOM).

120. **`extractFormat=text` always opens with exactly thirteen newlines, independent of the
     document, its format, and how many metadata keys the head carries.** #171's plain-text
     fixture (nine `meta` elements plus `title`) and this issue's HTML fixture (eleven `meta`
     elements plus `title`) both return `"\n" * 13` before the first character of content. The
     count is therefore a fixed artifact of Tika's XHTML-to-text serialization, not one newline
     per head child as the differing meta counts would otherwise suggest. It is reproducible as
     a constant. The trailing edge is *not* padded: the value simply ends with whatever the
     content ended with (`...Second line.\n\n` for the plain-text file whose last byte is a
     newline, `...Main paragraph.\n` for the HTML file whose last block is a `<p>`).

121. **HTML `extractOnly` returns the same `{responseHeader, file, file_metadata}` envelope as
     plain text, with `title` and `author` promoted into metadata.** `file_metadata` carries
     both `dc:title` and a bare `title` for the same value, plus `author` from
     `<meta name="author">`. The XHTML `file` value keeps `<div>` and `<p>` structure and
     rewrites the anchor with `captureAttr`'s `shape="rect"` attribute first; `extractFormat=text`
     flattens the same document to `Captured title\n\nIgnored wrapper Linked words\nMain
     paragraph.\n` after the thirteen leading newlines — the title text is part of the text
     output, and inline elements do not introduce breaks while block elements do. #171 had no
     ground truth for this path: its HTML captures were the *indexing* path, which answers with
     a bare `responseHeader`.

122. **A declared charset beats detection, a BOM beats detection, and the resolved charset is
     echoed in three places.** Posting the ISO-8859-1 bytes with no declared charset detects
     `ISO-8859-1`; posting the same bytes as `text/plain; charset=ISO-8859-1` also resolves
     `ISO-8859-1` but additionally propagates the declared value into `stream_content_type`
     (`application/octet-stream` in the undeclared case). The UTF-8-with-BOM file resolves
     `UTF-8` and the BOM is consumed, not emitted as U+FEFF in the value. In every case the
     resolved charset appears as the `Content-Encoding` metadata key, as the `charset=` of the
     second `Content-Type` metadata value, and inside the XHTML head's `Content-Type` meta.

123. **`X-Parsed-By` is the only part of the extractOnly envelope Wayfinder cannot honestly
     reproduce.** Every other metadata key (`resourceName`, `Content-Type`, `stream_name`,
     `stream_source_info`, `stream_size`, `stream_content_type`, `Content-Encoding`, and HTML's
     `dc:title`/`title`/`author`) is a property of the request and the document. `X-Parsed-By`
     names Java classes (`org.apache.tika.parser.DefaultParser`,
     `.csv.TextAndCSVParser`, `.html.HtmlParser`) that do not exist in a Rust server, and the
     XHTML `file` value embeds them as `meta` elements. This is a ratified divergence, not a
     to-do: see the PRD's ratified-divergence list.

124. **`extractFormat=text` leading newlines = (head `<meta>` count) + 2 when a
     non-empty `<title>` is present, else + 4 — finding 121 generalized and
     corrected.** Finding 121's "always exactly thirteen newlines" holds only
     for the plain-text/HTML fixtures it was measured on (9/11 metas + the
     empty/non-empty title). Office formats break it: DOCX has 41 head metas
     and 43 leading newlines, PPTX 42/44, XLSX 28/30, ODS/ODT/ODP/RTF 8/12
     (no title), HTML 11/13. The count is *driven by the head metadata size*,
     so it is not independently reproducible once Wayfinder's narrower
     metadata set (finding 125) means a different meta count than Tika. The
     differential harness collapses leading newlines for these rows; the
     constant-13 specialisation in `ExtractRender` stays correct for the
     plain-text/HTML rows where it was captured.

125. **Office/ODF/RTF metadata is rich and format-specific; Wayfinder's narrow
     promise (title/author/created + the request envelope) is a documented,
     normalized divergence.** Tika emits dozens of keys per format
     (`date`, `cp:revision`, `dc:creator`, `extended-properties:*`,
     `xmpTPg:NPages`, `meta:page-count`, ...) in a format-specific order in
     both `file_metadata` and the XHTML head. The PRD's stated promise is the
     narrow set only (`resourceName`, detected content type, format
     title/author when reliable); unknown metadata is dropped. So the six
     stream/resource keys plus `Content-Type` are the comparable core across
     every format, and the rest is stripped by `normalize_extract` for the
     office rows. This is the office-format counterpart of `X-Parsed-By`
     (finding 123) — same kind of divergence (a Java/Tika artefact Wayfinder
     has no honest equivalent for), larger cardinality.

126. **Each office family emits a distinct, reproducible XHTML body shape.**
     Captured (with embedded-content divs stripped from the python-docx/pptx
     inputs):
     - DOCX: `<h1>Heading</h1>\n<p>…</p>\n` — heading then paragraph blocks.
     - PPTX: per slide, `<div class="slide-content"><p>title</p>\n<p>bullet</p>\n</div>\n<div class="slide-master-content" />`.
     - XLSX: per sheet, `<div><h1>SheetName</h1>\n<table><tbody><tr>\t<td>…</td>…</tr>\n…</tbody></table>\n</div>\n`.
     - ODS: `<table><tr>\t<td><p>…</p>\n</td></tr>\n</table>\n` (no sheet-name heading, no `tbody`).
     - ODT: `<h1>Heading</h1><p>…</p>\n<p>…</p>` (no newline after the first `<h1>`).
     - ODP: per slide, `<div><p>…</p>\n</div>\n`.
     - RTF: `<p>…</p>\n<p><b>bold run</b></p>\n…` — paragraphs, inline `<b>` for bold runs.
     The leading `\t` before each `<td>` is Tika's table-cell serializer, not
     indentation; reproduced literally.

127. **Malformed-input 500 envelopes: truncated archives choke the zip reader
     (EOF/InvalidFormat); RTF is lenient and needs a multi-gigabyte `\bin`
     declaration to fail.** A 64-byte truncation of any zip-based format
     500s with a `root-error-class` like
     `org.apache.poi.openxml4j.exceptions.InvalidFormatException`. RTF is the
     exception: unbalanced groups, dangling control words, deep nesting, bad
     hex, and a huge `\u` value all parse to 200 (empty or partial text) —
     only `{\rtf1\ansi\bin9999999999 …` 500s, with
     `java.io.EOFException` as Tika tries to skip the claimed bytes and runs
     out. So the RTF malformed fixture is the `\bin` form, not a structural
     break; Wayfinder's `rtf-parser` reaching the same input is expected to
     error into the `Parse` arm (500) the same way.

128. **`json.nl` reshapes the extractOnly `file_metadata` NamedList but leaves
     `responseHeader` and `file` untouched.** Captured for #274 against
     `solr:9.10.1`: with the default (`flat`/omitted), `file_metadata` is the
     alternating array `["key",[values],...]` already pinned by #171/#258;
     `json.nl=map` makes it an object `{"key":[values],...}` (key order
     preserved), `json.nl=arrarr` an array of two-element arrays
     `[["key",[values]],...]`, and `json.nl=arrmap` an array of one-entry
     objects `[{"key":[values]},...]`. `responseHeader` is a `SimpleOrderedMap`
     and so stays an object in every shape, and `file` is a String value (not a
     nested NamedList), so neither moves under any `json.nl` — confirmed
     byte-identical across all four values. So `json.nl` on `/update/extract` is
     *not* accepted-and-ignored (the #258 follow-up's worry): it is a real
     feature the handler must implement, identical in model to the facet routes'
     `JsonNl`. Fixtures: `extract_plain_text_json_nl_{map,arrarr,arrmap}.json`
     (the `flat` baseline is `extract_plain_text_xml.json`). Side note: an
     *invalid* value (`json.nl=garbage`) makes Solr's JSONWriter emit truncated,
     invalid JSON (`"file_metadata"` with no value) while still answering HTTP
     200 — actively-worse behaviour Wayfinder does not reproduce; unknown values
     fall back to `flat` instead (PRD section 2 divergence, not captured as a
     fixture because the malformed body is unparseable by the harness).

## Findings from the `search_api_solr` 4.4.0 source sweep (wave 0b of #289-#302)

Unlike every finding above, these come from reading the module, not from a Solr
capture: `coverage/search_api_solr_4.4.0_source/` (three files —
`SearchApiSolrBackend.php`, `SolrConnector/SolrConnectorPluginBase.php`,
`SolrSpellcheckBackendTrait.php`). They answer "what does the client actually
emit?", which the wire capture cannot show for a code path the capture never
exercised. Line numbers are that snapshot's.

They are still ground truth for *scope* — what the module can send — but not for
Solr's *response* to it. Anything below that a Wayfinder issue implements still
needs a real `solr:9` fixture for the response shape.

129. **Function-query scoring is emitted inline in `q` as `{!boost b=...}`, never
     as `bf=`.** `SearchApiSolrBackend.php:1953-1977`: when `defType` is not
     `edismax` and the query sorts by `search_api_relevance`, the module
     prepends to the flattened keys either
     `{!boost b=sum(boost_document,<per-field boosts>)}` (when the index has
     `solr_document_boost_factors`) or the bare `{!boost b=boost_document}`,
     followed by `Utility::flattenKeysToPayloadScore($keys, $parse_mode)`. The
     per-field boost strings are processor-supplied templates with a
     `FIELD_PLACEHOLDER` substituted for the boostable field name, so the
     *function set* is open-ended by construction — it is whatever a boost
     processor writes, not a fixed list the module hard-codes. The only
     functions this snapshot names itself are `sum()`, `max()`, `geodist()`,
     and `payload_score` (via `flattenKeysToPayloadScore`, which is outside the
     three-file snapshot). So #289 cannot be scoped as "implement these N
     functions"; it must be scoped as a function-query *parser and evaluator*
     with `sum`, `boost_document` as a field reference, and `payload_score` as
     the concrete first targets, and `bf=` is not on the critical path at all.
     Note also that `{!boost b=...}` is a *query parser* local param on `q`,
     which is a different implementation site from the `bf`/`boost` request
     params Wayfinder accepts-and-warns on today (`src/lib.rs:2880`).

130. **`setGrouping()` sends exactly six `group.*` params, and always requests
     `group.ngroups=true`.** `SearchApiSolrBackend.php:4575-4634`:
     `group.field` (repeatable, one per grouping field), `group.ngroups=true`
     unconditionally ("we always want the number of groups returned so that we
     get pagers done right"), `group.truncate`, `group.facet`,
     `group.limit` (only when set *and* not 1), `group.offset` (when set), and
     `group.sort` as a single comma-joined string. `group=true` itself comes
     from Solarium's grouping component. The module refuses to group on a
     fulltext field or on anything it knows to be multiValued, logging an error
     instead — so #290's server side only needs single-valued non-text fields.
     Grouped responses are consumed at `2954` (`$result->getGrouping()`) and
     `2971-2987`, reading `$response['grouped'][<field>]['groups']`, each
     group's `['doclist']['docs']`, and `['ngroups']`. `group.format` and
     `group.main` are never sent, so the flat/`simple` response shape is out of
     scope for parity.

131. **Stock `search_api_autocomplete` uses the `terms` component only — and it
     sends `terms.prefix` and `terms.limit`, which Wayfinder's `TERMS_PARAMS`
     does not accept.** `getAutocompleteSuggestions()` (3973-3994) calls
     `setAutocompleteTermQuery()` (4033-4039), which sets exactly
     `terms.fl` (the fulltext fields), `terms.prefix` (the incomplete key), and
     `terms.limit` (`$query->getOption('limit') ?? 10`); results are read back
     out of the terms component at 4055-4075. The suggester component is *not*
     on this path: `twm_suggest` (2435-2436, a `solr_text_suggester` field) is
     the suggester's backing field and is reached from a different plugin.
     This is a live parity bug, not just a v3 gap. `TERMS_PARAMS` in
     `src/lib.rs` is `["terms", "terms.fl", "omitHeader", "wt", "json.nl"]`, and
     `TERMS_DEFAULT_LIMIT` is hard-coded — so with `strict_params = true` a
     stock autocomplete request 400s, and without it the prefix is silently
     dropped and the user gets the field's top 10 terms regardless of what they
     typed. The 75/75 coverage claim does not catch it because trace `00028.json`
     captured only `terms=true&terms.fl=tm_X3b_en_title`: the capture never typed
     a partial word. **#291 splits**: accepting `terms.prefix`/`terms.limit` is a
     small, urgent server fix that closes autocomplete, and the SuggestComponent
     (`/suggest`) is a separate, later piece gated on the
     `solr_text_suggester` data type (#300).

132. **`_version_` is only ever read, and only through a JSON facet
     aggregation.** All fourteen references
     (`SearchApiSolrBackend.php:1067-1089`, `4934-4940`, `5023-5123`) are the
     server-status "max document version" screens, which send
     `json.facet` with `{local_key: maxVersion, function: 'max(_version_)'}`,
     optionally nested under `terms` facets on `hash`, `index_id` and
     `ss_search_api_datasource`. The module never *writes* `_version_` and never
     sends it as an optimistic-concurrency precondition on update. So #293 is not
     "store a per-document version"; the real dependency is **JSON facets with
     aggregation functions and nesting**, a considerably larger and differently
     shaped piece of work than the issue assumes, and one whose only client is an
     admin diagnostics screen. Recommend rescoping #293 to that finding and
     deprioritising it accordingly.

133. **The module does emit `facet.heatmap`, and consumes the `counts_ints2D`
     grid.** `setRpt()` (called from `1873-1874` for the `search_api_rpt` query
     option) sends `facet=on`, `facet.heatmap=<field>`,
     `facet.heatmap.geom`, `facet.heatmap.format`, `facet.heatmap.maxCells` and
     `facet.heatmap.gridLevel`, plus an `fq` of `<field>:<geom>`; `geom` defaults
     to `["-180 -90" TO "180 90"]`. Extraction at `3263-3286` requires
     `facet_counts.facet_heatmaps.rpts_<name>` and sums `counts_ints2D`.
     Separately, `setSpatial()` (3243, and the body at the `setSpatial`
     definition) emits `sfield`/`pt`/`d` via Solarium's spatial component, adds
     `fl=<distance_field>:geodist()`, rewrites a sort on the distance field to
     `sort=geodist() <dir>`, filters with `{!geofilt}` / `{!bbox}` for `<`/`<=`
     and `{!frange l=..[ u=..]}geodist()` for `>`/`>=`/`BETWEEN`, and turns a
     facet on the distance field into N `facet.query` entries of the form
     `{!key=spatial-<field>__distance-<min>-<max>}{!frange l=<min> u=<max>}geodist()`.
     Every other operator throws. That fixes #292's split: heatmap
     (`rpt` type) and point-distance (`location` type) are two separate features
     with two separate field types, and the distance-facet rewrite is a third
     piece that depends on both `facet.query` and `geodist()`.

134. **The module declares twelve non-default data types plus an open-ended
     `solr_text_custom:<code>` family.** `supportsDataType()` (795-826):
     `location`, `rpt`, `solr_date_range`, `solr_string_storage`,
     `solr_string_docvalues`, `solr_text_omit_norms`, `solr_text_suggester`,
     `solr_text_spellcheck`, `solr_text_unstemmed`, `solr_text_wstoken`,
     `solr_text_custom`, `solr_text_custom_omit_norms`; anything matching
     `solr_text_custom:*` is accepted if the custom code is a configured
     `SolrFieldType`; anything else falls back to
     `Utility::getDataTypeInfo($type)['prefix']`. Field-name prefixing keys off
     `solr_text_*` as a class (2729) and `solr_date_range` gets its own indexing
     branch (2764). For #300 this means the type list is not a flat enum to
     copy: `solr_text_custom` is by design an escape hatch for site-defined
     analyzer chains, which Wayfinder's `presets/search-api.toml` has no
     equivalent for. Recommend #300 implement the ten closed types and record
     `solr_text_custom*` as an explicit descope with the reason.

135. **The site hash is never derived by the backend — it is read from
     `Utility::getSiteHash()` or overridden per datasource.**
     `getTargetedSiteHash()` (4098-4107) returns
     `$config['target_hash'] ?? Utility::getSiteHash()`, memoised per index;
     `Utility` is outside the three-file snapshot, so the derivation itself is
     not visible here. What *is* visible is the whole contract the server sees:
     the hash appears only as document content written by `getDocuments()` —
     `id` is `createId($site_hash, $index_id, $id)` (1333), the `hash` field is
     set to it (1345), and `sm_context_tags` gains
     `search_api_solr/site_hash:<hash>` (1346) — plus as an `fq` of
     `+hash:* +index_id:*` and a `terms` facet on `hash` in the status query
     (5019-5061). It is an opaque per-site string the client generates and the
     server only stores and matches literally. **#301 therefore has no server
     work at all**: it is a `DocumentBuilder` change to write the three fields
     in the module's format, and the derivation function it must match lives in
     `search_api_solr`'s `Utility`, which is not in the snapshot and must be
     fetched before implementing.

## Findings from the wave-1 capture prep of #295 and #308 (`{!tag}`/`{!ex}`, `terms.prefix`/`terms.limit`)

Back to wire evidence: 34 fixtures captured 2026-08-03 against a real `solr:9`
on the canonical `content` core (5 docs, `body` text_en, multiValued `category`),
via `capture.sh --only '^(facet_extag_|terms_prefix_|terms_limit_)'`. Rows are in
`manifest.tsv`, so the differential harness replays them verbatim.

One shared capture serves both issues because both wanted the same corpus and
the same run; the fixture prefixes keep them separable (`facet_extag_*` for
#295, `terms_prefix_*`/`terms_limit_*` for #308).

136. **`{!ex=<tag>}` on `facet.field` excludes exactly the `fq` carrying
     `{!tag=<tag>}`, and every way of *not* matching a tag is a silent no-op,
     never an error.** Baseline `fq=category:animals` with a plain
     `facet.field=category` counts `animals 2, classic 1, garden 0, misc 0`
     (`facet_extag_baseline`); tagging the `fq` and excluding it gives the
     unfiltered distribution `animals 2, classic 2, garden 1, misc 1`
     (`facet_extag_excluded`). All four near-misses return the *filtered* counts
     with HTTP 200 and no warning: `{!tag}` present but no `{!ex}`
     (`facet_extag_tag_no_ex`), `{!ex}` present but no `{!tag}`
     (`facet_extag_ex_no_tag`), `{!ex=nosuch}` naming a tag nobody set
     (`facet_extag_ex_unknown_tag`), and an empty value on either side —
     `{!ex=}` (`facet_extag_ex_empty`) or `{!tag=}` (`facet_extag_tag_empty`).
     For #295 this is the whole error model: there is none. A typo'd tag name
     degrades to "filter still applied", which is also the pre-#295 Wayfinder
     behaviour, so the feature can land without an error path.

137. **Exclusion is per-`fq` and tags are comma lists on both sides.** With two
     tagged filters, `{!ex=a}` drops only that one — `fq={!tag=a}animals` +
     `fq={!tag=b}classic` + `{!ex=a}` counts `classic 2, animals 1, misc 1,
     garden 0`, i.e. the `classic` filter is still in force
     (`facet_extag_ex_one_of_two`) — while `{!ex=a,b}` drops both and returns
     the fully unfiltered distribution (`facet_extag_ex_two_tags`). An untagged
     sibling `fq` is never excludable (`facet_extag_two_fq_one_tagged`, same
     counts as excluding one of two). A single `fq` may carry several tags:
     `fq={!tag=a,b}animals` with `{!ex=b}` excludes it
     (`facet_extag_multi_tag`), so the match is set-intersection between the
     `ex` list and the `tag` list, not string equality.

138. **`{!ex}` and `{!key}` compose in either order, and `key` still decides the
     response label.** `{!ex=cat key=unfiltered}` and
     `{!key=unfiltered ex=cat}` produce byte-identical bodies keyed
     `unfiltered` (`facet_extag_ex_with_key`, `facet_extag_key_before_ex`), so
     local-param order carries no meaning. Two `facet.field` entries on the same
     field, one plain-keyed and one excluded-and-keyed, both appear:
     `{"filtered": [animals 2, classic 1, garden 0, misc 0], "unfiltered":
     [animals 2, classic 2, garden 1, misc 1]}` (`facet_extag_both_facets`).
     That fixture is the direct evidence for #299's fix — one field, two facets,
     two distinct keys, different counts — and it means #295 must key buckets
     before deduplicating them, not after.

139. **`facet.query` accepts `{!ex}`, and its response key is the raw parameter
     value including the local-params prefix.** `facet.query={!ex=cat}category:classic`
     under a tagged `fq` answers `"facet_queries": {"{!ex=cat}category:classic": 2}`
     — count 2, the excluded value, under a key that still carries the
     `{!ex=cat}` text verbatim (`facet_extag_facet_query_ex`). Solr does not
     strip local params from `facet_queries` keys the way `{!key}` renames
     `facet_fields` keys. Wayfinder echoes the raw `facet.query` string as its
     key already, so this half needs the local params parsed for *effect* while
     the key stays untouched.

140. **`facet.mincount` and `facet.missing` apply to the post-exclusion counts.**
     With `{!ex=cat}` active, `facet.mincount=2` keeps `animals 2, classic 2` and
     drops `garden 1`/`misc 1` (`facet_extag_mincount`) — the `1`s are the
     unfiltered counts, so exclusion runs first and mincount filters the result.
     `facet.missing=true` appends the usual `null` bucket with count 1, doc5
     having no `category` (`facet_extag_missing`). No interaction to special-case.

141. **`terms.prefix` filters the term dictionary literally — no analysis, no
     error for a miss.** `terms.prefix=d` on `body` returns
     `["dog", 2, "dai", 1]`, count-descending then index-ascending
     (`terms_prefix_body_multi`); `th` returns the single `["think", 1]`
     (`terms_prefix_body_single`); `zzz` returns `[]` with HTTP 200
     (`terms_prefix_body_none`). A count tie breaks alphabetically —
     `a` gives `["afternoon", 1, "all", 1]` (`terms_prefix_tie`). The prefix is
     **case-sensitive and unanalysed**: `D` returns `[]`
     (`terms_prefix_case`) even though `d` matches two terms, so the component
     reads the indexed dictionary rather than running the field's analyzer over
     the prefix. It works on a `string` field too (`category`, prefix `c` ->
     `["classic", 2]`, `terms_prefix_string_field`), an empty
     `terms.prefix=` means no filter at all (`terms_prefix_empty`, 10 terms —
     the default limit), two `terms.fl` values each get their own filtered list
     (`terms_prefix_two_fields`), and an unknown field yields
     `{"nosuchfield": []}`, not a 400 (`terms_prefix_unknown_field`). That last
     one matters for #308: stock `search_api_autocomplete` will name fields that
     may not exist on the index.

142. **`terms.limit` defaults to 10, truncates after ordering, and `-1` means
     unlimited.** `terms.limit=1` on prefix `d` keeps only `["dog", 2]`
     (`terms_limit_below`), `99` returns both terms rather than padding
     (`terms_limit_above`), `0` returns `[]` (`terms_limit_zero`), and `-1`
     returns all 19 `body` terms (`terms_limit_negative`) — so the limit is
     applied to the already-ordered list and negative is a sentinel, not a
     clamp-to-zero. With no `terms.prefix`, `terms.limit=2` gives the top two of
     the whole dictionary (`terms_limit_no_prefix`). A non-numeric value is the
     one error case in the whole set: `terms.limit=abc` is **HTTP 400** with
     `"error": {"code": 400, "msg": "For input string: \"abc\""}` and an *empty
     but present* `"terms": {}` alongside it (`terms_limit_invalid`) — Solr
     emits the component's container before the parse fails, which Wayfinder's
     error envelope must reproduce. `json.nl=map` renders the pairs as an object,
     `{"body": {"dog": 2, "dai": 1}}` (`terms_prefix_json_nl_map`).

## Findings from the wave 2 function-query capture (#289)

Captured 2026-08-04 against a real `solr:9` on a dedicated `fnq` core (5 docs,
numeric `docValues` fields `boost_document`/`views`/`rating`/`price`, with `d4`
missing `price` and `d5` missing `views` to pin the missing-value default).
15 rows in `solr-ref/manifest-errors.tsv` under a dedicated `fnq_app` in
`tests/differential.rs`. This is the wire evidence for finding 129's
correction — the module emits `{!boost b=...}` on `q`, never `bf=` — and it
fixes the scope of #289's "first targets".

143. **A match-all scores a constant `1.0`, which is what makes a function
      boost's score comparable.** `q=*:*` (and `*:*` under `defType=edismax`)
      scores every document exactly `1.0`, so `{!boost b=<f>}*:*` is
      `1.0 * <f>` and `bf=<f>` under edismax is `1.0 + <f>` — the captured
      score is the pure function value, with no BM25 base. That keeps the
      differential comparison exact rather than under PRD's ratified
      BM25-magnitude divergence, and is the reason the `bf`/`boost`/`{!boost}`
      fixtures below all use `q=*:*` rather than a text query. `{!func}<f>`
      ranks by `<f>` directly (the function value *is* the score), matching
      all documents.

144. **`bf` is additive, `boost` is multiplicative, and `{!boost b=}` is
      multiplicative on the wrapped query.** Captured: `bf=sum(views,rating)`
      under `*:*&defType=edismax` scores `1.0 + sum` (`fnq_bf_additive`:
      d4=42, d2=35, …); `boost=product(rating,2)` scores `1.0 * product`
      (`fnq_boost_param`: d3=12, d5=10, …); `{!boost b=sum(...)}` on `*:*`
      scores `1.0 * sum` (`fnq_boost_sum`). `boost` may be a plain number
      (`boost=2`, the simplest constant function) or a function
      (`boost=product(rating,2)`); `bf` is always a function (a bare number is
      its constant form). A missing numeric field value resolves to `0.0`
      (`fnq_func_missing`: d5 has no `views`, so `sum(views,rating)` scores it
      `0+5`).

145. **`{!payload_score}` is a separate query parser over a payload-bearing
      field type, not an arithmetic function — so it is a follow-up to #289's
      evaluator, not part of it.** `Utility::flattenKeysToPayloadScore`
      (`src/Utility/Utility.php:981`, fetched from `git.drupalcode.org`'s
      `4.4.x` branch — outside the three-file snapshot) emits
      `{!payload_score f=boost_term v=<term> func=max}` blocks, one per search
      term, over a `boost_term_payload` field type (a text field with a
      DelimitedPayload payload filter). That is the `{!payload_score}` query
      parser reading indexed payloads — a different implementation site and a
      different field-type requirement from the arithmetic evaluator
      `{!boost b=...}` consumes. `ms`/`rord` are off the corrected client path
      too (finding 129 corrected the
      `product(...,recip(ms(...)))`-as-`bf` premise: `rord()` over a Points
      field is a hard 400) and need date/ordinal field types. So #289's
      arithmetic evaluator is the foundation; `{!payload_score}` and
      `ms`/`rord` extend `src/function_query.rs` later.

146. **Function-query errors are 400 `SyntaxError`s with Solr-Java messages
      the differential harness normalises away.** `bogus(1,2)` →
      `"Unknown function bogus in FunctionQuery(...)"`; `sum(boost_document`
      (unbalanced) → `"Expected ')' at position 18"`; `{!func}` (empty) →
      `"Expected identifier at pos 0 str=''"`; `nosuchfield` →
      `"undefined field: \"nosuchfield\""` (`fnq_err_*`). All are HTTP 400 with
      `error.code: 400`; `error.msg`/`metadata`/`trace` are dropped by
      `normalize`, so only status + `error.code` are wire-compared.

147. **`f.<X>.facet.*` resolves `X` against the field name, never against the
      `{!key=}` label.** `facet.field={!key=cat}category&f.category.facet.limit=1`
      returns one bucket under the label `cat`; the same query with
      `f.cat.facet.limit=1` returns all four
      (`facet_perfield_key_by_field` / `facet_perfield_key_by_key`, and
      `pf296_sort_key_by_field` / `pf296_sort_key_by_key` for `facet.sort`).
      This settles the premise #296 was written on. It also means the two
      addresses are not interchangeable in one direction only: a per-field
      param naming the key is silently ignored, not an error. Upstream never
      meets the distinction — `SearchApiSolrBackend::setFacets()` discards the
      Search API delta and sets `local_key` to the Solr field name, so key and
      field are always the same string on the real client wire (the captured
      contract's `f.ss_type.facet.missing` is that shape).

148. **`facet.*` settings can be carried as local params on `facet.field`, and
      Solr honours them.** `facet.field={!key=cat facet.limit=1}category`
      limits that facet; so do `facet.mincount`, `facet.missing` and
      `facet.sort` (`facet_perfield_lp_*`, `pf296_sort_lp`). A `key` is not
      required — `{!facet.limit=1}category` works and keeps the field name as
      the label (`facet_perfield_lp_no_key`). The mechanism is
      `SimpleFacets.parseParams`, which does
      `SolrParams.wrapDefaults(localParams, orig)`: the local params of the
      facet being parsed shadow the request params for that facet only.

149. **Local params are the only way to give two facets on one field different
      settings.** `{!key=a facet.limit=1}category` plus
      `{!key=b facet.limit=3}category` returns one bucket under `a` and three
      under `b` (`facet_perfield_two_lp`, and `pf296_sort_two_lp` for two
      different sort orders). The per-field form cannot express it: both
      facets share the field, so `f.category.facet.limit` sets both, and
      `f.a.facet.limit`/`f.b.facet.limit` set neither (finding 147,
      `facet_perfield_two_by_key`). This is the shape #299's delta-keyed
      facets produce and the reason #296 cannot be built out of
      `f.<field>.facet.*` alone.

150. **`facet.limit` is applied after `{!ex=}` exclusion, like
      `facet.mincount` and `facet.missing` (finding 140).** With
      `fq={!tag=cat}category:garden` and
      `facet.field={!ex=cat key=un}category&f.category.facet.limit=1`, Solr
      returns `animals 2` — the top bucket of the *excluded* (wider) list.
      Ranking the filtered counts would have returned `garden`
      (`facet_perfield_ex_limit_rank`, and its local-param twin). The
      `fq=category:animals` variants cannot show this: `animals` is the top
      bucket under either ranking, which is why the `garden` rows exist.
## Findings from the #300 non-default data-type source sweep

Fetched the full `search_api_solr` 4.4.0 source (outside the three-file
snapshot) to pin the type→dynamic-field-prefix table that `Utility::getDataTypeInfo()`
returns and that `SearchApiSolrBackend::supportsDataType()`/field-mapping rely on.
This is the authoritative answer to finding 134's "not a flat enum to copy": the
prefixes come from two distinct sites, and two of the twelve non-default types
are special-cased to fixed sink fields before the prefix logic ever runs.

151. **The data-type→prefix table has two sources, and suggester/spellcheck are
      fixed-field sinks, not prefix mappings.** `Utility::getDataTypeInfo()`
      (`src/Utility/Utility.php:56-110`) hard-codes the six defaults —
      `text=>'t'`, `string=>'s'`, `integer=>'it'` ("Use trie field for better
      sorting"), `decimal=>'ft'`, `date=>'d'`, `boolean=>'b'` — plus
      `duration=>'it'`, `uri=>'s'`, and the Search-API-Location `location=>'loc'`/
      `geohash=>'geo'`/`rpt=>'rpt'`. The `solr_*` non-default types get their
      prefixes from the `search_api_data_type_info_alter` hook
      (`src/Hook/SearchApiSolrHooks.php:837-861`), which sets
      `solr_date_range=>'dr'`, `solr_string_docvalues=>'zdv'`,
      `solr_string_storage=>'z'`, `solr_text_custom=>'tc'`,
      `solr_text_custom_omit_norms=>'toc'`, `solr_text_omit_norms=>'to'`,
      `solr_text_spellcheck=>'spellcheck'`, `solr_text_suggester=>'tw'`,
      `solr_text_unstemmed=>'tu'`, `solr_text_wstoken=>'tw'`. Each data-type
      plugin also declares its own prefix via `SearchApiDataTypePrefixInterface::
      getPrefix()` (`src/Plugin/search_api/data_type/*.php`), matching the hook.
      **But two of these never reach the generic prefix path:***
      `SearchApiSolrBackend`'s field-mapping loop special-cases
      `solr_text_suggester` → the fixed field `twm_suggest`
      (`SearchApiSolrBackend.php:2433-2437`) and `solr_text_spellcheck` → the
      fixed, language-specific field `spellcheck_<lang>` (`2440-2447`) *before*
      the `getDataTypeInfo` lookup at `2448`. So their registered `'tw'`/
      `'spellcheck'` prefixes are effectively dead for naming. For #300 this is
      the decision rule: a type is expressible on Wayfinder as a normal
      prefix+infix dynamic field iff it has a prefix and is NOT one of these two
      sinks; the two sinks need either a fixed static field (`twm_suggest`) or
      language-aware naming (`spellcheck_<lang>`). Value formatting is uniform —
      `addIndexField` normalises any `solr_text_*` to `'text'` before its switch
      (`2706-2708`), and `solr_date_range` is the lone extra branch, building
      `[$start TO $end]` (`2764-2768`).
