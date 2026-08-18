# Schema design and analyzers

Treat `schema.toml` as a versioned contract for one core. The canonical syntax,
validation, and migration authority is [Configuration](../../docs/CONFIGURATION.md);
the complete [schema inventory](../reference/schema.md) and [analyzer inventory](../reference/analyzers.md)
name every supported choice.

## Choose fields and input types

Use `string` or `keyword` for exact identifiers and categories; `text_general`
or `text_<language>` for analyzed prose; `int`/`long` and `float`/`double` for
numbers; `date` for RFC 3339 UTC timestamps. Use `location` only for supported
point spatial queries, `location_rpt` only for its constrained heatmap behavior,
and `date_range` only for intervals (not a scalar date). `boost_term_payload` is
only for the bounded payload evaluator. `stored` permits retrieval, `fast` is
required for sort/facet, `required` rejects absent values, and `multi_valued`
renders an array. Input JSON must match the field's type and declared cardinality;
validate representative documents before a bulk load.

Built-in text types supply their documented index and query analysis. `text_en`
uses lowercasing, English stopwords, and Porter-compatible stemming. Do not infer
an analyzer from a Solr field name: the inventory is authoritative.

## Create a custom index and query analyzer

A custom `[[field_types]]` has a unique non-built-in `name`, `tokenizer =
"simple"`, and ordered `lowercase`, `stopwords`, and `stemmer` filters. A
`query_tokenizer` plus `query_filters` creates a query-side chain; omitting it
uses the index analyzer. `query_filters` without `query_tokenizer` is rejected
at load time. Query analysis never rewrites postings, so changing it can alter
matches immediately and compatibility checks can still demand a reindex.

**Prerequisites:** test the intended tokens and retain the current schema/data
rollback set. **Visibility/durability:** index-analyzer changes affect only later
updates; query-only changes take effect after restart, not after old documents
are rewritten. **Failure/retry:** a startup rejection leaves the old process and
index intact; correct the candidate schema and retry against a fresh staging
directory. **Validation:** compare representative queries and counts. **Rollback:**
restart the old binary with its matching schema and data; do not edit persisted
schema metadata.

## Dynamic fields, copy fields, and synonyms

Dynamic patterns are only `*`, `prefix_*`, or `*_suffix`; the longest match wins
unless a static field wins. Unknown fields are rejected. Dynamic values use the
reserved catch-alls, so adding the first or removing the last dynamic rule is a
structural migration. `copy_fields` copies raw source input at index time and
the destination applies its own analyzer; a changed copy rule affects only new
documents.

Query synonyms are separate durable state: `POST /ui/synonyms` atomically
replaces `<data-dir>/synonyms.txt` for future query analysis and does not
reindex. It is a mutable operation, not a generic Solr synonym factory; its
route contract is in [wire routes](../reference/wire-routes.md#wayfinder-ui-routes).
Before posting, require same-origin authenticated operator access and keep a
known-good complete group set. The replacement is visible atomically after
success; an invalid submission is rejected without a partial write. Validate a
known query, and roll back by posting the saved complete group set. The UI is
not an index-time synonym migration.
