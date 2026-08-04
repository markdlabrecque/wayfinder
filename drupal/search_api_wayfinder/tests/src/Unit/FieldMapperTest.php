<?php

declare(strict_types=1);

namespace Drupal\Tests\search_api_wayfinder\Unit;

use Drupal\Core\Entity\TypedData\EntityDataDefinitionInterface;
use Drupal\Core\Field\FieldDefinitionInterface;
use Drupal\Core\Field\FieldStorageDefinitionInterface;
use Drupal\Core\Field\TypedData\FieldItemDataDefinitionInterface;
use Drupal\Core\TypedData\DataDefinitionInterface;
use Drupal\Core\TypedData\DataReferenceDefinitionInterface;
use Drupal\search_api\IndexInterface;
use Drupal\search_api\Item\FieldInterface;
use Drupal\search_api\Plugin\search_api\data_type\value\TextValue;
use Drupal\search_api_wayfinder\FieldMapper;
use PHPUnit\Framework\TestCase;

/**
 * Tests FieldMapper: SA field/type -> Wayfinder dynamic field name mapping,
 * and per-type index-time value formatting.
 *
 * Expected prefixes are copied from search_api_solr 4.4.0's
 * Utility::getDataTypeInfo() (vendor/drupal/search_api_solr/src/Utility/Utility.php,
 * lines 66-94: text=>'t', string=>'s', integer=>'it', decimal=>'ft',
 * date=>'d', boolean=>'b'), cross-checked against the dynamic_fields patterns
 * declared in presets/search-api.toml (ss_*, sm_*, ts_*, tm_*, its_*, itm_*,
 * ds_*, dm_*, bs_*, bm_*). The plan doc (docs/plans/57-search-api-wayfinder-backend.md,
 * locked decision 1) requires this exact naming.
 *
 * @coversDefaultClass \Drupal\search_api_wayfinder\FieldMapper
 * @group search_api_wayfinder
 */
class FieldMapperTest extends TestCase {

  /**
   * issue #342: fieldName() gains a 4th optional `$language` argument.
   * search_api_solr's formatSolrFieldNames()
   * (coverage/search_api_solr_4.4.0_source/src/Plugin/search_api/backend/SearchApiSolrBackend.php:2398-2538)
   * tags every field whose *prefix* starts with 't' with
   * `_X3b_<enc-lang>_` and forces the infix to 'm' regardless of
   * cardinality (:2450-2473); every other prefix is unaffected (:2474-2506).
   * `$language` defaults to 'und' (LanguageInterface::LANGCODE_NOT_SPECIFIED)
   * so non-language-aware callers keep working.
   *
   * @covers ::fieldName
   * @dataProvider fieldNameProvider
   */
  public function testFieldName(string $fieldId, string $type, bool $multiValued, string $expected, string $language = 'und'): void {
    $mapper = new FieldMapper();
    $this->assertSame($expected, $mapper->fieldName($fieldId, $type, $multiValued, $language));
  }

