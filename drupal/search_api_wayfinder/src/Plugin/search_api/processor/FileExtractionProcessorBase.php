<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder\Plugin\search_api\processor;

use Drupal\Core\Entity\EntityInterface;
use Drupal\Core\Entity\EntityTypeManagerInterface;
use Drupal\Core\Queue\QueueInterface;
use Drupal\file\FileInterface;
use Drupal\search_api\Item\ItemInterface;
use Drupal\search_api\Processor\ProcessorPluginBase;
use Drupal\search_api\SearchApiException;
use Drupal\search_api_wayfinder\Cache\ExtractionCacheInterface;
use Drupal\search_api_wayfinder\FileReferenceMapInterface;
use Drupal\search_api_wayfinder\Plugin\search_api\backend\WayfinderBackend;
use Psr\Log\LoggerInterface;

/**
 * Shared extraction plumbing for processors that index text pulled from files.
 *
 * Both attached-file discovery (#262's {@see FileExtraction}) and linked-file
 * discovery (#265's {@see LinkedFileExtraction}) resolve a set of files for an
 * item, record each file->item reference in the map, and run the same
 * cache/queue/extract loop with the same failure semantics. That loop and the
 * collaborators it needs live here so the two processors stay thin and the
 * behaviour stays identical.
 *
 * The inline vs queue extraction mode, the content-hash cache probe, and the
 * "one bad attachment never fails the batch" log-and-skip are all #262/#263
 * decisions carried verbatim into linked files: a linked document is just a
 * file the processor learned about from content rather than a reference field.
 */
abstract class FileExtractionProcessorBase extends ProcessorPluginBase {

  /**
   * Constructs a file-extraction processor.
   *
   * @param array $configuration
   *   Plugin configuration.
   * @param string $plugin_id
   *   The plugin id.
   * @param array $plugin_definition
   *   The plugin definition.
   * @param \Drupal\Core\Entity\EntityTypeManagerInterface|null $entityTypeManager
   *   The entity type manager, for loading file entities.
   * @param \Psr\Log\LoggerInterface|null $logger
   *   The logger channel for extraction failures.
   * @param \Drupal\search_api_wayfinder\Cache\ExtractionCacheInterface|null $cache
   *   (optional) The extraction cache. When NULL, every file is extracted
   *   inline with no caching -- the #262 tracer behaviour, retained for tests
   *   that do not exercise the cache.
   * @param \Drupal\Core\Queue\QueueInterface|null $queue
   *   (optional) The extraction queue, used only in queue mode. When NULL,
   *   queue mode degrades to inline extraction.
   * @param \Drupal\search_api_wayfinder\FileReferenceMapInterface|null $fileMap
   *   (optional) The file->item reference map, populated during indexing so a
   *   file change/delete can reindex every referencing item (#263) and #265's
   *   linked files can record references with no entity-reference field. When
   *   NULL, no mapping is recorded.
   */
  public function __construct(
    array $configuration,
    $plugin_id,
    array $plugin_definition,
    protected ?EntityTypeManagerInterface $entityTypeManager = NULL,
    protected ?LoggerInterface $logger = NULL,
    protected ?ExtractionCacheInterface $cache = NULL,
    protected ?QueueInterface $queue = NULL,
    protected ?FileReferenceMapInterface $fileMap = NULL,
  ) {
    parent::__construct($configuration, $plugin_id, $plugin_definition);
  }

  /**
   * Extracts text from a set of files for an item and records the references.
   *
   * Shared by attached-file and linked-file discovery: each file is recorded in
   * the file->item map (guarded for the tracer/tests that inject no map) and
   * run through {@see extractOrGetFromCache()}, and the resulting text is
   * concatenated. Returning '' lets a processor skip populating its field.
   *
   * @param iterable<\Drupal\file\FileInterface> $files
   *   The files resolved for this item, in any order.
   * @param \Drupal\search_api\Item\ItemInterface $item
   *   The item being indexed.
   *
   * @return string
   *   The concatenated extracted text, or '' if nothing was extracted.
   */
  protected function extractAndRecord(iterable $files, ItemInterface $item): string {
    $extraction = '';
    foreach ($files as $file) {
      if ($this->fileMap !== NULL) {
        $this->fileMap->record($this->getIndex()->id(), (int) $file->id(), $item->getId());
      }
      $extraction .= $this->extractOrGetFromCache($file, $item);
    }
    return $extraction;
  }

