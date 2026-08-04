<?php

declare(strict_types=1);

namespace Drupal\Tests\search_api_wayfinder\Unit;

use Drupal\Core\Field\FieldDefinitionInterface;
use Drupal\Core\Field\FieldStorageDefinitionInterface;
use Drupal\Core\Form\FormState;
use Drupal\Core\Language\LanguageInterface;
use Drupal\Core\Language\LanguageManagerInterface;
use Drupal\Core\Messenger\MessengerInterface;
use Drupal\Core\StringTranslation\TranslatableMarkup;
use Drupal\Core\StringTranslation\TranslationInterface;
use Drupal\search_api\IndexInterface;
use Drupal\search_api\Item\FieldInterface;
use Drupal\search_api\Query\ConditionGroup;
use Drupal\search_api\Query\QueryInterface;
use Drupal\search_api\Query\ResultSet;
use Drupal\search_api\SearchApiException;
use Drupal\search_api\Utility\FieldsHelper;
use Drupal\search_api_autocomplete\SearchInterface;
use Drupal\search_api_autocomplete\Suggestion\SuggestionInterface;
use Drupal\search_api_wayfinder\Plugin\search_api\backend\WayfinderBackend;
use Drupal\search_api_wayfinder\WayfinderClient;
use GuzzleHttp\Client;
use GuzzleHttp\Exception\ConnectException;
use GuzzleHttp\Handler\MockHandler;
use GuzzleHttp\HandlerStack;
use GuzzleHttp\Psr7\Request;
use GuzzleHttp\Psr7\Response;
use PHPUnit\Framework\TestCase;
use Symfony\Component\DependencyInjection\ContainerInterface;

/**
 * Tests WayfinderBackend's feature flags, the /select vs /mlt routing
 * decision in search() (M4, issue #78), and viewSettings() (M5, issue #79).
 *
 * getClient() builds a real WayfinderClient from Guzzle config, so the
 * search()/routing tests use a minimal subclass that substitutes a mocked
 * WayfinderClient instead of touching HTTP at all -- WayfinderBackend's own
 * HTTP behaviour is WayfinderClientTest's job, not this class's. The
 * viewSettings() tests below instead build a real backend via its DI
 * factory with a mocked http_client, since viewSettings() calls
 * {core}/admin/system directly through getClient()'s Guzzle client.
 *
 * The version string's location in the admin/system response
 * (lucene.solr-spec-version = "9.10.1") is ground truth from
 * solr-ref/responses/admin_system.json -- read the fixture, per the plan
 * doc's "Premises to verify before implementing" item (c). Wayfinder itself
 * now emits the renamed sibling key lucene.wayfinder-spec-version (#325), so
 * the fixture-derived mock bodies below patch that one key name before use;
 * the fixture is left untouched as ground truth for real Solr's shape.
 *
 * @coversDefaultClass \Drupal\search_api_wayfinder\Plugin\search_api\backend\WayfinderBackend
 * @group search_api_wayfinder
 */
class WayfinderBackendTest extends TestCase {

  /**
   * @covers ::getSupportedFeatures
   */
  public function testGetSupportedFeaturesIncludesSearchApiFacets(): void {
    $backend = new WayfinderBackend([], 'wayfinder', []);

    $this->assertContains('search_api_facets', $backend->getSupportedFeatures());
  }

  /**
   * #298: OR facets are advertised now that the server serves {!ex}/{!tag}
   * (#295) and QueryBuilder emits them. Without this the facets module keeps
   * every facet AND-filtered against the full fq set.
   *
   * @covers ::getSupportedFeatures
   */
  public function testGetSupportedFeaturesIncludesSearchApiFacetsOperatorOr(): void {
    $backend = new WayfinderBackend([], 'wayfinder', []);

    $this->assertContains('search_api_facets_operator_or', $backend->getSupportedFeatures());
  }

  /**
   * A WayfinderBackend whose getClient() returns an injected mock instead of
   * constructing a real Guzzle-backed WayfinderClient.
   *
   * @param array $configuration
   *   Extra plugin configuration, merged over defaultConfiguration() by
   *   ConfigurablePluginBase's constructor -- used by the highlighting tests
   *   below to set the backend-level "highlight" flag (see the correction
   *   note on QueryBuilderTest's hl tests: this is a config setting, not a
   *   query option, per search_api_solr's own convention).
   */
  private function backendWithClient(WayfinderClient $client, array $configuration = []): WayfinderBackend {
    return new class($configuration, 'wayfinder', [], $client) extends WayfinderBackend {
      private WayfinderClient $testClient;

      public function __construct(array $configuration, $plugin_id, array $plugin_definition, WayfinderClient $client) {
        parent::__construct($configuration, $plugin_id, $plugin_definition);
        $this->testClient = $client;
      }

      protected function getClient(): WayfinderClient {
        return $this->testClient;
      }
    };
  }

