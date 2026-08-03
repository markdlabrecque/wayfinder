<?php
// Creates a Search API server using the standalone "wayfinder" backend
// plugin (search_api_wayfinder module under test, WayfinderBackend) pointed
// at the wayfinder container from docker-compose.yml, and an index over
// node title/body (fulltext) -- deliberately using only built-in node
// properties so no custom field setup is needed for this round-trip check.
//
// Issue #80: adapted from the old worktree's
// setup_server_index.php (/Users/mark/Projects/wayfinder-57-search-api-wayfinder/drupal/search_api_wayfinder/tests/integration/setup_server_index.php),
// which configured a `search_api_solr` server with a `wayfinder` *connector*
// plugin (backend: 'search_api_solr', backend_config.connector: 'wayfinder').
// M1 (#75) superseded that architecture with a standalone backend, so this
// version configures the server directly with `backend: 'wayfinder'` and
// WayfinderBackend's own configuration schema (scheme/host/port/path/core/
// timeout/commitWithin/username/password -- see
// config/schema/search_api_wayfinder.schema.yml and
// WayfinderBackend::defaultConfiguration()) -- no search_api_solr, no
// Solarium, no connector plugin at all.
//
// `core` is "content", matching `[core].name` in the repo's
// `presets/search-api.toml` (the schema this harness's Wayfinder container
// loads, per docker-compose.yml) -- NOT a harness-local schema, so this
// proves the round trip against the actual preset shipped for Drupal sites.
//
// M1 scope note (WayfinderBackend::getSupportedFeatures() currently returns
// []): no facets, filters, or sorts are configured here. Only plain
// fulltext search is exercised, matching what QueryBuilder/ResponseParser
// implement as of M1.

use Drupal\search_api\Entity\Server;
use Drupal\search_api\Entity\Index;
use Drupal\field\Entity\FieldStorageConfig;
use Drupal\field\Entity\FieldConfig;

// Issue #262 tracer: a file field on the article bundle. The
// wayfinder_file_extraction processor discovers it (file-typed field on an
// indexed datasource), declares a `saw_field_attachments` computed property,
// and populates it from /update/extract. Creating it here, before the index
// below, is what makes that property resolvable when the index field mapped
// to it is validated on save. Plain `file` fields only this slice -- media /
// entity:file are documented follow-ups, not this tracer.
FieldStorageConfig::create([
  'entity_type' => 'node',
  'field_name' => 'field_attachments',
  'type' => 'file',
  'cardinality' => -1,
])->save();
FieldConfig::create([
  'entity_type' => 'node',
  'bundle' => 'article',
  'field_name' => 'field_attachments',
  'label' => 'Attachments',
])->save();

$server = Server::create([
  'id' => 'wf80_server',
  'name' => 'Wayfinder IT server',
  'description' => 'issue #80 integration verification: search_api_wayfinder standalone backend against a real Wayfinder instance.',
  'backend' => 'wayfinder',
  'backend_config' => [
    'scheme' => 'http',
    'host' => 'wayfinder',
    'port' => 8983,
    'path' => '/solr',
    'core' => 'content',
    'timeout' => 5,
    'commitWithin' => 1000,
    'username' => 'operator',
    'password' => 'secret',
  ],
  'status' => TRUE,
]);
$server->save();

$index = Index::create([
  'id' => 'wf80_index',
  'name' => 'Wayfinder IT index',
  'server' => 'wf80_server',
  'status' => TRUE,
  'datasource_settings' => [
    'entity:node' => [
      'bundles' => [
        'default' => FALSE,
        'selected' => ['article', 'page'],
      ],
    ],
  ],
  'tracker_settings' => [
    'default' => [],
  ],
  'field_settings' => [
    'title' => [
      'label' => 'Title',
      'datasource_id' => 'entity:node',
      'property_path' => 'title',
      'type' => 'text',
    ],
    'body' => [
      'label' => 'Body',
      'datasource_id' => 'entity:node',
      'property_path' => 'body',
      'type' => 'text',
    ],
    // Issue #262: the extracted attachment text lands in its OWN fulltext
    // field with independent boost (decision 2), not appended to body. The
    // property path is the processor's index-level computed property
    // (datasource NULL), so no datasource_id here.
    'file_content' => [
      'label' => 'File content',
      'property_path' => 'saw_field_attachments',
      'type' => 'text',
    ],
  ],
  'options' => [
    'index_directly' => TRUE,
    'cron_limit' => 50,
  ],
  // Enable the extraction processor alongside the index. No settings of its
  // own in this tracer, so an empty config block.
  'processor_settings' => [
    'wayfinder_file_extraction' => [],
  ],
]);
$index->save();

echo "server + index created\n";