  public static function fieldNameProvider(): array {
    return [
      // string -> ss_ / sm_ (presets/search-api.toml lines 78-90). Prefix 's'
      // does not start with 't', so formatSolrFieldNames() never tags it with
      // a language (SearchApiSolrBackend.php:2474-2506) -- language argument
      // omitted here, defaulting to 'und', to prove that too.
      'string single' => ['field_tags', 'string', FALSE, 'ss_field_tags'],
      'string multi' => ['field_tags', 'string', TRUE, 'sm_field_tags'],
      // issue #342 testing requirement: "every non-text type unchanged by
      // language" -- an explicit non-'und' language must make no difference
      // for a prefix ('s') that doesn't start with 't'.
      'string is unaffected by a non-default language' => ['field_sku', 'string', FALSE, 'ss_field_sku', 'de'],
      // integer -> its_ / itm_ (search_api_solr prefix 'it' + s|m; preset
      // lines 135-147). 'it' does not start with 't' either -- only a prefix
      // whose *first character* is 't' qualifies (:2450 checks
      // `$prefix[0] === 't'`), so 'it*' text-lookalikes are unaffected too.
      'integer single' => ['weight', 'integer', FALSE, 'its_weight'],
      'integer multi' => ['weight', 'integer', TRUE, 'itm_weight'],
      // decimal -> fts_ / ftm_ (search_api_solr prefix 'ft' + s|m).
      'decimal single' => ['price', 'decimal', FALSE, 'fts_price'],
      'decimal multi' => ['price', 'decimal', TRUE, 'ftm_price'],
      // date -> ds_ / dm_ (preset lines 180-192).
      'date single' => ['created', 'date', FALSE, 'ds_created'],
      'date multi' => ['created', 'date', TRUE, 'dm_created'],
      // boolean -> bs_ / bm_ (preset lines 195-207; Wayfinder maps this to
      // its 'string' type server-side, but the field *name* prefix stays 'b').
      'boolean single' => ['status', 'boolean', FALSE, 'bs_status'],
      'boolean multi' => ['status', 'boolean', TRUE, 'bm_status'],
      // --- issue #300: the search_api_solr non-default data types -------
      // solr_string_storage -> zs_ / zm_, unaffected by language ('z' prefix).
      'solr_string_storage single' => ['field_notes', 'solr_string_storage', FALSE, 'zs_field_notes'],
      'solr_string_storage multi' => ['field_notes', 'solr_string_storage', TRUE, 'zm_field_notes'],
      // solr_string_docvalues -> zdvs_ / zdvm_, unaffected by language.
      'solr_string_docvalues single' => ['field_uuid', 'solr_string_docvalues', FALSE, 'zdvs_field_uuid'],
      'solr_string_docvalues multi' => ['field_uuid', 'solr_string_docvalues', TRUE, 'zdvm_field_uuid'],
      // --- issue #342: text-family prefixes (start with 't') now always ---
      // --- carry `_X3b_<enc-lang>_` and infix 'm', cardinality ignored ----
      // Plain 'text', trace-confirmed name and field id
      // (solr-ref/search-api/trace/*.json: tm_X3b_en_body appears for the
      // item-language-'en' indexing traces). Both single- and multi-valued
      // 'body' produce the SAME name -- proving cardinality is ignored
      // (SearchApiSolrBackend.php:2450-2473, "$pref .= 'm' . SEPARATOR
      // . $language_id" runs unconditionally, no single/multi branch).
      'text single, language en' => ['body', 'text', FALSE, 'tm_X3b_en_body', 'en'],
      'text multi, language en' => ['body', 'text', TRUE, 'tm_X3b_en_body', 'en'],
      // Default language is 'und' when the caller passes none -- also
      // trace-confirmed (tm_X3b_und_title appears query-time in
      // solr-ref/search-api/trace/*.json for a multi-language site with no
      // language condition). 4-tuple: $language omitted, exercising the
      // 'und' default.
      'text default language is und' => ['title', 'text', FALSE, 'tm_X3b_und_title'],
      // de-AT: the one pair that proves _X3b_ encoding and
      // solr_text_spellcheck's own encoding genuinely differ. encodeSolrName()
      // replaces every non-[a-zA-Z0-9_] byte with 'X' + lowercase hex: '-' ->
      // 'X2d' (SearchApiSolrBackend.php:2466-2468 spells this exact example
      // out: "de-AT" -> "de_X2d_AT" inside the encoded language segment).
      'text with de-AT encodes the hyphen as X2d' => ['body', 'text', FALSE, 'tm_X3b_de_X2d_AT_body', 'de-AT'],
      // solr_text_unstemmed/_omit_norms/_wstoken ('tu'/'to'/'tw' all start
      // with 't') -> same _X3b_<lang>_ + forced 'm' infix treatment as plain
      // text (SearchApiSolrBackend.php:2450-2473 keys off the prefix's first
      // character, not the type name).
      'solr_text_unstemmed, language en' => ['title', 'solr_text_unstemmed', FALSE, 'tum_X3b_en_title', 'en'],
      'solr_text_omit_norms, language en' => ['title', 'solr_text_omit_norms', FALSE, 'tom_X3b_en_title', 'en'],
      'solr_text_wstoken, language en' => ['title', 'solr_text_wstoken', FALSE, 'twm_X3b_en_title', 'en'],
      // solr_text_suggester -> the FIXED sink field 'twm_suggest', regardless
      // of field id, cardinality, OR language. search_api_solr special-cases
      // this type before its generic prefix logic
      // (SearchApiSolrBackend.php:2433-2437): every solr_text_suggester field
      // indexes into the one field the SuggestComponent reads.
      'solr_text_suggester single' => ['field_suggest', 'solr_text_suggester', FALSE, 'twm_suggest'],
      'solr_text_suggester multi' => ['field_suggest', 'solr_text_suggester', TRUE, 'twm_suggest'],
      'solr_text_suggester is unaffected by a non-default language' => ['field_suggest', 'solr_text_suggester', FALSE, 'twm_suggest', 'de'],
      // solr_text_spellcheck -> 'spellcheck_' . str_replace('-', '_',
      // $language) -- explicitly NOT the _X3b_ encoding every other
      // text-family type gets (SearchApiSolrBackend.php:2440-2446, "Don't use
      // the language separator here!"). Field id and cardinality play no
      // part at all: the language alone determines the sink.
      'solr_text_spellcheck, language en' => ['field_x', 'solr_text_spellcheck', FALSE, 'spellcheck_en', 'en'],
      'solr_text_spellcheck with de-AT uses an underscore, not X3b' => ['field_x', 'solr_text_spellcheck', FALSE, 'spellcheck_de_AT', 'de-AT'],
      'solr_text_spellcheck is unaffected by field id' => ['field_y', 'solr_text_spellcheck', TRUE, 'spellcheck_en', 'en'],
    ];
  }

