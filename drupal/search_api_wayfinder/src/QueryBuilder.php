<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder;

use Drupal\search_api\IndexInterface;
use Drupal\search_api\Query\ConditionGroupInterface;
use Drupal\search_api\Query\ConditionInterface;
use Drupal\search_api\Query\QueryInterface;

/**
 * Translates a Search API QueryInterface into Wayfinder /select params.
 *
 * Fulltext keys become q/qf/defType. The index filter is core
 * multi-index-per-core wiring; Search API condition groups, sorts, and paging
 * are translated to the corresponding Solr-wire select parameters.
 */
class QueryBuilder {

  public function __construct(
    private readonly FieldMapper $fieldMapper = new FieldMapper(),
  ) {}

  /**
   * Builds the /select param array for the given query.
   *
   * @param bool $highlighting
   *   Whether to request highlighting. This is an explicit argument rather
   *   than something read off the query: Search API core's own "highlight"
   *   processor never touches the query object (it only reads back the
   *   "highlighted_fields" extra data a backend already populated), so there
   *   is no per-query hook to key off. search_api_solr's convention -- which
   *   plan doc locked decision 6 pins this module to -- is a backend-level
   *   config setting, so the backend reads its own configuration and passes
   *   the flag in.
   *
   * @return array<string, string|int|array<int, string>>
   */
  public function build(QueryInterface $query, bool $highlighting = FALSE): array {
    $index = $query->getIndex();
    $params = [];

    $keys = $query->getKeys();
    if ($keys === NULL) {
      $params['q'] = '*:*';
    }
    else {
      $params['q'] = $this->flattenKeys($keys);
      $params['defType'] = 'edismax';
      $params['qf'] = $this->buildQf($query, $index);
    }

    $filters = [$this->indexScopeFilter($index)];
    $conditions = $query->getConditionGroup();
    if (!$conditions->isEmpty()) {
      if ($conditions->getConjunction() === 'AND') {
        foreach ($conditions->getConditions() as $condition) {
          $filters[] = $this->buildConditionMember($condition, $index, $condition instanceof ConditionGroupInterface);
        }
      }
      else {
        $filters[] = $this->buildConditionGroup($conditions, $index, FALSE);
      }
    }
    $params['fq'] = count($filters) === 1 ? $filters[0] : $filters;

    $params += $this->buildFacets($query, $index);

    if ($highlighting) {
      $params += $this->buildHighlighting($query, $index);
    }

    $sort = $this->buildSort($query, $index);
    if ($sort !== '') {
      $params['sort'] = $sort;
    }

    $params += $this->buildPaging($query);

    return $params;
  }

  /**
   * Builds the /mlt param array for the given query.
   *
   * The 'search_api_mlt' option's shape is
   * ['id' => <search api item id>, 'fields' => <array of SA field ids>] --
   * that is what \Drupal\search_api\Plugin\views\argument\
   * SearchApiMoreLikeThis::query() sets, the only place core writes it.
   *
   * The seed document is looked up by the same '{index_id}-{item_id}'
   * composite id DocumentBuilder indexes under (locked decision 2), phrase-
   * quoted and escaped through FieldMapper::filterValue() like every other
   * value this class emits: item ids are datasource-derived, not
   * machine-name-constrained, so one containing '"' or '\' would otherwise
   * break out of the quoted phrase.
   *
   * mlt.fl is comma-joined, not space-joined like qf -- Solr's captured
   * convention (solr-ref/responses/mlt_baseline.json: 'mlt.fl=body,category').
   *
   * The similar-docs result set is scoped to this index with the same
   * index_id:"<id>" fq build() seeds (locked decision 2, core
   * multi-index-per-core wiring), via a shared helper. The server honours fq
   * on /mlt for the result set only -- it never filters which document q
   * resolves as the seed (finding 98; fixtures mlt_fq_scope /
   * mlt_fq_seed_not_filtered / mlt_fq_multiple_and) -- so on a core holding
   * more than one index this keeps MLT from returning documents from a
   * sibling index.
   *
   * @return array<string, string|int|array<int, string>>
   */
  public function buildMlt(QueryInterface $query): array {
    $index = $query->getIndex();
    $option = $query->getOption('search_api_mlt');
    if (!is_array($option) || !isset($option['id'])) {
      throw new \InvalidArgumentException('The search_api_mlt option must provide a seed item id.');
    }

    $params = [
      'q' => 'id:' . $this->fieldMapper->filterValue($index->id() . '-' . $option['id'], 'string'),
      'mlt.fl' => implode(',', $this->mapFieldNames((array) ($option['fields'] ?? []), $index)),
      'fq' => $this->indexScopeFilter($index),
    ];

    return $params + $this->buildPaging($query);
  }

