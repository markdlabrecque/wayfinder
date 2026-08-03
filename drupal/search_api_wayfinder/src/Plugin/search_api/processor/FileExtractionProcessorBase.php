<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder\Plugin\search_api\processor;

use Drupal\Component\Utility\Bytes;
use Drupal\Core\Entity\EntityInterface;
use Drupal\Core\Entity\EntityTypeManagerInterface;
use Drupal\Core\Form\FormStateInterface;
use Drupal\Core\Plugin\PluginFormInterface;
use Drupal\Core\Queue\QueueInterface;
use Drupal\file\FileInterface;
use Drupal\search_api\Item\ItemInterface;
use Drupal\search_api\Processor\ProcessorPluginBase;
use Drupal\search_api\SearchApiException;
use Drupal\search_api_wayfinder\Cache\ExtractionCacheInterface;
use Drupal\search_api_wayfinder\ExtractFileValidator;
use Drupal\search_api_wayfinder\FileReferenceMapInterface;
use Drupal\search_api_wayfinder\Plugin\search_api\backend\WayfinderBackend;
use Psr\Log\LoggerInterface;

/**
 * Shared extraction plumbing for processors that index text pulled from files.
 *
 * Both attached-file discovery (#262's {@see FileExtraction}) and linked-file
 * discovery (#265's {@see LinkedFileExtraction}) resolve a set of files for an
 * item, record each file->item reference in the map, and run the same
 * cache/queue/extract loop with the same failure semantics. That loop and the
 * collaborators it needs live here so the two processors stay thin and the
 * behaviour stays identical.
 *
 * The inline vs queue extraction mode, the content-hash cache probe, and the
 * "one bad attachment never fails the batch" log-and-skip are all #262/#263
 * decisions carried verbatim into linked files: a linked document is just a
 * file the processor learned about from content rather than a reference field.
 *
 * The shared settings form and config defaults (#266) also live here: both
 * processors run the same indexability rules and extraction limits (#264), so
 * one {@see PluginFormInterface} form on this base configures both. search_api
 * only renders a processor's form when it implements PluginFormInterface.
 */
abstract class FileExtractionProcessorBase extends ProcessorPluginBase implements PluginFormInterface {

  /**
   * Constructs a file-extraction processor.
   *
   * @param array $configuration
   *   Plugin configuration.
   * @param string $plugin_id
   *   The plugin id.
   * @param array $plugin_definition
   *   The plugin definition.
   * @param \Drupal\Core\Entity\EntityTypeManagerInterface|null $entityTypeManager
   *   The entity type manager, for loading file entities.
   * @param \Psr\Log\LoggerInterface|null $logger
   *   The logger channel for extraction failures.
   * @param \Drupal\search_api_wayfinder\Cache\ExtractionCacheInterface|null $cache
   *   (optional) The extraction cache. When NULL, every file is extracted
   *   inline with no caching -- the #262 tracer behaviour, retained for tests
   *   that do not exercise the cache.
   * @param \Drupal\Core\Queue\QueueInterface|null $queue
   *   (optional) The extraction queue, used only in queue mode. When NULL,
   *   queue mode degrades to inline extraction.
   * @param \Drupal\search_api_wayfinder\FileReferenceMapInterface|null $fileMap
   *   (optional) The file->item reference map, populated during indexing so a
   *   file change/delete can reindex every referencing item (#263) and #265's
   *   linked files can record references with no entity-reference field. When
   *   NULL, no mapping is recorded.
   */
  public function __construct(
    array $configuration,
    $plugin_id,
    array $plugin_definition,
    protected ?EntityTypeManagerInterface $entityTypeManager = NULL,
    protected ?LoggerInterface $logger = NULL,
    protected ?ExtractionCacheInterface $cache = NULL,
    protected ?QueueInterface $queue = NULL,
    protected ?FileReferenceMapInterface $fileMap = NULL,
  ) {
    parent::__construct($configuration, $plugin_id, $plugin_definition);
  }

