# Field and analyzer reference

`schema.toml` describes one core. A field value must match its declared type:
`string`/`keyword` are one raw term; `text_general` tokenizes/lowercases;
`text_en` also removes English stopwords and Porter-stems; `text_<code>`
stems only the shipped language set. `int`/`long` are signed 64-bit; `float`/
`double` are 64-bit floats; `date` is RFC 3339 UTC. JSON null is rejected, not
stored as an absent value. Scalars are required for single-valued fields; a
one-item array is unwrapped, while a longer array is 400. `multi_valued=true`
returns an array even for one value. `required=true` rejects a missing field;
`stored` controls retrieval and `fast` is required for compatible sorts/facets.

Special types: `location` and `location_rpt` encode a latitude/longitude point
(the latter has the bounded heatmap path); `date_range` retains interval text
and synthetic bounds, not a scalar date; `boost_term_payload` accepts bounded
payload text of final `term|float` tokens for `{!payload_score}` only.

## Fields, dynamics, copies, synonyms

Unknown names are rejected. A dynamic pattern is `*`, `prefix_*`, or `*_suffix`.
The longest matching pattern wins; a static field always wins. Dynamic values
live in `_dynamic`/`_dynamic_text`, are rewritten for request paths, and dotted
names require reindexing when their persisted encoding changes. Copy fields
apply at index time: the destination analyzes the source raw value with its own
type, can trigger its own cardinality constraint, and changing a copy rule only
affects new documents.

Query synonyms are durable groups in `<data-dir>/synonyms.txt`; `POST
/ui/synonyms` atomically replaces them for future query analysis. They do not
rewrite postings or require linked fetches. This is not a Solr synonym factory:
there are no Solr XML analyzer imports or open-ended `solr_text_custom` families.

## Custom analysis and migration

A custom `[[field_types]]` has `name`, `tokenizer="simple"`, and ordered
`lowercase`, `stopwords`, and/or `stemmer` filters. Stopword/stemmer filters
need a supported language; unsupported stopword lists fail schema load. A
separate `query_tokenizer` plus ordered `query_filters` changes query analysis
only; query filters without it fail load. It does not change existing postings.
Built-in names, `_version_`, and dynamic catch-all reservations cannot be
shadowed. Shipped analyzed presets use their fixed UAX-style tokenization,
accent folding, case normalization, word-delimiter splitting/catenation,
stopwords, and stemming in a type-specific order; these stages are not an
open custom-filter grammar. Static analyzed presets cap tokens at 32,766
Unicode scalars, while Search API suggest/payload chains use their documented
1–100 or 2–100 bounds. The shared analyzed dynamic catch-all retains its own
versioned chain, so declaring a dynamic rule with another analyzed type does
not create an independent Tantivy analyzer. Verify index and query tokens with
the cited tests before migration.

**State-change lifecycle.** Prerequisites: validate TOML and retain the prior
schema/data set. Visibility: a schema takes effect only on successful startup;
new index-analyzer/copy behavior is visible only for subsequently indexed data.
Durability: the persisted schema/analyzer contract is checked at startup.
Failure: incompatibility is fail-closed; do not edit its metadata. Retry with a
fresh data directory when instructed. Validation: index and query a type- and
cardinality-representative document. Rollback: stop the candidate and return to
the retained schema/data set. Static-field changes, first/last dynamic rule,
and changes to/from location, location_rpt, date_range, or boost_term_payload
require fresh data and reindex; analyzer-contract refusal is authoritative.

Advanced executable evidence: `tests/schema_layer.rs`,
`tests/analyzer_index_query_split.rs`, and `tests/dotted_dynamic_fields.rs`.
