<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder\Cache;

use Drupal\Core\KeyValueStore\KeyValueStoreInterface;
use Drupal\file\FileInterface;

/**
 * Keyvalue-backed extraction cache. The one backend shipped with issue #263.
 *
 * Entries are stored under a key derived from the file's content hash
 * (sha256 of its bytes), so identical content is extracted once and changed
 * content misses automatically -- see {@see ExtractionCacheInterface} for why
 * content-hash keying is the load-bearing decision.
 *
 * Cost note (ponytail): content addressing means the file is read once to hash
 * on every cache probe, even on a hit. That is the same trade-off
 * search_api_attachments makes, and it is a net win: a hit skips the upload
 * and the (much slower) server-side parse. If a future site finds hashing
 * itself dominant, a filehash-module field would key without reading -- but
 * that adds a contrib dependency for no demonstrated need, so it is out of
 * scope here.
 *
 * Derived from search_api_attachments' KeyValue cache (GPL-2.0-or-later, same
 * licence as this module): the keyvalue-store-backed get/set/delete shape is
 * adapted; the keying is content-hash here rather than the upstream file-id.
 */
class KeyValueExtractionCache implements ExtractionCacheInterface {

  /**
   * The hash algorithm used to address cache entries.
   *
   * Constant so the prefix in the stored key is self-describing and a future
   * algorithm change never collides with existing entries.
   */
  public const ALGORITHM = 'sha256';

  /**
   * @param \Drupal\Core\KeyValueStore\KeyValueStoreInterface $store
   *   The keyvalue store for the cache collection. The collection name is
   *   decided by the service definition (search_api_wayfinder.services.yml),
   *   not here, so this class is backend-agnostic.
   */
  public function __construct(
    private readonly KeyValueStoreInterface $store,
  ) {}

  /**
   * {@inheritdoc}
   */
  public function get(FileInterface $file): ?string {
    $value = $this->store->get($this->keyFor($file));
    return $value === NULL ? NULL : (string) $value;
  }

  /**
   * {@inheritdoc}
   */
  public function set(FileInterface $file, string $text): void {
    $this->store->set($this->keyFor($file), $text);
  }

  /**
   * {@inheritdoc}
   */
  public function delete(FileInterface $file): void {
    $this->store->delete($this->keyFor($file));
  }

  /**
   * {@inheritdoc}
   */
  public function clear(): void {
    $this->store->deleteAll();
  }

  /**
   * Resolves the cache key for a file: its content hash, prefixed with the
   * algorithm.
   *
   * An unreadable file (hash_file returns FALSE) must not crash or poison the
   * cache: a FALSE hash shared across every unreadable file would collide them
   * all under one key. It falls back to a per-file-id key, so a transiently
   * unreadable file degrades to "no sharing for this one file" instead. The
   * extraction itself will fail and be logged by the caller; the cache simply
   * does the safe thing in the meantime.
   *
   * @param \Drupal\file\FileInterface $file
   *   The file to address.
   *
   * @return string
   *   The cache key.
   */
  protected function keyFor(FileInterface $file): string {
    $hash = @hash_file(self::ALGORITHM, $file->getFileUri());
    if ($hash === FALSE) {
      // @phpcs:ignore Drupal.Security.Safety
      return 'file:' . $file->id();
    }
    return self::ALGORITHM . ':' . $hash;
  }

}
