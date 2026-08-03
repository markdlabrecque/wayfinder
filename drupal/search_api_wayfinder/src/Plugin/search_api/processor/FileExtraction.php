<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder\Plugin\search_api\processor;

use Drupal\Core\Entity\EntityInterface;
use Drupal\Core\Entity\EntityTypeManagerInterface;
use Drupal\Core\Field\FieldDefinitionInterface;
use Drupal\Core\StringTranslation\TranslatableMarkup;
use Drupal\search_api\Attribute\SearchApiProcessor;
use Drupal\search_api\Datasource\DatasourceInterface;
use Drupal\search_api\Item\ItemInterface;
use Drupal\search_api\Processor\ProcessorPluginBase;
use Drupal\search_api\Processor\ProcessorProperty;
use Drupal\search_api\SearchApiException;
use Drupal\search_api_wayfinder\Plugin\search_api\backend\WayfinderBackend;
use Psr\Log\LoggerInterface;
use Symfony\Component\DependencyInjection\ContainerInterface;

/**
 * Extracts text from file fields via Wayfinder's /update/extract endpoint and
 * exposes it as indexable fulltext content.
 *
 * The retained tracer bullet for issue #262 (epic #171): a thin vertical slice
 * that ports search_api_attachments' core behaviour into search_api_wayfinder
 * rather than depending on the contrib module. One file field on one entity
 * goes end to end -- declared computed property, populated in addFieldValues()
 * by a WayfinderClient multipart POST to /update/extract?extractOnly=true,
 * indexed, and found by a fulltext search.
 *
 * What this slice deliberately does NOT carry over from search_api_attachments
 * (each is a documented follow-up, not a gap):
 * - Media (entity_reference -> media -> file) and the entity:file datasource
 *   case. Only plain file-typed fields are discovered here.
 * - Extraction-result caching. Every reindex re-extracts; the Wayfinder
 *   server's own extraction budgets (#257) bound the work, but a persistent
 *   cache is the obvious next slice.
 * - The excluded-extensions / max-filesize / number-indexed configuration.
 * - A fallback queue for transient extraction failures.
 *
 * Field naming (decision 1, hard to change later): the `saw_` property prefix
 * (search_api_wayfinder) is distinct from search_api_attachments' `saa_`, so
 * both modules' properties can coexist without colliding machine names.
 * Running both modules against the same file fields would double-index
 * attachment text, so a site should pick one module -- this prefix makes that
 * a choice, not a silent clash.
 *
 * Derived from search_api_attachments' FilesExtractor (GPL-2.0-or-later, same
 * licence as this module): the per-file-field computed-property +
 * addFieldValues() shape and the file-field discovery loop are adapted from
 * its getPropertyDefinitions()/getFileFieldsAndFileEntityItems()/addFieldValues().
 * The extraction itself is delegated to WayfinderBackend::extractContentFromFile()
 * rather than an external text-extractor plugin, because Wayfinder extracts
 * in-process on the server.
 */
#[SearchApiProcessor(
  id: 'wayfinder_file_extraction',
  label: new TranslatableMarkup('Wayfinder file extraction'),
  description: new TranslatableMarkup('Extracts text from file fields via the Wayfinder /update/extract endpoint so attachments become searchable. Use this <em>instead of</em> the File attachments (search_api_attachments) processor for the same fields, not alongside it.'),
  stages: [
    'add_properties' => 0,
  ],
)]
class FileExtraction extends ProcessorPluginBase {

  /**
   * Prefix of the computed properties this processor declares.
   *
   * Distinct from search_api_attachments' `saa_` prefix (see class doc).
   */
  public const PREFIX = 'saw_';

  /**
   * @param array $configuration
   *   Plugin configuration.
   * @param string $plugin_id
   *   The plugin id.
   * @param array $plugin_definition
   *   The plugin definition.
   * @param \Drupal\Core\Entity\EntityTypeManagerInterface $entityTypeManager
   *   The entity type manager, for loading file entities from field values.
   * @param \Psr\Log\LoggerInterface $logger
   *   The logger channel for extraction failures.
   */
  public function __construct(
    array $configuration,
    $plugin_id,
    array $plugin_definition,
    protected ?EntityTypeManagerInterface $entityTypeManager = NULL,
    protected ?LoggerInterface $logger = NULL,
  ) {
    parent::__construct($configuration, $plugin_id, $plugin_definition);
  }