  /**
   * @covers ::defaultConfiguration
   */
  public function testDefaultConfigurationHasEmptyTopLevelCredentials(): void {
    $backend = new WayfinderBackend([], 'wayfinder', []);

    $defaults = $backend->defaultConfiguration();
    $this->assertArrayHasKey('username', $defaults);
    $this->assertArrayHasKey('password', $defaults);
    $this->assertSame('', $defaults['username']);
    $this->assertSame('', $defaults['password']);
  }

  /**
   * search_api_solr's BasicAuthTrait keeps the stored password in form state
   * rather than rendering it back into the HTML form.
   *
   * @covers ::buildConfigurationForm
   */
  public function testCredentialFormUsesAPasswordInputWithoutPlaintextDefault(): void {
    $backend = $this->createBackend(new Response(200, []), [
      'username' => 'alice',
      'password' => 'stored-secret',
    ]);
    $formState = new FormState();

    $form = $backend->buildConfigurationForm([], $formState);

    $this->assertArrayHasKey('username', $form);
    $this->assertSame('textfield', $form['username']['#type']);
    $this->assertArrayHasKey('password', $form);
    $this->assertSame('password', $form['password']['#type']);
    $this->assertArrayNotHasKey('#default_value', $form['password']);
    $this->assertSame('stored-secret', $formState->get('password'));
  }

  /**
   * An empty password field means "leave it unchanged" only when the
   * username still names the same account; this preserves an existing secret
   * without exposing it through #default_value.
   *
   * @covers ::validateConfigurationForm
   */
  public function testBlankPasswordPreservesStoredPasswordWhenUsernameIsUnchanged(): void {
    $backend = $this->createBackend(new Response(200, []), [
      'username' => 'alice',
      'password' => 'stored-secret',
    ]);
    $formState = (new FormState())->set('password', 'stored-secret')->setValues([
      'username' => 'alice',
      'password' => '',
    ]);
    $form = [
      'username' => ['#name' => 'username'],
      'password' => ['#name' => 'password'],
    ];

    $backend->validateConfigurationForm($form, $formState);

    $this->assertSame([], $formState->getErrors());
    $this->assertSame('stored-secret', $formState->getValue('password'));
  }

  /**
   * @dataProvider credentialValidationProvider
   * @covers ::validateConfigurationForm
   */
  public function testCredentialValidationRejectsUnsafeOrIncompletePairs(string $username, string $password, bool $valid): void {
    $backend = $this->createBackend(new Response(200, []));
    $formState = (new FormState())->setValues([
      'username' => $username,
      'password' => $password,
    ]);
    $form = [
      'username' => ['#name' => 'username'],
      'password' => ['#name' => 'password'],
    ];

    $backend->validateConfigurationForm($form, $formState);

    if ($valid) {
      $this->assertSame([], $formState->getErrors());
    }
    else {
      $this->assertNotSame([], $formState->getErrors());
    }
  }

  /**
   * @return array<string, array{0: string, 1: string, 2: bool}>
   */
  public static function credentialValidationProvider(): array {
    return [
      'username contains colon' => ['alice:admin', 's3cr3t', FALSE],
      'username contains ASCII control' => ["ali\tce", 's3cr3t', FALSE],
      'password contains ASCII control' => ['alice', "s3cr3t\n", FALSE],
      'username without password' => ['alice', '', FALSE],
      'password without username' => ['', 's3cr3t', FALSE],
      'valid non-ASCII credentials' => ['álïçé', 'sëcrêt', TRUE],
    ];
  }

  private function mockIndex(string $indexId = 'my_index'): IndexInterface {
    $field = $this->createMock(FieldInterface::class);
    $field->method('getFieldIdentifier')->willReturn('title');
    $field->method('getType')->willReturn('text');
    $field->method('getPropertyPath')->willReturn('title');
    $field->method('getDatasourceId')->willReturn('entity:test');

    $storage = $this->createMock(FieldStorageDefinitionInterface::class);
    $storage->method('getCardinality')->willReturn(1);
    $definition = $this->createMock(FieldDefinitionInterface::class);
    $definition->method('isList')->willReturn(TRUE);
    $definition->method('getFieldStorageDefinition')->willReturn($storage);

    $index = $this->createMock(IndexInterface::class);
    $index->method('id')->willReturn($indexId);
    $index->method('getFulltextFields')->willReturn(['title']);
    $index->method('getField')->willReturnCallback(
      fn (string $id) => $id === 'title' ? $field : NULL
    );
    $index->method('getFields')->willReturn(['title' => $field]);
    $index->method('getPropertyDefinitions')->willReturn(['title' => $definition]);
    $field->method('getIndex')->willReturn($index);

    return $index;
  }

