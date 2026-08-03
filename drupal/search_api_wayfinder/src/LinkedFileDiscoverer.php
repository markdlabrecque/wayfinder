<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder;

use Drupal\Component\Utility\Html;
use Drupal\Core\Entity\ContentEntityInterface;
use Drupal\Core\Entity\EntityStorageInterface;
use Drupal\Core\Entity\EntityTypeManagerInterface;
use Drupal\Core\StreamWrapper\LocalStream;
use Drupal\Core\StreamWrapper\StreamWrapperManagerInterface;
use Drupal\file\FileInterface;

/**
 * Default {@see LinkedFileDiscovererInterface} implementation.
 *
 * The four discovery mechanisms are implemented as small private resolvers that
 * all funnel into one return shape -- distinct files keyed by file id -- so the
 * processor can feed the result straight into the same extract/cache/map
 * plumbing attached files use (#262/#263).
 *
 * Media is resolved by inspecting the media entity's referenced files rather
 * than through \Drupal\media\MediaInterface::getSource(). That avoids a hard
 * dependency on the optional media module (and a test-sandbox autoload shim)
 * and is robust across media types: whatever file-typed field a media type uses
 * as its source, referencedEntities() surfaces it.
 *
 * Stream URIs are reconstructed through the stream wrapper manager, so a site
 * that relocated its public files path is still matched; only the public and
 * private schemes are consulted because those are the two that hold managed
 * downloadable documents in a default install.
 */
class LinkedFileDiscoverer implements LinkedFileDiscovererInterface {

  /**
   * The local stream schemes whose directory paths are consulted when
   * reconstructing a href into a stream URI. Public and private cover the
   * issue's two named forms (/sites/default/files/ and /system/files/).
   */
  private const LOCAL_SCHEMES = ['public', 'private'];

  /**
   * @param \Drupal\Core\Entity\EntityTypeManagerInterface $entityTypeManager
   *   The entity type manager, for loading files and media by id/uuid/uri.
   * @param \Drupal\Core\StreamWrapper\StreamWrapperManagerInterface $streamWrapperManager
   *   The stream wrapper manager, for reconstructing a href path into a
   *   public:// / private:// URI.
   */
  public function __construct(
    private readonly EntityTypeManagerInterface $entityTypeManager,
    private readonly StreamWrapperManagerInterface $streamWrapperManager,
  ) {}

  /**
   * {@inheritdoc}
   */
  public function discoverFromHtml(string $html): array {
    if ($html === '') {
      return [];
    }

    $dom = Html::load($html);
    $xpath = new \DOMXPath($dom);
    $found = [];

    // Mechanisms 1 (data-entity-type=file) and 2 (<drupal-media> embeds): both
    // manifest as a data-entity-type / data-entity-uuid attribute pair and
    // resolve to an entity with no URL parsing.
    foreach ($xpath->query('//*[@data-entity-type][@data-entity-uuid]') as $element) {
      \assert($element instanceof \DOMElement);
      foreach ($this->resolveEntityAttribute($element->getAttribute('data-entity-type'), $element->getAttribute('data-entity-uuid')) as $id => $file) {
        $found[$id] = $file;
      }
    }

    // Mechanism 4 (hrefs), including any entity: URI a bare <a href> carries.
    foreach ($xpath->query('//a[@href]') as $element) {
      \assert($element instanceof \DOMElement);
      foreach ($this->resolveUri($element->getAttribute('href')) as $id => $file) {
        $found[$id] = $file;
      }
    }

    return $found;
  }

  /**
   * {@inheritdoc}
   */
  public function discoverFromLinkUri(string $uri): array {
    return $uri === '' ? [] : $this->resolveUri($uri);
  }

  /**
   * Resolves a data-entity-type / data-entity-uuid pair to files.
   *
   * @return array<int, \Drupal\file\FileInterface>
   */
  private function resolveEntityAttribute(string $type, string $uuid): array {
    if ($uuid === '') {
      return [];
    }
    return match ($type) {
      'file' => $this->collectFiles($this->storage('file')->loadByProperties(['uuid' => $uuid])),
      'media' => $this->sourceFilesOfEntities($this->storage('media')->loadByProperties(['uuid' => $uuid])),
      default => [],
    };
  }

  /**
   * Resolves a link-field uri or href value to files.
   *
   * entity: URIs (mechanism 3) resolve directly; internal:, absolute, and bare
   * paths reduce to the href mechanism (4).
   *
   * @return array<int, \Drupal\file\FileInterface>
   */
  private function resolveUri(string $uri): array {
    if (str_starts_with($uri, 'entity:')) {
      [$entityType, $entityId] = $this->parseEntityUri($uri);
      return match ($entityType) {
        'file' => $this->loadFileById($entityId),
        'media' => $entityId > 0 ? $this->sourceFilesOfEntities([$this->storage('media')->load($entityId)]) : [],
        default => [],
      };
    }

    $path = $this->pathOf($uri);
    return $path === NULL ? [] : $this->resolvePath($path);
  }

