# Wayfinder

A Solr-wire-compatible search backend in Rust, built on Tantivy: one static
binary, one schema file, one data directory. No JVM, no ZooKeeper, no config-set
upload. See `docs/PRD.md` for scope and phases.

## Running

```sh
wayfinder <schema.toml> <data-dir> [bind-addr]     # bind-addr defaults to 127.0.0.1:8983
```

Two config files, deliberately separate:

| File | Scope | How it is passed |
|---|---|---|
| `schema.toml` | one core: fields, types, unique key | first CLI argument |
| `wayfinder.toml` | the server: tuning knobs (PRD §6) | `WAYFINDER_CONFIG` env var |

Neither is Solr's config format — Wayfinder matches Solr's *wire* API (param
names, semantics, JSON envelope), never its XML configuration.

## Server config (`wayfinder.toml`)

Every knob is optional and every section can be omitted. **No config file at all
means all defaults**, so the server runs with none. Unknown keys are a hard
error: this file is operator-facing, and a typo that silently no-ops is how a
tuning knob "stops working" without anyone noticing. (Request params go the other
way — Solr ignores unknown ones, and so does Wayfinder unless you set
`strict_params`.)

```toml
# Reject unknown request params with a 400 instead of ignoring them.
# Default false, matching real Solr, which serves such requests normally.
# Turn it on in development to find params Wayfinder does not implement yet.
strict_params = false

[indexing]
# IndexWriter arena size in bytes, across all writer threads. The main
# indexing-throughput lever. Tantivy requires ~15 MB per writer thread.
writer_heap = 32000000
# Indexing thread count. Default 1: a single writer thread allocates doc ids in
# insertion order, which is what Wayfinder's score tie-break relies on to match
# Solr's observed ordering of equally scored matches. Raise it for bulk load.
writer_threads = 1
# "log" (Tantivy's LogMergePolicy) or "no_merge" for bulk loading.
merge_policy = "log"
# LogMergePolicy parameters; both optional, ignored under "no_merge".
# merge_min_layer_size = 10000
# merge_level_log_size = 0.75

[query]
# Per-query time budget in ms. Parsed and exposed but NOT yet enforced --
# Tantivy has no query deadline. Setting it today has no effect.
# time_allowed = 5000
# Hard cap on `rows`, so a bad client cannot ask for a million documents.
# Solr has no equivalent request cap, so an over-limit request is clamped to
# this value rather than rejected -- a clamp keeps a working client working.
rows_limit = 10000
# Hard cap on `facet.limit`. Parsed and exposed; applied once `facet.limit`
# itself lands (that param is not implemented yet).
facet_limit_max = 1000

[resources]
# Stored-field compression: "none" or "lz4". Applied when the index is first
# created -- re-opening an existing index keeps the settings it was built with,
# so changing this on a populated data dir requires a reindex.
doc_store_compression = "lz4"
doc_store_blocksize = 16384
# Accepted but inert: Tantivy 0.26 creates searchers on demand rather than from
# a fixed pool, so there is nothing to size. Kept because PRD §6 names it.
searcher_pool_size = 1

[commit]
# Hard-commit thresholds. Parsed and exposed; the update pipeline that consumes
# them is not built yet, so they currently have no effect.
# autocommit_max_docs = 10000
# autocommit_max_time = 60000
```

No heap tuning knob exists, by design: Tantivy is mmap-based and the OS page
cache does the work Solr's heap sizing does. The absence is a feature.

### Which knobs are live today

| Live | Parsed and exposed, not yet acted on |
|---|---|
| `strict_params`, `query.rows_limit` | `query.time_allowed` |
| `indexing.writer_heap`, `writer_threads`, `merge_policy` (+ its params) | `query.facet_limit_max` |
| `resources.doc_store_compression`, `doc_store_blocksize` | `commit.autocommit_max_docs`, `autocommit_max_time` |
| | `resources.searcher_pool_size` (no Tantivy equivalent) |

## Tests

```sh
cargo test          # hermetic: no network, no Docker
```

Reference fixtures captured from a real Solr 9 live in `solr-ref/responses/` and
are the ground truth for envelope shape; `solr-ref/capture.sh` regenerates them
against `solr:9` in Docker. `docs/solr-ref-findings.md` records what those
captures proved.
