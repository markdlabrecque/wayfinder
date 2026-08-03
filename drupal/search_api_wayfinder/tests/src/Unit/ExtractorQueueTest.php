<?php

declare(strict_types=1);

namespace Drupal\Tests\search_api_wayfinder\Unit;

use Drupal\Core\Entity\EntityStorageInterface;
use Drupal\Core\Entity\EntityTypeManagerInterface;
use Drupal\file\FileInterface;
use Drupal\search_api\IndexInterface;
use Drupal\search_api\SearchApiException;
use Drupal\search_api\ServerInterface;
use Drupal\search_api\Utility\Utility;
use Drupal\search_api_wayfinder\Cache\ExtractionCacheInterface;
use Drupal\search_api_wayfinder\Plugin\QueueWorker\ExtractorQueue;
use Drupal\search_api_wayfinder\Plugin\search_api\backend\WayfinderBackend;
use PHPUnit\Framework\TestCase;
use Psr\Log\LoggerInterface;

/**
 * Tests the extraction queue worker (issue #263): a queued extraction job is
 * processed on cron -- the file is extracted (or its cache hit), the result is
 * cached, and the referencing item is marked for reindex so the next index
 * pass picks up the now-cached text instead of re-queuing.
 *
 * @coversDefaultClass \Drupal\search_api_wayfinder\Plugin\QueueWorker\ExtractorQueue
 * @group search_api_wayfinder
 */
class ExtractorQueueTest extends TestCase {

  /**
   * Builds the worker with mocked collaborators and an entityTypeManager whose
   * storages return the supplied file/index.
   */
  private function worker(?FileInterface $file = NULL, ?IndexInterface $index = NULL, ?ExtractionCacheInterface $cache = NULL, ?LoggerInterface $logger = NULL): ExtractorQueue {
    $fileStorage = $this->createMock(EntityStorageInterface::class);
    $fileStorage->method('load')->willReturn($file);

    $indexStorage = $this->createMock(EntityStorageInterface::class);
    $indexStorage->method('load')->willReturn($index);

    $etm = $this->createMock(EntityTypeManagerInterface::class);
    $etm->method('getStorage')->willReturnCallback(
      fn (string $type) => $type === 'file' ? $fileStorage : $indexStorage
    );

    return new ExtractorQueue(
      [],
      'wayfinder_extraction',
      ['id' => 'wayfinder_extraction'],
      $etm,
      $cache ?? $this->createMock(ExtractionCacheInterface::class),
      $logger ?? $this->createMock(LoggerInterface::class),
    );
  }

  /**
   * Builds an index whose server serves $backend, for backend resolution.
   */
  private function indexWithBackend(WayfinderBackend $backend): IndexInterface {
    $server = $this->createMock(ServerInterface::class);
    $server->method('getBackend')->willReturn($backend);
    $index = $this->createMock(IndexInterface::class);
    $index->method('getServerInstanceIfAvailable')->willReturn($server);
    return $index;
  }

  private function file(string $uri = '/tmp/job.pdf', int $id = 7): FileInterface {
    $file = $this->createMock(FileInterface::class);
    $file->method('getFileUri')->willReturn($uri);
    $file->method('id')->willReturn($id);
    return $file;
  }

  /**
   * The happy path on a cache miss: extract, cache, and mark the item for
   * reindex so its next index pass hits the cache instead of re-queuing.
   *
   * @covers ::processItem
   */
  public function testProcessItemExtractsCachesAndReindexesOnACacheMiss(): void {
    $backend = $this->createMock(WayfinderBackend::class);
    $backend->expects($this->once())
      ->method('extractContentFromFile')
      ->with('/tmp/job.pdf')
      ->willReturn('extracted text');

    $index = $this->indexWithBackend($backend);
    // Combined id "entity:node/3" -> datasource "entity:node", raw "3".
    $index->expects($this->once())
      ->method('trackItemsUpdated')
      ->with('entity:node', ['3']);

    $cache = $this->createMock(ExtractionCacheInterface::class);
    $cache->method('get')->willReturn(NULL);
    $cache->expects($this->once())->method('set')->with($this->file(), 'extracted text');

    $this->worker($this->file(), $index, $cache)
      ->processItem(['file_id' => 7, 'index_id' => 'content', 'item_id' => 'entity:node/3']);
  }