  private function mockQuery(IndexInterface $index, array $options = []): QueryInterface {
    $query = $this->createMock(QueryInterface::class);
    $query->method('getKeys')->willReturn(NULL);
    $query->method('getFulltextFields')->willReturn(NULL);
    $query->method('getIndex')->willReturn($index);
    $query->method('getConditionGroup')->willReturn(new ConditionGroup());
    $query->method('getSorts')->willReturn([]);
    $query->method('getOption')->willReturnCallback(
      static fn (string $name, $default = NULL) => $options[$name] ?? $default
    );
    $resultSet = new ResultSet($query);
    $query->method('getResults')->willReturn($resultSet);

    return $query;
  }

  /**
   * getSupportedFeatures() currently returns ['search_api_facets'] (M3);
   * M4 (plan doc line 124 / issue #78) adds "search_api_mlt". Deliberately
   * not asserting the array's exact shape -- only that MLT support is now
   * advertised.
   *
   * @covers ::getSupportedFeatures
   */
  public function testGetSupportedFeaturesIncludesSearchApiMlt(): void {
    $backend = new WayfinderBackend([], 'wayfinder', []);

    $this->assertContains('search_api_mlt', $backend->getSupportedFeatures());
  }

  /**
   * Result grouping (issue #290): the backend advertises search_api_grouping
   * so Search API enables its grouping feature for this backend.
   *
   * @covers ::getSupportedFeatures
   */
  public function testGetSupportedFeaturesIncludesSearchApiGrouping(): void {
    $backend = new WayfinderBackend([], 'wayfinder', []);

    $this->assertContains('search_api_grouping', $backend->getSupportedFeatures());
  }

  /**
   * #291: the backend advertises 'search_api_autocomplete' so the
   * search_api_autocomplete Server suggester activates this backend
   * (Server::getBackend() gates on supportsFeature('search_api_autocomplete'),
   * OR-ed with the instanceof checks -- finding 155). The method itself is
   * duck-typed, matching search_api_solr, which does NOT formally implement
   * the (documentation-only) AutocompleteBackendInterface.
   *
   * @covers ::getSupportedFeatures
   */
  public function testGetSupportedFeaturesIncludesSearchApiAutocomplete(): void {
    $backend = new WayfinderBackend([], 'wayfinder', []);

    $this->assertContains('search_api_autocomplete', $backend->getSupportedFeatures());
  }

  /**
   * issue #342: getSupportedFeatures() adds 'search_api_spellcheck', matching
   * search_api_solr's own advertised feature set
   * (SearchApiSolrBackend.php:777).
   *
   * @covers ::getSupportedFeatures
   */
  public function testGetSupportedFeaturesIncludesSearchApiSpellcheck(): void {
    $backend = new WayfinderBackend([], 'wayfinder', []);

    $this->assertContains('search_api_spellcheck', $backend->getSupportedFeatures());
  }

  /**
   * issue #300: supportsDataType() accepts the six default Search API types
   * plus the five search_api_solr non-default types that round-trip through
   * FieldMapper + presets/search-api.toml on Wayfinder's existing schema
   * types, plus solr_text_suggester (fixed sink field 'twm_suggest'). The
   * still-descoped types return FALSE so Search API surfaces them as
   * unsupported at config time rather than dropping them silently -- the
   * exact silent-loss bug #300 exists to fix.
   *
   * The accepted/rejected split is itself the spec: anything accepted must
   * have a FieldMapper prefix mapping AND a preset dynamic/static field, or
   * Drupal would accept the field at config time and fail at index time
   * (worse than refusing). See README "Not supported" for the per-type
   * descope reasons.
   *
   * @covers ::supportsDataType
   * @dataProvider supportsDataTypeProvider
   */
  public function testSupportsDataType(string $type, bool $expected): void {
    $backend = new WayfinderBackend([], 'wayfinder', []);

    $this->assertSame($expected, $backend->supportsDataType($type));
  }

