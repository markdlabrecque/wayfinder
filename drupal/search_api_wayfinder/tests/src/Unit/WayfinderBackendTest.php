<?php

declare(strict_types=1);

namespace Drupal\Tests\search_api_wayfinder\Unit;

use Drupal\Core\Field\FieldDefinitionInterface;
use Drupal\Core\Field\FieldStorageDefinitionInterface;
use Drupal\search_api\IndexInterface;
use Drupal\search_api\Item\FieldInterface;
use Drupal\search_api\Query\ConditionGroup;
use Drupal\search_api\Query\QueryInterface;
use Drupal\search_api\Query\ResultSet;
use Drupal\search_api_wayfinder\Plugin\search_api\backend\WayfinderBackend;
use Drupal\search_api_wayfinder\WayfinderClient;
use PHPUnit\Framework\TestCase;

/**
 * Tests WayfinderBackend's feature flags and the /select vs /mlt routing
 * decision in search() (M4, issue #78).
 *
 * getClient() builds a real WayfinderClient from Guzzle config, so these
 * tests use a minimal subclass that substitutes a mocked WayfinderClient
 * instead of touching HTTP at all -- WayfinderBackend's own HTTP behaviour is
 * WayfinderClientTest's job, not this class's.
 *
 * @coversDefaultClass \Drupal\search_api_wayfinder\Plugin\search_api\backend\WayfinderBackend
 * @group search_api_wayfinder
 */
class WayfinderBackendTest extends TestCase {

  /**
   * @covers ::getSupportedFeatures
   */
  public function testGetSupportedFeaturesIncludesSearchApiFacets(): void {
    $backend = new WayfinderBackend([], 'wayfinder', []);

    $this->assertContains('search_api_facets', $backend->getSupportedFeatures());
  }

  /**
   * A WayfinderBackend whose getClient() returns an injected mock instead of
   * constructing a real Guzzle-backed WayfinderClient.
   *
   * @param array $configuration
   *   Extra plugin configuration, merged over defaultConfiguration() by
   *   ConfigurablePluginBase's constructor -- used by the highlighting tests
   *   below to set the backend-level "highlight" flag (see the correction
   *   note on QueryBuilderTest's hl tests: this is a config setting, not a
   *   query option, per search_api_solr's own convention).
   */
  private function backendWithClient(WayfinderClient $client, array $configuration = []): WayfinderBackend {
    return new class($configuration, 'wayfinder', [], $client) extends WayfinderBackend {
      private WayfinderClient $testClient;

      public function __construct(array $configuration, $plugin_id, array $plugin_definition, WayfinderClient $client) {
        parent::__construct($configuration, $plugin_id, $plugin_definition);
        $this->testClient = $client;
      }

      protected function getClient(): WayfinderClient {
        return $this->testClient;
      }
    };
  }

  private function mockIndex(string $indexId = 'my_index'): IndexInterface {
    $field = $this->createMock(FieldInterface::class);
    $field->method('getFieldIdentifier')->willReturn('title');
    $field->method('getType')->willReturn('text');
    $field->method('getPropertyPath')->willReturn('title');
    $field->method('getDatasourceId')->willReturn('entity:test');

    $storage = $this->createMock(FieldStorageDefinitionInterface::class);
    $storage->method('getCardinality')->willReturn(1);
    $definition = $this->createMock(FieldDefinitionInterface::class);
    $definition->method('isList')->willReturn(TRUE);
    $definition->method('getFieldStorageDefinition')->willReturn($storage);

    $index = $this->createMock(IndexInterface::class);
    $index->method('id')->willReturn($indexId);
    $index->method('getFulltextFields')->willReturn(['title']);
    $index->method('getField')->willReturnCallback(
      fn (string $id) => $id === 'title' ? $field : NULL
    );
    $index->method('getFields')->willReturn(['title' => $field]);
    $index->method('getPropertyDefinitions')->willReturn(['title' => $definition]);
    $field->method('getIndex')->willReturn($index);

    return $index;
  }

  private function mockQuery(IndexInterface $index, array $options = []): QueryInterface {
    $query = $this->createMock(QueryInterface::class);
    $query->method('getKeys')->willReturn(NULL);
    $query->method('getFulltextFields')->willReturn(NULL);
    $query->method('getIndex')->willReturn($index);
    $query->method('getConditionGroup')->willReturn(new ConditionGroup());
    $query->method('getSorts')->willReturn([]);
    $query->method('getOption')->willReturnCallback(
      static fn (string $name, $default = NULL) => $options[$name] ?? $default
    );
    $resultSet = new ResultSet($query);
    $query->method('getResults')->willReturn($resultSet);

    return $query;
  }