  /**
   * {@inheritdoc}
   */
  public static function create(ContainerInterface $container, array $configuration, $plugin_id, $plugin_definition) {
    return new static(
      $configuration,
      $plugin_id,
      $plugin_definition,
      $container->get('entity_type.manager'),
      $container->get('logger.factory')->get('search_api_wayfinder'),
    );
  }

  /**
   * {@inheritdoc}
   *
   * Gates the processor to indexes served by the Wayfinder backend, so it
   * never appears on a search_api_solr or database-backend index.
   */
  public static function supportsIndex(\Drupal\search_api\IndexInterface $index): bool {
    $server = $index->getServerInstanceIfAvailable();
    return $server !== NULL && $server->getBackendId() === 'wayfinder';
  }

  /**
   * {@inheritdoc}
   */
  public function getPropertyDefinitions(?DatasourceInterface $datasource = NULL): array {
    $properties = [];
    if ($datasource === NULL) {
      foreach ($this->getFileFields() as $field_name => $label) {
        $properties[static::PREFIX . $field_name] = new ProcessorProperty([
          'label' => $this->t('Wayfinder file extraction: @label', ['@label' => $label]),
          'description' => $this->t('Text extracted from files in the @label field by the Wayfinder /update/extract endpoint.', ['@label' => $label]),
          // Declared as 'string' to match search_api_attachments; an admin
          // adds it to the index as 'text' for fulltext search (decision 2:
          // extracted text lands in its own field with independent boost, not
          // appended to the body).
          'type' => 'string',
          'processor_id' => $this->getPluginId(),
        ]);
      }
    }
    return $properties;
  }

  /**
   * {@inheritdoc}
   */
  public function addFieldValues(ItemInterface $item): void {
    $entity = $this->getEntity($item);
    if ($entity === NULL) {
      return;
    }
    $backend = $this->getBackend();
    if ($backend === NULL) {
      return;
    }

    foreach ($this->getFileFields() as $field_name => $label) {
      $property_path = static::PREFIX . $field_name;
      foreach ($this->getFieldsHelper()->filterForPropertyPath($item->getFields(), NULL, $property_path) as $field) {
        if (!$entity->hasField($field_name)) {
          continue;
        }
        $target_ids = array_filter(array_column($entity->get($field_name)->getValue(), 'target_id'));
        if (empty($target_ids)) {
          continue;
        }
        $files = $this->entityTypeManager->getStorage('file')->loadMultiple($target_ids);

        $extraction = '';
        foreach ($files as $file) {
          // @var \Drupal\file\FileInterface $file
          // FileInterface is duck-typed here (getFileUri()); a real site loads
          // File entities, which satisfy it.
          try {
            $extraction .= $backend->extractContentFromFile($file->getFileUri());
          }
          catch (\Throwable $e) {
            // Decision: extraction failure must not fail the whole index
            // batch. Log and index the item without this attachment's text.
            $this->logger?->error('Failed to extract text from file in field @field on @type @id while indexing @index: @message', [
              '@field' => $field_name,
              '@type' => $entity->getEntityTypeId(),
              '@id' => $entity->id(),
              '@index' => $this->getIndex()->id(),
              '@message' => $e->getMessage(),
            ]);
          }
        }

        if ($extraction !== '') {
          $field->addValue($extraction);
        }
      }
    }
  }

  /**
   * Resolves the item's original entity, or NULL if it cannot be loaded.
   *
   * Mirrors search_api_attachments' SearchApiException guard around
   * getOriginalObject(): a deleted entity mid-index is a no-op, not a crash.
   */
  private function getEntity(ItemInterface $item): ?EntityInterface {
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
  private function getBackend(): ?WayfinderBackend {
    $server = $this->getIndex()->getServerInstanceIfAvailable();
    $backend = $server?->getBackend();
    return $backend instanceof WayfinderBackend ? $backend : NULL;
  }

  /**
   * Discovers the file-typed fields available on the index's datasources.
   *
   * Simplified port of search_api_attachments'
   * getFileFieldsAndFileEntityItems(): file fields only (the media /
   * entity_reference -> media and the entity:file datasource cases are later
   * slices, not this tracer). Returns field machine name => human label.
   *
   * @return array<string, \Drupal\Core\StringTranslation\TranslatableMarkup|string>
   *   The file fields, keyed by machine name.
   */
  protected function getFileFields(): array {
    $file_fields = [];
    foreach ($this->getIndex()->getDatasources() as $datasource) {
      foreach ($datasource->getPropertyDefinitions() as $property) {
        if ($property instanceof FieldDefinitionInterface && $property->getType() === 'file') {
          $file_fields[$property->getName()] = $property->getLabel();
        }
      }
    }
    return $file_fields;
  }

}
