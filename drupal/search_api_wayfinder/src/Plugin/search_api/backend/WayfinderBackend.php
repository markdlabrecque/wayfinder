<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder\Plugin\search_api\backend;

use Drupal\Core\Form\FormStateInterface;
use Drupal\Core\Plugin\PluginFormInterface;
use Drupal\Core\StringTranslation\TranslatableMarkup;
use Drupal\search_api\Attribute\SearchApiBackend;
use Drupal\search_api\Backend\BackendPluginBase;
use Drupal\search_api\IndexInterface;
use Drupal\search_api\Query\QueryInterface;
use Drupal\search_api\SearchApiException;
use Drupal\search_api_wayfinder\DocumentBuilder;
use Drupal\search_api_wayfinder\FieldMapper;
use Drupal\search_api_wayfinder\QueryBuilder;
use Drupal\search_api_wayfinder\ResponseParser;
use Drupal\search_api_wayfinder\WayfinderClient;
use GuzzleHttp\ClientInterface;
use Symfony\Component\DependencyInjection\ContainerInterface;

/**
 * Search API backend talking Solr wire format to a Wayfinder server.
 *
 * Thin glue only: config form, feature flags, and delegation to the plain
 * translation classes (QueryBuilder, DocumentBuilder, ResponseParser,
 * FieldMapper) and WayfinderClient's HTTP transport. Per plan doc
 * "Architecture" section: fulltext search with filters/sorts/facets,
 * More Like This routing, optional server-side highlighting, indexing, and
 * ping.
 */
#[SearchApiBackend(
  id: 'wayfinder',
  label: new TranslatableMarkup('Wayfinder'),
  description: new TranslatableMarkup('Index items and query a Wayfinder server, a Solr-wire-compatible search backend.'),
)]
class WayfinderBackend extends BackendPluginBase implements PluginFormInterface {

  /**
   * The HTTP client, injected via the container for the Guzzle wrapper.
   */
  protected ClientInterface $httpClient;

  /**
   * {@inheritdoc}
   */
  public static function create(ContainerInterface $container, array $configuration, $plugin_id, $plugin_definition) {
    /** @var static $plugin */
    $plugin = parent::create($container, $configuration, $plugin_id, $plugin_definition);
    $plugin->httpClient = $container->get('http_client');
    return $plugin;
  }

  /**
   * {@inheritdoc}
   */
  public function defaultConfiguration() {
    return [
      'scheme' => 'http',
      'host' => 'localhost',
      'port' => 8983,
      'path' => '/solr',
      'core' => '',
      'timeout' => 5,
      'commitWithin' => 1000,
      // Deliberately exported plugin config, mirroring search_api_solr's
      // BasicAuthTrait. This keeps the backend dependency-free but means an
      // exported config file contains the password; use Drupal config access
      // controls appropriate to a secret. A Key integration is out of scope.
      'username' => '',
      'password' => '',
      // Server-side highlighting is opt-in per plan doc locked decision 6:
      // sites that prefer Search API's own algorithmic "highlight" processor
      // need nothing from the backend, and shouldn't pay for hl on every
      // query.
      'highlight' => FALSE,
    ];
  }

  /**
   * {@inheritdoc}
   */
  public function buildConfigurationForm(array $form, FormStateInterface $form_state) {
    $config = $this->getConfiguration();

    $form['scheme'] = [
      '#type' => 'select',
      '#title' => $this->t('HTTP protocol'),
      '#options' => ['http' => 'http', 'https' => 'https'],
      '#default_value' => $config['scheme'],
    ];
    $form['host'] = [
      '#type' => 'textfield',
      '#title' => $this->t('Wayfinder host'),
      '#default_value' => $config['host'],
      '#required' => TRUE,
    ];
    $form['port'] = [
      '#type' => 'number',
      '#title' => $this->t('Wayfinder port'),
      '#default_value' => $config['port'],
      '#required' => TRUE,
    ];
    $form['path'] = [
      '#type' => 'textfield',
      '#title' => $this->t('Base path'),
      '#default_value' => $config['path'],
    ];
    $form['core'] = [
      '#type' => 'textfield',
      '#title' => $this->t('Wayfinder core'),
      '#default_value' => $config['core'],
      '#required' => TRUE,
    ];
    $form['timeout'] = [
      '#type' => 'number',
      '#title' => $this->t('Request timeout (seconds)'),
      '#default_value' => $config['timeout'],
    ];
    $form['username'] = [
      '#type' => 'textfield',
      '#title' => $this->t('HTTP Basic authentication username'),
      '#default_value' => $config['username'],
    ];
    $form['password'] = [
      '#type' => 'password',
      '#title' => $this->t('HTTP Basic authentication password'),
    ];
    // Keep the stored password out of rendered form values. If the password
    // input is blank on submit, validation restores it only for this username.
    $form_state->set('password', $config['password']);
    $form['highlight'] = [
      '#type' => 'checkbox',
      '#title' => $this->t('Retrieve result highlighting from the server'),
      '#description' => $this->t('Ask Wayfinder for highlighted snippets and expose them as each result item\'s "highlighted_fields" data. Leave off to use Search API\'s own Highlight processor instead.'),
      '#default_value' => $config['highlight'] ?? FALSE,
    ];

    return $form;
  }

