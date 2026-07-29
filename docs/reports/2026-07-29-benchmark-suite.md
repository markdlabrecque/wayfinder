# Report: benchmark suite comparing Wayfinder vs Solr 9

- Branch: `13-benchmark-suite`
- Issue: [#13](https://github.com/markdlabrecque/wayfinder/issues/13) — benchmark suite.
- PR: [#61](https://github.com/markdlabrecque/wayfinder/pull/61), merged.
- Pipeline: test-writer -> implementor -> reviewer (2 rounds: BOUNCE with 5 must-fix items, then
  APPROVED) -> reporter (this report).

## What was built

A standalone `bench/` crate (`bench/Cargo.toml`, deliberately **not** a workspace member) that
benchmarks Wayfinder against Solr 9 head-to-head:

1. **Deterministic corpus generator (`bench/src/corpus.rs`).** A splitmix64-style PRNG produces a
   reproducible document corpus of configurable size, so repeated runs are comparable and no
   external fixture data is needed.
2. **Latency measurement with p95 (`bench/src/results.rs`).** p95 computed as nearest-rank,
   1-indexed, over sampled query latencies.
3. **Orchestration script (`bench/run.sh`).** Drives corpus generation, index population against
   both Wayfinder and a real Solr 9 container, a query-load phase, and memory sampling via
   `docker stats`.
4. **Multi-stage Docker build.** musl build stage -> `scratch` runtime image for Wayfinder, kept
   minimal for a fair container-memory comparison against Solr's container.
5. **`docs/benchmarks.md`.** Output document with real measured numbers from an actual run
   (corpus sizes up to 2M docs), not placeholder or illustrative figures.

Round-1 commit: `eea4667` (`feat(bench): benchmark suite comparing Wayfinder vs Solr 9 (#13)`).
Round-2 fix commit: `3320daf` (`fix(bench): address round-2 review findings in issue #13
benchmark suite`). Both merged into `main` via PR #61.

## Test evidence

- `cargo test`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`: all green at
  merge (`bench/` is intentionally excluded from the workspace, so these gates cover the rest of
  the repo as usual; `bench/`'s own correctness was established by the reviewer's targeted
  re-implementation checks below, since the crate carries no automated test suite of its own).

## Review outcome

**Round 1: BOUNCE.** Independent review (Opus) found 5 must-fix items in `eea4667`:

1. **GiB unit-parsing bug, `bench/run.sh`.** Docker's `MemUsage` token has no space between the
   number and the unit (e.g. `1.2GiB`, not `1.2 GiB`). The script's parsing never matched that
   shape, so any container using more than 1 GiB had its memory reported roughly 1000x low.
2. **Query-load loop ignored HTTP status.** Failed requests (4xx/5xx) were counted as fast
   successes in the latency samples rather than as failures, silently making error responses look
   like good performance.
3. **Solr schema add-field POST response discarded unchecked.** A failed schema mutation could go
   unnoticed rather than aborting the run.
4. **Fabricated 2M-doc row in `docs/benchmarks.md`.** The 2M-doc row showed numbers copied from
   the 50k-doc measurement rather than a real 2M-doc measurement, i.e. presented as measured when
   it was not.
5. **Incomplete disclosure of measurement path.** The write-up did not clearly state that memory
   was measured via the Docker container (`docker stats`) rather than the native process, which
   matters for interpreting the numbers.

**Round 2: APPROVED.** Fix commit `3320daf` addressed all 5. The same reviewer verified each fix
directly rather than re-asserting the diff:

- Replayed real `docker stats` output shapes through the actual awk parsing logic to confirm the
  GiB case now parses correctly.
- Injected a simulated HTTP 404 into a backgrounded copy of the query-load loop to confirm
  `set -e` now propagates the failure instead of silently counting it as a success.
- Injected a simulated HTTP 400 into the schema-POST path to confirm the script now aborts.
- Regenerated `docs/benchmarks.md` from the round-1 scalar measurements and diffed it
  byte-identical against the committed file, confirming no numbers were hand-edited and the 2M
  row is now genuinely derived from measured data (not copied from the 50k row).
- Checked boundary values (1,999,999 / 2,000,000 / 2,000,001 docs) against the code path that
  decides "not measured" vs. "measured," confirming the cutoff behaves as intended at the edges.

All gates (fmt/clippy/test) were green. Verdict: **APPROVED**, merged as PR #61.

Per the pipeline's 2-round cap: round 2 exhausted the cap. Six further findings surfaced during
round 2 were **explicitly not bounced back for a third round** and are recorded below as
follow-ups rather than resolved in this PR. The reviewer's own note is that `bench/run.sh` has
zero automated test coverage — every finding above and below came from independently
re-implementing its logic in a scratch script and probing it, not from a test suite exercising the
script itself — and it could use a dedicated review/test-seam pass beyond what this pipeline gave
it.

## Follow-ups deferred (not fixed in this PR — filed as #62, #63)

Items 3-5 -> [#62](https://github.com/markdlabrecque/wayfinder/issues/62). Items 1, 2, 6 ->
[#63](https://github.com/markdlabrecque/wayfinder/issues/63).

1. **`bench/src/results.rs:91`** — the p95 output row hardcodes the label
   `"(facet+filter+highlight, 50k docs)"` regardless of the actual `corpus_size` used for the run,
   so a 2M-doc run's latencies would be mislabeled as 50k in the printed report.
2. **`bench/src/results.rs:79-84`** — for runs with a corpus smaller than 2M docs, the real,
   correctly-measured under-load memory figure is computed but discarded and never surfaced; it
   is currently only printed for 2M-doc runs.
3. **`bench/run.sh:176-182`** — unrecognized memory units silently fall back to MiB scaling
   instead of failing loudly. Concretely: `512B` is misparsed as 512 MiB (~1000x over), and
   `1.5TiB` is misparsed as 1.5 MiB (~1,000,000x under). No test currently exercises this path.
4. **`bench/run.sh:151-164`** — `curl -sSf` on the Solr schema add-field POST discards the
   response body on failure, so the existing `grep -q '"errors"'` check is dead code on the
   realistic rejection path: the operator sees only `curl: (22) ... error: 400`, not which field
   Solr actually rejected.
5. **`bench/run.sh:147-148`** — `COLD_START_PATH` timing begins after `docker run -d` returns,
   excluding container-create time from the measurement, which understates cold-start latency
   slightly.
6. **`docs/benchmarks.md:1`** — prose regressed from "50k-doc corpus" to "50000-doc corpus"
   (cosmetic; introduced by a `{corpus_size}` template substitution).

Reviewer's overall note, repeated here for emphasis: `bench/run.sh` has no automated test
coverage of its own. All 5 must-fix findings and all 6 follow-ups above were caught by
independently re-implementing and probing its logic during review, not by any test in the suite —
this is itself evidence the script needs a dedicated test/review pass, not just this one round.

## Additional item noted during implementation (not in scope for #13)

The implementor flagged, during #13 work, that axum's default 2MB request-body limit could be an
issue for larger bulk-index requests used by the benchmark corpus. This was explicitly out of
scope for #13 and was not investigated or fixed as part of this work. Filed as
[#64](https://github.com/markdlabrecque/wayfinder/issues/64).

## Pointers

- Production/tooling code: `bench/Cargo.toml`, `bench/src/corpus.rs`, `bench/src/results.rs`,
  `bench/src/lib.rs`, `bench/src/bin/`, `bench/run.sh`, `bench/README.md`, Docker build files for
  the musl->scratch image.
- Docs: `docs/benchmarks.md`.
- Commits: `eea4667` (round 1), `3320daf` (round 2 fixes).
- PR: [#61](https://github.com/markdlabrecque/wayfinder/pull/61) (merged).
- Issue: [#13](https://github.com/markdlabrecque/wayfinder/issues/13).
