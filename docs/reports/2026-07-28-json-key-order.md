# Report: JSON object key order (issue #25)

- Branch: `25-json-key-order` (worktree off `main`)
- Issue: #25 — "v1: JSON object key order is alphabetised, not Solr's — breaks `json.nl=map` ordering"
- Spec: orchestrator task spec (scratchpad, not committed)
- Pipeline: `test-writer` → `implementor` → `reviewer` (1 round, approved) → `reporter`
- State at report time: **uncommitted working tree** (7 modified files, 5 untracked). Per the
  compatibility contract's untracked-fixture warning, commit the three new fixtures before anything
  re-runs `capture.sh`.

## The bug

`serde_json` was declared without the `preserve_order` feature, so `serde_json::Map` was a `BTreeMap`
and every object Wayfinder emitted came out alphabetised. Under `json.nl=map` that is semantically
load-bearing: `facet_ranges.counts` for a 0–200 range by 10 emitted `"0","10","100","110",…` instead
of `"0","10","20",…`.

The harness could not see it. `assert_matches_fixture` and `tests/common/diff.rs` compare parsed
`serde_json::Value`s, and parsing discards key order — the mirror image of the `error.msg` blind spot
in PRD section 8.

## Investigation (the point of the issue)

Established against a live `solr:9` on an isolated container, plus order-preserving reads of the
committed fixtures, **before** any implementation:

| Object | Solr's order |
|---|---|
| top level | `responseHeader, response[, facet_counts]`; error: `responseHeader, error`; bare error: `error`; ping: `responseHeader, status` |
| `responseHeader` | `status, QTime, params` (ping additionally leads with `zkConnected`) |
| `response` | `numFound, start, numFoundExact, docs` |
| `error` | `metadata, msg, code` |
| `facet_counts` | `facet_queries, facet_fields, facet_ranges, facet_intervals, facet_heatmaps` |
| `facet_ranges.<field>` | `counts, gap, start, end` |
| `facet_fields.<field>` (`json.nl=map`) | count-descending with an index tie-break by default; term order under `facet.sort=index` |
| `facet_ranges.<field>.counts` (`json.nl=map`) | ascending numeric bucket order |
| doc fields | index-time input order |
| `responseHeader.params` | Java `HashMap` iteration order — not reproducible |

Doc-field order was proven by probe: posting `{"category":…,"body":…,"id":…}` returned
`category, body, id`. It is **not** `fl` order (`fl=category,id,body` still returned
`id, body, category`) and not schema order. Wayfinder's `render_doc` emits schema order, which
coincides with input order for every committed corpus, because every corpus was indexed in schema
order.

`responseHeader.params` is neither request order nor alphabetical and cannot be reproduced by any
implementation, so it is permanently exempt from key-order comparison — consistent with findings
fact 6, which already required the differential normaliser to be order-insensitive there.

Almost none of those orders is alphabetical, so **the alphabetisation was wrong throughout the
envelope**, not only under `json.nl=map`.

## Design decision and blast radius

Chose **global `serde_json` `preserve_order`** over ordering only where order is load-bearing: the
investigation showed order is meaningful at nearly every construction site, so the targeted
alternative would mean hand-rolling insertion-ordered maps almost everywhere.

Blast radius turned out to be serialisation only, and both halves were verified rather than assumed:

- **No `src/` change was needed at all.** `git diff main -- src/` is empty. Every construction site
  already listed keys in Solr's order — the `json!` literals in `src/lib.rs` and `src/error.rs`,
  `Map::new()`-plus-insertion in `src/facet.rs`, `render_doc`'s schema-order iteration in
  `src/core_index.rs`. The feature flag alone made all 13 new tests pass on the first run.
- **`Value` equality stays order-insensitive** under `preserve_order` (`IndexMap`'s `PartialEq`
  compares as a map). Verified by a probe asserting both that `Map::keys()` reports document order
  *and* that two `Value`s differing only in key order compare equal. So `assert_matches_fixture`,
  `tests/common/diff.rs` and every existing `assert_eq!(Value, Value)` keep their exact prior
  meaning; `diff.rs` additionally sorts and dedups its merged key set, so it is order-independent by
  construction.
- `src/core_index.rs` funnels dynamic fields through a `BTreeMap<String, OwnedValue>` into Tantivy,
  so dynamic-field read-back order stays sorted, unaffected by the feature.
- One real behaviour change with no contract attached: Wayfinder's own `responseHeader.params` echo
  went from alphabetical to request order. Invisible to every assertion, and neither order matches
  Solr's `HashMap` order.

## Fixtures captured

Container isolation was mandatory: issue #24 was capturing against `wayfinder-solr-ref` on 8983
concurrently, and the canonical `solr-ref/capture.sh` rebuilds that container destructively. **The
canonical script was never run.** An isolated block was appended to the end of `capture.sh` using its
own container `wayfinder-solr-25`, its own port 8986, and its own `keyorder` core — the same precedent
as `wayfinder-solr-ref-strict` on 8984 and the `schemaless_probe` core — and only that block was
executed. `solr-ref/responses/` and both manifests were backed up outside the repo first.
`git diff main --name-status -- solr-ref/responses/` is empty: no existing fixture was re-captured.

A new `keyorder` core rather than the existing `facets` core, deliberately: every `facets`-core
fixture is a `q=*:*&rows=0` capture, so adding docs there to widen the range would have moved
`numFound` in all of them — re-capturing ground truth as a side effect.