  /**
   * issue #342, MF-2 (round-2 review bounce): the hyphen-to-underscore
   * transform `fieldName()`'s `solr_text_spellcheck` branch applies
   * (`'spellcheck_' . str_replace('-', '_', $language)`) must live in its own
   * method, `spellcheckDictionary()`, so `QueryBuilder`'s
   * `spellcheck.dictionary` param can call the SAME transform rather than
   * sending the raw langcode -- see QueryBuilderTest's
   * testSpellcheckDictionaryTransformsAHyphenatedLanguageLikeTheIndexedSink()
   * for why the two sides silently disagreeing today is a real bug.
   * `spellcheckDictionary()` itself returns just the transformed language
   * ('de_AT'), not the full sink name -- `fieldName()`'s branch still
   * prepends 'spellcheck_' itself, exercised directly by the next test.
   *
   * @covers ::spellcheckDictionary
   */
  public function testSpellcheckDictionaryTransformsHyphenToUnderscore(): void {
    $mapper = new FieldMapper();
    $this->assertSame('de_AT', $mapper->spellcheckDictionary('de-AT'));
  }

  /**
   * issue #342, MF-2 (round-2 review bounce): pins that `fieldName()`'s
   * `solr_text_spellcheck` sink name is built FROM `spellcheckDictionary()`
   * (`'spellcheck_' . $this->spellcheckDictionary($language)`), not a
   * second, independent copy of the same transform -- so the two can never
   * drift apart again the way index-time and query-time did before this
   * fix.
   *
   * @covers ::spellcheckDictionary
   * @covers ::fieldName
   */
  public function testFieldNameSpellcheckSinkAgreesWithSpellcheckDictionary(): void {
    $mapper = new FieldMapper();
    $this->assertSame(
      'spellcheck_' . $mapper->spellcheckDictionary('de-AT'),
      $mapper->fieldName('field_x', 'solr_text_spellcheck', FALSE, 'de-AT')
    );
  }

