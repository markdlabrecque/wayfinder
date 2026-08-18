# Drupal Search API boundaries

`presets/search-api.toml` provides Wayfinder's dynamic-field convention. Run one
Wayfinder process, schema, data directory, and endpoint per logical core; it is
not a Solr core-admin or configset service.

The system-admin version response supports the Search API version handshake.
Reindex after schema or analyzer migrations, and keep the old data directory
until representative queries and per-`index_id` counts pass. The exact fresh
index procedure is normative in [Deployment](../../docs/DEPLOYMENT.md).

Do not claim stock autocomplete compatibility: stock `search_api_solr` calls the
unsupported `/autocomplete` handler. The historical project-specific adapter is
not shipped here. Use the supported `/terms` prefix route or the bounded
`/suggest` lookup where appropriate. Boolean/query parsing, collation,
analyzed-text behavior, unindexed fields, and spatial features remain bounded
by [Compatibility](../../docs/COMPATIBILITY.md), not by Solr configuration.
