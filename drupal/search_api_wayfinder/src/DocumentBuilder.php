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

      if ($type === 'solr_text_suggester') {
        // Every solr_text_suggester field collapses to the ONE fixed sink
        // field twm_suggest (FieldMapper::fieldName(), FieldMapper.php:106-118),
        // so a plain `$doc[$name] = ...` assign lets the second such field on
        // an item silently overwrite the first. search_api_solr never hits
        // this because addIndexField() goes through Solarium's
        // Document::addField(), which APPENDS when the key already exists.
        // Issue #339: accumulate instead, in item-field iteration order.
        //
        // The sink is always an array, regardless of each contributing
        // field's cardinality: the preset declares twm_suggest as
        // multi_valued = true (presets/search-api.toml:99-103), so a
        // one-element array is the honest shape for a single-valued
        // suggester field. That is a small, deliberate divergence from
        // Solarium's scalar-when-one output, and is fine because
        // Solr/Wayfinder accept either shape for a multi-valued field.
        $doc[$name] = array_merge($doc[$name] ?? [], array_values($formatted));
        continue;
      }

      $doc[$name] = $multiValued ? array_values($formatted) : $formatted[0];

      if ($type === 'text') {
        // Confirmed-correct, not a descope: a multi-valued text field's
        // sort_* copy takes the FIRST value, matching captured
        // search_api_solr / solr:9. search_api_solr's own source copies only
        // the first value into sort_* -- SearchApiSolrBackend::addIndexField()
        // returns "the first value of $values that has been added to the
        // index", written as a scalar into each language-specific sort_*
        // field (coverage/.../SearchApiSolrBackend.php:1485, 2726). The
        // captured live-solr:9 trace agrees (solr-ref/search-api/trace/00001.json:
        // sm_field_topics = ["legacy", "documentation"] -> sort_*_field_topics
        // = "legacy"), and zero sort_* field carries more than one value
        // anywhere in the trace, so Solr's Lucene min/max selector never runs
        // on a text sort field. The non-text path is different: it sorts on
        // the actual mapped multi-valued fast field, where Wayfinder's native
        // min/max selection (src/collector.rs) IS what Solr does. Recorded as
        // finding #153 in docs/solr-ref-findings.md; pinned by
        // DocumentBuilderTest with an input whose first value is neither its
        // min nor its max. See issue #302.
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
