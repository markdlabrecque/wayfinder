<?php

declare(strict_types=1);

namespace Drupal\Tests\search_api_wayfinder\Unit;

use Drupal\search_api\IndexInterface;
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

  private function mockQuery(string $indexId = 'my_index'): QueryInterface {
    $index = $this->createMock(IndexInterface::class);
    $index->method('id')->willReturn($indexId);

    $query = $this->createMock(QueryInterface::class);
    $query->method('getIndex')->willReturn($index);

    // Query::execute() creates the ResultSet up front and hands it to the
    // backend via getResults(); ResponseParser::parse() must populate that
    // existing object rather than constructing a new one.
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

}
