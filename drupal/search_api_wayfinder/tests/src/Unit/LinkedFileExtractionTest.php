<?php

declare(strict_types=1);

namespace Drupal\Tests\search_api_wayfinder\Unit;

use Drupal\Core\Entity\ContentEntityInterface;
use Drupal\Core\Entity\EntityStorageInterface;
use Drupal\Core\Entity\EntityTypeManagerInterface;
use Drupal\Core\Field\FieldDefinitionInterface;
use Drupal\Core\Field\FieldItemListInterface;
use Drupal\Core\KeyValueStore\KeyValueStoreInterface;
use Drupal\Core\Queue\QueueInterface;
use Drupal\Core\TypedData\ComplexDataInterface;
use Drupal\file\FileInterface;
use Drupal\search_api\Datasource\DatasourceInterface;
use Drupal\search_api\IndexInterface;
use Drupal\search_api\Item\FieldInterface;
use Drupal\search_api\Item\ItemInterface;
use Drupal\search_api\Processor\ProcessorProperty;
use Drupal\search_api\SearchApiException;
use Drupal\search_api\ServerInterface;
use Drupal\search_api\Utility\FieldsHelperInterface;
use Drupal\search_api_wayfinder\Cache\ExtractionCacheInterface;
use Drupal\search_api_wayfinder\ExtractionInvalidator;
use Drupal\search_api_wayfinder\FileReferenceMap;
use Drupal\search_api_wayfinder\FileReferenceMapInterface;
use Drupal\search_api_wayfinder\LinkedFileDiscovererInterface;
use Drupal\search_api_wayfinder\Plugin\search_api\backend\WayfinderBackend;
use Drupal\search_api_wayfinder\Plugin\search_api\processor\LinkedFileExtraction;
use PHPUnit\Framework\TestCase;
use Psr\Log\LoggerInterface;

/**
 * Tests the LinkedFileExtraction processor (issue #265): the supportsIndex()
 * gate, the single aggregate saw_linked computed property, and addFieldValues()
 * discovering files linked from configured text/link fields and indexing their
 * extracted text -- plus the acceptance test that a changed linked file
 * reindexes every referencing item.
 *
 * The discovery mechanisms themselves are LinkedFileDiscovererTest's concern;
 * this test mocks the discoverer so the processor wiring is exercised in
 * isolation. The extraction cache/queue/map loop is FileExtractionTest's
 * concern and is shared via FileExtractionProcessorBase; here we prove the
 * linked path records references into the same map and feeds the same loop.
 *
 * @coversDefaultClass \Drupal\search_api_wayfinder\Plugin\search_api\processor\LinkedFileExtraction
 * @group search_api_wayfinder
 */
class LinkedFileExtractionTest extends TestCase {

  /**
   * The processor under test, rebuilt per test with that test's collaborators.
   */
  private function processor(
    ?LinkedFileDiscovererInterface $discoverer = NULL,
    ?EntityTypeManagerInterface $entityTypeManager = NULL,
    ?LoggerInterface $logger = NULL,
    ?ExtractionCacheInterface $cache = NULL,
    ?QueueInterface $queue = NULL,
    ?FileReferenceMapInterface $fileMap = NULL,
  ): LinkedFileExtraction {
    $processor = new LinkedFileExtraction(
      [],
      'wayfinder_linked_file_extraction',
      ['id' => 'wayfinder_linked_file_extraction', 'label' => 'Wayfinder linked file extraction'],
      $discoverer ?? $this->discovererReturning([]),
      $entityTypeManager ?? $this->createMock(EntityTypeManagerInterface::class),
      $logger ?? $this->createMock(LoggerInterface::class),
      $cache,
      $queue,
      $fileMap,
    );
    // $this->t() in getPropertyDefinitions() resolves the string-translation
    // service lazily without a bootstrapped Drupal; inject a no-op here.
    $processor->setStringTranslation($this->createMock(\Drupal\Core\StringTranslation\TranslationInterface::class));
    return $processor;
  }