  /**
   * {@inheritdoc}
   *
   * The defaults make the processor useful with no configuration at all: every
   * extractable file is indexed, with only the obviously-textless media/image
   * extensions excluded (the #264 default list) and private files held back by
   * the safe access-control default. Each setting feeds an ExtractFileValidator
   * rule (#264); the wiring that consumes them in addFieldValues() is the next
   * slice (#264 follow-up), so this form stores exactly the values that change
   * will read.
   */
  public function defaultConfiguration(): array {
    return [
      // inline: extract during indexing (content-hash cached). queue: defer
      // uncached files to the wayfinder_extraction cron worker.
      'extraction_mode' => 'inline',
      // Space-separated; stored verbatim from the #264 constant so the validator
      // explodes it the same way it falls back when none are configured.
      'excluded_extensions' => ExtractFileValidator::DEFAULT_EXCLUDED_EXTENSIONS,
      // Byte string; '0' means no size restriction.
      'max_filesize' => '0',
      // TRUE excludes private:// files (the safe access-control default). A
      // site that accepts the per-item leakage opts in by unsetting this.
      'excluded_private' => TRUE,
      // 0 means index every file in a field, no per-field cap.
      'number_indexed' => 0,
      // Byte string; '0' means no extracted-length cap (a relevance/bloat
      // control distinct from the server-side extraction budget in #257).
      'number_first_bytes' => '0',
    ] + parent::defaultConfiguration();
  }

  /**
   * {@inheritdoc}
   *
   * One control per setting. Both attached-file (#262) and linked-file (#265)
   * discovery run the same indexability rules and extraction limits, so the
   * form lives on this shared base rather than on each subclass.
   */
  public function buildConfigurationForm(array $form, FormStateInterface $form_state): array {
    $config = $this->configuration;

    $form['extraction_mode'] = [
      '#type' => 'radios',
      '#title' => $this->t('Extraction mode'),
      '#options' => [
        'inline' => $this->t('Inline — extract during indexing (cached by content hash)'),
        'queue' => $this->t('Deferred — enqueue uncached files for cron via the wayfinder_extraction worker, so a slow parser never stalls an index batch'),
      ],
      '#description' => $this->t('A file referenced by many items is only extracted once regardless of mode (the cache is content-hash keyed).'),
      '#default_value' => $config['extraction_mode'] ?? 'inline',
    ];

    $form['excluded_extensions'] = [
      '#type' => 'textarea',
      '#title' => $this->t('Excluded file extensions'),
      '#description' => $this->t('Extensions whose contents are never extracted, one per line (e.g. media and image types that carry no text). The check is by MIME type, so variations sharing one MIME (jpg/jpeg, tif/tiff) need only one representative. Leave blank to extract every extension.'),
      '#default_value' => implode("\n", self::normalizeExcludedExtensions((string) ($config['excluded_extensions'] ?? ''))),
      '#rows' => 3,
    ];

    $form['max_filesize'] = [
      '#type' => 'textfield',
      '#title' => $this->t('Maximum file size'),
      '#description' => $this->t('A file larger than this is not extracted. Use a size understood by Drupal (e.g. <em>10 MB</em>); <em>0</em> for no limit.'),
      '#default_value' => $config['max_filesize'] ?? '0',
      '#size' => 12,
    ];

    $form['excluded_private'] = [
      '#type' => 'checkbox',
      '#title' => $this->t('Exclude private files'),
      '#description' => $this->t('Indexing a private file makes its contents searchable through the item that references it, because Search API access control is per item, not per attachment. Leave checked (the safe default) to exclude <em>private://</em> files; uncheck only if you accept that leakage.'),
      '#default_value' => (bool) ($config['excluded_private'] ?? TRUE),
    ];

    $form['number_indexed'] = [
      '#type' => 'number',
      '#title' => $this->t('Maximum files per field'),
      '#description' => $this->t('Index at most this many files from any one file field (first N by widget weight). <em>0</em> for no limit.'),
      '#min' => 0,
      '#default_value' => (int) ($config['number_indexed'] ?? 0),
    ];

    $form['number_first_bytes'] = [
      '#type' => 'textfield',
      '#title' => $this->t('Maximum extracted length'),
      '#description' => $this->t('Bound extracted text to its first N bytes before indexing (a Drupal-side relevance/bloat control, distinct from the server-side extraction budget). Use a size (e.g. <em>1 MB</em>); <em>0</em> for no limit.'),
      '#default_value' => $config['number_first_bytes'] ?? '0',
      '#size' => 12,
    ];

    return $form;
  }

