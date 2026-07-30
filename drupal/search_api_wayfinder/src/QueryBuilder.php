<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder;

use Drupal\search_api\IndexInterface;
use Drupal\search_api\Query\QueryInterface;

/**
 * Translates a Search API QueryInterface into Wayfinder /select params.
 *
 * M1 scope only: plain fulltext keys -> q/qf/defType, plus the index_id fq
 * that is core multi-index-per-core wiring (locked decision 2), not a
 * user-authored filter. No condition groups, sorts, facets, or MLT yet --
 * those land in M2+ per the plan doc's milestone table.
 *
 * The keys -> q flattening is a Wayfinder-shaped adaptation of
 * search_api_solr's Utility::flattenKeys(): conjunction/negation/phrase
 * handling copied, but the per-term-per-field embedding
 * ("{!edismax qf='...'}") is dropped because Wayfinder's SELECT_PARAMS
 * exposes q/qf/defType as independent top-level params, not Solarium-style
 * inline local params.
 */
class QueryBuilder {

  public function __construct(
    private readonly FieldMapper $fieldMapper = new FieldMapper(),
  ) {}

  /**
   * Builds the /select param array for the given query.
   *
   * @return array<string, string>
   */
  public function build(QueryInterface $query): array {
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

    $params['fq'] = 'index_id:"' . $index->id() . '"';

    return $params;
  }

  /**
   * Flattens a Search API parsed-keys array into a plain fulltext query
   * string.
   *
   * @param array|string $keys
   *   Either a single term string, or a nested parsed-keys array with
   *   '#conjunction' ('AND'/'OR') and optional '#negation' (bool) keys.
   */
  private function flattenKeys($keys): string {
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
      $flattened = $this->flattenKeys($value);
      if ($flattened === '') {
        continue;
      }
      $parts[] = $flattened;
    }

    $glue = $conjunction === 'OR' ? ' OR ' : ' ';
    $combined = implode($glue, $parts);

    if ($negated) {
      return '-' . $combined;
    }

    return $combined;
  }

  /**
   * Escapes Lucene/Solr special characters in a single fulltext term, then
   * quotes it as a phrase if it contains whitespace.
   *
   * This is fulltext-*keys* escaping only (the one untrusted-input path M1
   * ships): a raw user search term must not be able to inject field queries
   * (':'), grouping ('(', ')'), or unbalanced quotes into Wayfinder's `q`.
   * Filter-value escaping is separate, out-of-scope M2 work (condition
   * groups aren't implemented yet). Copied from Solr's
   * ClientUtils::escapeQueryChars() char set, as search_api_solr does.
   */
  private function escapeTerm(string $term): string {
    $special = ['\\', '+', '-', '&&', '||', '!', '(', ')', '{', '}', '[', ']', '^', '"', '~', '*', '?', ':', '/'];
    $escaped = str_replace($special, array_map(fn ($char) => '\\' . $char, $special), $term);

    if (preg_match('/\s/', $escaped)) {
      return '"' . $escaped . '"';
    }
    return $escaped;
  }

  /**
   * Builds the qf param: fulltext fields (query intersect index), mapped
   * names, with '^boost' suffixes.
   */
  private function buildQf(QueryInterface $query, $index): string {
    $queryFulltextFields = $query->getFulltextFields();
    $indexFulltextFields = $index->getFulltextFields();

    $fieldIds = $queryFulltextFields === NULL
      ? $indexFulltextFields
      : array_intersect($indexFulltextFields, $queryFulltextFields);

    $qf = [];
    foreach ($fieldIds as $fieldId) {
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
