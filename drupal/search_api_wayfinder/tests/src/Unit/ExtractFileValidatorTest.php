<?php

declare(strict_types=1);

namespace Drupal\Tests\search_api_wayfinder\Unit;

use Drupal\file\FileInterface;
use Drupal\search_api_wayfinder\ExtractFileValidator;
use PHPUnit\Framework\TestCase;
use Symfony\Component\Mime\MimeTypeGuesserInterface;

/**
 * Tests ExtractFileValidator: the indexability guards and extraction limits
 * ported from search_api_attachments (ExtractFileValidator +
 * FilesExtractor::isFileIndexable/limitToAllowedNumber/limitBytes).
 *
 * Every rule here is a *refusal* or a *bound* -- its whole value is in not
 * doing work -- so per this repo's CLAUDE.md each is mutation-tested (break
 * the guard deliberately, confirm a test catches it, revert). The mutation
 * matrix is recorded in docs/reports/2026-08-04-indexability-rules-extraction.md.
 *
 * Why these exist alongside server-side budgets (#257): different trust
 * boundaries. #257 protects Wayfinder from *any* client and stays fail-closed
 * regardless of what Drupal does; these stop Drupal from uploading files it
 * already knows will be rejected and control index bloat / relevance skew the
 * server cannot see. Neither weakens the other.
 *
 * @coversDefaultClass \Drupal\search_api_wayfinder\ExtractFileValidator
 * @group search_api_wayfinder
 */
class ExtractFileValidatorTest extends TestCase {

  /**
   * Rule 1: excluded extensions are mapped to MIME types (via the MIME
   * guesser called on a synthetic `dummy.<ext>` filename) and de-duplicated,
   * because many extensions collapse to one MIME type (jpg/jpeg, tif/tiff).
   *
   * The willReturnMap keys assert the `dummy.` convention: the guesser must
   * be called with a synthetic filename built from the extension, never the
   * bare extension. Removing that prefix makes every call miss the map and
   * return NULL, failing this test.
   *
   * @covers ::getExcludedMimes
   */
  public function testGetExcludedMimesMapsExtensionsToMimesAndDeduplicates(): void {
    $guesser = $this->createMock(MimeTypeGuesserInterface::class);
    $guesser->method('guessMimeType')->willReturnMap([
      ['dummy.png', 'image/png'],
      ['dummy.jpg', 'image/jpeg'],
      ['dummy.jpeg', 'image/jpeg'],
    ]);

    $validator = new ExtractFileValidator($guesser);

    // jpg + jpeg map to the same MIME -> one entry, order preserved.
    $this->assertSame(
      ['image/png', 'image/jpeg'],
      $validator->getExcludedMimes(['png', 'jpg', 'jpeg']),
    );
  }

  /**
   * Rule 1 (default list): with no extensions supplied, the validator falls
   * back to DEFAULT_EXCLUDED_EXTENSIONS. The expected set is derived from the
   * constant rather than hard-coded, so this stays correct if the default
   * list is edited.
   *
   * @covers ::getExcludedMimes
   */
  public function testGetExcludedMimesFallsBackToDefaultExtensionsWhenNoneGiven(): void {
    $guesser = $this->createMock(MimeTypeGuesserInterface::class);
    $guesser->method('guessMimeType')->willReturnCallback(
      fn (string $path): string => 'x-test/' . substr($path, strlen('dummy.')),
    );

    $validator = new ExtractFileValidator($guesser);

    $expected = array_values(array_unique(array_map(
      fn (string $extension): string => 'x-test/' . $extension,
      explode(' ', ExtractFileValidator::DEFAULT_EXCLUDED_EXTENSIONS),
    )));

    $this->assertSame($expected, $validator->getExcludedMimes([]));
  }

  /**
   * Rule 2: files above the configured max size are refused; '0' / '' mean no
   * restriction. The boundary (file size == limit) is allowed, so the
   * comparison is `<=` not `<`.
   *
   * @covers ::isFileSizeAllowed
   * @dataProvider isFileSizeAllowedProvider
   */
  public function testIsFileSizeAllowed(int $fileSize, string $maxFilesize, bool $expected): void {
    $validator = new ExtractFileValidator($this->createMock(MimeTypeGuesserInterface::class));
    $file = $this->mockFile(size: $fileSize);

    $this->assertSame($expected, $validator->isFileSizeAllowed($file, $maxFilesize));
  }

  /**
   * Data provider for ::testIsFileSizeAllowed.
   *
   * Sizes use Drupal's Bytes::toNumber semantics (K = 1024): '1 KB' -> 1024.
   */
  public static function isFileSizeAllowedProvider(): array {
    return [
      'under limit is allowed' => [500, '1 KB', TRUE],
      'over limit is refused' => [2000, '1 KB', FALSE],
      'exactly at the limit is allowed' => [1024, '1 KB', TRUE],
      'zero limit means no restriction' => [5_000_000_000, '0', TRUE],
      'empty limit means no restriction' => [9999, '', TRUE],
    ];
  }

  /**
   * Rule 3: private-file policy.
   *
   * Access-control decision (issue #264): indexing a private file's contents
   * makes them searchable through the item that references it, because Search
   * API access control is per item, not per attachment. Default policy is to
   * EXCLUDE private files (exclude-private = TRUE) rather than leak them by
   * accident; a site that accepts the risk sets exclude-private = FALSE.
   *
   * @covers ::isPrivateFileAllowed
   * @dataProvider isPrivateFileAllowedProvider
   */
  public function testIsPrivateFileAllowed(string $uri, bool $excludedPrivate, bool $expected): void {
    $validator = new ExtractFileValidator($this->createMock(MimeTypeGuesserInterface::class));
    $file = $this->mockFile(uri: $uri);

    $this->assertSame($expected, $validator->isPrivateFileAllowed($file, $excludedPrivate));
  }

