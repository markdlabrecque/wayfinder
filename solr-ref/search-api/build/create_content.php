<?php
// Representative corpus for issue #55's capture: two bundles, a realistic
// field mix (fulltext, string, integer, date, boolean, multi-value).

use Drupal\node\Entity\Node;

$articles = [
  [
    'title' => 'The quick brown fox jumps over the lazy dog',
    'body' => 'A classic pangram used to test typefaces and search relevance. The fox is quick and clever.',
    'field_sku' => 'ART-001',
    'field_rating' => 5,
    'field_featured' => TRUE,
    'field_keywords' => ['animals', 'classic', 'pangram'],
    'field_event_date' => '2026-01-15',
    'sticky' => TRUE,
  ],
  [
    'title' => 'A lazy afternoon in the garden',
    'body' => 'Spent the afternoon reading in the garden. Bees buzzed lazily among the flowers.',
    'field_sku' => 'ART-002',
    'field_rating' => 3,
    'field_featured' => FALSE,
    'field_keywords' => ['garden', 'relaxation'],
    'field_event_date' => '2026-02-20',
    'sticky' => FALSE,
  ],
  [
    'title' => 'Quick thinking saves the day at the rocket launch',
    'body' => 'Engineers had to think quickly when the rocket launch sequence hit an anomaly.',
    'field_sku' => 'ART-003',
    'field_rating' => 4,
    'field_featured' => TRUE,
    'field_keywords' => ['rocket', 'engineering', 'mission'],
    'field_event_date' => '2026-03-05',
    'sticky' => FALSE,
  ],
  [
    'title' => 'Dogs and cats living together',
    'body' => 'A humorous look at household pets learning to coexist peacefully.',
    'field_sku' => 'ART-004',
    'field_rating' => 2,
    'field_featured' => FALSE,
    'field_keywords' => ['animals', 'humor'],
    'field_event_date' => '2026-04-10',
    'sticky' => FALSE,
  ],
];

foreach ($articles as $data) {
  $node = Node::create([
    'type' => 'article',
    'title' => $data['title'],
    'body' => ['value' => $data['body'], 'format' => 'basic_html'],
    'field_sku' => $data['field_sku'],
    'field_rating' => $data['field_rating'],
    'field_featured' => $data['field_featured'],
    'field_keywords' => $data['field_keywords'],
    'field_event_date' => $data['field_event_date'],
    'sticky' => $data['sticky'],
    'status' => 1,
  ]);
  $node->save();
}

$pages = [
  [
    'title' => 'About our mission',
    'body' => 'We build search infrastructure and believe in open standards and interoperability.',
    'field_priority' => 1,
    'field_published_on' => '2026-01-01',
    'field_archived' => FALSE,
    'field_topics' => ['company', 'mission'],
  ],
  [
    'title' => 'Archived legacy documentation',
    'body' => 'This page documents a legacy system that has since been retired and archived.',
    'field_priority' => 9,
    'field_published_on' => '2025-06-01',
    'field_archived' => TRUE,
    'field_topics' => ['legacy', 'documentation'],
  ],
];

foreach ($pages as $data) {
  $node = Node::create([
    'type' => 'page',
    'title' => $data['title'],
    'body' => ['value' => $data['body'], 'format' => 'basic_html'],
    'field_priority' => $data['field_priority'],
    'field_published_on' => $data['field_published_on'],
    'field_archived' => $data['field_archived'],
    'field_topics' => $data['field_topics'],
    'status' => 1,
  ]);
  $node->save();
}

echo "content created\n";