  public static function supportsDataTypeProvider(): array {
    return [
      // Six defaults.
      'text' => ['text', TRUE],
      'string' => ['string', TRUE],
      'integer' => ['integer', TRUE],
      'decimal' => ['decimal', TRUE],
      'date' => ['date', TRUE],
      'boolean' => ['boolean', TRUE],
      // issue #300: newly supported search_api_solr types.
      'solr_string_storage' => ['solr_string_storage', TRUE],
      'solr_string_docvalues' => ['solr_string_docvalues', TRUE],
      'solr_text_unstemmed' => ['solr_text_unstemmed', TRUE],
      'solr_text_omit_norms' => ['solr_text_omit_norms', TRUE],
      'solr_text_wstoken' => ['solr_text_wstoken', TRUE],
      'solr_text_suggester' => ['solr_text_suggester', TRUE],
      // issue #342: solr_text_spellcheck is no longer descoped now that
      // FieldMapper is language-aware and maps it to the spellcheck_<lang>
      // fixed sink (SearchApiSolrBackend.php:2440-2446).
      'solr_text_spellcheck maps to the spellcheck_<lang> sink' => ['solr_text_spellcheck', TRUE],
      // Descoped types: refused, with a README reason.
      'solr_date_range needs a server-side range type' => ['solr_date_range', FALSE],
      'solr_text_custom is a site escape hatch' => ['solr_text_custom', FALSE],
      'solr_text_custom_omit_norms is a site escape hatch' => ['solr_text_custom_omit_norms', FALSE],
      // Spatial types belong to #292, not here.
      'location is spatial (#292)' => ['location', FALSE],
      'rpt is spatial (#292)' => ['rpt', FALSE],
      // An unknown type is refused (no fallback to accepting everything).
      'unknown type' => ['totally_made_up', FALSE],
    ];
  }

  /**
   * Plan doc line 170-171: the "search_api_mlt" query option routes to
   * GET /mlt instead of /select.
   *
   * @covers ::search
   */
  public function testSearchRoutesToMltEndpointWhenMltOptionIsSet(): void {
    $index = $this->mockIndex();
    $query = $this->mockQuery($index, [
      'search_api_mlt' => ['id' => 'entity:node/1:en', 'fields' => ['title']],
    ]);

    $client = $this->createMock(WayfinderClient::class);
    $client->expects($this->never())->method('select');
    $client->expects($this->once())
      ->method('mlt')
      ->with($this->callback(fn (array $params) => $params['q'] === 'id:"my_index-entity:node/1:en"'))
      ->willReturn(['response' => ['numFound' => 0, 'docs' => []]]);

    $backend = $this->backendWithClient($client);
    $backend->search($query);
  }

  /**
   * Negative case: without the "search_api_mlt" option, search() keeps using
   * /select -- MLT routing must not leak into ordinary fulltext searches.
   *
   * @covers ::search
   */
  public function testSearchRoutesToSelectEndpointWhenMltOptionIsAbsent(): void {
    $index = $this->mockIndex();
    $query = $this->mockQuery($index);

    $client = $this->createMock(WayfinderClient::class);
    $client->expects($this->never())->method('mlt');
    $client->expects($this->once())
      ->method('select')
      ->willReturn(['response' => ['numFound' => 0, 'docs' => []]]);

    $backend = $this->backendWithClient($client);
    $backend->search($query);
  }

  /**
   * Negative case (and default): a plain search with the "highlight" config
   * left off (default FALSE) leaves result items without
   * "highlighted_fields" extra data -- see ResponseParserTest for the
   * positive (highlighting-populated) case, which is ResponseParser's
   * responsibility, not the backend's.
   *
   * @covers ::search
   */
  public function testSearchLeavesNoHighlightedFieldsExtraDataWithoutHighlighting(): void {
    $index = $this->mockIndex();
    $query = $this->mockQuery($index);

    $client = $this->createMock(WayfinderClient::class);
    $client->method('select')->willReturn([
      'response' => [
        'numFound' => 1,
        'start' => 0,
        'docs' => [['id' => 'my_index-doc1', 'score' => 1.0]],
      ],
    ]);

    $backend = $this->backendWithClient($client);
    $backend->search($query);

    $item = $query->getResults()->getResultItems()['doc1'];
    $this->assertFalse($item->hasExtraData('highlighted_fields'));
  }

  /**
   * Correction (orchestrator review): highlighting is triggered by a
   * backend-level config setting, not a query option (search_api_solr's own
   * convention, which the plan doc pins this to -- "populating
   * highlighted_fields extra data as search_api_solr does"). So search()
   * must read its own configuration (not $query->getOption()) to decide
   * whether to request hl at all.
   *
   * @covers ::search
   */
  public function testSearchRequestsHlWhenBackendIsConfiguredForHighlighting(): void {
    $index = $this->mockIndex();
    $query = $this->mockQuery($index);

    $client = $this->createMock(WayfinderClient::class);
    $client->expects($this->once())
      ->method('select')
      ->with($this->callback(fn (array $params) => ($params['hl'] ?? NULL) === 'true'))
      ->willReturn(['response' => ['numFound' => 0, 'docs' => []]]);

    $backend = $this->backendWithClient($client, ['highlight' => TRUE]);
    $backend->search($query);
  }