  /**
   * Returns extracted text for a file, hitting the cache first.
   *
   * Issue #263: the cache is keyed by file content hash, so a file referenced
   * by many items is extracted once, and a changed file (new hash) naturally
   * misses and is re-extracted. On a cache miss in queue mode the file is
   * deferred to the wayfinder_extraction cron worker instead of stalling the
   * index batch, and '' is indexed for now; the worker caches the text and
   * marks the item for reindex so the next pass hits the cache.
   *
   * Extraction failure is logged and returns '' (decision from #262): one bad
   * attachment never fails the whole batch.
   *
   * @param \Drupal\file\FileInterface $file
   *   The file to extract.
   * @param \Drupal\search_api\Item\ItemInterface $item
   *   The item being indexed, used as queue-item context for later reindex.
   *
   * @return string
   *   The extracted text, or '' on a cache miss in queue mode or on failure.
   */
  protected function extractOrGetFromCache(FileInterface $file, ItemInterface $item): string {
    if ($this->cache !== NULL) {
      $cached = $this->cache->get($file);
      if ($cached !== NULL) {
        return $cached;
      }
    }

    if ($this->isQueueMode() && $this->queue !== NULL) {
      $this->queueItem($file, $item);
      return '';
    }

    try {
      $backend = $this->getBackend();
      $text = $backend !== NULL ? (string) $backend->extractContentFromFile($file->getFileUri()) : '';
    }
    catch (\Throwable $e) {
      // Decision: extraction failure must not fail the whole index batch. Log
      // and index the item without this attachment's text.
      $this->logger?->error('Failed to extract text from file @uri while indexing: @message', [
        '@uri' => $file->getFileUri(),
        '@message' => $e->getMessage(),
      ]);
      return '';
    }

    if ($text !== '' && $this->cache !== NULL) {
      $this->cache->set($file, $text);
    }
    return $text;
  }

  /**
   * Whether the processor is configured to defer extraction to the queue.
   */
  private function isQueueMode(): bool {
    return ($this->configuration['extraction_mode'] ?? 'inline') === 'queue';
  }

  /**
   * Enqueues one extraction job for a file referenced by an item.
   *
   * The worker (ExtractorQueue) reloads the file and index, extracts+caches,
   * and marks the item for reindex so its next index pass hits the cache. The
   * item id is the combined form (datasource:raw); the worker splits it for
   * trackItemsUpdated().
   */
  private function queueItem(FileInterface $file, ItemInterface $item): void {
    $this->queue->createItem([
      'file_id' => (int) $file->id(),
      'index_id' => $this->getIndex()->id(),
      'item_id' => $item->getId(),
    ]);
  }

  /**
   * Resolves the item's original entity, or NULL if it cannot be loaded.
   *
   * Mirrors search_api_attachments' SearchApiException guard around
   * getOriginalObject(): a deleted entity mid-index is a no-op, not a crash.
   */
  protected function getEntity(ItemInterface $item): ?EntityInterface {
    try {
      $entity = $item->getOriginalObject()->getValue();
    }
    catch (SearchApiException $e) {
      return NULL;
    }
    return $entity instanceof EntityInterface ? $entity : NULL;
  }

  /**
   * Resolves the index's Wayfinder backend, or NULL if the index is not in
   * fact served by one (supportsIndex() normally prevents this, but
   * addFieldValues() defends regardless).
   */
  protected function getBackend(): ?WayfinderBackend {
    $server = $this->getIndex()->getServerInstanceIfAvailable();
    $backend = $server?->getBackend();
    return $backend instanceof WayfinderBackend ? $backend : NULL;
  }

}