Three new fixtures, indexed in `manifest-errors.tsv` (not `manifest.tsv`: another core on another
`host:port`, and the differential harness GETs `manifest.tsv` rows only):

| Fixture | What it establishes |
|---|---|
| `keyorder_range_wide_map.json` | `facet.range=views` 0–200 by 10, `json.nl=map`. Buckets `0,10,20,…,190`; alphabetical puts `100,110,…` before `20`. The decisive fixture. |
| `keyorder_facet_field_map.json` | `facet.field=tag&json.nl=map` → `apple 5, zebra 5, mango 2, banana 1` vs alphabetical `apple, banana, mango, zebra`. |
| `keyorder_facet_field_map_index.json` | same with `facet.sort=index` → `apple, banana, mango, zebra`. |

## Test evidence

New `tests/json_key_order.rs` (13 tests) and `tests/common/key_order.rs`.

The helper recovers key order from the **document bytes**, via a hand-written `Deserialize` driving
`MapAccess`/`SeqAccess` — deliberately *not* by parsing into `Value` and reading `.keys()`, which
would depend on the very feature under test and could pass for the wrong reason. Exemptions are exact
full-path matches: `responseHeader.params` (unreproducible `HashMap` order) and `_version_` / `_root_`
(Wayfinder deliberately omits them, findings fact 9). A canary test fails if `preserve_order` is ever
dropped.

**Red first:** 8 of the 13 were confirmed red before the fix, each an order mismatch.
`facet_field_json_nl_map_index_order_matches_solr` was green from the start, because `facet.sort=index`
order happens to be alphabetical; it exists to pin that fixing the count case does not break index
order.

**Gate, final:**

```
cargo fmt --check                            clean
cargo clippy --all-targets -- -D warnings    clean
cargo test                                   168 passed, 0 failed (11 suites; was 155 on main)
```

Docker was available; only the isolated container on 8986 was used.

## Mutation evidence

The guard is code whose whole value is failing correctly, so it was mutation-tested — twice,
independently.

By the implementor: dropping `preserve_order` turned exactly the 8 tests red; swapping
`numFound`/`start` in `src/lib.rs` failed 2 key-order tests while all 155 pre-existing tests still
passed — the blind spot demonstrated rather than argued.

By the reviewer, repeated on a full copy of the tree (never writing to the worktree; `diff -r` clean
afterwards, copy deleted), across five mutations:

| Mutation | Caught by |
|---|---|
| `numFound`/`start` swap | 2 failures at `response` |
| `msg`/`code` swap | 1 failure at `error` |
| `render_buckets` bucket sort (the exact bug the issue describes) | 2 failures at `facet_counts.facet_ranges.views.counts` and `facet_counts.facet_fields.tag` |
| reverse bucket sort | the index-order test failed — proving it is not tautological |
| reverse `render_doc`'s field list | 1 failure at `response.docs[0]` |

Vacuity probes confirmed the helper panics on a scalar body, an empty object, and a misspelled fixture
name.

## Review outcome

**Approved.** One round, no bounce, no must-fix items and no 5-minute items. The 2-round cap was not
hit.

Because only a single review round was spent, **this work could use more review passes** — the
follow-ups below are the items a second pair of eyes would most usefully take.

## Follow-ups (all deferred, none fixed)

1. `tests/common/key_order.rs` — `IGNORED_KEYS` (`_version_` / `_root_`) is matched by key name at any
   depth, not path-scoped. Harmless today (Wayfinder emits neither anywhere), but broader than the
   stated intent; scope to `response.docs[*]` if it ever matters.
2. `tests/json_key_order.rs` — the three query strings are hand-copied from `manifest-errors.tsv`,
   tied together only by a comment. Editing the manifest would silently desynchronise the test from
   its ground truth; parsing the row and asserting equality would close it.
3. `docs/solr-ref-findings.md` finding 26 discusses `params` order without mentioning that this change
   moved Wayfinder's own echo from alphabetical to request order. One sentence completes it.
4. No fixture returns dynamic fields inside `response.docs`, so dynamic-field doc key order is
   **uncovered**. Wayfinder appends them `BTreeMap`-sorted after the static fields; Solr would use
   input order. Expect a divergence if a future capture returns dynamic fields in docs.
5. `assert_same_key_order` skips short/empty arrays and one-sided objects by design, so it must always
   be paired with a value-level assertion. Both current whole-envelope tests check HTTP status, but
   neither asserts `docs` is non-empty — a future caller could get a green from an empty `docs`.
6. Finding 24's "doc field order is not `fl` order" half rests on the live probe, not on a committed
   fixture: the only multi-field `fl` capture (`select_term`, `fl=id,body`) cannot discriminate,
   because its fields are in input order anyway. Pinning it needs a fixture with `fl` in reversed
   order.

## Pointers

- New: `tests/json_key_order.rs`, `tests/common/key_order.rs`,
  `solr-ref/responses/keyorder_{range_wide_map,facet_field_map,facet_field_map_index}.json`
- Changed: `Cargo.toml`, `Cargo.lock`, `tests/common/mod.rs` (one line: `pub mod key_order;`),
  `solr-ref/capture.sh` (appended block only), `solr-ref/manifest-errors.tsv`, `docs/PRD.md`
  section 8, `docs/solr-ref-findings.md` (findings 21–26 appended, nothing renumbered)
- Untouched: **all of `src/`** — the fix needed no production code. `src/facet.rs` and
  `tests/faceting.rs` are byte-identical to `main`; issue #24 owns those.
