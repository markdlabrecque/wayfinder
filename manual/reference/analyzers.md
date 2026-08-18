# Custom analyzer option inventory

Source provenance: public `FieldTypeConfig` and `FilterConfig` declarations in
[`src/schema.rs`](../../src/schema.rs). This table is mechanically checked.
Canonical syntax, query-side behavior, and migration rules are in
[Configuration](../../docs/CONFIGURATION.md).

| Option | Bounded grammar and effect |
|---|---|
| `field_types.name` | Required custom type name; it must not shadow a built-in type. |
| `field_types.tokenizer` | Required index-side tokenizer; only `simple` is accepted. |
| `field_types.filters` | Optional ordered index-side filters; each filter runs in declaration order. |
| `field_types.query_tokenizer` | Optional query-side tokenizer; only `simple` is accepted. Omit it to use the index analyzer for queries. |
| `field_types.query_filters` | Optional ordered query-side filters; it is a load-time error without `query_tokenizer`. |
| `field_types.filters.kind` | Required filter kind: `lowercase`, `stopwords`, or `stemmer`. |
| `field_types.filters.language` | Optional for `lowercase`; required by `stopwords` and `stemmer`, with a supported language name or ISO-639-1 code. |

A query-side chain changes query analysis only. It never changes existing
postings; persisted analyzer-contract checks remain authoritative when deciding
whether to reindex.
