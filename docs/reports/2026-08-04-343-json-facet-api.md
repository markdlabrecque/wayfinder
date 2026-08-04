# #343 — `json.facet` (JSON Facet API)

**Date:** 2026-08-04. **Branch:** `markdlabrecque/issue-343-json-facet-api`.
**Spec:** `spec-343.md` + `spec-343-addendum.md` (orchestrator-authored task specs, not
committed to the repo). Continuation of #293's decision (PRD §5, v3 `_version_`): the
`_version_` field itself was delivered there; this issue is the real client dependency
that reads it — Solarium's JSON Facet API, evidenced only on the admin-diagnostics
`doGetMaxDocumentVersions()` path.

## Two corrected ticket premises

`gh issue view 343` and CLAUDE.md's "don't paper over a wrong ticket premise" rule make
these the deliverable, not a footnote.

**1. The wire form of the aggregation is not what the ticket says.** The ticket text
(and #293's own report) described the aggregation as `function: 'max(_version_)'`. That
is the *PHP option name* passed into Solarium's `createJsonFacetAggregation(['local_key'
=> 'maxVersion', 'function' => 'max(_version_)'])` call — not the wire form. Solarium's
`JsonAggregation::serialize()` returns `$this->getFunction()`, a **bare string**, so what
actually reaches Solr is:

```json
{"maxVersion":"max(_version_)"}
```

Never `{"function":...}`, never `{"type":"func","func":...}`. A parser built to the
ticket's literal wording — expecting an object with a `function` key — would have failed
against the real client on its very first request. This is finding 177, verified against
the frozen `SearchApiSolrBackend.php` 4.4.0 source and the 20 committed `jf343_*`
fixtures captured from real `solr:9`.

**2. `tests/version_descope_guard.rs` does not self-delete when `json.facet` lands.**
The ticket instructed: "delete `tests/version_descope_guard.rs`, it is a self-deleting
guard that names itself for removal when this lands." Checked directly: its
`REQUEST_NEEDLES` scan (`_version_`, `versions=true`, `max(_version_)`) runs only against
the 28 captured client *traces'* request side, and json.facet landing server-side does
not change those traces at all — the client never sent those needles request-side to
begin with; it is the *response* side that would eventually carry `json.facet`. The
source-channel checks (`source_never_requests_versions_true_for_optimistic_concurrency`,
`source_writes_whole_documents_not_atomic_updates`) guard the *write*-side descopes
(atomic updates, `versions=true`), which #343 does not touch at all. Deleting the file
outright would have silently dropped that write-side coverage.

The resolution — narrowing rather than deleting — was the user's decision. It landed as:
- Renamed `tests/version_descope_guard.rs` → `tests/version_write_descope_guard.rs`.
- Removed `"json.facet"` from `REQUEST_NEEDLES` (now `["_version_", "versions=true",
  "max(_version_)"]`) and dropped the json-facet deferral framing.
- Kept the two write-side source checks, the trace scan, and its positive control
  (`version_is_present_in_trace_responses_so_the_request_scan_is_not_blind`).
- Kept and updated the PRD tripwires, and added a new one,
  `prd_version_section_records_the_json_facet_read_path_as_landed_not_deferred`, so the
  narrowing cannot rot back into a PRD that describes a descope Wayfinder no longer has.
  A five-minute review finding (below) hardened this new tripwire further.

## Ratified divergences (finding 178, spec §1c)

Two fixtures record deliberate Wayfinder-vs-Solr mismatches, both `400` where Solr `200`s:

- **`jf343_err_no_docvalues`** — a `type: terms` facet naming a field with no docValues
  (an indexed-and-stored `text_en` field). Solr returns `200` with `{"buckets":[]}`. Same
  divergence family as finding 105's classic-facet behaviour (`facet_non_docvalues_text`).
- **`jf343_err_agg_text`** — `max(body)` over a text field. Solr returns the
  **lexicographic** maximum term as a string (`{"x":"zeta"}`). No captured client ever
  aggregates over text.

The reasoning for diverging rather than matching: silently ignoring an unfacetable field,
or handing the client a string where it expects a number, produces a response that is
*wrong but looks structurally fine* — worse than a loud 400 the client can act on. Both
are recorded in `ACCEPTED_DIVERGENCES` in `tests/differential.rs` with a check arm that
asserts the fixture is still the `200` shape it names and fails the moment Wayfinder's
own status starts matching it (self-expiring, per CLAUDE.md's skip-list rule).

## What was built

- `src/json_facet.rs` (new, 914 lines): `pub fn json_facets(index, params, base) ->
  Result<Option<Value>>`, self-gating on `json.facet`'s presence via `params.get`. Parses
  the JSON object (arbitrary nesting), validates against an allowlisted `type: terms`
  shape plus `max(<field>)` aggregations, reuses `facet::base_query`/`facet::narrowed`
  (no second base-query build — counts track `q`+`fq`), and renders the `facets` block:
  implicit `count`, terms buckets (`count desc` default, `mincount 1`, `limit -1`
  unlimited, `sort: index asc` variant), sub-facets inline in each bucket as siblings of
  `val`/`count`, and bare-scalar aggregations rendered as the field's native integer type
  (not routed through `stats.rs`'s float path — finding 177's whole point).
- `src/core_index.rs` / `src/facet.rs`: extended `terms_aggregation`'s previously-always-
  empty `sub_aggregation` to support nesting, and resolved `_version_` as an aggregation
  column through a path aware of `schema::VERSION_FIELD` (mirroring `stats::
  check_statable`) without making it facetable or sortable — `tests/version_field.rs`
  still asserts both 400.
- `src/lib.rs`: `"json.facet"` registered in `SELECT_PARAMS`; `body["facets"]` inserted
  after `facet_counts`, before `stats` (finding 175's wire order); the error split
  matches `PreQueryFacetError`'s existing shape (parse-time failures omit `response`,
  field-resolution failures keep it).
- Out-of-scope inputs (`type: query`/`range`, aggregation functions other than `max`,
  the `{"type":"func","func":...}` object form, and the ten unevidenced per-facet
  settings — `domain`, `offset`, `numBuckets`, `allBuckets`, `missing`, `prefix`,
  `method`, `refine`, `overrequest`, `excludeTags`) all 400 by name via a strict
  allowlist (`TERMS_KEYS`), each with a `ponytail:` comment naming the ceiling, rather
  than being silently accepted and ignored.
- Tests: `tests/json_facet.rs` (1005 lines) and `tests/json_key_order.rs` (187 lines,
  pinning the `facets` slot and sub-object order), both committed red in `531142c` and
  never touched again — confirmed directly from git log (`git log --oneline -- tests/
  json_facet.rs tests/json_key_order.rs` shows exactly one commit). The differential
  harness was wired for a new `jsonfacet343` hermetic core (schema, corpus, app, routing
  arm) per the addendum, mirroring the `g338n_` precedent.
- Docs: `docs/solr-ref-findings.md` findings 175-178 (renumbered from an initial 165-168
  in `5d08798` after a merge collision with #340's own finding range); `docs/PRD.md` §5
  v3 `_version_` rewritten to record the dependency as landed rather than deferred, and
  the parity-table JSON Facet API row struck through and marked shipped.

## Rebase and merge collisions

The branch was rebased onto `origin/main` after #340 (`{!payload_score}`) and #342
(language-aware text field naming / spellcheck) landed there. All conflicts were
append-at-end collisions in the files CLAUDE.md names as hot for exactly this reason —
`solr-ref/capture.sh`, `solr-ref/manifest-errors.tsv`, `docs/solr-ref-findings.md`, and
`tests/differential.rs` — resolved by keeping both sides' appended blocks, with the
finding numbers renumbered (165-168 → 175-178) to sit after #340's own findings. Reflog
confirms a real `rebase` (`HEAD@{1}` through `HEAD@{8}`, `rebase (finish)` back onto this
branch), not a merge. Gates were re-run clean after the rebase (below).

## Test evidence

Re-run directly, not restated from an earlier report:

```
$ cargo test
cargo test: 1321 passed (65 suites, 95.17s)

