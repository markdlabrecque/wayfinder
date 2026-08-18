# wayfinder-bench

Benchmark tooling for issue #13 (PRD §8: Conformance & benchmarking).
Standalone crate (`pure std`, no dependencies) -- not a workspace member of
the root `wayfinder` package. It has two jobs:

1. Generate a deterministic synthetic corpus (`src/corpus.rs`), so a
   benchmark run is reproducible byte-for-byte given the same seed and size.
2. Turn raw measurements into the `bench/RESULTS.md` table
   (`src/results.rs`), matching PRD §8's target table row-for-row.

## Running the 50k benchmark

```
bash bench/run.sh            # seed 42, 50,000 docs (default)
bash bench/run.sh 42 50000   # explicit
```

Requires `docker`, `curl`, and `cargo` on `PATH`. One command:

- generates the corpus + a matching schema (id/title/body/category, the
  same field shape `tests/edismax.rs` and `tests/common/mod.rs` already use),
- builds a native release Wayfinder binary and starts it,
- builds the repo-root `Dockerfile` image (for the image-size metric),
- indexes the corpus into Wayfinder, capturing resident-memory samples at
  startup-idle, post-index/pre-query, and under-load phases; runs a
  facet+filter+highlight query load, measures p95 latency, and measures
  on-disk index size,
- does the same against a real Solr 9 container,
- renders `bench/RESULTS.md` from the real measured numbers via
  `wayfinder_bench::results::render_markdown_table`.

Environment variables:

- `N_QUERIES` (default 200): number of queries in the load-test loop.
- `SOLR_HOST_PORT` (default 18983): host port Solr's container publishes on.
  Override if it collides with another Solr container already running
  (this repo's other worktrees each run their own on their own port).

## Running the 2M-doc corpus

Not automated as a single command -- generating, indexing, and holding a
2M-doc corpus is a multi-minute-to-multi-hour operation depending on
hardware, and the harness above is meant to be a quick local sanity check as
well as the full run. To run it:

```
bash bench/run.sh 42 2000000
```

The same script handles it (`gen_corpus` batches regardless of corpus size),
it just takes much longer end-to-end -- expect the corpus generation,
indexing (both engines), and Solr's own startup/GC behavior at that scale to
dominate wall-clock time. Increase `N_QUERIES` if you want a tighter p95
estimate under sustained load at that scale.

## Layout

- `src/corpus.rs` -- deterministic corpus generator (`generate`,
  `content_hash`) and the `Doc` type.
- `src/results.rs` -- `p95` (nearest-rank, 1-indexed), `render_markdown_table`,
  `BenchmarkResults`, `EngineMeasurements`.
- `src/bin/gen_corpus.rs` -- writes a generated corpus as batched
  Solr-update-shaped JSON files plus a matching schema TOML.
- `src/bin/render_report.rs` -- takes raw measurement scalars + latency
  sample files and writes `bench/RESULTS.md`.
- `run.sh` -- the end-to-end orchestration script described above.
- `tests/` -- see each file's module doc for what it covers; `dockerfile_build.rs`
  is gated behind `WAYFINDER_BENCH_DOCKER=1` (mirrors
  `WAYFINDER_DIFF_SOLR=1` in `tests/differential.rs`).
