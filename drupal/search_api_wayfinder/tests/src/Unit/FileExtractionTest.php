<?php

declare(strict_types=1);

namespace Drupal\Tests\search_api_wayfinder\Unit;

use Drupal\Core\Entity\ContentEntityInterface;
use Drupal\Core\Entity\EntityStorageInterface;
use Drupal\Core\Entity\EntityTypeManagerInterface;
use Drupal\Core\Field\FieldDefinitionInterface;
use Drupal\Core\Field\FieldItemListInterface;
use Drupal\Core\TypedData\ComplexDataInterface;
use Drupal\search_api\Datasource\DatasourceInterface;
use Drupal\search_api\IndexInterface;
use Drupal\search_api\Item\FieldInterface;
use Drupal\search_api\Item\ItemInterface;
use Drupal\search_api\Processor\ProcessorProperty;
use Drupal\search_api\SearchApiException;
use Drupal\search_api\ServerInterface;
use Drupal\search_api\Utility\FieldsHelperInterface;
use Drupal\search_api_wayfinder\Plugin\search_api\backend\WayfinderBackend;
use Drupal\search_api_wayfinder\Plugin\search_api\processor\FileExtraction;
use PHPUnit\Framework\TestCase;
use Psr\Log\LoggerInterface;

/**
 * Tests the FileExtraction processor (issue #262 tracer): the
 * supportsIndex() gate to WayfinderBackend indexes, the per-file-field
 * computed properties it declares, and addFieldValues() populating them by
 * calling WayfinderBackend::extractContentFromFile() -- including the
 * hard requirement that an extraction failure is logged and skipped rather
 * than failing the index batch.
 *
 * The /update/extract wire shape (multipart part name "file", extractOnly=true,
 * extractFormat=text) and the response envelope are WayfinderClientTest /
 * WayfinderBackendTest's concern; this test mocks WayfinderBackend so the
 * processor logic is exercised without HTTP.
 *
 * @coversDefaultClass \Drupal\search_api_wayfinder\Plugin\search_api\processor\FileExtraction
 * @group search_api_wayfinder
 */
class FileExtractionTest extends TestCase {

  /**
   * The processor under test. Rebuilt per test with that test's collaborators.
   */
  private function processor(?EntityTypeManagerInterface $entityTypeManager = NULL, ?LoggerInterface $logger = NULL): FileExtraction {
    $processor = new FileExtraction(
      [],
      'wayfinder_file_extraction',
      ['id' => 'wayfinder_file_extraction', 'label' => 'Wayfinder file extraction'],
      $entityTypeManager ?? $this->createMock(EntityTypeManagerInterface::class),
      $logger ?? $this->createMock(LoggerInterface::class),
    );
    // $this->t() in getPropertyDefinitions() resolves the string-translation
    // service lazily; without a bootstrapped Drupal that hits \Drupal::service,
    // so inject a no-op translation here. The label text is never asserted.
    $processor->setStringTranslation($this->createMock(\Drupal\Core\StringTranslation\TranslationInterface::class));
    return $processor;
  }

  /**
   * A datasource exposing one file field (field_files) and one non-file field
   * (title), so the "one computed property per file field" behaviour has
   * something to discriminate against.
   */
  private function datasourceWithFileField(): DatasourceInterface {
    $fileField = $this->createMock(FieldDefinitionInterface::class);
    $fileField->method('getType')->willReturn('file');
    $fileField->method('getName')->willReturn('field_files');
    $fileField->method('getLabel')->willReturn('Files');

    $titleField = $this->createMock(FieldDefinitionInterface::class);
    $titleField->method('getType')->willReturn('string');

    $datasource = $this->createMock(DatasourceInterface::class);
    $datasource->method('getPluginId')->willReturn('entity:node');
    $datasource->method('getPropertyDefinitions')->willReturn([
      'field_files' => $fileField,
      'title' => $titleField,
    ]);
    return $datasource;
  }

  /**
   * @covers ::supportsIndex
   */
  public function testSupportsIndexReturnsTrueForWayfinderBackend(): void {
    $server = $this->createMock(ServerInterface::class);
    $server->method('getBackendId')->willReturn('wayfinder');
    $index = $this->createMock(IndexInterface::class);
    $index->method('getServerInstanceIfAvailable')->willReturn($server);

    $this->assertTrue(FileExtraction::supportsIndex($index));
  }

  /**
   * @covers ::supportsIndex
   */
  public function testSupportsIndexReturnsFalseForOtherBackends(): void {
    $server = $this->createMock(ServerInterface::class);
    $server->method('getBackendId')->willReturn('search_api_solr');
    $index = $this->createMock(IndexInterface::class);
    $index->method('getServerInstanceIfAvailable')->willReturn($server);

    $this->assertFalse(FileExtraction::supportsIndex($index));
  }

