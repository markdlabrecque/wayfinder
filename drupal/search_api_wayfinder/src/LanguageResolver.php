<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder;

use Drupal\Core\Language\LanguageManagerInterface;
use Drupal\search_api\Query\ConditionGroupInterface;
use Drupal\search_api\Query\ConditionInterface;
use Drupal\search_api\Query\QueryInterface;

/**
 * Resolves which language(s) a query's text fields should be named for
 * (issue #342).
 *
 * search_api_solr does the same job in Utility::ensureLanguageCondition(),
 * called before every query it builds (SearchApiSolrBackend.php:3385, and the
 * indexing/autocomplete paths at :2169, :4010, :4553): a query either names
 * its languages through a `search_api_language` condition, or it means every
 * language the site has enabled. Utility is not vendored in coverage/, so the
 * order is encoded here directly:
 *
 * 1. the values of any `search_api_language` condition on the query
 *    (operators `=` and `IN`), including ones nested in a condition group;
 * 2. otherwise every enabled site language, in the language manager's order;
 * 3. otherwise -- and whenever 1 and 2 both come up empty --
 *    ['und'] (LanguageInterface::LANGCODE_NOT_SPECIFIED), which is also what
 *    search_api_solr's language-unspecific paths use
 *    (SearchApiSolrBackend.php:2582-2585).
 *
 * The language manager is optional: without a container (plain
 * `new QueryBuilder()`), step 2 is simply skipped.
 */
class LanguageResolver {

  /**
   * The Search API field id carrying an item's language. Reserved by Search
   * API core, and the field search_api_solr keys its own language condition
   * off.
   */
  private const LANGUAGE_FIELD = 'search_api_language';

  public function __construct(
    private readonly ?LanguageManagerInterface $languageManager = NULL,
  ) {}

  /**
   * Resolves the ordered, deduplicated language set for one query.
   *
   * @return array<int, string>
   *   A non-empty list of language ids.
   */
  public function resolve(QueryInterface $query): array {
    $conditionGroup = $query->getConditionGroup();
    $languages = $conditionGroup instanceof ConditionGroupInterface
      ? $this->languagesFromConditions($conditionGroup)
      : [];

    if ($languages === [] && $this->languageManager !== NULL) {
      foreach ($this->languageManager->getLanguages() as $language) {
        $languages[] = $language->getId();
      }
    }

    $languages = array_values(array_unique(array_filter(
      $languages,
      static fn ($language): bool => is_string($language) && $language !== ''
    )));

    return $languages === [] ? [FieldMapper::LANGUAGE_UNSPECIFIED] : $languages;
  }

  /**
   * Collects the values of every `search_api_language` condition in a group,
   * recursing into nested groups -- a language condition added by a processor
   * or a Views filter is not necessarily at the top level.
   *
   * Only `=` and `IN` say "the result is in these languages"; a negated or
   * range operator does not name a set of languages to search in, so it is
   * ignored here exactly as it is by search_api_solr's own language handling.
   *
   * @return array<int, string>
   */
  private function languagesFromConditions(ConditionGroupInterface $group): array {
    $languages = [];
    foreach ($group->getConditions() as $condition) {
      if ($condition instanceof ConditionGroupInterface) {
        $languages = array_merge($languages, $this->languagesFromConditions($condition));
        continue;
      }
      if (!$condition instanceof ConditionInterface || $condition->getField() !== self::LANGUAGE_FIELD) {
        continue;
      }
      $operator = strtoupper(trim((string) $condition->getOperator()));
      if ($operator !== '=' && $operator !== 'IN') {
        continue;
      }
      foreach ((array) $condition->getValue() as $value) {
        $languages[] = $value;
      }
    }
    return $languages;
  }

}
