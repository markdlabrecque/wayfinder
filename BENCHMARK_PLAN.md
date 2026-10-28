# Benchmark plan: Wayfinder vs Solr

Goal: a repeatable benchmark that an agent loop can run many times to hill-climb Wayfinder
until it matches, then beats, Solr on speed and efficiency. Correctness is a hard gate: a fast
wrong answer is a failed run.

Tick boxes as pieces land. Each phase ends in something that runs end to end.

## Starting point

`bench/` already exists (issue #13): a standalone pure-std crate with a deterministic synthetic
corpus, plus `bench/run.sh` (bash + curl). It measures startup RSS, post-index RSS, RSS under
load, cold start, warm and cold p95, image size, and index size. Last run: `bench/RESULTS.md`.

Known gaps for this goal:

- Synthetic corpus, one query shape (facet+filter+highlight).
- Wayfinder runs native, Solr runs in Docker, so overhead differs between the two.
- Only p95. No CPU cost, throughput, indexing rate, or visibility latency.
- No answer comparison against Solr, only HTTP status.
- Long human-readable output. Nothing a loop can read cheaply.

## Design rules

- **Rust driver.** Lives in `bench/`, run as `cargo run --release -p ...` or a built binary.
  No JS, Python, or k6 runtime in the loop.
- **Token-cheap output.** Default stdout is one line:
  `score 0.82 | p99 -4% | cpu -2% | rss +1% | correct 40/40 | PASS`.
  More lines only for regressions or failures. Full detail goes to `bench/out/<run>.json`.
- **Solr is measured once per corpus/config and cached** (`bench/out/solr-baseline.json`).
  Loop runs measure Wayfinder only, unless the baseline is missing or stale.
- **Equal footing.** Both engines run in Docker with the same `--cpus`, `--cpuset-cpus`, and
  `--memory`.
- **Noise-aware.** Repeat runs, report medians, and treat a change as a win only when it beats
  the measured noise band.
- **Outside `cargo test`.** Repo tests stay hermetic. The bench needs Docker and is run by hand
  or by the loop.

## Checklist

### Phase 0: Tracer bullet (one query, both engines, one line out)

- [x] GitHub issues filed: #440 (harness, claimed), #441 (results). Branch `440-perf-bench`.
- [x] Rust driver binary in `bench/` (`src/bin/wf-bench.rs`).
- [x] Start Solr and Wayfinder in Docker with identical resource limits.
- [x] Index a small fixed doc set into both.
- [x] Run one query N times against each and record latencies.
- [x] Compare the two answers (numFound + top ids).
- [x] Print the one-line summary and write the JSON detail file.

### Phase 1: Real corpus

- [ ] Pick the source: a fixed English Wikipedia slice (around 500k articles). Pin the exact
      dump or dataset revision.
- [ ] Download script: fetch once into `bench/data/` (gitignored) and verify by SHA-256.
- [ ] Converter: dump to JSON docs with `id`, `title`, `body`, plus derived facet-friendly
      fields (e.g. `category`, `length_bucket`, a date field, a numeric field).
- [ ] Shared schema for both engines: Wayfinder config plus Solr managed-schema fields.
- [ ] Size presets: `small` (~10k, smoke test) and `full` (~500k).

### Phase 2: Query suite (shared features only)

- [ ] Query file `bench/queries.tsv`: name, params, comparison mode.
- [ ] Pick about 10 to 15 shapes from the Solr-compatible surface both engines support:
  - [ ] single term
  - [ ] multi-term OR / AND
  - [ ] phrase
  - [ ] edismax with `qf` boosts
  - [ ] filter query (`fq`)
  - [ ] sort by field
  - [ ] deep paging (`start` large)
  - [ ] field facet
  - [ ] range facet
  - [ ] highlighting
  - [ ] grouping
  - [ ] stats
  - [ ] `*:*` match-all with rows
- [ ] Query terms sampled from the corpus with a fixed seed: common, mid, and rare terms.

### Phase 3: Correctness gate

- [ ] `cargo test` must pass (frozen fixtures in `solr-ref/responses/`).
- [ ] Per-query comparison against Solr's answer:
  - [ ] `numFound` equal
  - [ ] fixed sort: same ids in the same order
  - [ ] relevance sort: top-10 overlap at or above a threshold (start at 0.8)
  - [ ] facet buckets and counts equal
  - [ ] stats values equal (float tolerance)
  - [ ] highlight: a snippet exists for each doc that should have one
- [ ] Allowed-differences file `bench/allowed-diffs.tsv`, each line pointing at a section of
      `docs/COMPATIBILITY.md`. Anything else that differs fails the run.
- [ ] Wayfinder answers are hashed per query. A hash change is flagged even when Solr
      comparison still passes, so the loop can't drift quietly.

### Phase 4: Metrics

- [ ] Latency p50 / p95 / p99, warm (after warm-up batch).
- [ ] Latency p50 / p95 / p99, cold: fresh container start plus OS page cache evicted for the
      index files (`posix_fadvise(DONTNEED)`, no root needed), first query of each shape.
- [ ] Throughput: requests per second at fixed concurrency (e.g. 1, 8, 32).
- [ ] CPU seconds per 1,000 queries (from cgroup `cpu.stat`). This is the main efficiency number.
- [ ] Memory: idle RSS, post-index RSS, peak RSS under load (cgroup `memory.current` / `memory.peak`).
- [ ] Indexing throughput (docs per second, full corpus, same batch size for both).
- [ ] Commit-to-visible latency (add a doc, poll until searchable).
- [ ] Index size on disk.
- [ ] Startup time to first successful query.

### Phase 5: Scoring

- [ ] Per metric: ratio Wayfinder / Solr (lower is better, inverted for throughput).
- [ ] Score = geometric mean of ratios across queries and metrics. Below 1.0 = Wayfinder wins.
- [ ] Weights file so key metrics (warm p99, CPU per query) can count more.
- [ ] Noise band: run the same build several times, store the spread, and require a change to
      beat it before it counts.
- [ ] Baseline file of the last accepted Wayfinder run, so each loop step reports a delta.

### Phase 6: Stable environment

- [ ] Pin CPUs for each container, and pin the load generator to separate cores.
- [ ] Fixed Solr config: stock `solr:9`, default caches on, fixed heap.
- [ ] Record machine info and both versions in the JSON.
- [ ] Refuse to run (or warn) when the machine is busy (load average check).

### Phase 7: The hill-climb loop

- [ ] Loop instructions (prompt or skill): read the one-line result, pick a target, change code,
      run the gates, keep or revert.
- [ ] Guardrails: never edit `solr-ref/responses/`, `bench/allowed-diffs.tsv`, query files,
      or scoring without a human.
- [ ] Each kept win is a commit with the score delta in the message.
- [ ] Log of tried ideas (kept and rejected) so later loops don't repeat them.
- [ ] Stop rule: stop after N failed attempts in a row, or when the score target is hit.

### Phase 8: Cleanup

- [ ] Retire or fold in the old `bench/run.sh` and `bench/RESULTS.md` once the new driver
      covers their metrics.
- [ ] Update `bench/README.md`.

## Decisions

- **Machine:** the dedicated Linux desktop (16 cores, 60 GB). Hill-climb runs happen when it is quiet.
- **Wins:** the loop stacks wins as commits on one branch. One PR at the end.
- **Data:** real (Wikipedia slice), not synthetic.
- **Toolchain:** rustup installed in `~/.cargo` (no Rust was on the machine). Use
  `export PATH=$HOME/.cargo/bin:$PATH`.
- **How Wayfinder runs:** host-built glibc release binary (`target/release/wayfinder`),
  bind-mounted into a pinned `archlinux:base` container (glibc 2.44 matches the host). Not the
  shipped musl/scratch image: building musl needs `musl-gcc`, which isn't installed, and
  rebuilding the image per loop step is slow. Allocator differences between glibc and musl
  are a known gap in what this measures.
- **How both run:** `--network host`, same `--cpuset-cpus 2-5`, same `--memory 4g`, one engine at
  a time. CPU and memory come from each container's cgroup files.
- **Solr:** `solr:9` pinned by digest, `SOLR_HEAP=2g`, port 18983. Wayfinder on 18984.

## Running it

```
export PATH=$HOME/.cargo/bin:$PATH
cargo build --release                       # Wayfinder, from repo root
cargo build --release --manifest-path bench/Cargo.toml --bin wf-bench
bench/target/release/wf-bench --docs 2000 --iters 100
```

Detail lands in `bench/out/<ts>.json` (gitignored).

## Known issues to fix in later phases

- Memory uses cgroup `memory.current`, which counts page cache (Solr idles at ~2.4 GB). Phase 4
  should split anon memory from file cache (`memory.stat`).
- Solr gets only 20 warm-up requests, so its JIT is not fully warm. Phase 4 needs a real
  warm-up (time- or count-based, per query).
- Tiny synthetic corpus: first run (2k docs) shows Wayfinder far ahead on everything. Not
  meaningful until Phase 1 and 2 land.
- `check()` in `wf-bench.rs` has no unit test yet. Add one (and mutation-test it) in Phase 3.

## Open questions

- (fill in as they come up)

## Session log

- 2026-10-28: plan written. Issues #440 / #441 filed. Phase 0 done: `bench/src/bin/wf-bench.rs`
  runs both engines in Docker, one edismax query, answer check, one-line output. First run
  (2k docs, 100 iters): `score 0.029 ... correct 1/1 | PASS`.
