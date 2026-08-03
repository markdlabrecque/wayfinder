<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder\Plugin\search_api\processor;

use Drupal\Core\Entity\EntityTypeManagerInterface;
use Drupal\Core\Field\FieldDefinitionInterface;
use Drupal\Core\Queue\QueueInterface;
use Drupal\Core\StringTranslation\TranslatableMarkup;
use Drupal\file\FileInterface;
use Drupal\search_api\Attribute\SearchApiProcessor;
use Drupal\search_api\Datasource\DatasourceInterface;
use Drupal\search_api\Item\ItemInterface;
use Drupal\search_api\Processor\ProcessorProperty;
use Drupal\search_api_wayfinder\Cache\ExtractionCacheInterface;
use Drupal\search_api_wayfinder\FileReferenceMapInterface;
use Drupal\search_api_wayfinder\LinkedFileDiscovererInterface;
use Psr\Log\LoggerInterface;
use Symfony\Component\DependencyInjection\ContainerInterface;

/**
 * Extracts text from files *linked* from content and exposes it as indexable
 * fulltext content.
 *
 * Issue #265: the gap that motivated porting search_api_attachments into
 * search_api_wayfinder at all. A link to a downloadable document can appear
 * anywhere in a node -- body text, a link field, a paragraph, a Layout Builder
 * block -- and its extracted text should be indexed as part of that node.
 * {@see FileExtraction} (#262) handles files in file/image fields; this
 * processor handles files the content only references by link or embed,
 * discovered by {@see LinkedFileDiscovererInterface}.
 *
 * All extracted text lands in ONE aggregate field, {@see LINKED_PROPERTY}, that
 * an admin adds once and gives a lower boost than attached-file or body text.
 * The issue's fan-out hard part is one document linked from many items: the
 * content-hash cache (#263) extracts it once regardless, and a single
 * lower-boost field keeps fifty copies of one PDF from dominating relevance.
 *
 * Scan scope is configured text and link fields, NOT the fully rendered node:
 * a full view mode drags in menus, footers, and related-content teasers, so
 * every node would index the same site-wide PDFs. The raw field value is
 * parsed with Html::load() (never regex), and the link/embed resolvers live in
 * the discoverer so they are unit-testable per mechanism.
 *
 * Depth is capped at one hop: a discovered file's own content is never parsed
 * for further links. External links are off by construction -- only managed
 * files resolve, and nothing is fetched, so there is no SSRF surface.
 *
 * The cache/queue/extract loop and the file->item map recording are shared
 * with attached-file extraction through {@see FileExtractionProcessorBase};
 * this class owns only the scan of text/link fields and the aggregate field.
 */
#[SearchApiProcessor(
  id: 'wayfinder_linked_file_extraction',
  label: new TranslatableMarkup('Wayfinder linked file extraction'),
  description: new TranslatableMarkup('Extracts text from files <em>linked</em> from content (body links, embeds, link fields) via the Wayfinder /update/extract endpoint, not just attached files. Use alongside Wayfinder file extraction.'),
  stages: [
    'add_properties' => 0,
  ],
)]
class LinkedFileExtraction extends FileExtractionProcessorBase {

  /**
   * The single aggregate property holding every item's linked-document text.
   *
   * Distinct from {@see FileExtraction::PREFIX} per-file-field properties: one
   * field for all linked documents, for a lower boost (fan-out isolation).
   */
  public const LINKED_PROPERTY = 'saw_linked';

  /**
   * Text field types that carry formatted HTML and are scanned for links.
   *
   * Plain string/string_long fields are excluded: they hold no markup, so an
   * anchor or embed in one is the exception, and Html::load() on plain text
   * would only ever match a literal <a> tag.
   */
  private const TEXT_FIELD_TYPES = ['text', 'text_long', 'text_with_summary'];

  /**
   * The link field type (provided by core's link module).
   */
  private const LINK_FIELD_TYPE = 'link';

  /**
   * @param array $configuration
   *   Plugin configuration.
   * @param string $plugin_id
   *   The plugin id.
   * @param array $plugin_definition
   *   The plugin definition.
   * @param \Drupal\search_api_wayfinder\LinkedFileDiscovererInterface|null $discoverer
   *   The linked-file discoverer. When NULL (tests), no files are resolved.
   * @param \Drupal\Core\Entity\EntityTypeManagerInterface|null $entityTypeManager
   *   The entity type manager.
   * @param \Psr\Log\LoggerInterface|null $logger
   *   The logger channel for extraction failures.
   * @param \Drupal\search_api_wayfinder\Cache\ExtractionCacheInterface|null $cache
   *   (optional) The extraction cache.
   * @param \Drupal\Core\Queue\QueueInterface|null $queue
   *   (optional) The extraction queue, used only in queue mode.
   * @param \Drupal\search_api_wayfinder\FileReferenceMapInterface|null $fileMap
   *   (optional) The file->item reference map, so a changed linked file can
   *   reindex every referencing item (#263 invalidation).
   */
  public function __construct(
    array $configuration,
    $plugin_id,
    array $plugin_definition,
    protected ?LinkedFileDiscovererInterface $discoverer = NULL,
    ?EntityTypeManagerInterface $entityTypeManager = NULL,
    ?LoggerInterface $logger = NULL,
    ?ExtractionCacheInterface $cache = NULL,
    ?QueueInterface $queue = NULL,
    ?FileReferenceMapInterface $fileMap = NULL,
  ) {
    parent::__construct($configuration, $plugin_id, $plugin_definition, $entityTypeManager, $logger, $cache, $queue, $fileMap);
  }