  /**
   * issue #342 testing requirement: "the full trace-derived name list above,
   * as a single regression case pinning the module's output against real
   * captured client behaviour." Field id/type/cardinality pairs and their
   * expected mapped names are read directly from
   * solr-ref/search-api/trace/*.json (indexing traces 00001, 00010-00017,
   * 00024, 00028 for the single-language 'en' names; query-time traces
   * 00002-00009/00021/00022 additionally show the 'und' text names for a
   * multi-language site with no language condition).
   *
   * @covers ::fieldName
   * @dataProvider traceDerivedFieldNameProvider
   */
  public function testFieldNameMatchesTraceDerivedNames(string $fieldId, string $type, bool $multiValued, string $expected, string $language = 'und'): void {
    $mapper = new FieldMapper();
    $this->assertSame($expected, $mapper->fieldName($fieldId, $type, $multiValued, $language));
  }

  public static function traceDerivedFieldNameProvider(): array {
    return [
      'trace: field_archived (boolean)' => ['field_archived', 'boolean', FALSE, 'bs_field_archived'],
      'trace: field_featured (boolean)' => ['field_featured', 'boolean', FALSE, 'bs_field_featured'],
      'trace: sticky (boolean)' => ['sticky', 'boolean', FALSE, 'bs_sticky'],
      'trace: created (date)' => ['created', 'date', FALSE, 'ds_created'],
      'trace: field_event_date (date)' => ['field_event_date', 'date', FALSE, 'ds_field_event_date'],
      'trace: field_published_on (date)' => ['field_published_on', 'date', FALSE, 'ds_field_published_on'],
      'trace: field_priority (integer)' => ['field_priority', 'integer', FALSE, 'its_field_priority'],
      'trace: field_rating (integer)' => ['field_rating', 'integer', FALSE, 'its_field_rating'],
      'trace: nid (integer)' => ['nid', 'integer', FALSE, 'its_nid'],
      'trace: context_tags (string, multi)' => ['context_tags', 'string', TRUE, 'sm_context_tags'],
      'trace: field_keywords (string, multi)' => ['field_keywords', 'string', TRUE, 'sm_field_keywords'],
      'trace: field_topics (string, multi)' => ['field_topics', 'string', TRUE, 'sm_field_topics'],
      'trace: field_sku (string)' => ['field_sku', 'string', FALSE, 'ss_field_sku'],
      'trace: search_api_datasource (string)' => ['search_api_datasource', 'string', FALSE, 'ss_search_api_datasource'],
      'trace: search_api_id (string)' => ['search_api_id', 'string', FALSE, 'ss_search_api_id'],
      'trace: search_api_language (string)' => ['search_api_language', 'string', FALSE, 'ss_search_api_language'],
      'trace: type (string)' => ['type', 'string', FALSE, 'ss_type'],
      // Text fields, item language 'en' (indexing traces): cardinality is
      // irrelevant to the name (see fieldNameProvider above), so a single
      // 'FALSE' representative per field id is enough to pin the trace name.
      'trace: body (text, en)' => ['body', 'text', FALSE, 'tm_X3b_en_body', 'en'],
      'trace: title (text, en)' => ['title', 'text', FALSE, 'tm_X3b_en_title', 'en'],
      // Text fields, query-time on a multi-language site with no
      // search_api_language condition -> language resolves to 'und'.
      'trace: body (text, und)' => ['body', 'text', FALSE, 'tm_X3b_und_body'],
      'trace: title (text, und)' => ['title', 'text', FALSE, 'tm_X3b_und_title'],
    ];
  }

  /**
   * issue #342: sortFieldName() gains the same optional `$language`
   * argument as fieldName(). A text-type field sorts through
   * `sort_X3b_<enc-lang>_<id>` (encodeSolrName('sort' . SEPARATOR
   * . $sort_language_id . '_' . $name), :1483); every other type is
   * unaffected and keeps sorting on its ordinary mapped field name (current
   * FieldMapper::sortFieldName() behaviour, unchanged for non-text).
   *
   * @covers ::sortFieldName
   * @dataProvider sortFieldNameProvider
   */
  public function testSortFieldName(string $fieldId, string $type, bool $multiValued, string $expected, string $language = 'und'): void {
    $mapper = new FieldMapper();
    $this->assertSame($expected, $mapper->sortFieldName($fieldId, $type, $multiValued, $language));
  }

