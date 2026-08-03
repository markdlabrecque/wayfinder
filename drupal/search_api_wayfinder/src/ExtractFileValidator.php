<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder;

use Drupal\Component\Utility\Bytes;
use Drupal\file\FileInterface;
use Symfony\Component\Mime\MimeTypeGuesserInterface;

/**
 * Decides whether a file is indexable and bounds how much of it is indexed.
 *
 * Ported from search_api_attachments' {@see \Drupal\search_api_attachments\ExtractFileValidator}
 * (getExcludedMimes / isFileSizeAllowed / isPrivateFileAllowed) and
 * FilesExtractor::isFileIndexable() / ::limitToAllowedNumber() / ::limitBytes().
 * search_api_attachments is GPL-2.0-or-later, same licence as this module, so
 * adapting its code is licence-compatible; the class name and method roles
 * mirror it deliberately.
 *
 * This is guard code: every method's whole value is in *refusing* work, so
 * each is mutation-tested (tests/src/Unit/ExtractFileValidatorTest.php).
 *
 * Trust boundary: these rules are NOT a substitute for the server-side
 * extraction budgets in #257. #257 protects Wayfinder from any client and
 * stays fail-closed regardless of what Drupal does; these rules stop Drupal
 * from uploading files it already knows will be rejected and control index
 * bloat / relevance skew the server cannot see. Weakening one because the
 * other exists is a bug.
 *
 * All configuration is passed as explicit method parameters rather than read
 * from processor plugin config, so the rules are independently unit-testable
 * and decoupled from the extraction processor (#262) and the settings form /
 * config schema (#266) that wire them.
 */
class ExtractFileValidator {

  /**
   * Default excluded file extensions: media and image types that carry no
   * extractable text.
   *
   * Copied verbatim from search_api_attachments' ExtractFileValidator; a site
   * overrides this list through processor configuration (#266). Extensions are
   * internally mapped to a MIME type, so variations sharing one MIME (tif and
   * tiff, jpg and jpeg) need only one representative.
   */
  public const DEFAULT_EXCLUDED_EXTENSIONS = 'aif art avi bmp gif ico mov oga ogv png psd ra ram rgb flv';

  /**
   * Constructs an ExtractFileValidator.
   *
   * @param \Symfony\Component\Mime\MimeTypeGuesserInterface $mimeTypeGuesser
   *   The MIME type guesser used to turn excluded extensions into MIME types.
   */
  public function __construct(
    private readonly MimeTypeGuesserInterface $mimeTypeGuesser,
  ) {}

  /**
   * Maps excluded file extensions to their MIME types.
   *
   * The actual index-time check is by MIME type, not extension, so this
   * conversion happens once (at config time) and the resulting list is reused
   * for every file. Each extension is guessed as a synthetic `dummy.<ext>`
   * filename -- the guesser keys on a path with an extension, not a bare
   * extension. Many extensions collapse to one MIME type, so the result is
   * de-duplicated.
   *
   * @param string[] $extensions
   *   File extensions without a leading dot. When empty, falls back to
   *   DEFAULT_EXCLUDED_EXTENSIONS.
   *
   * @return string[]
   *   Unique MIME types for the given extensions.
   */
  public function getExcludedMimes(array $extensions): array {
    if (empty($extensions)) {
      $extensions = explode(' ', self::DEFAULT_EXCLUDED_EXTENSIONS);
    }

    $mimes = [];
    foreach ($extensions as $extension) {
      $guessed = $this->mimeTypeGuesser->guessMimeType('dummy.' . $extension);
      if ($guessed !== NULL) {
        $mimes[] = $guessed;
      }
    }

    return array_values(array_unique($mimes));
  }

  /**
   * Rule 2: whether the file is within the configured maximum size.
   *
   * @param \Drupal\file\FileInterface $file
   *   The file being considered.
   * @param string $maxFilesize
   *   Max size as a byte string understood by Bytes::toNumber (e.g. '10 MB'),
   *   or '0' / '' for no restriction.
   *
   * @return bool
   *   TRUE if the file should be indexed on size grounds.
   */
  public function isFileSizeAllowed(FileInterface $file, string $maxFilesize): bool {
    if ($maxFilesize === '' || $maxFilesize === '0') {
      return TRUE;
    }

    return $file->getSize() <= Bytes::toNumber($maxFilesize);
  }

