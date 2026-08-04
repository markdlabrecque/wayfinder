<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder;

use Drupal\Core\Language\LanguageManagerInterface;
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

  private readonly LanguageResolver $languageResolver;

  /**
   * The language manager is optional, exactly as in QueryBuilder: without it
   * (a plain `new ResponseParser()`) the resolved set comes from the query's
   * own search_api_language condition, or falls back to 'und'.
   */
  public function __construct(
    private readonly FieldMapper $fieldMapper = new FieldMapper(),
    ?LanguageManagerInterface $languageManager = NULL,
  ) {
    $this->languageResolver = new LanguageResolver($languageManager);
  }

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
    $fieldNamesById = $highlighting === NULL ? [] : $this->fieldNamesByFieldId($index, $query);

    $items = [];
    foreach ($docs as $doc) {
      $docId = (string) ($doc['id'] ?? '');
      $itemId = str_starts_with($docId, $prefix) ? substr($docId, strlen($prefix)) : $docId;

      $item = new Item($index, $itemId);
      $item->setScore((float) ($doc['score'] ?? 1.0));

      if ($highlighting !== NULL) {
        $snippets = $this->highlightedFields($highlighting[$docId] ?? [], $fieldNamesById);
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

    $spellcheck = $this->parseSpellcheck($response);
    if ($spellcheck !== NULL) {
      $resultSet->setExtraData('search_api_spellcheck', $spellcheck);
    }

    return $resultSet;
  }

  /**
   * Translates the response's `spellcheck` block into `search_api_spellcheck`
   * extra data (issue #342).
   *
   * The client-side contract is search_api_solr's
   * SolrSpellcheckBackendTrait.php:24-42 plus
   * SearchApiSolrBackend.php:2022-2034:
   * ['suggestions' => [<original term> => [<word>, ...]], 'collation' =>
   * <string>]. A term whose suggestion list is empty is dropped entirely --
   * the trait's own `if ($keys)` guard -- and `collation` is a single string,
   * the first collation only, so it is absent when none was returned.
   *
   * The wire shape read here is the FLAT named-list form: `suggestions` is an
   * interleaved [term, {...}, term, {...}] list and `collations` an
   * interleaved ['collation', <string>, ...] one. That is ground truth from
   * solr-ref/responses/spellcheck_flat.json, and it is the form this module
   * actually receives: only json.nl=map produces the object form
   * (spellcheck_map.json), and neither QueryBuilder nor WayfinderClient ever
   * sends json.nl (the same reasoning as the Terms response in
   * WayfinderBackend::getAutocompleteSuggestions(), finding 142).
   *
   * @return array{suggestions: array<string, array<int, string>>,
   *   collation?: string}|null
   *   NULL when the response carries no spellcheck block at all, so no extra
   *   data is set -- the same "absent means absent" convention as
   *   search_api_facets and highlighted_fields.
   */
  private function parseSpellcheck(array $response): ?array {
    $spellcheck = $response['spellcheck'] ?? NULL;
    if (!is_array($spellcheck)) {
      return NULL;
    }

    $suggestions = [];
    $flat = array_values(is_array($spellcheck['suggestions'] ?? NULL) ? $spellcheck['suggestions'] : []);
    for ($i = 0, $n = count($flat); $i + 1 < $n; $i += 2) {
      $term = $flat[$i];
      $info = $flat[$i + 1];
      if (!is_string($term) || !is_array($info)) {
        continue;
      }
      $words = array_values(array_map('strval', array_filter(
        (array) ($info['suggestion'] ?? []),
        static fn ($word): bool => is_scalar($word)
      )));
      // The trait drops a term it has no correction for rather than passing
      // an empty list on to the client.
      if ($words === []) {
        continue;
      }
      $suggestions[$term] = $words;
    }

    $data = ['suggestions' => $suggestions];

    // collations is the same interleaved form: ['collation', <string>, ...],
    // one pair per collation when spellcheck.maxCollations > 1. Only the
    // first surfaces, because the client contract is a single string.
    $collations = array_values(is_array($spellcheck['collations'] ?? NULL) ? $spellcheck['collations'] : []);
    for ($i = 0, $n = count($collations); $i + 1 < $n; $i += 2) {
      if ($collations[$i] === 'collation' && is_scalar($collations[$i + 1])) {
        $data['collation'] = (string) $collations[$i + 1];
        break;
      }
    }

    return $data;
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
   * issue #342: a text field can answer under several names (one per
   * language), so the lookup is driven by the field ids and their ordered
   * name variants, not by the response's own key order: the FIRST resolved
   * language present on the doc wins, whatever order the response lists its
   * keys in.
   *
   * @param array<string, mixed> $entry
   * @param array<string, array<int, string>> $fieldNamesById
   *
   * @return array<string, array<int, string>>
   */
  private function highlightedFields(array $entry, array $fieldNamesById): array {
    $highlighted = [];
    foreach ($fieldNamesById as $fieldId => $fieldNames) {
      foreach ($fieldNames as $fieldName) {
        if (!isset($entry[$fieldName]) || !is_array($entry[$fieldName])) {
          continue;
        }
        $highlighted[$fieldId] = array_values(array_map('strval', $entry[$fieldName]));
        break;
      }
    }
    return $highlighted;
  }

  /**
   * Builds the reverse lookup from Wayfinder dynamic field name back to
   * Search API field id, using the same FieldMapper mapping QueryBuilder
   * used on the way out.
   *
   * issue #342: a text field answers under one name per language, and a
   * document carries whichever variant it was indexed in, so each field id
   * maps to a LIST of names -- the query's resolved languages in order, then
   * 'und' as a final fall-back, which is the language-unspecific variant
   * search_api_solr itself falls back to
   * (SearchApiSolrBackend.php:2582-2585). Non-text fields keep their single
   * language-free name.
   *
   * @return array<string, array<int, string>>
   */
  private function fieldNamesByFieldId(IndexInterface $index, QueryInterface $query): array {
    $languages = $this->languageResolver->resolve($query);
    if (!in_array(FieldMapper::LANGUAGE_UNSPECIFIED, $languages, TRUE)) {
      $languages[] = FieldMapper::LANGUAGE_UNSPECIFIED;
    }

    $map = [];
    foreach ($index->getFields() as $fieldId => $field) {
      $fieldId = (string) $fieldId;
      $type = $field->getType();
      $multiValued = $this->fieldMapper->isMultiValued($field);
      if (!$this->fieldMapper->isLanguageSpecificTextType($type)) {
        $map[$fieldId] = [$this->fieldMapper->fieldName($fieldId, $type, $multiValued)];
        continue;
      }
      $map[$fieldId] = array_map(
        fn (string $language): string => $this->fieldMapper->fieldName($fieldId, $type, $multiValued, $language),
        $languages
      );
    }
    return $map;
  }

}
