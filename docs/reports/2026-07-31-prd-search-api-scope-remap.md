# PRD remap: scope to Search API usage, Solr 9.x parity roadmap for the rest

Date: 2026-07-31
Change: `docs/PRD.md` section 5 only. No code.

## What changed

1. **A second scoping rule.** v1 keeps "ship what Tantivy supports natively". From v2 onward
   the rule is now explicit: ship what `search_api_solr` demonstrably uses; everything else
   Solr 9.x offers is unscheduled, on a new "Solr 9.x parity roadmap" table. This generalises
   the descope pattern the PRD had already applied three times case by case (edismax six
   params, issue #136; `terms`/`admin/luke`/`admin/mbeans`, issue #57; `_version_` narrowing).

2. **New parity table** listing Solr 9.x features with zero client evidence: JSON
   Request/Facet API, `facet.pivot`/`facet.interval`, Collapse & Expand, Query Elevation,
   block join / nested documents, Realtime Get, TermVector, `cursorMark`, LTR, clustering
   (Carrot2), the Tagger handler, and atomic updates / optimistic concurrency (cross-referenced
   to the existing v3 `_version_` subsection). Stated reason any would ever be built: Solr 9.x
   wire parity as a goal in itself, or a new client capture showing real usage.

3. **Phases table corrections.**
   - v3: "grouping/collapse" split. The module's `collapse`-named identifiers
     (`setGrouping()`'s `$collapse_field` loop, `SearchApiSolrBackend.php:4579`) emit Solr
     *grouping* params (`group=true`), never the Collapse/Expand component
     (`fq={!collapse}` + `expand=true`). Grouping stays in v3; Collapse/Expand moved to the
     parity table. v3's spellcheck/suggester line now names the client path
     (`spellcheck.*`, `suggest`, `terms` — all in the coverage denominator).
   - v4: function queries and spatial now carry their client evidence inline. The
     `BoostMoreRecent` processor emits `product(…,recip(ms(…)))` boosts; spatial searches emit
     `{!geofilt}`/`bbox`/`{!frange}geodist()` and heatmap facets. Both were correctly phased
     but previously undefended.

4. **Non-goals left alone.** SolrCloud, streaming expressions, SQL, Tika `/extract`, XML/javabin
   are also client-unused but stay section 1 non-goals — a stronger statement than unscheduled.
   The Tika caveat (search_api_attachments *can* use `/update/extract`) is recorded as a knowing
   gap.

5. **Nothing unshipped.** `facet.range` is implemented but never emitted by the module; it is
   noted as surplus and kept.

## Evidence method

- `coverage/search_api_coverage_contract.json` parameter denominator: contains `spellcheck.*`,
  `terms`, `terms.fl`, `mlt.*`, `stats`, `group`-related and classic `facet.*` params; contains
  none of the parity-table features.
- Grep sweep of the vendored 4.4.0 core (`coverage/search_api_solr_4.4.0_source/src`) for every
  parity-table row: `json.facet`, `facet.pivot`, `facet.interval`, `facet.range`,
  `{!collapse}`, `getExpand`, `elevate`, `{!parent}`/`{!child}`, `cursorMark`, LTR, clustering,
  `/tvrh`, `/tag`, Realtime Get — all zero hits.
- Same sweep repeated against a fresh upstream clone of the module's 4.4.x branch including its
  submodules (`search_api_solr_autocomplete`, `_admin`, `_devel`, `_legacy`, `_log`) — also all
  zero hits. The submodule sweep matters because the vendored copy is `src/` only and the
  capture site had autocomplete uninstalled.
- Positive evidence for kept items: `BoostMoreRecent.php:138-139` (function-query boost),
  `SearchApiSolrBackend.php:3701-3719` (geofilt/bbox/geodist), `:4575-4597` (grouping),
  `:3854-3924` (facet.missing), `SolrSpellcheckBackendTrait.php` and
  `SolrAutocompleteBackendTrait.php` (spellcheck/suggester/terms).

## Follow-ups

- None required. The parity table carries its own revisit condition (new client capture), the
  same expiring-guard philosophy as `tests/edismax_descope_guard.rs`, though no code guard is
  possible for features with no implementation to guard.
