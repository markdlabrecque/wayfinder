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
- **Only the six default Search API data types** — `text`, `string`,
  `integer`, `decimal`, `date`, `boolean` (`FieldMapper`,
  `WayfinderBackend::supportsDataType()`). `solr_*` and location/spatial types
  are out of scope.
- **Multi-valued text sorting uses the first value** (`DocumentBuilder`) — a
  collation-aware multi-value text selector needs a dedicated schema/type
  design first. Non-text types sort natively with Wayfinder's multi-value
  min/max selection.
- **Per-field facet settings** (`QueryBuilder::buildFacets()`) —
  `facet.limit`/`facet.mincount`/`facet.missing`/`facet.sort` are *global* on
  the Wayfinder wire; there is no `f.<field>.facet.*` override. A query whose
  facets disagree on those settings cannot be expressed: the last facet's
  settings win for the whole request.

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
