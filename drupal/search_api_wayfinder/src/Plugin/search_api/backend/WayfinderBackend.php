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
 * "Architecture" section and M1 scope: plain fulltext search, indexing, and
 * ping only -- no facets/MLT/highlighting/filters/sorts yet.
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
  }

  /**
   * {@inheritdoc}
   */
  public function submitConfigurationForm(array &$form, FormStateInterface $form_state) {
    $values = $form_state->getValues();
    foreach (['scheme', 'host', 'port', 'path', 'core', 'timeout'] as $key) {
      if (array_key_exists($key, $values)) {
        $this->configuration[$key] = $values[$key];
      }
    }
  }

  /**
   * {@inheritdoc}
   */
  public function getSupportedFeatures() {
    // ponytail: MLT lands in M4 (plan doc milestone table) -- don't advertise
    // support the backend doesn't implement yet. Facet support is AND-only:
    // OR facets need {!ex}/{!tag}, which Wayfinder does not support.
    return ['search_api_facets'];
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
    $config = $this->getConfiguration();
    return [
      [
        'label' => $this->t('Server URL'),
        'info' => $this->getCoreUrl(),
      ],
    ];
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
    $params = (new QueryBuilder())->build($query);
    $response = $this->getClient()->select($params);
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
    return new WayfinderClient($this->httpClient, $this->getCoreUrl(), $timeout);
  }

}
