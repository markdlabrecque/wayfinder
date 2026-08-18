# Drupal Search API reference

Wayfinder ships `presets/search-api.toml`, a schema for the Drupal Search API
field-prefix model, not a Drupal connector. The preset maps the familiar static
identity/boost fields and dynamic `ss_`/`sm_`, `ts_`/`tm_`, numeric, date,
boolean-as-string, date-range, storage/docvalues, sort, spellcheck, and
suggest prefixes to Wayfinder types. Static fields win over a colliding prefix;
fast fields can sort/facet only within their declared constraints. Prefixes are
schema conventions, not runtime schema creation.

## Handshake and divergences

Point the backend at one Wayfinder process/core and use its reported server
version for the admin handshake. The server/system and selected schema/admin
metadata are the narrow handshake surface; there is no core reload,
field-analysis, configset-file route, Solr XML configuration, or SolrCloud
administration. No connector/adapter is shipped.

Stock `search_api_solr` autocomplete is unsupported. Use a deliberately
implemented Drupal UI/search flow, or bounded `/terms` prefix lookup and
`/suggest` where its dictionary semantics fit. Do not assume all Solr parsing,
collation, analyzer families, unindexed fields, spatial behavior, or exact score
magnitudes carry over. In particular, `search_api_solr` traffic can contain
inline local-parameter shapes whose retained low-recall behavior is intentional.

## Preset migration lifecycle

**Prerequisites:** identify every Search API index sharing the core, preserve the
old data directory/configuration, and have a fresh directory owned by the
service user. **Visibility:** a new preset/schema is active only after the new
process starts; reindexed documents become searchable only after commit.
**Durability:** retain the matching schema, configuration, and committed index
as one set. **Failure/retry:** startup analyzer/schema refusal means reindex,
not metadata surgery; a failed queue run can be retried after correcting its
input. **Validation:** complete reindex after a migration, then check admin
handshake, representative searches, and per-`index_id` counts. **Rollback:**
route back to the retained old process/data/schema until the new set passes.

The prefix contract and production-shaped requests are hermetically covered by
`tests/search_api_preset.rs`, `tests/search_api_source_evidence.rs`, and
`tests/local_params.rs`.