  /**
   * The index scope filter both build() and buildMlt() seed their fq with
   * (locked decision 2, core multi-index-per-core wiring). Shared so the two
   * call sites cannot drift apart.
   */
  private function indexScopeFilter(IndexInterface $index): string {
    return 'index_id:"' . $index->id() . '"';
  }

  /**
   * Builds the start/rows params from the query's offset/limit options.
   *
   * @return array<string, int>
   */
  private function buildPaging(QueryInterface $query): array {
    $params = [];

    $offset = $query->getOption('offset');
    if ($offset !== NULL) {
      $params['start'] = (int) $offset;
    }
    $limit = $query->getOption('limit');
    // Search API reserves -1 as its unlimited-results sentinel. Wayfinder
    // clamps oversized positive rows to its configured rows_limit, so send a
    // parseable maximum rather than omitting rows and falling back to 10.
    if ((int) $limit === -1) {
      $params['rows'] = PHP_INT_MAX;
    }
    elseif ($limit !== NULL) {
      $params['rows'] = (int) $limit;
    }

    return $params;
  }

  /**
   * Builds the hl/hl.fl params over the same fulltext field set as qf.
   *
   * hl.fl is comma-joined, matching the captured fixture convention
   * (solr-ref/responses/hl_multi_field_comma.json: 'hl.fl=body,category').
   *
   * @return array<string, string>
   */
  private function buildHighlighting(QueryInterface $query, IndexInterface $index): array {
    return [
      'hl' => 'true',
      'hl.fl' => implode(',', $this->mapFieldNames($this->fulltextFieldIds($query, $index), $index)),
    ];
  }

  /**
   * Maps Search API field ids to their Wayfinder dynamic field names,
   * skipping ids that are not part of the index.
   *
   * @param array<int|string, string> $fieldIds
   *
   * @return array<int, string>
   */
  private function mapFieldNames(array $fieldIds, IndexInterface $index): array {
    $names = [];
    foreach ($fieldIds as $fieldId) {
      $field = $index->getField($fieldId);
      if (!$field) {
        continue;
      }
      $names[] = $this->fieldMapper->fieldName($fieldId, $field->getType(), $this->fieldMapper->isMultiValued($field));
    }
    return $names;
  }

  /**
   * Resolves the query's fulltext fields against the index's, preserving the
   * index's field order.
   *
   * @return array<int|string, string>
   */
  private function fulltextFieldIds(QueryInterface $query, IndexInterface $index): array {
    $queryFulltextFields = $query->getFulltextFields();
    $indexFulltextFields = $index->getFulltextFields();

    return $queryFulltextFields === NULL
      ? $indexFulltextFields
      : array_intersect($indexFulltextFields, $queryFulltextFields);
  }

  /**
   * Flattens a Search API parsed-keys array into a plain fulltext query
   * string.
   *
   * @param array|string $keys
   *   Either a single term string, or a nested parsed-keys array with
   *   '#conjunction' ('AND'/'OR') and optional '#negation' (bool) keys.
   */
  private function flattenKeys($keys, bool $nested = FALSE): string {
    if (is_string($keys)) {
      return $this->escapeTerm($keys);
    }

    $conjunction = $keys['#conjunction'] ?? 'AND';
    $negated = !empty($keys['#negation']);

    $parts = [];
    foreach ($keys as $key => $value) {
      if ($key === '#conjunction' || $key === '#negation') {
        continue;
      }
      $flattened = $this->flattenKeys($value, TRUE);
      if ($flattened !== '') {
        $parts[] = $flattened;
      }
    }

    $combined = implode($conjunction === 'OR' ? ' OR ' : ' ', $parts);
    // Nested groups and any multi-term negation need a grouping boundary: a
    // bare "-a b" negates only a, whereas "-(a b)" negates the whole group.
    $grouped = $nested && count($parts) > 1;
    if ($negated && count($parts) > 1) {
      return '-(' . $combined . ')';
    }
    return $grouped ? '(' . $combined . ')' : ($negated ? '-' . $combined : $combined);
  }

