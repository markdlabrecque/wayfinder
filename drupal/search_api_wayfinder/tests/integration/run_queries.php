<?php
// Drives the real Search API query API (edismax fulltext) against a live
// Wayfinder instance through the standalone "wayfinder" backend
// (WayfinderBackend::search()), for issue #80's "real index+search round
// trip" acceptance item.
//
// Unlike the old worktree's run_queries.php
// (/Users/mark/Projects/wayfinder-57-search-api-wayfinder/drupal/search_api_wayfinder/tests/integration/run_queries.php),
// which ran fulltext and facet queries through the search_api_solr backend
// plus a Wayfinder connector, this standalone-backend harness exercises the
// plain fulltext round trip required for this ticket.
//
// This script exits non-zero (and prints a ROUNDTRIP: FAIL line) unless the
// node created by create_content.php comes back from a real Wayfinder core
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

// Wayfinder keeps its configured-core ping public, but all query endpoints
// require the backend's credentials. Prove both contracts before the normal
// authenticated Search API round trip below.
$unauthenticated = new \Drupal\search_api_wayfinder\WayfinderClient(
  \Drupal::service('http_client'),
  'http://wayfinder:8983/solr/content',
);
if (!$unauthenticated->ping()) {
  echo "AUTH: FAIL - unauthenticated client could not ping the public endpoint\n";
  exit(1);
}
try {
  $unauthenticated->select(['q' => '*:*']);
  echo "AUTH: FAIL - unauthenticated select unexpectedly succeeded\n";
  exit(1);
}
catch (\Drupal\search_api\SearchApiException $e) {
  if ($e->getMessage() !== 'authentication required') {
    echo "AUTH: FAIL - unauthenticated select message was: " . $e->getMessage() . "\n";
    exit(1);
  }
}
echo "AUTH: PASS - public ping and exact unauthenticated select failure verified\n";

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

if ($exit_code !== 0) {
  throw new \RuntimeException('Search API round trip failed.');
}
