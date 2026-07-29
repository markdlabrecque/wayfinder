<?php
// Drives search_api_solr's real query-building code through its documented
// query API (not the UI) to exercise the feature surface listed in issue
// #55: fulltext (edismax AND/OR/negated), facets, filters (string/range/
// boolean), sorts, spellcheck. Every one of these calls fires a real HTTP
// request at Solr through the mitmproxy capture, captured verbatim.

/** @var \Drupal\search_api\IndexInterface $index */
$index = \Drupal\search_api\Entity\Index::load('capture_index');
$pmm = \Drupal::service('plugin.manager.search_api.parse_mode');

function run($label, $query) {
  try {
    $results = $query->execute();
    echo "$label: " . $results->getResultCount() . " results\n";
  } catch (\Throwable $e) {
    echo "$label: ERROR " . $e->getMessage() . "\n";
  }
}

// --- Fulltext / edismax --------------------------------------------------
$q = $index->query();
$q->setParseMode($pmm->createInstance('edismax'));
$q->keys('quick');
$q->setFulltextFields(['title', 'body']);
run('edismax_single_term', $q);

$pm = $pmm->createInstance('edismax');
$pm->setConjunction('OR');
$q = $index->query();
$q->setParseMode($pm);
$q->keys('quick rocket');
$q->setFulltextFields(['title', 'body']);
run('edismax_or_conjunction', $q);

$q = $index->query();
$q->setParseMode($pmm->createInstance('edismax'));
$q->keys('quick fox');
$q->setFulltextFields(['title', 'body']);
run('edismax_and_conjunction', $q);

$q = $index->query();
$q->setParseMode($pmm->createInstance('direct'));
$q->keys('quick');
$q->setFulltextFields(['title', 'body']);
run('direct_parse_mode', $q);

// --- Sorts -----------------------------------------------------------------
$q = $index->query();
$q->sort('field_rating', 'DESC');
run('sort_integer_desc', $q);

$q = $index->query();
$q->sort('field_sku', 'ASC');
run('sort_string_asc', $q);

$q = $index->query();
$q->sort('field_event_date', 'ASC');
run('sort_date_asc', $q);

// --- Filters -----------------------------------------------------------------
$q = $index->query();
$q->addCondition('type', 'article');
run('filter_string_eq', $q);

$q = $index->query();
$q->addCondition('field_rating', 3, '>=');
run('filter_range_gte', $q);

$q = $index->query();
$q->addCondition('field_rating', [2, 4], 'BETWEEN');
run('filter_range_between', $q);

$q = $index->query();
$q->addCondition('field_featured', TRUE);
run('filter_boolean_true', $q);

$q = $index->query();
$cg = $q->createConditionGroup('OR');
$cg->addCondition('field_keywords', 'animals');
$cg->addCondition('field_keywords', 'garden');
$q->addConditionGroup($cg);
run('filter_multivalue_or', $q);

// --- Facets ------------------------------------------------------------
$q = $index->query();
$q->range(0, 0);
$q->setOption('search_api_facets', [
  'type' => [
    'field' => 'type',
    'limit' => 10,
    'min_count' => 1,
    'missing' => FALSE,
  ],
]);
run('facet_string_field', $q);

$q = $index->query();
$q->range(0, 0);
$q->setOption('search_api_facets', [
  'field_keywords' => [
    'field' => 'field_keywords',
    'limit' => 10,
    'min_count' => 1,
    'missing' => TRUE,
  ],
]);
run('facet_multivalue_missing', $q);

// --- Spellcheck --------------------------------------------------------
$q = $index->query();
$q->setParseMode($pmm->createInstance('edismax'));
$q->keys('qwick');
$q->setFulltextFields(['title', 'body']);
$q->setOption('search_api_spellcheck', ['keys' => ['qwick'], 'collate' => TRUE]);
run('spellcheck_misspelling', $q);

// --- More Like This ------------------------------------------------------
$q = $index->query();
$q->setOption('search_api_mlt', [
  'id' => '4m8z66-capture_index-entity:node/1:en',
  'fields' => ['body'],
]);
run('more_like_this', $q);

echo "done\n";
