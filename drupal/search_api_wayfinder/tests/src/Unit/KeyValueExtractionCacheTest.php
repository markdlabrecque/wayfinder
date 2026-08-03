<?php

declare(strict_types=1);

namespace Drupal\Tests\search_api_wayfinder\Unit;

use Drupal\Core\KeyValueStore\KeyValueStoreInterface;
use Drupal\file\FileInterface;
use Drupal\search_api_wayfinder\Cache\ExtractionCacheInterface;
use Drupal\search_api_wayfinder\Cache\KeyValueExtractionCache;
use PHPUnit\Framework\TestCase;

/**
 * Tests the keyvalue extraction cache (issue #263): extracted text is stored
 * keyed by file CONTENT HASH, so the same file is extracted once regardless of
 * how many items reference it, and a changed file naturally misses (different
 * content -> different hash) so it re-extracts.
 *
 * The store is mocked; the hash is real (sha256 over a real temp file), so the
 * "content change -> key change" behaviour is exercised honestly without
 * network or Docker.
 *
 * @coversDefaultClass \Drupal\search_api_wayfinder\Cache\KeyValueExtractionCache
 * @group search_api_wayfinder
 */
class KeyValueExtractionCacheTest extends TestCase {

  /**
   * A real temp file with the given contents, returning its path. hash_file()
   * over this path is what the cache keys on, so the tests stay hermetic
   * (local FS only) while exercising real content addressing.
   */
  private function tempFile(string $contents): string {
    $path = tempnam(sys_get_temp_dir(), 'wf_cache_');
    file_put_contents($path, $contents);
    return $path;
  }

  /**
   * A FileInterface mock whose getFileUri() points at $uri and id() is $id.
   */
  private function fileAt(string $uri, int $id = 1): FileInterface {
    $file = $this->createMock(FileInterface::class);
    $file->method('getFileUri')->willReturn($uri);
    $file->method('id')->willReturn($id);
    return $file;
  }

  /**
   * Builds a cache backed by a mocked keyvalue store that records set() calls
   * in an in-memory map, so get() round-trips without a real DB.
   */
  private function cache(array &$store): KeyValueExtractionCache {
    $kv = $this->createMock(KeyValueStoreInterface::class);
    $kv->method('get')->willReturnCallback(function (string $key) use (&$store) {
      return array_key_exists($key, $store) ? $store[$key] : NULL;
    });
    $kv->method('set')->willReturnCallback(function (string $key, $value) use (&$store) {
      $store[$key] = $value;
    });
    $kv->method('has')->willReturnCallback(function (string $key) use (&$store) {
      return array_key_exists($key, $store);
    });
    $kv->method('delete')->willReturnCallback(function (string $key) use (&$store) {
      unset($store[$key]);
    });
    $kv->method('deleteAll')->willReturnCallback(function () use (&$store) {
      $store = [];
    });
    return new KeyValueExtractionCache($kv);
  }

  /**
   * @covers ::get
   */
  public function testGetReturnsNullForAnUncachedFile(): void {
    $store = [];
    $cache = $this->cache($store);
    $file = $this->fileAt($this->tempFile('first'));

    $this->assertNull($cache->get($file));
  }

  /**
   * @covers ::set
   * @covers ::get
   */
  public function testGetReturnsStoredTextAfterSet(): void {
    $store = [];
    $cache = $this->cache($store);
    $file = $this->fileAt($this->tempFile('first'));

    $cache->set($file, 'extracted body');
    $this->assertSame('extracted body', $cache->get($file));
  }

  /**
   * The core requirement (#263): two files with IDENTICAL content share one
   * cache entry -- i.e. the key is the content hash, not the file id. A second
   * file object (different id) with the same bytes must hit the entry the first
   * one wrote. This is what makes "extract once regardless of how many items
   * reference it" true.
   *
   * @covers ::keyFor
   */
  public function testIdenticalContentHitsAcrossDifferentFileObjects(): void {
    $store = [];
    $cache = $this->cache($store);
    $a = $this->fileAt($this->tempFile('same bytes'), id: 1);
    $b = $this->fileAt($this->tempFile('same bytes'), id: 2);

    $cache->set($a, 'shared extraction');
    // Different file id, identical content -> cache hit.
    $this->assertSame('shared extraction', $cache->get($b));
  }

  /**
   * The core requirement (#263): a changed file (different content) naturally
   * misses, because its hash differs. This is the mechanism that makes "a
   * changed file invalidates and re-extracts" hold without explicit eviction.
   *
   * @covers ::keyFor
   */
  public function testChangedContentMissesBecauseTheHashDiffers(): void {
    $store = [];
    $cache = $this->cache($store);
    $original = $this->tempFile('version one');
    $file = $this->fileAt($original);

    $cache->set($file, 'first version text');

    // Same path, new contents -> different hash -> miss.
    file_put_contents($original, 'version two');
    $this->assertNull($cache->get($file));
  }

  /**
   * @covers ::delete
   */
  public function testDeleteRemovesTheEntryForThatContentHash(): void {
    $store = [];
    $cache = $this->cache($store);
    $file = $this->fileAt($this->tempFile('payload'));

    $cache->set($file, 'text');
    $cache->delete($file);
    $this->assertNull($cache->get($file));
  }

  /**
   * @covers ::clear
   */
  public function testClearEmptiesTheCache(): void {
    $store = [];
    $cache = $this->cache($store);
    $file = $this->fileAt($this->tempFile('payload'));

    $cache->set($file, 'text');
    $cache->clear();
    $this->assertNull($cache->get($file));
  }

  /**
   * The key for a file is the content hash prefixed with the algorithm, so the
   * stored entry is self-describing and never collides with a future algorithm
   * change. Asserting the exact shape locks the wire format of the key.
   *
   * @covers ::keyFor
   */
  public function testKeyIsSha256OfTheFileContents(): void {
    $store = [];
    $cache = $this->cache($store);
    $path = $this->tempFile('deterministic');
    $file = $this->fileAt($path);

    $cache->set($file, 'text');
    $expected = 'sha256:' . hash_file('sha256', $path);
    $this->assertArrayHasKey($expected, $store);
  }

  /**
   * @covers ::keyFor
   *
   * An unreadable file (hash_file fails) must not crash the cache or poison it
   * with a falsey key that every bad file would share. It falls back to a
   * per-file-id key so a transiently-unreadable file degrades to "no caching
   * for this one file" rather than a crash or cross-file collision.
   */
  public function testUnreadableFileDegradesToAPerFileIdKey(): void {
    $store = [];
    $cache = $this->cache($store);
    // A path that does not exist -> hash_file returns false.
    $file = $this->fileAt('/does/not/exist/' . uniqid('', TRUE), id: 42);

    $cache->set($file, 'text');
    $this->assertArrayHasKey('file:42', $store);
  }

  /**
   * Smoke test that the implementation satisfies the interface contract.
   */
  public function testImplementsExtractionCacheInterface(): void {
    $store = [];
    $cache = $this->cache($store);
    $this->assertInstanceOf(ExtractionCacheInterface::class, $cache);
  }

}
