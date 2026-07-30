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
 * component (single-site assumption for M1); reintroduce a hash component
 * if multi-site-one-core ever matters.
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

      // Cardinality comes from the index's own property-path definition,
      // not from how many values this particular item happens to carry --
      // see FieldMapper::isMultiValued() for why.
      $multiValued = $this->fieldMapper->isMultiValued($field);
      $name = $this->fieldMapper->fieldName($field->getFieldIdentifier(), $type, $multiValued);

      $doc[$name] = $multiValued ? array_values($formatted) : ($formatted[0] ?? NULL);
    }

    return [
      'add' => [
        'doc' => $doc,
      ],
    ];
  }

}
