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
   * @covers ::fieldName
   * @dataProvider fieldNameProvider
   */
  public function testFieldName(string $fieldId, string $type, bool $multiValued, string $expected): void {
    $mapper = new FieldMapper();
    $this->assertSame($expected, $mapper->fieldName($fieldId, $type, $multiValued));
  }

  public static function fieldNameProvider(): array {
    return [
      // string -> ss_ / sm_ (presets/search-api.toml lines 78-90).
      'string single' => ['field_tags', 'string', FALSE, 'ss_field_tags'],
      'string multi' => ['field_tags', 'string', TRUE, 'sm_field_tags'],
      // text -> ts_ / tm_ (presets/search-api.toml lines 93-103).
      'text single' => ['title', 'text', FALSE, 'ts_title'],
      'text multi' => ['title', 'text', TRUE, 'tm_title'],
      // integer -> its_ / itm_ (search_api_solr prefix 'it' + s|m; preset lines 135-147).
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
    ];
  }

}
