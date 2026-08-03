<?php

declare(strict_types=1);

namespace Drupal\Tests\search_api_wayfinder\Unit;

use Drupal\Core\Field\FieldDefinitionInterface;
use Drupal\Core\Field\FieldStorageDefinitionInterface;
use Drupal\Core\Form\FormState;
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
 * doc's "Premises to verify before implementing" item (c).
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
      'path' => '/solr',
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
    $body = (string) file_get_contents(__DIR__ . '/../../../../../solr-ref/responses/admin_system.json');
    $backend = $this->createBackend(new Response(200, [], $body));

    $settings = $backend->viewSettings();

    $versionRows = array_values(array_filter(
      $settings,
      fn (array $row) => (string) $row['label'] === 'Wayfinder version'
    ));

    $this->assertCount(
      1,
      $versionRows,
      'viewSettings() should include exactly one "Wayfinder version" row, sourced from admin/system (solr-ref/responses/admin_system.json: lucene.solr-spec-version).'
    );
    // Exact match, not a substring check: the fixture's sibling
    // lucene.solr-impl-version is "9.10.1 c135e6335c... - gerlowskija - ..."
    // and so *contains* "9.10.1" too. A real Wayfinder server emits
    // solr-impl-version as "{version} wayfinder" (src/lib.rs), so a substring
    // assertion would pass while the admin panel rendered "9.0.0 wayfinder".
    $this->assertSame(
      '9.10.1',
      $versionRows[0]['info'],
      'The version row must carry lucene.solr-spec-version verbatim, not lucene.solr-impl-version.'
    );
  }

  /**
   * @covers ::viewSettings
   */
  public function testViewSettingsStillIncludesServerUrl(): void {
    $body = (string) file_get_contents(__DIR__ . '/../../../../../solr-ref/responses/admin_system.json');
    $backend = $this->createBackend(new Response(200, [], $body), ['core' => 'mycore']);

    $settings = $backend->viewSettings();

    $urlRows = array_filter(
      $settings,
      fn (array $row) => str_contains((string) $row['info'], 'http://localhost:8983/solr/mycore')
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
      fn (array $row) => str_contains((string) $row['info'], 'http://localhost:8983/solr/mycore')
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
          new Request('GET', 'http://localhost:8983/solr/mycore/admin/system')
        ),
      ],
    ];
  }

  /**
   * A 200 response that does not carry lucene.solr-spec-version appends no
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
      fn (array $row) => str_contains((string) $row['info'], 'http://localhost:8983/solr/mycore')
    );
    $this->assertNotEmpty($urlRows, 'The server URL row must survive a version-less admin/system response.');
  }

  /**
   * @return array<string, array{0: string}>
   */
  public static function adminSystemWithoutVersionProvider(): array {
    return [
      'no lucene block' => ['{"responseHeader":{"status":0}}'],
      'lucene block without solr-spec-version' => ['{"lucene":{"lucene-spec-version":"9.12.3"}}'],
      'empty version string' => ['{"lucene":{"solr-spec-version":""}}'],
      'non-string version' => ['{"lucene":{"solr-spec-version":{"nested":"object"}}}'],
      'empty body' => [''],
    ];
  }

}