  /**
   * A discoverer double that returns $files for any html or uri it is asked
   * about. Asserting the right text/uri was passed is per-test via with().
   */
  private function discovererReturning(array $files): LinkedFileDiscovererInterface {
    $discoverer = $this->createMock(LinkedFileDiscovererInterface::class);
    $discoverer->method('discoverFromHtml')->willReturn($files);
    $discoverer->method('discoverFromLinkUri')->willReturn($files);
    return $discoverer;
  }

  /**
   * @covers ::supportsIndex
   */
  public function testSupportsIndexReturnsTrueForWayfinderBackend(): void {
    $server = $this->createMock(ServerInterface::class);
    $server->method('getBackendId')->willReturn('wayfinder');
    $index = $this->createMock(IndexInterface::class);
    $index->method('getServerInstanceIfAvailable')->willReturn($server);

    $this->assertTrue(LinkedFileExtraction::supportsIndex($index));
  }

  /**
   * @covers ::supportsIndex
   */
  public function testSupportsIndexReturnsFalseForOtherBackends(): void {
    $server = $this->createMock(ServerInterface::class);
    $server->method('getBackendId')->willReturn('search_api_solr');
    $index = $this->createMock(IndexInterface::class);
    $index->method('getServerInstanceIfAvailable')->willReturn($server);

    $this->assertFalse(LinkedFileExtraction::supportsIndex($index));
  }

  /**
   * @covers ::supportsIndex
   */
  public function testSupportsIndexReturnsFalseWhenNoServerAvailable(): void {
    $index = $this->createMock(IndexInterface::class);
    $index->method('getServerInstanceIfAvailable')->willReturn(NULL);

    $this->assertFalse(LinkedFileExtraction::supportsIndex($index));
  }

  /**
   * The processor declares exactly one aggregate property, saw_linked, that an
   * admin adds once for all linked-document text (the issue's "a separate
   * field with a lower boost" -- fan-out is one field, not one per source).
   *
   * @covers ::getPropertyDefinitions
   */
  public function testGetPropertyDefinitionsDeclaresTheSingleSawLinkedProperty(): void {
    $processor = $this->processor();

    $properties = $processor->getPropertyDefinitions(NULL);

    $this->assertSame(['saw_linked'], array_keys($properties));
    $this->assertInstanceOf(ProcessorProperty::class, $properties['saw_linked']);
  }

  /**
   * Processor-defined properties are index-level (datasource NULL).
   *
   * @covers ::getPropertyDefinitions
   */
  public function testGetPropertyDefinitionsReturnsEmptyForASpecificDatasource(): void {
    $processor = $this->processor();

    $this->assertSame([], $processor->getPropertyDefinitions($this->createMock(DatasourceInterface::class)));
  }

  /**
   * @covers ::addFieldValues
   */
  public function testAddFieldValuesDiscoversLinkedFilesFromBodyTextAndPopulatesTheField(): void {
    $bodyHtml = '<a href="/sites/default/files/report.pdf">report</a>';
    $file = $this->fileEntity(7);

    $discoverer = $this->createMock(LinkedFileDiscovererInterface::class);
    $discoverer->expects($this->once())->method('discoverFromHtml')->with($bodyHtml)->willReturn([7 => $file]);

    $backend = $this->createMock(WayfinderBackend::class);
    $backend->expects($this->once())->method('extractContentFromFile')->willReturn('report text');

    $indexField = $this->createMock(FieldInterface::class);
    $indexField->expects($this->once())->method('addValue')->with('report text');

    $fieldsHelper = $this->createMock(FieldsHelperInterface::class);
    $fieldsHelper->method('filterForPropertyPath')->willReturn(['saw_linked' => $indexField]);

    $processor = $this->processor($discoverer);
    $processor->setFieldsHelper($fieldsHelper);
    $processor->setIndex($this->indexWithScannableFields($backend));

    $processor->addFieldValues($this->itemWithBody($bodyHtml, $indexField));
  }

