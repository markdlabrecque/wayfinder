<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder;

use Drupal\search_api\Item\Item;
use Drupal\search_api\Query\QueryInterface;
use Drupal\search_api\Query\ResultSet;

/**
 * Parses a Wayfinder /select JSON response into a populated ResultSet.
 *
 * Envelope shape (response.numFound/start/docs[] with id + score) is ground
 * truth from solr-ref/responses/edismax_score_baseline.json. No facets or
 * highlighting parsing in M1 (plan doc milestone table: M3/M4).
 */
class ResponseParser {

  /**
   * Parses a decoded /select response body into the query's existing
   * ResultSet.
   *
   * Query::execute() (vendor/drupal/search_api) already created and holds a
   * ResultSet via $query->getResults(); BackendSpecificInterface::search() is
   * void, so the backend must populate that object in place rather than
   * constructing/returning a new one.
   */
  public function parse(array $response, QueryInterface $query): ResultSet {
    $resultSet = $query->getResults();

    $body = $response['response'] ?? ['numFound' => 0, 'docs' => []];
    $resultSet->setResultCount((int) ($body['numFound'] ?? 0));

    $index = $query->getIndex();
    $indexId = $index->id();
    $prefix = $indexId . '-';

    $items = [];
    foreach ($body['docs'] ?? [] as $doc) {
      $docId = (string) ($doc['id'] ?? '');
      $itemId = str_starts_with($docId, $prefix) ? substr($docId, strlen($prefix)) : $docId;

      $item = new Item($index, $itemId);
      $item->setScore((float) ($doc['score'] ?? 1.0));

      $items[$itemId] = $item;
    }

    $resultSet->setResultItems($items);

    return $resultSet;
  }

}