  public static function sortFieldNameProvider(): array {
    return [
      'text sort field carries the language, en' => ['title', 'text', FALSE, 'sort_X3b_en_title', 'en'],
      'text sort field defaults to und' => ['title', 'text', FALSE, 'sort_X3b_und_title'],
      // Non-text: unaffected by language, unchanged from today -- the mapped
      // field name itself (current FieldMapper::sortFieldName() behaviour).
      'non-text sort field is the mapped field name, unaffected by language' => ['weight', 'integer', FALSE, 'its_weight', 'en'],
    ];
  }

  /**
   * Builds a FieldInterface whose index property definition is shaped like a
   * real Drupal content-entity field: a FieldDefinitionInterface that is
   * list-by-construction (isList() unconditionally TRUE, matching
   * BaseFieldDefinition/FieldConfigBase in real core) wrapping a
   * FieldStorageDefinitionInterface with the given cardinality.
   *
   * This is the mock shape issue #81 says the old bare-DataDefinitionInterface
   * mocks could not see: isList() alone can't distinguish single from
   * multi-valued once every real field returns TRUE for it. The correct
   * signal is field-storage cardinality.
   */
  private function mockFieldWithStorageCardinality(string $propertyPath, int $cardinality): FieldInterface {
    $storage = $this->createMock(FieldStorageDefinitionInterface::class);
    $storage->method('getCardinality')->willReturn($cardinality);

    $definition = $this->createMock(FieldDefinitionInterface::class);
    $definition->method('isList')->willReturn(TRUE);
    $definition->method('getFieldStorageDefinition')->willReturn($storage);

    $index = $this->createMock(IndexInterface::class);
    $index->method('getPropertyDefinitions')->willReturn([$propertyPath => $definition]);

    $field = $this->createMock(FieldInterface::class);
    $field->method('getPropertyPath')->willReturn($propertyPath);
    $field->method('getDatasourceId')->willReturn('entity:node');
    $field->method('getIndex')->willReturn($index);

    return $field;
  }

  /**
   * Regression test for issue #81: a property that is list-by-construction
   * (isList() TRUE, matching every real content-entity field) but whose
   * field-storage cardinality is 1 must resolve to single-valued. The old
   * isList()-only implementation gets this wrong -- it would classify this
   * as multi-valued because isList() is TRUE.
   *
   * @covers ::isMultiValued
   */
  public function testIsMultiValuedIsFalseForListByConstructionFieldWithCardinalityOne(): void {
    $mapper = new FieldMapper();
    $field = $this->mockFieldWithStorageCardinality('title', 1);

    $this->assertFalse($mapper->isMultiValued($field));
  }

  /**
   * @covers ::isMultiValued
   */
  public function testIsMultiValuedIsTrueForUnlimitedCardinality(): void {
    $mapper = new FieldMapper();
    $field = $this->mockFieldWithStorageCardinality('field_tags', FieldStorageDefinitionInterface::CARDINALITY_UNLIMITED);

    $this->assertTrue($mapper->isMultiValued($field));
  }

  /**
   * @covers ::isMultiValued
   */
  public function testIsMultiValuedIsTrueForCardinalityGreaterThanOne(): void {
    $mapper = new FieldMapper();
    $field = $this->mockFieldWithStorageCardinality('field_authors', 3);

    $this->assertTrue($mapper->isMultiValued($field));
  }