  /**
   * Builds a WayfinderBackend via its DI factory, with a mocked http_client
   * that serves the given response for any request (matching the
   * MockHandler/HandlerStack pattern already used in WayfinderClientTest).
   *
   * A Throwable may be queued instead of a Response to simulate a transport
   * failure; MockHandler accepts either.
   */
  private function createBackend(Response|\Throwable $adminSystemResponse, array $configuration = []): WayfinderBackend {
    $mock = new MockHandler([$adminSystemResponse]);
    $handlerStack = HandlerStack::create($mock);
    $httpClient = new Client(['handler' => $handlerStack]);

    $stringTranslation = $this->createMock(TranslationInterface::class);
    $stringTranslation->method('translate')
      ->willReturnCallback(fn (string $string, array $args = []) => strtr($string, $args));
    // TranslatableMarkup::render() -- what casting a row's 'label' to string
    // goes through -- calls translateString(), not translate(). Without this
    // stub every label stringifies to '' and a label assertion would pass
    // vacuously.
    $stringTranslation->method('translateString')
      ->willReturnCallback(fn (TranslatableMarkup $translated) => $translated->getUntranslatedString());

    $container = $this->createMock(ContainerInterface::class);
    $container->method('get')->willReturnMap([
      ['http_client', ContainerInterface::EXCEPTION_ON_INVALID_REFERENCE, $httpClient],
      ['search_api.fields_helper', ContainerInterface::EXCEPTION_ON_INVALID_REFERENCE, $this->createMock(FieldsHelper::class)],
      ['messenger', ContainerInterface::EXCEPTION_ON_INVALID_REFERENCE, $this->createMock(MessengerInterface::class)],
      ['string_translation', ContainerInterface::EXCEPTION_ON_INVALID_REFERENCE, $stringTranslation],
    ]);

    $configuration += [
      'scheme' => 'http',
      'host' => 'localhost',
      'port' => 8983,
      'path' => '/wayfinder',
      'core' => 'mycore',
      'timeout' => 5,
      'commitWithin' => 1000,
    ];

    /** @var \Drupal\search_api_wayfinder\Plugin\search_api\backend\WayfinderBackend $backend */
    $backend = WayfinderBackend::create($container, $configuration, 'wayfinder', ['id' => 'wayfinder']);
    return $backend;
  }

  /**
   * @covers ::viewSettings
   */
  public function testViewSettingsIncludesVersionStringFromAdminSystem(): void {
    $body = $this->wayfinderAdminSystemBody();
    $backend = $this->createBackend(new Response(200, [], $body));

    $settings = $backend->viewSettings();

    $versionRows = array_values(array_filter(
      $settings,
      fn (array $row) => (string) $row['label'] === 'Wayfinder version'
    ));

    $this->assertCount(
      1,
      $versionRows,
      'viewSettings() should include exactly one "Wayfinder version" row, sourced from admin/system (solr-ref/responses/admin_system.json: lucene.wayfinder-spec-version).'
    );
    // Exact match, not a substring check: the fixture's sibling
    // lucene.wayfinder-impl-version is "9.10.1 c135e6335c... - gerlowskija - ..."
    // and so *contains* "9.10.1" too. A real Wayfinder server emits
    // wayfinder-impl-version as "{version} wayfinder" (src/lib.rs), so a substring
    // assertion would pass while the admin panel rendered "9.0.0 wayfinder".
    $this->assertSame(
      '9.10.1',
      $versionRows[0]['info'],
      'The version row must carry lucene.wayfinder-spec-version verbatim, not lucene.wayfinder-impl-version.'
    );
  }

  /**
   * Loads solr-ref/responses/admin_system.json (real captured Solr, left
   * untouched as ground truth for shape) and patches only the two sibling
   * keys Wayfinder itself renamed (#325): lucene.solr-spec-version and
   * lucene.solr-impl-version become lucene.wayfinder-spec-version and
   * lucene.wayfinder-impl-version. This reproduces what a real (renamed)
   * Wayfinder server now emits without touching the captured fixture.
   */
  private function wayfinderAdminSystemBody(): string {
    $raw = (string) file_get_contents(__DIR__ . '/../../../../../solr-ref/responses/admin_system.json');
    $decoded = json_decode($raw, TRUE);
    $decoded['lucene']['wayfinder-spec-version'] = $decoded['lucene']['solr-spec-version'];
    $decoded['lucene']['wayfinder-impl-version'] = $decoded['lucene']['solr-impl-version'];
    unset($decoded['lucene']['solr-spec-version'], $decoded['lucene']['solr-impl-version']);
    return (string) json_encode($decoded);
  }

  /**
   * @covers ::viewSettings
   */
  public function testViewSettingsStillIncludesServerUrl(): void {
    $body = $this->wayfinderAdminSystemBody();
    $backend = $this->createBackend(new Response(200, [], $body), ['core' => 'mycore']);

    $settings = $backend->viewSettings();

    $urlRows = array_filter(
      $settings,
      fn (array $row) => str_contains((string) $row['info'], 'http://localhost:8983/wayfinder/mycore')
    );

    $this->assertNotEmpty($urlRows, 'viewSettings() should still include the server core URL alongside the version handshake.');
  }