  /**
   * An index without an available server (unsaved, or a server that could not
   * be loaded) must not crash the static gate -- it just opts out.
   *
   * @covers ::supportsIndex
   */
  public function testSupportsIndexReturnsFalseWhenNoServerAvailable(): void {
    $index = $this->createMock(IndexInterface::class);
    $index->method('getServerInstanceIfAvailable')->willReturn(NULL);

    $this->assertFalse(FileExtraction::supportsIndex($index));
  }

  /**
   * @covers ::getPropertyDefinitions
   */
  public function testGetPropertyDefinitionsDeclaresOnePropertyPerFileFieldWithSawPrefix(): void {
    $index = $this->createMock(IndexInterface::class);
    $index->method('getDatasources')->willReturn([$this->datasourceWithFileField()]);
    $processor = $this->processor();
    $processor->setIndex($index);

    $properties = $processor->getPropertyDefinitions(NULL);

    $this->assertArrayHasKey('saw_field_files', $properties);
    $this->assertInstanceOf(ProcessorProperty::class, $properties['saw_field_files']);
    // Only file fields get a property -- the title field must not.
    $this->assertArrayNotHasKey('saw_title', $properties);
  }

  /**
   * The saw_ prefix is the field-naming decision (#262 decision 1): it must
   * not collide with search_api_attachments' saa_ prefix if both modules are
   * installed on one site. Asserting the exact machine name locks that.
   *
   * @covers ::getPropertyDefinitions
   */
  public function testPropertyNamesUseSawPrefixNotSearchApiAttachmentsSaaPrefix(): void {
    $index = $this->createMock(IndexInterface::class);
    $index->method('getDatasources')->willReturn([$this->datasourceWithFileField()]);
    $processor = $this->processor();
    $processor->setIndex($index);

    $properties = $processor->getPropertyDefinitions(NULL);

    $this->assertSame(['saw_field_files'], array_keys($properties));
    $this->assertStringStartsWith('saw_', array_key_first($properties));
  }

  /**
   * Processor-defined properties are index-level (datasource NULL); asking for
   * a specific datasource yields nothing. This is the Search API convention
   * AggregatedFields and search_api_attachments both follow.
   *
   * @covers ::getPropertyDefinitions
   */
  public function testGetPropertyDefinitionsReturnsEmptyForASpecificDatasource(): void {
    $processor = $this->processor();

    $this->assertSame([], $processor->getPropertyDefinitions($this->createMock(DatasourceInterface::class)));
  }

  /**
   * @covers ::addFieldValues
   */
  public function testAddFieldValuesExtractsFileTextAndPopulatesTheField(): void {
    $entityTypeManager = $this->createMock(EntityTypeManagerInterface::class);
    $fileStorage = $this->createMock(EntityStorageInterface::class);
    // FileInterface is a Drupal module class that the hermetic unit sandbox
    // does not autoload (it loads via Drupal's runtime module classloader on a
    // real site). The processor only needs getFileUri(), so an anonymous
    // double satisfies the duck-typed call without that dependency.
    $fileStorage->method('loadMultiple')
      ->willReturn([7 => new class {
        public function getFileUri(): string {
          return '/tmp/sample.txt';
        }
      }]);
    $entityTypeManager->method('getStorage')->willReturn($fileStorage);

    $backend = $this->createMock(WayfinderBackend::class);
    $backend->expects($this->once())
      ->method('extractContentFromFile')
      ->with('/tmp/sample.txt')
      ->willReturn('extracted text');

    $indexField = $this->createMock(FieldInterface::class);
    $indexField->expects($this->once())->method('addValue')->with('extracted text');

    $fieldsHelper = $this->createMock(FieldsHelperInterface::class);
    $fieldsHelper->expects($this->once())
      ->method('filterForPropertyPath')
      ->with($this->anything(), NULL, 'saw_field_files')
      ->willReturn(['saw_field_files' => $indexField]);

    $processor = $this->processor($entityTypeManager);
    $processor->setFieldsHelper($fieldsHelper);
    $processor->setIndex($this->indexForExtraction($backend));

    $item = $this->itemWithFileField($indexField);

    $processor->addFieldValues($item);
  }