  /**
   * A cache hit skips the extraction call entirely -- the worker only needs to
   * mark the item for reindex so the cached text flows into the index.
   *
   * @covers ::processItem
   */
  public function testProcessItemSkipsExtractionOnACacheHitButStillReindexes(): void {
    $backend = $this->createMock(WayfinderBackend::class);
    $backend->expects($this->never())->method('extractContentFromFile');

    $index = $this->indexWithBackend($backend);
    $index->expects($this->once())->method('trackItemsUpdated');

    $cache = $this->createMock(ExtractionCacheInterface::class);
    $cache->method('get')->willReturn('already cached');

    $this->worker($this->file(), $index, $cache)
      ->processItem(['file_id' => 7, 'index_id' => 'content', 'item_id' => 'entity:node/3']);
  }

  /**
   * A file deleted between queue time and cron is a no-op: nothing to extract,
   * nothing to reindex. (The file delete hook already reindexed referencing
   * items via the invalidator.)
   *
   * @covers ::processItem
   */
  public function testProcessItemNoOpsWhenTheFileIsGone(): void {
    $backend = $this->createMock(WayfinderBackend::class);
    $backend->expects($this->never())->method('extractContentFromFile');

    $index = $this->indexWithBackend($backend);
    $index->expects($this->never())->method('trackItemsUpdated');

    $this->worker(NULL, $index)
      ->processItem(['file_id' => 7, 'index_id' => 'content', 'item_id' => 'entity:node/3']);
  }

  /**
   * An index that no longer loads (deleted/disabled) is a no-op.
   *
   * @covers ::processItem
   */
  public function testProcessItemNoOpsWhenTheIndexIsGone(): void {
    $backend = $this->createMock(WayfinderBackend::class);
    $backend->expects($this->never())->method('extractContentFromFile');

    $this->worker($this->file(), NULL)
      ->processItem(['file_id' => 7, 'index_id' => 'content', 'item_id' => 'entity:node/3']);

    $this->addToAssertionCount(1);
  }

  /**
   * An index whose server no longer uses the Wayfinder backend is a no-op --
   * never extract through a different backend.
   *
   * @covers ::processItem
   */
  public function testProcessItemNoOpsWhenTheBackendIsNotWayfinder(): void {
    $server = $this->createMock(ServerInterface::class);
    // A non-Wayfinder backend: a bare mock of a different class.
    $server->method('getBackend')->willReturn($this->createMock(\Drupal\search_api\Backend\BackendInterface::class));

    $index = $this->createMock(IndexInterface::class);
    $index->method('getServerInstanceIfAvailable')->willReturn($server);
    $index->expects($this->never())->method('trackItemsUpdated');

    $backend = $this->createMock(WayfinderBackend::class);
    $backend->expects($this->never())->method('extractContentFromFile');

    $this->worker($this->file(), $index)
      ->processItem(['file_id' => 7, 'index_id' => 'content', 'item_id' => 'entity:node/3']);
  }

  /**
   * An extraction failure is logged and re-thrown so cron leaves the item in
   * the queue for a later retry -- the queue-appropriate behaviour for a
   * transient server failure, distinct from the inline path's batch-safety
   * log-and-skip (#262). The item is NOT marked for reindex until the
   * extraction succeeds and is cached.
   *
   * @covers ::processItem
   */
  public function testProcessItemLogsAndRethrowsOnExtractionFailure(): void {
    $backend = $this->createMock(WayfinderBackend::class);
    $backend->method('extractContentFromFile')
      ->willThrowException(new SearchApiException('server unavailable'));

    $index = $this->indexWithBackend($backend);
    $index->expects($this->never())->method('trackItemsUpdated');

    $logger = $this->createMock(LoggerInterface::class);
    $logger->expects($this->once())->method('error');

    $cache = $this->createMock(ExtractionCacheInterface::class);
    $cache->method('get')->willReturn(NULL);

    $this->expectException(SearchApiException::class);

    $this->worker($this->file(), $index, $cache, $logger)
      ->processItem(['file_id' => 7, 'index_id' => 'content', 'item_id' => 'entity:node/3']);
  }

}
