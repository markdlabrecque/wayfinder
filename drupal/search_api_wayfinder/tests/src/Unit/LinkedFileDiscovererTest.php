<?php

declare(strict_types=1);

namespace Drupal\Tests\search_api_wayfinder\Unit;

use Drupal\Core\Entity\ContentEntityInterface;
use Drupal\Core\Entity\EntityStorageInterface;
use Drupal\Core\Entity\EntityTypeManagerInterface;
use Drupal\Core\StreamWrapper\LocalStream;
use Drupal\Core\StreamWrapper\StreamWrapperManagerInterface;
use Drupal\file\FileInterface;
use Drupal\search_api_wayfinder\LinkedFileDiscoverer;
use PHPUnit\Framework\TestCase;

/**
 * Tests the linked-file discoverer (issue #265): resolving files that content
 * *links to* (body embeds, link fields, hrefs) rather than attaches.
 *
 * One test per discovery mechanism, in the issue's reliability order, plus the
 * relative and private href forms the issue calls out and the
 * changed-linked-file reindex path's input (every mechanism feeds the same
 * file->item map as #262). The discoverer is pure -- it never touches the
 * extraction client or the index -- so it is tested in isolation, mocking only
 * Drupal's entity stores and the stream wrapper manager.
 *
 * @coversDefaultClass \Drupal\search_api_wayfinder\LinkedFileDiscoverer
 * @group search_api_wayfinder
 */
class LinkedFileDiscovererTest extends TestCase {

  /**
   * The discoverer under test, built with the given stores and wrappers.
   */
  private function discoverer(?EntityTypeManagerInterface $etm = NULL, ?StreamWrapperManagerInterface $swm = NULL): LinkedFileDiscoverer {
    return new LinkedFileDiscoverer(
      $etm ?? $this->createMock(EntityTypeManagerInterface::class),
      $swm ?? $this->emptyWrappers(),
    );
  }

  /**
   * Mechanism 1: a data-entity-type="file" / data-entity-uuid attribute (the
   * form CKEditor's editor module writes when a file is inserted) resolves
   * straight to the file by UUID -- no URL parsing.
   *
   * @covers ::discoverFromHtml
   */
  public function testDiscoversAFileByDataEntityTypeAttribute(): void {
    $file = $this->fileEntity(7);
    $etm = $this->etmWithFileStorage(['uuid' => 'FILE-UUID'], [7 => $file]);

    $found = $this->discoverer($etm)->discoverFromHtml(
      '<p>See <a data-entity-type="file" data-entity-uuid="FILE-UUID" href="/sites/default/files/d.pdf">the doc</a>.</p>',
    );

    $this->assertSame([7 => $file], $found);
  }

  /**
   * A data-entity-type that is not file or media (e.g. node) is not a document
   * and resolves to nothing.
   *
   * @covers ::discoverFromHtml
   */
  public function testIgnoresANonDocumentDataEntityType(): void {
    $etm = $this->createMock(EntityTypeManagerInterface::class);
    $etm->expects($this->never())->method('getStorage');

    $found = $this->discoverer($etm)->discoverFromHtml(
      '<a data-entity-type="node" data-entity-uuid="NODE-UUID">a node link</a>',
    );

    $this->assertSame([], $found);
  }

  /**
   * Mechanism 2: a <drupal-media> embed (the media_embed filter placeholder)
   * carries data-entity-type="media" / data-entity-uuid and resolves to the
   * media's source file by inspecting the media's file-typed fields -- no
   * MediaInterface dependency, so the optional media module need not be loaded.
   *
   * @covers ::discoverFromHtml
   */
  public function testDiscoversAFileFromADrupalMediaEmbed(): void {
    $source = $this->fileEntity(9);
    $media = $this->createMock(ContentEntityInterface::class);
    $media->method('referencedEntities')->willReturn([$source]);

    $etm = $this->createMock(EntityTypeManagerInterface::class);
    $etm->method('getStorage')->willReturnCallback(fn (string $type) => match ($type) {
      'media' => $this->storageReturning(['uuid' => 'MEDIA-UUID'], [3 => $media]),
      default => $this->createMock(EntityStorageInterface::class),
    });

    $found = $this->discoverer($etm)->discoverFromHtml(
      '<drupal-media data-entity-type="media" data-entity-uuid="MEDIA-UUID"></drupal-media>',
    );

    $this->assertSame([9 => $source], $found);
  }

  /**
   * Mechanism 3 (entity: form): a link-field uri entity:file/N resolves
   * directly to file N by id.
   *
   * @covers ::discoverFromLinkUri
   */
  public function testDiscoversAFileFromAnEntityFileLinkUri(): void {
    $file = $this->fileEntity(2);
    $etm = $this->etmWithFileStorageReturningLoad(2, $file);

    $found = $this->discoverer($etm)->discoverFromLinkUri('entity:file/2');

    $this->assertSame([2 => $file], $found);
  }

