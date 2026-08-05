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
    // issue #362: DocumentBuilder writes one language-agnostic sort_<id>
    // copy per sortable field (no longer one per site language), so it no
    // longer needs the language manager the way QueryBuilder/ResponseParser
    // below still do.
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
   * Retrieves spellcheck-based autocomplete suggestions (#385).
   *
   * The transport half of this module's Spellcheck suggester plugin
   * (src/Plugin/search_api_autocomplete/suggester/Spellcheck.php), which stays
   * thin and delegates here -- the same split the Terms path uses above.
   * Upstream's equivalent pair is
   * search_api_solr_autocomplete's suggester/Spellcheck.php:106-168 plus
   * SolrSpellcheckBackendTrait::extractSpellCheckSuggestions().
   *
   * Stock search_api_solr sends this to the /autocomplete request handler;
   * Wayfinder has no such route (#351), so the spellcheck component is asked
   * for on a plain /select with rows=0 and no q/fq -- see
   * QueryBuilder::buildAutocompleteSpellcheck() for why that is legal and for
   * the deliberately omitted spellcheck.count.
   *
   * The nesting matters and is upstream's: extractSpellCheckSuggestions()
   * returns <original term> => [<suggested word>, ...], and
   * getAutocompleteSpellCheckSuggestions() emits ONE
   * createFromSuggestedKeys($keys) per ELEMENT of each term's list
   * (Spellcheck.php:160-168) -- one suggestion per suggested word, not one per
   * corrected token. The {word, freq} extendedResults member shape is
   * normalised by ResponseParser::extractSpellcheckSuggestions(), shared with
   * the search path so the two cannot disagree about the envelope.
   *
   * A transport failure degrades to an empty suggestion list rather than
   * throwing out of the widget, as the Terms path does.
   *
   * @param \Drupal\search_api\Query\QueryInterface $query
   *   A query representing the base search.
   * @param string $user_input
   *   The complete user input for the fulltext search keywords so far.
   *
   * @return \Drupal\search_api_autocomplete\Suggestion\SuggestionInterface[]
   *   An array of autocomplete suggestions.
   */
  public function getSpellcheckAutocompleteSuggestions(QueryInterface $query, string $user_input): array {
    try {
      $params = (new QueryBuilder(new FieldMapper(), $this->languageManager))->buildAutocompleteSpellcheck($query, $user_input);
      $response = $this->getClient()->select($params);
    }
    catch (SearchApiException $e) {
      return [];
    }

    $factory = new SuggestionFactory($user_input);
    $suggestions = [];
    foreach (ResponseParser::extractSpellcheckSuggestions($response) as $words) {
      foreach ($words as $keys) {
        $suggestions[] = $factory->createFromSuggestedKeys($keys);
      }
    }

    return $this->filterDuplicateAutocompleteSuggestions($suggestions);
  }

  /**
   * Retrieves suggester-based autocomplete suggestions (#385).
   *
   * The transport half of this module's Suggester plugin
   * (src/Plugin/search_api_autocomplete/suggester/Suggester.php). Upstream's
   * equivalent is search_api_solr_autocomplete's suggester/Suggester.php:
   * 204-224 (the query + dedupe) and :301-317 (the response walk).
   *
   * QueryBuilder::buildAutocompleteSuggester() emits the suggest.* params and
   * WayfinderClient::suggest() GETs /suggest (server read path: #384). The
   * response is nested dictionary -> query -> suggestions[], so every
   * dictionary key and every query key present contributes, in response order
   * -- upstream walks `$phrases_result->getAll()` across all dictionaries
   * (:305-306), not just the first. Each entry's `term` is passed to
   * createFromSuggestedKeys() VERBATIM (:308): the suggestions are phrases, and
   * any <b> markup Solr put in them is Solr's own highlighting, which this
   * layer must neither assume nor strip (ground truth #384's fixture
   * solr-ref/responses/suggest_q_infix_en.json).
   *
   * Every dictionary in the response is walked, but note the builder's second
   * ponytail (QueryBuilder::buildAutocompleteSuggester()): #384's server reads
   * a single suggest.dictionary value, so today a multilingual query can only
   * ever produce ONE dictionary key here. The multi-dictionary walk is written
   * to upstream's contract, not to a shape this server currently emits.
   *
   * setDictionary() is called behind upstream's own method_exists() guard
   * (:309-311): it exists only in newer search_api_autocomplete releases, and
   * this module keeps that module a soft dependency.
   *
   * A transport failure degrades to an empty suggestion list, as above.
   *
   * @param \Drupal\search_api\Query\QueryInterface $query
   *   A query representing the base search.
   * @param string $user_input
   *   The complete user input for the fulltext search keywords so far.
   * @param array $contextFilterTags
   *   Raw (unencoded) context filter tags from the plugin's configuration,
   *   e.g. ['search_api/index:my_index', 'drupal/langcode:en'].
   *
   * @return \Drupal\search_api_autocomplete\Suggestion\SuggestionInterface[]
   *   An array of autocomplete suggestions.
   */
  public function getSuggesterAutocompleteSuggestions(QueryInterface $query, string $user_input, array $contextFilterTags = []): array {
    try {
      $params = (new QueryBuilder(new FieldMapper(), $this->languageManager))
        ->buildAutocompleteSuggester($query, $user_input, $contextFilterTags);
      $response = $this->getClient()->suggest($params);
    }
    catch (SearchApiException $e) {
      return [];
    }

    $factory = new SuggestionFactory($user_input);
    $suggestions = [];
    // The outer level gets the same is_array() guard as every inner level
    // below and as the Terms walk above: `??` only substitutes for a MISSING
    // or NULL key, so a response whose 'suggest' key is present but scalar
    // would otherwise reach foreach() and raise a PHP warning. A real decoded
    // Solr/Wayfinder envelope cannot carry a scalar there, which is exactly
    // why the not-quite-the-expected-shape case must degrade to no
    // suggestions rather than surface a warning in the widget.
    $dictionaries = $response['suggest'] ?? NULL;
    foreach (is_array($dictionaries) ? $dictionaries : [] as $dictionary => $queries) {
      if (!is_array($queries)) {
        continue;
      }
      foreach ($queries as $phrases) {
        foreach ((is_array($phrases) ? $phrases['suggestions'] ?? [] : []) as $phrase) {
          if (!is_array($phrase) || !is_string($phrase['term'] ?? NULL)) {
            continue;
          }
          $suggestion = $factory->createFromSuggestedKeys($phrase['term']);
          if (method_exists($suggestion, 'setDictionary')) {
            $suggestion->setDictionary((string) $dictionary);
          }
          $suggestions[] = $suggestion;
        }
      }
    }

    return $this->filterDuplicateAutocompleteSuggestions($suggestions);
  }

  /**
   * Removes duplicate suggestions, keyed on the suggested keys.
   *
   * Mirrors search_api_solr's SolrAutocompleteBackendTrait::
   * filterDuplicateAutocompleteSuggestions() (:50-66), which both the
   * Spellcheck and the Suggester plugin call on their result list. The one
   * divergence is deliberate: upstream unsets in place and leaves holes in the
   * array's keys, this returns a re-indexed list, because every consumer here
   * treats the return value as an ordered list.
   *
   * Upstream's condition also ORs in getUrl() ("keep it if EITHER the keys or
   * the url is new"), which is dropped here because neither of these two paths
   * ever sets a url: both build their suggestions with
   * createFromSuggestedKeys(), whose url is always NULL, so the url half of
   * the test can never change the outcome. Restore it if a url-carrying
   * suggestion source is ever added.
   *
   * @param \Drupal\search_api_autocomplete\Suggestion\SuggestionInterface[] $suggestions
   *   The suggestions to filter.
   *
   * @return \Drupal\search_api_autocomplete\Suggestion\SuggestionInterface[]
   *   The suggestions, first occurrence of each suggested-keys value only.
   */
  protected function filterDuplicateAutocompleteSuggestions(array $suggestions): array {
    $seen = [];
    $filtered = [];
    foreach ($suggestions as $suggestion) {
      $keys = $suggestion->getSuggestedKeys();
      if (in_array($keys, $seen, TRUE)) {
        continue;
      }
      $seen[] = $keys;
      $filtered[] = $suggestion;
    }
    return $filtered;
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
