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
 * date-range type), solr_text_custom* (site-defined analyzer escape hatch),
 * location/rpt (#292).
 *
 * issue #342 made the naming language-aware, mirroring search_api_solr's
 * formatSolrFieldNames()
 * (SearchApiSolrBackend.php:2398-2538): every type whose *prefix* starts with
 * 't' is named '<prefix>m_X3b_<enc-lang>_<fieldId>' -- the infix is always 'm'
 * and the cardinality is ignored (:2450-2473) -- while every other prefix
 * keeps its plain '<prefix><s|m>_<fieldId>' name (:2474-2506). That is a
 * breaking rename of every text field on the wire; see README.
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
   * The language separator search_api_solr puts between a text field's infix
   * and its language id, before encoding:
   * SolrBackendInterface::SEARCH_API_SOLR_LANGUAGE_SEPARATOR is ';', which
   * encodeSolrName() turns into 'X3b' (SearchApiSolrBackend.php:2470,
   * '$pref .= "m" . SEPARATOR . $language_id'). SolrBackendInterface is not
   * vendored in coverage/, so the constant's value is pinned here from the
   * captured client traces instead: every text field name in
   * solr-ref/search-api/trace/*.json reads 'tm_X3b_<lang>_<field id>'.
   */
  private const LANGUAGE_SEPARATOR = ';';

  /**
   * The language id used when a caller names none:
   * LanguageInterface::LANGCODE_NOT_SPECIFIED. This is also the language
   * search_api_solr's own facet path resolves to
   * (getLanguageSpecificSolrFieldNames(LANGCODE_NOT_SPECIFIED, ...),
   * SearchApiSolrBackend.php:2582-2585).
   */
  public const LANGUAGE_UNSPECIFIED = 'und';

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
   *
   * `$language` defaults to 'und' so callers with no language in hand (the
   * facet path, and every pre-#342 call site) keep working: 'und' is
   * search_api_solr's own language-unspecific choice
   * (SearchApiSolrBackend.php:2582-2585).
   */
  public function fieldName(string $fieldId, string $type, bool $multiValued, string $language = self::LANGUAGE_UNSPECIFIED): string {
    // solr_text_suggester is one of the two types that do NOT follow the
    // prefix+infix dynamic-field convention: every field of this type indexes
    // into the fixed sink field the SuggestComponent reads, regardless of id,
    // cardinality or language. See SUGGESTER_SINK_FIELD; search_api_solr
    // special-cases it at SearchApiSolrBackend.php:2433-2437, before both the
    // generic prefix logic and encodeSolrName().
    if ($type === 'solr_text_suggester') {
      return self::SUGGESTER_SINK_FIELD;
    }

    // solr_text_spellcheck is the other one: a fixed per-language sink, whose
    // name is deliberately NOT built with the language separator or run
    // through encodeSolrName() -- SearchApiSolrBackend.php:2440-2446, "Don't
    // use the language separator here! This field name is used without in
    // solrconfig.xml." A hyphen in the langcode becomes an underscore, so
    // 'de-AT' gives 'spellcheck_de_AT' where a text field gives
    // 'tm_X3b_de_X2d_AT_*'.
    if ($type === 'solr_text_spellcheck') {
      return 'spellcheck_' . str_replace('-', '_', $language);
    }

    $prefix = self::TYPE_PREFIXES[$type] ?? $type;
    if ($this->isTextPrefix($prefix)) {
      // Every text type is multi-valued on the wire and language-tagged:
      // "$pref .= 'm' . SEPARATOR . $language_id" runs unconditionally, with
      // no single/multi branch at all (SearchApiSolrBackend.php:2450-2473),
      // because Search API processors can produce several boosted string
      // tokens for one single-valued Drupal field.
      $infix = 'm' . self::LANGUAGE_SEPARATOR . $language;
    }
    else {
      $infix = $multiValued ? 'm' : 's';
    }

    // The whole assembled name is encoded, exactly as upstream does at
    // SearchApiSolrBackend.php:2504-2505 ("$name = $pref . '_' .
    // $search_api_name; ... Utility::encodeSolrName($name)").
    return $this->encodeSolrName($prefix . $infix . '_' . $fieldId);
  }

  /**
   * Whether a Search API field type is named language-specifically, i.e. maps
   * to one '<prefix>m_X3b_<lang>_<id>' name per language rather than a single
   * language-free name.
   *
   * This is the naming question, deliberately NOT the value-formatting one
   * isTextType() answers: the two fixed sinks (solr_text_suggester,
   * solr_text_spellcheck) format their values as text but are named by
   * SearchApiSolrBackend.php:2433-2446's earlier special cases, and their
   * type names carry no 't' prefix in TYPE_PREFIXES, so they answer FALSE
   * here.
   */
  public function isLanguageSpecificTextType(string $type): bool {
    return $this->isTextPrefix(self::TYPE_PREFIXES[$type] ?? $type);
  }

  /**
   * Whether a type prefix belongs to the text family, which is what decides
   * the language-tagged 'm' infix.
   *
   * search_api_solr's test is on the *prefix*, not the type name:
   * "strpos($pref, 't') === 0" (SearchApiSolrBackend.php:2450). Derived from
   * TYPE_PREFIXES rather than a hand-written type list so a future 't*' type
   * is covered automatically -- and so 'it' (integer) and the two 's'-prefixed
   * fixed sinks are correctly left out, since only the prefix's FIRST
   * character counts.
   */
  private function isTextPrefix(string $prefix): bool {
    return str_starts_with($prefix, 't');
  }

  /**
   * Encodes a Solr field name, mirroring search_api_solr's
   * Utility::encodeSolrName(): every character outside [a-zA-Z0-9_] is
   * replaced by '_X' + the lowercase hex of its byte + '_', so the encoded
   * run stays a separate underscore-delimited segment of the name.
   *
   * Utility is not vendored in coverage/, so the rule is pinned from the
   * captured client traces rather than copied from the method
   * (solr-ref/search-api/trace/*.json):
   * - ';' (SEARCH_API_SOLR_LANGUAGE_SEPARATOR) -> '_X3b_', which is why
   *   'tm' + 'm;en_body' reads 'tm_X3b_en_body' there;
   * - '/' -> '_X2f_' and ':' -> '_X3a_', which is why the index id
   *   'search_api/index:capture_index' reads
   *   'search_api_X2f_index_X3a_capture_index' there;
   * - '-' -> '_X2d_', the case SearchApiSolrBackend.php:2466-2468 spells out
   *   itself: 'de-AT' gives 'tm_X3b_de_X2d_AT_*'.
   */
  private function encodeSolrName(string $name): string {
    return (string) preg_replace_callback(
      '/[^a-zA-Z0-9_]/',
      static fn (array $match): string => '_X' . bin2hex($match[0]) . '_',
      $name
    );
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
   *
   * issue #342: the text sort field is language-specific too --
   * encodeSolrName('sort' . SEPARATOR . $sort_language_id . '_' . $name),
   * SearchApiSolrBackend.php:1483 -- so 'title' in English sorts on
   * 'sort_X3b_en_title'. Non-text types are unaffected: their sort field IS
   * their mapped field, which carries no language.
   */
  public function sortFieldName(string $fieldId, string $type, bool $multiValued, string $language = self::LANGUAGE_UNSPECIFIED): string {
    return $this->isTextPrefix(self::TYPE_PREFIXES[$type] ?? $type)
      ? $this->encodeSolrName('sort' . self::LANGUAGE_SEPARATOR . $language . '_' . $fieldId)
      : $this->fieldName($fieldId, $type, $multiValued, $language);
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
