<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder\Cache;

use Drupal\file\FileInterface;

/**
 * Persists extracted attachment text keyed by file content hash.
 *
 * Issue #263: the same file must be extracted once regardless of how many
 * index items reference it. Keying by content hash (not file id) makes both
 * halves of the requirement fall out of one decision:
 * - Identical content (the same file referenced by many items, or a second
 *   file with the same bytes) shares one entry.
 * - Changed content has a different hash and so naturally misses; the stale
 *   entry under the old hash is simply never read again.
 *
 * This interface is the cache boundary: a second backend (#266's settings form
 * may offer a file-based one) implements it the same way. One backend ships
 * with this issue (KeyValueExtractionCache); a second is added only if a real
 * site demonstrates the need, per the issue's "ship one" guidance.
 *
 * Derived in shape from search_api_attachments' AttachmentsCacheInterface
 * (GPL-2.0-or-later, same licence as this module): the get/set/delete/clear
 * surface is adapted, but the keying strategy is content-hash here where the
 * upstream is file-id based with explicit eviction.
 */
interface ExtractionCacheInterface {

  /**
   * Returns the cached extraction for a file, or NULL on a miss.
   *
   * @param \Drupal\file\FileInterface $file
   *   The file whose extracted text is cached.
   *
   * @return string|null
   *   The cached extracted text, or NULL if nothing is cached for this file's
   *   current content.
   */
  public function get(FileInterface $file): ?string;

  /**
   * Stores the extracted text for a file, keyed by its content hash.
   *
   * @param \Drupal\file\FileInterface $file
   *   The file whose extraction is being cached.
   * @param string $text
   *   The extracted text.
   */
  public function set(FileInterface $file, string $text): void;

  /**
   * Removes the cache entry for a file's current content hash.
   *
   * Note: because the key is the content hash, this clears the entry matching
   * the file AS IT IS NOW. An entry left under an older (pre-change) hash is
   * already unreachable and is reaped by ::clear().
   *
   * @param \Drupal\file\FileInterface $file
   *   The file whose cache entry should be removed.
   */
  public function delete(FileInterface $file): void;

  /**
   * Empties the entire cache.
   */
  public function clear(): void;

}
