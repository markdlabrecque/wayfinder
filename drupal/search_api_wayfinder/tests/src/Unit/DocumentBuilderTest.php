<?php

declare(strict_types=1);

namespace Drupal\Tests\search_api_wayfinder\Unit;

use Drupal\Core\Field\FieldDefinitionInterface;
use Drupal\Core\Field\FieldStorageDefinitionInterface;
use Drupal\search_api\IndexInterface;
use Drupal\search_api\Item\FieldInterface;
use Drupal\search_api\Item\ItemInterface;
use Drupal\search_api\Plugin\search_api\data_type\value\TextValue;
use Drupal\search_api_wayfinder\DocumentBuilder;
use Drupal\search_api_wayfinder\FieldMapper;
use PHPUnit\Framework\TestCase;

/**
 * Tests DocumentBuilder: Search API item -> Solr "add" update command.
 *
 * Command shape ({"add": {"doc": {...}}}) and static fields (id, index_id,
 * ss_search_api_language, ss_search_api_datasource) are per plan doc
 * (docs/plans/57-search-api-wayfinder-backend.md, "Indexing translation"
 * section, lines 181-191) and locked decision 2 (document id is
 * "{index_id}-{item_id}", no site hash for M1 -- ponytail noted there).
 * update_add_commit.json / update_add_nocommit.json in solr-ref/responses/
 * confirm Wayfinder accepts the command-object POST form; those fixtures
 * don't capture doc *shape* (bodies aren't echoed back), so per-field
 * expectations here are derived from FieldMapper's contract instead.
 *
 * @coversDefaultClass \Drupal\search_api_wayfinder\DocumentBuilder
 * @group search_api_wayfinder
 */
class DocumentBuilderTest extends TestCase {

  private function mockField(string $id, string $type, array $values, bool $multiValued = FALSE): FieldInterface {
    $field = $this->createMock(FieldInterface::class);
    $field->method('getFieldIdentifier')->willReturn($id);
    $field->method('getType')->willReturn($type);
    $field->method('getValues')->willReturn($values);
    $field->method('getPropertyPath')->willReturn($id);
    $field->method('getDatasourceId')->willReturn('entity:test');

    // FieldMapper::isMultiValued() reads cardinality from the index's own
    // property-path definitions, not from how many values this item happens
    // to carry -- see FieldMapper's doc comment. The mock shape here is
    // deliberately realistic (issue #81): a real content-entity field is
    // list-by-construction (isList() unconditionally TRUE, matching
    // BaseFieldDefinition/FieldConfigBase in core) -- the single/multi
    // signal actually lives in field-storage cardinality, not isList().
    $storage = $this->createMock(FieldStorageDefinitionInterface::class);
    $storage->method('getCardinality')->willReturn(
      $multiValued ? FieldStorageDefinitionInterface::CARDINALITY_UNLIMITED : 1
    );

    $definition = $this->createMock(FieldDefinitionInterface::class);
    $definition->method('isList')->willReturn(TRUE);
    $definition->method('getFieldStorageDefinition')->willReturn($storage);

    $index = $this->createMock(IndexInterface::class);
    $index->method('getPropertyDefinitions')->willReturn([$id => $definition]);
    $field->method('getIndex')->willReturn($index);

    return $field;
  }

  private function mockItem(string $itemId, string $datasourceId, string $language, array $fields): ItemInterface {
    $item = $this->createMock(ItemInterface::class);
    $item->method('getId')->willReturn($itemId);
    $item->method('getDatasourceId')->willReturn($datasourceId);
    $item->method('getLanguage')->willReturn($language);
    $item->method('getFields')->willReturn($fields);
    return $item;
  }

