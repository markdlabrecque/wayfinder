<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder;

use Drupal\Core\Language\LanguageManagerInterface;
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
 *
 * issue #342: every text field now has one name per language
 * (tm_X3b_<lang>_<id>), so each context applies the query's resolved language
 * set the way search_api_solr does:
 * - qf / hl.fl / mlt.fl / terms.fl emit every variant;
 * - a condition on a text field ORs across the variants (createFilterQuery(),
 *   SearchApiSolrBackend.php:3392);
 * - a sort uses the FIRST resolved language only (:1483 takes one
 *   $sort_language_id);
 * - a facet is language-unspecific, i.e. 'und'
 *   (getSolrFieldNames() -> getLanguageSpecificSolrFieldNames(
 *   LANGCODE_NOT_SPECIFIED, ...), :4424/:4524 -> :2582-2585).
 * See LanguageResolver for where the set itself comes from.
 */
class QueryBuilder {

  /**
   * Stored fields needed to construct a v1 Search API result item.
   *
   * `id` carries the composite Search API item id, `index_id` preserves the
   * multi-index identity selected by the mandatory fq, and `score` populates
   * the result item's relevance. Wayfinder supports literal names and
   * positive globs in fl, but no exclusions (src/core_index.rs), so an
   * explicit consumer-derived list is the only shape that keeps the
   * twm_suggest and spellcheck_* index-side sinks out of response documents.
   *
   * search_api_solr 4.4.0 likewise always sets fl and treats id/language/score
   * as its baseline required fields (SearchApiSolrBackend.php:2108-2121,
   * 2163-2202). This module encodes language in the composite id and its
   * ResponseParser reads only id and score from each document.
   *
   * ponytail: Search API's search_api_retrieved_field_values option is not
   * honoured yet. Supporting stored result data later must extend this list
   * with the requested mapped fields instead of falling back to `*`, which
   * would expose the plumbing sinks again.
   */
  private const SELECT_FIELDS = ['id', 'index_id', 'score'];

  /**
   * The resolved language set for the query currently being built.
   *
   * @var array<int, string>
   */
  private array $languages = [FieldMapper::LANGUAGE_UNSPECIFIED];

  private readonly LanguageResolver $languageResolver;

  /**
   * The language manager is optional (NULL from every plain
   * `new QueryBuilder()`); WayfinderBackend::create() injects the container's.
   */
  public function __construct(
    private readonly FieldMapper $fieldMapper = new FieldMapper(),
    ?LanguageManagerInterface $languageManager = NULL,
  ) {
    $this->languageResolver = new LanguageResolver($languageManager);
  }

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
    $this->languages = $this->languageResolver->resolve($query);
    $params = [];

    $keys = $query->getKeys();
    if ($keys === NULL) {
      $params['q'] = '*:*';
    }
    else {
      $params['q'] = $this->flattenKeys($keys);
      $params['defType'] = 'edismax';
    }

    // qf is built for every select query, keys or not: search_api_solr sets
    // the edismax query fields unconditionally for any non-MLT select
    // (SearchApiSolrBackend.php:1764-1791 -- the setQueryFields() call sits
    // outside every keys check), and only the *defType* switch is keyed off
    // the keys. It is dropped when the query searches no fulltext field at
    // all, rather than sent empty.
    $qf = $this->buildQf($query, $index);
    if ($qf !== '') {
      $params['qf'] = $qf;
    }

    $params['fl'] = implode(',', self::SELECT_FIELDS);

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

    $params += $this->buildSpellcheck($query);

