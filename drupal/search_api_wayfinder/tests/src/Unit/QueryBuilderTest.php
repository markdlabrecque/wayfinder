<?php

declare(strict_types=1);

namespace Drupal\Tests\search_api_wayfinder\Unit;

use Drupal\Core\Field\FieldDefinitionInterface;
use Drupal\Core\Field\FieldStorageDefinitionInterface;
use Drupal\search_api\IndexInterface;
use Drupal\search_api\Item\FieldInterface;
use Drupal\search_api\Query\ConditionGroup;
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

  private function mockQuery($keys, ?array $queryFulltextFields, IndexInterface $index, ?ConditionGroup $conditions = NULL, array $sorts = [], array $options = []): QueryInterface {
    $query = $this->createMock(QueryInterface::class);
    $query->method('getKeys')->willReturn($keys);
    $query->method('getFulltextFields')->willReturn($queryFulltextFields);
    $query->method('getIndex')->willReturn($index);
    $query->method('getConditionGroup')->willReturn($conditions ?? new ConditionGroup());
    $query->method('getSorts')->willReturn($sorts);
    $query->method('getOption')->willReturnCallback(
      static fn (string $name, $default = NULL) => $options[$name] ?? $default
    );
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
   * M2 uses Search API's real ConditionGroup objects, not a test-only shape.
   * A top-level AND emits one fq for each member; nested OR remains a single,
   * parenthesised fq. The mandatory index filter remains the first fq.
   *
   * @covers ::build
   */
  public function testNestedConditionGroupsKeepOneFqPerTopLevelMember(): void {
    $index = $this->mockIndex([], [
      'status' => $this->mockIndexField('status', 'string', FALSE),
      'weight' => $this->mockIndexField('weight', 'integer', FALSE),
      'created' => $this->mockIndexField('created', 'date', FALSE),
    ]);
    $or = (new ConditionGroup('OR'))
      ->addCondition('weight', 10, '>=')
      ->addCondition('created', 0, '<');
    $conditions = (new ConditionGroup('AND'))
      ->addCondition('status', 'published')
      ->addConditionGroup($or);

    $params = (new QueryBuilder())->build($this->mockQuery(NULL, NULL, $index, $conditions));

    $this->assertSame([
      'index_id:"my_index"',
      'ss_status:"published"',
      '(its_weight:[10 TO *] OR ds_created:[* TO 1970-01-01T00:00:00Z})',
    ], $params['fq']);
  }

  /**
   * @covers ::build
   * @dataProvider filterOperatorProvider
   */
  public function testFilterOperatorsUseSolrWireSemantics(string $operator, $value, string $expected): void {
    $index = $this->mockIndex([], ['value' => $this->mockIndexField('value', 'integer', FALSE)]);
    $conditions = (new ConditionGroup())->addCondition('value', $value, $operator);

    $params = (new QueryBuilder())->build($this->mockQuery(NULL, NULL, $index, $conditions));

    $this->assertSame($expected, $params['fq'][1]);
  }

  public static function filterOperatorProvider(): array {
    return [
      'equals' => ['=', 10, 'its_value:10'],
      'not equals' => ['<>', 10, '(*:* -its_value:10)'],
      'less than' => ['<', 10, 'its_value:[* TO 10}'],
      'less than or equal' => ['<=', 10, 'its_value:[* TO 10]'],
      'greater than' => ['>', 10, 'its_value:{10 TO *]'],
      'greater than or equal' => ['>=', 10, 'its_value:[10 TO *]'],
      'between' => ['BETWEEN', [10, 20], 'its_value:[10 TO 20]'],
      'not between' => ['NOT BETWEEN', [10, 20], '(*:* -its_value:[10 TO 20])'],
      'in' => ['IN', [10, 20], 'its_value:(10 20)'],
      'not in' => ['NOT IN', [10, 20], '(*:* -its_value:(10 20))'],
      'equals null means missing' => ['=', NULL, '-its_value:[* TO *]'],
      'not equals null means present' => ['<>', NULL, 'its_value:[* TO *]'],
    ];
  }

  /**
   * search_api_solr 4.3.13 rejects empty IN arrays rather than silently
   * broadening them; M2 preserves that validation contract.
   *
   * @covers ::build
   */
  public function testEmptyInIsRejected(): void {
    $index = $this->mockIndex([], ['value' => $this->mockIndexField('value', 'integer', FALSE)]);
    $conditions = (new ConditionGroup())->addCondition('value', [], 'IN');

    $this->expectException(\InvalidArgumentException::class);
    $this->expectExceptionMessage('empty array is not allowed');
    (new QueryBuilder())->build($this->mockQuery(NULL, NULL, $index, $conditions));
  }

  /**
   * @covers ::build
   */
  public function testFilterValuesUsePinnedUpstreamPhraseEscapingAndTypeFormatting(): void {
    $index = $this->mockIndex([], [
      'text' => $this->mockIndexField('text', 'text', FALSE),
      'date' => $this->mockIndexField('date', 'date', FALSE),
      'boolean' => $this->mockIndexField('boolean', 'boolean', FALSE),
      'integer' => $this->mockIndexField('integer', 'integer', FALSE),
      'decimal' => $this->mockIndexField('decimal', 'decimal', FALSE),
    ]);
    $special = "space + - && || ! ( ) { } [ ] ^ \" ~ * ? : \\ / OR *:*";
    $conditions = (new ConditionGroup())
      ->addCondition('text', $special)
      ->addCondition('date', 0)
      ->addCondition('boolean', TRUE)
      ->addCondition('integer', 42)
      ->addCondition('decimal', 3.14);

    $params = (new QueryBuilder())->build($this->mockQuery(NULL, NULL, $index, $conditions));

    $this->assertSame([
      'index_id:"my_index"',
      'ts_text:"space + - && || ! ( ) { } [ ] ^ \\" ~ * ? : \\\\ / OR *:*"',
      'ds_date:1970-01-01T00:00:00Z',
      'bs_boolean:"true"',
      'its_integer:42',
      'fts_decimal:3.14',
    ], $params['fq']);
  }

  /**
   * @covers ::build
   */
  public function testSortsAndPagingUseWayfinderSelectParameters(): void {
    $index = $this->mockIndex([], [
      'title' => $this->mockIndexField('title', 'text', FALSE),
      'weight' => $this->mockIndexField('weight', 'integer', FALSE),
    ]);
    $query = $this->mockQuery(NULL, NULL, $index, NULL, [
      'search_api_relevance' => 'DESC',
      'search_api_id' => 'ASC',
      'title' => 'ASC',
      'weight' => 'DESC',
    ], ['offset' => 20, 'limit' => 10]);

    $params = (new QueryBuilder())->build($query);

    $this->assertSame('score desc,id asc,sort_title asc,its_weight desc', $params['sort']);
    $this->assertSame(20, $params['start']);
    $this->assertSame(10, $params['rows']);
  }

  /**
   * @covers ::build
   */
  public function testUnlimitedLimitMapsToTheLargestPositiveRowCount(): void {
    $index = $this->mockIndex([], []);

    $params = (new QueryBuilder())->build($this->mockQuery(NULL, NULL, $index, NULL, [], ['limit' => -1]));

    $this->assertSame(PHP_INT_MAX, $params['rows']);
  }

  /**
   * @covers ::build
   */
  public function testSortsUseActualCardinalityAndSearchApiPseudoFields(): void {
    $index = $this->mockIndex([], [
      'tags' => $this->mockIndexField('tags', 'string', TRUE),
    ]);
    $query = $this->mockQuery(NULL, NULL, $index, NULL, [
      'tags' => 'ASC',
      'search_api_datasource' => 'DESC',
      'search_api_language' => 'ASC',
    ]);

    $params = (new QueryBuilder())->build($query);

    $this->assertSame('sm_tags asc,ss_search_api_datasource desc,ss_search_api_language asc', $params['sort']);
  }

  /**
   * search_api_solr 4.3.13 flattenKeys() preserves grouping before negation.
   *
   * @covers ::build
   */
  public function testNestedOrAndNegatedTwoTermGroupsKeepTheirSemantics(): void {
    $index = $this->mockIndex(['title'], [
      'title' => $this->mockIndexField('title', 'text', FALSE),
    ]);
    $keys = [
      '#conjunction' => 'AND',
      0 => 'quick',
      1 => [
        '#conjunction' => 'OR',
        0 => 'cat',
        1 => 'dog',
      ],
      2 => [
        '#conjunction' => 'AND',
        '#negation' => TRUE,
        0 => 'banned',
        1 => 'forbidden',
      ],
    ];

    $params = (new QueryBuilder())->build($this->mockQuery($keys, NULL, $index));

    $this->assertSame('quick (cat OR dog) -(banned forbidden)', $params['q']);
  }

  /**
   * search_api_solr 4.3.13 stops at NULL in mixed IN and loses later arms.
   * Wayfinder deliberately preserves both arms for this issue's conditions.
   *
   * @covers ::build
   * @dataProvider nullAndSingleRangeOperatorProvider
   */
  public function testPinnedUpstreamNullWildcardAndSingleRangeOperators(string $operator, $value, string $expected): void {
    $index = $this->mockIndex([], ['value' => $this->mockIndexField('value', 'integer', FALSE)]);
    $conditions = (new ConditionGroup())->addCondition('value', $value, $operator);

    $params = (new QueryBuilder())->build($this->mockQuery(NULL, NULL, $index, $conditions));

    $this->assertSame($expected, $params['fq'][1]);
  }

  public static function nullAndSingleRangeOperatorProvider(): array {
    return [
      'in null and value' => ['IN', [NULL, 10], '(its_value:10 OR -its_value:[* TO *])'],
      'not in null and value' => ['NOT IN', [NULL, 10], '(its_value:[* TO *] -its_value:(10))'],
      'literal wildcard exists' => ['=', '*', 'its_value:*'],
      'one element between' => ['BETWEEN', [10], 'its_value:10'],
      'one element not between' => ['NOT BETWEEN', [10], '(*:* -its_value:10)'],
    ];
  }

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

  /**
   * M3 facets. `$query->getOption('search_api_facets')` is the contrib
   * `facets` module's option shape (verified against
   * `facets/src/QueryType/QueryTypePluginBase::getFacetOptions()`, not
   * guessed): an associative array keyed by facet delta (in practice the
   * facet's field identifier), each entry `['field' => <SA field id>,
   * 'limit' => int, 'operator' => 'and'|'or', 'min_count' => int,
   * 'missing' => bool, 'query_type' => string]`. Plan doc locked decision 4
   * says OR facets are out of scope (no `{!ex}`/`{!tag}`), so 'operator' is
   * not translated to anything here.
   *
   * `facet.sort` is not part of that contrib shape (`getFacetOptions()` never
   * sets a 'sort' key) but the plan doc calls for "facet.sort from the
   * facet's sort if given" -- so these tests treat 'sort' as an optional key
   * on the per-facet info array, present or absent, per plan doc wording.
   *
   * `src/facet.rs` (`facet_fields()`) reads `facet.limit`/`facet.mincount`/
   * `facet.missing`/`facet.sort` as single global params applied to every
   * `facet.field` entry -- there is no `f.<field>.facet.*` per-field
   * override and no local-params support (matches locked decision 4). So
   * these tests only exercise multi-facet requests where every facet shares
   * identical limit/mincount/missing/sort settings; a query with two facets
   * asking for different values is left unspecified deliberately (see
   * handoff notes -- this is a real premise gap in the plan doc's per-facet
   * phrasing, not a guessed requirement).
   *
   * @covers ::build
   */
  public function testNoFacetsOptionProducesNoFacetParams(): void {
    $index = $this->mockIndex([], [
      'category' => $this->mockIndexField('category', 'string', FALSE),
    ]);
    $query = $this->mockQuery(NULL, NULL, $index, NULL, [], []);

    $params = (new QueryBuilder())->build($query);

    $this->assertArrayNotHasKey('facet', $params);
    $this->assertArrayNotHasKey('facet.field', $params);
    $this->assertArrayNotHasKey('facet.limit', $params);
    $this->assertArrayNotHasKey('facet.mincount', $params);
    $this->assertArrayNotHasKey('facet.missing', $params);
    $this->assertArrayNotHasKey('facet.sort', $params);
  }

  /**
   * @covers ::build
   */
  public function testSingleFacetProducesFacetParams(): void {
    $index = $this->mockIndex([], [
      'category' => $this->mockIndexField('category', 'string', FALSE),
    ]);
    $facets = [
      'category' => [
        'field' => 'category',
        'limit' => 10,
        'min_count' => 2,
        'missing' => FALSE,
        'operator' => 'and',
        'query_type' => 'search_api_string',
      ],
    ];
    $query = $this->mockQuery(NULL, NULL, $index, NULL, [], ['search_api_facets' => $facets]);

    $params = (new QueryBuilder())->build($query);

    $this->assertSame('true', $params['facet']);
    $this->assertSame('ss_category', $params['facet.field']);
    $this->assertSame(10, $params['facet.limit']);
    $this->assertSame(2, $params['facet.mincount']);
    $this->assertSame('false', $params['facet.missing']);
    $this->assertArrayNotHasKey('facet.sort', $params);
  }

  /**
   * @covers ::build
   */
  public function testFacetMissingTrueSendsFacetMissingStringTrue(): void {
    $index = $this->mockIndex([], [
      'category' => $this->mockIndexField('category', 'string', FALSE),
    ]);
    $facets = [
      'category' => [
        'field' => 'category',
        'limit' => 10,
        'min_count' => 1,
        'missing' => TRUE,
      ],
    ];
    $query = $this->mockQuery(NULL, NULL, $index, NULL, [], ['search_api_facets' => $facets]);

    $params = (new QueryBuilder())->build($query);

    $this->assertSame('true', $params['facet.missing']);
  }

  /**
   * @covers ::build
   */
  public function testFacetSortIsSentWhenGivenOnTheFacetOption(): void {
    $index = $this->mockIndex([], [
      'category' => $this->mockIndexField('category', 'string', FALSE),
    ]);
    $facets = [
      'category' => [
        'field' => 'category',
        'limit' => 10,
        'min_count' => 1,
        'missing' => FALSE,
        'sort' => 'index',
      ],
    ];
    $query = $this->mockQuery(NULL, NULL, $index, NULL, [], ['search_api_facets' => $facets]);

    $params = (new QueryBuilder())->build($query);

    $this->assertSame('index', $params['facet.sort']);
  }

  /**
   * @covers ::build
   */
  public function testMultipleFacetsProduceMultipleFacetFieldEntries(): void {
    $index = $this->mockIndex([], [
      'category' => $this->mockIndexField('category', 'string', FALSE),
      'brand' => $this->mockIndexField('brand', 'string', TRUE),
    ]);
    // Both facets share identical limit/mincount/missing -- see class-level
    // doc comment on why this test suite does not exercise divergent
    // per-facet settings.
    $facets = [
      'category' => [
        'field' => 'category',
        'limit' => 10,
        'min_count' => 1,
        'missing' => FALSE,
      ],
      'brand' => [
        'field' => 'brand',
        'limit' => 10,
        'min_count' => 1,
        'missing' => FALSE,
      ],
    ];
    $query = $this->mockQuery(NULL, NULL, $index, NULL, [], ['search_api_facets' => $facets]);

    $params = (new QueryBuilder())->build($query);

    // "brand" is multi-valued (per mockIndexField's TRUE arg above), so its
    // mapped name must use the 'm' infix, exactly as qf/fq mapping already
    // does elsewhere in this test class.
    $this->assertSame(['ss_category', 'sm_brand'], $params['facet.field']);
    $this->assertSame(10, $params['facet.limit']);
    $this->assertSame(1, $params['facet.mincount']);
    $this->assertSame('false', $params['facet.missing']);
  }

  /**
   * Search API's facet arrays use `limit => 0` to mean "no limit" -- it is the
   * ordinary case, not an edge case: every facet array in
   * `vendor/drupal/search_api/tests/src/Kernel/BackendTestBase.php` uses it,
   * and `search_api_db` reads it as unlimited (`if ((int) $limit > 0)`).
   * Wayfinder disagrees: `facet.limit=0` truncates to zero buckets
   * (`solr-ref/responses/facet_limit_zero.json` returns an empty array) and
   * only a *negative* limit means "as many as the server allows"
   * (`facet_limit_unlimited.json`; `src/facet.rs` `facet_fields()` maps
   * `requested_limit < 0` to `config.query.facet_limit_max`). So the builder
   * must translate, or a default facet config silently returns nothing.
   *
   * @covers ::build
   */
  public function testFacetLimitZeroMeansUnlimitedAndIsSentAsMinusOne(): void {
    $index = $this->mockIndex([], [
      'category' => $this->mockIndexField('category', 'string', FALSE),
    ]);
    $facets = [
      'category' => [
        'field' => 'category',
        'limit' => 0,
        'min_count' => 1,
        'missing' => FALSE,
      ],
    ];
    $query = $this->mockQuery(NULL, NULL, $index, NULL, [], ['search_api_facets' => $facets]);

    $params = (new QueryBuilder())->build($query);

    $this->assertSame(-1, $params['facet.limit']);
  }

  /**
   * @covers ::build
   */
  public function testFacetFieldNameUsesTheSameFieldMapperAsFilters(): void {
    $index = $this->mockIndex([], [
      'weight' => $this->mockIndexField('weight', 'integer', FALSE),
    ]);
    $facets = [
      'weight' => [
        'field' => 'weight',
        'limit' => 5,
        'min_count' => 1,
        'missing' => FALSE,
      ],
    ];
    $query = $this->mockQuery(NULL, NULL, $index, NULL, [], ['search_api_facets' => $facets]);

    $params = (new QueryBuilder())->build($query);

    $this->assertSame('its_weight', $params['facet.field']);
  }

}
