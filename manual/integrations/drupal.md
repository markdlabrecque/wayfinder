# Drupal Search API integration

Wayfinder supports the bounded Drupal Search API convention in
`presets/search-api.toml`; it is not a Solr server/configset implementation.
Read the [Drupal boundary inventory](../reference/drupal.md),
[Compatibility](../../docs/COMPATIBILITY.md), and the hermetic preset contract
in [`tests/search_api_preset.rs`](../../tests/search_api_preset.rs) before
configuration.

## Set up one logical core

Run one Wayfinder process, port, schema, and data directory for each logical
Drupal core. Start from the preset and configure the Drupal endpoint/core name
to match that process. The preset's dynamic fields represent Search API's field
convention; static schema changes remain Wayfinder migrations. The system-admin
version response provides the supported Search API admin handshake.

**Prerequisites:** create an empty owned data directory, preserve the old
schema/data endpoint, and verify Drupal can reach the chosen HTTP endpoint
through a trusted proxy. **Visibility/durability:** queued Drupal indexing is
not searchable until Wayfinder commits it; wait for/force the configured commit
policy. **Failure/retry:** handshake, schema, or index errors should be corrected
before requeuing; do not point two processes at one directory. **Validation:**
check admin handshake, representative searches, and per-`index_id` counts after
a durable commit. **Rollback:** restore the prior endpoint/schema/data set and
requeue only after it is healthy.

## Prefix model and deliberate divergences

The preset is a field-prefix/dynamic-field model, not imported Solr XML
analyzers or arbitrary `solr_text_custom` analyzer families. Boolean parsing,
collation, analyzed text, unindexed fields, and spatial behavior remain bounded
by Wayfinder's schema and compatibility contract. Server responses use the
retained JSON wire, not Solr configuration files or core-admin lifecycle.

Stock `search_api_solr` autocomplete is **unsupported**: it calls
`/autocomplete`, which Wayfinder does not serve. No connector/adapter is shipped.
Use Drupal work that can call the supported `/terms` prefix route or bounded
`/suggest` dictionary route, or provide and maintain an integration outside this
project. Do not retry the absent handler or treat an HTTP 404 as a transient
indexing failure.

## Reindex after a migration

A static schema/analyzer migration requires a new directory. Keep the old core
available, start a new process/port with the candidate schema, reset/requeue all
Search API indexes that share it, then run Drupal's normal index processing.
Commit and validate counts and representative queries before switching callers.
A failed candidate is disposable; retry it from the complete source feed. Roll
back by retaining/repointing the old endpoint and data directory. The normative
fresh-index sequence is [Deployment](../../docs/DEPLOYMENT.md#reindex).
