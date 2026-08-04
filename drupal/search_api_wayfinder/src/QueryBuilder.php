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
          $fq = $this->buildConditionMember($condition, $index, $condition instanceof ConditionGroupInterface);
          $filters[] = $this->tagFilterQuery($condition, $fq);
        }
      }
      else {
        $filters[] = $this->tagFilterQuery($conditions, $this->buildConditionGroup($conditions, $index, FALSE));
      }
    }
    $params['fq'] = count($filters) === 1 ? $filters[0] : $filters;

    $params += $this->buildFacets($query, $index);

    $params += $this->buildGrouping($query, $index);

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
   * Builds the /terms param array for an autocomplete request (#291).
   *
   * search_api_autocomplete's stock Server suggester reads the indexed term
   * dictionary through the Terms component, not the SuggestComponent (finding
   * 153 -- the SuggestComponent is not on any evidenced client path). This
   * mirrors search_api_solr's setAutocompleteTermQuery() + getAutocompleteFields()
   * (coverage/search_api_solr_4.4.0_source ... 4011-4039): terms.fl is the
   * query's fulltext fields mapped to their Wayfinder names -- every
   * solr_text_suggester field collapses to the fixed sink 'twm_suggest'
   * (#300/finding 151), deduped so the dictionary is not requested twice --
   * terms.prefix is the incomplete key the user typed, and terms.limit is the
   * query's suggestion limit (default 10, finding 142).
   *
   * No q/fq: the Terms component scans the dictionary, it does not run a
   * search, so unlike build()/buildMlt() there is no index scope filter.
   * omitHeader=true follows search_api_solr's standard envelope convention on
   * every endpoint (e.g. MLT_PARAMS' omitHeader/TZ note).
   *
   * @return array<string, string|int|array<int, string>>
   */
  public function buildAutocompleteTerms(QueryInterface $query, string $incomplete_key): array {
    $index = $query->getIndex();
    $fields = array_values(array_unique(
      $this->mapFieldNames($this->fulltextFieldIds($query, $index), $index)
    ));

    return [
      'terms' => 'true',
      'terms.fl' => count($fields) === 1 ? $fields[0] : $fields,
      'terms.prefix' => $incomplete_key,
      'terms.limit' => (int) ($query->getOption('limit') ?? 10),
      'omitHeader' => 'true',
    ];
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
   * Prefixes a built fq with {!tag=...} when its source carries Search API
   * condition-group tags. The `facets` module tags an OR facet's filter
   * condition group with `facet:<search_api_field_name>` (#298) -- the same
   * string buildFacets() puts in {!ex=...} -- so the tag must reach the wire
   * here for the exclusion to bite. search_api_solr does the same in
   * reduceFilterQueries(): a condition group's tags become {!tag} local params
   * on its resulting fq. Plain conditions carry no tags and pass through
   * unchanged, so existing facet-free queries are byte-identical.
   */
  private function tagFilterQuery($member, string $fq): string {
    if (!$member instanceof ConditionGroupInterface) {
      return $fq;
    }
    $tags = array_keys($member->getTags());
    if ($tags === []) {
      return $fq;
    }
    return '{!tag=' . implode(',', $tags) . '}' . $fq;
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
   * Issue #296: each facet's limit/min_count/sort/missing travel as local
   * params on that facet's own facet.field value
   * ({!key=<delta> facet.limit=10 ...}<field>), never as global facet.*
   * params. Two reasons, both captured (findings 147-151,
   * docs/solr-ref-findings.md): f.<field>.facet.* addresses the *Solr field*,
   * never the {!key=} delta this module always sends (#299), and two facets
   * over one field share that field -- so only the local-param form can give
   * them different settings (finding 149, solr-ref/responses/
   * facet_perfield_two_lp.json). That retires the previous "last facet's
   * settings win for the whole request" ceiling.
   *
   * Precedence on the server is f.<field>.facet.X > local param > global
   * facet.X (finding 152); this module emits only the local-param form, so a
   * site that also sets a global facet.* elsewhere loses to these, which is
   * the intended reading.
   *
   * 'operator' => 'or' translates to {!ex=facet:<field>} (#298); the matching
   * {!tag=facet:<field>} lands on the facet's own fq in build().
   *
   * ponytail: facet.query carries no settings here -- Solr does honour local
   * params on facet.query too, but nothing captures it and this module emits
   * no facet.query at all. Likewise nothing range-related: search_api_solr's
   * only range facets are its 'search_api_granular' query type, which it
   * builds through Solarium's createFacetRange(['local_key' => ..., 'start'
   * => ..., 'end' => ..., 'gap' => ...]) rather than by writing any
   * f.<field>.facet.range.* itself (coverage/search_api_solr_4.4.0_source,
   * SearchApiSolrBackend::setFacets). What Solarium then puts on the wire for
   * those is unverified -- Solarium is not vendored here -- and moot for now,
   * since this module emits no facet.range at all and Wayfinder honours only
   * global facet.range.* (src/facet.rs facet_ranges()).
   *
   * @return array<string, string|int|array<int, string>>
   */
  private function buildFacets(QueryInterface $query, IndexInterface $index): array {
    $facets = $query->getOption('search_api_facets') ?: [];
    if (!is_array($facets) || $facets === []) {
      return [];
    }

    $fields = [];
    foreach ($facets as $delta => $facet) {
      $fieldId = $facet['field'] ?? NULL;
      if (!is_string($fieldId) || $fieldId === '' || !($field = $index->getField($fieldId))) {
        throw new \InvalidArgumentException('Facet field is missing or is not part of the index.');
      }

      $fieldName = $this->fieldMapper->fieldName($fieldId, $field->getType(), $this->fieldMapper->isMultiValued($field));
      // Local-params prefix for this facet.field: always {!key=<delta>} (#299)
      // so two facets on one field answer under distinct keys, plus
      // {!ex=facet:<field>} for OR-operator facets (#298) so the facet counts
      // against the base query minus its own tagged fq. The ex tag is the
      // *Search API field id*, not the mapped Solr field: search_api_solr's
      // SearchApiSolrBackend emits addExcludes(['facet:' . $info['field']]) --
      // the exact string the facets module puts in {!tag=...} on the fq, built
      // from the Search API field name. A colon in that tag is fine: the
      // server's local_params::read_value treats it as a bare value (pinned in
      // src/local_params.rs). A delta failing [A-Za-z0-9_:-]+ falls back to the
      // bare field name for the key half only; the ex half is unaffected
      // because it is built from the field id, not the delta. ex precedes key,
      // the order solr-ref/responses/facet_extag_both_facets.json captures.
      $prefix = '';
      if (strtolower((string) ($facet['operator'] ?? 'and')) === 'or') {
        $prefix = 'ex=facet:' . $fieldId;
      }
      if (preg_match('/^[A-Za-z0-9_:-]+$/', (string) $delta)) {
        $prefix .= ($prefix === '' ? '' : ' ') . 'key=' . $delta;
      }

      // #296: this facet's own settings, appended to the same block, in the
      // order they are read here.
      $settings = [];
      if (isset($facet['limit'])) {
        // Search API uses limit <= 0 for "no limit" (every facet array in
        // BackendTestBase uses limit => 0 as the ordinary case), whereas
        // Wayfinder reads facet.limit=0 as "truncate to zero buckets" and only
        // a negative limit as unlimited (src/facet.rs BucketShaping::for_field,
        // solr-ref/responses/facet_limit_zero.json vs
        // facet_limit_unlimited.json). Translate rather than pass through.
        $limit = (int) $facet['limit'];
        $settings['facet.limit'] = (string) ($limit > 0 ? $limit : -1);
      }
      if (isset($facet['min_count'])) {
        $settings['facet.mincount'] = (string) (int) $facet['min_count'];
      }
      if (isset($facet['sort'])) {
        $settings['facet.sort'] = (string) $facet['sort'];
      }
      if (isset($facet['missing'])) {
        // Sent as the literal string Solr expects, never a PHP bool -- a bool
        // cast to string would turn FALSE into '', which parse_bool rejects
        // with a 400 (src/params.rs).
        $settings['facet.missing'] = $facet['missing'] ? 'true' : 'false';
      }
      foreach ($settings as $name => $value) {
        $prefix .= ($prefix === '' ? '' : ' ') . $name . '=' . $this->localParamValue($value);
      }

      $fields[] = $prefix === '' ? $fieldName : '{!' . $prefix . '}' . $fieldName;
    }

    return [
      'facet' => 'true',
      'facet.field' => count($fields) === 1 ? $fields[0] : $fields,
    ];
  }

  /**
   * One local-param value, safe to place inside a {!...} block.
   *
   * #296: a setting value is free-form input the same way a facet delta is
   * (#299's guard covers only the key), and 'sort' in particular comes
   * straight off the facet array. A value carrying whitespace would split
   * into a bogus second local param and one carrying '}' would close the
   * block early, letting the rest of the value become query-affecting wire
   * text. Unlike the delta there is no safe fallback that keeps the setting's
   * meaning -- dropping it silently changes the answer -- so an unsafe value
   * is quoted instead of dropped.
   *
   * Double quotes are what the server's block grammar reads: find_block_end()
   * skips a '}' inside a quoted value and read_value() stops at the matching
   * unescaped closing quote, honouring backslash escapes on the way
   * (src/local_params.rs). So '"' and '\' inside the value are backslash
   * escaped, and everything else survives verbatim.
   *
   * Safe values are left bare so the ordinary wire stays byte-identical to
   * what #298/#299 already captured.
   */
  private function localParamValue(string $value): string {
    if (preg_match('/^[A-Za-z0-9_:.\/-]+$/', $value)) {
      return $value;
    }
    return '"' . str_replace(['\\', '"'], ['\\\\', '\\"'], $value) . '"';
  }

  /**
   * Builds the group.* params from the query's 'search_api_grouping' option
   * (issue #290).
   *
   * The option's shape is search_api_solr's setGrouping() input (coverage/
   * search_api_solr_4.4.0_source ... SearchApiSolrBackend::setGrouping,
   * finding 130): ['use_grouping' => bool, 'fields' => <SA field ids to
   * collapse on>, 'truncate' => bool, 'group_facet' => bool,
   * 'group_limit' => int, 'group_offset' => int, 'group_sort' =>
   * [<field id> => 'asc'|'desc']].
   *
   * Grouping only supports single-valued, non-fulltext fields -- both the
   * module (setGrouping logs an error and skips) and the server
   * (src/grouping.rs validate_group_field 400s) refuse anything else. To keep
   * a misconfigured grouping from 400-ing the whole request, those fields are
   * skipped here the same way setGrouping skips them, never reaching
   * group.field. If every requested field is unsuitable, grouping is not
   * activated (no group.* params emitted).
   *
   * group.ngroups is sent unconditionally ("we always want the number of
   * groups returned so that we get pagers done right", finding 130).
   * group.limit mirrors setGrouping: omitted at its default of 1 (so the wire
   * never carries group.limit=1). group.truncate/group.facet are accepted for
   * strict_params parity but the server treats them as a no-op until their
   * group+facet interaction is fixture-backed (src/lib.rs SELECT_PARAMS).
   *
   * @return array<string, string|int|array<int, string>>
   */
  private function buildGrouping(QueryInterface $query, IndexInterface $index): array {
    $grouping = $query->getOption('search_api_grouping');
    if (!is_array($grouping) || empty($grouping['use_grouping'])) {
      return [];
    }

    $groupFields = [];
    foreach (($grouping['fields'] ?? []) as $fieldId) {
      $field = is_string($fieldId) ? $index->getField($fieldId) : NULL;
      if (!$field) {
        continue;
      }
      // Grouping needs a fast, single-valued column (finding 130 /
      // src/grouping.rs). Skip a fulltext or multi-valued field rather than
      // emit a group.field the server would 400 on -- mirrors setGrouping's
      // own "is not supported" skip.
      if ($field->getType() === 'text' || $this->fieldMapper->isMultiValued($field)) {
        continue;
      }
      $groupFields[] = $this->fieldMapper->sortFieldName($fieldId, $field->getType(), FALSE);
    }

    if ($groupFields === []) {
      return [];
    }

    $params = [
      'group' => 'true',
      'group.ngroups' => 'true',
      'group.field' => count($groupFields) === 1 ? $groupFields[0] : $groupFields,
    ];

    // setGrouping sends group.limit only when set and != 1 (its default).
    if (!empty($grouping['group_limit']) && (int) $grouping['group_limit'] != 1) {
      $params['group.limit'] = (int) $grouping['group_limit'];
    }
    if (isset($grouping['group_offset'])) {
      $params['group.offset'] = (int) $grouping['group_offset'];
    }
    if (!empty($grouping['truncate'])) {
      $params['group.truncate'] = 'true';
    }
    if (!empty($grouping['group_facet'])) {
      $params['group.facet'] = 'true';
    }
    if (!empty($grouping['group_sort'])) {
      $sorts = [];
      foreach ($grouping['group_sort'] as $sortFieldId => $order) {
        $sorts[] = $this->mapSortFieldId((string) $sortFieldId, $index) . ' ' . (strtolower((string) $order) === 'desc' ? 'desc' : 'asc');
      }
      $params['group.sort'] = implode(',', $sorts);
    }

    return $params;
  }

  /**
   * Builds Solr's comma-separated sort parameter.
   */
  private function buildSort(QueryInterface $query, IndexInterface $index): string {
    $sorts = [];
    foreach ($query->getSorts() as $fieldId => $direction) {
      $sorts[] = $this->mapSortFieldId((string) $fieldId, $index) . ' ' . (strtolower(trim((string) $direction)) === 'desc' ? 'desc' : 'asc');
    }
    return implode(',', $sorts);
  }

  /**
   * Maps a Search API field id used for sorting (and group.sort) to its
   * Wayfinder field name: the four search_api_* pseudo-fields resolve to
   * their reserved columns, everything else to its fast sort column. Shared
   * by `sort` and `group.sort` so both honour the same pseudo-fields.
   */
  private function mapSortFieldId(string $fieldId, IndexInterface $index): string {
    return match ($fieldId) {
      'search_api_relevance' => 'score',
      'search_api_id' => 'id',
      'search_api_datasource' => 'ss_search_api_datasource',
      'search_api_language' => 'ss_search_api_language',
      default => $this->sortFieldName($fieldId, $index),
    };
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
