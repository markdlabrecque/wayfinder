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
      // issue #342: the item's own language decides every text field's name
      // (tm_X3b_<lang>_<id>) and the spellcheck sink it feeds -- indexing is
      // the one context where the language is unambiguous, mirroring
      // search_api_solr's getLanguageSpecificSolrFieldNames($item_language...)
      // in indexItems() (SearchApiSolrBackend.php:2169).
      $language = $item->getLanguage();
      $name = $this->fieldMapper->fieldName($field->getFieldIdentifier(), $type, $multiValued, $language);

      if ($type === 'solr_text_suggester' || $type === 'solr_text_spellcheck') {
        // Every solr_text_suggester field collapses to the ONE fixed sink
        // field twm_suggest, and every solr_text_spellcheck field on an item
        // of one language to the ONE fixed sink spellcheck_<lang>
        // (FieldMapper::fieldName(); SearchApiSolrBackend.php:2433-2446), so a
        // plain `$doc[$name] = ...` assign lets the second such field on
        // an item silently overwrite the first. search_api_solr never hits
        // this because addIndexField() goes through Solarium's
        // Document::addField(), which APPENDS when the key already exists.
        // Issue #339: accumulate instead, in item-field iteration order.
        // Issue #342: the spellcheck sink is the same kind of fixed sink, so
        // it reuses this branch rather than duplicating it.
        //
        // ponytail: the sink is always an array, regardless of each contributing
        // field's cardinality: the preset declares twm_suggest as
        // multi_valued = true (presets/search-api.toml:99-103), so a
        // one-element array is the honest shape for a single-valued
        // suggester field. That is a small, deliberate divergence from
        // Solarium's scalar-when-one output, and is fine because
        // Solr/Wayfinder accept either shape for a multi-valued field.
        $doc[$name] = array_merge($doc[$name] ?? [], array_values($formatted));
        continue;
      }

      // issue #342: a text field is multi-valued on the wire whatever its
      // Drupal cardinality -- FieldMapper::fieldName() forces the 'm' infix
      // for every text-family prefix (SearchApiSolrBackend.php:2450-2473) --
      // so its value must always be written as an array, or a single-valued
      // text field would send a scalar to a multi_valued dynamic field.
      $doc[$name] = $multiValued || $this->fieldMapper->isLanguageSpecificTextType($type)
        ? array_values($formatted)
        : $formatted[0];

      if ($this->fieldMapper->usesLanguageSpecificSortCopy($name)) {
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
        // Issue #358: this path is NOT text-only. Upstream's gate is a
        // first-character test on the MAPPED field name ('t' or 's'),
        // (SearchApiSolrBackend.php:1448-1454), so string fields (ss_*/sm_*)
        // get the same scalar sort copy text does. The two sinks
        // (twm_suggest, spellcheck_*) are excluded by name upstream and by
        // usesLanguageSpecificSortCopy() here -- and they already branched
        // off above, so $name is never one of them at this point.
        //
        // issue #362: a SINGLE language-agnostic sort_<id> copy, not one per
        // language. search_api_solr fills sort_X3b_<lang>_<id> for every
        // language (SearchApiSolrBackend.php:1469-1481) only because real
        // Solr types each copy as a language-specific collated_<lang>
        // (different orderings); Wayfinder maps every sort_* to plain string
        // (no collation), so the copies would be byte-identical. The item's
        // own language is passed only as the sort/index MODE flag
        // (FieldMapper::sortFieldName); it does not appear in the name.
        $key = $this->fieldMapper->sortFieldName($field->getFieldIdentifier(), $type, $multiValued, $language);
        // First write wins, mirroring upstream's `if (!$doc->{$key})` guard
        // (SearchApiSolrBackend.php:1479): a later field must never
        // overwrite a sort copy an earlier one already placed. (isset()
        // rather than upstream's falsy test, so a legitimately empty first
        // value still counts as written.)
        if (!isset($doc[$key])) {
          $doc[$key] = $formatted[0];
        }
      }
    }

    return [
      'add' => [
        'doc' => $doc,
      ],
    ];
  }

}
