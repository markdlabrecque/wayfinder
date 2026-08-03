<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder;

use Drupal\Core\KeyValueStore\KeyValueStoreInterface;

/**
 * Keyvalue-backed file->item reference map. See {@see FileReferenceMapInterface}
 * for why the mapping is kept explicitly.
 *
 * Each file's references are stored under one key as a list of
 * ['index' => ..., 'item' => ...] entries, de-duplicated on write so that
 * reindexing an unchanged item (which re-runs the processor and re-records)
 * does not grow the map without bound.
 */
class FileReferenceMap implements FileReferenceMapInterface {

  /**
   * @param \Drupal\Core\KeyValueStore\KeyValueStoreInterface $store
   *   The keyvalue store for the map collection. The collection name is decided
   *   by the service definition, not here, so this class is store-agnostic.
   */
  public function __construct(
    private readonly KeyValueStoreInterface $store,
  ) {}

  /**
   * {@inheritdoc}
   */
  public function record(string $indexId, int $fileId, string $itemId): void {
    $key = $this->keyFor($fileId);
    $entries = $this->storedEntries($key);

    $candidate = ['index' => $indexId, 'item' => $itemId];
    foreach ($entries as $entry) {
      if ($entry === $candidate) {
        return;
      }
    }
    $entries[] = $candidate;
    $this->store->set($key, $entries);
  }

  /**
   * {@inheritdoc}
   */
  public function itemsForFile(int $fileId): array {
    return $this->storedEntries($this->keyFor($fileId));
  }

  /**
   * {@inheritdoc}
   */
  public function clear(): void {
    $this->store->deleteAll();
  }

  /**
   * {@inheritdoc}
   */
  public function forgetFile(int $fileId): void {
    $this->store->delete($this->keyFor($fileId));
  }

  /**
   * Builds the storage key for a file.
   */
  private function keyFor(int $fileId): string {
    return 'file:' . $fileId;
  }

  /**
   * Reads the entries stored under a key, normalised to a list.
   *
   * @return array<int, array{index: string, item: string}>
   */
  private function storedEntries(string $key): array {
    $value = $this->store->get($key);
    return is_array($value) ? $value : [];
  }

}
