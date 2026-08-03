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
use Drupal\search_api_wayfinder\Cache\KeyValueExtractionCache;
use Drupal\search_api_wayfinder\FileReferenceMap;
use Drupal\search_api_wayfinder\FileReferenceMapInterface;
use Drupal\search_api_wayfinder\Plugin\search_api\backend\WayfinderBackend;
use Drupal\search_api_wayfinder\Plugin\search_api\processor\FileExtraction;
use PHPUnit\Framework\TestCase;
use Psr\Log\LoggerInterface;

/**
 * Tests the FileExtraction processor (issue #262 tracer): the
 * supportsIndex() gate to WayfinderBackend indexes, the per-file-field
 * computed properties it declares, and addFieldValues() populating them by
 * calling WayfinderBackend::extractContentFromFile() -- including the
 * hard requirement that an extraction failure is logged and skipped rather
 * than failing the index batch.
 *
 * The /update/extract wire shape (multipart part name "file", extractOnly=true,
 * extractFormat=text) and the response envelope are WayfinderClientTest /
 * WayfinderBackendTest's concern; this test mocks WayfinderBackend so the
 * processor logic is exercised without HTTP.
 *
 * @coversDefaultClass \Drupal\search_api_wayfinder\Plugin\search_api\processor\FileExtraction
 * @group search_api_wayfinder
 */
class FileExtractionTest extends TestCase {

  /**
   * The processor under test. Rebuilt per test with that test's collaborators.
   */
  private function processor(?EntityTypeManagerInterface $entityTypeManager = NULL, ?LoggerInterface $logger = NULL, ?ExtractionCacheInterface $cache = NULL, ?QueueInterface $queue = NULL, ?FileReferenceMapInterface $fileMap = NULL): FileExtraction {
    $processor = new FileExtraction(
      [],
      'wayfinder_file_extraction',
      ['id' => 'wayfinder_file_extraction', 'label' => 'Wayfinder file extraction'],
      $entityTypeManager ?? $this->createMock(EntityTypeManagerInterface::class),
      $logger ?? $this->createMock(LoggerInterface::class),
      $cache,
      $queue,
      $fileMap,
    );
    // $this->t() in getPropertyDefinitions() resolves the string-translation
    // service lazily; without a bootstrapped Drupal that hits \Drupal::service,
    // so inject a no-op translation here. The label text is never asserted.
    $processor->setStringTranslation($this->createMock(\Drupal\Core\StringTranslation\TranslationInterface::class));
    return $processor;
  }

  /**
   * A datasource exposing one file field (field_files) and one non-file field
   * (title), so the "one computed property per file field" behaviour has
   * something to discriminate against.
   */
  private function datasourceWithFileField(): DatasourceInterface {
    $fileField = $this->createMock(FieldDefinitionInterface::class);
    $fileField->method('getType')->willReturn('file');
    $fileField->method('getName')->willReturn('field_files');
    $fileField->method('getLabel')->willReturn('Files');

    $titleField = $this->createMock(FieldDefinitionInterface::class);
    $titleField->method('getType')->willReturn('string');

    $datasource = $this->createMock(DatasourceInterface::class);
    $datasource->method('getPluginId')->willReturn('entity:node');
    $datasource->method('getPropertyDefinitions')->willReturn([
      'field_files' => $fileField,
      'title' => $titleField,
    ]);
    return $datasource;
  }

  /**
   * @covers ::supportsIndex
   */
  public function testSupportsIndexReturnsTrueForWayfinderBackend(): void {
    $server = $this->createMock(ServerInterface::class);
    $server->method('getBackendId')->willReturn('wayfinder');
    $index = $this->createMock(IndexInterface::class);
    $index->method('getServerInstanceIfAvailable')->willReturn($server);

    $this->assertTrue(FileExtraction::supportsIndex($index));
  }

  /**
   * @covers ::supportsIndex
   */
  public function testSupportsIndexReturnsFalseForOtherBackends(): void {
    $server = $this->createMock(ServerInterface::class);
    $server->method('getBackendId')->willReturn('search_api_solr');
    $index = $this->createMock(IndexInterface::class);
    $index->method('getServerInstanceIfAvailable')->willReturn($server);

    $this->assertFalse(FileExtraction::supportsIndex($index));
  }

