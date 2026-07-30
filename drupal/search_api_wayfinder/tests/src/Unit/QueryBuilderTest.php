<?php

declare(strict_types=1);

namespace Drupal\Tests\search_api_wayfinder\Unit;

use Drupal\Core\Field\FieldDefinitionInterface;
use Drupal\Core\Field\FieldStorageDefinitionInterface;
use Drupal\search_api\IndexInterface;
use Drupal\search_api\Item\FieldInterface;
use Drupal\search_api\Query\QueryInterface;
use Drupal\search_api_wayfinder\QueryBuilder;
use PHPUnit\Framework\TestCase;

/**
 * Tests QueryBuilder for M1's plain-fulltext-keys case only.
 *
 * Per the M1 task spec: "$query->getKeys() -> q (parsed-keys nested array
 * flattened per plan doc Query translation section, keys NULL => q=*:* no
 * defType), qf from fulltext fields (inter) index fields with mapped names.
 * No condition groups, no sorts, no facets, no MLT, no highlighting in M1."
 *
 * Wayfinder's SELECT_PARAMS (docs/plans/57-search-api-wayfinder-backend.md,
 * architecture table lines 24-30) exposes q/qf/defType/fq as independent
 * top-level params -- NOT search_api_solr's inline "{!edismax qf='...'}" local
 * param syntax, which requires Solarium's query helper and is explicitly out
 * of scope ("{!tag}/{!ex} local params ... out of scope"). So the keys ->
 * q flattening implemented here is a Wayfinder-shaped adaptation of
 * search_api_solr's Utility::flattenKeys() (conjunction/negation/phrase
 * handling copied; the per-term-per-field embedding is not, since qf is sent
 * as its own param). This is a test-writer interpretation of an underspecified
 * point -- see handoff notes.
 *
 * index_id fq is asserted here (not deferred to M2) because it is locked
 * decision 2 (core multi-index-per-core wiring), not a user-authored filter.
 *
 * @coversDefaultClass \Drupal\search_api_wayfinder\QueryBuilder
 * @group search_api_wayfinder
 */
class QueryBuilderTest extends TestCase {

  /**
   * @var array<string, bool>
   *   Cardinality by field id, populated by mockIndexField() and consumed by
   *   mockIndex() when building the property-definition mocks that
   *   FieldMapper::isMultiValued() walks -- see FieldMapper's doc comment.
   */
  private array $multiValuedById = [];

  private function mockIndexField(string $id, string $type, bool $multiValued, float $boost = 1.0): FieldInterface {
    $this->multiValuedById[$id] = $multiValued;

    $field = $this->createMock(FieldInterface::class);
    $field->method('getFieldIdentifier')->willReturn($id);
    $field->method('getType')->willReturn($type);
    $field->method('getBoost')->willReturn($boost);
    $field->method('getPropertyPath')->willReturn($id);
    $field->method('getDatasourceId')->willReturn('entity:test');
    return $field;
  }

  private function mockIndex(array $fulltextFieldIds, array $fields, string $indexId = 'my_index'): IndexInterface {
    $index = $this->createMock(IndexInterface::class);
    $index->method('id')->willReturn($indexId);
    $index->method('getFulltextFields')->willReturn($fulltextFieldIds);
    $index->method('getField')->willReturnCallback(
      fn (string $id) => $fields[$id] ?? NULL
    );

    $properties = [];
    foreach (array_keys($fields) as $id) {
      // Realistic shape (issue #81): a content-entity field is
      // list-by-construction (isList() unconditionally TRUE) -- the actual
      // single/multi signal is field-storage cardinality, not isList().
      $multiValued = $this->multiValuedById[$id] ?? FALSE;
      $storage = $this->createMock(FieldStorageDefinitionInterface::class);
      $storage->method('getCardinality')->willReturn(
        $multiValued ? FieldStorageDefinitionInterface::CARDINALITY_UNLIMITED : 1
      );

      $definition = $this->createMock(FieldDefinitionInterface::class);
      $definition->method('isList')->willReturn(TRUE);
      $definition->method('getFieldStorageDefinition')->willReturn($storage);
      $properties[$id] = $definition;
    }
    $index->method('getPropertyDefinitions')->willReturn($properties);

    foreach ($fields as $field) {
      $field->method('getIndex')->willReturn($index);
    }

    return $index;
  }

  private function mockQuery($keys, ?array $queryFulltextFields, IndexInterface $index): QueryInterface {
    $query = $this->createMock(QueryInterface::class);
    $query->method('getKeys')->willReturn($keys);
    $query->method('getFulltextFields')->willReturn($queryFulltextFields);
    $query->method('getIndex')->willReturn($index);
    return $query;
  }

  /**
   * @covers ::build
   */
  public function testSingleTermKeysProducePlainQuery(): void {
    $index = $this->mockIndex(['title', 'body'], [
      'title' => $this->mockIndexField('title', 'text', FALSE),
      'body' => $this->mockIndexField('body', 'text', TRUE),
    ]);
    $query = $this->mockQuery('rocket', NULL, $index);

    $params = (new QueryBuilder())->build($query);

    $this->assertSame('rocket', $params['q']);
    $this->assertSame('edismax', $params['defType']);
    // "body" is multi-valued on the index (per mockIndexField's TRUE arg
    // above), so qf must use the 'm' dynamic-field name for it, not 's'.
    $this->assertSame('ts_title tm_body', $params['qf']);
    $this->assertSame('index_id:"my_index"', $params['fq']);
  }

