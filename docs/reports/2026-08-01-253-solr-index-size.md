# Issue #253: Solr index-size scope

## Approved spec

Correct the index-size comparison so Solr measures `data/index`, excluding
`tlog`. Wayfinder continues to report its full Tantivy/native data directory;
the rendered path discloses its schema/analyzer metadata.

## Implementation

- Changed the Solr benchmark `du` target from `data/` to `data/index`.
- Made both engines' storage scopes explicit in the rendered measurement path.
- Added two regression tests in `bench/tests/index_size_scope.rs`.

## Evidence

- Initial red: `cargo test --manifest-path bench/Cargo.toml --test index_size_scope`
  failed both new tests for the old path and old rendered description.
- Targeted green: `cargo test --manifest-path bench/Cargo.toml --test index_size_scope --test results_table`
  passed 16 tests.
- Full Implementer gate: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test && cargo test --manifest-path bench/Cargo.toml`
  passed.
- Mutation check: temporarily changing `/data/index` back to `/data` made the
  focused source-guard test fail; the implementation was then restored.
- Independent Reviewer: **APPROVED**, no findings. Its root fmt, clippy, and
  test gate passed.

## Deliberate skips

No live 2M benchmark was rerun: this is a deterministic path correction pinned
by a hermetic guard. No fresh benchmark numbers are claimed.

## Deviations and risks

No deliberate deviations. No unresolved risks.