  /**
   * {@inheritdoc}
   */
  public static function create(ContainerInterface $container, array $configuration, $plugin_id, $plugin_definition) {
    return new static(
      $configuration,
      $plugin_id,
      $plugin_definition,
      $container->get('search_api_wayfinder.linked_file_discoverer'),
      $container->get('entity_type.manager'),
      $container->get('logger.factory')->get('search_api_wayfinder'),
      $container->get('search_api_wayfinder.extraction_cache'),
      $container->get('queue')->get('wayfinder_extraction'),
      $container->get('search_api_wayfinder.file_reference_map'),
    );
  }

  /**
   * {@inheritdoc}
   *
   * Same Wayfinder-backend gate as FileExtraction: the processor only makes
   * sense on an index served by the Wayfinder backend.
   */
  public static function supportsIndex(\Drupal\search_api\IndexInterface $index): bool {
    $server = $index->getServerInstanceIfAvailable();
    return $server !== NULL && $server->getBackendId() === 'wayfinder';
  }

  /**
   * {@inheritdoc}
   *
   * Declares the single aggregate {@see LINKED_PROPERTY} regardless of the
   * index's fields: it is always available for an admin to add once, and the
   * scan in addFieldValues() skips cleanly when no text/link fields exist.
   */
  public function getPropertyDefinitions(?DatasourceInterface $datasource = NULL): array {
    if ($datasource !== NULL) {
      return [];
    }
    return [
      static::LINKED_PROPERTY => new ProcessorProperty([
        'label' => $this->t('Wayfinder linked file extraction'),
        'description' => $this->t('Text extracted from files linked or embedded in this item\'s content by the Wayfinder /update/extract endpoint. Add as a lower-boost fulltext field: one document linked from many items fans out across them.'),
        // Declared 'string'; an admin adds it to the index as 'text'. Lower
        // than attached-file/body boost to bound the fan-out relevance hit.
        'type' => 'string',
        'processor_id' => $this->getPluginId(),
      ]),
    ];
  }

  /**
   * {@inheritdoc}
   */
  public function addFieldValues(ItemInterface $item): void {
    // The admin-added field is the gate: skip all discovery work when saw_linked
    // is not on the item, so an index that does not use the property pays
    // nothing -- same shape as FileExtraction.
    $linked_fields = $this->getFieldsHelper()->filterForPropertyPath($item->getFields(), NULL, static::LINKED_PROPERTY);
    if ($linked_fields === []) {
      return;
    }

    $entity = $this->getEntity($item);
    if ($entity === NULL) {
      return;
    }

    $files = $this->discoverLinkedFiles($entity);
    if ($files === []) {
      return;
    }

    $extraction = $this->extractAndRecord($files, $item);
    if ($extraction === '') {
      return;
    }

    foreach ($linked_fields as $field) {
      $field->addValue($extraction);
    }
  }

  /**
   * Discovers every distinct file linked or embedded in an entity's scannable
   * text and link fields.
   *
   * @param \Drupal\Core\Entity\ContentEntityInterface $entity
   *   The item's original entity.
   *
   * @return array<int, \Drupal\file\FileInterface>
   *   Distinct files keyed by id.
   */
  protected function discoverLinkedFiles(\Drupal\Core\Entity\EntityInterface $entity): array {
    if ($this->discoverer === NULL) {
      return [];
    }

    $found = [];
    foreach ($this->getScannableFields() as $field_name => $kind) {
      if (!$entity->hasField($field_name)) {
        continue;
      }
      foreach ($entity->get($field_name)->getValue() as $value) {
        $resolved = $kind === self::LINK_FIELD_TYPE
          ? $this->discoverer->discoverFromLinkUri((string) ($value['uri'] ?? ''))
          : $this->discoverer->discoverFromHtml((string) ($value['value'] ?? ''));
        foreach ($resolved as $id => $file) {
          $found[$id] = $file;
        }
      }
    }
    return $found;
  }

  /**
   * Discovers the configured text and link fields on the index's datasources.
   *
   * @return array<string, string>
   *   Field machine name => 'text' or 'link', the kind controlling which
   *   discoverer method its value is fed to.
   */
  protected function getScannableFields(): array {
    $scannable = [];
    foreach ($this->getIndex()->getDatasources() as $datasource) {
      foreach ($datasource->getPropertyDefinitions() as $name => $property) {
        if (!$property instanceof FieldDefinitionInterface) {
          continue;
        }
        $type = $property->getType();
        if (in_array($type, self::TEXT_FIELD_TYPES, TRUE)) {
          $scannable[$name] = 'text';
        }
        elseif ($type === self::LINK_FIELD_TYPE) {
          $scannable[$name] = 'link';
        }
      }
    }
    return $scannable;
  }

}