  /**
   * Mechanism 3 (entity: media form): entity:media/N resolves to the media and
   * then to its source file. A link to a node (entity:node/N) is not a document
   * and resolves to nothing.
   *
   * @covers ::discoverFromLinkUri
   */
  public function testDiscoversAFileFromAnEntityMediaLinkUri(): void {
    $source = $this->fileEntity(11);
    $media = $this->createMock(ContentEntityInterface::class);
    $media->method('referencedEntities')->willReturn([$source]);

    $etm = $this->createMock(EntityTypeManagerInterface::class);
    $etm->method('getStorage')->willReturnCallback(fn (string $type) => match ($type) {
      'media' => $this->storageReturningLoad(3, $media),
      default => $this->createMock(EntityStorageInterface::class),
    });

    $this->assertSame([11 => $source], $this->discoverer($etm)->discoverFromLinkUri('entity:media/3'));
  }

  /**
   * @covers ::discoverFromLinkUri
   */
  public function testALinkToANodeResolvesToNothing(): void {
    $etm = $this->createMock(EntityTypeManagerInterface::class);
    $etm->expects($this->never())->method('getStorage');

    $this->assertSame([], $this->discoverer($etm)->discoverFromLinkUri('entity:node/1'));
  }

  /**
   * Mechanism 3 + 4 (internal: relative form): internal:/sites/default/files/…
   * reduces to the public href mechanism and resolves through the stream
   * wrapper manager to a public:// uri looked up in file_managed.
   *
   * @covers ::discoverFromLinkUri
   * @covers ::resolvePath
   */
  public function testDiscoversAFileFromAnInternalRelativeLinkUri(): void {
    $file = $this->fileEntity(5);
    $etm = $this->etmWithFileStorage(['uri' => 'public://docs/foo.pdf'], [5 => $file]);

    $found = $this->discoverer($etm, $this->publicPrivateWrappers())->discoverFromLinkUri('internal:/sites/default/files/docs/foo.pdf');

    $this->assertSame([5 => $file], $found);
  }

  /**
   * Mechanism 4 (relative href): /sites/default/files/… in body markup resolves
   * to a managed file by reconstructing the public:// stream uri. This is
   * exactly where the existing contrib option fails (search_api_attachments_bf
   * only matches absolute URLs whose host equals the site's).
   *
   * @covers ::discoverFromHtml
   * @covers ::resolvePath
   */
  public function testDiscoversAFileFromARelativeHref(): void {
    $file = $this->fileEntity(5);
    $etm = $this->etmWithFileStorage(['uri' => 'public://docs/foo.pdf'], [5 => $file]);

    $found = $this->discoverer($etm, $this->publicPrivateWrappers())->discoverFromHtml(
      '<p><a href="/sites/default/files/docs/foo.pdf">download</a></p>',
    );

    $this->assertSame([5 => $file], $found);
  }

  /**
   * Mechanism 4 (private href): /system/files/… resolves to a private:// uri,
   * the other form the issue names explicitly.
   *
   * @covers ::discoverFromHtml
   * @covers ::resolvePath
   */
  public function testDiscoversAFileFromAPrivateHref(): void {
    $file = $this->fileEntity(6);
    $etm = $this->etmWithFileStorage(['uri' => 'private://docs/secret.pdf'], [6 => $file]);

    $found = $this->discoverer($etm, $this->publicPrivateWrappers())->discoverFromHtml(
      '<p><a href="/system/files/docs/secret.pdf">download</a></p>',
    );

    $this->assertSame([6 => $file], $found);
  }

  /**
   * An absolute href pointing at the site resolves through its path component
   * (the host is irrelevant -- only managed files resolve, so there is no SSRF
   * surface and nothing is fetched).
   *
   * @covers ::discoverFromHtml
   * @covers ::resolvePath
   */
  public function testDiscoversAFileFromAnAbsoluteSiteHref(): void {
    $file = $this->fileEntity(5);
    $etm = $this->etmWithFileStorage(['uri' => 'public://docs/foo.pdf'], [5 => $file]);

    $found = $this->discoverer($etm, $this->publicPrivateWrappers())->discoverFromHtml(
      '<a href="https://example.com/sites/default/files/docs/foo.pdf">d</a>',
    );

    $this->assertSame([5 => $file], $found);
  }

  /**
   * An external absolute href never matches a managed file, so it resolves to
   * nothing -- external links are off by construction (no fetch, no SSRF).
   *
   * @covers ::discoverFromHtml
   */
  public function testAnExternalAbsoluteHrefResolvesToNothing(): void {
    $storage = $this->createMock(EntityStorageInterface::class);
    $storage->expects($this->never())->method('loadByProperties');
    $etm = $this->createMock(EntityTypeManagerInterface::class);
    $etm->method('getStorage')->willReturn($storage);

    $found = $this->discoverer($etm, $this->publicPrivateWrappers())->discoverFromHtml(
      '<a href="https://other.example/somewhere/else.pdf">external</a>',
    );

    $this->assertSame([], $found);
  }

  /**
   * A path that maps to no managed file (a node route, a random path) resolves
   * to nothing rather than erroring.
   *
   * @covers ::resolvePath
   */
  public function testAnUnresolvableHrefResolvesToNothing(): void {
    $etm = $this->etmWithFileStorage(['uri' => 'public://node/1'], []);

    $found = $this->discoverer($etm, $this->publicPrivateWrappers())->discoverFromHtml(
      '<a href="/node/1">a page</a>',
    );

    $this->assertSame([], $found);
  }

