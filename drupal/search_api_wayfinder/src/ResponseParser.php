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

    [$docs, $count] = $this->extractResultDocs($response, $query);
    $resultSet->setResultCount($count);

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
    foreach ($docs as $doc) {
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
   * Pulls the flat docs list and the result count out of the response.
   *
   * For a normal `/select` this is `response.docs` / `response.numFound`. For
   * a grouped response (issue #290, `search_api_grouping.use_grouping`) the
   * server returns a `grouped` block INSTEAD of `response` (src/lib.rs), so
   * the docs are flattened out of each group's `doclist.docs` -- mirroring
   * search_api_solr's extractResult half (coverage/
   * search_api_solr_4.4.0_source ... SearchApiSolrBackend.php:2962-2987).
   *
   * `grouped` is keyed by the group.field value QueryBuilder emitted, i.e. the
   * mapped fast field name, so the same FieldMapper mapping resolves the key
   * here. The count is ngroups for the single-field case (the realistic one);
   * multi-field grouping has no single group count, and search_api_solr falls
   * back to count($block) (the block's key count) -- mirrored for parity.
   *
   * @return array{0: array<int, array>, 1: int}
   *   [docs, resultCount].
   */
  private function extractResultDocs(array $response, QueryInterface $query): array {
    $grouping = $query->getOption('search_api_grouping');
    if (!is_array($grouping) || empty($grouping['use_grouping'])) {
      $body = $response['response'] ?? ['numFound' => 0, 'docs' => []];
      return [$body['docs'] ?? [], (int) ($body['numFound'] ?? 0)];
    }

    $index = $query->getIndex();
    $fields = $grouping['fields'] ?? [];

    // Map each requested field id once to the name that keys its grouped
    // block, skipping anything not on the index (QueryBuilder skipped text /
    // multi-valued before emitting group.field, so only resolvable fields are
    // present on the wire, but the request may name a field the index lacks).
    $fieldNames = [];
    foreach ($fields as $fieldId) {
      $field = is_string($fieldId) ? $index->getField($fieldId) : NULL;
      if ($field) {
        $fieldNames[$fieldId] = $this->fieldMapper->sortFieldName($fieldId, $field->getType(), $this->fieldMapper->isMultiValued($field));
      }
    }

    $docs = [];
    $count = 0;
    foreach ($fieldNames as $fieldName) {
      $block = $response['grouped'][$fieldName] ?? NULL;
      if (!is_array($block)) {
        continue;
      }
      // count($block) matches search_api_solr's multi-field fall-back
      // (matches/ngroups/groups key count); the single-field override below
      // corrects it to ngroups.
      $count = count($block);
      foreach ($block['groups'] ?? [] as $group) {
        foreach ($group['doclist']['docs'] ?? [] as $doc) {
          $docs[] = $doc;
        }
      }
    }

    // Single-field grouping: the result count is the number of GROUPS
    // (ngroups), not the number of documents -- so a paged view collapses
    // many docs per group and still paginates by group.
    if (count($fieldNames) === 1) {
      $fieldName = reset($fieldNames);
      if (isset($response['grouped'][$fieldName]['ngroups'])) {
        $count = (int) $response['grouped'][$fieldName]['ngroups'];
      }
    }

    return [$docs, $count];
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

    // QueryBuilder emits each facet under {!key=<delta>}<field>, so the core
    // labels that facet's buckets with the delta and facet_fields is keyed by
    // it -- two facets on one field thus answer under two distinct keys
    // (src/facet.rs split_facet_key,
    // solr-ref/responses/facet_extag_both_facets.json). A delta that is not a
    // safe local-params value falls back to the bare field name there, so
    // register the delta AND the mapped field name as keys for the same
    // delta: the normal response is delta-keyed, the fallback is
    // field-name-keyed, and only one of the two ever appears.
    $deltaByKey = [];
    foreach ($requested as $delta => $facet) {
      $fieldId = $facet['field'] ?? NULL;
      if (!is_string($fieldId) || !($field = $index->getField($fieldId))) {
        continue;
      }
      $fieldName = $this->fieldMapper->fieldName($fieldId, $field->getType(), $this->fieldMapper->isMultiValued($field));
      $deltaByKey[$delta] = $delta;
      $deltaByKey[$fieldName] = $delta;
    }

    $facets = [];
    foreach ($response['facet_counts']['facet_fields'] ?? [] as $key => $pairs) {
      if (!isset($deltaByKey[$key]) || !is_array($pairs)) {
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
      $facets[$deltaByKey[$key]] = $terms;
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