  /**
   * {@inheritdoc}
   *
   * Byte-size fields (max_filesize, number_first_bytes) must parse: '0' and ''
   * mean "no restriction" and are always valid; anything else must pass
   * Drupal's own Drupal\Component\Utility\Bytes::validate(). We use that rather
   * than Bytes::toNumber(), which throws a TypeError on inputs like 'huge'
   * (a unit letter with no leading number) and would crash the form build.
   */
  public function validateConfigurationForm(array &$form, FormStateInterface $form_state): void {
    foreach (['max_filesize', 'number_first_bytes'] as $key) {
      $value = trim((string) $form_state->getValue($key));
      if ($value === '' || $value === '0') {
        continue;
      }
      if (!Bytes::validate($value)) {
        $form_state->setErrorByName(
          $key,
          $this->t('Enter a size such as <em>10 MB</em>, or <em>0</em> for no limit.'),
        );
      }
    }
  }

  /**
   * {@inheritdoc}
   *
   * excluded_extensions is normalised to the exact shape the validator's
   * getExcludedMimes() will explode(' ', ...): a space-separated list of
   * lowercase, dot-stripped, de-duplicated extensions. The other settings are
   * stored verbatim under their config keys.
   */
  public function submitConfigurationForm(array &$form, FormStateInterface $form_state): void {
    foreach (['extraction_mode', 'max_filesize', 'number_first_bytes'] as $key) {
      $this->configuration[$key] = $form_state->getValue($key);
    }
    $this->configuration['excluded_extensions'] = implode(' ', self::normalizeExcludedExtensions((string) $form_state->getValue('excluded_extensions')));
    $this->configuration['excluded_private'] = (bool) $form_state->getValue('excluded_private');
    $this->configuration['number_indexed'] = (int) $form_state->getValue('number_indexed');
  }

  /**
   * Splits a raw excluded-extensions box into a canonical list.
   *
   * Accepts any whitespace separation (the textarea's one-per-line, or a pasted
   * space-delimited blob), lowercases, strips a leading dot, and de-duplicates
   * -- so 'PNG\n.jpg\njpg' collapses to ['png', 'jpg']. Empty input yields [].
   *
   * @return string[]
   *   Unique, lowercase, dot-stripped extensions.
   */
  private static function normalizeExcludedExtensions(string $raw): array {
    $extensions = [];
    foreach (preg_split('/\s+/', trim($raw)) as $extension) {
      $extension = ltrim(strtolower($extension), '.');
      if ($extension !== '') {
        $extensions[$extension] = $extension;
      }
    }
    return array_values($extensions);
  }

  /**
   * Extracts text from a set of files for an item and records the references.,
   *
   * Shared by attached-file and linked-file discovery: each file is recorded in
   * the file->item map (guarded for the tracer/tests that inject no map) and
   * run through {@see extractOrGetFromCache()}, and the resulting text is
   * concatenated. Returning '' lets a processor skip populating its field.
   *
   * @param iterable<\Drupal\file\FileInterface> $files
   *   The files resolved for this item, in any order.
   * @param \Drupal\search_api\Item\ItemInterface $item
   *   The item being indexed.
   *
   * @return string
   *   The concatenated extracted text, or '' if nothing was extracted.
   */
  protected function extractAndRecord(iterable $files, ItemInterface $item): string {
    $extraction = '';
    foreach ($files as $file) {
      if ($this->fileMap !== NULL) {
        $this->fileMap->record($this->getIndex()->id(), (int) $file->id(), $item->getId());
      }
      $extraction .= $this->extractOrGetFromCache($file, $item);
    }
    return $extraction;
  }

