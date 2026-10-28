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

- [x] Pick the source: `wikimedia/wikipedia` on Hugging Face, config `20231101.en`, revision
      `b04c8d1c`. First 4 parquet shards (~1.4 GB).
- [x] Download script: `bench/fetch-wiki.sh` fetches into `bench/data/` (gitignored), SHA-256 checked.
- [x] Converter: `bench/src/wiki.rs` reads parquet directly (no JSON copy on disk). Fields:
      `id`, `title`, `body`, plus derived `len` (int), `len_bucket` (string), `initial` (string).
      No date field: the dataset has none.
- [x] Shared schema for both engines: in `wf-bench.rs` (`WF_SCHEMA` and Solr `add-field`).
- [x] Size presets: `--docs 10000` for smoke runs, `--docs 500000` for full runs.

### Phase 2: Query suite (shared features only)

- [x] Query file `bench/queries.tsv`: name, params, comparison mode.
- [x] Pick about 10 to 15 shapes from the Solr-compatible surface both engines support:
  - [x] single term
  - [x] multi-term OR / AND
  - [x] phrase
  - [x] edismax with `qf` boosts
  - [x] filter query (`fq`)
  - [x] sort by field
  - [x] deep paging (`start` large)
  - [x] field facet
  - [x] range facet
  - [x] highlighting
  - [x] grouping
  - [x] stats
  - [x] `*:*` match-all with rows
- [ ] Query terms sampled from the corpus with a fixed seed. **Committed, not yet run end to end**
      (see "Handoff" below).

### Phase 3: Correctness gate

- [ ] `cargo test` must pass (frozen fixtures in `solr-ref/responses/`).
- [x] Per-query comparison against Solr's answer:
  - [x] `numFound` equal
  - [x] fixed sort: same ids in the same order
  - [x] relevance sort: top-10 overlap at or above a threshold (start at 0.8)
  - [x] facet buckets and counts equal
  - [x] stats values equal (float tolerance)
  - [x] highlight: a snippet exists for each doc that should have one
