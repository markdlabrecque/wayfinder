<?php

declare(strict_types=1);

namespace Drupal\Tests\search_api_wayfinder\Unit;

use Drupal\Core\KeyValueStore\KeyValueStoreInterface;
use Drupal\search_api_wayfinder\FileReferenceMap;
use Drupal\search_api_wayfinder\FileReferenceMapInterface;
use PHPUnit\Framework\TestCase;

/**
 * Tests the file->item reference map (issue #263): the explicit record of
 * which index items reference which file, kept so that a file change or
 * deletion can mark every referencing item for reindex via
 * trackItemsUpdated(), and so that #265 (linked-file discovery) can record
 * references it has no entity-reference field to derive them from.
 *
 * The store is mocked; the map is pure logic over it.
 *
 * @coversDefaultClass \Drupal\search_api_wayfinder\FileReferenceMap
 * @group search_api_wayfinder
 */
class FileReferenceMapTest extends TestCase {

  /**
   * An in-memory keyvalue store double so the map round-trips without a DB.
   */
  private function store(array &$bag): KeyValueStoreInterface {
    $kv = $this->createMock(KeyValueStoreInterface::class);
    $kv->method('get')->willReturnCallback(function ($key) use (&$bag) {
      return array_key_exists($key, $bag) ? $bag[$key] : NULL;
    });
    $kv->method('set')->willReturnCallback(function ($key, $value) use (&$bag) {
      $bag[$key] = $value;
    });
    $kv->method('delete')->willReturnCallback(function ($key) use (&$bag) {
      unset($bag[$key]);
    });
    $kv->method('deleteAll')->willReturnCallback(function () use (&$bag) {
      $bag = [];
    });
    return $kv;
  }

  private function map(array &$bag): FileReferenceMap {
    return new FileReferenceMap($this->store($bag));
  }

  /**
   * @covers ::record
   * @covers ::itemsForFile
   */
  public function testRecordedItemIsReturnedForItsFile(): void {
    $bag = [];
    $map = $this->map($bag);
    $map->record('content', 7, 'entity:node/1');

    $items = $map->itemsForFile(7);
    $this->assertSame([['index' => 'content', 'item' => 'entity:node/1']], $items);
  }

  /**
   * @covers ::itemsForFile
   */
  public function testItemsForAnUnknownFileIsEmpty(): void {
    $bag = [];
    $map = $this->map($bag);
    $this->assertSame([], $map->itemsForFile(999));
  }

  /**
   * Many items (on one index or several) referencing the same file are all
   * returned, so a single file change reindexes every referencing item.
   *
   * @covers ::record
   * @covers ::itemsForFile
   */
  public function testMultipleItemsAcrossIndexesAreAllReturned(): void {
    $bag = [];
    $map = $this->map($bag);
    $map->record('content', 7, 'entity:node/1');
    $map->record('content', 7, 'entity:node/2');
    $map->record('media_index', 7, 'entity:media/5');

    $items = $map->itemsForFile(7);
    $this->assertCount(3, $items);
    $this->assertContains(['index' => 'content', 'item' => 'entity:node/1'], $items);
    $this->assertContains(['index' => 'content', 'item' => 'entity:node/2'], $items);
    $this->assertContains(['index' => 'media_index', 'item' => 'entity:media/5'], $items);
  }

  /**
   * Recording the same (index, item, file) twice must not duplicate the entry;
   * a reindex of an unchanged item re-runs the processor and re-records, so the
   * map must be idempotent or it grows without bound.
   *
   * @covers ::record
   */
  public function testRecordingTheSameReferenceTwiceDeduplicates(): void {
    $bag = [];
    $map = $this->map($bag);
    $map->record('content', 7, 'entity:node/1');
    $map->record('content', 7, 'entity:node/1');

    $this->assertCount(1, $map->itemsForFile(7));
  }

  /**
   * Records for different files must not bleed into one another.
   *
   * @covers ::itemsForFile
   */
  public function testRecordsAreIsolatedPerFile(): void {
    $bag = [];
    $map = $this->map($bag);
    $map->record('content', 7, 'entity:node/1');
    $map->record('content', 8, 'entity:node/9');

    $this->assertSame([['index' => 'content', 'item' => 'entity:node/1']], $map->itemsForFile(7));
    $this->assertSame([['index' => 'content', 'item' => 'entity:node/9']], $map->itemsForFile(8));
  }

  /**
   * @covers ::clear
   */
  public function testClearEmptiesTheMap(): void {
    $bag = [];
    $map = $this->map($bag);
    $map->record('content', 7, 'entity:node/1');
    $map->clear();
    $this->assertSame([], $map->itemsForFile(7));
  }

  /**
   * @covers ::forgetFile
   */
  public function testForgetFileRemovesOnlyThatFilesReferences(): void {
    $bag = [];
    $map = $this->map($bag);
    $map->record('content', 7, 'entity:node/1');
    $map->record('content', 8, 'entity:node/2');

    $map->forgetFile(7);
    $this->assertSame([], $map->itemsForFile(7));
    // Other files are untouched.
    $this->assertSame([['index' => 'content', 'item' => 'entity:node/2']], $map->itemsForFile(8));
  }

  /**
   * Smoke test that the implementation satisfies the interface contract.
   */
  public function testImplementsFileReferenceMapInterface(): void {
    $bag = [];
    $this->assertInstanceOf(FileReferenceMapInterface::class, $this->map($bag));
  }

}
