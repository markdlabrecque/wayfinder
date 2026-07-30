<?php

declare(strict_types=1);

namespace Drupal\Tests\search_api_wayfinder\Unit;

use Drupal\search_api_wayfinder\FieldMapper;
use PHPUnit\Framework\TestCase;

/**
 * Tests FieldMapper: SA field/type -> Wayfinder dynamic field name mapping,
 * and per-type index-time value formatting.
 *
 * Expected prefixes are copied from search_api_solr 4.4.0's
 * Utility::getDataTypeInfo() (vendor/drupal/search_api_solr/src/Utility/Utility.php,
 * lines 66-94: text=>'t', string=>'s', integer=>'it', decimal=>'ft',
 * date=>'d', boolean=>'b'), cross-checked against the dynamic_fields patterns
 * declared in presets/search-api.toml (ss_*, sm_*, ts_*, tm_*, its_*, itm_*,
 * ds_*, dm_*, bs_*, bm_*). The plan doc (docs/plans/57-search-api-wayfinder-backend.md,
 * locked decision 1) requires this exact naming.
 *
 * @coversDefaultClass \Drupal\search_api_wayfinder\FieldMapper
 * @group search_api_wayfinder
 */
class FieldMapperTest extends TestCase {

  /**
   * @covers ::fieldName
   * @dataProvider fieldNameProvider
   */
  public function testFieldName(string $fieldId, string $type, bool $multiValued, string $expected): void {
    $mapper = new FieldMapper();
    $this->assertSame($expected, $mapper->fieldName($fieldId, $type, $multiValued));
  }

  public static function fieldNameProvider(): array {
    return [
      // string -> ss_ / sm_ (presets/search-api.toml lines 78-90).
      'string single' => ['field_tags', 'string', FALSE, 'ss_field_tags'],
      'string multi' => ['field_tags', 'string', TRUE, 'sm_field_tags'],
      // text -> ts_ / tm_ (presets/search-api.toml lines 93-103).
      'text single' => ['title', 'text', FALSE, 'ts_title'],
      'text multi' => ['title', 'text', TRUE, 'tm_title'],
      // integer -> its_ / itm_ (search_api_solr prefix 'it' + s|m; preset lines 135-147).
      'integer single' => ['weight', 'integer', FALSE, 'its_weight'],
      'integer multi' => ['weight', 'integer', TRUE, 'itm_weight'],
      // decimal -> fts_ / ftm_ (search_api_solr prefix 'ft' + s|m).
      'decimal single' => ['price', 'decimal', FALSE, 'fts_price'],
      'decimal multi' => ['price', 'decimal', TRUE, 'ftm_price'],
      // date -> ds_ / dm_ (preset lines 180-192).
      'date single' => ['created', 'date', FALSE, 'ds_created'],
      'date multi' => ['created', 'date', TRUE, 'dm_created'],
      // boolean -> bs_ / bm_ (preset lines 195-207; Wayfinder maps this to
      // its 'string' type server-side, but the field *name* prefix stays 'b').
      'boolean single' => ['status', 'boolean', FALSE, 'bs_status'],
      'boolean multi' => ['status', 'boolean', TRUE, 'bm_status'],
    ];
  }

  /**
   * @covers ::formatValue
   * @dataProvider formatValueProvider
   */
  public function testFormatValue(string $type, $value, $expected): void {
    $mapper = new FieldMapper();
    $this->assertSame($expected, $mapper->formatValue($value, $type));
  }

  public static function formatValueProvider(): array {
    return [
      // Dates: Unix timestamp (Search API's internal representation for the
      // 'date' type) -> ISO 8601 UTC, per plan doc line 154-155.
      'date epoch zero' => ['date', 0, '1970-01-01T00:00:00Z'],
      'date epoch nonzero' => ['date', 1700000000, '2023-11-14T22:13:20Z'],
      // Booleans: literal "true"/"false" strings (plan doc line 155-157,
      // presets/search-api.toml header comment lines 14-19: Wayfinder has no
      // boolean type, search_api_solr already sends these as JSON strings).
      'boolean true' => ['boolean', TRUE, 'true'],
      'boolean false' => ['boolean', FALSE, 'false'],
      // Text/string: passed through as-is.
      'string as-is' => ['string', 'Some Value', 'Some Value'],
      'text as-is' => ['text', 'Some fulltext body', 'Some fulltext body'],
      // Numerics: bare, no quoting/formatting.
      'integer bare' => ['integer', 42, 42],
      'decimal bare' => ['decimal', 3.14, 3.14],
    ];
  }

}