  /**
   * The admin/system handshake is an informational panel, not a critical
   * path: a failing call must degrade to the server-URL row rather than
   * throwing an exception out of the server's View page.
   *
   * Both WayfinderClient::request() failure arms are covered -- a non-200
   * error envelope (RequestException, which carries a response) and a
   * transport failure (ConnectException, which does not) -- since each
   * reaches the catch in viewSettings() by a different route.
   *
   * @dataProvider adminSystemFailureProvider
   * @covers ::viewSettings
   */
  public function testViewSettingsDegradesGracefullyWhenAdminSystemFails(Response|\Throwable $failure): void {
    $backend = $this->createBackend($failure, ['core' => 'mycore']);

    $settings = $backend->viewSettings();

    $urlRows = array_filter(
      $settings,
      fn (array $row) => str_contains((string) $row['info'], 'http://localhost:8983/wayfinder/mycore')
    );
    $this->assertNotEmpty($urlRows, 'A failed admin/system handshake must still leave the server URL row in place.');

    $versionRows = array_filter(
      $settings,
      fn (array $row) => (string) $row['label'] === 'Wayfinder version'
    );
    $this->assertSame([], $versionRows, 'A failed admin/system handshake must not produce a version row.');
  }

  /**
   * @covers ::extractContentFromFile
   */
  public function testExtractContentFromFileReturnsExtractedTextUnderFileKey(): void {
    // The mock client returns the full /update/extract envelope; the backend
    // reads the text out of the "file" key (the multipart part name -- #258).
    $client = $this->createMock(WayfinderClient::class);
    $client->expects($this->once())
      ->method('extract')
      ->with('/tmp/sample.txt')
      ->willReturn(['responseHeader' => ['status' => 0], 'file' => "Hello plain text.\nSecond line."]);

    $backend = $this->backendWithClient($client);

    $this->assertSame("Hello plain text.\nSecond line.", $backend->extractContentFromFile('/tmp/sample.txt'));
  }

  /**
   * A response that carries no "file" key yields an empty string rather than
   * a missing-index notice: the item still indexes, just without attachment
   * text. This is the backend half of "extraction failure must not fail the
   * whole index batch" (#262).
   *
   * @covers ::extractContentFromFile
   */
  public function testExtractContentFromFileReturnsEmptyStringWhenResponseHasNoFileKey(): void {
    $client = $this->createMock(WayfinderClient::class);
    $client->method('extract')->willReturn(['responseHeader' => ['status' => 0]]);

    $backend = $this->backendWithClient($client);

    $this->assertSame('', $backend->extractContentFromFile('/tmp/sample.txt'));
  }

  /**
   * A non-200 /update/extract response (or transport failure) propagates as
   * SearchApiException out of the backend -- the processor's per-file catch
   * is what stops it failing the batch, not the backend swallowing it.
   *
   * @covers ::extractContentFromFile
   */
  public function testExtractContentFromFilePropagatesSearchApiExceptionFromClient(): void {
    $client = $this->createMock(WayfinderClient::class);
    $client->method('extract')
      ->willThrowException(new SearchApiException('authentication required'));

    $backend = $this->backendWithClient($client);

    $this->expectException(SearchApiException::class);
    $this->expectExceptionMessage('authentication required');

    $backend->extractContentFromFile('/tmp/sample.txt');
  }

  /**
   * @return array<string, array{0: \GuzzleHttp\Psr7\Response|\Throwable}>
   */
  public static function adminSystemFailureProvider(): array {
    return [
      'error envelope' => [new Response(500, [], '{"error":{"msg":"boom","code":500}}')],
      'bare non-200' => [new Response(404, [], 'not found')],
      'transport failure' => [
        new ConnectException(
          'Connection refused',
          new Request('GET', 'http://localhost:8983/wayfinder/mycore/admin/system')
        ),
      ],
    ];
  }

  /**
   * A 200 response that does not carry lucene.wayfinder-spec-version appends no
   * version row, rather than an empty or "null"-rendering one.
   *
   * @dataProvider adminSystemWithoutVersionProvider
   * @covers ::viewSettings
   */
  public function testViewSettingsOmitsVersionRowWhenResponseLacksVersion(string $body): void {
    $backend = $this->createBackend(new Response(200, [], $body), ['core' => 'mycore']);

    $settings = $backend->viewSettings();

    $versionRows = array_filter(
      $settings,
      fn (array $row) => (string) $row['label'] === 'Wayfinder version'
    );
    $this->assertSame([], $versionRows, 'No version row should be appended when the response carries no usable version string.');

    $urlRows = array_filter(
      $settings,
      fn (array $row) => str_contains((string) $row['info'], 'http://localhost:8983/wayfinder/mycore')
    );
    $this->assertNotEmpty($urlRows, 'The server URL row must survive a version-less admin/system response.');
  }