  /**
   * Escapes Lucene/Solr special characters in a single fulltext term, then
   * quotes it as a phrase if it contains whitespace.
   */
  private function escapeTerm(string $term): string {
    $special = ['\\', '+', '-', '&&', '||', '!', '(', ')', '{', '}', '[', ']', '^', '"', '~', '*', '?', ':', '/'];
    $escaped = str_replace($special, array_map(fn ($char) => '\\' . $char, $special), $term);

    return preg_match('/\s/', $escaped) ? '"' . $escaped . '"' : $escaped;
  }

  /**
   * Translates one condition or nested group.
   */
  private function buildConditionMember($condition, IndexInterface $index, bool $nested): string {
    if ($condition instanceof ConditionInterface) {
      return $this->buildCondition($condition, $index);
    }
    if ($condition instanceof ConditionGroupInterface) {
      return $this->buildConditionGroup($condition, $index, $nested);
    }
    throw new \InvalidArgumentException('Unsupported Search API condition member.');
  }

  /**
   * Recursively translates a Search API condition group.
   */
  private function buildConditionGroup(ConditionGroupInterface $group, IndexInterface $index, bool $parenthesize): string {
    $parts = [];
    foreach ($group->getConditions() as $condition) {
      $parts[] = $this->buildConditionMember($condition, $index, $condition instanceof ConditionGroupInterface);
    }
    $query = implode($group->getConjunction() === 'OR' ? ' OR ' : ' AND ', $parts);
    return $parenthesize ? '(' . $query . ')' : $query;
  }

  /**
   * Translates one Search API condition to a Lucene query string.
   */
  private function buildCondition(ConditionInterface $condition, IndexInterface $index): string {
    $fieldId = $condition->getField();
    if (!is_string($fieldId) || $fieldId === '' || !($field = $index->getField($fieldId))) {
      throw new \InvalidArgumentException('Condition field is missing or is not part of the index.');
    }

    $fieldName = $this->fieldMapper->fieldName($fieldId, $field->getType(), $this->fieldMapper->isMultiValued($field));
    $operator = strtoupper(trim((string) $condition->getOperator()));
    $value = $condition->getValue();

    if (in_array($operator, ['BETWEEN', 'NOT BETWEEN'], TRUE)) {
      if (!is_array($value) || $value === [] || count($value) > 2) {
        throw new \InvalidArgumentException('BETWEEN requires an array of one or two values.');
      }
      $values = array_values($value);
      if (count($values) === 1) {
        // search_api_solr 4.3.13 normalizes a one-member range to its scalar
        // counterpart rather than emitting an invalid Lucene range.
        $operator = $operator === 'BETWEEN' ? '=' : '<>';
        $value = $values[0];
      }
    }

    if ($value === NULL) {
      return match ($operator) {
        '=' => '-' . $fieldName . ':[* TO *]',
        '<>' => $fieldName . ':[* TO *]',
        default => throw new \InvalidArgumentException('NULL is supported only with = and <> conditions.'),
      };
    }

    if ($value === '*' && !in_array($operator, ['=', 'BETWEEN', 'NOT BETWEEN'], TRUE)) {
      throw new \InvalidArgumentException('Unsupported operator for wildcard searches.');
    }

    return match ($operator) {
      '=' => $fieldName . ':' . ($value === '*' ? '*' : $this->fieldMapper->filterValue($value, $field->getType())),
      '<>' => '(*:* -' . $fieldName . ':' . $this->fieldMapper->filterValue($value, $field->getType()) . ')',
      '<' => $fieldName . ':[* TO ' . $this->fieldMapper->filterValue($value, $field->getType()) . '}',
      '<=' => $fieldName . ':[* TO ' . $this->fieldMapper->filterValue($value, $field->getType()) . ']',
      '>' => $fieldName . ':{' . $this->fieldMapper->filterValue($value, $field->getType()) . ' TO *]',
      '>=' => $fieldName . ':[' . $this->fieldMapper->filterValue($value, $field->getType()) . ' TO *]',
      'BETWEEN' => $fieldName . ':[' . $this->rangeValues($value, $field->getType()) . ']',
      'NOT BETWEEN' => '(*:* -' . $fieldName . ':[' . $this->rangeValues($value, $field->getType()) . '])',
      'IN' => $this->inQuery($fieldName, $value, $field->getType()),
      'NOT IN' => $this->notInQuery($fieldName, $value, $field->getType()),
      default => throw new \InvalidArgumentException(sprintf('Unsupported condition operator "%s".', $condition->getOperator())),
    };
  }

