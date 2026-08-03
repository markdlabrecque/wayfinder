<?php

declare(strict_types=1);

namespace Drupal\Tests\search_api_wayfinder\Unit;

use Drupal\Core\Entity\EntityStorageInterface;
use Drupal\Core\Entity\EntityTypeManagerInterface;
use Drupal\file\FileInterface;
use Drupal\search_api\IndexInterface;
use Drupal\search_api\Utility\Utility;
use Drupal\search_api_wayfinder\ExtractionInvalidator;
use Drupal\search_api_wayfinder\FileReferenceMap;
use PHPUnit\Framework\TestCase;
use Psr\Log\LoggerInterface;

/**
 * Tests the extraction invalidator (issue #263): when a file changes or is
 * deleted, every index item referencing it is marked for reindex via
 * IndexInterface::trackItemsUpdated(), using the references the FileExtraction
 * processor recorded in the FileReferenceMap during indexing.
 *
 * The invalidator is the bridge between the file lifecycle (hook_file_update /
 * hook_file_delete, thin wrappers in search_api_wayfinder.module) and Search
 * API's tracker. It is unit-tested directly; the hooks only forward to it.
 *
 * @coversDefaultClass \Drupal\search_api_wayfinder\ExtractionInvalidator
 * @group search_api_wayfinder
 */
class ExtractionInvalidatorTest extends TestCase {

  /**
   * An in-memory keyvalue store double for the FileReferenceMap.
   */
  private function store(array &$bag): \Drupal\Core\KeyValueStore\KeyValueStoreInterface {
    $kv = $this->createMock(\Drupal\Core\KeyValueStore\KeyValueStoreInterface::class);
    $kv->method('get')->willReturnCallback(function ($key) use (&$bag) {
      return array_key_exists($key, $bag) ? $bag[$key] : NULL;
    });
    $kv->method('set')->willReturnCallback(function ($key, $value) use (&$bag) {
      $bag[$key] = $value;
    });
    $kv->method('delete')->willReturnCallback(function ($key) use (&$bag) {
      unset($bag[$key]);
    });
    return $kv;
  }

  /**
   * Builds an invalidator whose map is seeded with $references and whose index
   * storage returns the supplied index mocks keyed by id.
   *
   * @param array $references
   *   References to record, as [['index' => id, 'item' => combinedId], ...].
   * @param array $indexes
   *   Index mocks keyed by index id.
   * @param \Drupal\search_api_wayfinder\FileReferenceMap|null $map
   *   (output) the seeded map, for forget() assertions.
   */
  private function invalidator(array $references, array $indexes, ?FileReferenceMap &$map = NULL, ?LoggerInterface $logger = NULL): ExtractionInvalidator {
    $bag = [];
    $map = new FileReferenceMap($this->store($bag));
    foreach ($references as $ref) {
      $map->record($ref['index'], 7, $ref['item']);
    }

    $storage = $this->createMock(EntityStorageInterface::class);
    $storage->method('loadMultiple')->willReturnCallback(
      fn (array $ids) => array_intersect_key($indexes, array_flip($ids))
    );

    $etm = $this->createMock(EntityTypeManagerInterface::class);
    $etm->method('getStorage')->willReturn($storage);

    return new ExtractionInvalidator($map, $etm, $logger ?? $this->createMock(LoggerInterface::class));
  }

  private function file(int $id = 7): FileInterface {
    $file = $this->createMock(FileInterface::class);
    $file->method('id')->willReturn($id);
    return $file;
  }

  /**
   * @covers ::onFileUpdate
   */
  public function testOnFileUpdateMarksEveryReferencingItemForReindex(): void {
    $index = $this->createMock(IndexInterface::class);
    // trackItemsUpdated($datasource_id, $raw_ids): the combined ids recorded
    // by the map are split to raw ids, grouped per datasource.
    $index->expects($this->once())
      ->method('trackItemsUpdated')
      ->with('entity:node', ['1', '2']);

    $invalidator = $this->invalidator(
      [['index' => 'content', 'item' => 'entity:node/1'], ['index' => 'content', 'item' => 'entity:node/2']],
      ['content' => $index],
    );

    $invalidator->onFileUpdate($this->file());
  }