  /**
   * Rule 3: whether a private file may be indexed under the configured policy.
   *
   * Access-control decision (issue #264): indexing a private file's contents
   * makes them searchable through the item that references it, because Search
   * API access control is per item, not per attachment. The default policy is
   * to EXCLUDE private files ($excludedPrivate = TRUE) rather than leak them
   * by accident; a site that accepts the risk sets $excludedPrivate = FALSE.
   *
   * @param \Drupal\file\FileInterface $file
   *   The file being considered.
   * @param bool $excludedPrivate
   *   TRUE to exclude files in the private:// scheme.
   *
   * @return bool
   *   TRUE if the file should be indexed on private-storage grounds.
   */
  public function isPrivateFileAllowed(FileInterface $file, bool $excludedPrivate): bool {
    if (!$excludedPrivate) {
      return TRUE;
    }

    return !str_starts_with($file->getFileUri(), 'private://');
  }

  /**
   * Rule 4: limits the number of files indexed per file field.
   *
   * Keeps the first N file IDs (widget-weight order), matching
   * search_api_attachments. 0 means no restriction.
   *
   * @param array $fileIds
   *   The file IDs referenced by one field.
   * @param int $numberIndexed
   *   Maximum number of files to index; 0 for no restriction.
   *
   * @return array
   *   At most $numberIndexed file IDs.
   */
  public function limitToAllowedNumber(array $fileIds, int $numberIndexed): array {
    // 0 means no restriction (the form's #min is 0). Any negative is treated
    // the same defensively rather than letting array_slice's negative-length
    // semantics corrupt the list.
    if ($numberIndexed <= 0) {
      return $fileIds;
    }

    return array_slice($fileIds, 0, $numberIndexed);
  }

  /**
   * Rule 5: bounds the extracted string to its first N bytes before indexing.
   *
   * The cut is multibyte-safe (mb_strcut), so it never splits a character
   * mid-codepoint. This is a Drupal-side relevance / bloat control, distinct
   * from the server-side extraction budget in #257 (different trust boundary).
   *
   * @param string $extractedText
   *   The full extracted text.
   * @param string $numberFirstBytes
   *   Max bytes as a byte string understood by Bytes::toNumber (e.g. '1 MB'),
   *   or '0' / '' for no restriction.
   *
   * @return string
   *   At most the first $numberFirstBytes bytes of $extractedText.
   */
  public function limitBytes(string $extractedText, string $numberFirstBytes): string {
    if ($numberFirstBytes === '' || $numberFirstBytes === '0') {
      return $extractedText;
    }

    $bytes = (int) Bytes::toNumber($numberFirstBytes);
    if ($bytes <= 0) {
      return $extractedText;
    }

    return mb_strcut($extractedText, 0, $bytes);
  }

  /**
   * Composite guard: whether a file should be indexed at all.
   *
   * Combines rules 1 (excluded MIME), 2 (size) and 3 (private). A file is
   * indexable only when all three pass. Filesystem-existence and permanence
   * checks are deliberately NOT ported here: they are processor-side
   * preconditions (non-hermetic, #262 owns them), not part of the five
   * indexability rules in #264.
   *
   * @param \Drupal\file\FileInterface $file
   *   The file being considered.
   * @param string[] $excludedMimes
   *   MIME types to exclude (from ::getExcludedMimes()).
   * @param string $maxFilesize
   *   Max size byte string, or '0' / '' for no restriction.
   * @param bool $excludedPrivate
   *   TRUE to exclude private:// files.
   *
   * @return bool
   *   TRUE if the file passes every indexability rule.
   */
  public function isFileIndexable(FileInterface $file, array $excludedMimes, string $maxFilesize, bool $excludedPrivate): bool {
    if ($this->isExcludedMimeType($file, $excludedMimes)) {
      return FALSE;
    }

    if (!$this->isFileSizeAllowed($file, $maxFilesize)) {
      return FALSE;
    }

    if (!$this->isPrivateFileAllowed($file, $excludedPrivate)) {
      return FALSE;
    }

    return TRUE;
  }

  /**
   * Whether the file's MIME type is in the excluded set.
   *
   * @param \Drupal\file\FileInterface $file
   *   The file being considered.
   * @param string[] $excludedMimes
   *   MIME types to exclude.
   *
   * @return bool
   *   TRUE if the file's MIME type is excluded.
   */
  private function isExcludedMimeType(FileInterface $file, array $excludedMimes): bool {
    return in_array($file->getMimeType(), $excludedMimes, TRUE);
  }

}
