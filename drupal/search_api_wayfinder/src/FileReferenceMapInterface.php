<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder;

/**
 * Explicit record of which index items reference which file.
 *
 * Issue #263's invalidation requirement: when a file changes or is deleted,
 * every index item referencing it must be marked for reindex
 * (IndexInterface::trackItemsUpdated()). For attached files the entity
 * reference gives that mapping implicitly, but #265 (linked-file discovery)
 * indexes files referenced only by a URL in content -- no reference field to
 * query -- so the mapping is kept explicitly here and reused by both.
 *
 * The map is populated by the FileExtraction processor during indexing and
 * consulted by the ExtractionInvalidator on file update/delete. Item ids are
 * stored in Search API's combined form (datasource:raw) so the invalidator can
 * split them for trackItemsUpdated().
 */
interface FileReferenceMapInterface {

  /**
   * Records that an item on an index references a file.
   *
   * Idempotent: recording the same (index, item, file) twice stores one entry.
   *
   * @param string $indexId
   *   The Search API index id.
   * @param int $fileId
   *   The Drupal file entity id.
   * @param string $itemId
   *   The item id in combined form (e.g. "entity:node:1").
   */
  public function record(string $indexId, int $fileId, string $itemId): void;

  /**
   * Returns every item referencing a file.
   *
   * @param int $fileId
   *   The Drupal file entity id.
   *
   * @return array<int, array{index: string, item: string}>
   *   A list of references, each with the index id and combined item id.
   */
  public function itemsForFile(int $fileId): array;

  /**
   * Empties the entire map.
   */
  public function clear(): void;

  /**
   * Removes every reference recorded for a file.
   *
   * Called when a file is deleted: its references are dead, and leaving them
   * would leak a growing set of entries pointing at a file id that will never
   * be indexed again.
   *
   * @param int $fileId
   *   The Drupal file entity id.
   */
  public function forgetFile(int $fileId): void;

}