  /**
   * {@inheritdoc}
   */
  public function validateConfigurationForm(array &$form, FormStateInterface $form_state) {
    $host = $form_state->getValue('host');
    if (is_string($host) && trim($host) === '') {
      $form_state->setError($form['host'], $this->t('Wayfinder host cannot be empty.'));
    }

    $core = $form_state->getValue('core');
    if (is_string($core) && trim($core) === '') {
      $form_state->setError($form['core'], $this->t('Wayfinder core cannot be empty.'));
    }

    $port = $form_state->getValue('port');
    if ($port !== NULL && $port !== '' && ((int) $port < 1 || (int) $port > 65535)) {
      $form_state->setError($form['port'], $this->t('Port must be between 1 and 65535.'));
    }

    $username = $form_state->getValue('username');
    $password = $form_state->getValue('password');
    if ($password === '' && $username === ($this->configuration['username'] ?? '')) {
      $password = $form_state->get('password');
      $form_state->setValue('password', $password);
    }

    if (!is_string($username) || !is_string($password)) {
      return;
    }
    if (($username === '') !== ($password === '')) {
      $form_state->setErrorByName('password', $this->t('HTTP Basic authentication requires both a username and a password.'));
    }
    if (str_contains($username, ':')) {
      $form_state->setErrorByName('username', $this->t('HTTP Basic authentication usernames cannot contain a colon.'));
    }
    if (preg_match('/[\x00-\x1F\x7F]/', $username) === 1) {
      $form_state->setErrorByName('username', $this->t('HTTP Basic authentication usernames cannot contain ASCII control characters.'));
    }
    if (preg_match('/[\x00-\x1F\x7F]/', $password) === 1) {
      $form_state->setErrorByName('password', $this->t('HTTP Basic authentication passwords cannot contain ASCII control characters.'));
    }
  }

  /**
   * {@inheritdoc}
   */
  public function submitConfigurationForm(array &$form, FormStateInterface $form_state) {
    $values = $form_state->getValues();
    foreach (['scheme', 'host', 'port', 'path', 'core', 'timeout', 'username', 'password', 'highlight'] as $key) {
      if (array_key_exists($key, $values)) {
        $this->configuration[$key] = $values[$key];
      }
    }
  }

  /**
   * {@inheritdoc}
   */
  public function getSupportedFeatures() {
    // Facet support is AND-only: OR facets need {!ex}/{!tag}, which Wayfinder
    // does not support, so 'search_api_facets_operator_or' stays unadvertised
    // (plan doc locked decision 4).
    return ['search_api_facets', 'search_api_mlt'];
  }

  /**
   * {@inheritdoc}
   */
  public function supportsDataType($type) {
    return in_array($type, ['text', 'string', 'integer', 'decimal', 'date', 'boolean'], TRUE);
  }

  /**
   * {@inheritdoc}
   */
  public function isAvailable() {
    try {
      return $this->getClient()->ping();
    }
    catch (\Throwable $e) {
      return FALSE;
    }
  }

