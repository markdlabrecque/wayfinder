<?php

declare(strict_types=1);

namespace Drupal\Tests\search_api_wayfinder\Unit;

use Drupal\Core\Field\FieldDefinitionInterface;
use Drupal\Core\Field\FieldStorageDefinitionInterface;
use Drupal\search_api\IndexInterface;
use Drupal\search_api\Item\FieldInterface;
use Drupal\search_api\Item\ItemInterface;
use Drupal\search_api\Plugin\search_api\data_type\value\TextValue;
use Drupal\search_api_wayfinder\DocumentBuilder;
use Drupal\search_api_wayfinder\FieldMapper;
use PHPUnit\Framework\TestCase;

/**
 * Tests DocumentBuilder: Search API item -> Solr "add" update command.
 *
 * Command shape ({"add": {"doc": {...}}}) and static fields (id, index_id,
 * ss_search_api_language, ss_search_api_datasource) are per plan doc
 * (docs/plans/57-search-api-wayfinder-backend.md, "Indexing translation"
 * section, lines 181-191) and locked decision 2 (document id is
 * "{index_id}-{item_id}", no site hash for M1 -- ponytail noted there).
 * update_add_commit.json / update_add_nocommit.json in solr-ref/responses/
 * confirm Wayfinder accepts the command-object POST form; those fixtures
 * don't capture doc *shape* (bodies aren't echoed back), so per-field
 * expectations here are derived from FieldMapper's contract instead.
 *
 * @coversDefaultClass \Drupal\search_api_wayfinder\DocumentBuilder
 * @group search_api_wayfinder
 */
class DocumentBuilderTest extends TestCase {

  private function mockField(string $id, string $type, array $values, bool $multiValued = FALSE): FieldInterface {
    $field = $this->createMock(FieldInterface::class);
    $field->method('getFieldIdentifier')->willReturn($id);
    $field->method('getType')->willReturn($type);
    $field->method('getValues')->willReturn($values);
    $field->method('getPropertyPath')->willReturn($id);
    $field->method('getDatasourceId')->willReturn('entity:test');

    // FieldMapper::isMultiValued() reads cardinality from the index's own
    // property-path definitions, not from how many values this item happens
    // to carry -- see FieldMapper's doc comment. The mock shape here is
    // deliberately realistic (issue #81): a real content-entity field is
    // list-by-construction (isList() unconditionally TRUE, matching
    // BaseFieldDefinition/FieldConfigBase in core) -- the single/multi
    // signal actually lives in field-storage cardinality, not isList().
    $storage = $this->createMock(FieldStorageDefinitionInterface::class);
    $storage->method('getCardinality')->willReturn(
      $multiValued ? FieldStorageDefinitionInterface::CARDINALITY_UNLIMITED : 1
    );

    $definition = $this->createMock(FieldDefinitionInterface::class);
    $definition->method('isList')->willReturn(TRUE);
    $definition->method('getFieldStorageDefinition')->willReturn($storage);

    $index = $this->createMock(IndexInterface::class);
    $index->method('getPropertyDefinitions')->willReturn([$id => $definition]);
    $field->method('getIndex')->willReturn($index);

    return $field;
  }

  private function mockItem(string $itemId, string $datasourceId, string $language, array $fields): ItemInterface {
    $item = $this->createMock(ItemInterface::class);
    $item->method('getId')->willReturn($itemId);
    $item->method('getDatasourceId')->willReturn($datasourceId);
    $item->method('getLanguage')->willReturn($language);
    $item->method('getFields')->willReturn($fields);
    return $item;
  }

