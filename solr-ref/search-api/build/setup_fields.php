<?php
// Adds a representative field mix to the 'article' and 'page' content types
// (fulltext, string, integer, date, boolean, multi-value) for issue #55's
// search_api_solr capture. Run via `drush php:script`.

use Drupal\field\Entity\FieldConfig;
use Drupal\field\Entity\FieldStorageConfig;

function wf_add_field($entity_type, $bundle, $field_name, $type, $cardinality, $settings = [], $label = null) {
  if (!FieldStorageConfig::loadByName($entity_type, $field_name)) {
    FieldStorageConfig::create([
      'field_name' => $field_name,
      'entity_type' => $entity_type,
      'type' => $type,
      'cardinality' => $cardinality,
      'settings' => $settings,
    ])->save();
  }
  if (!FieldConfig::loadByName($entity_type, $bundle, $field_name)) {
    FieldConfig::create([
      'field_name' => $field_name,
      'entity_type' => $entity_type,
      'bundle' => $bundle,
      'label' => $label ?: $field_name,
    ])->save();
  }
}

// article: string, integer, boolean, multi-value string
wf_add_field('node', 'article', 'field_sku', 'string', 1, ['max_length' => 64], 'SKU');
wf_add_field('node', 'article', 'field_rating', 'integer', 1, [], 'Rating');
wf_add_field('node', 'article', 'field_featured', 'boolean', 1, [], 'Featured');
wf_add_field('node', 'article', 'field_keywords', 'string', -1, ['max_length' => 64], 'Keywords');
wf_add_field('node', 'article', 'field_event_date', 'datetime', 1, ['datetime_type' => 'date'], 'Event date');

// page: integer, date, boolean, multi-value
wf_add_field('node', 'page', 'field_priority', 'integer', 1, [], 'Priority');
wf_add_field('node', 'page', 'field_published_on', 'datetime', 1, ['datetime_type' => 'date'], 'Published on');
wf_add_field('node', 'page', 'field_archived', 'boolean', 1, [], 'Archived');
wf_add_field('node', 'page', 'field_topics', 'string', -1, ['max_length' => 64], 'Topics');

echo "fields created\n";