  /**
   * An index without an available server (unsaved, or a server that could not
   * be loaded) must not crash the static gate -- it just opts out.
   *
   * @covers ::supportsIndex
   */
  public function testSupportsIndexReturnsFalseWhenNoServerAvailable(): void {
    $index = $this->createMock(IndexInterface::class);
    $index->method('getServerInstanceIfAvailable')->willReturn(NULL);

    $this->assertFalse(FileExtraction::supportsIndex($index));
  }

  /**
   * @covers ::getPropertyDefinitions
   */
  public function testGetPropertyDefinitionsDeclaresOnePropertyPerFileFieldWithSawPrefix(): void {
    $index = $this->createMock(IndexInterface::class);
    $index->method('getDatasources')->willReturn([$this->datasourceWithFileField()]);
    $processor = $this->processor();
    $processor->setIndex($index);

    $properties = $processor->getPropertyDefinitions(NULL);

    $this->assertArrayHasKey('saw_field_files', $properties);
    $this->assertInstanceOf(ProcessorProperty::class, $properties['saw_field_files']);
    // Only file fields get a property -- the title field must not.
    $this->assertArrayNotHasKey('saw_title', $properties);
  }

  /**
   * The saw_ prefix is the field-naming decision (#262 decision 1): it must
   * not collide with search_api_attachments' saa_ prefix if both modules are
   * installed on one site. Asserting the exact machine name locks that.
   *
   * @covers ::getPropertyDefinitions
   */
  public function testPropertyNamesUseSawPrefixNotSearchApiAttachmentsSaaPrefix(): void {
    $index = $this->createMock(IndexInterface::class);
    $index->method('getDatasources')->willReturn([$this->datasourceWithFileField()]);
    $processor = $this->processor();
    $processor->setIndex($index);

    $properties = $processor->getPropertyDefinitions(NULL);

    $this->assertSame(['saw_field_files'], array_keys($properties));
    $this->assertStringStartsWith('saw_', array_key_first($properties));
  }

  /**
   * Processor-defined properties are index-level (datasource NULL); asking for
   * a specific datasource yields nothing. This is the Search API convention
   * AggregatedFields and search_api_attachments both follow.
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
  public function testAddFieldValuesExtractsFileTextAndPopulatesTheField(): void {
    $entityTypeManager = $this->createMock(EntityTypeManagerInterface::class);
    $fileStorage = $this->createMock(EntityStorageInterface::class);
    // A real FileInterface mock: the processor now passes the file entity to
    // the cache (#263), so a duck-typed double no longer satisfies the type.
    $fileStorage->method('loadMultiple')
      ->willReturn([7 => $this->fileEntityAt('/tmp/sample.txt', 7)]);
    $entityTypeManager->method('getStorage')->willReturn($fileStorage);

    $backend = $this->createMock(WayfinderBackend::class);
    $backend->expects($this->once())
      ->method('extractContentFromFile')
      ->with('/tmp/sample.txt')
      ->willReturn('extracted text');

    $indexField = $this->createMock(FieldInterface::class);
    $indexField->expects($this->once())->method('addValue')->with('extracted text');

    $fieldsHelper = $this->createMock(FieldsHelperInterface::class);
    $fieldsHelper->expects($this->once())
      ->method('filterForPropertyPath')
      ->with($this->anything(), NULL, 'saw_field_files')
      ->willReturn(['saw_field_files' => $indexField]);

    $processor = $this->processor($entityTypeManager);
    $processor->setFieldsHelper($fieldsHelper);
    $processor->setIndex($this->indexForExtraction($backend));

    $item = $this->itemWithFileField($indexField);

    $processor->addFieldValues($item);
  }

  /**
   * The hard requirement (#262): an extraction failure must be logged and
   * skipped -- the item still indexes, just without that attachment's text.
   * A single bad file can never fail the whole batch.
   *
   * @covers ::addFieldValues
   */
  public function testAddFieldValuesLogsAndSkipsOnExtractionFailureWithoutFailingTheBatch(): void {
    $entityTypeManager = $this->createMock(EntityTypeManagerInterface::class);
    $fileStorage = $this->createMock(EntityStorageInterface::class);
    $fileStorage->method('loadMultiple')
      ->willReturn([7 => $this->fileEntityAt('/tmp/corrupt.pdf', 7)]);
    $entityTypeManager->method('getStorage')->willReturn($fileStorage);

    $backend = $this->createMock(WayfinderBackend::class);
    $backend->method('extractContentFromFile')
      ->willThrowException(new SearchApiException('unsupported format'));

    // The failure is logged ...
    $logger = $this->createMock(LoggerInterface::class);
    $logger->expects($this->once())->method('error');

    // ... and the field is left untouched (no empty/garbage value added).
    $indexField = $this->createMock(FieldInterface::class);
    $indexField->expects($this->never())->method('addValue');

    $fieldsHelper = $this->createMock(FieldsHelperInterface::class);
    $fieldsHelper->method('filterForPropertyPath')
      ->willReturn(['saw_field_files' => $indexField]);

    $processor = $this->processor($entityTypeManager, $logger);
    $processor->setFieldsHelper($fieldsHelper);
    $processor->setIndex($this->indexForExtraction($backend));

    // No exception escapes -- that is the batch-safety guarantee.
    $processor->addFieldValues($this->itemWithFileField($indexField));

    $this->addToAssertionCount(1);
  }