  /**
   * @covers ::addFieldValues
   */
  public function testAddFieldValuesDiscoversLinkedFilesFromALinkFieldUri(): void {
    $file = $this->fileEntity(2);

    $discoverer = $this->createMock(LinkedFileDiscovererInterface::class);
    $discoverer->expects($this->once())->method('discoverFromLinkUri')->with('entity:file/2')->willReturn([2 => $file]);
    $discoverer->method('discoverFromHtml')->willReturn([]);

    $backend = $this->createMock(WayfinderBackend::class);
    $backend->method('extractContentFromFile')->willReturn('linked doc text');

    $indexField = $this->createMock(FieldInterface::class);
    $indexField->expects($this->once())->method('addValue')->with('linked doc text');

    $fieldsHelper = $this->createMock(FieldsHelperInterface::class);
    $fieldsHelper->method('filterForPropertyPath')->willReturn(['saw_linked' => $indexField]);

    $processor = $this->processor($discoverer);
    $processor->setFieldsHelper($fieldsHelper);
    $processor->setIndex($this->indexWithScannableFields($backend));

    $processor->addFieldValues($this->itemWithLinkField('entity:file/2', $indexField));
  }

  /**
   * If the admin never added saw_linked to the index, the processor does no
   * discovery at all -- no wasted HTML parsing for unconfigured indexes.
   *
   * @covers ::addFieldValues
   */
  public function testAddFieldValuesDoesNoDiscoveryWhenSawLinkedIsNotOnTheItem(): void {
    $discoverer = $this->createMock(LinkedFileDiscovererInterface::class);
    $discoverer->expects($this->never())->method('discoverFromHtml');
    $discoverer->expects($this->never())->method('discoverFromLinkUri');

    $fieldsHelper = $this->createMock(FieldsHelperInterface::class);
    $fieldsHelper->method('filterForPropertyPath')->willReturn([]);

    $processor = $this->processor($discoverer);
    $processor->setFieldsHelper($fieldsHelper);
    $processor->setIndex($this->indexWithScannableFields($this->createMock(WayfinderBackend::class)));

    $processor->addFieldValues($this->itemWithBody('<a href="/x">x</a>', $this->createMock(FieldInterface::class)));
  }

  /**
   * A discovered file that yields no extractable text (empty extraction) leaves
   * the field untouched -- mirroring FileExtraction's empty-value skip.
   *
   * @covers ::addFieldValues
   */
  public function testAddFieldValuesLeavesTheFieldEmptyWhenExtractionYieldsNothing(): void {
    $file = $this->fileEntity(7);
    $discoverer = $this->discovererReturning([7 => $file]);

    $backend = $this->createMock(WayfinderBackend::class);
    $backend->method('extractContentFromFile')->willReturn('');

    $indexField = $this->createMock(FieldInterface::class);
    $indexField->expects($this->never())->method('addValue');

    $fieldsHelper = $this->createMock(FieldsHelperInterface::class);
    $fieldsHelper->method('filterForPropertyPath')->willReturn(['saw_linked' => $indexField]);

    $processor = $this->processor($discoverer);
    $processor->setFieldsHelper($fieldsHelper);
    $processor->setIndex($this->indexWithScannableFields($backend));

    $processor->addFieldValues($this->itemWithBody('<a href="/x">x</a>', $indexField));
  }

  /**
   * If the underlying object can't be loaded (deleted entity, etc.)
   * addFieldValues no-ops rather than crashing.
   *
   * @covers ::addFieldValues
   */
  public function testAddFieldValuesNoOpsWhenTheOriginalObjectCannotBeLoaded(): void {
    $discoverer = $this->createMock(LinkedFileDiscovererInterface::class);
    $discoverer->expects($this->never())->method('discoverFromHtml');

    $fieldsHelper = $this->createMock(FieldsHelperInterface::class);
    $fieldsHelper->method('filterForPropertyPath')->willReturn(['saw_linked' => $this->createMock(FieldInterface::class)]);

    $processor = $this->processor($discoverer);
    $processor->setFieldsHelper($fieldsHelper);
    $processor->setIndex($this->indexWithScannableFields($this->createMock(WayfinderBackend::class)));

    // saw_linked is on the item so the gate passes and execution reaches
    // getEntity(), which is where the no-op must happen.
    $item = $this->createMock(ItemInterface::class);
    $item->method('getFields')->willReturn(['saw_linked' => $this->createMock(FieldInterface::class)]);
    $item->method('getOriginalObject')->willThrowException(new SearchApiException('Failed to load original object'));

    $processor->addFieldValues($item);

    $this->addToAssertionCount(1);
  }