$ cargo fmt --check
(clean, exit 0)

$ cargo clippy --all-targets -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.45s
(clean, exit 0)
```

### The 20 `jf343_*` differential rows

Ran `cargo test --test differential manifest_errors_every_row_runs_against_the_matching_hermetic_app -- --nocapture`
directly and read the per-row diff output (not the pass/fail summary):

- **16 rows diff at 0**: `jf343_agg_max`, `jf343_terms`, `jf343_terms_limit`,
  `jf343_terms_nested`, `jf343_deep_max`, `jf343_terms_fq`, `jf343_terms_q`,
  `jf343_with_classic`, `jf343_terms_mincount0`, `jf343_err_bad_json`,
  `jf343_err_bad_type`, `jf343_err_bad_func`, `jf343_err_unknown_field`,
  `jf343_with_classic_stats`, `jf343_empty_object`, `jf343_terms_sort_index`.
- **2 rows are `ACCEPTED_DIVERGENCES`** (short-circuited before the differ runs, per
  spec §1c above): `jf343_err_no_docvalues`, `jf343_err_agg_text`.
- **2 rows are `EXPECTED_DIVERGENCES_MANIFEST_ERRORS`**, and the claim that only opaque
  `_version_` *values* go unchecked holds — verified from the actual diff output, not
  asserted:
  - `jf343_agg_max_version` — exactly **1** diff:
    `facets.maxVersion` (`expected: "1872604773983715328"`, `actual: "1785865083462"`).
  - `jf343_deep_version` — exactly **5** diffs: `facets.maxVersion` plus the four
    `facets.siteHashes.buckets[*].indexes.buckets[*].dataSources.buckets[*].
    maxVersionPerDataSource` leaves. Every other field in both rows — status, the
    `facets` key set and slot, bucket nesting/ordering/counts, and each aggregation's
    integer (not float) rendering — is diffed for real and matches.

  Both are Solr's update-log-derived `_version_` magnitude vs. Wayfinder's epoch-millis
  seed; `tests/json_facet.rs` pins the envelope and checks each leaf against the index's
  real fast-field maximum instead.

16 + 2 + 2 = 20, matching the fixture count and the spec's accounting exactly.

## Review outcome

Stage 3 (Opus) **approved in round 1** — no second round needed. It verified empirically
rather than by reading the diff:
- Ran the differential binary directly to confirm sub-aggregation scoping is correct
  against Solr's own nested output at all four topology levels (`jf343_deep_max`).
- Confirmed `TERMS_KEYS` is a strict allowlist, so all ten unevidenced settings and any
  unknown key 400 by name rather than being silently accepted.
- Confirmed from git log that the implementor touched no assertion file.

It raised two five-minute items, both fixed in a follow-up commit (`4de98a7`):
1. A `type`-less `json.facet` member 400ed with "only `type: terms` is supported" —
   misdescribing the problem for a caller who did want terms (Solr defaults a type-less
   object to `terms`; the client always sends `type` explicitly, so there is no fixture
   for the omitted form and no evidenced default to infer). Fixed to name the missing key
   instead of claiming terms is unsupported, with a `ponytail:` comment.
2. The new anti-deferral tripwire (`prd_version_section_records_the_json_facet_read_path
   _as_landed_not_deferred`) matched only the literal spellings `json facet`/`json.facet`
   against `deferred`/`not v1 work` — so `json-facet` escaped it, and the PRD's own Guard
   paragraph passed it by one letter (`deferral` vs. `deferred`). Hardened to strip
   punctuation before matching and to match the `defer` stem; mutation-tested against
   both escape strings (`**The JSON-facet read path remains deferred.**` and `**The real
   dependency, kept in deferral.**`), both now caught, both previously passed.

**Note on what stage 3 was reviewing unreviewed:** `tests/version_write_descope_guard.rs`
and `docs/PRD.md` were authored by the orchestrator, not the stage-1 test-writer — they
are documentation/guard files, not the pipeline's protected assertion files. Stage 3
reviewed them on that explicit basis. The pipeline's "the implementor edited no test"
property stays checkable from git only for the two files that matter for it —
`tests/json_facet.rs` and `tests/json_key_order.rs` — both confirmed above to have a
single commit each in their git history.

## Follow-ups

Reviewer-surfaced; assessed here rather than transcribed verbatim.

- **Duplicated count query on every `json.facet` request** — `json_facets` calls
  `index.count(&base_query)` for the implicit `count`, a second full count query over
  clauses the main `select` handler has already counted for `numFound`. Correct today,
  but wasted work on every request that uses `json.facet`. Worth a small follow-up issue
  if `json.facet` sees real traffic; not urgent given its only known client is an admin
  diagnostics screen.
- **Repeated `json.facet` params are unevidenced and unnamed as a ceiling.** Solr merges
  repeated instances of the same param; Wayfinder's `json_facets` calls `params.get
  ("json.facet")` (single-value), so a repeated `json.facet` silently uses whichever
  value `Params::get` happens to return. No captured client ever repeats it. Worth a
  one-line `ponytail:` comment at the `params.get` call site naming this as a ceiling,
  even without a fixture — cheap to add, and the current absence of any comment there
  is a minor gap relative to every other named ceiling in the module.
  Not escalated to its own issue; the module's existing pattern of naming every other
  refusal makes this look like an oversight worth a quick note, not new scope.
- **`PreQueryFacetError::wrap` is `pub` where its neighbours are `pub(crate)`.**
  Confirmed: `pub struct PreQueryFacetError` (line 70) and `pub fn wrap` (line 88) in
  `src/facet.rs`, versus `pub(crate) fn from_params`, `narrowed`, `base_query`,
  `render_named_list`, `check_facetable` alongside them. `json_facet.rs` is in the same
  crate, so `pub(crate)` would work identically; this looks like an unintentional
  visibility widening rather than a deliberate public API surface (Wayfinder is a
  binary, not a published library). Worth tightening in a small follow-up, not urgent.
- **`limit` below `-1` is treated as unlimited, matching Solr, but undocumented as an
  equivalence.** The `ponytail:` comment at the truncate site documents the `-1`-is-
  unlimited ceiling (bounded by `MAX_FACET_TERMS`) but doesn't call out that anything
  `< -1` (e.g. `-5`) is treated the same as `-1` rather than rejected. No fixture
  exercises a negative-non-`-1` limit — Solr's own behaviour here is unverified against
  real `solr:9`. Worth a one-line comment addition; not worth a fixture-capture round
  trip on its own.
- **`REQUEST_NEEDLES` is nominally three entries but effectively two.** `"max(_version_)"`
  is a strict substring of `"_version_"`, so the third needle can never fire on its own —
  any request matching it already matched the first. The doc comment above the constant
  implies three independent checks. Harmless (the guard still catches everything it's
  supposed to), but worth a one-line comment noting the redundancy so a future reader
  doesn't assume three independent code paths are covered.

## Commands run

```
cargo test                                     # 1321 passed, 65 suites
cargo fmt --check                              # clean
cargo clippy --all-targets -- -D warnings      # clean
cargo test --test differential \
  manifest_errors_every_row_runs_against_the_matching_hermetic_app -- --nocapture
                                                # 1 passed; per-row diff output verified by hand
```