  /**
   * References spanning multiple indexes each get their own trackItemsUpdated
   * call on the right index.
   *
   * @covers ::onFileUpdate
   */
  public function testOnFileUpdateReindexesAcrossMultipleIndexes(): void {
    $indexA = $this->createMock(IndexInterface::class);
    $indexA->expects($this->once())->method('trackItemsUpdated')->with('entity:node', ['1']);
    $indexB = $this->createMock(IndexInterface::class);
    $indexB->expects($this->once())->method('trackItemsUpdated')->with('entity:media', ['5']);

    $invalidator = $this->invalidator(
      [['index' => 'a', 'item' => 'entity:node/1'], ['index' => 'b', 'item' => 'entity:media/5']],
      ['a' => $indexA, 'b' => $indexB],
    );

    $invalidator->onFileUpdate($this->file());
  }

  /**
   * References on one index but two datasources produce two trackItemsUpdated
   * calls (one per datasource), each with its own raw ids.
   *
   * @covers ::onFileUpdate
   */
  public function testOnFileUpdateGroupsByDatasource(): void {
    $index = $this->createMock(IndexInterface::class);
    $index->expects($this->exactly(2))->method('trackItemsUpdated')
      ->willReturnCallback(function (string $datasource, array $ids): void {
        $expected = [
          'entity:node' => ['1'],
          'entity:media' => ['9'],
        ];
        $this->assertSame($expected[$datasource], $ids);
      });

    $invalidator = $this->invalidator(
      [['index' => 'content', 'item' => 'entity:node/1'], ['index' => 'content', 'item' => 'entity:media/9']],
      ['content' => $index],
    );

    $invalidator->onFileUpdate($this->file());
  }

  /**
   * A file with no recorded references triggers no index work at all.
   *
   * @covers ::onFileUpdate
   */
  public function testOnFileUpdateNoOpsForAFileWithNoReferences(): void {
    $storage = $this->createMock(EntityStorageInterface::class);
    $storage->expects($this->never())->method('loadMultiple');
    $etm = $this->createMock(EntityTypeManagerInterface::class);
    $etm->method('getStorage')->willReturn($storage);

    $bag = [];
    $map = new FileReferenceMap($this->store($bag));
    $invalidator = new ExtractionInvalidator($map, $etm);

    $invalidator->onFileUpdate($this->file(999));
    $this->addToAssertionCount(1);
  }

  /**
   * An index that could not be loaded (deleted/disabled) is skipped and logged,
   * but remaining indexes are still invalidated.
   *
   * @covers ::onFileUpdate
   */
  public function testOnFileUpdateSkipsAndLogsAMissingIndexButContinues(): void {
    $indexB = $this->createMock(IndexInterface::class);
    $indexB->expects($this->once())->method('trackItemsUpdated');

    $logger = $this->createMock(LoggerInterface::class);
    $logger->expects($this->once())->method('warning');

    $invalidator = $this->invalidator(
      [['index' => 'gone', 'item' => 'entity:node/1'], ['index' => 'b', 'item' => 'entity:node/2']],
      ['b' => $indexB],
      $map,
      $logger,
    );

    $invalidator->onFileUpdate($this->file());
  }

  /**
   * onFileDelete invalidates (so referencing items reindex without the deleted
   * file's text) AND forgets the file's map entry so it does not leak.
   *
   * @covers ::onFileDelete
   */
  public function testOnFileDeleteInvalidatesThenForgetsTheFile(): void {
    $index = $this->createMock(IndexInterface::class);
    $index->expects($this->once())->method('trackItemsUpdated')->with('entity:node', ['1']);

    $invalidator = $this->invalidator(
      [['index' => 'content', 'item' => 'entity:node/1']],
      ['content' => $index],
      $map,
    );

    $invalidator->onFileDelete($this->file());

    $this->assertSame([], $map->itemsForFile(7));
  }

}
