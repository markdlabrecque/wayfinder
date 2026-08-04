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
   * 'missing' => bool, 'query_type' => string]`. 'operator' => 'or' now
   * translates to {!ex=facet:<field>} (#298, reversing plan doc locked
   * decision 4 once the server served {!ex}/{!tag} in #295); 'and' (and an
   * absent key) stays unexcluded.
   *
   * `facet.sort` is not part of that contrib shape (`getFacetOptions()` never
   * sets a 'sort' key) but the plan doc calls for "facet.sort from the
   * facet's sort if given" -- so these tests treat 'sort' as an optional key
   * on the per-facet info array, present or absent, per plan doc wording.
   *
   * Issue #296 (findings 147-151, `docs/solr-ref-findings.md`): the server
   * only honours per-field facet settings as a local param on `facet.field`
   * itself (`f.<field>.facet.*` resolves against the Solr field name, never
   * the `{!key=}` delta this module always sends -- finding 147, and #299
   * keys every facet by the Search API delta). So `buildFacets()` emits
   * `limit`/`min_count`/`sort`/`missing` as `facet.<name>=<value>` local
   * params appended to the SAME block `{!key=<delta>}`/`{!ex=...}` already
   * carries -- never as top-level `facet.limit`/`facet.mincount`/
   * `facet.sort`/`facet.missing` params, and never shared across facets. The
   * previous "last facet's settings win for the whole request" ceiling
   * (ponytail, now removed) is gone: two facets on one field keep independent
   * settings, which is the routine shape #299 produces and the reason #296
   * exists. Order inside the block is `ex`, `key`, then `facet.limit`,
   * `facet.mincount`, `facet.sort`, `facet.missing` -- the same order the
   * settings are read from the Search API facet array below, and matching
   * captured Solr rows where a key/ex prefix is followed by facet.* settings
   * (`solr-ref/responses/facet_perfield_lp_limit.json` et al.).
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
  public function testSingleFacetEmitsSettingsAsLocalParamsOnItsFacetField(): void {
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
    // #296: limit/mincount/missing travel as local params on this facet's own
    // facet.field, not as global facet.* params -- there is no top-level
    // facet.limit/facet.mincount/facet.missing at all.
    $this->assertSame(
      '{!key=category facet.limit=10 facet.mincount=2 facet.missing=false}ss_category',
      $params['facet.field'],
    );
    $this->assertArrayNotHasKey('facet.limit', $params);
    $this->assertArrayNotHasKey('facet.mincount', $params);
    $this->assertArrayNotHasKey('facet.missing', $params);
    $this->assertArrayNotHasKey('facet.sort', $params);
  }

  /**
   * @covers ::build
   */
  public function testFacetMissingTrueIsSentAsALocalParamStringTrue(): void {
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

    // Sent as the literal string Solr expects (never a PHP bool cast to '').
    $this->assertSame(
      '{!key=category facet.limit=10 facet.mincount=1 facet.missing=true}ss_category',
      $params['facet.field'],
    );
    $this->assertArrayNotHasKey('facet.missing', $params);
  }

  /**
   * @covers ::build
   */
  public function testFacetSortIsSentAsALocalParamWhenGivenOnTheFacetOption(): void {
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

    // sort sits between mincount and missing -- the order the settings are
    // read from the facet array in buildFacets().
    $this->assertSame(
      '{!key=category facet.limit=10 facet.mincount=1 facet.sort=index facet.missing=false}ss_category',
      $params['facet.field'],
    );
    $this->assertArrayNotHasKey('facet.sort', $params);
  }

  /**
   * #296: two facets on two different fields, each with its own settings
   * baked into its own facet.field local-params block -- no top-level
   * facet.* param is shared or overwritten between them, unlike the removed
   * last-wins ceiling.
   *
   * @covers ::build
   */
  public function testMultipleFacetsEachCarryTheirOwnSettingsAsLocalParams(): void {
    $index = $this->mockIndex([], [
      'category' => $this->mockIndexField('category', 'string', FALSE),
      'brand' => $this->mockIndexField('brand', 'string', TRUE),
    ]);
    $facets = [
      'category' => [
        'field' => 'category',
        'limit' => 10,
        'min_count' => 1,
        'missing' => FALSE,
      ],
      'brand' => [
        'field' => 'brand',
        'limit' => 3,
        'min_count' => 2,
        'missing' => TRUE,
      ],
    ];
    $query = $this->mockQuery(NULL, NULL, $index, NULL, [], ['search_api_facets' => $facets]);

    $params = (new QueryBuilder())->build($query);

    // "brand" is multi-valued (per mockIndexField's TRUE arg above), so its
    // mapped name uses the 'm' infix, exactly as qf/fq mapping already does
    // elsewhere in this class.
    $this->assertSame(
      [
        '{!key=category facet.limit=10 facet.mincount=1 facet.missing=false}ss_category',
        '{!key=brand facet.limit=3 facet.mincount=2 facet.missing=true}sm_brand',
      ],
      $params['facet.field'],
    );
    $this->assertArrayNotHasKey('facet.limit', $params);
    $this->assertArrayNotHasKey('facet.mincount', $params);
    $this->assertArrayNotHasKey('facet.missing', $params);
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
   * must translate, or a default facet config silently returns nothing --
   * now inside the local-params block rather than a top-level param.
   *
   * @covers ::build
   */
  public function testFacetLimitZeroMeansUnlimitedAndIsSentAsMinusOneInTheLocalParam(): void {
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

    $this->assertSame(
      '{!key=category facet.limit=-1 facet.mincount=1 facet.missing=false}ss_category',
      $params['facet.field'],
    );
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

    $this->assertSame(
      '{!key=weight facet.limit=5 facet.mincount=1 facet.missing=false}its_weight',
      $params['facet.field'],
    );
  }

  /**
   * #299/#296: two Search API facets on the same field must not collapse,
   * AND must keep independent settings -- the shape #299's delta-keyed
   * facets routinely produce, and the reason #296 cannot be built from
   * `f.<field>.facet.*` alone (finding 149: the per-field form addresses the
   * field, which both facets share, so it can only ever set both or neither).
   * The core answers one key per distinct facet.field value, so QueryBuilder
   * emits a distinct {!key=<delta> facet.limit=...} per facet -- here two
   * facets over `category` come back under `category_top` (limited to 5) and
   * `category_all` (unlimited) instead of both sharing one global limit.
   * Ground truth for the shape: `solr-ref/responses/facet_perfield_two_lp.json`
   * (two facets on one field, each carrying its own `facet.limit` local
   * param); the prefix wire format itself is fixed by src/facet.rs
   * split_facet_key ({!key=label ...}field, no space before field).
   *
   * @covers ::build
   */
  public function testTwoFacetsOnOneFieldEmitDistinctKeyedFacetFieldEntriesWithTheirOwnSettings(): void {
    $index = $this->mockIndex([], [
      'category' => $this->mockIndexField('category', 'string', FALSE),
    ]);
    $facets = [
      'category_top' => [
        'field' => 'category',
        'limit' => 5,
        'min_count' => 1,
        'missing' => FALSE,
      ],
      'category_all' => [
        'field' => 'category',
        'limit' => 0,
        'min_count' => 1,
        'missing' => FALSE,
      ],
    ];
    $query = $this->mockQuery(NULL, NULL, $index, NULL, [], ['search_api_facets' => $facets]);

    $params = (new QueryBuilder())->build($query);

    // Both entries facet the same mapped field (ss_category); the {!key=}
    // label AND the facet.limit local param differ per facet -- 5 vs
    // unlimited (-1, Search API's limit => 0).
    $this->assertSame(
      [
        '{!key=category_top facet.limit=5 facet.mincount=1 facet.missing=false}ss_category',
        '{!key=category_all facet.limit=-1 facet.mincount=1 facet.missing=false}ss_category',
      ],
      $params['facet.field'],
    );
  }

  /**
   * #299 guard: the facet delta is the array key of search_api_facets, which
   * is "in practice the facet's field identifier" (a machine name) but is not
   * constrained to be one. A delta carrying `}` or whitespace would break out
   * of the {!key=...} local-params block (src/local_params.rs terminates the
   * block on `}` and splits pairs on whitespace), so buildFacets() falls back
   * to dropping just the `key=` half for a delta that is not
   * [A-Za-z0-9_:-]+ rather than emit a broken prefix. This is the *key* guard
   * only -- issue #296's facet.* settings are not delta-derived, so they are
   * still emitted in the block; only the key is dropped, same fallback #298
   * already established for an OR facet's `{!ex=...}` half
   * (`testOrFacetWithHostileDeltaKeepsExButDropsKey` below).
   *
   * Mutation-tested: removing the guard makes this emit
   * '{!key=bad delta facet.limit=10 ...}ss_category' and the assertion fails.
   *
   * @covers ::build
   */
  public function testHostileFacetDeltaDropsTheKeyButKeepsSettings(): void {
    $index = $this->mockIndex([], [
      'category' => $this->mockIndexField('category', 'string', FALSE),
    ]);
    // Delta contains a space (and would also break on `}`): not a safe
    // local-params value.
    $facets = [
      'bad delta' => [
        'field' => 'category',
        'limit' => 10,
        'min_count' => 1,
        'missing' => FALSE,
      ],
    ];
    $query = $this->mockQuery(NULL, NULL, $index, NULL, [], ['search_api_facets' => $facets]);

    $params = (new QueryBuilder())->build($query);

    $this->assertSame(
      '{!facet.limit=10 facet.mincount=1 facet.missing=false}ss_category',
      $params['facet.field'],
    );
  }

  /**
   * #296: a facet setting value is not necessarily a safe local-params token
   * either -- `facet.sort` on the facet array is free-form input the same way
   * the delta is (#299's guard covers only the key), and a value carrying a
   * space or a `}` would otherwise break out of the block the same way a
   * hostile delta does. Unlike the delta (which has a safe fallback -- the
   * bare field name / a dropped key), a setting has no safe fallback that
   * keeps its meaning, so it must be quoted rather than dropped: wrapped in
   * `"..."`, which src/local_params.rs's block-value grammar already reads
   * (a `}` inside a quoted value does not close the block -- `find_block_end`
   * skips it -- and read_value stops at the matching, non-escaped closing
   * quote).
   *
   * Mutation-tested: removing the guard makes this emit
   * '{!key=cat facet.sort=count} extra=malicious}ss_category' (the block
   * closing early, right after count) and the assertion fails.
   *
   * @covers ::build
   */
  public function testHostileFacetSortValueIsQuotedRatherThanBreakingTheBlock(): void {
    $index = $this->mockIndex([], [
      'category' => $this->mockIndexField('category', 'string', FALSE),
    ]);
    $facets = [
      'cat' => [
        'field' => 'category',
        'min_count' => 1,
        'missing' => FALSE,
        // A space and an unescaped `}` -- both would otherwise terminate the
        // block early or split into a bogus second local param.
        'sort' => 'count} extra=malicious',
      ],
    ];
    $query = $this->mockQuery(NULL, NULL, $index, NULL, [], ['search_api_facets' => $facets]);

    $params = (new QueryBuilder())->build($query);

    $this->assertSame(
      '{!key=cat facet.mincount=1 facet.sort="count} extra=malicious" facet.missing=false}ss_category',
      $params['facet.field'],
    );
  }

  /**
   * #298: an OR-operator facet emits {!ex=facet:<field>} so its counts run
   * against the base query minus its own tagged fq (server-side {!ex}/{!tag}
   * landed in #295). The ex tag is the *Search API field id* -- the exact
   * string search_api_solr's SearchApiSolrBackend puts in {!ex=...} via
   * addExcludes(['facet:' . $info['field']]) and the facets module matches in
   * {!tag=...} -- never the mapped Solr field. The delta key (#299) is kept
   * so ResponseParser still resolves the facet by key, and #296's settings
   * follow both.
   *
   * @covers ::build
   */
  public function testOrOperatorFacetEmitsExclusionTagOnTheSearchApiField(): void {
    $index = $this->mockIndex([], [
      'category' => $this->mockIndexField('category', 'string', FALSE),
    ]);
    $facets = [
      'category' => [
        'field' => 'category',
        'limit' => 10,
        'min_count' => 1,
        'missing' => FALSE,
        'operator' => 'or',
        'query_type' => 'search_api_string',
      ],
    ];
    $query = $this->mockQuery(NULL, NULL, $index, NULL, [], ['search_api_facets' => $facets]);

    $params = (new QueryBuilder())->build($query);

    // ex before key, settings after -- the shape
    // solr-ref/responses/facet_extag_both_facets.json proves is the
    // reproducible OR-facet UI, extended by #296's facet.* settings. The tag
    // carries a colon, which the server's local_params::read_value reads as
    // a bare value (pinned in src/local_params.rs).
    $this->assertSame(
      '{!ex=facet:category key=category facet.limit=10 facet.mincount=1 facet.missing=false}ss_category',
      $params['facet.field'],
    );
  }

  /**
   * Regression for #298: the AND-operator case is unchanged -- no {!ex}. The
   * whole point of the exclusion is that only OR facets get it.
   *
   * @covers ::build
   * @dataProvider nonOrOperatorProvider
   */
  public function testNonOrOperatorFacetEmitsNoExclusionTag(?string $operator): void {
    $index = $this->mockIndex([], [
      'category' => $this->mockIndexField('category', 'string', FALSE),
    ]);
    $facet = [
      'field' => 'category',
      'limit' => 10,
      'min_count' => 1,
      'missing' => FALSE,
    ];
    if ($operator !== NULL) {
      $facet['operator'] = $operator;
    }
    $query = $this->mockQuery(NULL, NULL, $index, NULL, [], ['search_api_facets' => ['category' => $facet]]);

    $params = (new QueryBuilder())->build($query);

    $this->assertSame(
      '{!key=category facet.limit=10 facet.mincount=1 facet.missing=false}ss_category',
      $params['facet.field'],
    );
  }

  public static function nonOrOperatorProvider(): array {
    return [
      'explicit and' => ['and'],
      'absent (defaults to and)' => [NULL],
    ];
  }

  /**
   * #298: an OR facet whose delta is not a safe local-params value still gets
   * its {!ex} (built from the field id, not the delta) but drops the key, the
   * same fallback #299 established for the key half -- #296's settings are
   * unaffected by the delta and are still emitted.
   *
   * @covers ::build
   */
  public function testOrFacetWithHostileDeltaKeepsExButDropsKey(): void {
    $index = $this->mockIndex([], [
      'category' => $this->mockIndexField('category', 'string', FALSE),
    ]);
    $facets = [
      'bad delta' => [
        'field' => 'category',
        'limit' => 10,
        'min_count' => 1,
        'missing' => FALSE,
        'operator' => 'or',
      ],
    ];
    $query = $this->mockQuery(NULL, NULL, $index, NULL, [], ['search_api_facets' => $facets]);

    $params = (new QueryBuilder())->build($query);

    $this->assertSame(
      '{!ex=facet:category facet.limit=10 facet.mincount=1 facet.missing=false}ss_category',
      $params['facet.field'],
    );
  }

  /**
   * #298: a condition group tagged by the facets module with
   * facet:<search_api_field_name> emits {!tag=...} on its fq, the matching
   * half of the {!ex} on the facet field. search_api_solr does this in
   * reduceFilterQueries(): condition-group tags become {!tag} local params on
   * the resulting fq. The index-scope fq is never tagged.
   *
   * @covers ::build
   */
  public function testTaggedConditionGroupEmitsTagLocalParamOnItsFq(): void {
    $index = $this->mockIndex([], [
      'category' => $this->mockIndexField('category', 'string', FALSE),
    ]);
    $facetFilter = (new ConditionGroup('OR', ['facet:category']))
      ->addCondition('category', 'animals')
      ->addCondition('category', 'classic');
    $conditions = (new ConditionGroup('AND'))->addConditionGroup($facetFilter);

    $params = (new QueryBuilder())->build($this->mockQuery(NULL, NULL, $index, $conditions));

    $this->assertSame([
      'index_id:"my_index"',
      '{!tag=facet:category}(ss_category:"animals" OR ss_category:"classic")',
    ], $params['fq']);
  }

  /**
   * #298: multiple tags on one condition group render comma-separated
   * ({!tag=a,b}), the form solr-ref/responses/facet_extag_multi_tag.json
   * captures. Each tag keeps its colon as one bare token.
   *
   * @covers ::build
   */
  public function testMultipleTagsOnAConditionGroupRenderCommaSeparated(): void {
    $index = $this->mockIndex([], [
      'category' => $this->mockIndexField('category', 'string', FALSE),
    ]);
    $facetFilter = (new ConditionGroup('OR', ['facet:category', 'facet:brand']))
      ->addCondition('category', 'animals')
      ->addCondition('category', 'classic');
    $conditions = (new ConditionGroup('AND'))->addConditionGroup($facetFilter);

    $params = (new QueryBuilder())->build($this->mockQuery(NULL, NULL, $index, $conditions));

    $this->assertSame(
      '{!tag=facet:category,facet:brand}(ss_category:"animals" OR ss_category:"classic")',
      $params['fq'][1],
    );
  }

  /**
   * Regression for #298: an untagged condition group emits no {!tag} prefix,
   * so existing facet-free queries are byte-identical.
   *
   * @covers ::build
   */
  public function testUntaggedConditionGroupEmitsNoTagLocalParam(): void {
    $index = $this->mockIndex([], [
      'category' => $this->mockIndexField('category', 'string', FALSE),
    ]);
    $or = (new ConditionGroup('OR'))
      ->addCondition('category', 'animals')
      ->addCondition('category', 'classic');
    $conditions = (new ConditionGroup('AND'))->addConditionGroup($or);

    $params = (new QueryBuilder())->build($this->mockQuery(NULL, NULL, $index, $conditions));

    $this->assertSame('(ss_category:"animals" OR ss_category:"classic")', $params['fq'][1]);
  }

  /**
   * #298: the full OR-facet UI shape -- a tagged fq AND an ex-tagged facet on
   * the same field in one request. This is the Drupal-side analogue of
   * solr-ref/responses/facet_extag_both_facets.json: the facet counts run
   * against the base minus the tagged fq, and the result resolves by key.
   *
   * The facet.field half also carries #296's settings, exactly as its sibling
   * testOrOperatorFacetEmitsExclusionTagOnTheSearchApiField (same facet array)
   * does -- what this test is *for* is the fq/facet.field pairing, which is
   * unchanged.
   *
   * @covers ::build
   */
  public function testOrFacetWithTaggedFilterEmitsBothHalves(): void {
    $index = $this->mockIndex([], [
      'category' => $this->mockIndexField('category', 'string', FALSE),
    ]);
    $facetFilter = (new ConditionGroup('OR', ['facet:category']))
      ->addCondition('category', 'animals')
      ->addCondition('category', 'classic');
    $conditions = (new ConditionGroup('AND'))->addConditionGroup($facetFilter);
    $facets = [
      'category' => [
        'field' => 'category',
        'limit' => 10,
        'min_count' => 1,
        'missing' => FALSE,
        'operator' => 'or',
      ],
    ];
    $query = $this->mockQuery(NULL, NULL, $index, $conditions, [], ['search_api_facets' => $facets]);

    $params = (new QueryBuilder())->build($query);

    $this->assertSame('{!tag=facet:category}(ss_category:"animals" OR ss_category:"classic")', $params['fq'][1]);
    $this->assertSame(
      '{!ex=facet:category key=category facet.limit=10 facet.mincount=1 facet.missing=false}ss_category',
      $params['facet.field'],
    );
  }

  /**
   * M4 (issue #78): the "search_api_mlt" query option, per plan doc line
   * 170-171 ("route to GET /mlt with q=id:"{index_id}-{item_id}" and
   * mlt.fl from the option's fields (mapped)") and the option's real shape,
   * confirmed against \Drupal\search_api\Plugin\views\argument\
   * SearchApiMoreLikeThis::query() (the only place core sets it):
   * ['id' => <search api item id>, 'fields' => <array of field ids>].
   * Document id composite format ("{index_id}-{item_id}") is locked decision
   * 2, and is exactly what DocumentBuilder::buildAddCommand() emits, so the
   * MLT seed lookup must query the same composite id, quoted because item
   * ids routinely contain ':' (e.g. "entity:node/1:en" per
   * ResponseParserTest) which would otherwise break the Lucene query.
   *
   * mlt.fl is comma-joined (not space-joined like qf) to match the captured
   * fixture convention (solr-ref/responses/mlt_baseline.json manifest entry:
   * "mlt.fl=body,category").
   *
   * @covers ::buildMlt
   */
  public function testBuildMltRoutesByCompositeIdAndMapsConfiguredFields(): void {
    $index = $this->mockIndex(['title', 'body'], [
      'title' => $this->mockIndexField('title', 'text', FALSE),
      'body' => $this->mockIndexField('body', 'text', TRUE),
    ], 'my_index');
    $query = $this->mockQuery(NULL, NULL, $index, NULL, [], [
      'search_api_mlt' => [
        'id' => 'entity:node/1:en',
        'fields' => ['title', 'body'],
      ],
    ]);

    $params = (new QueryBuilder())->buildMlt($query);

    $this->assertSame('id:"my_index-entity:node/1:en"', $params['q']);
    $this->assertSame('ts_title,tm_body', $params['mlt.fl']);
  }

  /**
   * @covers ::buildMlt
   */
  public function testBuildMltAppliesTheSamePagingOptionsAsSelect(): void {
    $index = $this->mockIndex([], [
      'title' => $this->mockIndexField('title', 'text', FALSE),
    ], 'my_index');
    $query = $this->mockQuery(NULL, NULL, $index, NULL, [], [
      'search_api_mlt' => ['id' => 'entity:node/2:en', 'fields' => ['title']],
      'offset' => 5,
      'limit' => 10,
    ]);

    $params = (new QueryBuilder())->buildMlt($query);

    $this->assertSame(5, $params['start']);
    $this->assertSame(10, $params['rows']);
  }

  /**
   * Reviewer round 1, must-fix 2: the seed item id is datasource-derived, not
   * constrained to machine names, so an id carrying '"' or '\' must not be
   * able to break out of the quoted phrase and inject query syntax. The
   * escaping is FieldMapper::filterValue()'s, the same one every other value
   * path in QueryBuilder uses.
   *
   * @covers ::buildMlt
   */
  public function testBuildMltEscapesQuotesAndBackslashesInTheSeedItemId(): void {
    $index = $this->mockIndex([], [
      'title' => $this->mockIndexField('title', 'text', FALSE),
    ], 'my_index');
    $query = $this->mockQuery(NULL, NULL, $index, NULL, [], [
      'search_api_mlt' => ['id' => 'evil" OR id:[* TO *] back\\slash', 'fields' => ['title']],
    ]);

    $params = (new QueryBuilder())->buildMlt($query);

    $this->assertSame('id:"my_index-evil\\" OR id:[* TO *] back\\\\slash"', $params['q']);
  }

  /**
   * Reviewer round 1, must-fix 3: the missing-seed-id guard is a validation
   * check, and this repo's working agreement requires those to be covered
   * (mutation-tested) rather than trusted. Without it a malformed option
   * silently produces id:"my_index-" and an empty result set.
   *
   * @covers ::buildMlt
   */
  public function testBuildMltRejectsAnMltOptionWithoutASeedItemId(): void {
    $index = $this->mockIndex([], [
      'title' => $this->mockIndexField('title', 'text', FALSE),
    ], 'my_index');
    $query = $this->mockQuery(NULL, NULL, $index, NULL, [], [
      'search_api_mlt' => ['fields' => ['title']],
    ]);

    $this->expectException(\InvalidArgumentException::class);
    (new QueryBuilder())->buildMlt($query);
  }

  /**
   * Issue #297: buildMlt() must scope the similar-docs result set to this
   * index, reusing the exact index_id:"<id>" filter build() seeds rather
   * than inventing a second convention. The server now honours fq on /mlt
   * for the result set (finding 98; fixtures mlt_fq_scope /
   * mlt_fq_seed_not_filtered / mlt_fq_multiple_and are already green in the
   * differential harness), so without this fq a core holding more than one
   * index can return documents from a sibling index. q's id: seed lookup is
   * unchanged -- covered by testBuildMltRoutesByCompositeIdAndMapsConfiguredFields.
   *
   * @covers ::buildMlt
   */
  public function testBuildMltScopesResultsToTheIndex(): void {
    $index = $this->mockIndex([], [
      'title' => $this->mockIndexField('title', 'text', FALSE),
    ], 'my_index');
    $query = $this->mockQuery(NULL, NULL, $index, NULL, [], [
      'search_api_mlt' => ['id' => 'entity:node/1:en', 'fields' => ['title']],
    ]);

    $params = (new QueryBuilder())->buildMlt($query);

    $this->assertSame('index_id:"my_index"', $params['fq']);
  }

  /**
   * M4 (issue #78): highlighting is "optional-but-in-scope" (plan doc line
   * 84-87) -- unlike Search API core's own algorithmic "highlight" processor
   * ("needs nothing from the backend", same paragraph: confirmed against
   * vendor/drupal/search_api's Highlight processor, which has no
   * preprocessSearchQuery() and never touches QueryInterface at all).
   *
   * Correction (orchestrator review, confirmed against
   * Highlight.php:342 -- postprocessSearchResults() only ever *reads*
   * $item->getExtraData('highlighted_fields', []) to skip fields the backend
   * already populated; it never inspects any query option): the trigger this
   * test-writer originally modelled as a query option was wrong. The plan
   * doc's "as search_api_solr does" is the real precedent, and
   * search_api_solr's own convention is a **backend-level config setting**,
   * not a per-query option -- the backend either always requests hl (per its
   * own configuration) or never does, independent of the query object. So
   * QueryBuilder::build() takes the highlight flag as an explicit second
   * argument (the backend's job is to read its own config and pass it in),
   * not something read off $query->getOption().
   *
   * hl.fl reuses the same mapped fulltext field set as qf (comma-joined, to
   * match the captured hl.fl fixture convention -- see
   * solr-ref/responses/hl_multi_field_comma.json manifest entry:
   * "hl.fl=body,category").
   *
   * @covers ::build
   */
  public function testBuildAddsHlParamsWhenHighlightingIsRequested(): void {
    $index = $this->mockIndex(['title', 'body'], [
      'title' => $this->mockIndexField('title', 'text', FALSE),
      'body' => $this->mockIndexField('body', 'text', TRUE),
    ]);
    $query = $this->mockQuery('rocket', NULL, $index);

    $params = (new QueryBuilder())->build($query, TRUE);

    $this->assertSame('true', $params['hl']);
    $this->assertSame('ts_title,tm_body', $params['hl.fl']);
  }

  /**
   * Negative case (and default): highlighting off means no hl params at all,
   * so a plain search doesn't pay for highlighting nobody configured.
   *
   * @covers ::build
   */
  public function testBuildOmitsHlParamsWhenHighlightingIsNotRequested(): void {
    $index = $this->mockIndex(['title'], [
      'title' => $this->mockIndexField('title', 'text', FALSE),
    ]);
    $query = $this->mockQuery('rocket', NULL, $index);

    $params = (new QueryBuilder())->build($query);

    $this->assertArrayNotHasKey('hl', $params);
    $this->assertArrayNotHasKey('hl.fl', $params);
  }

  /**
   * Result grouping (issue #290): search_api_grouping.use_grouping activates
   * the group.* component. The shape asserted here is search_api_solr's
   * setGrouping() output (finding 130): group.ngroups is unconditional, and
   * group.field is the single-valued fast field name. Ground truth for the
   * server half is solr-ref/responses/group_basic.json.
   *
   * @covers ::buildGrouping
   */
  public function testGroupingOptionEmitsGroupParams(): void {
    $index = $this->mockIndex([], [
      'type' => $this->mockIndexField('type', 'string', FALSE),
    ]);
    $query = $this->mockQuery(NULL, NULL, $index, NULL, [], [
      'search_api_grouping' => [
        'use_grouping' => TRUE,
        'fields' => ['type'],
      ],
    ]);

    $params = (new QueryBuilder())->build($query);

    $this->assertSame('true', $params['group']);
    $this->assertSame('true', $params['group.ngroups']);
    $this->assertSame('ss_type', $params['group.field']);
    // group.limit defaults to 1 and is omitted at its default (finding 130).
    $this->assertArrayNotHasKey('group.limit', $params);
  }

  /**
   * group.limit (when != 1), group.offset, group.sort, group.truncate and
   * group.facet map straight from the option onto the wire.
   *
   * @covers ::buildGrouping
   */
  public function testGroupingLimitOffsetSortTruncateAndFacetAreEmitted(): void {
    $index = $this->mockIndex([], [
      'type' => $this->mockIndexField('type', 'string', FALSE),
      'weight' => $this->mockIndexField('weight', 'integer', FALSE),
    ]);
    $query = $this->mockQuery(NULL, NULL, $index, NULL, [], [
      'search_api_grouping' => [
        'use_grouping' => TRUE,
        'fields' => ['type'],
        'group_limit' => 3,
        'group_offset' => 1,
        'group_sort' => ['search_api_id' => 'asc'],
        'truncate' => TRUE,
        'group_facet' => TRUE,
      ],
    ]);

    $params = (new QueryBuilder())->build($query);

    $this->assertSame(3, $params['group.limit']);
    $this->assertSame(1, $params['group.offset']);
    $this->assertSame('id asc', $params['group.sort']);
    $this->assertSame('true', $params['group.truncate']);
    $this->assertSame('true', $params['group.facet']);
  }

  /**
   * Multiple group fields become a repeatable group.field array, in request
   * order. Mirrors solr-ref/responses/group_multi_field.json.
   *
   * @covers ::buildGrouping
   */
  public function testGroupingMultipleFieldsBecomeAnArray(): void {
    $index = $this->mockIndex([], [
      'type' => $this->mockIndexField('type', 'string', FALSE),
      'weight' => $this->mockIndexField('weight', 'integer', FALSE),
    ]);
    $query = $this->mockQuery(NULL, NULL, $index, NULL, [], [
      'search_api_grouping' => [
        'use_grouping' => TRUE,
        'fields' => ['type', 'weight'],
      ],
    ]);

    $params = (new QueryBuilder())->build($query);

    $this->assertSame(['ss_type', 'its_weight'], $params['group.field']);
  }

  /**
   * A fulltext or multi-valued group field is skipped, never emitted, so the
   * request is not 400-ed by the server (finding 130 / src/grouping.rs
   * validate_group_field). Every requested field unsuitable -> grouping not
   * activated.
   *
   * @covers ::buildGrouping
   */
  public function testGroupingSkipsFulltextAndMultivaluedFields(): void {
    $index = $this->mockIndex([], [
      'body' => $this->mockIndexField('body', 'text', FALSE),
      'tags' => $this->mockIndexField('tags', 'string', TRUE),
    ]);
    $query = $this->mockQuery(NULL, NULL, $index, NULL, [], [
      'search_api_grouping' => [
        'use_grouping' => TRUE,
        'fields' => ['body', 'tags'],
      ],
    ]);

    $params = (new QueryBuilder())->build($query);

    $this->assertArrayNotHasKey('group', $params, 'no usable group field -> no group.* params');
  }

  /**
   * Default: no search_api_grouping option -> no group.* params at all.
   *
   * @covers ::buildGrouping
   */
  public function testNoGroupingOptionProducesNoGroupParams(): void {
    $index = $this->mockIndex([], [
      'type' => $this->mockIndexField('type', 'string', FALSE),
    ]);
    $query = $this->mockQuery(NULL, NULL, $index);

    $params = (new QueryBuilder())->build($query);

    $this->assertArrayNotHasKey('group', $params);
    $this->assertArrayNotHasKey('group.field', $params);
  }

  /**
   * #291: autocomplete builds a /terms request, not /suggest. The evidenced
   * wire (finding 154/131) is exactly terms=true, terms.fl (the query's
   * fulltext fields, mapped through FieldMapper), terms.prefix (the
   * incomplete key), terms.limit (the query's suggestion limit, default 10),
   * plus omitHeader=true as search_api_solr's standard envelope convention.
   * No q, no fq: the Terms component reads the indexed dictionary, it does
   * not run a search.
   *
   * @covers ::buildAutocompleteTerms
   */
  public function testBuildAutocompleteTermsSendsTermsPrefixFlAndLimit(): void {
    $index = $this->mockIndex(['title'], [
      'title' => $this->mockIndexField('title', 'text', FALSE),
    ]);
    $query = $this->mockQuery(NULL, NULL, $index, NULL, [], ['limit' => 5]);

    $params = (new QueryBuilder())->buildAutocompleteTerms($query, 'roc');

    $this->assertSame('true', $params['terms']);
    $this->assertSame('ts_title', $params['terms.fl']);
    $this->assertSame('roc', $params['terms.prefix']);
    $this->assertSame(5, $params['terms.limit']);
    $this->assertSame('true', $params['omitHeader']);
    // The terms component scans the dictionary; no query/filters apply.
    $this->assertArrayNotHasKey('q', $params);
    $this->assertArrayNotHasKey('fq', $params);
  }

  /**
   * @covers ::buildAutocompleteTerms
   */
  public function testBuildAutocompleteTermsDefaultsLimitToTen(): void {
    $index = $this->mockIndex(['title'], [
      'title' => $this->mockIndexField('title', 'text', FALSE),
    ]);
    $query = $this->mockQuery(NULL, NULL, $index);

    $params = (new QueryBuilder())->buildAutocompleteTerms($query, 'r');

    $this->assertSame(10, $params['terms.limit']);
  }

  /**
   * #300 integration: every solr_text_suggester field indexes into the one
   * fixed sink field 'twm_suggest' (finding 151), so two such fields must
   * collapse to a single terms.fl value rather than request the dictionary
   * twice. This is the field the SuggestComponent would read too, but the
   * terms component reaches it via terms.fl (finding 154).
   *
   * @covers ::buildAutocompleteTerms
   */
  public function testBuildAutocompleteTermsDedupesTwmSuggestAcrossSuggesterFields(): void {
    $index = $this->mockIndex(['suggest_a', 'suggest_b'], [
      'suggest_a' => $this->mockIndexField('suggest_a', 'solr_text_suggester', FALSE),
      'suggest_b' => $this->mockIndexField('suggest_b', 'solr_text_suggester', TRUE),
    ]);
    $query = $this->mockQuery(NULL, NULL, $index);

    $params = (new QueryBuilder())->buildAutocompleteTerms($query, 'r');

    $this->assertSame('twm_suggest', $params['terms.fl']);
  }

  /**
   * @covers ::buildAutocompleteTerms
   */
  public function testBuildAutocompleteTermsEmitsMultipleFieldsAsRepeatedFl(): void {
    $index = $this->mockIndex(['title', 'body'], [
      'title' => $this->mockIndexField('title', 'text', FALSE),
      'body' => $this->mockIndexField('body', 'text', TRUE),
    ]);
    $query = $this->mockQuery(NULL, NULL, $index);

    $params = (new QueryBuilder())->buildAutocompleteTerms($query, 'r');

    $this->assertSame(['ts_title', 'tm_body'], $params['terms.fl']);
  }

  /**
   * The Server suggester calls $query->setFulltextFields() with the
   * configured autocomplete fields (search_api_autocomplete Server.php), so
   * buildAutocompleteTerms must honour that subset, not the whole index.
   *
   * @covers ::buildAutocompleteTerms
   */
  public function testBuildAutocompleteTermsHonoursQueryFulltextFieldSubset(): void {
    $index = $this->mockIndex(['title', 'body'], [
      'title' => $this->mockIndexField('title', 'text', FALSE),
      'body' => $this->mockIndexField('body', 'text', TRUE),
    ]);
    $query = $this->mockQuery(NULL, ['title'], $index);

    $params = (new QueryBuilder())->buildAutocompleteTerms($query, 'r');

    $this->assertSame('ts_title', $params['terms.fl']);
  }

}