  /**
   * A file field the admin never added to the index has no matching index
   * field (filterForPropertyPath returns []), so nothing is extracted -- no
   * wasted /update/extract calls for unconfigured fields.
   *
   * @covers ::addFieldValues
   */
  public function testAddFieldValuesExtractsNothingWhenNoIndexFieldReferencesTheProperty(): void {
    $backend = $this->createMock(WayfinderBackend::class);
    $backend->expects($this->never())->method('extractContentFromFile');

    $fieldsHelper = $this->createMock(FieldsHelperInterface::class);
    $fieldsHelper->method('filterForPropertyPath')->willReturn([]);

    $processor = $this->processor();
    $processor->setFieldsHelper($fieldsHelper);
    $processor->setIndex($this->indexForExtraction($backend));

    $processor->addFieldValues($this->itemWithFileField($this->createMock(FieldInterface::class)));
  }

  /**
   * If the underlying object can't be loaded (deleted entity, etc.)
   * addFieldValues no-ops rather than crashing -- mirroring
   * search_api_attachments' SearchApiException guard.
   *
   * @covers ::addFieldValues
   */
  public function testAddFieldValuesNoOpsWhenTheOriginalObjectCannotBeLoaded(): void {
    $backend = $this->createMock(WayfinderBackend::class);
    $backend->expects($this->never())->method('extractContentFromFile');

    $fieldsHelper = $this->createMock(FieldsHelperInterface::class);
    $fieldsHelper->method('filterForPropertyPath')->willReturn([]);

    $processor = $this->processor();
    $processor->setFieldsHelper($fieldsHelper);
    $processor->setIndex($this->indexForExtraction($backend));

    $item = $this->createMock(ItemInterface::class);
    $item->method('getOriginalObject')->willThrowException(new SearchApiException('Failed to load original object'));

    $processor->addFieldValues($item);

    $this->addToAssertionCount(1);
  }

  /**
   * Builds the index mock shared by the addFieldValues tests: it advertises
   * the field_files file field, points at $backend via its server, and
   * (harmlessly) serves whatever fields the item carries.
   */
  private function indexForExtraction(WayfinderBackend $backend): IndexInterface {
    $server = $this->createMock(ServerInterface::class);
    $server->method('getBackend')->willReturn($backend);

    $index = $this->createMock(IndexInterface::class);
    $index->method('getDatasources')->willReturn([$this->datasourceWithFileField()]);
    $index->method('getServerInstanceIfAvailable')->willReturn($server);
    return $index;
  }

  /**
   * An item whose original object is a node carrying one file (target_id 7)
   * in field_files. The index field that the processor will populate is passed
   * in so filterForPropertyPath can return it.
   */
  private function itemWithFileField(FieldInterface $indexField): ItemInterface {
    $fileList = $this->createMock(FieldItemListInterface::class);
    $fileList->method('getValue')->willReturn([['target_id' => 7]]);

    $node = $this->createMock(ContentEntityInterface::class);
    $node->method('hasField')->willReturnCallback(fn (string $name) => $name === 'field_files');
    $node->method('get')->willReturnCallback(fn (string $name) => $name === 'field_files' ? $fileList : NULL);

    $adapter = $this->createMock(ComplexDataInterface::class);
    $adapter->method('getValue')->willReturn($node);

    $item = $this->createMock(ItemInterface::class);
    $item->method('getOriginalObject')->willReturn($adapter);
    $item->method('getFields')->willReturn(['saw_field_files' => $indexField]);
    return $item;
  }

