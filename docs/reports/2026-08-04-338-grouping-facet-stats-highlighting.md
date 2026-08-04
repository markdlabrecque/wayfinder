# #338 — grouping + facet/stats/highlighting, real `group.truncate`/`group.facet`

**Date:** 2026-08-04. **Branch:** `markdlabrecque/issue-338-facet-stats-highlighting`.
**Spec:** the task spec handed into this pipeline, and findings **160-164** in
`docs/solr-ref-findings.md` — the grouped-envelope shape and both flags' true semantics are
derived entirely from the 31 captured `g338*` fixtures, never from what the implementation
produces.

A `group=true` response now carries `facet_counts`, `stats` and `highlighting` alongside
`grouped`, exactly as `group=true&facet=true` fixtures show (#290 shipped `grouped` alone).
`group.truncate=true` and `group.facet=true`, previously accepted-but-inert params kept only
for `strict_params` parity, are now real.

## Scope corrections (ticket premises the fixtures refuted)

The ticket got the envelope-alongside-`grouped` premise right, but was wrong about the scope
of both flags — this is the kind of divergence CLAUDE.md says to flag, not paper over:

- **`group.truncate=true`** does not affect facets alone. It recomputes **`stats`**,
  **`facet.query`** and **`facet.range`** over the collapsed group set too (finding 161):
  over the full 6-doc match set `popularity` stats are count 6/min 5/max 40/sum 120; collapsed,
  count 3/min 10/max 20/sum 45 — exactly `{g1:10, g2:20, g6:15}`.
- **`group.facet=true`** is not field-facet-only (the ticket and Solr's own docs both say
  this). It also regroups **`facet.query`** and **`facet.range`** counts into distinct-group
  counts (finding 162: `facet.query=category:blog` 2→1, `facet.range` popularity buckets
  `[0:4,25:2]`→`[0:3,25:2]`). It does **not** touch `stats` — `group.truncate` moves stats,
  `group.facet` does not, and the two are genuinely independent effects, not the same
  mechanism applied twice.

Four things neither the ticket nor Solr's documentation settle, answered by the capture:

- Two `group.field` values collapse/regroup on the **first** one only (`g338_truncate_multi`,
  `g338_groupfacet_multi`).
- Both flags are **paging-independent**: `rows=1` still facets/counts over every group, not
  just the page returned (`g338_truncate_rows`, `g338_groupfacet_rows`).
- Truncate's "most relevant document" of a group is whichever `group.sort` picks, not the
  main `sort` — `group.sort=id desc` moves the collapsed set from `{g1,g2,g6}` to `{g4,g5,g6}`
  (`g338_truncate_groupsort`, finding 161).
- `{!ex=...}` composes with `group.facet` by counting groups over the **excluded facet's own
  reduced base**, which is a *superset* of what the grouping pass bucketed — this is finding
  164 and also the round-1 review defect below.

## Implementation

`src/lib.rs`'s early return at the old `group=true` branch became a fall-through: the grouped
branch still runs before the ungrouped top-N search (a grouped request never materialises the
hit list it would discard), but everything after it — `facet_counts`, `stats`, `highlighting`,
`spellcheck`, the response header — is now shared code, not duplicated, with `grouped`
occupying the key slot `response` occupies on the ungrouped path.

**`group.truncate`** (`src/grouping.rs`): `crate::collector::GroupingFruit` already holds every
matching doc of every group in `group.sort` order, so the collapsed set is `groups[i].docs[0]`
for each group of the *first* `group.field` — no second search pass. It's fed to facets/stats
as a `DocSetQuery`, a hand-written `Query`/`Weight`/`Scorer` keyed by `SegmentId` (not raw
`DocAddress`/`(segment_ord, doc_id)` pairs, which would be vulnerable to a reader reload
shifting an ordinal onto a different segment — see the round-1 defect below). It's built inside
`grouping()` from the segment list returned by the *same* searcher that ran the collector, and
appended to `BaseClauses` after the `fq` clauses, so #295's positional `{!tag}`/`{!ex}`
alignment is untouched (a tagless clause, so no `{!ex=...}` can drop it).

**`group.facet`**: field facets, `facet.query` and `facet.range` all count *distinct group
values* rather than documents. `CoreIndex::term_facet_grouped` is a group-column
sub-aggregation per bucket (`min_doc_count: 1` is load-bearing — 0 would surface the
term-dictionary's zero-count fill as a spurious extra group); `CoreIndex::distinct_group_count`
answers `facet.query`/`facet.range`/`facet.missing` the same way. Both are followed by a
`MustNot ExistsQuery` pass that adds the null-group bucket back, because a document missing the
group field produces no group term for a terms aggregation to see (finding 163).

## Review — round 1 (must-fix) and round 2 (approved)

Reviewer (Opus, read-only) ran two rounds; round 1 found two real defects, both fixed at the
root rather than patched:

1. **`{!ex=...}` + `group.facet` undercounted.** The original implementation built a
   doc→group map from the grouping pass's fruit, on the assumption that every document
   matching a facet's query also matches the base query. False for an excluded facet: `src/
   facet.rs` hands it a reduced base with the tagged `fq` clauses removed, a superset of what
   the grouping pass bucketed, so documents the pass never saw were silently absent from the
   map. Concrete repro: `fq={!tag=t}category:news` + `facet.query={!ex=t}category:blog` +
   `group.facet=true` returned 0 where Solr returns 1 (fixture `g338_ex_groupfacet`, finding
   164, captured before the fix landed). The fix deletes the doc→group map outright —
   `distinct_group_count`/`term_facet_grouped` are terms aggregations over each facet's own
   base query, so an excluded facet's wider base is answered correctly by construction.
2. **Stale `DocAddress` resolution across reader generations.** `search_grouping`,
   `segment_ids()` and `doc_set()` each independently called `self.reader.searcher()` under
   `ReloadPolicy::Manual`; a commit reloading the reader mid-request could shift a
   `segment_ord` onto a different segment between calls. Fixed by having `search_grouping`
   return the `SegmentId` list of the *same* searcher that collected the fruit, and having
   `GroupedOutcome` carry the truncate set as an already-built `DocSetQuery` rather than raw
   addresses — nothing resolves a `DocAddress` against a segment list from a second searcher
   call any more. `CoreIndex::doc_set`/`segment_ids` (now unused) were removed rather than left
   dead.

Also flagged, and handled honestly rather than accepted: the implementor self-reported that the
null-group `+1` correction (finding 163) survived a deliberate mutation test on the #290 `g1..
g6` corpus, but only because that corpus's one null-group document (g6) carries no facetable
`category` value at all — the mutant was invisible on that corpus, not actually caught. Rather
than accept the green result, a purpose-built corpus (`g338null`, own core, finding 163: h4/h5
have no `type` but do have `category=news`) was captured, and the mutant is now killed by two
tests (`g338n_facet` vs `g338n_groupfacet`: `news` is 4 documents but 3 groups).

Round 2: reviewer re-ran all three gates independently and approved.

## Evidence

Verified independently on the rebased tree (not taken from a stage handoff):

- `cargo test`: **1261 passed / 0 failed**, 63 suites.
- `cargo fmt --check`: clean.
- `cargo clippy --all-targets -- -D warnings`: clean.
- 31 new fixtures under `solr-ref/responses/g338*.json`, 31 rows in
  `solr-ref/manifest-errors.tsv`, findings 160-164 in `docs/solr-ref-findings.md`.
- The #290 parity guard test (`group.truncate`/`group.facet` accepted-but-inert) was
  **replaced** with real-semantics tests, per the spec's instruction not to leave a weakened
  version — checked directly against `tests/grouping.rs`, it is gone and the fixture-backed
  assertions are in its place.
- No entry was added to `EXPECTED_DIVERGENCES`/`ACCEPTED_DIVERGENCES` to absorb a diff; every
  `g338*` row matches captured Solr.

## Remaining ceilings and follow-ups (file as their own issues)

- A grouped response with an invalid `facet`/`stats`/`hl` param omits the `response`/`grouped`
  block from the error envelope entirely, rather than attaching one the way the ungrouped path
  attaches `response` to a facet/stats/hl 400 (issue #35's precedent). No fixture captures a
  grouped-plus-invalid-component request; marked `ponytail:` in `src/lib.rs` naming the ceiling.
- The fall-through makes `wt=xml`, `debug=true` and `spellcheck` reachable on a grouped response
  for the first time — previously unreachable behind the early return. The right direction, but
  entirely unfixtured; no `g338*` capture exercises any of the three together with `group=true`.
- `facet.heatmap` (#334, landed on `main` mid-flight and rebased in) is not group-aware: it
  takes `base`, so `group.truncate`'s collapsed-set restriction reaches it incidentally, but
  `group.facet` does not regroup heatmap cell counts into distinct-group counts. Unfixtured
  either way — no `g338`+`facet.heatmap` fixture exists.
- Per-bucket cost: `distinct_group_count` runs once per `facet.range` bucket, and each call is
  two searcher passes (the terms aggregation plus the null-group `ExistsQuery` pass) where the
  ungrouped path answers the same bucket with one cheap `count`. Fine at this corpus's scale;
  worth measuring before `group.facet` meets a wide `facet.range`.
- A pre-existing hazard, not a #338 defect but adjacent to the class the round-1 `DocSetQuery`
  fix closed: `render_doc` (`src/core_index.rs`) still re-resolves grouping-pass `DocAddress`es
  against a fresh `self.reader.searcher()` call rather than a pinned segment list. The
  ungrouped `/select` path has the same shape. Left alone here — fixing it is a bigger, separate
  change than #338's scope, and it deserves its own issue and its own reproduction.
- No coverage for `group.field` on a dynamic field. The null-group correction depends on
  `ExistsQuery` resolving the dotted `<json_field>.<name>` column; spot-checked working in
  tantivy 0.26.1, so this is a coverage gap, not a known bug.
- `MAX_FACET_TERMS = 65000`: Tantivy's terms aggregation checks `bucket_count > limit` only
  *after* `size` has already trimmed the result, so a bucket set wider than the limit silently
  reports exactly 65000 rather than surfacing an error. Pre-existing, not introduced here, but
  the `group.facet` sub-aggregations share the same limit and the same silent-truncation shape.

## Rebase friction, for whoever hits it next

- The branch name (`markdlabrecque/issue-338-facet-stats-highlighting`) diverges from the
  repo's `<issue>-<short-slug>` convention (`CLAUDE.md`) — it should have been
  `338-facet-stats-highlighting`.
- #334 (`facet.heatmap`) landed on `main` while this branch was in flight and claimed finding
  number 159, colliding with this branch's own 159. Resolved by renumbering this branch's
  findings to 160-164 and updating every citation in `src/` and `tests/` (commit `39f75e2`).
  The same rebase collided on the append-only files `solr-ref/capture.sh`,
  `solr-ref/manifest-errors.tsv` and `docs/solr-ref-findings.md` — resolved by rebuilding those
  three as `origin/main`'s version plus this branch's appended tail, per the repo's append-only
  convention for those files.
