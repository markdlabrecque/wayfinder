<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder\Plugin\QueueWorker;

use Drupal\Core\Entity\EntityTypeManagerInterface;
use Drupal\Core\Plugin\ContainerFactoryPluginInterface;
use Drupal\Core\Queue\Attribute\QueueWorker;
use Drupal\Core\Queue\QueueWorkerBase;
use Drupal\Core\StringTranslation\TranslatableMarkup;
use Drupal\file\FileInterface;
use Drupal\search_api\IndexInterface;
use Drupal\search_api\Utility\Utility;
use Drupal\search_api_wayfinder\Cache\ExtractionCacheInterface;
use Drupal\search_api_wayfinder\Plugin\search_api\backend\WayfinderBackend;
use Psr\Log\LoggerInterface;
use Symfony\Component\DependencyInjection\ContainerInterface;

/**
 * Processes deferred file extractions on cron (issue #263).
 *
 * When the FileExtraction processor runs in queue mode, a cache miss during
 * indexing enqueues a job here instead of stalling the index batch. On cron,
 * this worker: loads the file, ensures the extraction is cached (extracting on
 * a miss), and marks the referencing item for reindex -- so the next index
 * pass hits the cache and indexes the text instead of re-queuing.
 *
 * Failure semantics differ from the inline path: an extraction failure here is
 * logged and re-thrown so cron leaves the item in the queue for retry (the
 * queue-appropriate answer to a transient server failure). The inline path's
 * log-and-skip (#262) exists because one bad file must not fail an index
 * batch; here there is no batch to protect, so retrying is correct. A
 * permanently-unsupported file retried indefinitely is a known limitation
 * (ponytail) shared with search_api_attachments, addressed when a real site
 * hits it.
 *
 * Derived from search_api_attachments' ExtractorQueue (GPL-2.0-or-later, same
 * licence as this module): the processItem shape (load file + index, extract,
 * cache, reindex) is adapted; the transport is the Wayfinder backend rather
 * than an external text-extractor plugin.
 */
#[QueueWorker(
  id: 'wayfinder_extraction',
  title: new TranslatableMarkup('Wayfinder file extraction'),
  cron: ['time' => 60],
)]
class ExtractorQueue extends QueueWorkerBase implements ContainerFactoryPluginInterface {

  /**
   * @param array $configuration
   *   Plugin configuration.
   * @param string $plugin_id
   *   The plugin id.
   * @param array $plugin_definition
   *   The plugin definition.
   * @param \Drupal\Core\Entity\EntityTypeManagerInterface $entityTypeManager
   *   The entity type manager, for loading the file and index.
   * @param \Drupal\search_api_wayfinder\Cache\ExtractionCacheInterface $cache
   *   The extraction cache.
   * @param \Psr\Log\LoggerInterface $logger
   *   The logger channel for extraction failures.
   */
  public function __construct(
    array $configuration,
    $plugin_id,
    array $plugin_definition,
    protected readonly EntityTypeManagerInterface $entityTypeManager,
    protected readonly ExtractionCacheInterface $cache,
    protected readonly LoggerInterface $logger,
  ) {
    parent::__construct($configuration, $plugin_id, $plugin_definition);
  }

  /**
   * {@inheritdoc}
   */
  public static function create(ContainerInterface $container, array $configuration, $plugin_id, $plugin_definition) {
    return new static(
      $configuration,
      $plugin_id,
      $plugin_definition,
      $container->get('entity_type.manager'),
      $container->get('search_api_wayfinder.extraction_cache'),
      $container->get('logger.factory')->get('search_api_wayfinder'),
    );
  }

  /**
   * {@inheritdoc}
   *
   * @param array{file_id: int, index_id: string, item_id: string}|null $data
   *   The queued job: the file id, the index id, and the combined item id
   *   (datasource/raw).
   */
  public function processItem($data): void {
    $file = $this->loadFile((int) ($data['file_id'] ?? 0));
    if (!$file instanceof FileInterface) {
      return;
    }
    $index = $this->loadIndex((string) ($data['index_id'] ?? ''));
    if (!$index instanceof IndexInterface) {
      return;
    }
    $backend = $this->wayfinderBackend($index);
    if ($backend === NULL) {
      return;
    }

    if ($this->cache->get($file) === NULL) {
      try {
        $text = (string) $backend->extractContentFromFile($file->getFileUri());
      }
      catch (\Throwable $e) {
        $this->logger->error('Deferred extraction failed for file @uri; cron will retry: @message', [
          '@uri' => $file->getFileUri(),
          '@message' => $e->getMessage(),
        ]);
        // Re-throw so cron leaves the item in the queue for retry.
        throw $e;
      }
      if ($text !== '') {
        $this->cache->set($file, $text);
      }
    }

    // Mark the item for reindex so the now-cached text flows into the index on
    // the next pass (a cache hit there, so no re-queue).
    [$datasource_id, $raw_id] = Utility::splitCombinedId((string) ($data['item_id'] ?? ''));
    $index->trackItemsUpdated((string) $datasource_id, [(string) $raw_id]);
  }

  /**
   * Loads a file entity by id, or NULL.
   */
  private function loadFile(int $file_id): ?FileInterface {
    if ($file_id <= 0) {
      return NULL;
    }
    $file = $this->entityTypeManager->getStorage('file')->load($file_id);
    return $file instanceof FileInterface ? $file : NULL;
  }

  /**
   * Loads a Search API index by id, or NULL.
   */
  private function loadIndex(string $index_id): ?IndexInterface {
    if ($index_id === '') {
      return NULL;
    }
    $index = $this->entityTypeManager->getStorage('search_api_index')->load($index_id);
    return $index instanceof IndexInterface ? $index : NULL;
  }

  /**
   * Resolves the index's Wayfinder backend, or NULL if the index is not in
   * fact served by one.
   */
  private function wayfinderBackend(IndexInterface $index): ?WayfinderBackend {
    $server = $index->getServerInstanceIfAvailable();
    $backend = $server?->getBackend();
    return $backend instanceof WayfinderBackend ? $backend : NULL;
  }

}