  /**
   * Acceptance test #1 (issue #263): a second extraction of the same file hits
   * the cache and does NOT call the client. Two items reference the same file;
   * the backend's extractContentFromFile must run exactly once, and both items
   * receive the extracted text.
   *
   * @covers ::addFieldValues
   * @covers ::extractOrGetFromCache
   */
  public function testASecondExtractionOfTheSameFileHitsTheCacheAndSkipsTheClient(): void {
    $path = tempnam(sys_get_temp_dir(), 'wf_acc1_');
    file_put_contents($path, 'attachment body');

    $entityTypeManager = $this->createMock(EntityTypeManagerInterface::class);
    $fileStorage = $this->createMock(EntityStorageInterface::class);
    $fileStorage->method('loadMultiple')->willReturn([7 => $this->fileEntityAt($path, 7)]);
    $entityTypeManager->method('getStorage')->willReturn($fileStorage);

    $backend = $this->createMock(WayfinderBackend::class);
    // The load-bearing assertion: extraction runs ONCE despite two items.
    $backend->expects($this->once())
      ->method('extractContentFromFile')
      ->with($path)
      ->willReturn('extracted text');

    $indexField = $this->createMock(FieldInterface::class);
    $indexField->expects($this->exactly(2))->method('addValue')->with('extracted text');

    $fieldsHelper = $this->createMock(FieldsHelperInterface::class);
    $fieldsHelper->method('filterForPropertyPath')->willReturn(['saw_field_files' => $indexField]);

    $store = [];
    $cache = new KeyValueExtractionCache($this->inMemoryStore($store));
    $processor = $this->processor($entityTypeManager, NULL, $cache);
    $processor->setFieldsHelper($fieldsHelper);
    $processor->setIndex($this->indexForExtraction($backend));

    $processor->addFieldValues($this->itemWithFileField($indexField));
    $processor->addFieldValues($this->itemWithFileField($indexField));
  }

  /**
   * Acceptance test #2 (issue #263): a changed file invalidates and
   * re-extracts. The cache is keyed by content hash, so when the file's bytes
   * change between two index passes the second is a cache MISS and the client
   * is called again -- yielding the new text.
   *
   * @covers ::addFieldValues
   * @covers ::extractOrGetFromCache
   */
  public function testAChangedFileIsReExtractedAfterItsContentChanges(): void {
    $path = tempnam(sys_get_temp_dir(), 'wf_acc2_');
    file_put_contents($path, 'version one');

    $entityTypeManager = $this->createMock(EntityTypeManagerInterface::class);
    $fileStorage = $this->createMock(EntityStorageInterface::class);
    $fileStorage->method('loadMultiple')->willReturn([7 => $this->fileEntityAt($path, 7)]);
    $entityTypeManager->method('getStorage')->willReturn($fileStorage);

    $backend = $this->createMock(WayfinderBackend::class);
    // Called twice: once for content v1, once for v2 (new hash -> miss).
    $backend->expects($this->exactly(2))
      ->method('extractContentFromFile')
      ->willReturnOnConsecutiveCalls('v1 text', 'v2 text');

    $added = [];
    $indexField = $this->createMock(FieldInterface::class);
    $indexField->expects($this->exactly(2))
      ->method('addValue')
      ->willReturnCallback(function ($value) use (&$added): void {
        $added[] = $value;
      });

    $fieldsHelper = $this->createMock(FieldsHelperInterface::class);
    $fieldsHelper->method('filterForPropertyPath')->willReturn(['saw_field_files' => $indexField]);

    $store = [];
    $cache = new KeyValueExtractionCache($this->inMemoryStore($store));
    $processor = $this->processor($entityTypeManager, NULL, $cache);
    $processor->setFieldsHelper($fieldsHelper);
    $processor->setIndex($this->indexForExtraction($backend));

    $processor->addFieldValues($this->itemWithFileField($indexField));
    // Same path, new contents -> different hash -> cache miss.
    file_put_contents($path, 'version two');
    $processor->addFieldValues($this->itemWithFileField($indexField));

    $this->assertSame(['v1 text', 'v2 text'], $added);
  }