  /**
   * The hard requirement (#262): an extraction failure must be logged and
   * skipped -- the item still indexes, just without that attachment's text.
   * A single bad file can never fail the whole batch.
   *
   * @covers ::addFieldValues
   */
  public function testAddFieldValuesLogsAndSkipsOnExtractionFailureWithoutFailingTheBatch(): void {
    $entityTypeManager = $this->createMock(EntityTypeManagerInterface::class);
    $fileStorage = $this->createMock(EntityStorageInterface::class);
    $fileStorage->method('loadMultiple')
      ->willReturn([7 => new class {
        public function getFileUri(): string {
          return '/tmp/corrupt.pdf';
        }
      }]);
    $entityTypeManager->method('getStorage')->willReturn($fileStorage);

    $backend = $this->createMock(WayfinderBackend::class);
    $backend->method('extractContentFromFile')
      ->willThrowException(new SearchApiException('unsupported format'));

    // The failure is logged ...
    $logger = $this->createMock(LoggerInterface::class);
    $logger->expects($this->once())->method('error');

    // ... and the field is left untouched (no empty/garbage value added).
    $indexField = $this->createMock(FieldInterface::class);
    $indexField->expects($this->never())->method('addValue');

    $fieldsHelper = $this->createMock(FieldsHelperInterface::class);
    $fieldsHelper->method('filterForPropertyPath')
      ->willReturn(['saw_field_files' => $indexField]);

    $processor = $this->processor($entityTypeManager, $logger);
    $processor->setFieldsHelper($fieldsHelper);
    $processor->setIndex($this->indexForExtraction($backend));

    // No exception escapes -- that is the batch-safety guarantee.
    $processor->addFieldValues($this->itemWithFileField($indexField));

    $this->addToAssertionCount(1);
  }

  /**
   * A file field the admin never added to the index has no matching index
   * field (filterForPropertyPath returns []), so nothing is extracted -- no
   * wasted /update/extract calls for unconfigured fields.
   *
   * @covers ::addFieldValues
   */
  public function testAddFieldValuesExtractsNothingWhenNoIndexFieldReferencesTheProperty(): void {
    $backend = $this->createMock(WayfinderBackend::class);
    $backend->expects($this->never())->method('extractContentFromFile');

    $fieldsHelper = $this->createMock(FieldsHelperInterface::class);
    $fieldsHelper->method('filterForPropertyPath')->willReturn([]);

    $processor = $this->processor();
    $processor->setFieldsHelper($fieldsHelper);
    $processor->setIndex($this->indexForExtraction($backend));

    $processor->addFieldValues($this->itemWithFileField($this->createMock(FieldInterface::class)));
  }

  /**
   * If the underlying object can't be loaded (deleted entity, etc.)
   * addFieldValues no-ops rather than crashing -- mirroring
   * search_api_attachments' SearchApiException guard.
   *
   * @covers ::addFieldValues
   */
  public function testAddFieldValuesNoOpsWhenTheOriginalObjectCannotBeLoaded(): void {
    $backend = $this->createMock(WayfinderBackend::class);
    $backend->expects($this->never())->method('extractContentFromFile');

    $fieldsHelper = $this->createMock(FieldsHelperInterface::class);
    $fieldsHelper->method('filterForPropertyPath')->willReturn([]);

    $processor = $this->processor();
    $processor->setFieldsHelper($fieldsHelper);
    $processor->setIndex($this->indexForExtraction($backend));

    $item = $this->createMock(ItemInterface::class);
    $item->method('getOriginalObject')->willThrowException(new SearchApiException('Failed to load original object'));

    $processor->addFieldValues($item);

    $this->addToAssertionCount(1);
  }

  /**
   * Builds the index mock shared by the addFieldValues tests: it advertises
   * the field_files file field, points at $backend via its server, and
   * (harmlessly) serves whatever fields the item carries.
   */
  private function indexForExtraction(WayfinderBackend $backend): IndexInterface {
    $server = $this->createMock(ServerInterface::class);
    $server->method('getBackend')->willReturn($backend);

    $index = $this->createMock(IndexInterface::class);
    $index->method('getDatasources')->willReturn([$this->datasourceWithFileField()]);
    $index->method('getServerInstanceIfAvailable')->willReturn($server);
    return $index;
  }

  /**
   * An item whose original object is a node carrying one file (target_id 7)
   * in field_files. The index field that the processor will populate is passed
   * in so filterForPropertyPath can return it.
   */
  private function itemWithFileField(FieldInterface $indexField): ItemInterface {
    $fileList = $this->createMock(FieldItemListInterface::class);
    $fileList->method('getValue')->willReturn([['target_id' => 7]]);

    $node = $this->createMock(ContentEntityInterface::class);
    $node->method('hasField')->willReturnCallback(fn (string $name) => $name === 'field_files');
    $node->method('get')->willReturnCallback(fn (string $name) => $name === 'field_files' ? $fileList : NULL);

    $adapter = $this->createMock(ComplexDataInterface::class);
    $adapter->method('getValue')->willReturn($node);

    $item = $this->createMock(ItemInterface::class);
    $item->method('getOriginalObject')->willReturn($adapter);
    $item->method('getFields')->willReturn(['saw_field_files' => $indexField]);
    return $item;
  }

}
