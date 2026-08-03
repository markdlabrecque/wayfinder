<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder;

use Drupal\Core\Entity\EntityTypeManagerInterface;
use Drupal\file\FileInterface;
use Drupal\search_api\IndexInterface;
use Drupal\search_api\Utility\Utility;
use Psr\Log\LoggerInterface;

/**
 * Marks index items for reindex when a file they reference changes or is
 * deleted.
 *
 * Issue #263's invalidation requirement. The {@see FileReferenceMap} records
 * which items referenced which file during indexing; this class is the other
 * half -- it reads that map when a file changes and calls
 * IndexInterface::trackItemsUpdated() for every referencing item so the next
 * index pass re-extracts (a changed file: new content hash -> cache miss; a
 * deleted file: the processor skips the missing attachment).
 *
 * The file lifecycle hooks in search_api_wayfinder.module are thin wrappers
 * that forward to onFileUpdate()/onFileDelete(); all the logic lives here so it
 * is unit-testable without a bootstrapped Drupal.
 *
 * Trust boundary: this reindexes on every file update, including metadata-only
 * saves. Because the cache is content-hash keyed, an unchanged file's reindex
 * is a pure cache hit, so the spurious reindex is cheap. A narrower
 * "content-actually-changed" guard (comparing the old and new hash) is a #266
 * refinement, not a correctness fix.
 */
class ExtractionInvalidator {

  /**
   * The Search API index entity type id, used to load referencing indexes.
   */
  private const INDEX_STORAGE = 'search_api_index';

  /**
   * @param \Drupal\search_api_wayfinder\FileReferenceMapInterface $fileMap
   *   The file->item reference map populated during indexing.
   * @param \Drupal\Core\Entity\EntityTypeManagerInterface $entityTypeManager
   *   The entity type manager, for loading Search API index entities.
   * @param \Psr\Log\LoggerInterface|null $logger
   *   (optional) Logger for recoverable problems (a referencing index that no
   *   longer loads).
   */
  public function __construct(
    private readonly FileReferenceMapInterface $fileMap,
    private readonly EntityTypeManagerInterface $entityTypeManager,
    private readonly ?LoggerInterface $logger = NULL,
  ) {}

  /**
   * Marks every item referencing a changed file for reindex.
   *
   * @param \Drupal\file\FileInterface $file
   *   The file that changed.
   */
  public function onFileUpdate(FileInterface $file): void {
    $this->invalidateReferences($this->fileId($file));
  }

  /**
   * Marks every item referencing a deleted file for reindex, then drops the
   * file's map entry so it does not leak.
   *
   * @param \Drupal\file\FileInterface $file
   *   The file that was deleted.
   */
  public function onFileDelete(FileInterface $file): void {
    $file_id = $this->fileId($file);
    $this->invalidateReferences($file_id);
    $this->fileMap->forgetFile($file_id);
  }

  /**
   * Resolves a file's id to an int, tolerating a NULL id (a file that was never
   * saved has no references in the map anyway).
   */
  private function fileId(FileInterface $file): int {
    return (int) $file->id();
  }

  /**
   * For every referencing item, calls trackItemsUpdated() on its index.
   *
   * Items are grouped per (index, datasource) so each index receives one
   * trackItemsUpdated() call per datasource, with the raw (un-combined) item
   * ids the tracker expects. A referencing index that can no longer be loaded
   * (deleted or disabled) is skipped and logged rather than aborting the rest.
   *
   * @param int $fileId
   *   The file id whose references to invalidate.
   */
  private function invalidateReferences(int $fileId): void {
    $references = $this->fileMap->itemsForFile($fileId);
    if ($references === []) {
      return;
    }

    // Group combined item ids by index id, then by datasource within an index.
    $by_index = [];
    foreach ($references as $reference) {
      $by_index[$reference['index']][] = $reference['item'];
    }

    $indexes = $this->loadIndexes(array_keys($by_index));
    foreach ($by_index as $index_id => $combined_ids) {
      $index = $indexes[$index_id] ?? NULL;
      if (!$index instanceof IndexInterface) {
        $this->logger?->warning('Could not load index @index to reindex references to file @file; skipping.', [
          '@index' => $index_id,
          '@file' => $fileId,
        ]);
        continue;
      }

      foreach ($this->groupByDatasource($combined_ids) as $datasource_id => $raw_ids) {
        $index->trackItemsUpdated($datasource_id, $raw_ids);
      }
    }
  }

  /**
   * Groups combined item ids by datasource, splitting to raw ids.
   *
   * @param string[] $combinedIds
   *   Item ids in combined form (datasource:raw).
   *
   * @return array<string, string[]>
   *   Raw item ids keyed by datasource id.
   */
  private function groupByDatasource(array $combinedIds): array {
    $grouped = [];
    foreach ($combinedIds as $combined_id) {
      [$datasource_id, $raw_id] = Utility::splitCombinedId($combined_id);
      $grouped[(string) $datasource_id][] = (string) $raw_id;
    }
    return $grouped;
  }

  /**
   * Loads the given index entities, keyed by id.
   *
   * @return array<string, \Drupal\search_api\IndexInterface>
   */
  private function loadIndexes(array $index_ids): array {
    return $this->entityTypeManager
      ->getStorage(self::INDEX_STORAGE)
      ->loadMultiple($index_ids);
  }

}