  /**
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandShapeAndStaticFields(): void {
    // Regression for issue #83: real Search API hands `text`-type field
    // values as TextValue objects (TextDataType::getValue()), never plain
    // PHP strings -- a mock that passes a bare string here (as this test
    // did before #83) can't see FieldMapper::formatValue() failing to
    // stringify the object, because a plain string is already a string.
    $item = $this->mockItem('node/1:en', 'entity:node', 'en', [
      'title' => $this->mockField('title', 'text', [new TextValue('Hello world')]),
    ]);

    $builder = new DocumentBuilder(new FieldMapper());
    $command = $builder->buildAddCommand($item, 'my_index');

    $this->assertArrayHasKey('add', $command);
    $this->assertArrayHasKey('doc', $command['add']);
    $doc = $command['add']['doc'];

    // Document id = "{index_id}-{item_id}" (locked decision 2).
    $this->assertSame('my_index-node/1:en', $doc['id']);
    $this->assertSame('my_index', $doc['index_id']);
    $this->assertSame('en', $doc['ss_search_api_language']);
    $this->assertSame('entity:node', $doc['ss_search_api_datasource']);
    $this->assertSame('Hello world', $doc['ts_title']);

    // commitWithin is NOT part of this command: Wayfinder's /update parser
    // only reads add.doc from the body -- commitWithin is a query param on
    // POST /update, sent by WayfinderClient::update(), not embedded here.
    $this->assertArrayNotHasKey('commitWithin', $command['add']);
  }

  /**
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandSingleValuedFieldIsScalar(): void {
    $item = $this->mockItem('node/3:en', 'entity:node', 'en', [
      'field_price' => $this->mockField('field_price', 'decimal', [9.99]),
    ]);
    $builder = new DocumentBuilder(new FieldMapper());
    $doc = $builder->buildAddCommand($item, 'my_index')['add']['doc'];

    // Single value -> scalar into the *s_* field name, not a JSON array.
    $this->assertArrayHasKey('fts_field_price', $doc);
    $this->assertSame(9.99, $doc['fts_field_price']);
  }

  /**
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandMultiValuedFieldIsJsonArray(): void {
    $item = $this->mockItem('node/4:en', 'entity:node', 'en', [
      'field_tags' => $this->mockField('field_tags', 'string', ['red', 'blue'], TRUE),
    ]);
    $builder = new DocumentBuilder(new FieldMapper());
    $doc = $builder->buildAddCommand($item, 'my_index')['add']['doc'];

    // Multi-valued -> array into the *m_* field name (plan doc line 185).
    $this->assertSame(['red', 'blue'], $doc['sm_field_tags']);
  }

  /**
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandFormatsBooleanAndDateValues(): void {
    $item = $this->mockItem('node/5:en', 'entity:node', 'en', [
      'field_active' => $this->mockField('field_active', 'boolean', [TRUE]),
      'field_created' => $this->mockField('field_created', 'date', [0]),
    ]);
    $builder = new DocumentBuilder(new FieldMapper());
    $doc = $builder->buildAddCommand($item, 'my_index')['add']['doc'];

    $this->assertSame('true', $doc['bs_field_active']);
    $this->assertSame('1970-01-01T00:00:00Z', $doc['ds_field_created']);
  }

  /**
   * search_api_solr 4.3.13 indexes a deterministic scalar sort copy for text.
   *
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandAddsDeterministicTextSortCopies(): void {
    $item = $this->mockItem('node/7:en', 'entity:node', 'en', [
      'title' => $this->mockField('title', 'text', [new TextValue('Single title')]),
      'field_body' => $this->mockField(
        'field_body',
        'text',
        [new TextValue('First paragraph'), new TextValue('Second paragraph')],
        TRUE
      ),
    ]);

    $doc = (new DocumentBuilder(new FieldMapper()))->buildAddCommand($item, 'my_index')['add']['doc'];

    $this->assertSame('Single title', $doc['sort_title']);
    $this->assertSame('First paragraph', $doc['sort_field_body']);
  }

  /**
   * Pins #302: a multi-valued text field's sort_* copy takes the FIRST value,
   * matching captured search_api_solr / solr:9 — not the min/max selector the
   * non-text path uses.
   *
   * search_api_solr's own source copies only the first value into sort_*:
   * SearchApiSolrBackend::addIndexField() returns "the first value of $values
   * that has been added to the index" (coverage/.../SearchApiSolrBackend.php,
   * the @return at line 2726), and the caller writes that scalar into each
   * language-specific sort_* field (line 1485). The captured live-solr:9 trace
   * confirms it (`solr-ref/search-api/trace/00001.json`): a document with
   * `sm_field_topics = ["legacy", "documentation"]` indexes as
   * `sort_X3b_en_field_topics = "legacy"` — the first value, not the min
   * (`documentation`) nor the max. Zero sort_* field carries more than one
   * value anywhere in the trace, so Solr's Lucene min/max selector never runs
   * on a text sort field. Recorded as finding #153 in
   * docs/solr-ref-findings.md.
   *
   * The values below are chosen so the first value is NEITHER the min NOR the
   * max: first='mango', min='apple', max='zebra'. A regression that "fixed"
   * this to min/max selection (the tempting wrong fix) would make this test
   * fail, where the sibling test above would not — its ['First paragraph',
   * 'Second paragraph'] happens to sort first==min.
   *
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandMultivaluedTextSortTakesFirstValueNotMinMax(): void {
    $item = $this->mockItem('node/302:en', 'entity:node', 'en', [
      'field_pick' => $this->mockField(
        'field_pick',
        'text',
        // first='mango' is neither the min ('apple') nor the max ('zebra').
        [new TextValue('mango'), new TextValue('apple'), new TextValue('zebra')],
        TRUE
      ),
    ]);

    $doc = (new DocumentBuilder(new FieldMapper()))->buildAddCommand($item, 'my_index')['add']['doc'];

    $this->assertSame('mango', $doc['sort_field_pick']);
  }

  /**
   * Regression test for issue #83, end-to-end through the
   * DocumentBuilder -> FieldMapper path: a multi-valued `text` field whose
   * values are TextValue objects (their real shape from Search API, per
   * TextDataType::getValue()) must produce a doc array of plain strings that
   * json_encode()s to a JSON array of strings -- not '{}' objects, which is
   * the malformed body Wayfinder's /update parser rejected in the #80
   * integration harness ("field tm_body expects a string value, got {}").
   *
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandTextValueFieldSerializesToPlainStringsInJson(): void {
    $item = $this->mockItem('node/6:en', 'entity:node', 'en', [
      'field_body' => $this->mockField(
        'field_body',
        'text',
        [new TextValue('First paragraph'), new TextValue('Second paragraph')],
        TRUE
      ),
    ]);
    $builder = new DocumentBuilder(new FieldMapper());
    $doc = $builder->buildAddCommand($item, 'my_index')['add']['doc'];

    $this->assertSame(['First paragraph', 'Second paragraph'], $doc['tm_field_body']);

    $encoded = json_decode(json_encode($doc), TRUE);
    $this->assertSame(['First paragraph', 'Second paragraph'], $encoded['tm_field_body']);
  }

  /**
   * An item field with no values is omitted from the document entirely --
   * Solr never receives null/[] for an absent field, and Wayfinder rejects
   * null for a typed field ("field ts_X expects a string value, got null").
   * Surfaced by the #262 file-extraction tracer, where the computed
   * attachment field is empty on every item that has no attachment.
   *
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandOmitsFieldsWithNoValues(): void {
    $item = $this->mockItem('node/8:en', 'entity:node', 'en', [
      'title' => $this->mockField('title', 'text', [new TextValue('Has a value')]),
      'field_optional' => $this->mockField('field_optional', 'string', [], FALSE),
    ]);

    $doc = (new DocumentBuilder(new FieldMapper()))->buildAddCommand($item, 'my_index')['add']['doc'];

    // The populated field is present ...
    $this->assertSame('Has a value', $doc['ts_title']);
    $this->assertArrayHasKey('sort_title', $doc);
    // ... and the empty optional field is omitted entirely, not null/[], so
    // it can never be rejected as a null for a typed field.
    $this->assertArrayNotHasKey('ss_field_optional', $doc);
  }

}