  /**
   * The processor records every (linked file -> item) reference it indexes into
   * the file map, so a later file change or delete can mark this item for
   * reindex (#263 invalidation) -- the map is the explicit reference a URL link
   * has no entity-reference field to hang on.
   *
   * @covers ::addFieldValues
   */
  public function testAddFieldValuesRecordsTheLinkedFileItemReferenceInTheMap(): void {
    $file = $this->fileEntity(7);
    $discoverer = $this->discovererReturning([7 => $file]);

    $backend = $this->createMock(WayfinderBackend::class);
    $backend->method('extractContentFromFile')->willReturn('text');

    $indexField = $this->createMock(FieldInterface::class);
    $fieldsHelper = $this->createMock(FieldsHelperInterface::class);
    $fieldsHelper->method('filterForPropertyPath')->willReturn(['saw_linked' => $indexField]);

    $bag = [];
    $map = new FileReferenceMap($this->inMemoryStore($bag));
    $processor = $this->processor($discoverer, NULL, NULL, NULL, NULL, $map);
    $processor->setFieldsHelper($fieldsHelper);

    $index = $this->indexWithScannableFields($backend);
    $index->method('id')->willReturn('content');
    $processor->setIndex($index);

    $item = $this->itemWithBody('<a href="/x">x</a>', $indexField);
    $item->method('getId')->willReturn('entity:node/1');
    $processor->addFieldValues($item);

    $this->assertSame([['index' => 'content', 'item' => 'entity:node/1']], $map->itemsForFile(7));
  }

  /**
   * A linked file shared by many items is extracted once (cache hit on the
   * second) and recorded for both -- proving the fan-out hard part (one PDF
   * linked from fifty nodes) is handled by the shared cache + map, not fifty
   * re-extractions.
   *
   * @covers ::addFieldValues
   */
  public function testAFannedOutLinkedFileIsExtractedOnceAndRecordedForEachItem(): void {
    $path = tempnam(sys_get_temp_dir(), 'wf_linked_');
    file_put_contents($path, 'shared doc body');

    // A real file backed by a path so the content-hash cache keys it.
    $file = $this->createMock(FileInterface::class);
    $file->method('id')->willReturn(8);
    $file->method('getFileUri')->willReturn($path);

    $discoverer = $this->discovererReturning([8 => $file]);

    $backend = $this->createMock(WayfinderBackend::class);
    // The load-bearing assertion: extraction runs ONCE despite two items.
    $backend->expects($this->once())->method('extractContentFromFile')->with($path)->willReturn('shared text');

    $indexField = $this->createMock(FieldInterface::class);
    $indexField->expects($this->exactly(2))->method('addValue')->with('shared text');

    $fieldsHelper = $this->createMock(FieldsHelperInterface::class);
    $fieldsHelper->method('filterForPropertyPath')->willReturn(['saw_linked' => $indexField]);

    $store = [];
    $cache = new \Drupal\search_api_wayfinder\Cache\KeyValueExtractionCache($this->inMemoryStore($store));
    $bag = [];
    $map = new FileReferenceMap($this->inMemoryStore($bag));
    $processor = $this->processor($discoverer, NULL, NULL, $cache, NULL, $map);
    $processor->setFieldsHelper($fieldsHelper);

    $index = $this->indexWithScannableFields($backend);
    $index->method('id')->willReturn('content');
    $processor->setIndex($index);

    $item1 = $this->itemWithBody('<a href="/x">x</a>', $indexField);
    $item1->method('getId')->willReturn('entity:node/1');
    $item2 = $this->itemWithBody('<a href="/y">y</a>', $indexField);
    $item2->method('getId')->willReturn('entity:node/2');

    $processor->addFieldValues($item1);
    $processor->addFieldValues($item2);

    // Both items are recorded as referencing the shared file.
    $refs = $map->itemsForFile(8);
    $this->assertCount(2, $refs);
    $this->assertSame([['index' => 'content', 'item' => 'entity:node/1'], ['index' => 'content', 'item' => 'entity:node/2']], $refs);
  }