  /**
   * Formats two range endpoints, treating NULL and literal * as unbounded.
   */
  private function rangeValues(array $value, string $type): string {
    if (count($value) !== 2) {
      throw new \InvalidArgumentException('BETWEEN requires an array of exactly two values.');
    }
    $values = array_values($value);
    return $this->rangeEndpoint($values[0], $type) . ' TO ' . $this->rangeEndpoint($values[1], $type);
  }

  /**
   * Formats a range endpoint using search_api_solr's NULL/* wildcard rule.
   */
  private function rangeEndpoint($value, string $type): string {
    return $value === NULL || $value === '*' ? '*' : $this->fieldMapper->filterValue($value, $type);
  }

  /**
   * Builds an IN query, where NULL adds a missing-field alternative.
   */
  private function inQuery(string $fieldName, $value, string $type): string {
    $values = $this->listValues($value);
    $hasNull = in_array(NULL, $values, TRUE);
    $values = array_values(array_filter($values, static fn ($item) => $item !== NULL));
    if ($values === []) {
      return '(*:* -' . $fieldName . ':[* TO *])';
    }

    $parts = $this->formatListValues($values, $type);
    $valueQuery = count($values) === 1
      ? $fieldName . ':' . $parts
      : $fieldName . ':(' . $parts . ')';
    return $hasNull
      ? '(' . $valueQuery . ' OR -' . $fieldName . ':[* TO *])'
      : $valueQuery;
  }

  /**
   * Builds a NOT IN query, where NULL requires a present field.
   */
  private function notInQuery(string $fieldName, $value, string $type): string {
    $values = $this->listValues($value);
    $hasNull = in_array(NULL, $values, TRUE);
    $values = array_values(array_filter($values, static fn ($item) => $item !== NULL));
    if ($values === []) {
      return $hasNull ? '(' . $fieldName . ':[* TO *])' : '(*:* -' . $fieldName . ':())';
    }
    $parts = $this->formatListValues($values, $type);
    return $hasNull
      ? '(' . $fieldName . ':[* TO *] -' . $fieldName . ':(' . $parts . '))'
      : '(*:* -' . $fieldName . ':(' . $parts . '))';
  }

  /**
   * Validates a non-empty IN/NOT IN array.
   *
   * search_api_solr 4.3.13's createFilterQuery() rejects empty arrays and
   * handles NULL members before phrase escaping.
   */
  private function listValues($value): array {
    if (!is_array($value) || $value === []) {
      throw new \InvalidArgumentException('An empty array is not allowed for IN conditions.');
    }
    if (in_array('*', $value, TRUE)) {
      throw new \InvalidArgumentException('Unsupported operator for wildcard searches.');
    }
    return array_values($value);
  }

  /**
   * Formats list members after NULL handling and rejects literal * for IN.
   */
  private function formatListValues(array $values, string $type): string {
    if (in_array('*', $values, TRUE)) {
      throw new \InvalidArgumentException('Unsupported operator for wildcard searches.');
    }
    return implode(' ', array_map(fn ($item) => $this->fieldMapper->filterValue($item, $type), $values));
  }