  /**
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandShapeAndStaticFields(): void {
    // Regression for issue #83: real Search API hands `text`-type field
    // values as TextValue objects (TextDataType::getValue()), never plain
    // PHP strings -- a mock that passes a bare string here (as this test
    // did before #83) can't see FieldMapper::formatValue() failing to
    // stringify the object, because a plain string is already a string.
    $item = $this->mockItem('node/1:en', 'entity:node', 'en', [
      'title' => $this->mockField('title', 'text', [new TextValue('Hello world')]),
    ]);

    $builder = new DocumentBuilder(new FieldMapper());
    $command = $builder->buildAddCommand($item, 'my_index');

    $this->assertArrayHasKey('add', $command);
    $this->assertArrayHasKey('doc', $command['add']);
    $doc = $command['add']['doc'];

    // Document id = "{index_id}-{item_id}" (locked decision 2).
    $this->assertSame('my_index-node/1:en', $doc['id']);
    $this->assertSame('my_index', $doc['index_id']);
    $this->assertSame('en', $doc['ss_search_api_language']);
    $this->assertSame('entity:node', $doc['ss_search_api_datasource']);
    // issue #342: text fields are now always tm_X3b_<lang>_<id> (infix
    // always 'm', language from $item->getLanguage()) and ALWAYS written as
    // an array, even single-valued -- FieldMapper::fieldName() forces the
    // 'm' infix for every text-family prefix regardless of cardinality
    // (SearchApiSolrBackend.php:2450-2473).
    $this->assertSame(['Hello world'], $doc['tm_X3b_en_title']);

    // commitWithin is NOT part of this command: Wayfinder's /update parser
    // only reads add.doc from the body -- commitWithin is a query param on
    // POST /update, sent by WayfinderClient::update(), not embedded here.
    $this->assertArrayNotHasKey('commitWithin', $command['add']);
  }

  /**
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandSingleValuedFieldIsScalar(): void {
    $item = $this->mockItem('node/3:en', 'entity:node', 'en', [
      'field_price' => $this->mockField('field_price', 'decimal', [9.99]),
    ]);
    $builder = new DocumentBuilder(new FieldMapper());
    $doc = $builder->buildAddCommand($item, 'my_index')['add']['doc'];

    // Single value -> scalar into the *s_* field name, not a JSON array.
    $this->assertArrayHasKey('fts_field_price', $doc);
    $this->assertSame(9.99, $doc['fts_field_price']);
  }

  /**
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandMultiValuedFieldIsJsonArray(): void {
    $item = $this->mockItem('node/4:en', 'entity:node', 'en', [
      'field_tags' => $this->mockField('field_tags', 'string', ['red', 'blue'], TRUE),
    ]);
    $builder = new DocumentBuilder(new FieldMapper());
    $doc = $builder->buildAddCommand($item, 'my_index')['add']['doc'];

    // Multi-valued -> array into the *m_* field name (plan doc line 185).
    $this->assertSame(['red', 'blue'], $doc['sm_field_tags']);
  }

  /**
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandFormatsBooleanAndDateValues(): void {
    $item = $this->mockItem('node/5:en', 'entity:node', 'en', [
      'field_active' => $this->mockField('field_active', 'boolean', [TRUE]),
      'field_created' => $this->mockField('field_created', 'date', [0]),
    ]);
    $builder = new DocumentBuilder(new FieldMapper());
    $doc = $builder->buildAddCommand($item, 'my_index')['add']['doc'];

    $this->assertSame('true', $doc['bs_field_active']);
    $this->assertSame('1970-01-01T00:00:00Z', $doc['ds_field_created']);
  }

  /**
   * search_api_solr 4.3.13 indexes a deterministic scalar sort copy for text.
   *
   * issue #362: the copy is a SINGLE language-agnostic `sort_<id>` field, not
   * one per language -- Wayfinder has no collation type, so the per-language
   * copies search_api_solr fills would be byte-identical here (see
   * docs/reports/2026-08-12-362-identical-sort-copies.md).
   *
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandAddsDeterministicTextSortCopies(): void {
    $item = $this->mockItem('node/7:en', 'entity:node', 'en', [
      'title' => $this->mockField('title', 'text', [new TextValue('Single title')]),
      'field_body' => $this->mockField(
        'field_body',
        'text',
        [new TextValue('First paragraph'), new TextValue('Second paragraph')],
        TRUE
      ),
    ]);

    $doc = (new DocumentBuilder(new FieldMapper()))->buildAddCommand($item, 'my_index')['add']['doc'];

    // One language-agnostic sort copy per sortable field, carrying the FIRST
    // value -- no sort_X3b_<lang>_* variants exist.
    $this->assertSame('Single title', $doc['sort_title']);
    $this->assertSame('First paragraph', $doc['sort_field_body']);
    $this->assertArrayNotHasKey('sort_X3b_en_title', $doc);
    $this->assertArrayNotHasKey('sort_X3b_und_title', $doc);
  }

  /**
   * Pins #302: a multi-valued text field's sort_* copy takes the FIRST value,
   * matching captured search_api_solr / solr:9 — not the min/max selector the
   * non-text path uses.
   *
   * search_api_solr's own source copies only the first value into sort_*:
   * SearchApiSolrBackend::addIndexField() returns "the first value of $values
   * that has been added to the index" (coverage/.../SearchApiSolrBackend.php,
   * the @return at line 2726), and the caller writes that scalar into each
   * language-specific sort_* field (line 1485). The captured live-solr:9 trace
   * confirms it (`solr-ref/search-api/trace/00001.json`): a document with
   * `sm_field_topics = ["legacy", "documentation"]` indexes as
   * `sort_X3b_en_field_topics = "legacy"` — the first value, not the min
   * (`documentation`) nor the max. Zero sort_* field carries more than one
   * value anywhere in the trace, so Solr's Lucene min/max selector never runs
   * on a text sort field. Recorded as finding #153 in
   * docs/solr-ref-findings.md.
   *
   * The values below are chosen so the first value is NEITHER the min NOR the
   * max: first='mango', min='apple', max='zebra'. A regression that "fixed"
   * this to min/max selection (the tempting wrong fix) would make this test
   * fail, where the sibling test above would not — its ['First paragraph',
   * 'Second paragraph'] happens to sort first==min.
   *
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandMultivaluedTextSortTakesFirstValueNotMinMax(): void {
    $item = $this->mockItem('node/302:en', 'entity:node', 'en', [
      'field_pick' => $this->mockField(
        'field_pick',
        'text',
        // first='mango' is neither the min ('apple') nor the max ('zebra').
        [new TextValue('mango'), new TextValue('apple'), new TextValue('zebra')],
        TRUE
      ),
    ]);

    $doc = (new DocumentBuilder(new FieldMapper()))->buildAddCommand($item, 'my_index')['add']['doc'];

    // issue #362: one language-agnostic sort copy, carrying the FIRST value.
    $this->assertSame('mango', $doc['sort_field_pick']);
  }

  /**
   * Regression test for issue #83, end-to-end through the
   * DocumentBuilder -> FieldMapper path: a multi-valued `text` field whose
   * values are TextValue objects (their real shape from Search API, per
   * TextDataType::getValue()) must produce a doc array of plain strings that
   * json_encode()s to a JSON array of strings -- not '{}' objects, which is
   * the malformed body Wayfinder's /update parser rejected in the #80
   * integration harness ("field tm_body expects a string value, got {}").
   *
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandTextValueFieldSerializesToPlainStringsInJson(): void {
    $item = $this->mockItem('node/6:en', 'entity:node', 'en', [
      'field_body' => $this->mockField(
        'field_body',
        'text',
        [new TextValue('First paragraph'), new TextValue('Second paragraph')],
        TRUE
      ),
    ]);
    $builder = new DocumentBuilder(new FieldMapper());
    $doc = $builder->buildAddCommand($item, 'my_index')['add']['doc'];

    $this->assertSame(['First paragraph', 'Second paragraph'], $doc['tm_X3b_en_field_body']);

    $encoded = json_decode(json_encode($doc), TRUE);
    $this->assertSame(['First paragraph', 'Second paragraph'], $encoded['tm_X3b_en_field_body']);
  }

  /**
   * issue #342: a SINGLE-valued text field must still be written as a
   * one-element JSON array, not a bare scalar -- FieldMapper::fieldName()
   * now forces the 'm' infix for every text-family field regardless of
   * cardinality (SearchApiSolrBackend.php:2450-2473, "$pref .= 'm'
   * . SEPARATOR . $language_id" runs unconditionally), so the generic
   * `$multiValued ? array_values($formatted) : $formatted[0]` branch
   * (DocumentBuilder.php:93) must get a text-specific case that always
   * writes an array.
   *
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandSingleValuedTextFieldIsStillAnArray(): void {
    $item = $this->mockItem('node/342:en', 'entity:node', 'en', [
      'title' => $this->mockField('title', 'text', [new TextValue('Only value')], FALSE),
    ]);

    $doc = (new DocumentBuilder(new FieldMapper()))->buildAddCommand($item, 'my_index')['add']['doc'];

    $this->assertSame(['Only value'], $doc['tm_X3b_en_title']);
  }

  /**
   * An item field with no values is omitted from the document entirely --
   * Solr never receives null/[] for an absent field, and Wayfinder rejects
   * null for a typed field ("field ts_X expects a string value, got null").
   * Surfaced by the #262 file-extraction tracer, where the computed
   * attachment field is empty on every item that has no attachment.
   *
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandOmitsFieldsWithNoValues(): void {
    $item = $this->mockItem('node/8:en', 'entity:node', 'en', [
      'title' => $this->mockField('title', 'text', [new TextValue('Has a value')]),
      'field_optional' => $this->mockField('field_optional', 'string', [], FALSE),
    ]);

    $doc = (new DocumentBuilder(new FieldMapper()))->buildAddCommand($item, 'my_index')['add']['doc'];

    // The populated field is present ...
    $this->assertSame(['Has a value'], $doc['tm_X3b_en_title']);
    // issue #362: one language-agnostic sort copy.
    $this->assertArrayHasKey('sort_title', $doc);
    // ... and the empty optional field is omitted entirely, not null/[], so
    // it can never be rejected as a null for a typed field.
    $this->assertArrayNotHasKey('ss_field_optional', $doc);
  }

  /**
   * Issue #339: TWO solr_text_suggester fields on one item must ACCUMULATE
   * into the fixed sink field `twm_suggest`, not have the second silently
   * overwrite the first via `$doc[$name] = ...` (DocumentBuilder.php:93).
   *
   * search_api_solr does not hit this bug because its Solarium
   * Document::addField() appends to an existing key instead of assigning;
   * search_api_wayfinder's plain array-assign needs its own accumulation
   * path for the one field name every solr_text_suggester field collapses
   * to (FieldMapper::fieldName()).
   *
   * Both cardinalities are exercised deliberately: a single-valued
   * suggester field ('field_suggest_a') and a multi-valued one
   * ('field_suggest_b'), in item-field iteration order, so the assertion is
   * precise about both completeness (no value lost) and ordering (values
   * appear in field order, not sorted or grouped by field).
   *
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandAccumulatesMultipleSuggesterFieldsIntoSinkField(): void {
    $item = $this->mockItem('node/339:en', 'entity:node', 'en', [
      'field_suggest_a' => $this->mockField(
        'field_suggest_a',
        'solr_text_suggester',
        [new TextValue('alpha')],
      ),
      'field_suggest_b' => $this->mockField(
        'field_suggest_b',
        'solr_text_suggester',
        [new TextValue('bravo'), new TextValue('charlie')],
        TRUE
      ),
    ]);

    $doc = (new DocumentBuilder(new FieldMapper()))->buildAddCommand($item, 'my_index')['add']['doc'];

    // Every field of type solr_text_suggester maps to the ONE fixed sink
    // field (FieldMapper::SUGGESTER_SINK_FIELD) -- there must be no
    // per-field dynamic field name alongside it.
    $this->assertArrayHasKey('twm_suggest', $doc);
    $this->assertSame(
      ['alpha', 'bravo', 'charlie'],
      $doc['twm_suggest'],
      'twm_suggest must accumulate every solr_text_suggester field value, in item-field iteration order -- not have the last field silently overwrite the first.'
    );
  }

  /**
   * Issue #339: `twm_suggest` is ALWAYS a JSON array, even for a single
   * solr_text_suggester field with cardinality 1 -- the preset declares it
   * multi_valued = true (presets/search-api.toml:99-103), so a one-element
   * array is what a single-valued suggester field must produce, not a bare
   * scalar (which is what today's generic single/multi branch in
   * DocumentBuilder::buildAddCommand() produces for every other type).
   *
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandSingleSuggesterFieldIsOneElementArray(): void {
    $item = $this->mockItem('node/339-1:en', 'entity:node', 'en', [
      'field_suggest' => $this->mockField(
        'field_suggest',
        'solr_text_suggester',
        [new TextValue('only value')],
      ),
    ]);

    $doc = (new DocumentBuilder(new FieldMapper()))->buildAddCommand($item, 'my_index')['add']['doc'];

    $this->assertSame(['only value'], $doc['twm_suggest']);
  }

  /**
   * Issue #339: a solr_text_suggester field with zero values must not create
   * a `twm_suggest` key at all -- the empty-value omission rule
   * (DocumentBuilder.php:63) applies to the sink field exactly like any
   * other field; accumulation must not turn "no values contributed" into an
   * empty array being written to the doc.
   *
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandOmitsSinkFieldWhenSuggesterFieldHasNoValues(): void {
    $item = $this->mockItem('node/339-2:en', 'entity:node', 'en', [
      'field_suggest_empty' => $this->mockField(
        'field_suggest_empty',
        'solr_text_suggester',
        [],
      ),
    ]);

    $doc = (new DocumentBuilder(new FieldMapper()))->buildAddCommand($item, 'my_index')['add']['doc'];

    $this->assertArrayNotHasKey('twm_suggest', $doc);
  }

  /**
   * Issue #339: solr_text_suggester fields alongside a plain 'text' field and
   * another typed field must not disturb the other fields' normal shape, and
   * must NOT get a sort_* copy themselves. The sort-copy gate (issue #358)
   * is the mapped name's first character -- 't' or 's' -- and the suggester
   * sink ('twm_suggest') is routed away by its own accumulation branch above
   * before that gate ever runs, so it can never qualify.
   *
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandSuggesterFieldsDoNotAffectOtherFieldsOrGetSortCopy(): void {
    $item = $this->mockItem('node/339-3:en', 'entity:node', 'en', [
      'title' => $this->mockField('title', 'text', [new TextValue('Plain title')]),
      'field_price' => $this->mockField('field_price', 'decimal', [9.99]),
      'field_suggest_a' => $this->mockField(
        'field_suggest_a',
        'solr_text_suggester',
        [new TextValue('one')],
      ),
      'field_suggest_b' => $this->mockField(
        'field_suggest_b',
        'solr_text_suggester',
        [new TextValue('two')],
        TRUE
      ),
    ]);

    $doc = (new DocumentBuilder(new FieldMapper()))->buildAddCommand($item, 'my_index')['add']['doc'];

    // Other fields keep their current shape exactly.
    $this->assertSame(['Plain title'], $doc['tm_X3b_en_title']);
    // issue #362: one language-agnostic sort copy.
    $this->assertSame('Plain title', $doc['sort_title']);
    $this->assertSame(9.99, $doc['fts_field_price']);

    // The sink field accumulates both suggester fields.
    $this->assertSame(['one', 'two'], $doc['twm_suggest']);

    // No sort_* field for either suggester field id, and no per-field
    // dynamic field name leaking through for them either.
    $this->assertArrayNotHasKey('sort_field_suggest_a', $doc);
    $this->assertArrayNotHasKey('sort_field_suggest_b', $doc);
    $this->assertArrayNotHasKey('twm_field_suggest_a', $doc);
    $this->assertArrayNotHasKey('twm_field_suggest_b', $doc);
    $this->assertArrayNotHasKey('tws_field_suggest_a', $doc);
    $this->assertArrayNotHasKey('tws_field_suggest_b', $doc);
  }

  /**
   * issue #342: solr_text_spellcheck accumulates into its `spellcheck_<lang>`
   * sink exactly the way solr_text_suggester accumulates into `twm_suggest`
   * (DocumentBuilder.php:71-90, issue #339) -- two solr_text_spellcheck
   * fields on one item, both with item language 'en', must not overwrite
   * each other via a plain `$doc[$name] = ...` assign, since both map to the
   * SAME sink field name `spellcheck_en`
   * (FieldMapper::fieldName('...', 'solr_text_spellcheck', ..., 'en') ==
   * 'spellcheck_en' regardless of field id/cardinality).
   *
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandAccumulatesMultipleSpellcheckFieldsIntoLanguageSink(): void {
    $item = $this->mockItem('node/342-sc1:en', 'entity:node', 'en', [
      'field_spellcheck_a' => $this->mockField(
        'field_spellcheck_a',
        'solr_text_spellcheck',
        [new TextValue('alpha')],
      ),
      'field_spellcheck_b' => $this->mockField(
        'field_spellcheck_b',
        'solr_text_spellcheck',
        [new TextValue('bravo'), new TextValue('charlie')],
        TRUE
      ),
    ]);

    $doc = (new DocumentBuilder(new FieldMapper()))->buildAddCommand($item, 'my_index')['add']['doc'];

    $this->assertArrayHasKey('spellcheck_en', $doc);
    $this->assertSame(
      ['alpha', 'bravo', 'charlie'],
      $doc['spellcheck_en'],
      'spellcheck_en must accumulate every solr_text_spellcheck field value, in item-field iteration order.'
    );
  }

  /**
   * issue #342: a single solr_text_spellcheck field with cardinality 1 must
   * still produce a one-element JSON array in its language sink -- the same
   * "sink is always an array" rule #339 established for twm_suggest.
   *
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandSingleSpellcheckFieldIsOneElementArray(): void {
    $item = $this->mockItem('node/342-sc2:en', 'entity:node', 'en', [
      'field_spellcheck' => $this->mockField(
        'field_spellcheck',
        'solr_text_spellcheck',
        [new TextValue('only value')],
      ),
    ]);

    $doc = (new DocumentBuilder(new FieldMapper()))->buildAddCommand($item, 'my_index')['add']['doc'];

    $this->assertSame(['only value'], $doc['spellcheck_en']);
  }

  /**
   * issue #342: a solr_text_spellcheck field with zero values must not
   * create its language sink key at all -- the empty-value omission rule
   * (DocumentBuilder.php:63) applies to this sink exactly like twm_suggest.
   *
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandOmitsSpellcheckSinkFieldWhenFieldHasNoValues(): void {
    $item = $this->mockItem('node/342-sc3:en', 'entity:node', 'en', [
      'field_spellcheck_empty' => $this->mockField(
        'field_spellcheck_empty',
        'solr_text_spellcheck',
        [],
      ),
    ]);

    $doc = (new DocumentBuilder(new FieldMapper()))->buildAddCommand($item, 'my_index')['add']['doc'];

    $this->assertArrayNotHasKey('spellcheck_en', $doc);
  }

  /**
   * issue #342: solr_text_spellcheck and solr_text_suggester are two
   * DIFFERENT fixed sinks (spellcheck_<lang> vs twm_suggest) -- one field of
   * each type on the same item must accumulate independently, not collide.
   *
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandSpellcheckAndSuggesterSinksDoNotCollide(): void {
    $item = $this->mockItem('node/342-sc4:en', 'entity:node', 'en', [
      'field_spellcheck' => $this->mockField(
        'field_spellcheck',
        'solr_text_spellcheck',
        [new TextValue('sc value')],
      ),
      'field_suggest' => $this->mockField(
        'field_suggest',
        'solr_text_suggester',
        [new TextValue('sg value')],
      ),
    ]);

    $doc = (new DocumentBuilder(new FieldMapper()))->buildAddCommand($item, 'my_index')['add']['doc'];

    $this->assertSame(['sc value'], $doc['spellcheck_en']);
    $this->assertSame(['sg value'], $doc['twm_suggest']);
  }

  /**
   * issue #362: a text field carries a SINGLE language-agnostic sort copy,
   * `sort_<id>`, regardless of how many languages the site has enabled.
   * Pre-#362 (the #342 MF-3 behaviour) DocumentBuilder wrote N+1 identical
   * copies -- one per enabled language plus 'und' -- mirroring
   * search_api_solr's fill loop (SearchApiSolrBackend.php:1469-1481). That
   * fill is load-bearing in real Solr only because each copy is typed
   * `collated_<lang>` (different orderings); Wayfinder maps every `sort_*`
   * to plain `string`, so the copies were byte-identical with no ordering
   * benefit. Measured cost: ~30% index overhead on a monolingual site,
   * ~3.5x on an 8-language site (docs/reports/2026-08-12-362-...).
   *
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandWritesASingleLanguageAgnosticSortCopy(): void {
    $item = $this->mockItem('node/362-1:de', 'entity:node', 'de', [
      'title' => $this->mockField('title', 'text', [new TextValue('Deutscher Titel')]),
    ]);

    $doc = (new DocumentBuilder(new FieldMapper()))->buildAddCommand($item, 'my_index')['add']['doc'];

    // Exactly one sort copy, carrying the first value. No per-language
    // sort_X3b_<lang>_* variants exist any more -- not the item's own 'de',
    // not 'und', not any other site language.
    $this->assertSame('Deutscher Titel', $doc['sort_title']);
    $this->assertArrayNotHasKey('sort_X3b_de_title', $doc);
    $this->assertArrayNotHasKey('sort_X3b_und_title', $doc);
    $this->assertArrayNotHasKey('sort_X3b_en_title', $doc);
  }

  /**
   * First write wins: upstream's fill loop guards each write with
   * `if (!$doc->{$key})` (SearchApiSolrBackend.php:1479), so a later field
   * must not overwrite an earlier sort copy for the same key. Wayfinder's
   * sort key is the field id (`sort_<field-id>`, issue #362), so two
   * DIFFERENT Search API fields never naturally collide on one sort key the
   * way upstream's generic `$name`-keyed loop can; this test uses a
   * synthetic double -- two mock fields that both report the SAME field
   * identifier ('title') via getFieldIdentifier(), simulating the only way
   * such a collision can occur -- to exercise the guard directly.
   *
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandSortCopyFirstWriteWinsOnAKeyCollision(): void {
    $item = $this->mockItem('node/342-mf3-2:en', 'entity:node', 'en', [
      'title_a' => $this->mockField('title', 'text', [new TextValue('First')]),
      'title_b' => $this->mockField('title', 'text', [new TextValue('Second')]),
    ]);

    $doc = (new DocumentBuilder(new FieldMapper()))->buildAddCommand($item, 'my_index')['add']['doc'];

    // Both fields map to the same sort_title key; the FIRST field's value
    // must win, not be silently overwritten by the second.
    $this->assertSame('First', $doc['sort_title']);
  }

  /**
   * Issue #358: a SINGLE-valued string field gets a sort_* copy -- a string
   * field whose mapped name begins with 's' gets the same first-value sort
   * copy a text field does, because upstream's gate is a first-character
   * test on the mapped name ('t' or 's'), not a text-only check
   * (SearchApiSolrBackend.php:1448-1454). The live trace
   * (solr-ref/search-api/trace/00001.json) indexes `ss_field_sku =
   * "ART-001"` with `sort_X3b_en_field_sku = "ART-001"`; issue #362 keeps
   * the copy but makes it a single language-agnostic `sort_field_sku` (a
   * deliberate divergence from the trace's per-language naming -- Wayfinder
   * has no collation type, so the per-language copies are redundant).
   *
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandAddsSortCopyForSingleValuedStringField(): void {
    $item = $this->mockItem('node/358a:en', 'entity:node', 'en', [
      'field_sku' => $this->mockField('field_sku', 'string', ['ART-001']),
    ]);

    $doc = (new DocumentBuilder(new FieldMapper()))
      ->buildAddCommand($item, 'capture_index')['add']['doc'];

    // The mapped field keeps its ordinary single-valued scalar shape ...
    $this->assertSame('ART-001', $doc['ss_field_sku']);
    // ... and gets a single language-agnostic sort copy (issue #362).
    $this->assertSame('ART-001', $doc['sort_field_sku']);
    $this->assertArrayNotHasKey('sort_X3b_en_field_sku', $doc);
  }

  /**
   * Issue #358: a MULTI-valued string field's sort_* copy takes the FIRST
   * value, exactly like a text field's (finding #153) -- not the min/max
   * selector the non-text path uses.
   *
   * The live trace confirms it (solr-ref/search-api/trace/00001.json):
   * `sm_field_keywords = ["animals", "classic", "pangram"]` indexes as
   * `sort_X3b_en_field_keywords = "animals"` (the first value), and
   * `sm_field_topics = ["legacy", "documentation"]` as
   * `sort_X3b_en_field_topics = "legacy"`. No sort_* field carries more than
   * one value anywhere in the trace.
   *
   * The values below are chosen so the first value is NEITHER the min NOR the
   * max: first='mango', min='apple', max='zebra'. A regression that took the
   * min or max would fail here.
   *
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandMultivaluedStringSortCopyTakesFirstValueNotMinMax(): void {
    $item = $this->mockItem('node/358b:en', 'entity:node', 'en', [
      'field_pick' => $this->mockField(
        'field_pick',
        'string',
        // first='mango' is neither the min ('apple') nor the max ('zebra').
        ['mango', 'apple', 'zebra'],
        TRUE
      ),
    ]);

    $doc = (new DocumentBuilder(new FieldMapper()))
      ->buildAddCommand($item, 'capture_index')['add']['doc'];

    // The mapped multi-valued field stays an array ...
    $this->assertSame(['mango', 'apple', 'zebra'], $doc['sm_field_pick']);
    // ... and its single language-agnostic sort copy is the FIRST value
    // (issue #362).
    $this->assertSame('mango', $doc['sort_field_pick']);
  }

  /**
   * Issue #358: a field type whose mapped name does NOT begin with 't' or 's'
   * still gets no sort_* copy. The trace confirms it for integer
   * (`its_field_rating`, no sort copy), and the same holds for decimal, date
   * and boolean -- they sort on their own mapped fast field, where Wayfinder's
   * native min/max selection is what Solr does. This guards that broadening
   * the sort-copy gate to 't'/'s' did not sweep in the other prefixes.
   *
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandIneligibleTypeGetsNoSortCopy(): void {
    $item = $this->mockItem('node/358c:en', 'entity:node', 'en', [
      'field_rating' => $this->mockField('field_rating', 'integer', [5]),
    ]);

    $doc = (new DocumentBuilder(new FieldMapper()))
      ->buildAddCommand($item, 'capture_index')['add']['doc'];

    $this->assertSame(5, $doc['its_field_rating']);
    $this->assertArrayNotHasKey('sort_field_rating', $doc);
  }

  /**
   * Issue #358 regression: broadening the sort-copy gate to string fields must
   * not disturb text's sort copy on the same branch. A text field and a string
   * field on one item must EACH get their sort_* copy (text unchanged from
   * #342, string newly added), while the first-value/first-write rules hold
   * for both.
   *
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandTextAndStringFieldsEachGetSortCopies(): void {
    $item = $this->mockItem('node/358d:en', 'entity:node', 'en', [
      'title' => $this->mockField('title', 'text', [new TextValue('Hello')]),
      'field_sku' => $this->mockField('field_sku', 'string', ['SKU-1']),
    ]);

    $doc = (new DocumentBuilder(new FieldMapper()))
      ->buildAddCommand($item, 'capture_index')['add']['doc'];

    // Text: unchanged -- array value, single language-agnostic sort copy
    // (issue #362).
    $this->assertSame(['Hello'], $doc['tm_X3b_en_title']);
    $this->assertSame('Hello', $doc['sort_title']);
    // String: newly gets the same shape of sort copy.
    $this->assertSame('SKU-1', $doc['ss_field_sku']);
    $this->assertSame('SKU-1', $doc['sort_field_sku']);
  }

  /**
   * SPEC-385 deliverable E / test 16: `suggest.cfq` on a Suggester lookup
   * filters on the document's `sm_context_tags` (src/core_index.rs:4859), and
   * this module currently indexes no such field -- so without this, any
   * context-filtered Suggester lookup returns nothing. Mirrors
   * SearchApiSolrBackend.php:1343-1347 minus the site-hash tag (premise 6:
   * this module has no site hash, DocumentBuilder.php:15). `sm_*` is a
   * multi-valued dynamic field (presets/search-api.toml:115-116), so this
   * asserts an array of exactly two tags. Values are hand-computed from
   * FieldMapper's own documented `encodeSolrName()` transform (`;` ->
   * `_X3b_`, `/` -> `_X2f_`, `:` -> `_X3a_`) -- the real output, not a
   * substring match, per the spec's explicit instruction.
   *
   * @covers ::buildAddCommand
   */
  public function testBuildAddCommandIndexesContextTagsForIndexAndLanguage(): void {
    $item = $this->mockItem('node/1:en', 'entity:node', 'en', [
      'title' => $this->mockField('title', 'text', [new TextValue('Hello world')]),
    ]);

    $doc = (new DocumentBuilder(new FieldMapper()))
      ->buildAddCommand($item, 'my_index')['add']['doc'];

    $this->assertSame(
      ['search_api_X2f_index_X3a_my_index', 'drupal_X2f_langcode_X3a_en'],
      $doc['sm_context_tags']
    );
  }

}