  /**
   * Splits an entity: URI into [entity-type, id].
   *
   * entity:file/2 -> ['file', 2]. A malformed body yields ['', 0], which the
   * callers treat as unresolvable.
   *
   * @return array{0: string, 1: int}
   */
  private function parseEntityUri(string $uri): array {
    $body = substr($uri, strlen('entity:'));
    $parts = explode('/', $body, 2);
    return [$parts[0] ?? '', (int) ($parts[1] ?? 0)];
  }

  /**
   * Reduces a uri to a site path suitable for href reconstruction.
   *
   * internal:/foo -> /foo; an absolute URL -> its path component; a bare value
   * is treated as a path. Returns NULL for nothing reconstructable.
   */
  private function pathOf(string $uri): ?string {
    if (str_starts_with($uri, 'internal:')) {
      $path = substr($uri, strlen('internal:'));
      return $path === '' || $path === false ? NULL : $path;
    }
    if (preg_match('#^[a-z][a-z0-9+.\-]*://#i', $uri)) {
      $parts = parse_url($uri);
      return $parts['path'] ?? NULL;
    }
    return $uri === '' ? NULL : $uri;
  }

  /**
   * Reconstructs a site path into a stream URI and looks it up in file_managed.
   *
   * For each local scheme, the wrapper's directory path (e.g. sites/default/
   * files) is matched as a path segment anywhere in $path so a subdirectory
   * install still resolves; the remainder becomes scheme://<remainder>.
   *
   * @return array<int, \Drupal\file\FileInterface>
   */
  private function resolvePath(string $path): array {
    foreach (self::LOCAL_SCHEMES as $scheme) {
      $wrapper = $this->streamWrapperManager->getViaScheme($scheme);
      if (!$wrapper instanceof LocalStream) {
        continue;
      }
      $directory = $wrapper->getDirectoryPath();
      if ($directory === '') {
        continue;
      }
      $prefix = '/' . trim($directory, '/') . '/';
      $position = strpos($path, $prefix);
      if ($position === false) {
        continue;
      }
      $relative = substr($path, $position + strlen($prefix));
      if ($relative === '' || $relative === false) {
        continue;
      }
      return $this->loadFileByUri($scheme . '://' . rawurldecode($relative));
    }
    return [];
  }

  /**
   * Loads a file by its stream URI.
   *
   * @return array<int, \Drupal\file\FileInterface>
   */
  private function loadFileByUri(string $uri): array {
    return $this->collectFiles($this->storage('file')->loadByProperties(['uri' => $uri]));
  }

  /**
   * Loads a single file by id.
   *
   * @return array<int, \Drupal\file\FileInterface>
   */
  private function loadFileById(int $id): array {
    if ($id <= 0) {
      return [];
    }
    $file = $this->storage('file')->load($id);
    return $file instanceof FileInterface ? [(int) $file->id() => $file] : [];
  }

  /**
   * Resolves the source files of one or more media entities.
   *
   * @param iterable<\Drupal\Core\Entity\EntityInterface> $entities
   *   Media entities to inspect.
   *
   * @return array<int, \Drupal\file\FileInterface>
   */
  private function sourceFilesOfEntities(iterable $entities): array {
    $files = [];
    foreach ($entities as $entity) {
      if (!$entity instanceof ContentEntityInterface) {
        continue;
      }
      // The media source field is a file/image entity reference; rather than
      // depend on MediaInterface::getSource() (and the optional media module),
      // take every referenced file. A media with extra file fields picks those
      // up too, which is desirable for "make this media's documents searchable."
      foreach ($entity->referencedEntities() as $referenced) {
        if ($referenced instanceof FileInterface) {
          $files[(int) $referenced->id()] = $referenced;
        }
      }
    }
    return $files;
  }

  /**
   * Filters a loadByProperties result down to files keyed by id.
   *
   * @param array<\Drupal\Core\Entity\EntityInterface> $entities
   *
   * @return array<int, \Drupal\file\FileInterface>
   */
  private function collectFiles(array $entities): array {
    $files = [];
    foreach ($entities as $id => $entity) {
      if ($entity instanceof FileInterface) {
        $files[(int) $id] = $entity;
      }
    }
    return $files;
  }

  /**
   * The entity storage for a type.
   */
  private function storage(string $entityTypeId): EntityStorageInterface {
    return $this->entityTypeManager->getStorage($entityTypeId);
  }

}
