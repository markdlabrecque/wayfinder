<?php
// Drives the real Search API query API (edismax fulltext) against a live
// Wayfinder instance through the standalone "wayfinder" backend
// (WayfinderBackend::search()), for issue #80's "real index+search round
// trip" acceptance item.
//
// Unlike the old worktree's run_queries.php
// (/Users/mark/Projects/wayfinder-57-search-api-wayfinder/drupal/search_api_wayfinder/tests/integration/run_queries.php,
// which ran a fulltext query AND a facet query through the search_api_solr
// backend + wayfinder connector), this version only runs the plain fulltext
// query: M1 (see WayfinderBackend::getSupportedFeatures(), currently `[]`)
// has not implemented facets yet, so a facet query here would not be
// exercising this ticket's scope -- it would just be asserting on a
// not-yet-built feature. Facets get their own round trip in M3 (#57 plan
// doc, "search_api_facets" contrib module) once QueryBuilder emits
// `facet.field` and ResponseParser parses `facet_counts`.
//
// KNOWN BLOCKER (#84): this currently fails, but not because of anything in
// search_api_wayfinder. Indexing succeeds; the search step 400s because
// Wayfinder core's edismax `qf`/`pf` resolution (CoreIndex::
// resolve_field_weights, src/core_index.rs) only resolves statically
// declared fields, never a `[[dynamic_fields]]` pattern match like
// `ts_title` -- unlike the `q` text path, which does rewrite dynamic names
// via rewrite_dynamic_fields(). Since search_api_solr's field-naming
// convention (and this repo's own presets/search-api.toml) is entirely
// dynamic-field-based, `defType=edismax` against real Drupal `qf` values is
// unusable until #84 lands. Re-run this harness once #84 closes; it should
// go green with no changes needed here.
//
// This script is the harness's actual "red" assertion for issue #80: it
// exits non-zero (and prints a ROUNDTRIP: FAIL line) unless the node
// created by create_content.php comes back from a real Wayfinder core
// through WayfinderBackend::search(), so run.sh can fail loudly instead of
// silently reporting "0 results" as if that were fine.

use Drupal\search_api\Entity\Index;

$index = Index::load('wf80_index');
if (!$index) {
  echo "ROUNDTRIP: FAIL - index 'wf80_index' does not exist (setup_server_index.php did not run or failed)\n";
  exit(1);
}

$pmm = \Drupal::service('plugin.manager.search_api.parse_mode');

$exit_code = 0;

try {
  $query = $index->query();
  $query->setParseMode($pmm->createInstance('terms'));
  $query->keys('wayfinderroundtrip');
  $query->setFulltextFields(['title', 'body']);
  $results = $query->execute();

  $count = $results->getResultCount();
  echo "fulltext_wayfinderroundtrip: $count results\n";

  $found_target = FALSE;
  foreach ($results->getResultItems() as $item) {
    echo "  result item id: " . $item->getId() . "\n";
    $found_target = TRUE;
  }

  if ($count < 1 || !$found_target) {
    echo "ROUNDTRIP: FAIL - expected at least 1 result for 'wayfinderroundtrip', got $count\n";
    $exit_code = 1;
  }
  else {
    echo "ROUNDTRIP: PASS - real index+search round trip through WayfinderBackend::search() succeeded\n";
  }
}
catch (\Throwable $e) {
  echo "ROUNDTRIP: FAIL - " . get_class($e) . ": " . $e->getMessage() . "\n";
  echo $e->getTraceAsString() . "\n";
  $exit_code = 1;
}

exit($exit_code);
