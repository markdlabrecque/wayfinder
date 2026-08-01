# Issue #231: graceful shutdown and logging

Issue: [#231](https://github.com/markdlabrecque/wayfinder/issues/231)

## Approved spec

On SIGTERM or Ctrl-C, stop accepting work, drain in-flight requests, then flush pending `commitWithin`/autocommit writes. Add tracing configured by `RUST_LOG`, INFO request logs, and DEBUG logs for ignored parameters. Keep the existing argv contract and coverage CLI unchanged.

## Verified premise

`CoreIndex::schedule_commit` arms an asynchronous deadline. The existing `commitWithin=60000` test proves a document is immediately invisible, so termination before that deadline creates a real data-loss window.

## Implementation

- `Cargo.toml` and `Cargo.lock`: enabled Tokio signal handling and Tower HTTP tracing; added `tracing` and `tracing-subscriber`.
- `src/lib.rs`: added `AppServer` and `ShutdownHandle` so the process can flush the same core after Axum drains; added `TraceLayer` INFO request logging and DEBUG ignored-parameter logging.
- `src/main.rs`: initializes `RUST_LOG` tracing, handles SIGTERM/Ctrl-C graceful shutdown, and flushes after draining without changing argv or the coverage CLI.
- `tests/ops_shutdown.rs`: added Unix process integration coverage for SIGTERM shutdown and commit flushing.

Accepted deviation: flushing is unconditional rather than gated on `pending_docs`, because delete-only scheduled updates are not represented by `pending_docs`. This prevents their scheduled changes from being lost.

## Test progression and evidence

- Initial `cargo test --test ops_shutdown` — RED as expected: the process exited with signal 15 before graceful shutdown support.
- `cargo test --test ops_shutdown` — passed.
- `cargo test --test search_api_coverage coverage_command_requires_complete_deterministic_contract_schema_and_output` — passed.
- `cargo test --test server_config` — passed.
- `cargo test` — passed.
- `cargo fmt --check` — passed.
- `cargo clippy --all-targets -- -D warnings` — passed.
- Independent Reviewer `cargo test` — passed.

## Review

Reviewer verdict: **APPROVE**. No findings or follow-ups. The unconditional flush was reviewed and accepted for delete-only scheduled updates.

## Risks and scope limits

- The process integration test is `cfg(unix)`; the non-Unix Ctrl-C path is compile-covered only.
- No readiness endpoint, shutdown metrics, or other operational controls were added.
