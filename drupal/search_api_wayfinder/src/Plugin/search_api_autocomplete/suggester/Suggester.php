<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder\Plugin\search_api_autocomplete\suggester;

use Drupal\Core\Form\FormStateInterface;
use Drupal\Core\Language\LanguageInterface;
use Drupal\Core\Language\LanguageManagerInterface;
use Drupal\Core\Plugin\PluginFormInterface;
use Drupal\Core\StringTranslation\TranslatableMarkup;
use Drupal\search_api\Plugin\PluginFormTrait;
use Drupal\search_api\Query\QueryInterface;
use Drupal\search_api_autocomplete\Attribute\SearchApiAutocompleteSuggester;
use Drupal\search_api_autocomplete\SearchInterface;
use Drupal\search_api_autocomplete\Suggester\SuggesterPluginBase;
use Symfony\Component\DependencyInjection\ContainerInterface;

/**
 * Suggests complete phrases for the entered string, via Wayfinder's suggester.
 *
 * Ports search_api_solr_autocomplete's Suggester
 * (coverage/search_api_solr_4.4.0_source/modules/search_api_solr_autocomplete/
 * src/Plugin/search_api_autocomplete/suggester/Suggester.php) to this backend.
 * Upstream drives Solr's SuggestComponent through the /autocomplete request
 * handler; Wayfinder serves the same lookup on its own /suggest endpoint (#351
 * ruled out an /autocomplete route; the server-side read path is #384), so the
 * transport is QueryBuilder::buildAutocompleteSuggester() ->
 * WayfinderClient::suggest() -> WayfinderBackend::
 * getSuggesterAutocompleteSuggestions() (#385). This class is deliberately
 * thin: configuration plus the context-filter-tag assembly, nothing else.
 *
 * SOFT DEPENDENCY. search_api_autocomplete is intentionally absent from
 * search_api_wayfinder.info.yml's dependencies and from composer.json's
 * "require" (see the "_comment" in composer.json's autoload-dev). That is safe
 * precisely because of where this file lives: Drupal only ever discovers and
 * autoloads classes under src/Plugin/search_api_autocomplete/ through that
 * module's own plugin manager, which does not exist unless the module is
 * installed. So the search_api_autocomplete classes named above are never
 * resolved on a site without it.
 *
 * The plugin id is search_api_wayfinder_suggester, NOT
 * search_api_solr_suggester: this is a different backend, and reusing
 * upstream's id would collide with the real module's plugin if both are
 * installed on one site.
 */
#[SearchApiAutocompleteSuggester(
  id: 'search_api_wayfinder_suggester',
  label: new TranslatableMarkup('Wayfinder Suggester'),
  description: new TranslatableMarkup("Suggest complete phrases for the entered string based on Wayfinder's suggest component."),
)]
class Suggester extends SuggesterPluginBase implements PluginFormInterface {

  use PluginFormTrait;
  use BackendTrait;

  /**
   * The language manager.
   *
   * Only the configuration form uses it (to list the site's languages), but it
   * is a constructor dependency rather than a \Drupal::service() call for the
   * same reason upstream makes it one (Suggester.php:50-70).
   */
  protected LanguageManagerInterface $languageManager;

  /**
   * Constructs the Wayfinder suggester plugin.
   */
  public function __construct(array $configuration, $plugin_id, array $plugin_definition, LanguageManagerInterface $language_manager) {
    parent::__construct($configuration, $plugin_id, $plugin_definition);
    $this->languageManager = $language_manager;
  }

  /**
   * {@inheritdoc}
   */
  public static function create(ContainerInterface $container, array $configuration, $plugin_id, $plugin_definition) {
    return new static(
      $configuration,
      $plugin_id,
      $plugin_definition,
      $container->get('language_manager')
    );
  }

  /**
   * {@inheritdoc}
   *
   * Upstream gates on the Solr major version being >= 6, because the
   * SuggestComponent's lookup API is that new (Suggester.php:78-82). Wayfinder
   * has no version handshake to compare here -- /suggest either exists or the
   * request fails and the backend degrades to no suggestions -- so "is the
   * backend a Wayfinder backend at all" is the whole condition.
   */
  public static function supportsSearch(SearchInterface $search) {
    return (bool) static::getWayfinderBackend($search->getIndex());
  }