  /**
   * {@inheritdoc}
   */
  public function viewSettings() {
    $info = [
      [
        'label' => $this->t('Server URL'),
        'info' => $this->getCoreUrl(),
      ],
    ];

    // ponytail: an admin/system handshake that fails (unreachable server,
    // error envelope, unexpected body) drops the version row silently rather
    // than reporting the failure. This is an informational admin panel, and
    // the server's reachability is already reported by isAvailable()/ping();
    // surfacing the transport error here as well needs a place to put it that
    // does not read as a second, contradictory availability verdict.
    //
    // ponytail: this is also a second *blocking* HTTP request on the server's
    // View page, on top of isAvailable()'s ping -- against an unreachable
    // server the page waits out the configured timeout twice. Collapsing the
    // two into one probe means reworking isAvailable()'s contract, which is
    // Search API core's, not ours.
    //
    // Only SearchApiException is caught: that is the single failure mode
    // WayfinderClient::request() raises here (its encodeQuery() can throw
    // InvalidArgumentException, but not for adminSystem()'s static params).
    // Catching \Throwable would also swallow a genuine Error/TypeError bug.
    try {
      $system = $this->getClient()->adminSystem();
    }
    catch (SearchApiException $e) {
      return $info;
    }

    // Version lives at lucene.solr-spec-version -- ground truth
    // solr-ref/responses/admin_system.json.
    $version = $system['lucene']['solr-spec-version'] ?? NULL;
    if (is_string($version) && $version !== '') {
      $info[] = [
        'label' => $this->t('Wayfinder version'),
        'info' => $version,
      ];
    }

    return $info;
  }

  /**
   * {@inheritdoc}
   */
  public function indexItems(IndexInterface $index, array $items) {
    $client = $this->getClient();
    $builder = new DocumentBuilder(new FieldMapper());
    $commitWithin = $this->getConfiguration()['commitWithin'] ?? 1000;

    $indexedIds = [];
    foreach ($items as $id => $item) {
      $command = $builder->buildAddCommand($item, $index->id());
      $client->update($command, ['commitWithin' => $commitWithin]);
      $indexedIds[] = $id;
    }

    return $indexedIds;
  }

  /**
   * {@inheritdoc}
   */
  public function deleteItems(IndexInterface $index, array $item_ids) {
    $docIds = array_map(fn (string $id) => $index->id() . '-' . $id, $item_ids);
    $this->getClient()->update(['delete' => $docIds]);
  }

  /**
   * {@inheritdoc}
   */
  public function deleteAllIndexItems(IndexInterface $index, $datasource_id = NULL) {
    $query = 'index_id:"' . $index->id() . '"';
    if ($datasource_id) {
      $query .= ' AND ss_search_api_datasource:"' . $datasource_id . '"';
    }
    $this->getClient()->update(['delete' => ['query' => $query]]);
  }

  /**
   * {@inheritdoc}
   */
  public function search(QueryInterface $query): void {
    $queryBuilder = new QueryBuilder();
    $client = $this->getClient();

    // A More Like This query names a seed document rather than search keys,
    // and Wayfinder answers it on its own endpoint (plan doc architecture
    // table): route by the option, not by the keys.
    if ($query->getOption('search_api_mlt')) {
      $response = $client->mlt($queryBuilder->buildMlt($query));
    }
    else {
      // Highlighting is a backend-level setting, not a query option -- see
      // QueryBuilder::build()'s $highlighting argument.
      $highlight = !empty($this->configuration['highlight']);
      $response = $client->select($queryBuilder->build($query, $highlight));
    }

    (new ResponseParser())->parse($response, $query);
  }

  /**
   * Builds the base core URL from the current configuration.
   */
  protected function getCoreUrl(): string {
    $config = $this->getConfiguration();
    $path = '/' . ltrim((string) $config['path'], '/');
    return sprintf(
      '%s://%s:%s%s/%s',
      $config['scheme'],
      $config['host'],
      $config['port'],
      rtrim($path, '/'),
      $config['core']
    );
  }

  /**
   * Builds a WayfinderClient for the current configuration.
   */
  protected function getClient(): WayfinderClient {
    $timeout = (float) ($this->getConfiguration()['timeout'] ?? 5);
    $config = $this->getConfiguration();
    return new WayfinderClient(
      $this->httpClient,
      $this->getCoreUrl(),
      $timeout,
      $config['username'] ?? '',
      $config['password'] ?? '',
    );
  }

}
