# Server configuration inventory

Source provenance: public server configuration structs and defaults in
[`src/config.rs`](../../src/config.rs). This inventory is mechanically checked.
Canonical syntax and operational explanation are in
[Configuration](../../docs/CONFIGURATION.md); [Deployment](../../docs/DEPLOYMENT.md)
shows fail-closed `WAYFINDER_CONFIG` preflight.

Every key is optional. A missing server-config file preserves the current program
behavior of selecting defaults; a present unreadable file errors. Unknown keys
are rejected. `auth` is parsed separately so credentials are not rendered in
configuration diagnostics.

| Key | Default | Unit | Validation | Lifecycle and effect |
|---|---|---|---|---|
| `strict_params` | `false` | boolean | boolean | Live at startup; reject request names not in the route allowlist. |
| `[auth]` | absent | table | only `username` and `password` | Optional HTTP Basic configuration table. |
| `[indexing]` | defaults below | table | known keys only | Index-writer and merge settings. |
| `[query]` | defaults below | table | known keys only | Request limit settings. |
| `[resources]` | defaults below | table | known keys only | Request and index resource settings. |
| `[commit]` | defaults below | table | known keys only | Autocommit settings. |
| `[admin]` | defaults below | table | known keys only | Reported-version setting. |
| `[extraction]` | defaults below | table | known keys only | Multipart extraction budgets. |
| `auth.username` | unset | string | nonempty, no `:`, no ASCII controls; requires password | Live at startup; enables HTTP Basic with password. |
| `auth.password` | unset | secret string | nonempty, no ASCII controls; requires username | Live at startup; enables HTTP Basic with username. |
| `indexing.writer_heap` | `32000000` | bytes | integer | Startup; IndexWriter arena budget. |
| `indexing.writer_threads` | `1` | count | at least 1 | Startup; IndexWriter thread count. |
| `indexing.merge_policy` | `"log"` | enum | `log` or `no_merge` | Startup; merge policy. |
| `indexing.merge_min_layer_size` | unset | bytes | unsigned integer | Startup; log-policy setting, ignored under `no_merge`. |
| `indexing.merge_level_log_size` | unset | ratio | floating point | Startup; log-policy setting, ignored under `no_merge`. |
| `query.time_allowed` | unset | milliseconds | unsigned integer | Parsed but inert. |
| `query.rows_limit` | `10000` | rows | unsigned integer | Startup; clamps request `rows`. |
| `query.facet_limit_max` | `1000` | buckets | unsigned integer | Startup; clamps `facet.limit`. |
| `resources.doc_store_compression` | `"lz4"` | enum | `none` or `lz4` | Index creation only; existing index retains its setting. |
| `resources.doc_store_blocksize` | `16384` | bytes | unsigned integer | Index creation only; existing index retains its setting. |
| `resources.searcher_pool_size` | `1` | count | unsigned integer | Parsed but inert; Tantivy has no fixed pool. |
| `resources.max_body_size` | `10000000` | bytes | unsigned integer | Startup; general request-body ceiling. |
| `extraction.max_body_bytes` | `33554432` | bytes | unsigned integer | Startup; multipart content budget. |
| `extraction.max_concurrency` | `4` | count | unsigned integer | Startup; concurrent extraction budget. |
| `extraction.max_inflight_uploads` | `8` | count | unsigned integer; `0` rejects all uploads | Startup; upload intake budget. |
| `extraction.max_output_scalars` | `10000000` | Unicode scalars | unsigned integer | Startup; extracted-text scalar budget. |
| `extraction.max_output_bytes` | `40000000` | bytes | unsigned integer | Startup; extracted-text byte budget. |
| `extraction.deadline_secs` | `30` | seconds | unsigned integer; `0` expires immediately | Startup; extraction deadline. |
| `commit.autocommit_max_docs` | unset | documents | unsigned integer | Startup; commits when pending-document threshold is reached. |
| `commit.autocommit_max_time` | unset | milliseconds | unsigned integer | Startup; commits after pending-write age threshold. |
| `admin.reported_server_version` | `"9.0.0"` | version string | string | Startup; reported server version. |
| `admin.reported_solr_version` | alias of `admin.reported_server_version` | version string | string | Compatibility alias; same effect. |