  /**
   * {@inheritdoc}
   *
   * Mirrors Suggester.php:87-93 minus one key.
   *
   * ponytail: upstream's 'search_api_solr/site_hash' => TRUE is absent. This
   * module indexes no site hash at all (DocumentBuilder.php:15, issue #301:
   * one core per site is the supported topology, not a temporary
   * simplification), so DocumentBuilder writes no
   * 'search_api_solr/site_hash:<hash>' context tag for a suggest.cfq to filter
   * on. The cost is that this plugin cannot offer upstream's "From this site
   * only" restriction; there is nothing to restrict on. Restoring it means
   * introducing a site hash first, index side and query side together.
   */
  public function defaultConfiguration() {
    return [
      'search_api/index' => '',
      'drupal/langcode' => 'any',
    ];
  }

  /**
   * {@inheritdoc}
   *
   * Mirrors Suggester.php:101-141 minus the site-hash checkbox (see
   * defaultConfiguration()).
   */
  public function buildConfigurationForm(array $form, FormStateInterface $form_state) {
    $search = $this->getSearch();
    $server = $search->getIndex()->getServerInstance();

    $index_options['any'] = $this->t('Any index');
    foreach ($server->getIndexes() as $index) {
      $index_options[$index->id()] = $this->t('Index @index', ['@index' => $index->label()]);
    }

    $form['search_api/index'] = [
      '#type' => 'radios',
      '#title' => $this->t('Index'),
      '#description' => $this->t('Limit the suggestion dictionary to entries to those created by a specific index.'),
      '#options' => $index_options,
      '#default_value' => $this->getConfiguration()['search_api/index'] ?: $search->getIndex()->id(),
    ];

    $langcode_options['any'] = $this->t('Any language');
    $langcode_options['multilingual'] = $this->t('Let the Wayfinder server handle it dynamically.');
    foreach ($this->languageManager->getLanguages() as $language) {
      $langcode_options[$language->getId()] = $language->getName();
    }
    $langcode_options[LanguageInterface::LANGCODE_NOT_SPECIFIED] = $this->t('Undefined');

    $form['drupal/langcode'] = [
      '#type' => 'radios',
      '#title' => $this->t('Language'),
      '#description' => $this->t('Limit the suggestion dictionary to entries that belong to a specific language.'),
      '#options' => $langcode_options,
      '#default_value' => $this->getConfiguration()['drupal/langcode'],
    ];

    return $form;
  }

  /**
   * {@inheritdoc}
   *
   * Upstream's own submit handler (Suggester.php:146-149).
   */
  public function submitConfigurationForm(array &$form, FormStateInterface $form_state) {
    $this->setConfiguration($form_state->getValues());
  }

  /**
   * {@inheritdoc}
   *
   * Mirrors Suggester.php:157-177: assemble the context filter tags from this
   * plugin's configuration in their RAW, unencoded form -- QueryBuilder
   * encodes them, matching the encoding DocumentBuilder applies to
   * sm_context_tags -- and delegate. 'any' means "add no tag at all" for both
   * options (:169-174), which is what makes the dictionary fall back to 'und'.
   *
   * $incomplete_key is unused: the suggester looks up the whole entered string
   * as an infix, so upstream passes only $user_input into the suggest query
   * too (:279, `$suggester_component->setQuery($user_input)`).
   *
   * ponytail: no 'search_api_solr/site_hash:' tag (:166-168) -- see
   * defaultConfiguration().
   */
  public function getAutocompleteSuggestions(QueryInterface $query, $incomplete_key, $user_input) {
    $backend = static::getWayfinderBackend($this->getSearch()->getIndex());
    if (!$backend) {
      return [];
    }

    $config = $this->getConfiguration();
    $tags = [];
    if (!empty($config['search_api/index']) && 'any' !== $config['search_api/index']) {
      $tags[] = 'search_api/index:' . $config['search_api/index'];
    }
    if ('any' !== ($config['drupal/langcode'] ?? 'any')) {
      $tags[] = 'drupal/langcode:' . $config['drupal/langcode'];
    }

    return $backend->getSuggesterAutocompleteSuggestions($query, (string) $user_input, $tags);
  }

}