  /**
   * Regression test for issue #81 round 2: a multi-segment property path
   * (e.g. `body:value`) must actually descend past the top-level field
   * definition to score the leaf. `FieldDefinitionInterface` is a
   * `ListDataDefinitionInterface`, not a `ComplexDataDefinitionInterface`, so
   * the walk has to unwrap it (via `getItemDefinition()`) before it can see
   * the item definition's own nested properties -- a walk that just checks
   * `instanceof ComplexDataDefinitionInterface` on the raw field definition
   * dead-ends after segment 1 and would wrongly return single-valued only
   * because nothing further was scored (never observing this specific
   * `body` field's real cardinality).
   *
   * Single-cardinality field ('body', cardinality 1) whose item definition
   * is complex and contains a single-valued 'value' sub-property -> the
   * whole path resolves single-valued.
   *
   * @covers ::isMultiValued
   */
  public function testIsMultiValuedDescendsMultiSegmentPathAndResolvesSingle(): void {
    $valueProperty = $this->createMock(DataDefinitionInterface::class);
    $valueProperty->method('isList')->willReturn(FALSE);

    $itemDefinition = $this->createMock(FieldItemDataDefinitionInterface::class);
    $itemDefinition->method('getPropertyDefinitions')->willReturn(['value' => $valueProperty]);

    $storage = $this->createMock(FieldStorageDefinitionInterface::class);
    $storage->method('getCardinality')->willReturn(1);

    $fieldDefinition = $this->createMock(FieldDefinitionInterface::class);
    $fieldDefinition->method('isList')->willReturn(TRUE);
    $fieldDefinition->method('getFieldStorageDefinition')->willReturn($storage);
    $fieldDefinition->method('getItemDefinition')->willReturn($itemDefinition);

    $index = $this->createMock(IndexInterface::class);
    $index->method('getPropertyDefinitions')->willReturn(['body' => $fieldDefinition]);

    $field = $this->createMock(FieldInterface::class);
    $field->method('getPropertyPath')->willReturn('body:value');
    $field->method('getDatasourceId')->willReturn('entity:node');
    $field->method('getIndex')->willReturn($index);

    $mapper = new FieldMapper();
    $this->assertFalse($mapper->isMultiValued($field));
  }

  /**
   * Regression test for issue #81 round 2 -- the exact data-loss case: a
   * single-cardinality reference field (e.g. `field_ref`) whose *target*
   * entity's property (`field_tags`) is unlimited-cardinality. The whole
   * path must resolve multi-valued, driven by the referenced field's real
   * cardinality, not the referencing field's cardinality of 1.
   *
   * Path: field_ref:entity:field_tags.
   *   - field_ref: FieldDefinitionInterface, storage cardinality 1
   *     (single-valued reference), item definition is complex and has an
   *     'entity' property that is a DataReferenceDefinitionInterface.
   *   - entity: unwraps (getTargetDefinition()) to an
   *     EntityDataDefinitionInterface (complex), which has a 'field_tags'
   *     property.
   *   - field_tags: FieldDefinitionInterface, storage cardinality -1
   *     (unlimited) -> multi-valued.
   *
   * DocumentBuilder::buildAddCommand() does
   * `$multiValued ? array_values($formatted) : ($formatted[0] ?? NULL)`, so
   * failing to reach this leaf and wrongly resolving single-valued would
   * silently drop every tag but the first -- worse than the original #81
   * bug, which was lossy only in field-name prefix, not data.
   *
   * @covers ::isMultiValued
   */
  public function testIsMultiValuedDescendsThroughReferenceToMultiValuedTarget(): void {
    $tagsStorage = $this->createMock(FieldStorageDefinitionInterface::class);
    $tagsStorage->method('getCardinality')->willReturn(FieldStorageDefinitionInterface::CARDINALITY_UNLIMITED);

    $tagsFieldDefinition = $this->createMock(FieldDefinitionInterface::class);
    $tagsFieldDefinition->method('isList')->willReturn(TRUE);
    $tagsFieldDefinition->method('getFieldStorageDefinition')->willReturn($tagsStorage);

    $targetEntityDefinition = $this->createMock(EntityDataDefinitionInterface::class);
    $targetEntityDefinition->method('getPropertyDefinitions')->willReturn(['field_tags' => $tagsFieldDefinition]);

    $entityProperty = $this->createMock(DataReferenceDefinitionInterface::class);
    $entityProperty->method('isList')->willReturn(FALSE);
    $entityProperty->method('getTargetDefinition')->willReturn($targetEntityDefinition);

    $refItemDefinition = $this->createMock(FieldItemDataDefinitionInterface::class);
    $refItemDefinition->method('getPropertyDefinitions')->willReturn(['entity' => $entityProperty]);

    $refStorage = $this->createMock(FieldStorageDefinitionInterface::class);
    $refStorage->method('getCardinality')->willReturn(1);

    $refFieldDefinition = $this->createMock(FieldDefinitionInterface::class);
    $refFieldDefinition->method('isList')->willReturn(TRUE);
    $refFieldDefinition->method('getFieldStorageDefinition')->willReturn($refStorage);
    $refFieldDefinition->method('getItemDefinition')->willReturn($refItemDefinition);

    $index = $this->createMock(IndexInterface::class);
    $index->method('getPropertyDefinitions')->willReturn(['field_ref' => $refFieldDefinition]);

    $field = $this->createMock(FieldInterface::class);
    $field->method('getPropertyPath')->willReturn('field_ref:entity:field_tags');
    $field->method('getDatasourceId')->willReturn('entity:node');
    $field->method('getIndex')->willReturn($index);

    $mapper = new FieldMapper();
    $this->assertTrue($mapper->isMultiValued($field));
  }

