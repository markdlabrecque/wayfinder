<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder\Plugin\search_api\backend;

use Drupal\Core\Form\FormStateInterface;
use Drupal\Core\Language\LanguageManagerInterface;
use Drupal\Core\Plugin\PluginFormInterface;
use Drupal\Core\StringTranslation\TranslatableMarkup;
use Drupal\search_api\Attribute\SearchApiBackend;
use Drupal\search_api\Backend\BackendPluginBase;
use Drupal\search_api\IndexInterface;
use Drupal\search_api\Query\QueryInterface;
use Drupal\search_api\SearchApiException;
use Drupal\search_api_autocomplete\SearchInterface;
use Drupal\search_api_autocomplete\Suggestion\SuggestionFactory;
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
   * The language manager, injected via the container (issue #342).
   *
   * QueryBuilder/ResponseParser use it to resolve "every enabled site
   * language" when a query carries no search_api_language condition -- step 2
   * of LanguageResolver's order. It stays NULL for a plugin built without a
   * container (`new WayfinderBackend(...)` in unit tests), which simply skips
   * that step.
   */
  protected ?LanguageManagerInterface $languageManager = NULL;

  /**
   * {@inheritdoc}
   */
  public static function create(ContainerInterface $container, array $configuration, $plugin_id, $plugin_definition) {
    /** @var static $plugin */
    $plugin = parent::create($container, $configuration, $plugin_id, $plugin_definition);
    $plugin->httpClient = $container->get('http_client');
    $plugin->languageManager = $container->get('language_manager');
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
      'path' => '/wayfinder',
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
    // One feature per line so a sibling branch (#290 adds
    // 'search_api_grouping') lands on its own line and merges mechanically,
    // not by re-editing a single-line array. Order matches search_api_solr's
    // SearchApiSolrBackend::getSupportedFeatures().
    return [
      'search_api_facets',
      'search_api_facets_operator_or',
      'search_api_grouping',
      'search_api_mlt',
      // #291: advertises the search_api_autocomplete feature. The Server
      // suggester gates on supportsFeature('search_api_autocomplete') (OR-ed
      // with the instanceof checks -- finding 155) and then duck-types
      // getAutocompleteSuggestions(); like search_api_solr, the backend does
      // NOT formally implement the (documentation-only)
      // AutocompleteBackendInterface, so this flag is what activates it.
      'search_api_autocomplete',
      // #342: real suggestions and collations are answered by the server
      // (spellcheck.* params in QueryBuilder, the spellcheck envelope in
      // ResponseParser), so the backend advertises the feature the same way
      // search_api_solr does (SearchApiSolrBackend.php:777).
      'search_api_spellcheck',
    ];
  }

  /**
   * {@inheritdoc}
   */
  public function supportsDataType($type) {
    // One type per line so a sibling branch that adds another lands on its
    // own line and merges mechanically, not by re-editing a single-line array.
    //
    // The accepted set is exactly the types that round-trip through
    // FieldMapper's prefix table AND presets/search-api.toml on Wayfinder's
    // existing schema types: the six Search API defaults plus the
    // search_api_solr non-default types implemented in issue #300.
    //
    // Anything NOT listed here is an explicit descope (README "Not
    // supported"), returned FALSE so Search API surfaces the field as
    // unsupported at config time rather than accepting it and failing at
    // index time -- the silent-loss bug #300 exists to fix:
    // - solr_date_range: needs a server-side date-range type (Wayfinder's
    //   'date' holds a single instant, not a [start TO end] range).
    // - solr_text_custom / solr_text_custom_omit_norms: site-defined analyzer
    //   escape hatch (SolrFieldType entities); the preset has no equivalent.
    // - location / rpt: spatial types, belong to #292.
    $supported = [
      'text',
      'string',
      'integer',
      'decimal',
      'date',
      'boolean',
      'solr_string_storage',
      'solr_string_docvalues',
      'solr_text_unstemmed',
      'solr_text_omit_norms',
      'solr_text_wstoken',
      'solr_text_suggester',
      // #342: FieldMapper is language-aware now, so this type maps cleanly to
      // its fixed per-language sink 'spellcheck_<lang>'
      // (SearchApiSolrBackend.php:2440-2446).
      'solr_text_spellcheck',
    ];
    return in_array($type, $supported, TRUE);
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

    // Version lives at lucene.wayfinder-spec-version -- ground truth
    // solr-ref/responses/admin_system.json.
    $version = $system['lucene']['wayfinder-spec-version'] ?? NULL;
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
    // issue #342 (MF-3): the language manager decides which language-specific
    // sort_* copies each document carries, so it has to reach DocumentBuilder
    // exactly like it already reaches QueryBuilder/ResponseParser below --
    // otherwise indexing fills only the 'und' copy while search() sorts on
    // sort_X3b_<languages[0]>_<id>.
    $builder = new DocumentBuilder(new FieldMapper(), $this->languageManager);
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
    $queryBuilder = new QueryBuilder(new FieldMapper(), $this->languageManager);
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

    (new ResponseParser(new FieldMapper(), $this->languageManager))->parse($response, $query);
  }

  /**
   * Retrieves autocomplete suggestions for partial user input (#291).
   *
   * This is the duck-typed implementation of search_api_autocomplete's
   * AutocompleteBackendInterface contract (finding 155): the Server suggester
   * calls it after checking supportsFeature('search_api_autocomplete'), and
   * search_api_solr -- the parity target -- implements it the same way, by
   * signature match rather than a formal `implements` (the interface is
   * documentation-only). The SuggestionFactory/SearchInterface types it names
   * are autoloaded only when this runs, which is only ever from inside an
   * installed search_api_autocomplete, so the backend has no hard dependency
   * on that module.
   *
   * The path is the Terms component, not the SuggestComponent (finding 154):
   * QueryBuilder::buildAutocompleteTerms() emits terms=true/terms.fl/
   * terms.prefix/terms.limit, WayfinderClient::terms() GETs /terms, and the
   * interleaved [term, count, ...] lists are folded into SuggestionInterface[]
   * via SuggestionFactory::createFromSuggestionSuffix(suffix, count) --
   * mirroring search_api_solr's getAutocompleteTermSuggestions (finding 156),
   * including the cross-field term merge.
   *
   * A transport failure degrades to an empty suggestion list rather than
   * throwing out of the widget, mirroring search_api_solr's catch around the
   * autocomplete query (SearchApiSolrBackend.php:3981-3992).
   *
   * @param \Drupal\search_api\Query\QueryInterface $query
   *   A query representing the base search, with all completely entered words
   *   in the user input so far as the search keys.
   * @param \Drupal\search_api_autocomplete\SearchInterface $search
   *   An object containing details about the search the user is on, and
   *   settings for the autocompletion.
   * @param string $incomplete_key
   *   The start of another fulltext keyword for the search, which should be
   *   completed.
   * @param string $user_input
   *   The complete user input for the fulltext search keywords so far.
   *
   * @return \Drupal\search_api_autocomplete\Suggestion\SuggestionInterface[]
   *   An array of autocomplete suggestions.
   */
  public function getAutocompleteSuggestions(QueryInterface $query, SearchInterface $search, string $incomplete_key, string $user_input): array {
    try {
      $params = (new QueryBuilder(new FieldMapper(), $this->languageManager))->buildAutocompleteTerms($query, $incomplete_key);
      $response = $this->getClient()->terms($params);
    }
    catch (SearchApiException $e) {
      // A failing autocomplete query must not break the search widget --
      // return no suggestions, as search_api_solr does.
      return [];
    }

    // Merge every terms.fl field's [term, count, ...] list into one term->count
    // map (search_api_solr's getAutocompleteTermSuggestions, finding 156). The
    // default Terms response shape is the interleaved flat list (finding 142:
    // only json.nl=map rewrites it to an object, and this client sends no
    // json.nl). A collision across fields keeps the last field's count, as the
    // upstream loop does.
    $terms = [];
    foreach ($response['terms'] ?? [] as $list) {
      if (!is_array($list)) {
        continue;
      }
      for ($i = 0, $n = count($list); $i + 1 < $n; $i += 2) {
        $term = $list[$i];
        if (is_string($term)) {
          $terms[$term] = $list[$i + 1];
        }
      }
    }

    $factory = new SuggestionFactory($user_input);
    $suggestions = [];
    foreach ($terms as $term => $count) {
      // The suggestion is the typed prefix's completion: the term minus the
      // incomplete key the user already entered.
      $suffix = mb_substr($term, mb_strlen($incomplete_key));
      $suggestions[] = $factory->createFromSuggestionSuffix($suffix, is_numeric($count) ? (int) $count : NULL);
    }
    return $suggestions;
  }

  /**
   * Extracts text from a file via Wayfinder's /update/extract endpoint.
   *
   * Mirrors search_api_solr's SearchApiSolrBackend::extractContentFromFile()
   * signature and contract (the evidenced client path #171/#258 captured):
   * upload the file, read the extracted text back. The file_attachments
   * processor (issue #262) reaches this through the index's server backend.
   *
   * Extraction failure propagates as SearchApiException; the processor's
   * per-file catch turns that into a logged skip so one bad attachment never
   * fails the whole index batch. A response without a "file" key yields an
   * empty string, so the item still indexes minus that attachment's text.
   *
   * @param string $filepath
   *   Real filesystem path to the file to extract.
   *
   * @return string
   *   The extracted plain text.
   *
   * @throws \Drupal\search_api\SearchApiException
   */
  public function extractContentFromFile(string $filepath): string {
    $response = $this->getClient()->extract($filepath);
    // The multipart part name "file" is the result key (#258), not
    // resource.name; Wayfinder emits the raw key and we read it directly.
    return (string) ($response['file'] ?? '');
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
