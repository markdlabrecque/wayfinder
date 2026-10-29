# wayfinder-bench

`wf-bench` measures Wayfinder against Solr 9 on speed, cost and correctness
(issue #440). It indexes the same Wikipedia articles into both engines, runs
the same queries against each, checks that Wayfinder's answers match Solr's,
and folds every measurement into one score.

This is a standalone crate, not a workspace member of the root `wayfinder`
package. The benchmark is run by hand and is not part of CI. CI does run the
crate's own unit tests.

## Requirements

- Linux with cgroup v2. CPU and memory are read from each container's cgroup
  under `/sys/fs/cgroup`.
- `docker`, with the current user allowed to run containers.
- `cargo`, `curl` and `sha256sum`.
- At least 8 cores. Engines are pinned to cores `2-5` and `wf-bench` itself
  to `6-7` (`CPUSET` and `DRIVER_CPUSET` in `src/bin/wf-bench.rs`).

## Running it

From the repo root:

```sh
bench/fetch-wiki.sh       # once: ~1.7 GB of Wikipedia parquet into bench/data/
cargo build --release     # the Wayfinder binary wf-bench runs
cargo run --release --manifest-path bench/Cargo.toml --bin wf-bench -- --docs 10000
```

`fetch-wiki.sh` downloads pinned shards and checks each one by SHA-256.
Re-running it skips shards that are already there. Pass a shard count (1-4)
to fetch fewer.

`wf-bench` uses the release binary at `target/release/wayfinder`. Rebuild it
after every change you want measured.

### Flags

| Flag | Default | What it does |
|---|---|---|
| `--docs N` | 10000 | Articles indexed into each engine. Use 10000 for a quick check and 500000 for a full run. |
| `--iters N` | 200 | Timed requests per query shape, after warm-up. |
| `--cold N` | 5 | Cold restarts per engine. Each one times the first request of every shape. |
| `--force` | off | Run even when the machine is busy. The warning is saved in the JSON. |
| `--noise K` | off | Measure the same Wayfinder build K times (at least 2) and save each metric's min and max to `bench/out/noise.json`. Prints the score's spread and exits. |
| `--accept` | off | Save a passing run's score, answer fingerprints and protected-file hashes as the baseline. |
| `--accept-protected` | off | For a person only: save the protected files' hashes as they stand, after reviewing a change to them. Measures nothing. |

### Protected files

`solr-ref/responses/`, `allowed-diffs.tsv`, `queries.tsv`, `weights.tsv` and
the scoring code decide the score, so the tuning loop may not change them
(`PROTECTED` in `src/protected.rs`). `--accept` saves their hashes to
`bench/out/wf-protected.json`. A run whose protected files differ prints
`FAIL protected: <files>` and exits 1 before measuring anything, until a
person runs `--accept-protected`.

### Noise

The same build scores a little differently each run. Run `--noise 5` on the
accepted build to measure how much. After that, every run labels its change
from the accepted score: `win` or `loss` only when the change is bigger than
the score's spread (max minus min) over the noise runs, `noise` otherwise.
Before `noise.json` exists the label reads `noise not measured`.

### Tuning loop

[TUNING.md](TUNING.md) has the steps for an agent that runs the benchmark
over and over, tries a speed-up each time, and keeps only the wins.

### What a run does

1. Checks the protected files against the accepted hashes, then runs the
   repo's `cargo test`. If either fails, `wf-bench` prints `FAIL protected`
   or `FAIL tests` and exits 1. A build that breaks the tests is never scored.
2. Checks the 1-minute load average. Above 2.0 it exits 2, unless `--force`
   is given.
3. Starts Solr (`solr:9`, pinned by digest, 2 GB heap) in Docker. Then it
   starts Wayfinder in Docker, using the host-built binary. Only one engine
   runs at a time. Both get the same cores, a 4 GB memory limit and host
   networking.
4. For each engine: indexes the docs, times every query shape in
   `queries.tsv`, measures how long a new doc takes to become searchable,
   runs the load test, then restarts the engine cold.
5. Compares every timed Wayfinder answer with Solr's. A query listed in
   `allowed-diffs.tsv` is reported as waived and does not fail the run.
6. Writes the full detail and prints one summary line.

## Output

Stdout is one line, so a loop can read the result cheaply. For example:

```
score 0.812 (wf/solr, <1 = wf wins) | p50 0.91 p99 0.77 cpu 0.85 mem 0.14 | correct 15/15 | PASS | bench/out/1761700000.json
```

Any waived or failing query gets a line of its own after it. The run exits 1
if any query fails the correctness check.

The full detail goes to `bench/out/<unix-ts>.json` (gitignored): every metric
for both engines, each metric's Wayfinder/Solr ratio under `ratios`, the
failures, and the environment (kernel, CPU, core split, Wayfinder git sha,
Solr version and image).

## Score

Each metric becomes a Wayfinder/Solr ratio. Throughput ratios are inverted, so
lower is always better for Wayfinder. Metrics measured per query shape or per
load level are first averaged into one ratio each. The score is the weighted
geometric mean of those ratios, using the weights in `weights.tsv`. Below 1.0
means Wayfinder wins.

## What it measures

| Metric | How |
|---|---|
| Warm latency p50/p95/p99 | Per query shape, after 300 warm-up requests. Each request uses new terms, so Solr's result cache cannot answer it. |
| Repeat latency | The same URL 100 times, so caches are hot. |
| First query | The first request of each shape right after indexing. |
| Cold latency p50/p95/p99 | Stop the container, evict its index from the page cache (`posix_fadvise`, no root needed), start it again, and time the first request of each shape. |
| CPU per 1,000 queries | `usage_usec` from the container's `cpu.stat`. |
| Load | Searches per second and latency with 1, 8 and 32 users at once. |
| Startup | Container start to the first answered search. |
| Index size | Size on disk of Solr's Lucene `data/index` (no tlog) and Wayfinder's data dir. |
| Indexing speed | Docs and bytes per second, from the first batch to the final commit. |
| Add to searchable | Add one doc, then poll until a search finds it. |
| Memory | Idle, post-index and peak `anon` from the cgroup's `memory.stat`, so disk cache does not count. Disk cache and `memory.peak` are reported separately. |

`wf-bench` does not build the shipped container image. To check the image
size, use the gated live test:

```sh
WAYFINDER_BENCH_DOCKER=1 cargo test --manifest-path bench/Cargo.toml --test dockerfile_build
```

## Files

- `fetch-wiki.sh`: downloads the corpus into `bench/data/` (gitignored).
- `queries.tsv`: the query shapes, as `name<TAB>mode<TAB>params`. `{t}`-style
  placeholders are filled from corpus terms with a fixed seed (`src/terms.rs`).
- `allowed-diffs.tsv`: queries whose differences from Solr are known and
  accepted, each pointing at a section of `docs/COMPATIBILITY.md`.
- `weights.tsv`: how much each metric counts in the score.
- `ideas.tsv`: append-only log of every idea the tuning loop tried, with its
  score change and whether it was kept or dropped. See `TUNING.md`.
- `src/bin/wf-bench.rs`: the driver. The library modules under `src/` hold the
  measurement and scoring logic, each covered by a test in `tests/`.
- `out/`: run results (gitignored).

## Tests

```sh
cargo test --manifest-path bench/Cargo.toml
```

These tests need no Docker or network. `dockerfile_build.rs` only runs when
`WAYFINDER_BENCH_DOCKER=1` is set.
