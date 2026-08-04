# search_api_wayfinder

A [Search API](https://www.drupal.org/project/search_api) backend plugin
(plugin id `wayfinder`) that indexes and queries a **Wayfinder** server —
the Solr-wire-compatible search backend in this repository.

It talks Solr wire format directly over Guzzle (Drupal core's `http_client`
service). It does **not** depend on `search_api_solr` or Solarium, and it does
not use a connector plugin: the backend plugin is self-contained. Field-naming
conventions and a handful of method-level behaviours are ported from
`search_api_solr` (both modules are GPL-2.0-or-later) so that the two produce
consistent wire output and a consistent config UX — the itemised list is in
`docs/reports/2026-07-30-search-api-wayfinder-m5-polish.md` at the repository
root.

## Install

```
composer require wayfinder/search_api_wayfinder
drush en search_api_wayfinder
```

The module is not on Packagist; add this repository as a Composer `path` or
`vcs` repository first. (`tests/integration/run.sh` does exactly this with a
`path` repo, if you want a worked example.)

Then, in Drupal:

1. **Configuration → Search and metadata → Search API → Add server**, and pick
   **Wayfinder** as the backend.
2. Fill in scheme/host/port/base path/core to point at your Wayfinder instance
   (defaults: `http://localhost:8983/solr/<core>`). Optionally set the request
   timeout and enable **Retrieve result highlighting from the server**.
3. Add an index against that server and index content as usual.

Once the server is saved, its *View* page shows the server URL plus the
Wayfinder version, read from `{core}/admin/system`.

## Wayfinder-side schema

Create the Wayfinder core from this repository's **`presets/search-api.toml`**.

That preset expresses the `search_api_solr` dynamic-field naming convention
(`ts_title`, `tm_body`, `its_count`, `sm_tags`, `sort_*`, …) as Wayfinder
`[[fields]]`/`[[dynamic_fields]]` rules, derived from the captured ground-truth
configset at `solr-ref/search-api/configset/schema.xml`. This module's
`FieldMapper` emits exactly those names, so a core built from any other schema
will silently fail to match the fields the module writes and queries.

## Not supported

Deliberate descopes, each with its reason. (Each is also marked with a
`ponytail:` comment at its site in `src/`.)

- **Two Drupal sites sharing one core** (`DocumentBuilder`) — ids are
  `<index_id>-<item_id>`, with no `search_api_solr`-style site-hash component,
  so **one core per site is the supported topology**. This is a decision
  (issue #301), not a pending simplification: several sites on one host means
  several Wayfinder processes with one core each, which the server already
  assumes — it serves a single core per process (`docs/PRD.md` open question 1).

  Nothing enforces this. Point two sites at one core and documents whose index
  and item ids coincide overwrite each other silently, with no error on either
  side. The module cannot detect it; keep the topology correct by
  configuration.
- **Search API data types** (`FieldMapper`, `WayfinderBackend::supportsDataType()`).
  The six defaults (`text`, `string`, `integer`, `decimal`, `date`, `boolean`)
  plus the `search_api_solr` non-default types that round-trip on Wayfinder's
  existing schema types are supported (issue #300):
  `solr_string_storage`, `solr_string_docvalues`, `solr_text_unstemmed`,
  `solr_text_omit_norms`, `solr_text_wstoken`, and `solr_text_suggester` (which
  indexes into the fixed sink field `twm_suggest` that `#291`'s SuggestComponent
  reads).

  Two indexability divergences on the newly-supported types are accepted and
  documented at their `ponytail:` sites, not silently papered over:
  - `solr_string_storage` / `solr_string_docvalues` are *storage/docValues-only*
    in Solr (`indexed=false`). Wayfinder has no unindexed field, so they are
    indexed (and therefore queryable) here — a superset of their Solr
    capability. A field deliberately kept out of search by typing it
    storage-only **becomes searchable** on Wayfinder.
  - The `solr_text_*` variants all collapse onto `text_general`, so their Solr
    analyzer-chain distinctions (unstemmed vs. omit-norms vs. whitespace-
    tokenized) are not preserved — a scoring-quality divergence, not a data-
    integrity one.

  Still **not** supported, each with its reason rather than a silent omission:
  - `solr_date_range` — Wayfinder's `date` type holds a single instant, not a
    `[start TO end]` range; needs a new server-side date-range type.
  - `solr_text_spellcheck` — indexes into the language-specific fixed sink
    `spellcheck_<lang>`; the `FieldMapper` has no language-aware naming yet.
    Lands with the spellcheck work.
  - `solr_text_custom` / `solr_text_custom_omit_norms` — `search_api_solr`'s
    escape hatch for site-defined analyzer chains (`SolrFieldType` entities);
    the preset has no equivalent.
  - `location` / `rpt` — spatial types, scope of #292.
- **Multi-valued text sorting uses the first value** (`DocumentBuilder`) — a
  collation-aware multi-value text selector needs a dedicated schema/type
  design first. Non-text types sort natively with Wayfinder's multi-value
  min/max selection.
- **Per-facet settings beyond the four** (`QueryBuilder::buildFacets()`) —
  each facet's `limit`/`min_count`/`sort`/`missing` *is* expressed
  independently, as local params on that facet's own `facet.field`
  (`{!key=<delta> facet.limit=10 ...}<field>`, issue #296), so two facets on
  one field keep their own settings. Nothing else is per-facet: Wayfinder
  honours `facet.prefix`/`facet.method` in neither form, and `facet.range.*`
  only globally, so a range facet cannot disagree with another one.

## Testing

The PHPUnit suite is hermetic (no network, no Docker) and runs in CI:

```
cd drupal/search_api_wayfinder && composer install && vendor/bin/phpunit
```

For a real end-to-end check — Docker-based, installs a live Drupal site
against a live Wayfinder built from `presets/search-api.toml` — run the
env-gated integration harness:

```
WAYFINDER_INTEGRATION=1 bash drupal/search_api_wayfinder/tests/integration/run.sh
```
