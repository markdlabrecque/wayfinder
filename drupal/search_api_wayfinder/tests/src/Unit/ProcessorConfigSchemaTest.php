<?php

declare(strict_types=1);

namespace Drupal\Tests\search_api_wayfinder\Unit;

use Drupal\search_api_wayfinder\Plugin\search_api\processor\FileExtraction;
use Drupal\search_api_wayfinder\Plugin\search_api\processor\LinkedFileExtraction;
use PHPUnit\Framework\TestCase;
use Symfony\Component\Yaml\Yaml;

/**
 * Tests that the extraction processors' configuration has a config schema
 * (issue #266): "an unschemad processor config breaks config export tests."
 *
 * search_api stores processor config under the index config entity's
 * processor_settings.<plugin_id>, typed via
 * `plugin.plugin_configuration.search_api_processor.<plugin_id>`. A key the
 * processor stores but the schema does not declare fails Drupal's config
 * validation (SchemaCheckTrait / ConfigSchemaValidator) on export and import,
 * and trips the core config-test profile. This unit test is a hermetic proxy
 * for that kernel check: it parses the schema YAML directly and asserts every
 * defaultConfiguration() key is declared with the right type -- no Drupal
 * kernel, no database, no config-export round-trip needed.
 *
 * @group search_api_wayfinder
 */
class ProcessorConfigSchemaTest extends TestCase {

  /**
   * The parsed module schema, cached per test run.
   *
   * @var array<string, mixed>
   */
  private static array $schema;

  /**
   * The two processors whose config needs scheming, keyed by plugin id.
   *
   * @return array<string, \Drupal\search_api_wayfinder\Plugin\search_api\processor\FileExtractionProcessorBase>
   *   Both processors.
   */
  private function processors(): array {
    return [
      'wayfinder_file_extraction' => new FileExtraction(
        [],
        'wayfinder_file_extraction',
        ['id' => 'wayfinder_file_extraction', 'label' => 'x'],
      ),
      'wayfinder_linked_file_extraction' => new LinkedFileExtraction(
        [],
        'wayfinder_linked_file_extraction',
        ['id' => 'wayfinder_linked_file_extraction', 'label' => 'x'],
      ),
    ];
  }

  /**
   * Loads the module schema once.
   *
   * @return array<string, mixed>
   *   The parsed schema mapping.
   */
  private function schema(): array {
    if (!isset(self::$schema)) {
      $path = dirname(__DIR__, 3) . '/config/schema/search_api_wayfinder.schema.yml';
      self::$schema = Yaml::parseFile($path);
    }
    return self::$schema;
  }

  /**
   * Both processor plugin ids get an explicit schema entry that extends
   * search_api's default processor configuration (so `weights` is covered) and
   * is a mapping. Without this entry the processor falls through to the
   * catch-all `plugin.plugin_configuration.search_api_processor.*` which knows
   * nothing about our keys.
   */
  public function testSchemaDeclaresBothProcessorConfigurations(): void {
    foreach (['wayfinder_file_extraction', 'wayfinder_linked_file_extraction'] as $plugin_id) {
      $key = 'plugin.plugin_configuration.search_api_processor.' . $plugin_id;
      $this->assertArrayHasKey($key, $this->schema(), $plugin_id);

      $entry = $this->schema()[$key];
      // Must extend search_api's base so the inherited `weights` key keeps its
      // schema, and must declare its own mapping for the settings.
      $this->assertSame('search_api.default_processor_configuration', $entry['type'], $plugin_id);
      $this->assertArrayHasKey('mapping', $entry, $plugin_id);
    }
  }

  /**
   * Every key a processor can store (per defaultConfiguration()) is declared in
   * the schema with the right scalar type, so export/import round-trips without
   * an "incomplete schema" error. `weights` is inherited from the base type and
   * is intentionally NOT in the local mapping.
   *
   * This is the precise guard the issue names: if a future setting is added to
   * defaultConfiguration() but not the schema, this test fails immediately.
   */
  public function testSchemaCoversEveryDefaultConfigurationKeyWithTheRightType(): void {
    $expected_types = [
      'extraction_mode' => 'string',
      'excluded_extensions' => 'string',
      'max_filesize' => 'string',
      'excluded_private' => 'boolean',
      'number_indexed' => 'integer',
      'number_first_bytes' => 'string',
    ];

    foreach ($this->processors() as $plugin_id => $processor) {
      $mapping = $this->schema()['plugin.plugin_configuration.search_api_processor.' . $plugin_id]['mapping'] ?? NULL;
      $this->assertNotNull($mapping, "$plugin_id has no schema mapping");

      foreach ($processor->defaultConfiguration() as $key => $value) {
        // weights is inherited from search_api.default_processor_configuration;
        // it must NOT be re-declared locally.
        if ($key === 'weights') {
          $this->assertArrayNotHasKey('weights', $mapping, "$plugin_id re-declares inherited weights");
          continue;
        }
        $this->assertArrayHasKey($key, $mapping, "$plugin_id: config key '$key' has no schema");
        $this->assertSame(
          $expected_types[$key],
          $mapping[$key]['type'],
          "$plugin_id: '$key' schema type",
        );
      }
    }
  }

  /**
   * The defaultConfiguration() keys match across both processors (the form is
   * shared), and the excluded-extensions default is the #264 constant. A drift
   * between the two processors' defaults would mean one form silently stores
   * settings the other does not default.
   */
  public function testBothProcessorsShareIdenticalDefaults(): void {
    $processors = $this->processors();
    $file = $processors['wayfinder_file_extraction']->defaultConfiguration();
    $linked = $processors['wayfinder_linked_file_extraction']->defaultConfiguration();

    unset($file['weights'], $linked['weights']);
    $this->assertSame($file, $linked);

    $this->assertSame(
      \Drupal\search_api_wayfinder\ExtractFileValidator::DEFAULT_EXCLUDED_EXTENSIONS,
      $file['excluded_extensions'],
    );
  }

}