  /**
   * In queue mode, the processor does NOT call the client during indexing: it
   * enqueues one extraction job per file and indexes nothing yet. The queue
   * worker (#263) later extracts, caches, and marks the item for reindex. This
   * keeps a slow parser from stalling an index batch.
   *
   * @covers ::addFieldValues
   * @covers ::extractOrGetFromCache
   */
  public function testQueueModeEnqueuesInsteadOfExtractingInline(): void {
    $entityTypeManager = $this->createMock(EntityTypeManagerInterface::class);
    $fileStorage = $this->createMock(EntityStorageInterface::class);
    $fileStorage->method('loadMultiple')->willReturn([7 => $this->fileEntityAt('/tmp/queued.pdf', 7)]);
    $entityTypeManager->method('getStorage')->willReturn($fileStorage);

    $backend = $this->createMock(WayfinderBackend::class);
    $backend->expects($this->never())->method('extractContentFromFile');

    $queue = $this->createMock(QueueInterface::class);
    $queue->expects($this->once())->method('createItem')
      ->willReturn(TRUE);

    $indexField = $this->createMock(FieldInterface::class);
    $indexField->expects($this->never())->method('addValue');

    $fieldsHelper = $this->createMock(FieldsHelperInterface::class);
    $fieldsHelper->method('filterForPropertyPath')->willReturn(['saw_field_files' => $indexField]);

    // Queue mode is opted into via processor config (the form lands in #266).
    $processor = $this->processor($entityTypeManager, NULL, NULL, $queue);
    $processor->setConfiguration(['extraction_mode' => 'queue']);
    $processor->setFieldsHelper($fieldsHelper);
    $processor->setIndex($this->indexForExtraction($backend));

    $processor->addFieldValues($this->itemWithFileField($indexField));
  }

  /**
   * The processor records every (file -> item) reference it indexes into the
   * file map, so a later file change or delete can mark this item for reindex
   * (#263 invalidation) and #265's linked files can reuse the same map.
   *
   * @covers ::addFieldValues
   */
  public function testAddFieldValuesRecordsTheFileItemReferenceInTheMap(): void {
    $entityTypeManager = $this->createMock(EntityTypeManagerInterface::class);
    $fileStorage = $this->createMock(EntityStorageInterface::class);
    $fileStorage->method('loadMultiple')->willReturn([7 => $this->fileEntityAt('/tmp/a.txt', 7)]);
    $entityTypeManager->method('getStorage')->willReturn($fileStorage);

    $backend = $this->createMock(WayfinderBackend::class);
    $backend->method('extractContentFromFile')->willReturn('text');

    $indexField = $this->createMock(FieldInterface::class);
    $fieldsHelper = $this->createMock(FieldsHelperInterface::class);
    $fieldsHelper->method('filterForPropertyPath')->willReturn(['saw_field_files' => $indexField]);

    $bag = [];
    $map = new FileReferenceMap($this->inMemoryStore($bag));
    $processor = $this->processor($entityTypeManager, NULL, NULL, NULL, $map);
    $processor->setFieldsHelper($fieldsHelper);

    $index = $this->indexForExtraction($backend);
    $index->method('id')->willReturn('content');
    $processor->setIndex($index);

    $item = $this->itemWithFileField($indexField);
    $item->method('getId')->willReturn('entity:node/1');
    $processor->addFieldValues($item);

    $this->assertSame([['index' => 'content', 'item' => 'entity:node/1']], $map->itemsForFile(7));
  }

  /**
   * A FileInterface mock backed by a real path and id. getFileUri() is what the
   * processor hands the backend; id() is what the file->item map records.
   */
  private function fileEntityAt(string $uri, int $id): FileInterface {
    $file = $this->createMock(FileInterface::class);
    $file->method('getFileUri')->willReturn($uri);
    $file->method('id')->willReturn($id);
    return $file;
  }

  /**
   * An in-memory keyvalue store double so the cache round-trips without a DB.
   */
  private function inMemoryStore(array &$store): KeyValueStoreInterface {
    $kv = $this->createMock(KeyValueStoreInterface::class);
    $kv->method('get')->willReturnCallback(function ($key) use (&$store) {
      return array_key_exists($key, $store) ? $store[$key] : NULL;
    });
    $kv->method('set')->willReturnCallback(function ($key, $value) use (&$store) {
      $store[$key] = $value;
    });
    return $kv;
  }

}
