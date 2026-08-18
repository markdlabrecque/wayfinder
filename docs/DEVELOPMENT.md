# Development

## Prerequisites

Wayfinder is a Rust project. Use the toolchain pinned by the repository and build from the project
root:

```sh
cargo build
cargo run -- presets/search-api.toml /tmp/wayfinder-data
```

The server defaults to `127.0.0.1:8983`. Set `WAYFINDER_CONFIG` to exercise a server configuration.

## Repository map

| Path | Purpose |
|---|---|
| `src/lib.rs` | Axum routes, handlers, parameter allowlists, and response assembly |
| `src/core_index.rs` | Tantivy index lifecycle, updates, commits, and query execution |
| `src/schema.rs` | Schema TOML, field types, analyzers, and dynamic fields |
| `src/config.rs` | Server configuration and defaults |
| `src/query.rs`, `src/edismax.rs`, `src/function_query.rs` | Query parsing and scoring |
| `src/facet.rs`, `src/json_facet.rs`, `src/stats.rs` | Aggregation paths |
| `src/extract.rs` | In-process document extraction and resource budgets |
| `tests/` | Hermetic integration tests |
| `solr-ref/responses/` | Frozen wire fixtures |
| `presets/` | Shipped schema presets |
| `bench/` | Standalone benchmark harness |

Wayfinder runs one configured core per process. The request path is broadly:

```text
HTTP -> parameter parsing -> query/update pipeline -> Tantivy -> response assembly
```

A core owns one `IndexWriter`; readers reload after commits. Query work stays Tantivy-native, while
wire handlers preserve the documented existing-client envelope.

## Required gates

Run the same commands as CI:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Tests must remain hermetic: no network and no Docker. Tests derived from frozen fixtures take their
expected values from those fixtures, never from implementation output.

Tests come before implementation and must be observed failing for the intended reason. Validation
whose value is rejecting bad input should be mutation-tested by temporarily breaking the guard,
confirming a test catches it, and reverting the mutation.

## Working on the wire

The supported wire is a bounded existing-client contract, not a Solr parity roadmap. Read
[COMPATIBILITY.md](COMPATIBILITY.md) before changing routes, request parameters, envelopes, or
unsupported boundaries.

When implementing a request parameter, add it to the correct allowlist in `src/lib.rs`; otherwise
`strict_params = true` rejects a parameter the handler supports. Preserve fixture-derived
assertions and do not rewrite frozen fixtures to match implementation output.

Historical Solr/client observations used by code comments remain in
[`solr-ref/FINDINGS.md`](../solr-ref/FINDINGS.md). They are evidence, not product scope.

## Schema and configuration changes

Read [CONFIGURATION.md](CONFIGURATION.md). Unknown server-config keys are startup errors. Any new
server knob must have a documented default and be classified as live or intentionally inert.
Schema changes need startup-compatibility coverage; Tantivy cannot add fields to an existing index.

## Benchmarks

The benchmark harness is a separate dependency-free Rust crate and requires Docker and `curl`:

```sh
bash bench/run.sh             # seed 42, 50,000 documents
bash bench/run.sh 42 2000000  # full corpus; long-running
```

It generates a deterministic corpus, measures native Wayfinder against a Solr 9 container, and
writes [`bench/RESULTS.md`](../bench/RESULTS.md). `N_QUERIES` controls the load-test count and
`SOLR_HOST_PORT` avoids host-port collisions. Benchmarks are manual and non-hermetic; they are not
part of `cargo test`.

Current measured results are retained in `bench/RESULTS.md`. RSS includes mmap-backed index pages,
so it does not isolate allocator memory from the OS page cache; compare environments and cache
conditions before drawing conclusions from absolute values.

## Pull requests

Every change goes through a feature branch and PR. Use conventional, ASCII-only commit messages
such as `feat(schema): ...`, `fix(config): ...`, and `docs: ...`. Rebase onto current `main` and
rerun all gates before merging when other work has landed.
