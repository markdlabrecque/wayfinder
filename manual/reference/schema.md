# Schema and analyzer inventory

Source provenance: built-in declarations and `FieldTypeConfig` in
[`src/schema.rs`](../../src/schema.rs). This table is mechanically checked.
Canonical schema syntax and migration rules are in
[Configuration](../../docs/CONFIGURATION.md).

## Built-in field types

| Built-in type | Bounded behavior |
|---|---|
| `string` | One exact, unanalyzed term. |
| `keyword` | One exact, unanalyzed term. |
| `text_general` | Tokenized and lowercased text. |
| `text_en` | Lowercase, English stopwords, and Porter-compatible stemming. |
| `int` | Signed 64-bit integer. |
| `long` | Signed 64-bit integer. |
| `float` | 64-bit float. |
| `double` | 64-bit float. |
| `date` | RFC 3339 UTC timestamp. |
| `location` | Latitude/longitude point stored through synthetic fast columns; supports documented spatial queries. |
| `location_rpt` | Latitude/longitude point with the same encoding; supports the documented heatmap boundary. |
| `boost_term_payload` | Payload-bearing text: whitespace tokens, length 2–100, lowercase, duplicate removal, and final `\|<float>` payload. It is for the bounded `{!payload_score}` evaluator. |
| `date_range` | Interval-valued date. Verbatim text is retained and synthetic start/end date columns support documented interval predicates; it is not a scalar date. |
| `text_ar` | Lowercase and Arabic stemming. |
| `text_da` | Lowercase and Danish stemming. |
| `text_nl` | Lowercase and Dutch stemming. |
| `text_fi` | Lowercase and Finnish stemming. |
| `text_fr` | Lowercase and French stemming. |
| `text_de` | Lowercase and German stemming. |
| `text_el` | Lowercase and Greek stemming. |
| `text_hu` | Lowercase and Hungarian stemming. |
| `text_it` | Lowercase and Italian stemming. |
| `text_no` | Lowercase and Norwegian stemming. |
| `text_pt` | Lowercase and Portuguese stemming. |
| `text_ro` | Lowercase and Romanian stemming. |
| `text_ru` | Lowercase and Russian stemming. |
| `text_es` | Lowercase and Spanish stemming. |
| `text_sv` | Lowercase and Swedish stemming. |
| `text_ta` | Lowercase and Tamil stemming. |
| `text_tr` | Lowercase and Turkish stemming. |

## Custom analyzer options

A `[[field_types]]` entry has required `name` and `tokenizer`, optional ordered
`filters`, optional `query_tokenizer`, and optional ordered `query_filters`.
Only `simple` is a custom tokenizer. Supported filter `kind` values are
`lowercase`, `stopwords`, and `stemmer`; the latter two require `language` (an
ISO-639-1 code or language name). `query_filters` without `query_tokenizer` is
a load-time error. A declared query chain is used only at query time, under the
same analyzer identity as the index chain; it never changes indexed terms.

```toml
[[field_types]]
name = "text_query_split"
tokenizer = "simple"
query_tokenizer = "simple"

[[field_types.filters]]
kind = "lowercase"

[[field_types.query_filters]]
kind = "lowercase"
```

## Migration implications

Adding, removing, or changing a static field requires a fresh data directory
and reindex. The first dynamic rule adds catch-all fields and likewise requires
a fresh data directory; removing the last rule does too. Changes to a custom
index analyzer affect only newly indexed content and can require reindexing if
the persisted analyzer contract rejects them. A query-only analyzer change does
not rewrite postings, but compatibility checks remain authoritative. `location`,
`location_rpt`, and `date_range` create synthetic columns, so changing to or
from those types is a static schema migration. `boost_term_payload` has a
specialized index and query contract; treat changes to or from it as a reindex
migration.
