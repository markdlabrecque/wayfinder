<?php
// Creates the Search API Solr server (pointed at the mitm capture proxy, NOT
// Solr directly -- that is load-bearing for issue #55's HTTP trace) and an
// index over the two content types set up by setup_fields.php.

use Drupal\search_api\Entity\Server;
use Drupal\search_api\Entity\Index;

$server = Server::create([
  'id' => 'capture_server',
  'name' => 'Capture server',
  'description' => 'issue #55 capture: points at the mitmproxy in front of the capture-only Solr, not Solr directly.',
  'backend' => 'search_api_solr',
  'backend_config' => [
    'site_hash' => TRUE,
    'retrieve_data' => TRUE,
    'highlight_data' => TRUE,
    'domain' => 'generic',
    'server_prefix' => '',
    'connector' => 'standard',
    'connector_config' => [
      'scheme' => 'http',
      'host' => 'mitm',
      'port' => 8983,
      'path' => '/solr',
      'core' => 'search_api_capture',
      'timeout' => 10,
      'index_timeout' => 30,
      'optimize_timeout' => 10,
      'finalize_timeout' => 30,
      'solr_version' => '',
      'http_method' => 'AUTO',
      'commit_within' => 1000,
      'jmx' => FALSE,
      'jts' => FALSE,
      'solr_install_dir' => '',
      'skip_schema_check' => TRUE,
    ],
    'optimize' => FALSE,
    'fallback_multiple' => TRUE,
  ],
  'status' => TRUE,
]);
$server->save();

$index = Index::create([
  'id' => 'capture_index',
  'name' => 'Capture index',
  'server' => 'capture_server',
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
    'created' => [
      'label' => 'Created',
      'datasource_id' => 'entity:node',
      'property_path' => 'created',
      'type' => 'date',
    ],
    'sticky' => [
      'label' => 'Sticky',
      'datasource_id' => 'entity:node',
      'property_path' => 'sticky',
      'type' => 'boolean',
    ],
    'nid' => [
      'label' => 'Node ID',
      'datasource_id' => 'entity:node',
      'property_path' => 'nid',
      'type' => 'integer',
    ],
    'type' => [
      'label' => 'Content type',
      'datasource_id' => 'entity:node',
      'property_path' => 'type',
      'type' => 'string',
    ],
    'field_sku' => [
      'label' => 'SKU',
      'datasource_id' => 'entity:node',
      'property_path' => 'field_sku',
      'type' => 'string',
    ],
    'field_rating' => [
      'label' => 'Rating',
      'datasource_id' => 'entity:node',
      'property_path' => 'field_rating',
      'type' => 'integer',
    ],
    'field_featured' => [
      'label' => 'Featured',
      'datasource_id' => 'entity:node',
      'property_path' => 'field_featured',
      'type' => 'boolean',
    ],
    'field_keywords' => [
      'label' => 'Keywords',
      'datasource_id' => 'entity:node',
      'property_path' => 'field_keywords',
      'type' => 'string',
    ],
    'field_event_date' => [
      'label' => 'Event date',
      'datasource_id' => 'entity:node',
      'property_path' => 'field_event_date',
      'type' => 'date',
    ],
    'field_priority' => [
      'label' => 'Priority',
      'datasource_id' => 'entity:node',
      'property_path' => 'field_priority',
      'type' => 'integer',
    ],
    'field_published_on' => [
      'label' => 'Published on',
      'datasource_id' => 'entity:node',
      'property_path' => 'field_published_on',
      'type' => 'date',
    ],
    'field_archived' => [
      'label' => 'Archived',
      'datasource_id' => 'entity:node',
      'property_path' => 'field_archived',
      'type' => 'boolean',
    ],
    'field_topics' => [
      'label' => 'Topics',
      'datasource_id' => 'entity:node',
      'property_path' => 'field_topics',
      'type' => 'string',
    ],
  ],
  'options' => [
    'index_directly' => TRUE,
    'cron_limit' => 50,
  ],
]);
$index->save();

echo "server + index created\n";
