<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder\Plugin\search_api_autocomplete\suggester;

use Drupal\search_api\IndexInterface;
use Drupal\search_api_wayfinder\Plugin\search_api\backend\WayfinderBackend;

/**
 * Resolves an index's Wayfinder backend, for the autocomplete suggesters.
 *
 * Ported from search_api_solr_autocomplete's own BackendTrait
 * (coverage/search_api_solr_4.4.0_source/modules/search_api_solr_autocomplete/
 * src/Plugin/search_api_autocomplete/suggester/BackendTrait.php:24-41), with
 * the backend class swapped: search_api_solr tests
 * `$backend instanceof SolrBackendInterface`, we test WayfinderBackend, since
 * a Wayfinder suggester is meaningless against any other backend.
 *
 * Like every class in this directory, this file is only ever autoloaded from
 * inside an installed search_api_autocomplete (the plugin manager is what
 * discovers src/Plugin/search_api_autocomplete/), so naming that module's
 * classes here creates no hard dependency -- see the class comment on
 * Spellcheck/Suggester and composer.json's autoload-dev "_comment".
 */
trait BackendTrait {

  /**
   * Retrieves the Wayfinder backend for the given index, if it supports
   * autocomplete.
   *
   * @param \Drupal\search_api\IndexInterface $index
   *   The search index.
   *
   * @return \Drupal\search_api_wayfinder\Plugin\search_api\backend\WayfinderBackend|null
   *   The backend plugin of the index's server, if it exists, is a Wayfinder
   *   backend, and advertises the search_api_autocomplete feature; NULL
   *   otherwise.
   */
  protected static function getWayfinderBackend(IndexInterface $index): ?WayfinderBackend {
    try {
      if (
        $index->hasValidServer() &&
        ($server = $index->getServerInstance()) &&
        ($backend = $server->getBackend()) &&
        $backend instanceof WayfinderBackend &&
        $server->supportsFeature('search_api_autocomplete')
      ) {
        return $backend;
      }
    }
    catch (\Exception $e) {
      // A broken server reference must never break the search form: upstream
      // logs and falls through to NULL here as well (BackendTrait.php:36-39).
      // \Drupal is used statically rather than injected because this is a
      // static method on a plugin, exactly as upstream's is.
      \Drupal::logger('search_api_wayfinder')->error($e->getMessage());
    }
    return NULL;
  }

}
