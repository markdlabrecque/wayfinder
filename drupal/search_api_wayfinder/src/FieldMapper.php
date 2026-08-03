<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder;

use Drupal\Core\Field\FieldDefinitionInterface;
use Drupal\Core\Field\FieldStorageDefinitionInterface;
use Drupal\Core\TypedData\ComplexDataDefinitionInterface;
use Drupal\Core\TypedData\DataDefinitionInterface;
use Drupal\Core\TypedData\DataReferenceDefinitionInterface;
use Drupal\Core\TypedData\ListDataDefinitionInterface;
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
 * issue #300 widened this beyond the six default types to the search_api_solr
 * non-default types that round-trip on Wayfinder's existing schema types:
 * solr_string_storage, solr_string_docvalues, solr_text_unstemmed,
 * solr_text_omit_norms, solr_text_wstoken (each a normal prefix+infix dynamic
 * field), and solr_text_suggester (the fixed sink field 'twm_suggest'). The
 * prefix table is ground truth from search_api_solr 4.4.0's
 * Utility::getDataTypeInfo() (the six defaults) and the
 * search_api_data_type_info_alter hook in src/Hook/SearchApiSolrHooks.php (the
 * solr_* types). What is NOT here is an explicit descope, recorded with its
 * reason in README "Not supported": solr_date_range (needs a server-side
 * date-range type), solr_text_spellcheck (language-specific fixed sink),
 * solr_text_custom* (site-defined analyzer escape hatch), location/rpt (#292).
 */
class FieldMapper {

  /**
   * Type prefixes, copied from search_api_solr's Utility::getDataTypeInfo()
   * and the search_api_data_type_info_alter hook in src/Hook/SearchApiSolrHooks.php.
   *
   * @var array<string, string>
   */
  private const TYPE_PREFIXES = [
    // Six default Search API types (Utility::getDataTypeInfo lines 66-94).
    'text' => 't',
    'string' => 's',
    'integer' => 'it',
    'decimal' => 'ft',
    'date' => 'd',
    'boolean' => 'b',
    // issue #300: search_api_solr non-default types (the alter hook's
    // prefix table). Each maps to a Wayfinder type in presets/search-api.toml.
    'solr_string_storage' => 'z',
    'solr_string_docvalues' => 'zdv',
    'solr_text_unstemmed' => 'tu',
    'solr_text_omit_norms' => 'to',
    'solr_text_wstoken' => 'tw',
  ];

  /**
   * The fixed sink field every solr_text_suggester field indexes into.
   *
   * search_api_solr special-cases this type before its generic prefix logic
   * (SearchApiSolrBackend.php:2433-2437): regardless of field id or
   * cardinality, the value lands in the one field the SuggestComponent reads.
   * The SuggestComponent query itself is #291; #300 lands only the field type
   * so the field stops being silently dropped at config time.
   */
  private const SUGGESTER_SINK_FIELD = 'twm_suggest';

  /**
   * Whether a Search API field type is a text type whose values are fulltext.
   *
   * Mirrors search_api_solr's addIndexField() normalisation
   * (SearchApiSolrBackend.php:2706-2708): any type starting with 'solr_text_'
   * is treated as 'text' for value formatting and phrase-quoting. Without
   * this, a solr_text_unstemmed TextValue object would fall through
   * formatValue()'s default branch and json_encode() to '{}' -- the exact
   * malformed-body regression #83 fixed for plain 'text'.
   */
  private function isTextType(string $type): bool {
    return $type === 'text' || str_starts_with($type, 'solr_text_');
  }

  /**
   * Whether a Search API field type is a string type whose filter values are
   * phrase-quoted.
   *
   * solr_string_storage / solr_string_docvalues extend search_api's
   * StringDataType, so their filter values are phrase-quoted exactly like the
   * 'string' type. (In Solr a storage-only field can't be filtered at all; in
   * Wayfinder the documented divergence is that these fields ARE indexed, so a
   * filter on them must still produce valid Lucene phrase syntax.)
   */
  private function isStringType(string $type): bool {
    return $type === 'string' || str_starts_with($type, 'solr_string_');
  }

  /**
   * Maps a Search API field to its Wayfinder dynamic field name.
   */
  public function fieldName(string $fieldId, string $type, bool $multiValued): string {
    // solr_text_suggester is the one type that does NOT follow the
    // prefix+infix dynamic-field convention: every field of this type indexes
    // into the fixed sink field the SuggestComponent reads, regardless of id
    // or cardinality. See SUGGESTER_SINK_FIELD.
    if ($type === 'solr_text_suggester') {
      return self::SUGGESTER_SINK_FIELD;
    }

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
        if ($this->isTextType($type)) {
          // Fulltext field values arrive as TextValue objects (not plain
          // strings); json_encode() would otherwise serialize them to '{}'.
          // __toString() delegates to toText(), reflecting the current text
          // (post-mutation via setText()), not just the constructor value.
          // Covers plain 'text' and every 'solr_text_*' variant (#300):
          // search_api_solr normalises the whole class to 'text' before
          // formatting (SearchApiSolrBackend.php:2706-2708).
          return $value instanceof \Stringable ? (string) $value : $value;
        }

        return $value;
    }
  }

  /**
   * Formats one filter value for Lucene syntax.
   *
   * search_api_solr 4.3.13 treats text, string, and boolean filters as
   * phrases. Inside those phrases only a literal backslash or double quote is
   * escaped; the other Lucene punctuation is ordinary phrase content. Numeric
   * and date values remain bare after their normal Search API formatting.
   */
  public function filterValue($value, string $type): string {
    $formatted = $this->formatValue($value, $type);
    // search_api_solr 4.3.13 treats text, string, and boolean filters as
    // phrases. Inside those phrases only a literal backslash or double quote
    // is escaped; the other Lucene punctuation is ordinary phrase content.
    // The solr_text_* variants (#300) phrase-quote exactly like plain 'text'
    // (isTextType), and solr_string_* like 'string'. Numeric and date values
    // remain bare after their normal Search API formatting.
    if ($this->isTextType($type) || $this->isStringType($type) || $type === 'boolean') {
      return '"' . str_replace(['\\', '"'], ['\\\\', '\\"'], (string) $formatted) . '"';
    }
    return (string) $formatted;
  }

  /**
   * Maps a Search API field to the field used by Wayfinder sorting.
   *
   * Text fields sort through their dedicated sort_* dynamic field; every
   * other type sorts on the actual mapped field, preserving cardinality so
   * Wayfinder can use its native multi-value min/max selection.
   */
  public function sortFieldName(string $fieldId, string $type, bool $multiValued): string {
    return $this->isTextType($type)
      ? 'sort_' . $fieldId
      : $this->fieldName($fieldId, $type, $multiValued);
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
   * through the index's property definitions.
   *
   * `isList()` alone is not a reliable signal: real content-entity field
   * definitions (`FieldDefinitionInterface`, e.g. `BaseFieldDefinition`,
   * `FieldConfigBase`) are lists by construction and return TRUE
   * unconditionally, regardless of the field's actual cardinality. Where a
   * path segment is a `FieldDefinitionInterface`, the real signal is its
   * field-storage cardinality (`getFieldStorageDefinition()->getCardinality()`):
   * `1` is single-valued, anything else (`-1` unlimited, or `> 1`) is
   * multi-valued. Only fall back to the generic `isList()` check for
   * property types that aren't field definitions (e.g. plain nested
   * TypedData properties), where the list level may still live on an
   * intermediate property rather than the leaf.
   *
   * A path segment must still be unwrapped (list-item / reference-target)
   * before descending into its nested properties -- mirroring
   * search_api_solr's `FieldsHelper::getInnerProperty()` -- because
   * `FieldDefinitionInterface` extends `ListDataDefinitionInterface`, not
   * `ComplexDataDefinitionInterface`: without unwrapping, the walk can never
   * reach a second path segment (e.g. `field_ref:entity:field_tags`), which
   * silently drops all but the first value for any such field once mapped
   * to a wrongly-single-valued dynamic field.
   */
  public function isMultiValued(FieldInterface $field): bool {
    try {
      $properties = $field->getIndex()->getPropertyDefinitions($field->getDatasourceId());
    }
    catch (SearchApiException $e) {
      return FALSE;
    }

    return $this->propertyPathIsMultiValued($field->getPropertyPath(), $properties);
  }

  /**
   * Walks a colon-separated property path through nested property
   * definitions, returning TRUE as soon as any segment (intermediate or
   * leaf) resolves as multi-valued.
   *
   * @param array<string, \Drupal\Core\TypedData\DataDefinitionInterface> $properties
   *   Property definitions to start walking from, keyed by property name.
   */
  private function propertyPathIsMultiValued(string $propertyPath, array $properties): bool {
    foreach (explode(IndexInterface::PROPERTY_PATH_SEPARATOR, $propertyPath) as $name) {
      if (!isset($properties[$name])) {
        return FALSE;
      }

      $definition = $properties[$name];

      if ($definition instanceof FieldDefinitionInterface) {
        $storage = $definition->getFieldStorageDefinition();
        if ($storage instanceof FieldStorageDefinitionInterface && $storage->getCardinality() !== 1) {
          return TRUE;
        }
      }
      elseif ($definition->isList()) {
        return TRUE;
      }

      $definition = $this->unwrapProperty($definition);

      if ($definition instanceof ComplexDataDefinitionInterface) {
        $properties = $definition->getPropertyDefinitions();
      }
      else {
        return FALSE;
      }
    }

    return FALSE;
  }

  /**
   * Unwraps a property definition to the shape its nested properties (if
   * any) actually live on, mirroring search_api_solr's
   * `FieldsHelper::getInnerProperty()`: a list definition (including a
   * `FieldDefinitionInterface`, which is one) unwraps to its item
   * definition, and a data-reference definition (e.g. an entity-reference
   * field's target) unwraps to its target definition. Without this, a
   * `FieldDefinitionInterface` -- which is never itself
   * `ComplexDataDefinitionInterface` -- looks like a dead end and the
   * property-path walk can never descend past it.
   */
  private function unwrapProperty(DataDefinitionInterface $property): ?DataDefinitionInterface {
    while ($property instanceof ListDataDefinitionInterface) {
      $property = $property->getItemDefinition();
    }
    while ($property instanceof DataReferenceDefinitionInterface) {
      $property = $property->getTargetDefinition();
    }
    return $property;
  }

}