- [x] Allowed-differences file `bench/allowed-diffs.tsv`, each line pointing at a section of
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
      Code done (#449): `wf-bench` records `index_docs_per_s` and `index_bytes_per_s` for both
      engines. The live full-corpus run is deferred to #441, per the owner's decision.
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

- [x] Pin CPUs for each container, and pin the load generator to separate cores. Engines on
      `2-5`, `wf-bench` itself on `6-7` (`sched_setaffinity`).
- [ ] Fixed Solr config: stock `solr:9`, default caches on, fixed heap.
- [x] Record machine info and both versions in the JSON (`env`: kernel, CPU model, core split,
      Wayfinder git sha, Solr version, image digest, heap).
- [x] Refuse to run (or warn) when the machine is busy: 1-min load average over 2.0 exits 2;
      `--force` runs anyway and records the warning.

### Phase 7: The hill-climb loop

- [ ] Loop instructions (prompt or skill): read the one-line result, pick a target, change code,
      run the gates, keep or revert.
- [ ] Guardrails: never edit `solr-ref/responses/`, `bench/allowed-diffs.tsv`, query files,
      or scoring without a human.
- [ ] Each kept win is a commit with the score delta in the message.
- [ ] Log of tried ideas (kept and rejected) so later loops don't repeat them.
- [ ] Stop rule: stop after N failed attempts in a row, or when the score target is hit.

### Phase 8: Cleanup

- [x] Retire or fold in the old `bench/run.sh` and `bench/RESULTS.md` once the new driver
      covers their metrics. Removed (#459) with the pipeline only they used (`gen_corpus`,
      `render_report`, `corpus.rs`, `results.rs`, `query_result_cache_stat.py`). Every old
      metric has a `wf-bench` equivalent except image size, which `wf-bench` does not build;
      `bench/tests/dockerfile_build.rs` still checks it.
- [x] Update `bench/README.md`.

## Decisions

- **Machine:** the dedicated Linux desktop (16 cores, 60 GB). Hill-climb runs happen when it is quiet.
- **Wins:** the loop stacks wins as commits on one branch. One PR at the end.
- **Warm = varied queries** (option 1, 2026-10-28). Every timed request is a query the engine
  has not seen before, filled from pools of real corpus terms. This stops Solr's
  queryResultCache from turning the warm test into a cache lookup. The same URL repeated is
  kept as a separate `repeat_p50` metric that is reported, not scored.
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
bench/fetch-wiki.sh                         # once, ~1.4 GB
bench/target/release/wf-bench --docs 10000 --iters 200
```

Detail lands in `bench/out/<ts>.json` (gitignored).

## Known issues to fix in later phases

- Memory uses cgroup `memory.current`, which counts page cache (Solr idles at ~2.4 GB). Phase 4
  should split anon memory from file cache (`memory.stat`).
- Solr gets only 20 warm-up requests, so its JIT is not fully warm. Phase 4 needs a real
  warm-up (time- or count-based, per query).
- One query only, so the score is not meaningful until Phase 2 lands.

- **Count mismatches to investigate** (20k docs): `bool_or` wf=2988 solr=2987, `edismax`
  wf=2696 solr=2693. Likely analyzer differences (`text_en` stemming/tokenizing). Find the
  extra doc ids, then either fix Wayfinder or list them in `allowed-diffs.tsv` with the
  `docs/COMPATIBILITY.md` section that covers it.
## Open questions

- (none right now)

## First full-suite findings (20k docs, 2026-10-28)

`score 2.666 | p50 8.08 p99 6.41 cpu 6.74 mem 0.14 | correct 14/16 | FAIL`

Biggest gaps (Wayfinder / Solr, warm p50): `match_all` sorted by `id` 1972x (140 ms),
`sort_field` 598x (50 ms), `range_query` 95x, `deep_page` 78x, `group` 10x. These all sort or
scan by field values, so they share a likely root cause. Wayfinder wins every cold first query
and uses ~7x less memory.

## Handoff: varied warm queries (committed on `440-perf-bench`)

Committed and passing fmt/clippy/tests, but **not run end to end yet**:

- `bench/src/terms.rs` (new): scans the first 20k docs and builds two pools with a fixed seed:
  2,000 terms and 2,000 adjacent word pairs. Each has doc frequency between 3 and 20% of the
  sample, with stopwords removed. `Pools::fill(template, i)` fills the placeholders `{t}`,
  `{t2}`, `{bigram}`, `{start}`, `{lo}`, `{hi}`, always the same way for the same `i`.
- `bench/src/lib.rs`: adds `pub mod terms;`.
- `bench/queries.tsv`: now 15 templates using those placeholders. `term_common` and
  `term_rare` merged into `term`. `match_all` pages from a random `{start}`. `deep_page` is a
  term query at `start=1000`. `range_query` uses `len:[{lo} TO {hi}]`.
- `bench/src/bin/wf-bench.rs`:
  - `Query` holds raw `(key, template)` pairs, and `Query::url(base, pools, i)` fills and encodes.
  - Request `i` of shape `n` uses seed `n * 1_000_000 + i`.
  - Sequence per shape: request 0 is cold, requests 1..=300 are warm-up (fixed count, so both
    engines see the same sequence), then `--iters` timed requests, then 100 repeats of
    request 0 for `repeat_p50`.
  - Every timed answer is checked, not just one. A failing shape reports `k/iters wrong`
    plus the first failing URL so it can be replayed.
  - The last `cargo build --release --bin wf-bench` after these edits was clean.
- `bench/tests/terms.rs` (new): one test that filled queries are stable across runs, have no
  placeholders left, and actually vary. Passes (fixture uses 12 words so terms stay
  under the 20% doc-frequency cap).

Next steps:
1. `./target/release/wf-bench --docs 20000 --iters 200`. Check that correctness holds across
   the varied requests. Expect more mismatches than before, because many more queries are
   now compared.
2. Go on to the count mismatches under "Known issues" and Phase 4.

## Session log

- 2026-10-28: plan written. Issues #440 / #441 filed. Phase 0 done: `bench/src/bin/wf-bench.rs`
  runs both engines in Docker, one edismax query, answer check, one-line output. First run
  (2k docs, 100 iters): `score 0.029 ... correct 1/1 | PASS`.
- 2026-10-28: Phase 1 done. First real-data run (20k Wikipedia docs, 200 iters):
  `score 0.194 | p50 0.45 p99 0.18 cpu 0.12 mem 0.15 | correct 1/1 | PASS`.
- 2026-10-28: Phases 2 and 3 mostly done. 16-query suite, `src/check.rs` gate (tests in
  `tests/check.rs`, all 10 mutations caught), `allowed-diffs.tsv` (empty).
- 2026-10-28: Chose varied warm queries (option 1). Implementation started, handed off
  (see "Handoff" above). Committed and pushed.
