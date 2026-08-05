# #355 — correct finding 132: nested terms facets are primary, `max(_version_)` is the fallback

**Date:** 2026-08-04. **Branch:** `markdlabrecque/issue-355-correct-finding-132`.
**Spec:** `docs/specs/355-finding-132-amendment.md`. **Source of truth:**
`coverage/search_api_solr_4.4.0_source/.../SearchApiSolrBackend.php`, read line-for-line.

## Premise check (spec Step 1)

The issue was written to land **before** #343 so #343 would not build to the wrong first
target. #343 has already shipped (PR #365, `src/json_facet.rs`) and built the *correct* shape:
`type: terms` with `facet`-key nesting, `local_key` echoed as the response key, `limit: -1` as
unlimited, `max()` aggregations. Fixtures: `jf343_terms_nested`, `jf343_deep_max`,
`jf343_terms_limit`, `jf343_agg_max_version`. So — as the spec predicted — this collapses to
**docs-only**. No implementation gap in #343.

## What finding 132 got right and wrong

- **Right (kept):** `search_api_solr` reads `_version_` only through a JSON facet, never via
  `stats.field`, never writes it, never sends `versions=true`.
- **Wrong (corrected):** the *shape*. Finding 132 presented `max(_version_)` as the read path
  with `terms` nesting "optional". The reverse is true.

## Verified shape, against the frozen source

Two distinct JSON-facet callers, both admin-diagnostics screens with base query
`+hash:* +index_id:*` (`{!key=search_api}` fq), `rows=1`, `fl=id`:

- **`doDocumentCounts()` (`:4895`) — document counts.** Primary = nested `terms` with **no
  `_version_`**: top-level `siteHashes` (`hash`, `limit: -1`, `:4914`) → nested `numDocsPerIndex`
  (`index_id`, `limit: -1`, `:4920`, `addFacet` `:4928`). `max(_version_)` is only the `catch`
  fallback (`:4934-4940`), a minimal facet over the one always-present field.
- **`doGetMaxDocumentVersions()` (`:5033`, ← `getMaxDocumentVersions()` `:4987` ← caller `:1064`)
  — max document version.** Primary = top-level `max(_version_)` (`:5052-5054`) **plus** a
  four-level nested topology: `siteHashes` (`hash`, `:5057`) → `indexes` (`index_id`, `:5063`) →
  `dataSources` (`ss_search_api_datasource`, `:5069`) → `maxVersionPerDataSource` (`max(_version_)`,
  `:5075-5077`, wired by `addFacet` `:5080-5082`). `catch` fallback (`:5088-5093`) is a bare
  `max(_version_)`.

So the normal case is `type: terms` + `local_key` + arbitrary nesting; `max()` is the fallback
and the per-datasource leaf. This is what #343 built.

## Ticket-premise correction (do not paper over)

The issue **and** the spec placed the `omitHeader=false` / SOLR-13509 guard at `:5079-5085`. That
range is `doGetMaxDocumentVersions()`'s `addFacet` nesting — **no `setOmitHeader` call lives
there.** The guard is `:4943-4949` in **`doDocumentCounts()`** only (the fallback): for Solr
>= 8.1.0 it forces `setOmitHeader(FALSE)` because the facet NPEs inside Solr when headers are
omitted (SOLR-13509; comment `:4944-4948`). `doGetMaxDocumentVersions()` has no such call. Both
finding 191 and the PRD note the corrected location.

## The `omitHeader=false` detail — choice stated (spec Step 3)

Not covered by an existing fixture (`jf343_agg_max_version` sends `omitHeader=false`, i.e. the
workaround, not the failing combo), **not** newly captured, **and not** ratified as a divergence.
Reason: the guard is unconditional for Solr >= 8.1, so no captured client request ever omits the
header on this path and the upstream NPE is never reached — there is no live wire gap for
`EXPECTED_DIVERGENCES` to assert, unlike finding 128's `json.nl=garbage` case. Recorded in finding
190 as a client behaviour; revisit only if a capture shows a client hitting the NPE.

## Changes

- `docs/solr-ref-findings.md`: appended a `**Amended by finding 191 (2026-08-04):**` pointer to
  finding 132 (body left in place, per the batch-amendment convention in `docs/specs/README.md`),
  and added finding 191 with the full correction.
- `docs/PRD.md` §5 v3: rewrote the "What the client actually does with `_version_`" paragraph to
  the corrected shape (nested terms primary, `max()` fallback) and pointed at finding 191; the
  "earlier draft … `stats.field`" sentence now records both layered corrections.

## Gates

```
cargo test --test finding_citations   → 2 passed (unique numbering; every citation resolves)
cargo fmt --check                     → clean
cargo clippy --all-targets -- -D warnings → no warnings
```

## Renumber

On rebase, #382 (PR #382, `q.op`/`qt`) had already landed its own finding **190** at the
bottom of the same file. Collision resolved by keeping #382's finding 190 and renumbering this
amendment's new finding to **191**; all cross-references (the finding-132 pointer, PRD sec 5,
this report) updated. `finding_citations` confirmed both 190 and 191 resolve and stay unique.

## Sibling

#351 also amends a finding in `docs/solr-ref-findings.md` (still OPEN). Same append convention,
so it merges mechanically — rebase onto `main` and re-run `finding_citations` if it lands first.
If #351 also grabs the next free number, renumber as above.
