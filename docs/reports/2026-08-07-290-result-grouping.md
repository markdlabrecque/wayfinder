# #290 — result grouping (`group=true`, `group.field`)

**Date:** 2026-08-07. **Branch:** `markdlabrecque/issue-290-result-grouping-group-true`.
**Spec:** finding **130** in `docs/solr-ref-findings.md` (what `setGrouping()` sends) and the
14 captured `group_*` fixtures in `solr-ref/responses/` — the grouped response envelope is
derived entirely from them, never from the implementation.

`group=true` + `group.field` buckets every matching document by a single-valued,
non-text field and returns a `grouped: {<field>: {matches, ngroups, groups: [{groupValue,
doclist}]}}` envelope *instead of* the normal `response` doclist. This is the shape
`search_api_grouping` consumes (`$response['grouped'][<field>]['groups']`, each group's
`['doclist']['docs']`, and `['ngroups']`).

## Scope

Finding 130 narrows the surface precisely:

- **Implemented & fixture-backed:** `group`, `group.field` (repeatable), `group.ngroups`,
  `group.limit`, `group.offset`, `group.sort`.
- **Accepted for `strict_params` parity, not yet fixture-backed:** `group.truncate`,
  `group.facet`. `setGrouping()` sends both (finding 130); their TRUE semantics — computing
  `facet_counts` over collapsed groups — only matter when `facet=true` is also set, and no
  fixture captures that interaction. With `facet` absent (the captured cases) both are a
  no-op, so accepting them changes nothing today. A ponytail + parity guard test record the
  ceiling (see *Follow-ups*).
- **Out of scope, deliberately rejected:** `group.format` and `group.main` are **never**
  sent (finding 130). They are absent from `SELECT_PARAMS`, so they **400 under
  `strict_params`** rather than being silently accepted as an unimplemented param — two
  tests pin this.

## Behaviour

- **Envelope shape** (`group_basic`): `{responseHeader, grouped: {<field>: {matches,
  ngroups, groups:[{groupValue, doclist:{numFound, start, maxScore?, numFoundExact,
  docs}}]}}}`. `grouped` replaces `response`; no `facet_counts`/`stats`/`highlighting`
  block (no `group_*` fixture combines grouping with another component).
- **`ngroups`** (`group_ngroups_off`): present only when `group.ngroups=true`; absent
  otherwise (not zero). `setGrouping()` sends it unconditionally.
- **`group.limit` / `group.offset`** (`group_limit`/`group_offset`): within-group paging of
  the `doclist.docs`. `group.limit` defaults to **1**.
