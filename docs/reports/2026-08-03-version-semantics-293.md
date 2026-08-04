# #293 — `_version_` semantics: decide and record

**Date:** 2026-08-03. **Branch:** `markdlabrecque/implement-issue-293`.
**Spec:** finding **132** in `docs/solr-ref-findings.md` (PR #307's source sweep) — what
`search_api_solr` 4.4.0 actually does with `_version_` — read against
`coverage/search_api_solr_4.4.0_source/.../SearchApiSolrBackend.php`.

## Decision

This is a "decide and implement" issue, and finding 132 corrected its premise. Verified
directly against the frozen source:

- The client reads `_version_` **only** through a JSON facet aggregation — Solarium's
  `createJsonFacetAggregation` with `function: 'max(_version_)'` (PHP 4938-4940, 5052-5092),
  optionally nested under `terms` facets on `hash`, `index_id`, `ss_search_api_datasource`.
  These are the server-status "max document `_version_`" admin diagnostics screens. The PHP
  comment at 4934 says the field was picked only because it is "the only field we can be 99%
  sure exists in any index" — a cheap always-present probe for a facet, not a version.
- It **never** reads `_version_` via `stats.field`, **never** writes it, **never** sends
  `versions=true`, **never** uses atomic-update modifiers. The 28 captured search traces
  carry zero request-side `_version_`; the coverage contract has no `_version_` parameter.

So the v1 outcome is a **recorded no-op-with-decision** (the legitimate outcome the issue
body endorses): the `_version_` field work (#99/#102) is real, fast, populated per document,
and stats-only — exactly the shape JSON faceting needs when it lands — and nothing a site
*searches* depends on `_version_`. The real client dependency (**JSON faceting with
aggregation functions and nesting**: `json.facet`, `type: terms` nesting, `max()`) is a
separate, larger, deprioritized feature whose only client is an admin diagnostics screen;
it is tracked under JSON faceting, not here.

## The documented defect this fixes

PRD §5's v3 `_version_` section stated the client references `_version_` "exactly once,
read-only: `stats.field=_version_&function=max(_version_)`". That attributed the wrong read
path to the client. Finding 132 is the correction; this issue applies it. (Per the repo's
"don't paper over a wrong ticket premise" rule.)

## Changes

- **`docs/PRD.md`** (§5, v3 `_version_`): rewrote the section to the JSON-facet premise,
  recorded the decision (delivered field kept as-is; real dependency deferred and rescoped to
  JSON faceting; atomic updates / `versions=true` / ordering guarantees out of scope with
  evidence), and referenced finding 132 + #293. The forward-looking Architecture / Tracer /
  Testing subsections were folded into a "Delivered in v1" paragraph now that the field is
  implemented and tested in `tests/version_field.rs`.
- **`docs/PRD.md`** (§5 parity table): corrected the "JSON Request API / JSON Facet API" row,
  which claimed `json.facet` "appears nowhere in its source" — literally true (the PHP uses
  Solarium's `createJsonFacetAggregation`, not the bare string) but functionally false and a
  direct contradiction of finding 132. It now records that the JSON Facet API is used, only on
  admin diagnostics screens, and points at the v3 `_version_` subsection.
- **`tests/version_descope_guard.rs`** (new): a self-deleting guard over both evidence
  channels, mirroring `tests/edismax_descope_guard.rs`:
  - *trace channel* — no captured request references `_version_` / `versions=true` /
    `max(_version_)` / `json.facet` in any form (path, headers, or body); the corpus is still
    the 28 traces; a positive control confirms `_version_` is present in *responses* so the
    request-side scan is not blind.
  - *source channel* — the 4.4.0 source reads `_version_` only through
    `createJsonFacetAggregation` + `max(_version_)`, never requests `versions=true`, and
    writes whole documents via `addDocument(s)`.
  - *PRD tripwires* — the section records the JSON-facet premise, no longer attributes
    `stats.field` to the client, and references #293.
- **Stale comments** corrected to stop attributing the stats path to the client:
  `src/lib.rs` (`function` param), `src/stats.rs` (`check_statable` exception),
  `src/core_index.rs` (`version_seed`). Functionality is unchanged — `stats.field=_version_`
  and `function=max(_version_)` are real, correct capabilities of the statable field, kept as
  a building block; they are simply not the path any captured client takes.

No production behaviour changed. The existing `tests/version_field.rs` (7 tests) continues to
pin the field semantics: internal i64 fast field, absent from `schema.toml`, user-declared
name rejected, dynamic rules cannot forge/sort/facet it, stats-only access, gapless
per-document versions with `stats max(_version_)` returning the latest.

## Commands / results

```
cargo fmt --check                              # clean
cargo clippy --all-targets -- -D warnings      # clean
cargo test                                     # 40 binaries, 0 failed
cargo test --test version_descope_guard        # 9 passed
cargo test --test version_field                # 7 passed
```

The guard was confirmed **red for the right reason** before the PRD correction (the 6
evidence checks passed; the 3 PRD-correction checks failed on the wrong stats.field premise),
then **green** after — that red→green transition is the guard's built-in mutation proof. Its
positive control (`version_is_present_in_trace_responses_so_the_request_scan_is_not_blind`)
proves the request/source substring scans actually detect `_version_`.

## Follow-ups

- **JSON faceting** (`json.facet`, `type: terms` nesting, aggregation functions such as
  `max()`): the actual client dependency, deferred. Track under a JSON-faceting issue, not
  under `_version_`. The delivered `_version_` field is exactly what its `max(_version_)`
  aggregation will read, so landing it costs nothing extra at this layer.
- If a future capture or a regenerated coverage source ever sends `_version_` request-side,
  `versions=true`, or stops reading `_version_` through a JSON facet aggregation,
  `tests/version_descope_guard.rs` goes red: revisit this decision (#293) with the new
  evidence rather than weakening the guard.