  /**
   * getSupportedFeatures() currently returns ['search_api_facets'] (M3);
   * M4 (plan doc line 124 / issue #78) adds "search_api_mlt". Deliberately
   * not asserting the array's exact shape -- only that MLT support is now
   * advertised.
   *
   * @covers ::getSupportedFeatures
   */
  public function testGetSupportedFeaturesIncludesSearchApiMlt(): void {
    $backend = new WayfinderBackend([], 'wayfinder', []);

    $this->assertContains('search_api_mlt', $backend->getSupportedFeatures());
  }

  /**
   * Plan doc line 170-171: the "search_api_mlt" query option routes to
   * GET /mlt instead of /select.
   *
   * @covers ::search
   */
  public function testSearchRoutesToMltEndpointWhenMltOptionIsSet(): void {
    $index = $this->mockIndex();
    $query = $this->mockQuery($index, [
      'search_api_mlt' => ['id' => 'entity:node/1:en', 'fields' => ['title']],
    ]);

    $client = $this->createMock(WayfinderClient::class);
    $client->expects($this->never())->method('select');
    $client->expects($this->once())
      ->method('mlt')
      ->with($this->callback(fn (array $params) => $params['q'] === 'id:"my_index-entity:node/1:en"'))
      ->willReturn(['response' => ['numFound' => 0, 'docs' => []]]);

    $backend = $this->backendWithClient($client);
    $backend->search($query);
  }

  /**
   * Negative case: without the "search_api_mlt" option, search() keeps using
   * /select -- MLT routing must not leak into ordinary fulltext searches.
   *
   * @covers ::search
   */
  public function testSearchRoutesToSelectEndpointWhenMltOptionIsAbsent(): void {
    $index = $this->mockIndex();
    $query = $this->mockQuery($index);

    $client = $this->createMock(WayfinderClient::class);
    $client->expects($this->never())->method('mlt');
    $client->expects($this->once())
      ->method('select')
      ->willReturn(['response' => ['numFound' => 0, 'docs' => []]]);

    $backend = $this->backendWithClient($client);
    $backend->search($query);
  }

  /**
   * Negative case (and default): a plain search with the "highlight" config
   * left off (default FALSE) leaves result items without
   * "highlighted_fields" extra data -- see ResponseParserTest for the
   * positive (highlighting-populated) case, which is ResponseParser's
   * responsibility, not the backend's.
   *
   * @covers ::search
   */
  public function testSearchLeavesNoHighlightedFieldsExtraDataWithoutHighlighting(): void {
    $index = $this->mockIndex();
    $query = $this->mockQuery($index);

    $client = $this->createMock(WayfinderClient::class);
    $client->method('select')->willReturn([
      'response' => [
        'numFound' => 1,
        'start' => 0,
        'docs' => [['id' => 'my_index-doc1', 'score' => 1.0]],
      ],
    ]);

    $backend = $this->backendWithClient($client);
    $backend->search($query);

    $item = $query->getResults()->getResultItems()['doc1'];
    $this->assertFalse($item->hasExtraData('highlighted_fields'));
  }

  /**
   * Correction (orchestrator review): highlighting is triggered by a
   * backend-level config setting, not a query option (search_api_solr's own
   * convention, which the plan doc pins this to -- "populating
   * highlighted_fields extra data as search_api_solr does"). So search()
   * must read its own configuration (not $query->getOption()) to decide
   * whether to request hl at all.
   *
   * @covers ::search
   */
  public function testSearchRequestsHlWhenBackendIsConfiguredForHighlighting(): void {
    $index = $this->mockIndex();
    $query = $this->mockQuery($index);

    $client = $this->createMock(WayfinderClient::class);
    $client->expects($this->once())
      ->method('select')
      ->with($this->callback(fn (array $params) => ($params['hl'] ?? NULL) === 'true'))
      ->willReturn(['response' => ['numFound' => 0, 'docs' => []]]);

    $backend = $this->backendWithClient($client, ['highlight' => TRUE]);
    $backend->search($query);
  }

}