  /**
   * Data provider for ::testIsPrivateFileAllowed.
   */
  public static function isPrivateFileAllowedProvider(): array {
    return [
      'public file, exclude-private on -> allowed' => ['public://docs/foo.pdf', TRUE, TRUE],
      'private file, exclude-private on -> refused (the default, safe policy)' => ['private://docs/secret.pdf', TRUE, FALSE],
      'private file, exclude-private off -> allowed (opt-in to private indexing)' => ['private://docs/secret.pdf', FALSE, TRUE],
      'public file, exclude-private off -> allowed' => ['public://docs/foo.pdf', FALSE, TRUE],
    ];
  }

  /**
   * Rule 4: at most N files per file field are indexed; 0 means no
   * restriction. The slice keeps the first N (widget-weight order), matching
   * search_api_attachments' behaviour.
   *
   * @covers ::limitToAllowedNumber
   * @dataProvider limitToAllowedNumberProvider
   */
  public function testLimitToAllowedNumber(array $fileIds, int $numberIndexed, array $expected): void {
    $validator = new ExtractFileValidator($this->createMock(MimeTypeGuesserInterface::class));

    $this->assertSame($expected, $validator->limitToAllowedNumber($fileIds, $numberIndexed));
  }

  /**
   * Data provider for ::testLimitToAllowedNumber.
   */
  public static function limitToAllowedNumberProvider(): array {
    return [
      'zero means no restriction' => [[10, 20, 30, 40, 50], 0, [10, 20, 30, 40, 50]],
      'slices to the first N' => [[10, 20, 30, 40, 50], 3, [10, 20, 30]],
      'fewer than the limit is unchanged' => [[10, 20], 3, [10, 20]],
      'an empty list stays empty' => [[], 3, []],
    ];
  }

  /**
   * Rule 5: the extracted string is bounded to the first N bytes before
   * indexing; '0' / '' mean no restriction. The cut is multibyte-safe
   * (mb_strcut), so it never splits a character mid-codepoint.
   *
   * The 'aあb' cases are the multibyte mutation guard: 'あ' is three UTF-8
   * bytes, so a 2-byte budget must yield 'a' (it cannot include 'あ'). A
   * substr() implementation would return a broken 2-byte sequence here.
   *
   * @covers ::limitBytes
   * @dataProvider limitBytesProvider
   */
  public function testLimitBytes(string $text, string $maxBytes, string $expected): void {
    $validator = new ExtractFileValidator($this->createMock(MimeTypeGuesserInterface::class));

    $this->assertSame($expected, $validator->limitBytes($text, $maxBytes));
  }

  /**
   * Data provider for ::testLimitBytes.
   */
  public static function limitBytesProvider(): array {
    return [
      'zero means no restriction' => ['abcdefghij', '0', 'abcdefghij'],
      'empty means no restriction' => ['abcdefghij', '', 'abcdefghij'],
      'cuts to the first N bytes' => ['abcdefghij', '5', 'abcde'],
      'multibyte-safe: a 2-byte budget stops before the 3-byte char' => ['aあb', '2', 'a'],
      'multibyte-safe: a budget covering the whole string is unchanged' => ['aあb', '5', 'aあb'],
    ];
  }

  /**
   * The composite guard: isFileIndexable refuses a file if ANY of the
   * MIME / size / private rules refuse it, and allows it only when all three
   * pass. This is the load-bearing entry point the extraction processor
   * (#262) will call per file.
   *
   * @covers ::isFileIndexable
   */
  public function testIsFileIndexableComposesAllGuardRules(): void {
    $guesser = $this->createMock(MimeTypeGuesserInterface::class);
    $guesser->method('guessMimeType')->willReturnMap([
      ['dummy.png', 'image/png'],
    ]);
    $validator = new ExtractFileValidator($guesser);
    $excludedMimes = $validator->getExcludedMimes(['png']);

    // Allowed by every rule.
    $allowed = $this->mockFile(mime: 'application/pdf', size: 500, uri: 'public://docs/ok.pdf');
    $this->assertTrue($validator->isFileIndexable($allowed, $excludedMimes, '1 KB', TRUE));

    // Refused by the excluded-MIME rule.
    $excluded = $this->mockFile(mime: 'image/png', size: 500, uri: 'public://docs/img.png');
    $this->assertFalse($validator->isFileIndexable($excluded, $excludedMimes, '1 KB', TRUE));

    // Refused by the size rule.
    $tooBig = $this->mockFile(mime: 'application/pdf', size: 10_000, uri: 'public://docs/huge.pdf');
    $this->assertFalse($validator->isFileIndexable($tooBig, $excludedMimes, '1 KB', TRUE));

    // Refused by the private-file rule (default exclude-private = TRUE).
    $private = $this->mockFile(mime: 'application/pdf', size: 500, uri: 'private://docs/secret.pdf');
    $this->assertFalse($validator->isFileIndexable($private, $excludedMimes, '1 KB', TRUE));
  }

  /**
   * Builds a FileInterface mock wired with the getters the validator reads.
   */
  private function mockFile(int $size = 0, string $mime = 'application/octet-stream', string $uri = 'public://file.bin'): FileInterface {
    $file = $this->createMock(FileInterface::class);
    $file->method('getSize')->willReturn($size);
    $file->method('getMimeType')->willReturn($mime);
    $file->method('getFileUri')->willReturn($uri);
    return $file;
  }

}
