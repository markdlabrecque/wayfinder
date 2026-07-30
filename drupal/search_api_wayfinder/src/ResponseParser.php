<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder;

use Drupal\search_api\IndexInterface;
use Drupal\search_api\Item\Item;
use Drupal\search_api\Query\QueryInterface;
use Drupal\search_api\Query\ResultSet;

/**
 * Parses a Wayfinder /select or /mlt JSON response into a populated ResultSet.
 *
 * Envelope shape (response.numFound/start/docs[] with id + score) is ground
 * truth from solr-ref/responses/edismax_score_baseline.json; /mlt uses the
 * same "response" block for its similar documents (mlt_baseline.json), so one
 * parser serves both. Facet counts and highlighting snippets are read off the
 * sibling "facet_counts"/"highlighting" blocks when present.
 */
class ResponseParser {

  public function __construct(
    private readonly FieldMapper $fieldMapper = new FieldMapper(),
  ) {}

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

    // Only pay for the reverse field-name lookup when the response actually
    // carries highlighting; NULL means "highlighting was not requested", and
    // items must then be left without the extra data entirely (callers use
    // hasExtraData() to fall back to Search API's own highlight processor).
    $highlighting = isset($response['highlighting']) && is_array($response['highlighting'])
      ? $response['highlighting']
      : NULL;
    $fieldIdByName = $highlighting === NULL ? [] : $this->fieldIdsByFieldName($index);

    $items = [];
    foreach ($body['docs'] ?? [] as $doc) {
      $docId = (string) ($doc['id'] ?? '');
      $itemId = str_starts_with($docId, $prefix) ? substr($docId, strlen($prefix)) : $docId;

      $item = new Item($index, $itemId);
      $item->setScore((float) ($doc['score'] ?? 1.0));

      if ($highlighting !== NULL) {
        $snippets = $this->highlightedFields($highlighting[$docId] ?? [], $fieldIdByName);
        if ($snippets !== []) {
          $item->setExtraData('highlighted_fields', $snippets);
        }
      }

      $items[$itemId] = $item;
    }

    $resultSet->setResultItems($items);

    $facets = $this->parseFacets($response, $query);
    if ($facets !== NULL) {
      $resultSet->setExtraData('search_api_facets', $facets);
    }

    return $resultSet;
  }

  /**
   * Translates facet_counts.facet_fields into search_api_facets extra data.
   *
   * facet_counts.facet_fields is Solr's default flat array-pairs shape
   * (["term", count, "term", count, ...], no json.nl sent) -- ground truth
   * from solr-ref/responses/facet_basic.json and facet_missing.json, where the
   * missing bucket is a literal null key appended last.
   *
   * The extra-data shape ([$delta => [['filter' => string, 'count' => int],
   * ...]]) is what the contrib facets module's query-type plugins read; the
   * missing bucket's filter is the literal '!' string SearchApiString::build()
   * checks for.
   *
   * @return array<string, array<int, array{filter: string, count: int}>>|null
   *   NULL when the query requested no facets, so no extra data is set at all.
   */
  private function parseFacets(array $response, QueryInterface $query): ?array {
    $requested = $query->getOption('search_api_facets') ?: [];
    if (!is_array($requested) || $requested === []) {
      return NULL;
    }

    $index = $query->getIndex();

    // Wayfinder keys facet_fields by the mapped dynamic field name, the same
    // name QueryBuilder emitted in facet.field, so map back to the deltas.
    //
    // ponytail: two deltas facetting the same field collapse -- facet.field is
    // sent twice, the core answers with one key, and only the last delta gets
    // parsed results. Distinguishing them needs per-facet tagging Wayfinder
    // has no wire support for.
    $deltaByFieldName = [];
    foreach ($requested as $delta => $facet) {
      $fieldId = $facet['field'] ?? NULL;
      if (!is_string($fieldId) || !($field = $index->getField($fieldId))) {
        continue;
      }
      $fieldName = $this->fieldMapper->fieldName($fieldId, $field->getType(), $this->fieldMapper->isMultiValued($field));
      $deltaByFieldName[$fieldName] = $delta;
    }

    $facets = [];
    foreach ($response['facet_counts']['facet_fields'] ?? [] as $fieldName => $pairs) {
      if (!isset($deltaByFieldName[$fieldName]) || !is_array($pairs)) {
        continue;
      }
      $terms = [];
      $values = array_values($pairs);
      for ($i = 0; $i + 1 < count($values); $i += 2) {
        $terms[] = [
          'count' => (int) $values[$i + 1],
          // Terms are double-quoted and the missing bucket is the bare '!'
          // sentinel: that is the extra-data contract Search API's own
          // conformance suite asserts against (BackendTestBase::checkFacets(),
          // e.g. ['count' => 2, 'filter' => '"article_category"']), and it
          // compares the raw array, before any downstream unquoting.
          'filter' => $values[$i] === NULL ? '!' : '"' . (string) $values[$i] . '"',
        ];
      }
      $facets[$deltaByFieldName[$fieldName]] = $terms;
    }

    return $facets;
  }

  /**
   * Translates one document's highlighting entry into the per-item
   * "highlighted_fields" extra-data shape.
   *
   * The response block is keyed by the *mapped* dynamic field name
   * ('ts_body'), the same name QueryBuilder emitted in hl.fl -- shape ground
   * truth solr-ref/responses/hl_basic.json, {docId: {fieldName: [snippet,
   * ...]}} -- whereas ItemInterface::getExtraData() documents
   * "highlighted_fields" as "an array, keyed by field IDs". So map back.
   *
   * Field names with no Search API field behind them (Wayfinder-internal
   * fields, or fields dropped from the index since the query ran) are
   * skipped rather than leaked through under their raw dynamic name.
   *
   * @param array<string, mixed> $entry
   * @param array<string, string> $fieldIdByName
   *
   * @return array<string, array<int, string>>
   */
  private function highlightedFields(array $entry, array $fieldIdByName): array {
    $highlighted = [];
    foreach ($entry as $fieldName => $snippets) {
      if (!isset($fieldIdByName[$fieldName]) || !is_array($snippets)) {
        continue;
      }
      $highlighted[$fieldIdByName[$fieldName]] = array_values(array_map('strval', $snippets));
    }
    return $highlighted;
  }

  /**
   * Builds the reverse lookup from Wayfinder dynamic field name back to
   * Search API field id, using the same FieldMapper mapping QueryBuilder
   * used on the way out.
   *
   * @return array<string, string>
   */
  private function fieldIdsByFieldName(IndexInterface $index): array {
    $map = [];
    foreach ($index->getFields() as $fieldId => $field) {
      $fieldId = (string) $fieldId;
      $fieldName = $this->fieldMapper->fieldName($fieldId, $field->getType(), $this->fieldMapper->isMultiValued($field));
      $map[$fieldName] = $fieldId;
    }
    return $map;
  }

}
