# Configuration

Wayfinder uses two independent TOML files:

| File | Scope | Input |
|---|---|---|
| `schema.toml` | One core: fields, analyzers, and mappings | First CLI argument |
| `wayfinder.toml` | Process-level tuning and security | `WAYFINDER_CONFIG` |

Neither file uses Solr configuration syntax.

## Schema

```toml
[core]
name = "content"
unique_key = "id"
default_field = "body"

[[fields]]
name = "id"
type = "string"
stored = true
required = true

[[fields]]
name = "title"
type = "text_en"
stored = true

[[fields]]
name = "body"
type = "text_en"
stored = true

[[fields]]
name = "category"
type = "string"
fast = true
multi_valued = true

[[dynamic_fields]]
pattern = "*_i"
type = "int"
stored = true
fast = true

[[copy_fields]]
source = "title"
dest = "body"
```

### Field options

| Option | Meaning |
|---|---|
| `stored` | Retrievable through `fl` and returned in response documents |
| `required` | Reject documents missing the field |
| `fast` | Tantivy fast field; required for sorting and faceting |
| `multi_valued` | Render as a JSON array even when only one value is present |

### Field types

| Type | Behavior |
|---|---|
| `string`, `keyword` | One exact, unanalyzed term |
| `text_general` | Tokenized and lowercased |
| `text_en` | Lowercase, English stopword removal, and Porter-compatible stemming |
| `text_<code>` | Lowercase and stem for `ar da nl en fi fr de el hu it no pt ro ru es sv ta tr` |
| `int`, `long` | Signed 64-bit integer |
| `float`, `double` | 64-bit float |
| `date` | RFC 3339 UTC timestamp |
| `location` | Latitude/longitude point with synthetic fast columns for supported spatial queries |
| `location_rpt` | Latitude/longitude point with the same encoding and the supported heatmap boundary |
| `date_range` | Interval-valued date with verbatim value plus synthetic start/end columns; not a scalar date |
| `boost_term_payload` | Payload-bearing text for the bounded `{!payload_score}` evaluator; tokens use a final `|<float>` payload |

Static `text_en` fields apply the captured Porter terminal-`y` behavior (`day` becomes `dai`, while
`sky` remains `sky`). The shared `_dynamic_text` catch-all retains the v1 Snowball behavior because
Tantivy gives analyzed dynamic rules one catch-all analyzer regardless of their declared text type.

### Custom analyzers

```toml
[[field_types]]
name = "text_en_custom"
tokenizer = "simple"

[[field_types.filters]]
kind = "lowercase"

[[field_types.filters]]
kind = "stopwords"
language = "english"

[[field_types.filters]]
kind = "stemmer"
language = "english"
```

Filters run in declaration order. Supported kinds are `lowercase`, `stopwords`, and `stemmer`.
Tantivy has no stopword list for Arabic, Greek, Romanian, Tamil, or Turkish; requesting one is a
load-time error. `tokenizer` supports `simple` for custom types.

A custom type can separately declare `query_tokenizer` and ordered `query_filters`; they accept
the same tokenizer and filter options as the index-side `tokenizer` and `filters`. Omitting
`query_tokenizer` uses the index analyzer at query time. `query_filters` without `query_tokenizer`
is a load-time error. A query-side chain changes query analysis only; it never changes existing
postings.

Custom type names cannot shadow built-ins. `_version_` is always reserved. `_dynamic` and
`_dynamic_text` are reserved whenever dynamic rules exist. Duplicate field names, custom type
names, and dynamic patterns are load-time errors.

### Dynamic and copy fields

A dynamic pattern is `*`, `*_suffix`, or `prefix_*`. The longest matching pattern wins; a static
field always wins over a dynamic match. Unknown document fields are rejected.

Dynamic values are stored internally in `_dynamic` or `_dynamic_text` JSON catch-alls and are
rewritten to ordinary field paths for queries and responses.

Copy fields apply at index time. The destination analyzes the source's raw value using its own
field type. Changing copy rules affects only newly indexed documents.

### Schema changes