  /**
   * Acceptance test (issue #265): a changed linked file reindexes every
   * referencing item. Two items index a shared linked file, populating the map;
   * ExtractionInvalidator::onFileUpdate() then marks BOTH for reindex via
   * trackItemsUpdated(). This is the URL-link case #263's invalidation was
   * shaped for (no entity reference to derive the mapping from).
   *
   * @covers ::addFieldValues
   */
  public function testAChangedLinkedFileReindexesEveryReferencingItem(): void {
    $file = $this->fileEntity(9);
    $discoverer = $this->discovererReturning([9 => $file]);

    $backend = $this->createMock(WayfinderBackend::class);
    $backend->method('extractContentFromFile')->willReturn('text');

    $indexField = $this->createMock(FieldInterface::class);
    $fieldsHelper = $this->createMock(FieldsHelperInterface::class);
    $fieldsHelper->method('filterForPropertyPath')->willReturn(['saw_linked' => $indexField]);

    $bag = [];
    $map = new FileReferenceMap($this->inMemoryStore($bag));

    // The index records every trackItemsUpdated() call so the assertion can
    // prove both items were marked.
    $invalidated = [];
    $index = $this->indexWithScannableFields($backend);
    $index->method('id')->willReturn('content');
    $index->method('trackItemsUpdated')
      ->willReturnCallback(function (string $datasource, array $rawIds) use (&$invalidated): void {
        foreach ($rawIds as $id) {
          $invalidated[$datasource][] = $id;
        }
      });

    $processor = $this->processor($discoverer, NULL, NULL, NULL, NULL, $map);
    $processor->setFieldsHelper($fieldsHelper);
    $processor->setIndex($index);

    $item1 = $this->itemWithBody('<a href="/x">x</a>', $indexField);
    $item1->method('getId')->willReturn('entity:node/1');
    $item2 = $this->itemWithBody('<a href="/y">y</a>', $indexField);
    $item2->method('getId')->willReturn('entity:node/2');

    $processor->addFieldValues($item1);
    $processor->addFieldValues($item2);

    // The same index entity is what the invalidator loads by id.
    $indexStorage = $this->createMock(EntityStorageInterface::class);
    $indexStorage->method('loadMultiple')->willReturn(['content' => $index]);
    $etm = $this->createMock(EntityTypeManagerInterface::class);
    $etm->method('getStorage')->willReturn($indexStorage);

    $invalidator = new ExtractionInvalidator($map, $etm);
    $invalidator->onFileUpdate($file);

    $this->assertSame(['entity:node' => ['1', '2']], $invalidated);
  }

  // ---------------------------------------------------------------- helpers.

  /**
   * Builds the index mock shared by the addFieldValues tests: it advertises a
   * body text field and a link field, and points at $backend via its server.
   */
  private function indexWithScannableFields(WayfinderBackend $backend): IndexInterface {
    $server = $this->createMock(ServerInterface::class);
    $server->method('getBackend')->willReturn($backend);

    $index = $this->createMock(IndexInterface::class);
    $index->method('getDatasources')->willReturn([$this->datasourceWithBodyAndLink()]);
    $index->method('getServerInstanceIfAvailable')->willReturn($server);
    return $index;
  }

