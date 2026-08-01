# Issue #243: truthful idle resident-memory measurement

## Scope

Correct the benchmark harness and report so the PRD's `< 50 MB` idle target is tested against a freshly started, configured engine rather than the RSS retained after indexing. Preserve the post-index measurement as a separate capacity-planning data point for both Wayfinder and Solr. Memory usage itself is unchanged and remains out of scope.

## Changed behavior

- `bench/run.sh` samples three distinct RSS phases for both engines:
  1. startup idle, after health/schema readiness and before indexing;
  2. post-index, after the final commit and before query load;
  3. maximum sampled during query load.
- Solr's startup sample occurs after its benchmark fields have been added successfully, matching Wayfinder's configured-and-ready state.
- `render_report` carries all three values independently and the table gives the `< 50 MB` target only to startup idle. The post-index row has no invented PRD baseline or target.
- Every measured resident-memory row states that RSS includes allocator-resident memory plus mmap-backed index pages.
- Hermetic source-contract tests pin sample ordering and the positional `render_report` argument contract.

## Corrected 2M-document evidence

Command:

```sh
N_QUERIES=200 bash bench/run.sh 42 2000000
```

The corrected run generated `docs/benchmarks.md` with:

| Phase | Solr | Wayfinder |
|---|---:|---:|
| Startup idle | 736.7 MB | 9.0 MB |
| Post-index, before query load | 845.2 MB | 2153.8 MB |
| Maximum during query load | 915.4 MB | 2161.5 MB |

Wayfinder therefore meets the PRD startup-idle target. The much larger post-index RSS remains visible but is no longer presented as the target's idle measurement. The harness cannot attribute post-index RSS between allocator retention and mmap-backed index pages.

## Verification

Initial red evidence:

- `cargo test --manifest-path bench/Cargo.toml --test run_sh_memory_phases` failed because no startup sample existed between readiness and indexing.
- `cargo test --manifest-path bench/Cargo.toml --test results_table` failed because the data model/table had no distinct post-index phase.

Final gates passed:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo fmt --check --manifest-path bench/Cargo.toml
cargo clippy --manifest-path bench/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path bench/Cargo.toml
```

Mutation evidence: renaming the Wayfinder startup assignment made `run_sh_memory_phases` fail; the mutation was reverted. The review-bounce positional-contract guard was also confirmed to fail when a Solr phase variable was swapped.

## Review

- Round 1 requested that Solr be schema-configured before its startup sample and that the positional report wiring be guarded. Both were fixed, then the 2M benchmark was rerun.
- Round 2 verified production ordering, report wiring, corrected report semantics, and a full green gate. It identified that the source-order test could still pass if sampling moved into the schema failure branch. The guard now requires the failure branch to close before `SOLR_STARTUP_IDLE_MB` is assigned; the full bench fmt/clippy/test gate passed afterward.
- No unresolved implementation risks or deferred follow-ups remain in this issue. Memory reduction remains explicitly out of scope.
