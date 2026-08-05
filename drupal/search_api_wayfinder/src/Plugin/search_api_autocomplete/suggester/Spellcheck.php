<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder\Plugin\search_api_autocomplete\suggester;

use Drupal\Core\Form\FormStateInterface;
use Drupal\Core\Plugin\PluginFormInterface;
use Drupal\Core\StringTranslation\TranslatableMarkup;
use Drupal\search_api\Plugin\PluginFormTrait;
use Drupal\search_api\Query\QueryInterface;
use Drupal\search_api_autocomplete\Attribute\SearchApiAutocompleteSuggester;
use Drupal\search_api_autocomplete\SearchInterface;
use Drupal\search_api_autocomplete\Suggester\SuggesterPluginBase;

/**
 * Suggests corrections for the entered words, via Wayfinder's spellchecker.
 *
 * Ports search_api_solr_autocomplete's Spellcheck suggester
 * (coverage/search_api_solr_4.4.0_source/modules/search_api_solr_autocomplete/
 * src/Plugin/search_api_autocomplete/suggester/Spellcheck.php) to this
 * backend. Stock search_api_solr sends the spellcheck component to Solr's
 * /autocomplete request handler; Wayfinder has no such route and will not get
 * one (#351), so the transport is a plain /select carrying spellcheck.* --
 * built by QueryBuilder::buildAutocompleteSpellcheck(), executed and decoded
 * by WayfinderBackend::getSpellcheckAutocompleteSuggestions() (#385). This
 * class is deliberately thin, mirroring the split the Terms path already uses.
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
 * The plugin id is search_api_wayfinder_spellcheck, NOT
 * search_api_solr_spellcheck: this is a different backend, and reusing
 * upstream's id would collide with the real module's plugin if both are
 * installed on one site.
 */
#[SearchApiAutocompleteSuggester(
  id: 'search_api_wayfinder_spellcheck',
  label: new TranslatableMarkup('Wayfinder Spellcheck'),
  description: new TranslatableMarkup("Suggest corrections for the entered words based on Wayfinder's spellcheck component. Note: Be careful when activating this feature if you run multiple indexes in one Wayfinder core! The spellcheck component is not able to distinguish between the different indexes and returns suggestions for the complete core. If you run multiple indexes in one core you might get suggestions that lead to zero results on a specific index!"),
)]
class Spellcheck extends SuggesterPluginBase implements PluginFormInterface {

  use PluginFormTrait;
  use BackendTrait;

  /**
   * {@inheritdoc}
   *
   * Upstream gates on the Solr major version being >= 4
   * (Spellcheck.php:48-52); there is no Wayfinder equivalent of that check --
   * every Wayfinder version this module supports serves spellcheck.* on
   * /select -- so "is the backend a Wayfinder backend at all" is the whole
   * condition.
   */
  public static function supportsSearch(SearchInterface $search) {
    return (bool) static::getWayfinderBackend($search->getIndex());
  }

  /**
   * {@inheritdoc}
   *
   * No configuration, exactly as upstream (Spellcheck.php:57-59).
   */
  public function defaultConfiguration() {
    return [];
  }

  /**
   * {@inheritdoc}
   *
   * No configuration form, exactly as upstream (Spellcheck.php:64-66).
   */
  public function buildConfigurationForm(array $form, FormStateInterface $form_state) {
    return [];
  }

  /**
   * {@inheritdoc}
   *
   * Mirrors Spellcheck.php:74-82: no backend means no suggestions, and the
   * work itself is delegated. $incomplete_key is unused because the spellcheck
   * component corrects the complete user input rather than completing a
   * prefix -- upstream likewise passes only $user_input into
   * setAutocompleteSpellCheckQuery() (:142-147, `'keys' => [$user_input]`).
   */
  public function getAutocompleteSuggestions(QueryInterface $query, $incomplete_key, $user_input) {
    $backend = static::getWayfinderBackend($this->getSearch()->getIndex());
    if (!$backend) {
      return [];
    }

    return $backend->getSpellcheckAutocompleteSuggestions($query, (string) $user_input);
  }

}