  /**
   * @covers ::formatValue
   * @dataProvider formatValueProvider
   */
  public function testFormatValue(string $type, $value, $expected): void {
    $mapper = new FieldMapper();
    $this->assertSame($expected, $mapper->formatValue($value, $type));
  }

  public static function formatValueProvider(): array {
    return [
      // Dates: Unix timestamp (Search API's internal representation for the
      // 'date' type) -> ISO 8601 UTC, per plan doc line 154-155.
      'date epoch zero' => ['date', 0, '1970-01-01T00:00:00Z'],
      'date epoch nonzero' => ['date', 1700000000, '2023-11-14T22:13:20Z'],
      // Booleans: literal "true"/"false" strings (plan doc line 155-157,
      // presets/search-api.toml header comment lines 14-19: Wayfinder has no
      // boolean type, search_api_solr already sends these as JSON strings).
      'boolean true' => ['boolean', TRUE, 'true'],
      'boolean false' => ['boolean', FALSE, 'false'],
      // Text/string: passed through as-is.
      'string as-is' => ['string', 'Some Value', 'Some Value'],
      'text as-is' => ['text', 'Some fulltext body', 'Some fulltext body'],
      // Numerics: bare, no quoting/formatting.
      'integer bare' => ['integer', 42, 42],
      'decimal bare' => ['decimal', 3.14, 3.14],
      // issue #300: the solr_text_* variants format values exactly like plain
      // 'text' -- search_api_solr normalises any 'solr_text_*' to 'text'
      // before formatting (SearchApiSolrBackend.php:2706-2708), and
      // FieldMapper::isTextType() mirrors that. A plain-string solr_text_*
      // value therefore passes through unchanged.
      'solr_text_unstemmed as-is' => ['solr_text_unstemmed', 'Some fulltext body', 'Some fulltext body'],
      // solr_string_* variants format like 'string': passthrough.
      'solr_string_storage as-is' => ['solr_string_storage', 'Stored only', 'Stored only'],
      'solr_string_docvalues as-is' => ['solr_string_docvalues', 'DocValues', 'DocValues'],
    ];
  }

  /**
   * Regression test for issue #83: real Search API hands `text`-type field
   * values as \Drupal\search_api\Plugin\search_api\data_type\value\TextValue
   * objects (see TextDataType::getValue()), not plain PHP strings -- the
   * `formatValue()` default branch returning $value untouched means
   * json_encode() of the returned TextValue serializes to '{}' (all
   * properties are `protected`, class is not JsonSerializable), which is
   * exactly the malformed body that broke real indexing in the #80
   * integration harness ("field tm_body expects a string value, got {}").
   *
   * Using the real TextValue class here (not a mock): its constructor takes
   * only a plain string and it has no other collaborators, so it is
   * constructible under bare PHPUnit with no Drupal bootstrap, and using the
   * real class (rather than a hand-rolled stub) pins the assertion to the
   * actual __toString()/toText() contract rather than to what a mock author
   * assumed that contract does.
   *
   * @covers ::formatValue
   */
  public function testFormatValueCastsTextValueObjectToPlainString(): void {
    $mapper = new FieldMapper();
    $value = new TextValue('Some fulltext body');

    $result = $mapper->formatValue($value, 'text');

    $this->assertIsString($result, 'formatValue() must return a plain string for text-type TextValue input, not the object itself.');
    $this->assertSame('Some fulltext body', $result);
    // Guard against a superficial fix that stringifies to the wrong thing
    // (e.g. a cast that hits get_object_vars() or similar) -- '{}' is the
    // exact malformed shape json_encode() produces for a bare TextValue.
    $this->assertNotSame('{}', $result);
  }