- **`rows` / `start`** (`group_rows_start`): paginate the *groups* list, not the docs.
- **`group.sort`** (`group_sort`): orders docs WITHIN a group. Group *ordering* is always
  the main `sort` (of each group's top doc) — pinned by `group_sort`, whose group order is
  unchanged from `group_basic` even though within-group order is `id desc`.
- **Numeric field** (`group_numeric`): `groupValue` is the JSON number, not a string.
- **`fq`** (`group_fq`): filter queries compose into the main query (`q` AND every `fq`),
  so the grouped set is exactly the filtered match set.
- **`fl=score`** (`group_fl_score`): each `doclist` gains `maxScore`, the max across the
  whole group; per-doc `score` appears as in `/select`.
- **Zero matches** (`group_zero`): `matches:0`, `ngroups:0`, `groups:[]`.
- **Null group** (`group_basic`): a document missing the group field forms a
  `groupValue:null` group (Solr keeps it; it is not dropped).

## Validation (400s, mirroring Solr)

`src/grouping.rs::validate_group_field` rejects, with messages containing the field name:

- **undefined** (`group_err_unknown_field`): `undefined field: "nosuchfield"`.
- **multiValued** (`group_err_multivalued`): `can not use FieldCache on multivalued field:
  category`. A text field is not fast, so it falls through to the not-fast 400 — the
  module's "refuses to group on a fulltext field" rule (finding 130) is enforced by the
  schema, not a separate text check.
- **no `group.field`** (`group_err_no_field`): `Specify at least one field, function or
  query to group by.`

These are mutation-guarded: `grouping_multivalued_rejection_is_not_lossy` asserts a disabled
check cannot leak category values into the response, verified by deliberately disabling the
check (it failed both multivalued tests) and reverting.

## Implementation

Tantivy has no native grouping collector, so collection + bucketing is a new collector in
`src/collector.rs` (`GroupingCollector`) reusing the module's private sort machinery
(`SegmentSortColumn`, `compare_hits`, `Hit`, `SortValue`):

- Each segment collects one `GroupRecord` per matching doc — its address, score, group
  value, and sort keys under both the main `sort` and `group.sort`.
- `merge_fruits` sorts the whole match set by the main clauses (group rank = first-seen in
  main-sort order), buckets by group value, and sorts each bucket by `group.sort` (or
  reuses the main order when `group.sort` is absent — `within_is_main`).
- An **empty** `main`/`within` clause set becomes the implicit `score desc`, exactly as
  `AllScoredHits`/`TopScoredHits` already do — so an unsorted grouped request ranks groups
  by their top doc's relevance, not by document address.

`src/grouping.rs` owns the request half: parsing `group.*`, validating each field, running
the collector per `group.field`, and shaping the per-field envelope (applying
`group.limit`/`group.offset` and `rows`/`start`). `CoreIndex::search_grouping` composes
`fq` and dispatches, mirroring `search_top`.

`src/lib.rs`:

- `grouping::grouping(...)` branches in `select` **before** the ungrouped top-N search, so a
  grouped request never pays for the hits it would discard.
- `check_sort` was refactored to expose `parse_sort_spec(&schema, params, spec)` so `sort`
  and `group.sort` share one field-direction grammar (comma does not delimit the field
  token; direction checked before the field resolves; dynamic-only match sorts on its
  catch-all fast column — findings 18/34/35, issue #66).
- `SELECT_PARAMS` gains the six implemented `group.*` params plus `group.truncate`/
  `group.facet` (parity), but **not** `group.format`/`group.main`.

## Tests (TDD)

Red tests first (`tests/grouping.rs`, 23 tests) confirmed failing for the right reasons,
then implementation. Every fixture row is double-covered:

- `tests/grouping.rs`: hermetic coverage of every `group_*` fixture plus the
  `group.format`/`group.main` 400s, the `truncate`/`facet` parity guard, the relevance
  ordering correction, and the mutation guards.
- `tests/differential.rs`: the 14 `group_*` rows in `solr-ref/manifest-errors.tsv` run
  against a hermetic `grouping` core (duplicated schema/corpus, like the `facets33`/`sortdebt`
  precedents) and diff against the committed fixtures — the compatibility-evidence gate. The
  11 happy-path + 3 error fixtures all match captured Solr bit-for-bit.

Two test premises that the corpus refuted were **corrected**, not papered over (CLAUDE.md):
`group_multi_field` has 6 unique ids (asserted 5), and the relevance test's `q=lazy` ties
g1/g2 (both 3-token bodies, "lazy" once) so the doc-address tiebreak — not BM25 — orders
them; the query became `q=lazy garden` so g2 matches more terms and the score gap is real.

## Follow-ups / ceilings

- **`group.truncate` / `group.facet` true semantics** (ponytail in `SELECT_PARAMS` and the
  parity guard test): the group+facet interaction needs its own fixtures
  (`group.truncate=true`/`group.facet=true` alongside `facet=true`) before the
  collapsed-group facet computation lands in `src/grouping.rs`/`src/facet.rs`. Until then
  both default to a no-op (correct for every captured request).
- **Grouping + facet/stats/highlighting combination** is not fixture-backed; the grouped
  branch returns `grouped` alone. A request that sets both `group=true` and `facet=true`
  gets `grouped` without `facet_counts` today.