  /**
   * @return array<string, array{0: string}>
   */
  public static function adminSystemWithoutVersionProvider(): array {
    return [
      'no lucene block' => ['{"responseHeader":{"status":0}}'],
      'lucene block without wayfinder-spec-version' => ['{"lucene":{"lucene-spec-version":"9.12.3"}}'],
      'empty version string' => ['{"lucene":{"wayfinder-spec-version":""}}'],
      'non-string version' => ['{"lucene":{"wayfinder-spec-version":{"nested":"object"}}}'],
      'empty body' => [''],
    ];
  }

  /**
   * #291: getAutocompleteSuggestions() builds a /terms request through
   * QueryBuilder, sends it via WayfinderClient::terms(), and folds the
   * interleaved [term, count, ...] list into SuggestionInterface[] via
   * SuggestionFactory::createFromSuggestionSuffix(suffix, count) -- mirroring
   * search_api_solr's getAutocompleteTermSuggestions (finding 156). The suffix
   * is the term minus the typed prefix: 'rocket' with incomplete 'roc' ->
   * suffix 'ket', count 5.
   *
   * @covers ::getAutocompleteSuggestions
   */
  public function testGetAutocompleteSuggestionsBuildsTermsQueryAndParsesSuggestions(): void {
    $index = $this->mockIndex();
    $query = $this->mockQuery($index, ['limit' => 10]);
    $search = $this->createMock(SearchInterface::class);

    $client = $this->createMock(WayfinderClient::class);
    $client->expects($this->once())
      ->method('terms')
      ->with($this->callback(function (array $params): bool {
        // #342: 'title' is a text field, no search_api_language condition
        // and no injected language manager on this bare
        // `new WayfinderBackend(...)`, so the resolved language is the
        // 'und' fallback -> tm_X3b_und_title (SearchApiSolrBackend.php:
        // 2450-2473).
        return $params['terms'] === 'true'
          && $params['terms.fl'] === 'tm_X3b_und_title'
          && $params['terms.prefix'] === 'roc'
          && $params['terms.limit'] === 10
          && $params['omitHeader'] === 'true';
      }))
      ->willReturn(['terms' => ['tm_X3b_und_title' => ['rocket', 5, 'rocker', 2]]]);

    $backend = $this->backendWithClient($client);
    $suggestions = $backend->getAutocompleteSuggestions($query, $search, 'roc', 'roc');

    $this->assertCount(2, $suggestions);
    $this->assertContainsOnlyInstancesOf(SuggestionInterface::class, $suggestions);
    $this->assertSame('ket', $suggestions[0]->getSuggestionSuffix());
    $this->assertSame(5, $suggestions[0]->getResultsCount());
    $this->assertSame('ker', $suggestions[1]->getSuggestionSuffix());
    $this->assertSame(2, $suggestions[1]->getResultsCount());
  }

  /**
   * A failing /terms request (transport error, non-200 error envelope) degrades
   * to an empty suggestion list rather than throwing out of the autocomplete
   * widget -- mirroring search_api_solr's SearchApiException catch around the
   * autocomplete query (SearchApiSolrBackend.php:3981-3992). This is the
   * backend's one acceptance guard, so it is mutation-tested: remove the
   * catch and this test fails because the exception propagates.
   *
   * @covers ::getAutocompleteSuggestions
   */
  public function testGetAutocompleteSuggestionsReturnsEmptyArrayOnTransportError(): void {
    $index = $this->mockIndex();
    $query = $this->mockQuery($index);
    $search = $this->createMock(SearchInterface::class);

    $client = $this->createMock(WayfinderClient::class);
    $client->method('terms')
      ->willThrowException(new SearchApiException('connection refused'));

    $backend = $this->backendWithClient($client);
    $this->assertSame([], $backend->getAutocompleteSuggestions($query, $search, 'roc', 'roc'));
  }

  /**
   * Terms from every terms.fl field are merged into one term->count map
   * (search_api_solr's getAutocompleteTermSuggestions, finding 156), so a
   * multi-field response yields one flat suggestion list.
   *
   * @covers ::getAutocompleteSuggestions
   */
  public function testGetAutocompleteSuggestionsMergesTermsAcrossFields(): void {
    $index = $this->mockIndex();
    $query = $this->mockQuery($index);
    $search = $this->createMock(SearchInterface::class);

    $client = $this->createMock(WayfinderClient::class);
    $client->method('terms')->willReturn([
      'terms' => [
        'ts_title' => ['rocket', 5],
        'tm_body' => ['rocker', 2],
      ],
    ]);

    $backend = $this->backendWithClient($client);
    $suggestions = $backend->getAutocompleteSuggestions($query, $search, 'roc', 'roc');

    $suffixes = array_map(fn (SuggestionInterface $s) => $s->getSuggestionSuffix(), $suggestions);
    sort($suffixes);
    $this->assertSame(['ker', 'ket'], $suffixes);
  }

