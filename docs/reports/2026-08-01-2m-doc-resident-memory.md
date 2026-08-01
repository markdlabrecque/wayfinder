# Issue #234: 2M document resident-memory benchmark

## Approved spec

Ran `N_QUERIES=200 bash bench/run.sh 42 2000000` on a local Docker Desktop/OrbStack host, not dedicated hardware.

## Implementation

- Fixed the benchmark image build by copying the compile-time `templates/` and `coverage/` inputs into the Docker builder stage.
- Made the generated Notes section distinguish literal 2M runs from smaller runs.
- Added regression coverage for the generated 2M caveats.
- Populated `docs/benchmarks.md` from the successful run.

## Evidence

The first run failed because the Dockerfile omitted compile-time templates and the coverage contract. The existing live Docker test reproduced the failure; after the Dockerfile fix, `WAYFINDER_BENCH_DOCKER=1 cargo test --manifest-path bench/Cargo.toml --test dockerfile_build` passed.

| Engine | Idle RSS | Maximum RSS under load | p95 query latency | Index size |
|---|---:|---:|---:|---:|
| Solr | 866.2 MB | 960.7 MB | 10.42 ms | 583.2 MB |
| Wayfinder | 2133.8 MB | 2839.7 MB | 93.04 ms | 330.8 MB |

Wayfinder missed the `< 500 MB` target. RSS increased by 705.9 MB between the post-index idle sample and the maximum sampled during query load. This is a temporal comparison only: the harness cannot distinguish allocator-resident memory from mmap-backed index pages. Solr uses container `docker stats`; Wayfinder uses native-process `ps`, so overhead is not directly comparable.

Full gate passed:

- `cargo fmt --check`
- `cargo fmt --check --manifest-path bench/Cargo.toml`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`
- `cargo test --manifest-path bench/Cargo.toml`

## Review

Round 1 requested neutral phase-to-phase RSS wording instead of causal attribution to queries. Round 2 approved the correction; the scoped bench gate passed 33/33 tests.

No accepted deviations. The unresolved result is the failed performance target and the documented measurement limitations, not an implementation blocker for this benchmark report.