  /**
   * A href wrapped in an entity: URI (entity:file/N inside an <a href>) resolves
   * the same way a link-field uri does -- the attribute path is preferred, but a
   * bare href must not be missed.
   *
   * @covers ::discoverFromHtml
   */
  public function testDiscoversAFileFromAnEntityHref(): void {
    $file = $this->fileEntity(2);
    $etm = $this->etmWithFileStorageReturningLoad(2, $file);

    $found = $this->discoverer($etm)->discoverFromHtml('<a href="entity:file/2">doc</a>');

    $this->assertSame([2 => $file], $found);
  }

  /**
   * The same file discovered more than once in one field (attribute AND href)
   * is returned once -- the cache and the map key on the file, not the link.
   *
   * @covers ::discoverFromHtml
   */
  public function testFilesDiscoveredMultipleWaysAreDeduplicated(): void {
    $file = $this->fileEntity(7);
    // Same uuid for the attribute; same uri for the href -> one file.
    $storage = $this->createMock(EntityStorageInterface::class);
    $storage->method('loadByProperties')->willReturnCallback(function (array $props) use ($file) {
      return isset($props['uuid']) ? [7 => $file] : [7 => $file];
    });
    $etm = $this->createMock(EntityTypeManagerInterface::class);
    $etm->method('getStorage')->willReturn($storage);

    $found = $this->discoverer($etm, $this->publicPrivateWrappers())->discoverFromHtml(
      '<a data-entity-type="file" data-entity-uuid="FILE-UUID" href="/sites/default/files/d.pdf">d</a>',
    );

    $this->assertSame([7 => $file], $found);
  }

  /**
   * An empty string is a no-op, not a parse error.
   *
   * @covers ::discoverFromHtml
   * @covers ::discoverFromLinkUri
   */
  public function testEmptyInputResolvesToNothing(): void {
    $discoverer = $this->discoverer();
    $this->assertSame([], $discoverer->discoverFromHtml(''));
    $this->assertSame([], $discoverer->discoverFromLinkUri(''));
  }

  // ---------------------------------------------------------------- helpers.

  /**
   * A FileInterface mock with an id, the value the map and dedup key on.
   */
  private function fileEntity(int $id): FileInterface {
    $file = $this->createMock(FileInterface::class);
    $file->method('id')->willReturn($id);
    return $file;
  }

  /**
   * EntityTypeManager whose file storage returns $files for a loadByProperties
   * with exactly $expectedProps (used for uuid and uri lookups).
   */
  private function etmWithFileStorage(array $expectedProps, array $files): EntityTypeManagerInterface {
    $etm = $this->createMock(EntityTypeManagerInterface::class);
    $etm->method('getStorage')->willReturn($this->storageReturning($expectedProps, $files));
    return $etm;
  }

  /**
   * EntityTypeManager whose file storage returns $file for load($id) (used for
   * entity:file/N).
   */
  private function etmWithFileStorageReturningLoad(int $id, FileInterface $file): EntityTypeManagerInterface {
    $etm = $this->createMock(EntityTypeManagerInterface::class);
    $etm->method('getStorage')->willReturn($this->storageReturningLoad($id, $file));
    return $etm;
  }

  /**
   * A storage double whose loadByProperties returns $files only when called with
   * exactly $expectedProps.
   */
  private function storageReturning(array $expectedProps, array $files): EntityStorageInterface {
    $storage = $this->createMock(EntityStorageInterface::class);
    $storage->method('loadByProperties')->willReturnCallback(fn (array $props) => $props === $expectedProps ? $files : []);
    return $storage;
  }

  /**
   * A storage double whose load($id) returns $file.
   */
  private function storageReturningLoad(int $id, object $file): EntityStorageInterface {
    $storage = $this->createMock(EntityStorageInterface::class);
    $storage->method('load')->willReturnCallback(fn ($arg) => $arg === $id ? $file : NULL);
    return $storage;
  }

  /**
   * Stream wrapper manager exposing public and private local wrappers with the
   * standard directory paths.
   */
  private function publicPrivateWrappers(): StreamWrapperManagerInterface {
    $public = $this->createMock(LocalStream::class);
    $public->method('getDirectoryPath')->willReturn('sites/default/files');
    $private = $this->createMock(LocalStream::class);
    $private->method('getDirectoryPath')->willReturn('system/files');

    $swm = $this->createMock(StreamWrapperManagerInterface::class);
    $swm->method('getViaScheme')->willReturnCallback(fn (string $scheme) => match ($scheme) {
      'public' => $public,
      'private' => $private,
      default => NULL,
    });
    return $swm;
  }

  /**
   * Stream wrapper manager that resolves no schemes (the discoverer's behaviour
   * when no local wrappers are available).
   */
  private function emptyWrappers(): StreamWrapperManagerInterface {
    $swm = $this->createMock(StreamWrapperManagerInterface::class);
    $swm->method('getViaScheme')->willReturn(NULL);
    return $swm;
  }

}