  /**
   * A TextValue's rendered text can itself change after tokenization
   * (toText() re-joins from tokens if any are set, per TextValue::toText());
   * formatValue() must reflect whatever __toString() currently produces, not
   * just the value the object was constructed with.
   *
   * @covers ::formatValue
   */
  public function testFormatValueUsesTextValueCurrentStringRepresentation(): void {
    $mapper = new FieldMapper();
    $value = new TextValue('original text');
    $value->setText('mutated text');

    $result = $mapper->formatValue($value, 'text');

    $this->assertSame('mutated text', $result);
  }

  /**
   * issue #300: a solr_text_* variant must hit the SAME text branch as plain
   * 'text', so a TextValue object is cast to a plain string -- not left as
   * an object. Without isTextType() covering the solr_text_* class, a
   * solr_text_unstemmed TextValue falls through formatValue()'s default
   * branch untouched and json_encode() serialises it to '{}', the exact
   * malformed-body regression #83 fixed for plain 'text'.
   *
   * @covers ::formatValue
   * @dataProvider solrTextTypeProvider
   */
  public function testFormatValueCastsTextValueObjectForSolrTextVariants(string $type): void {
    $mapper = new FieldMapper();
    $value = new TextValue('Some fulltext body');

    $result = $mapper->formatValue($value, $type);

    $this->assertIsString($result, "formatValue() must return a plain string for {$type} TextValue input.");
    $this->assertSame('Some fulltext body', $result);
    $this->assertNotSame('{}', $result);
  }

  public static function solrTextTypeProvider(): array {
    return [
      'solr_text_unstemmed' => ['solr_text_unstemmed'],
      'solr_text_omit_norms' => ['solr_text_omit_norms'],
      'solr_text_wstoken' => ['solr_text_wstoken'],
      'solr_text_suggester' => ['solr_text_suggester'],
    ];
  }

  /**
   * @covers ::filterValue
   * @dataProvider filterValueProvider
   */
  public function testFilterValue(string $type, $value, string $expected): void {
    $mapper = new FieldMapper();
    $this->assertSame($expected, $mapper->filterValue($value, $type));
  }

  public static function filterValueProvider(): array {
    return [
      // Text/string/boolean are phrase-quoted; inside the phrase only a
      // literal backslash or double quote is escaped.
      'text quoted' => ['text', 'foo', '"foo"'],
      'text escapes quote' => ['text', 'a"b', '"a\\"b"'],
      'text escapes backslash' => ['text', 'a\\b', '"a\\\\b"'],
      'string quoted' => ['string', 'foo bar', '"foo bar"'],
      'boolean quoted' => ['boolean', TRUE, '"true"'],
      // Numeric/date stay bare after their normal formatting.
      'integer bare' => ['integer', 42, '42'],
      'decimal bare' => ['decimal', 3.14, '3.14'],
      'date bare' => ['date', 0, '1970-01-01T00:00:00Z'],
      // issue #300: solr_text_* variants phrase-quote exactly like plain
      // 'text' (isTextType() covers the whole class). A solr_text_* filter
      // value that skipped the phrase branch would be emitted bare,
      // breaking Lucene phrase semantics for these fields.
      'solr_text_unstemmed quoted' => ['solr_text_unstemmed', 'foo', '"foo"'],
      'solr_text_suggester quoted' => ['solr_text_suggester', 'foo', '"foo"'],
      // solr_string_* variants phrase-quote like 'string'.
      'solr_string_storage quoted' => ['solr_string_storage', 'foo bar', '"foo bar"'],
      'solr_string_docvalues quoted' => ['solr_string_docvalues', 'foo', '"foo"'],
    ];
  }

}
