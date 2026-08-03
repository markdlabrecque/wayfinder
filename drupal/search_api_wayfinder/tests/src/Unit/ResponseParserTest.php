<?php

declare(strict_types=1);

namespace Drupal\Tests\search_api_wayfinder\Unit;

use Drupal\Core\Field\FieldDefinitionInterface;
use Drupal\Core\Field\FieldStorageDefinitionInterface;
use Drupal\search_api\IndexInterface;
use Drupal\search_api\Item\FieldInterface;
use Drupal\search_api\Query\QueryInterface;
use Drupal\search_api\Query\ResultSet;
use Drupal\search_api_wayfinder\ResponseParser;
use PHPUnit\Framework\TestCase;

/**
 * Tests ResponseParser: /select JSON response -> populated ResultSet.
 *
 * The envelope shape asserted here (response.numFound, response.docs[] each
 * with "id" and "score") is ground truth from
 * solr-ref/responses/edismax_score_baseline.json -- read directly, not
 * guessed. Wayfinder's own fixture uses arbitrary short doc ids ("eA" etc.)
 * because it's a core-Rust differential fixture, not Search API traffic; the
 * "{index_id}-{item_id}" convention used in the fixture built here for the
 * id-stripping tests comes from locked decision 2 in
 * docs/plans/57-search-api-wayfinder-backend.md ("Document id is
 * {index_id}-{item_id}"), not from a captured fixture (none exists for
 * Search API-shaped docs yet). No facets/highlighting parsing in M1.
 *
 * @coversDefaultClass \Drupal\search_api_wayfinder\ResponseParser
 * @group search_api_wayfinder
 */
class ResponseParserTest extends TestCase {

  /**
   * @var array<string, bool>
   *   Cardinality by field id, populated by mockIndexField() -- mirrors
   *   QueryBuilderTest's identically named helper/property, since
   *   FieldMapper::isMultiValued() needs the same property-definition mocks
   *   there.
   */
  private array $multiValuedById = [];

  private function mockIndexField(string $id, string $type, bool $multiValued): FieldInterface {
    $this->multiValuedById[$id] = $multiValued;

    $field = $this->createMock(FieldInterface::class);
    $field->method('getFieldIdentifier')->willReturn($id);
    $field->method('getType')->willReturn($type);
    $field->method('getPropertyPath')->willReturn($id);
    $field->method('getDatasourceId')->willReturn('entity:test');
    return $field;
  }