  /**
   * Builds the facet.* params from the query's 'search_api_facets' option.
   *
   * The option is the contrib facets module's shape (its
   * QueryTypePluginBase::getFacetOptions()): keyed by facet delta, each entry
   * ['field' => <SA field id>, 'limit' => int, 'operator' => 'and'|'or',
   * 'min_count' => int, 'missing' => bool, 'query_type' => string], plus an
   * optional 'sort'.
   *
   * ponytail: Wayfinder's facet.limit/facet.mincount/facet.missing/facet.sort
   * are *global* params applied to every facet.field entry (src/facet.rs,
   * facet_fields()) -- there is no f.<field>.facet.* per-field override on the
   * wire. So a query whose facets disagree on those settings cannot be
   * expressed: the last facet's settings win for the whole request. That is
   * the ceiling until Wayfinder grows per-field facet params.
   *
   * ponytail: 'operator' is not translated. OR facets need {!ex}/{!tag} local
   * params, which Wayfinder does not support (plan doc locked decision 4), so
   * every facet is filtered by the full fq set.
   *
   * @return array<string, string|int|array<int, string>>
   */
  private function buildFacets(QueryInterface $query, IndexInterface $index): array {
    $facets = $query->getOption('search_api_facets') ?: [];
    if (!is_array($facets) || $facets === []) {
      return [];
    }

    $fields = [];
    $params = [];
    foreach ($facets as $facet) {
      $fieldId = $facet['field'] ?? NULL;
      if (!is_string($fieldId) || $fieldId === '' || !($field = $index->getField($fieldId))) {
        throw new \InvalidArgumentException('Facet field is missing or is not part of the index.');
      }

      $fields[] = $this->fieldMapper->fieldName($fieldId, $field->getType(), $this->fieldMapper->isMultiValued($field));

      if (isset($facet['limit'])) {
        // Search API uses limit <= 0 for "no limit" (every facet array in
        // BackendTestBase uses limit => 0 as the ordinary case), whereas
        // Wayfinder reads facet.limit=0 as "truncate to zero buckets" and only
        // a negative limit as unlimited (src/facet.rs facet_fields(),
        // solr-ref/responses/facet_limit_zero.json vs
        // facet_limit_unlimited.json). Translate rather than pass through.
        $limit = (int) $facet['limit'];
        $params['facet.limit'] = $limit > 0 ? $limit : -1;
      }
      if (isset($facet['min_count'])) {
        $params['facet.mincount'] = (int) $facet['min_count'];
      }
      if (isset($facet['sort'])) {
        $params['facet.sort'] = (string) $facet['sort'];
      }
      if (isset($facet['missing'])) {
        // Sent as the literal string Solr expects, never a PHP bool: the client
        // casts params with (string), which would turn FALSE into ''. Guarded
        // like the settings above so a later facet that omits 'missing' does
        // not silently clobber an earlier facet's TRUE -- last facet that
        // *states* a setting wins, consistently across all four.
        $params['facet.missing'] = $facet['missing'] ? 'true' : 'false';
      }
    }

    return [
      'facet' => 'true',
      'facet.field' => count($fields) === 1 ? $fields[0] : $fields,
    ] + $params;
  }

  /**
   * Builds Solr's comma-separated sort parameter.
   */
  private function buildSort(QueryInterface $query, IndexInterface $index): string {
    $sorts = [];
    foreach ($query->getSorts() as $fieldId => $direction) {
      $fieldName = match ($fieldId) {
        'search_api_relevance' => 'score',
        'search_api_id' => 'id',
        'search_api_datasource' => 'ss_search_api_datasource',
        'search_api_language' => 'ss_search_api_language',
        default => $this->sortFieldName($fieldId, $index),
      };
      $sorts[] = $fieldName . ' ' . (strtolower(trim((string) $direction)) === 'desc' ? 'desc' : 'asc');
    }
    return implode(',', $sorts);
  }

  /**
   * Maps a Search API sort field to its Wayfinder field name.
   */
  private function sortFieldName(string $fieldId, IndexInterface $index): string {
    $field = $index->getField($fieldId);
    if (!$field) {
      throw new \InvalidArgumentException('Sort field is not part of the index.');
    }
    return $this->fieldMapper->sortFieldName($fieldId, $field->getType(), $this->fieldMapper->isMultiValued($field));
  }

  /**
   * Builds the qf param: fulltext fields (query intersect index), mapped
   * names, with '^boost' suffixes.
   */
  private function buildQf(QueryInterface $query, IndexInterface $index): string {
    $qf = [];
    foreach ($this->fulltextFieldIds($query, $index) as $fieldId) {
      $field = $index->getField($fieldId);
      if (!$field) {
        continue;
      }
      $name = $this->fieldMapper->fieldName($fieldId, $field->getType(), $this->fieldMapper->isMultiValued($field));
      $boost = $field->getBoost();
      $qf[] = $boost != 1.0 ? $name . '^' . $this->formatBoost($boost) : $name;
    }

    return implode(' ', $qf);
  }

  /**
   * Formats a boost value without trailing ".0" for whole numbers.
   */
  private function formatBoost(float $boost): string {
    return rtrim(rtrim(sprintf('%.2f', $boost), '0'), '.');
  }

}
