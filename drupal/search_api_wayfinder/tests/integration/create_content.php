<?php
// Minimal corpus for issue #80's index+fulltext round-trip check: one node
// with a distinctive title/body term ("wayfinderroundtrip") that cannot
// collide with default Drupal standard-profile content, plus a couple of
// filler nodes so the query is proven to be selective, not just "anything
// in the index comes back". Adapted (not copied verbatim -- trimmed to the
// M1 plain-fulltext scope, no facet-bearing bundle split needed) from the
// old worktree's create_content.php
// (/Users/mark/Projects/wayfinder-57-search-api-wayfinder/drupal/search_api_wayfinder/tests/integration/create_content.php).

use Drupal\node\Entity\Node;

$target = Node::create([
  'type' => 'article',
  'title' => 'The wayfinderroundtrip beacon guides lost travellers',
  'body' => [
    'value' => 'This node exists solely to prove the search_api_wayfinder round trip: index it into a real Wayfinder core, search for wayfinderroundtrip, get this node back.',
    'format' => 'basic_html',
  ],
  'status' => 1,
]);
$target->save();
echo "target node id: " . $target->id() . "\n";

$fillers = [
  ['title' => 'A lazy afternoon in the garden', 'body' => 'Spent the afternoon reading in the garden.'],
  ['title' => 'About our mission', 'body' => 'We build search infrastructure and believe in open standards.'],
];
foreach ($fillers as $data) {
  Node::create([
    'type' => 'article',
    'title' => $data['title'],
    'body' => ['value' => $data['body'], 'format' => 'basic_html'],
    'status' => 1,
  ])->save();
}

echo "content created\n";