  /**
   * Returns extracted text for a file, hitting the cache first.
   *
   * Issue #263: the cache is keyed by file content hash, so a file referenced
   * by many items is extracted once, and a changed file (new hash) naturally
   * misses and is re-extracted. On a cache miss in queue mode the file is
   * deferred to the wayfinder_extraction cron worker instead of stalling the
   * index batch, and '' is indexed for now; the worker caches the text and
   * marks the item for reindex so the next pass hits the cache.
   *
   * Extraction failure is logged and returns '' (decision from #262): one bad
   * attachment never fails the whole batch.
   *
   * @param \Drupal\file\FileInterface $file
   *   The file to extract.
   * @param \Drupal\search_api\Item\ItemInterface $item
   *   The item being indexed, used as queue-item context for later reindex.
   *
   * @return string
   *   The extracted text, or '' on a cache miss in queue mode or on failure.
   */
  protected function extractOrGetFromCache(FileInterface $file, ItemInterface $item): string {
    if ($this->cache !== NULL) {
      $cached = $this->cache->get($file);
      if ($cached !== NULL) {
        return $cached;
      }
    }

    if ($this->isQueueMode() && $this->queue !== NULL) {
      $this->queueItem($file, $item);
      return '';
    }

    try {
      $backend = $this->getBackend();
      $text = $backend !== NULL ? (string) $backend->extractContentFromFile($file->getFileUri()) : '';
    }
    catch (\Throwable $e) {
      // Decision: extraction failure must not fail the whole index batch. Log
      // and index the item without this attachment's text.
      $this->logger?->error('Failed to extract text from file @uri while indexing: @message', [
        '@uri' => $file->getFileUri(),
        '@message' => $e->getMessage(),
      ]);
      return '';
    }

    if ($text !== '' && $this->cache !== NULL) {
      $this->cache->set($file, $text);
    }
    return $text;
  }

  /**
   * Whether the processor is configured to defer extraction to the queue.
   */
  private function isQueueMode(): bool {
    return ($this->configuration['extraction_mode'] ?? 'inline') === 'queue';
  }

  /**
   * Enqueues one extraction job for a file referenced by an item.
   *
   * The worker (ExtractorQueue) reloads the file and index, extracts+caches,
   * and marks the item for reindex so its next index pass hits the cache. The
   * item id is the combined form (datasource:raw); the worker splits it for
   * trackItemsUpdated().
   */
  private function queueItem(FileInterface $file, ItemInterface $item): void {
    $this->queue->createItem([
      'file_id' => (int) $file->id(),
      'index_id' => $this->getIndex()->id(),
      'item_id' => $item->getId(),
    ]);
  }

  /**
   * Resolves the item's original entity, or NULL if it cannot be loaded.
   *
   * Mirrors search_api_attachments' SearchApiException guard around
   * getOriginalObject(): a deleted entity mid-index is a no-op, not a crash.
   */
  protected function getEntity(ItemInterface $item): ?EntityInterface {
    try {
      $entity = $item->getOriginalObject()->getValue();
    }
    catch (SearchApiException $e) {
      return NULL;
    }
    return $entity instanceof EntityInterface ? $entity : NULL;
  }

  /**
   * Resolves the index's Wayfinder backend, or NULL if the index is not in
   * fact served by one (supportsIndex() normally prevents this, but
   * addFieldValues() defends regardless).
   */
  protected function getBackend(): ?WayfinderBackend {
    $server = $this->getIndex()->getServerInstanceIfAvailable();
    $backend = $server?->getBackend();
    return $backend instanceof WayfinderBackend ? $backend : NULL;
  }

}