  /**
   * Builds a mock query. Optional `$fields`/`$facets` args are only needed by
   * the facet-parsing tests, which must resolve each facet's mapped Wayfinder
   * field name (via the real FieldMapper, exactly as QueryBuilder does) to
   * match `facet_counts.facet_fields`' keys back to the query's facet deltas.
   *
   * @param array<string, \Drupal\search_api\Item\FieldInterface> $fields
   * @param array<string, array> $facets
   *   `search_api_facets` option shape, see QueryBuilderTest's class-level
   *   facet doc comment.
   */
  private function mockQuery(string $indexId = 'my_index', array $fields = [], array $facets = []): QueryInterface {
    $index = $this->createMock(IndexInterface::class);
    $index->method('id')->willReturn($indexId);
    $index->method('getField')->willReturnCallback(
      fn (string $id) => $fields[$id] ?? NULL
    );

    $properties = [];
    foreach (array_keys($fields) as $id) {
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

    $query = $this->createMock(QueryInterface::class);
    $query->method('getIndex')->willReturn($index);
    $query->method('getOption')->willReturnCallback(
      static fn (string $name, $default = NULL) => $name === 'search_api_facets' ? $facets : $default
    );

    // Query::execute() creates the ResultSet up front and hands it to the
    // backend via getResults(); ResponseParser::parse() must populate that
    // existing object rather than constructing a new one.
    $resultSet = new ResultSet($query);
    $query->method('getResults')->willReturn($resultSet);

    return $query;
  }

  /**
   * Mocks an index whose fields FieldMapper can map (field id => type), for
   * the highlighting reverse-mapping tests below: ResponseParser must turn
   * Wayfinder's "highlighting" block -- keyed by *mapped* dynamic field name
   * (e.g. "ts_body"), same as solr-ref/responses/hl_basic.json's shape -- back
   * into Search API field ids, because
   * \Drupal\search_api\Item\ItemInterface::getExtraData() documents
   * "highlighted_fields" as "an array, keyed by field IDs" (confirmed by
   * reading ItemInterface.php directly), and search_api_solr's own
   * highlighting integration (which this module's locked decision 6 is
   * pinned to) populates that same per-item, per-field-id shape.
   */
  private function mockQueryWithFields(array $typeByFieldId, string $indexId = 'my_index'): QueryInterface {
    $index = $this->createMock(IndexInterface::class);
    $index->method('id')->willReturn($indexId);

    $fields = [];
    $properties = [];
    foreach ($typeByFieldId as $id => $type) {
      $field = $this->createMock(FieldInterface::class);
      $field->method('getFieldIdentifier')->willReturn($id);
      $field->method('getType')->willReturn($type);
      $field->method('getPropertyPath')->willReturn($id);
      $field->method('getDatasourceId')->willReturn('entity:test');
      $fields[$id] = $field;

      $storage = $this->createMock(FieldStorageDefinitionInterface::class);
      $storage->method('getCardinality')->willReturn(1);
      $definition = $this->createMock(FieldDefinitionInterface::class);
      $definition->method('isList')->willReturn(TRUE);
      $definition->method('getFieldStorageDefinition')->willReturn($storage);
      $properties[$id] = $definition;
    }
    $index->method('getFields')->willReturn($fields);
    $index->method('getPropertyDefinitions')->willReturn($properties);
    foreach ($fields as $field) {
      $field->method('getIndex')->willReturn($index);
    }

    $query = $this->createMock(QueryInterface::class);
    $query->method('getIndex')->willReturn($index);
    $resultSet = new ResultSet($query);
    $query->method('getResults')->willReturn($resultSet);

    return $query;
  }

  /**
   * @covers ::parse
   */
  public function testParsePopulatesResultCount(): void {
    $response = [
      'response' => [
        'numFound' => 4,
        'start' => 0,
        'docs' => [],
      ],
    ];

    $resultSet = (new ResponseParser())->parse($response, $this->mockQuery());

    $this->assertInstanceOf(ResultSet::class, $resultSet);
    $this->assertSame(4, $resultSet->getResultCount());
  }

  /**
   * @covers ::parse
   */
  public function testParseStripsIndexIdPrefixFromDocIdToRecoverItemId(): void {
    $response = [
      'response' => [
        'numFound' => 1,
        'start' => 0,
        'docs' => [
          ['id' => 'my_index-entity:node/1:en', 'score' => 0.5],
        ],
      ],
    ];

    $resultSet = (new ResponseParser())->parse($response, $this->mockQuery('my_index'));
    $items = $resultSet->getResultItems();

    $this->assertArrayHasKey('entity:node/1:en', $items);
    $this->assertSame('entity:node/1:en', $items['entity:node/1:en']->getId());
  }

  /**
   * @covers ::parse
   */
  public function testParseSetsItemScore(): void {
    $response = [
      'response' => [
        'numFound' => 1,
        'start' => 0,
        'docs' => [
          ['id' => 'my_index-entity:node/1:en', 'score' => 0.871532],
        ],
      ],
    ];

    $resultSet = (new ResponseParser())->parse($response, $this->mockQuery('my_index'));
    $item = $resultSet->getResultItems()['entity:node/1:en'];

    $this->assertSame(0.871532, $item->getScore());
  }

  /**
   * @covers ::parse
   */
  public function testParseMultipleDocsPreservesOrderAndCount(): void {
    // Shape matches solr-ref/responses/edismax_score_baseline.json exactly
    // (numFound 4, four docs each with id + score), just with SA-style doc
    // ids substituted for the id-stripping test above.
    $response = [
      'response' => [
        'numFound' => 4,
        'start' => 0,
        'docs' => [
          ['id' => 'my_index-eC', 'score' => 0.871532],
          ['id' => 'my_index-eD', 'score' => 0.72299594],
          ['id' => 'my_index-eB', 'score' => 0.71525735],
          ['id' => 'my_index-eA', 'score' => 0.5274755],
        ],
      ],
    ];

    $resultSet = (new ResponseParser())->parse($response, $this->mockQuery('my_index'));

    $this->assertSame(4, $resultSet->getResultCount());
    $this->assertSame(['eC', 'eD', 'eB', 'eA'], array_keys($resultSet->getResultItems()));
  }

  /**
   * @covers ::parse
   */
  public function testParseDefaultsScoreToOneWhenAbsent(): void {
    // fl=id,index_id (no score) is a legal M1 request shape when the query
    // doesn't need relevance ordering; Item's own default score is 1.0.
    $response = [
      'response' => [
        'numFound' => 1,
        'start' => 0,
        'docs' => [
          ['id' => 'my_index-entity:node/1:en'],
        ],
      ],
    ];

    $resultSet = (new ResponseParser())->parse($response, $this->mockQuery('my_index'));
    $item = $resultSet->getResultItems()['entity:node/1:en'];

    $this->assertSame(1.0, $item->getScore());
  }

  /**
   * M3 facets. `facet_counts.facet_fields`' flat array-pairs shape
   * (`["term", count, "term", count, ...]`, no `json.nl` sent) is ground
   * truth from `solr-ref/responses/facet_basic.json` and
   * `facet_missing.json` -- read directly, not guessed. The `facet_fields`
   * key is the facet delta: QueryBuilder emits `{!key=<delta>}<field>`
   * (issue #299), so the core labels each facet's buckets with the delta
   * rather than the mapped field name (see
   * testParseAttachesBothDeltasWhenTwoFacetsShareOneField for the case this
   * fixes).
   *
   * The extra-data shape (`[$delta => [['count' => int, 'filter' => string],
   * ...]]`) matches what `facets/src/Plugin/facets/query_type/
   * SearchApiString.php::build()` reads off `$this->results`.
   *
   * Term filters are **double-quoted**, the missing bucket is the bare `!`
   * sentinel. That is not a stylistic choice: Search API's own conformance
   * suite (`vendor/drupal/search_api/tests/src/Kernel/BackendTestBase.php`,
   * `checkFacets()` and friends) `assertEquals`es the raw extra-data array
   * against `['count' => 2, 'filter' => '"article_category"']`, i.e. before
   * any downstream unquoting, so a backend emitting bare terms fails the
   * suite. `search_api_db` writes the same shape
   * (`$row->value !== NULL ? '"' . $row->value . '"' : '!'`).
   *
   * @covers ::parse
   */
  public function testParsePopulatesSearchApiFacetsExtraData(): void {
    $facets = [
      'category' => [
        'field' => 'category',
        'limit' => 10,
        'min_count' => 1,
        'missing' => FALSE,
      ],
    ];
    $index = ['category' => $this->mockIndexField('category', 'string', FALSE)];
    $response = [
      'response' => ['numFound' => 5, 'start' => 0, 'docs' => []],
      'facet_counts' => [
        'facet_fields' => [
          'category' => ['animals', 2, 'classic', 2, 'garden', 1, 'misc', 1],
        ],
      ],
    ];

    $resultSet = (new ResponseParser())->parse($response, $this->mockQuery('my_index', $index, $facets));

    $this->assertSame([
      'category' => [
        ['count' => 2, 'filter' => '"animals"'],
        ['count' => 2, 'filter' => '"classic"'],
        ['count' => 1, 'filter' => '"garden"'],
        ['count' => 1, 'filter' => '"misc"'],
      ],
    ], $resultSet->getExtraData('search_api_facets'));
  }

  /**
   * @covers ::parse
   */
  public function testParseMultipleFacetFieldsEachGetTheirOwnDeltaKey(): void {
    $facets = [
      'category' => ['field' => 'category', 'limit' => 10, 'min_count' => 1, 'missing' => FALSE],
      'brand' => ['field' => 'brand', 'limit' => 10, 'min_count' => 1, 'missing' => FALSE],
    ];
    $index = [
      'category' => $this->mockIndexField('category', 'string', FALSE),
      'brand' => $this->mockIndexField('brand', 'string', TRUE),
    ];
    $response = [
      'response' => ['numFound' => 5, 'start' => 0, 'docs' => []],
      'facet_counts' => [
        'facet_fields' => [
          // #299: facet_fields is keyed by the facet delta (the {!key=...}
          // label QueryBuilder emitted), not the mapped field name.
          'category' => ['animals', 2, 'classic', 3],
          'brand' => ['acme', 4],
        ],
      ],
    ];

    $resultSet = (new ResponseParser())->parse($response, $this->mockQuery('my_index', $index, $facets));

    $extraData = $resultSet->getExtraData('search_api_facets');
    $this->assertSame([
      ['count' => 2, 'filter' => '"animals"'],
      ['count' => 3, 'filter' => '"classic"'],
    ], $extraData['category']);
    $this->assertSame([
      ['count' => 4, 'filter' => '"acme"'],
    ], $extraData['brand']);
  }

  /**
   * `facet_missing.json`: the missing bucket is the JSON literal `null` key
   * at the end of the flat pairs, unconditionally last, when `facet.missing`
   * was requested. `search_api_solr`'s `extractFacets()` (its Solarium
   * result reader) translates that bucket's empty term to the literal
   * filter string `"!"`, which the `facets` module's `SearchApiString`
   * query type specifically checks for (`isMissing() && $result_filter ===
   * '!'`) -- so `'!'` is not an arbitrary choice, it is the string contract
   * the contrib facets module expects for the "missing" bucket.
   *
   * @covers ::parse
   */
  public function testParseMissingBucketUsesBangFilter(): void {
    $facets = [
      'category' => ['field' => 'category', 'limit' => 10, 'min_count' => 1, 'missing' => TRUE],
    ];
    $index = ['category' => $this->mockIndexField('category', 'string', FALSE)];
    $response = [
      'response' => ['numFound' => 5, 'start' => 0, 'docs' => []],
      'facet_counts' => [
        'facet_fields' => [
          'category' => ['animals', 2, 'classic', 2, 'garden', 1, 'misc', 1, NULL, 1],
        ],
      ],
    ];

    $resultSet = (new ResponseParser())->parse($response, $this->mockQuery('my_index', $index, $facets));

    $extraData = $resultSet->getExtraData('search_api_facets') ?? [];
    $categoryTerms = $extraData['category'] ?? [];
    $this->assertNotEmpty($categoryTerms, 'search_api_facets extra data must include the "category" delta');
    $this->assertSame(['count' => 1, 'filter' => '!'], end($categoryTerms));
  }

  /**
   * No facets were requested: no `search_api_facets` extra data at all
   * (matching the pattern the rest of this codebase uses for optional,
   * request-dependent data -- e.g. `sort`/`start` are simply absent from
   * QueryBuilder's params array when not requested, rather than present with
   * an empty/default value).
   *
   * @covers ::parse
   */
  public function testParseSetsNoFacetsExtraDataWhenNoFacetsWereRequested(): void {
    $response = [
      'response' => ['numFound' => 0, 'start' => 0, 'docs' => []],
    ];

    $resultSet = (new ResponseParser())->parse($response, $this->mockQuery('my_index'));

    $this->assertFalse($resultSet->hasExtraData('search_api_facets'));
  }

  /**
   * #299: two Search API facets on the same field must each get their own
   * results. QueryBuilder emits a distinct {!key=<delta>} per facet, so the
   * core answers facet_fields under two distinct keys (one per delta) even
   * though both count the same column. Response shape derived from
   * solr-ref/responses/facet_extag_both_facets.json (one field, two {!key=}
   * labels, two distinct keys with different counts) -- not from what the
   * code produces.
   *
   * @covers ::parse
   */
  public function testParseAttachesBothDeltasWhenTwoFacetsShareOneField(): void {
    $facets = [
      'category_top' => ['field' => 'category', 'limit' => 10, 'min_count' => 1, 'missing' => FALSE],
      'category_all' => ['field' => 'category', 'limit' => 10, 'min_count' => 1, 'missing' => FALSE],
    ];
    $index = ['category' => $this->mockIndexField('category', 'string', FALSE)];
    // Both keys facet the same field; counts differ (mirroring
    // facet_extag_both_facets.json's filtered/unfiltered pair) to prove the
    // two results are not collapsed.
    $response = [
      'response' => ['numFound' => 5, 'start' => 0, 'docs' => []],
      'facet_counts' => [
        'facet_fields' => [
          'category_top' => ['animals', 2, 'classic', 1, 'garden', 0, 'misc', 0],
          'category_all' => ['animals', 2, 'classic', 2, 'garden', 1, 'misc', 1],
        ],
      ],
    ];

    $resultSet = (new ResponseParser())->parse($response, $this->mockQuery('my_index', $index, $facets));
    $extraData = $resultSet->getExtraData('search_api_facets');

    $this->assertSame([
      ['count' => 2, 'filter' => '"animals"'],
      ['count' => 1, 'filter' => '"classic"'],
      ['count' => 0, 'filter' => '"garden"'],
      ['count' => 0, 'filter' => '"misc"'],
    ], $extraData['category_top']);
    $this->assertSame([
      ['count' => 2, 'filter' => '"animals"'],
      ['count' => 2, 'filter' => '"classic"'],
      ['count' => 1, 'filter' => '"garden"'],
      ['count' => 1, 'filter' => '"misc"'],
    ], $extraData['category_all']);
  }

  /**
   * #299 fallback: when the facet delta is not a safe local-params value,
   * QueryBuilder emits the bare mapped field name (no {!key=} prefix), so the
   * core keys that facet's buckets by the field name. parseFacets must still
   * resolve that field-name key back to the hostile delta -- this is the
   * inverse of QueryBuilderTest::testHostileFacetDeltaFallsBackToBareFieldName.
   *
   * @covers ::parse
   */
  public function testParseResolvesAHostileDeltaByItsBareFieldNameKey(): void {
    // Delta contains a space: not [A-Za-z0-9_:-]+, so QueryBuilder falls back
    // to the bare mapped field name and the response is keyed by it.
    $facets = [
      'bad delta' => ['field' => 'category', 'limit' => 10, 'min_count' => 1, 'missing' => FALSE],
    ];
    $index = ['category' => $this->mockIndexField('category', 'string', FALSE)];
    $response = [
      'response' => ['numFound' => 5, 'start' => 0, 'docs' => []],
      'facet_counts' => [
        'facet_fields' => [
          'ss_category' => ['animals', 2, 'classic', 3],
        ],
      ],
    ];

    $resultSet = (new ResponseParser())->parse($response, $this->mockQuery('my_index', $index, $facets));

    $this->assertSame([
      ['count' => 2, 'filter' => '"animals"'],
      ['count' => 3, 'filter' => '"classic"'],
    ], $resultSet->getExtraData('search_api_facets')['bad delta']);
  }

  /**
   * M4 (issue #78): a Wayfinder /select response's "highlighting" block
   * (shape ground truth: solr-ref/responses/hl_basic.json --
   * {docId: {fieldName: [snippet, ...]}}) must populate each matching item's
   * "highlighted_fields" extra data, keyed back by Search API field id (not
   * the raw mapped dynamic field name), per plan doc locked decision 6
   * ("populating highlighted_fields extra data as search_api_solr does") and
   * ItemInterface::getExtraData()'s documented shape.
   *
   * @covers ::parse
   */
  public function testParsePopulatesHighlightedFieldsExtraDataKeyedByFieldId(): void {
    $query = $this->mockQueryWithFields(['body' => 'text', 'category' => 'string']);

    $response = [
      'response' => [
        'numFound' => 1,
        'start' => 0,
        'docs' => [
          ['id' => 'my_index-doc1', 'score' => 1.0],
        ],
      ],
      'highlighting' => [
        'my_index-doc1' => [
          'ts_body' => ['the quick <em>fox</em>'],
        ],
      ],
    ];

    $resultSet = (new ResponseParser())->parse($response, $query);
    $item = $resultSet->getResultItems()['doc1'];

    $this->assertSame(['body' => ['the quick <em>fox</em>']], $item->getExtraData('highlighted_fields'));
  }

  /**
   * Negative case: a plain (non-highlighted) response leaves items without
   * any "highlighted_fields" extra data at all, rather than setting an empty
   * array -- callers use hasExtraData() to decide whether to fall back to
   * Search API's own core "highlight" processor (plan doc line 84-87).
   *
   * @covers ::parse
   */
  public function testParseSetsNoHighlightedFieldsExtraDataWhenResponseHasNoHighlightingBlock(): void {
    $query = $this->mockQueryWithFields(['body' => 'text']);

    $response = [
      'response' => [
        'numFound' => 1,
        'start' => 0,
        'docs' => [
          ['id' => 'my_index-doc1', 'score' => 1.0],
        ],
      ],
    ];

    $resultSet = (new ResponseParser())->parse($response, $query);
    $item = $resultSet->getResultItems()['doc1'];

    $this->assertFalse($item->hasExtraData('highlighted_fields'));
  }

}
