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

// Issue #262 tracer: an attachment whose text exists NOWHERE else in the
// corpus, so a fulltext hit for `wayfinderattachment262` can only come from
// /update/extract having indexed the file's contents. file_save_data() writes
// to the public stream wrapper and returns a managed File entity, which the
// node's field_attachments (created in setup_server_index.php) references.
$attachment_text = "This text lives only inside the attached file. "
  . "The searchable token is wayfinderattachment262.";
// Drupal 11 dropped the procedural file_save_data(); the file_system service
// + a managed File entity is the version-stable way to produce the fixture.
$uri = \Drupal::service('file_system')->saveData(
  $attachment_text,
  'public://wayfinder-attachment-262.txt',
  \Drupal\Core\File\FileSystemInterface::EXISTS_REPLACE,
);
if ($uri === FALSE) {
  throw new \RuntimeException('Failed to write the attachment fixture file.');
}
$file = \Drupal\file\Entity\File::create([
  'uri' => $uri,
  'status' => 1,
  'uid' => 1,
]);
$file->save();
echo "attachment file id: " . $file->id() . " (uri $uri)\n";

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

// The attachment node: its title/body deliberately do NOT contain the token,
// so the only way the token can be found by search is through the extracted
// file content. This is the end of the #262 vertical slice.
$attached = Node::create([
  'type' => 'article',
  'title' => 'A report attached, nothing searchable in the title',
  'body' => [
    'value' => 'The body is innocuous prose. The searchable content is in the attachment only.',
    'format' => 'basic_html',
  ],
  'field_attachments' => [
    ['target_id' => $file->id()],
  ],
  'status' => 1,
]);
$attached->save();
echo "attachment node id: " . $attached->id() . " (file id " . $file->id() . ")\n";

echo "content created\n";