Wayfinder stores the schema beside the index and refuses startup when the configured Tantivy
schema is incompatible. Adding, removing, or changing a static field requires a fresh data
directory and reindex. Adding the first dynamic rule or removing the last does too, because that
adds or removes the catch-all fields.

Copy rules and custom analyzer definitions are not structural, but index-analyzer changes affect
only newly indexed content. Query-only `query_tokenizer`/`query_filters` changes do not rewrite
postings. Persisted analyzer-contract checks may still require reindexing when old and new
query-time analysis would disagree. Changing to or from `location`, `location_rpt`, `date_range`,
or `boost_term_payload` is a static-field migration and requires a fresh data directory and
reindex. Follow the startup error rather than editing persisted schema metadata.

The mechanically checked [manual schema inventory](../manual/reference/schema.md) lists every
built-in type, custom analyzer option, and migration boundary.

`presets/search-api.toml` supplies the Drupal Search API dynamic-field convention.

## Server configuration

Every key is optional. A missing file selects all defaults. Unknown keys are hard errors.

```toml
strict_params = false

# [auth]
# username = "operator"
# password = "replace-with-a-secret"

[indexing]
writer_heap = 32000000
writer_threads = 1
merge_policy = "log"       # "log" or "no_merge"
# merge_min_layer_size = 10000
# merge_level_log_size = 0.75

[query]
# time_allowed = 5000      # accepted but not enforced
rows_limit = 10000
facet_limit_max = 1000

[resources]
doc_store_compression = "lz4" # "none" or "lz4"
doc_store_blocksize = 16384
searcher_pool_size = 1          # accepted but inert
max_body_size = 10000000

[extraction]
max_body_bytes = 33554432
max_concurrency = 4
max_inflight_uploads = 8
max_output_scalars = 10000000
max_output_bytes = 40000000
deadline_secs = 30

[commit]
# autocommit_max_docs = 10000
# autocommit_max_time = 60000 # milliseconds

[admin]
reported_server_version = "9.0.0"
```

### Behavior and limits

- `strict_params` rejects request names outside each route's allowlist. Accepted names may still be
  deliberately inert or have documented limits.
- `[auth]` enables HTTP Basic authentication. Both values are required and nonempty; usernames
  cannot contain `:` and neither component can contain ASCII controls. See
  [DEPLOYMENT.md](DEPLOYMENT.md) before sending credentials across a network.
- `writer_heap` is the total IndexWriter arena budget. Tantivy requires roughly 15 MB per writer
  thread. One writer thread is the default because insertion-order document IDs provide stable
  score tie-breaking; raise it for bulk loading.
- `merge_policy = "no_merge"` is intended for controlled bulk-loading scenarios. Log policy
  parameters are ignored under it.
- `rows_limit` and `facet_limit_max` clamp oversized requests rather than rejecting them.
- Document-store settings apply only when creating an index. Changing them for an existing data
  directory has no effect; reindex to apply them.
- `max_body_size` is the general Axum request-body cap. Extraction uses its own transport,
  concurrency, output, and deadline budgets.
- Either autocommit threshold commits pending writes and makes them visible. Omit a threshold to
  disable that trigger.
- `reported_solr_version` remains accepted as an alias for `reported_server_version`.
- Wayfinder has no heap-size knob: Tantivy is mmap-based and relies on the OS page cache.

### Live knobs

| Live | Parsed but intentionally inert |
|---|---|
| `strict_params`, `auth.username`, `auth.password` | `query.time_allowed` |
| `indexing.writer_heap`, `writer_threads`, `merge_policy`, `merge_min_layer_size`, `merge_level_log_size` | `resources.searcher_pool_size` |
| `query.rows_limit`, `query.facet_limit_max` | |
| `resources.doc_store_compression`, `doc_store_blocksize`, `max_body_size` | |
| `extraction.max_body_bytes`, `max_concurrency`, `max_inflight_uploads`, `max_output_scalars`, `max_output_bytes`, `deadline_secs` | |
| `commit.autocommit_max_docs`, `autocommit_max_time` | |
| `admin.reported_server_version` (`reported_solr_version` alias) | |
