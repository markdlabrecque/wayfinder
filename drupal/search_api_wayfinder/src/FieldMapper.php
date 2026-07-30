<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder;

use Drupal\Core\TypedData\ComplexDataDefinitionInterface;
use Drupal\search_api\IndexInterface;
use Drupal\search_api\Item\FieldInterface;
use Drupal\search_api\SearchApiException;

/**
 * Maps Search API field identifiers/types to Wayfinder dynamic field names,
 * and formats field values for indexing.
 *
 * Prefixes are copied from search_api_solr's dynamic-field naming convention
 * (locked decision 1, docs/plans/57-search-api-wayfinder-backend.md): every
 * Search API data type gets a single-letter (or two-letter) type prefix, plus
 * an 's' or 'm' infix for single/multi-valued, e.g. 'ts_title' / 'tm_body'.
 *
 * ponytail: only the six default Search API types are mapped (text, string,
 * integer, decimal, date, boolean) -- solr_* / location types are out of
 * scope per WayfinderBackend::supportsDataType().
 */
class FieldMapper {

  /**
   * Type prefixes, copied from search_api_solr's Utility::getDataTypeInfo().
   *
   * @var array<string, string>
   */
  private const TYPE_PREFIXES = [
    'text' => 't',
    'string' => 's',
    'integer' => 'it',
    'decimal' => 'ft',
    'date' => 'd',
    'boolean' => 'b',
  ];

  /**
   * Maps a Search API field to its Wayfinder dynamic field name.
   */
  public function fieldName(string $fieldId, string $type, bool $multiValued): string {
    $prefix = self::TYPE_PREFIXES[$type] ?? $type;
    $infix = $multiValued ? 'm' : 's';
    return $prefix . $infix . '_' . $fieldId;
  }

  /**
   * Formats a single field value for indexing, per Search API data type.
   *
   * @return string|int|float
   *   The formatted value.
   */
  public function formatValue($value, string $type) {
    switch ($type) {
      case 'date':
        return gmdate('Y-m-d\TH:i:s\Z', (int) $value);

      case 'boolean':
        return $value ? 'true' : 'false';

      default:
        return $value;
    }
  }

  /**
   * Determines whether a field is multi-valued, from the index's own
   * property-path cardinality -- NOT from how many values happen to be set
   * on one particular item, so the same field always maps to the same
   * dynamic field name regardless of a single item's/query's data.
   *
   * This is a Wayfinder-shaped port of search_api_solr's
   * getPropertyPathCardinality() (locked decision 1: copying method-level
   * logic from search_api_solr is allowed, both modules are
   * GPL-2.0-or-later): walk the field's property path segment by segment
   * through the index's property definitions, since the "is this a list"
   * flag usually lives on an intermediate property in the path, not
   * necessarily the leaf.
   */
  public function isMultiValued(FieldInterface $field): bool {
    try {
      $properties = $field->getIndex()->getPropertyDefinitions($field->getDatasourceId());
    }
    catch (SearchApiException $e) {
      return FALSE;
    }

    return $this->propertyPathIsList($field->getPropertyPath(), $properties);
  }

  /**
   * Walks a colon-separated property path through nested property
   * definitions, returning TRUE as soon as any segment (intermediate or
   * leaf) is a list-typed property.
   *
   * @param array<string, \Drupal\Core\TypedData\DataDefinitionInterface> $properties
   *   Property definitions to start walking from, keyed by property name.
   */
  private function propertyPathIsList(string $propertyPath, array $properties): bool {
    foreach (explode(IndexInterface::PROPERTY_PATH_SEPARATOR, $propertyPath) as $name) {
      if (!isset($properties[$name])) {
        return FALSE;
      }

      $definition = $properties[$name];
      if ($definition->isList()) {
        return TRUE;
      }

      if ($definition instanceof ComplexDataDefinitionInterface) {
        $properties = $definition->getPropertyDefinitions();
      }
      else {
        return FALSE;
      }
    }

    return FALSE;
  }

}