  /**
   * @covers ::build
   */
  public function testNullKeysProduceMatchAllWithNoDefType(): void {
    $index = $this->mockIndex(['title'], [
      'title' => $this->mockIndexField('title', 'text', FALSE),
    ]);
    $query = $this->mockQuery(NULL, NULL, $index);

    $params = (new QueryBuilder())->build($query);

    $this->assertSame('*:*', $params['q']);
    $this->assertArrayNotHasKey('defType', $params);
    // index_id filtering still applies -- it's core multi-index wiring, not a
    // "filter" feature the match-all match-everything case should skip.
    $this->assertSame('index_id:"my_index"', $params['fq']);
  }

  /**
   * @covers ::build
   */
  public function testOrConjunctionKeysAreJoinedWithExplicitOr(): void {
    $index = $this->mockIndex(['title'], [
      'title' => $this->mockIndexField('title', 'text', FALSE),
    ]);
    $keys = [
      '#conjunction' => 'OR',
      0 => 'cat',
      1 => 'dog',
    ];
    $query = $this->mockQuery($keys, NULL, $index);

    $params = (new QueryBuilder())->build($query);

    $this->assertSame('cat OR dog', $params['q']);
  }

  /**
   * @covers ::build
   */
  public function testAndConjunctionKeysAreSpaceJoined(): void {
    $index = $this->mockIndex(['title'], [
      'title' => $this->mockIndexField('title', 'text', FALSE),
    ]);
    $keys = [
      '#conjunction' => 'AND',
      0 => 'quick',
      1 => 'fox',
    ];
    $query = $this->mockQuery($keys, NULL, $index);

    $params = (new QueryBuilder())->build($query);

    $this->assertSame('quick fox', $params['q']);
  }

  /**
   * @covers ::build
   */
  public function testMultiWordTermIsQuotedAsPhrase(): void {
    $index = $this->mockIndex(['title'], [
      'title' => $this->mockIndexField('title', 'text', FALSE),
    ]);
    $keys = [
      '#conjunction' => 'AND',
      0 => 'brown fox',
    ];
    $query = $this->mockQuery($keys, NULL, $index);

    $params = (new QueryBuilder())->build($query);

    $this->assertSame('"brown fox"', $params['q']);
  }

  /**
   * @covers ::build
   */
  public function testNegatedNestedGroupIsPrefixedWithMinus(): void {
    $index = $this->mockIndex(['title'], [
      'title' => $this->mockIndexField('title', 'text', FALSE),
    ]);
    $keys = [
      '#conjunction' => 'AND',
      0 => 'quick',
      1 => [
        '#conjunction' => 'AND',
        '#negation' => TRUE,
        0 => 'banned',
      ],
    ];
    $query = $this->mockQuery($keys, NULL, $index);

    $params = (new QueryBuilder())->build($query);

    $this->assertSame('quick -banned', $params['q']);
  }

  /**
   * @covers ::build
   */
  public function testQfIntersectsQueryFulltextFieldsWithIndexFulltextFields(): void {
    $index = $this->mockIndex(['title', 'body', 'summary'], [
      'title' => $this->mockIndexField('title', 'text', FALSE),
      'body' => $this->mockIndexField('body', 'text', TRUE),
      'summary' => $this->mockIndexField('summary', 'text', FALSE),
    ]);
    // Query restricts the search to just "title": qf should not include
    // "body" or "summary" even though they're fulltext on the index.
    $query = $this->mockQuery('rocket', ['title'], $index);

    $params = (new QueryBuilder())->build($query);

    $this->assertSame('ts_title', $params['qf']);
  }

  /**
   * @covers ::build
   */
  public function testQfIncludesFieldBoost(): void {
    $index = $this->mockIndex(['title', 'body'], [
      'title' => $this->mockIndexField('title', 'text', FALSE, 2.0),
      'body' => $this->mockIndexField('body', 'text', TRUE, 1.0),
    ]);
    $query = $this->mockQuery('rocket', NULL, $index);

    $params = (new QueryBuilder())->build($query);

    $this->assertSame('ts_title^2 tm_body', $params['qf']);
  }

  /**
   * @covers ::build
   */
  public function testSpecialCharactersInKeysAreEscaped(): void {
    // M1's one untrusted-input path: a raw search term must not be able to
    // inject a field query or break Tantivy's query grammar via an
    // unescaped ':' or unbalanced quote/paren.
    $index = $this->mockIndex(['title'], [
      'title' => $this->mockIndexField('title', 'text', FALSE),
    ]);
    $query = $this->mockQuery('field:value', NULL, $index);

    $params = (new QueryBuilder())->build($query);

    $this->assertSame('field\\:value', $params['q']);
  }

}