  /**
   * A response with no term dictionary to offer (empty list, or no terms block
   * at all) degrades to an empty suggestion array rather than throwing -- the
   * widget simply renders nothing. This mirrors search_api_solr, which
   * returns the (empty) merged map unchanged.
   *
   * @dataProvider emptyTermsResponseProvider
   * @covers ::getAutocompleteSuggestions
   */
  public function testGetAutocompleteSuggestionsReturnsEmptyArrayWhenTermsBlockIsEmpty(array $response): void {
    $index = $this->mockIndex();
    $query = $this->mockQuery($index);
    $search = $this->createMock(SearchInterface::class);

    $client = $this->createMock(WayfinderClient::class);
    $client->method('terms')->willReturn($response);

    $backend = $this->backendWithClient($client);
    $this->assertSame([], $backend->getAutocompleteSuggestions($query, $search, 'roc', 'roc'));
  }

  /**
   * @return array<string, array{0: array}>
   */
  public static function emptyTermsResponseProvider(): array {
    return [
      'empty list per field' => [['terms' => ['ts_title' => []]]],
      'no terms key' => [['responseHeader' => ['status' => 0]]],
      'empty body' => [[]],
    ];
  }

  /**
   * issue #342: create() injects the container's language_manager service so
   * QueryBuilder can resolve "all enabled site languages" (step 2 of the
   * language-resolution order) when the query carries no
   * search_api_language condition. Proven end-to-end through search(): a
   * text field's qf expands to one variant per enabled language, in the
   * language manager's order, exactly as if the injected manager had been
   * passed to `new QueryBuilder($fieldMapper, $languageManager)` directly
   * (mirrors QueryBuilderTest::
   * testLanguageResolvesFromTheInjectedLanguageManagerWhenNoConditionIsPresent).
   *
   * @covers ::create
   * @covers ::search
   */
  public function testCreateInjectsLanguageManagerSoSearchExpandsEnabledLanguages(): void {
    $index = $this->mockIndex();
    $query = $this->mockQuery($index);

    $languageManager = $this->createMock(LanguageManagerInterface::class);
    $en = $this->createMock(LanguageInterface::class);
    $en->method('getId')->willReturn('en');
    $de = $this->createMock(LanguageInterface::class);
    $de->method('getId')->willReturn('de');
    $languageManager->method('getLanguages')->willReturn(['en' => $en, 'de' => $de]);

    $selectResponse = new Response(200, [], (string) json_encode([
      'response' => ['numFound' => 0, 'start' => 0, 'docs' => []],
    ]));
    $mock = new MockHandler([$selectResponse]);
    $handlerStack = HandlerStack::create($mock);
    $httpClient = new Client(['handler' => $handlerStack]);

    $stringTranslation = $this->createMock(TranslationInterface::class);
    $stringTranslation->method('translate')
      ->willReturnCallback(fn (string $string, array $args = []) => strtr($string, $args));

    $container = $this->createMock(ContainerInterface::class);
    $container->method('get')->willReturnMap([
      ['http_client', ContainerInterface::EXCEPTION_ON_INVALID_REFERENCE, $httpClient],
      ['search_api.fields_helper', ContainerInterface::EXCEPTION_ON_INVALID_REFERENCE, $this->createMock(FieldsHelper::class)],
      ['messenger', ContainerInterface::EXCEPTION_ON_INVALID_REFERENCE, $this->createMock(MessengerInterface::class)],
      ['string_translation', ContainerInterface::EXCEPTION_ON_INVALID_REFERENCE, $stringTranslation],
      ['language_manager', ContainerInterface::EXCEPTION_ON_INVALID_REFERENCE, $languageManager],
    ]);

    $configuration = [
      'scheme' => 'http',
      'host' => 'localhost',
      'port' => 8983,
      'path' => '/wayfinder',
      'core' => 'mycore',
      'timeout' => 5,
      'commitWithin' => 1000,
    ];

    /** @var \Drupal\search_api_wayfinder\Plugin\search_api\backend\WayfinderBackend $backend */
    $backend = WayfinderBackend::create($container, $configuration, 'wayfinder', ['id' => 'wayfinder']);
    $backend->search($query);

    $sentRequest = $mock->getLastRequest();
    $this->assertNotNull($sentRequest, 'search() must have sent a request through the real WayfinderClient.');
    parse_str($sentRequest->getUri()->getQuery(), $sentParams);

    $this->assertSame('tm_X3b_en_title tm_X3b_de_title', $sentParams['qf'] ?? NULL);
  }

}
