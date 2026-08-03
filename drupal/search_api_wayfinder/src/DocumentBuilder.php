<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder;

use Drupal\search_api\Item\ItemInterface;

/**
 * Builds Solr "add" update commands from Search API items.
 *
 * Command shape and static fields per plan doc "Indexing translation"
 * section and locked decision 2 (docs/plans/57-search-api-wayfinder-backend.md).
 *
 * ponytail: document id deliberately omits search_api_solr's site-hash
 * component. Decided in issue #301: one core per site is the supported
 * topology, not a temporary simplification. Several sites on one host are
 * several Wayfinder processes with one core each -- the server already
 * enforces single-core-per-process (docs/PRD.md open question 1). Nothing
 * detects two sites pointed at the same core; see README "Not supported".
 *
 * `commitWithin` is deliberately NOT part of this command: Wayfinder's
 * `/update` parser (`parse_update_commands` in src/lib.rs) only ever reads
 * `add.doc` from the POST body -- `commitWithin` is a `/update` *query*
 * param (`UPDATE_PARAMS`), read separately from the body. Putting it in the
 * doc here would silently do nothing (doc still gets added, but never
 * committed). See WayfinderClient::update()/WayfinderBackend::indexItems(),
 * which pass it as a query param instead.
 */
class DocumentBuilder {

  public function __construct(
    private readonly FieldMapper $fieldMapper,
  ) {}

  /**
   * Builds a Solr "add" command for a single Search API item.
   *
   * @return array
   *   An ["add" => ["doc" => [...]]] structure.
   */
  public function buildAddCommand(ItemInterface $item, string $indexId): array {
    $doc = [
      'id' => $indexId . '-' . $item->getId(),
      'index_id' => $indexId,
      'ss_search_api_language' => $item->getLanguage(),
      'ss_search_api_datasource' => $item->getDatasourceId(),
    ];

    foreach ($item->getFields() as $field) {
      $type = $field->getType();
      $values = $field->getValues();
      $formatted = array_map(
        fn ($value) => $this->fieldMapper->formatValue($value, $type),
        $values
      );

      // An item field with no values is omitted entirely -- Solr never
      // receives null/[] for an absent field, and Wayfinder rejects null for
      // a typed field ("field ts_X expects a string value, got null"). This
      // is what lets optional/computed fields -- e.g. the #262 file-extraction
      // field on an item with no attachment -- index cleanly.
      if ($formatted === []) {
        continue;
      }

      // Cardinality comes from the index's own property-path definition,
      // not from how many values this particular item happens to carry --
      // see FieldMapper::isMultiValued() for why.
      $multiValued = $this->fieldMapper->isMultiValued($field);
      $name = $this->fieldMapper->fieldName($field->getFieldIdentifier(), $type, $multiValued);

      $doc[$name] = $multiValued ? array_values($formatted) : $formatted[0];

      if ($type === 'text') {
        // ponytail: multi-valued text sorts use the first Search API value.
        // Wayfinder has native min/max selection only for the actual mapped
        // fast field; a collation-aware multi-value text selector needs a
        // dedicated schema/type design before this can be broadened.
        $doc[$this->fieldMapper->sortFieldName($field->getFieldIdentifier(), $type, $multiValued)] = $formatted[0];
      }
    }

    return [
      'add' => [
        'doc' => $doc,
      ],
    ];
  }

}