  /**
   * A datasource exposing a body (text_with_summary) and a link field, the two
   * field shapes the linked-file processor scans.
   */
  private function datasourceWithBodyAndLink(): DatasourceInterface {
    $body = $this->createMock(FieldDefinitionInterface::class);
    $body->method('getType')->willReturn('text_with_summary');
    $body->method('getName')->willReturn('body');

    $link = $this->createMock(FieldDefinitionInterface::class);
    $link->method('getType')->willReturn('link');
    $link->method('getName')->willReturn('field_link');

    $datasource = $this->createMock(DatasourceInterface::class);
    $datasource->method('getPluginId')->willReturn('entity:node');
    $datasource->method('getPropertyDefinitions')->willReturn([
      'body' => $body,
      'field_link' => $link,
      'title' => $this->titleField(),
    ]);
    return $datasource;
  }

  /**
   * A non-text, non-link field the processor must skip.
   */
  private function titleField(): FieldDefinitionInterface {
    $title = $this->createMock(FieldDefinitionInterface::class);
    $title->method('getType')->willReturn('string');
    return $title;
  }

  /**
   * An item whose original object is a node carrying the body field with
   * $bodyHtml as its value.
   */
  private function itemWithBody(string $bodyHtml, FieldInterface $indexField): ItemInterface {
    $bodyList = $this->createMock(FieldItemListInterface::class);
    $bodyList->method('getValue')->willReturn([['value' => $bodyHtml, 'format' => 'full_html']]);

    $node = $this->createMock(ContentEntityInterface::class);
    $node->method('hasField')->willReturnCallback(fn (string $name) => $name === 'body');
    $node->method('get')->willReturnCallback(fn (string $name) => $name === 'body' ? $bodyList : NULL);

    return $this->itemWrapping($node, $indexField);
  }

  /**
   * An item whose original object is a node carrying a link field with $uri.
   */
  private function itemWithLinkField(string $uri, FieldInterface $indexField): ItemInterface {
    $linkList = $this->createMock(FieldItemListInterface::class);
    $linkList->method('getValue')->willReturn([['uri' => $uri, 'title' => 'a doc']]);

    $node = $this->createMock(ContentEntityInterface::class);
    $node->method('hasField')->willReturnCallback(fn (string $name) => $name === 'field_link');
    $node->method('get')->willReturnCallback(fn (string $name) => $name === 'field_link' ? $linkList : NULL);

    return $this->itemWrapping($node, $indexField);
  }

  /**
   * Wraps a node in an item with the saw_linked index field available.
   */
  private function itemWrapping(ContentEntityInterface $node, FieldInterface $indexField): ItemInterface {
    $adapter = $this->createMock(ComplexDataInterface::class);
    $adapter->method('getValue')->willReturn($node);

    $item = $this->createMock(ItemInterface::class);
    $item->method('getOriginalObject')->willReturn($adapter);
    $item->method('getFields')->willReturn(['saw_linked' => $indexField]);
    return $item;
  }

  /**
   * A FileInterface mock with an id and a uri. getFileUri() must be stubbed so
   * the extraction path (which calls it) does not raise a typed-null TypeError
   * that the processor's failure try/catch would silently swallow.
   */
  private function fileEntity(int $id): FileInterface {
    $file = $this->createMock(FileInterface::class);
    $file->method('id')->willReturn($id);
    $file->method('getFileUri')->willReturn('private://linked-' . $id . '.pdf');
    return $file;
  }

  /**
   * An in-memory keyvalue store double so the map round-trips without a DB.
   */
  private function inMemoryStore(array &$store): KeyValueStoreInterface {
    $kv = $this->createMock(KeyValueStoreInterface::class);
    // Both callbacks must capture $store by reference: an arrow fn captures by
    // value (a snapshot at definition time) and so would never see writes.
    $kv->method('get')->willReturnCallback(function ($key) use (&$store) {
      return array_key_exists($key, $store) ? $store[$key] : NULL;
    });
    $kv->method('set')->willReturnCallback(function ($key, $value) use (&$store): void {
      $store[$key] = $value;
    });
    return $kv;
  }

}