    return $params;
  }

  /**
   * Builds the spellcheck.* params from the query's 'search_api_spellcheck'
   * option (issue #342).
   *
   * The option's shape is search_api_solr's setSpellcheck() input
   * (SearchApiSolrBackend.php:4639-4670): ['keys' => <array of entered
   * words>, 'count' => int, 'collate' => bool].
   *
   * spellcheck.dictionary is one repeated value per resolved language, in
   * resolved order -- upstream's own per-language dictionary selection --
   * emitted with the same "scalar when there is one, array when there are
   * several" convention fq/facet.field/terms.fl already use here.
   *
   * ponytail: the option's 'count' is deliberately dropped. Wayfinder's
   * server does not list spellcheck.count in SELECT_PARAMS (src/lib.rs:286-292
   * has spellcheck, spellcheck.q, spellcheck.dictionary, spellcheck.collate,
   * spellcheck.accuracy, spellcheck.maxCollations only), and the server runs
   * strict_params = true, so sending it would 400 the whole query rather than
   * be ignored. The ceiling is therefore "the server's default suggestion
   * count", until spellcheck.count is added server-side; adding it to the
   * Rust allowlist is out of scope for this issue.
   *
   * @return array<string, string|array<int, string>>
   */
  private function buildSpellcheck(QueryInterface $query): array {
    $options = $query->getOption('search_api_spellcheck');
    if (!is_array($options)) {
      return [];
    }

    $params = ['spellcheck' => 'true'];

    $keys = $options['keys'] ?? [];
    if (is_array($keys) && $keys !== []) {
      $params['spellcheck.q'] = implode(' ', array_map('strval', $keys));
    }

    // issue #342 (MF-2): the dictionary name is the INDEXED sink's language
    // component, not the raw langcode -- FieldMapper::spellcheckDictionary()
    // is the single shared transform, so this param and
    // FieldMapper::fieldName()'s 'spellcheck_' . <dictionary> sink cannot
    // drift apart again.
    $dictionaries = array_map(
      fn (string $language): string => $this->fieldMapper->spellcheckDictionary($language),
      $this->languages
    );
    $params['spellcheck.dictionary'] = count($dictionaries) === 1
      ? $dictionaries[0]
      : $dictionaries;

    if (!empty($options['collate'])) {
      $params['spellcheck.collate'] = 'true';
    }

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
    $this->languages = $this->languageResolver->resolve($query);
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
    $this->languages = $this->languageResolver->resolve($query);
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
   * issue #342: a text field contributes one name per resolved language --
   * hl.fl/mlt.fl/terms.fl all name the fields to read, and a document only
   * ever carries the variant it was indexed in, so every variant has to be
   * listed for the field to be found at all.
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
      foreach ($this->fieldNameVariants((string) $fieldId, $field->getType(), $this->fieldMapper->isMultiValued($field)) as $name) {
        $names[] = $name;
      }
    }
    return $names;
  }

  /**
   * Every name one Search API field maps to under the resolved language set:
   * one per language for a text field, a single language-free name for
   * anything else.
   *
   * @return array<int, string>
   */
  private function fieldNameVariants(string $fieldId, string $type, bool $multiValued): array {
    if (!$this->fieldMapper->isLanguageSpecificTextType($type)) {
      return [$this->fieldMapper->fieldName($fieldId, $type, $multiValued)];
    }
    return array_map(
      fn (string $language): string => $this->fieldMapper->fieldName($fieldId, $type, $multiValued, $language),
      $this->languages
    );
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

    // issue #342: a text field has one name per resolved language, and a
    // document carries only the one it was indexed in, so a POSITIVE condition
    // has to match ANY of them -- search_api_solr ORs the same way in
    // createFilterQuery() (SearchApiSolrBackend.php:3392). A single resolved
    // language (the common case) produces exactly the pre-#342 string, with
    // no added parentheses.
    $fieldNames = $this->fieldNameVariants($fieldId, $field->getType(), $this->fieldMapper->isMultiValued($field));
    $operator = strtoupper(trim((string) $condition->getOperator()));
    $value = $condition->getValue();

    $parts = array_map(
      fn (string $fieldName): string => $this->buildConditionForFieldName($fieldName, $field->getType(), $operator, $value, $condition),
      $fieldNames
    );

    if (count($parts) === 1) {
      return $parts[0];
    }

    // issue #342 (MF-1, MF-4): the conjunction follows the per-variant
    // clause's polarity -- a clause an ABSENT variant satisfies trivially
    // joins with AND, anything else with OR. See isNegatedClause() for the
    // rule and the full table. A document carries only the variant it was
    // indexed in, so OR-ing such a clause matches every document: `body <>
    // "hello"` would return the document whose body IS "hello", and an
    // exclusion filter would silently do nothing.
    $conjunction = $this->isNegatedClause($operator, $value) ? ' AND ' : ' OR ';

    return '(' . implode($conjunction, $parts) . ')';
  }

  /**
   * Whether a condition's per-variant clause is satisfied trivially by a
   * document that lacks that language variant (issue #342, MF-1 and MF-4).
   *
   * THE GOVERNING RULE, and the only question to ask when changing this
   * method or adding an operator: *does a document lacking this language
   * variant satisfy the per-variant clause?* If yes, the variants must be
   * combined with AND -- otherwise the variants the document does not carry
   * make the whole disjunction true and the condition matches everything. If
   * no, combine with OR, so the one variant the document does carry can
   * satisfy it. The question is about the emitted CLAUSE, never the
   * operator's name: `= NULL` and `<> NULL` answer it in opposite directions
   * from the same operator pair, and so do `IN [a, NULL]` and `IN [a, b]`.
   *
   * Ground truth is upstream's own version of the rule for a fulltext field
   * checked against NULL, "'=' === $condition->getOperator() ? 'AND' : 'OR'"
   * (SearchApiSolrBackend.php:3450-3459): `= NULL` ("field is missing") joins
   * with AND, `<> NULL` ("field exists") with OR.
   *
   * The full table over the operators this class supports -- an absent
   * variant satisfies the clause, so AND:
   * - `= NULL` and `IN [NULL]`, which emit the bare missing-field clause
   *   "-field:[* TO *]";
   * - `<>` and `NOT BETWEEN`, and `NOT IN` without a NULL member, which emit
   *   "*:* -field:...";
   * - `IN` with ANY NULL member, which emits
   *   "(field:(...) OR -field:[* TO *])" (inQuery()) -- the missing-field
   *   disjunct is true for every variant the document lacks, so this ANDs
   *   despite `IN` being a positive operator and despite the clause also
   *   having non-NULL values to match. This is MF-4: keying the `IN` arm off
   *   "has no non-NULL value" instead made `body IN ['a', NULL]` match every
   *   document.
   *
   * An absent variant does NOT satisfy the clause, so OR:
   * - `=`, `BETWEEN`, `IN` without a NULL member -- plain value matches;
   * - `<> NULL`, `NOT IN [NULL]`, and `NOT IN` WITH a NULL member, which all
   *   keep a "field:[* TO *]" existence requirement (notInQuery()) that pins
   *   the clause to the one variant the document actually carries. AND would
   *   require every variant to exist and would exclude every document.
   *
   * @param mixed $value
   */
  private function isNegatedClause(string $operator, $value): bool {
    if ($value === NULL) {
      return $operator === '=';
    }

    if (($operator === 'IN' || $operator === 'NOT IN') && is_array($value)) {
      // A NULL member adds the missing-field alternative to IN and removes it
      // from NOT IN (inQuery()/notInQuery()), so it flips the answer in
      // opposite directions for the two operators.
      $hasNull = in_array(NULL, $value, TRUE);
      return $operator === 'NOT IN' ? !$hasNull : $hasNull;
    }

    return $operator === '<>' || $operator === 'NOT BETWEEN';
  }

  /**
   * Translates one Search API condition against one concrete field name.
   *
   * Split out of buildCondition() for #342: the operator/value handling is
   * identical for every language variant of a text field, only the field name
   * differs.
   *
   * @param mixed $value
   */
  private function buildConditionForFieldName(string $fieldName, string $type, string $operator, $value, ConditionInterface $condition): string {
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
      '=' => $fieldName . ':' . ($value === '*' ? '*' : $this->fieldMapper->filterValue($value, $type)),
      '<>' => '(*:* -' . $fieldName . ':' . $this->fieldMapper->filterValue($value, $type) . ')',
      '<' => $fieldName . ':[* TO ' . $this->fieldMapper->filterValue($value, $type) . '}',
      '<=' => $fieldName . ':[* TO ' . $this->fieldMapper->filterValue($value, $type) . ']',
      '>' => $fieldName . ':{' . $this->fieldMapper->filterValue($value, $type) . ' TO *]',
      '>=' => $fieldName . ':[' . $this->fieldMapper->filterValue($value, $type) . ' TO *]',
      'BETWEEN' => $fieldName . ':[' . $this->rangeValues($value, $type) . ']',
      'NOT BETWEEN' => '(*:* -' . $fieldName . ':[' . $this->rangeValues($value, $type) . '])',
      'IN' => $this->inQuery($fieldName, $value, $type),
      'NOT IN' => $this->notInQuery($fieldName, $value, $type),
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

      // issue #342: facets are language-UNSPECIFIC. search_api_solr's facet
      // path calls getSolrFieldNames($index)
      // (SearchApiSolrBackend.php:4424, :4524), which is
      // getLanguageSpecificSolrFieldNames(LANGCODE_NOT_SPECIFIED, ...)
      // (:2582-2585), so a facet on a text field targets tm_X3b_und_<id>
      // whatever the query's resolved languages are -- hence the default
      // 'und' language here, and no variant expansion.
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
    // issue #342: a sort takes ONE field name, so a text field sorts on the
    // first resolved language's sort_X3b_<lang>_<id> -- search_api_solr
    // likewise passes a single $sort_language_id into the sort field name
    // (SearchApiSolrBackend.php:1483). Non-text fields ignore the language.
    return $this->fieldMapper->sortFieldName($fieldId, $field->getType(), $this->fieldMapper->isMultiValued($field), $this->languages[0]);
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
      // Correction (#342 review): an earlier comment here claimed getBoost()
      // is nullable in production. It is not -- Field::getBoost() is
      // "return $this->boost ?? 1.0;"
      // (vendor/drupal/search_api/src/Item/Field.php:604), and
      // FieldInterface::getBoost() documents the 1.0 default. NULL only ever
      // reaches this line from a test double that stubs getBoost() without a
      // return value, so the ?? is kept purely so such a mock cannot push NULL
      // into formatBoost(); it is defensive, not a real-world case.
      $boost = (float) ($field->getBoost() ?? 1.0);
      // issue #342: one qf entry per language variant, each carrying the
      // field's boost -- a document only ever holds the variant it was
      // indexed in, so all of them have to be searched.
      foreach ($this->fieldNameVariants((string) $fieldId, $field->getType(), $this->fieldMapper->isMultiValued($field)) as $name) {
        $qf[] = $boost != 1.0 ? $name . '^' . $this->formatBoost($boost) : $name;
      }
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
