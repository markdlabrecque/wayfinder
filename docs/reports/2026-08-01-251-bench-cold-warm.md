# #251 — report cold and warm p95 separately in `bench/run.sh`

Issue: [#251]. Branch: `251-bench-cold-warm`. Direct predecessor: #246
(`docs/reports/2026-08-01-246-p95-attribution.md`), whose suspect-1
correction is what raised this issue: `bench/run.sh` fired the same query 200
times, so Solr served 199 of them from its `queryResultCache` while
Wayfinder, which has no cache, executed all 200. The PRD's `<= baseline` p95
target was therefore being judged against a cached Solr and an uncached
Wayfinder. Nobody chose that; it fell out of the benchmark's shape. Ran
through the full test-writer / implementor / reviewer / reporter pipeline.

## What changed

- `bench/src/corpus.rs` gained `query_terms()` -> 48 terms; `gen_corpus`
  writes `terms.txt`.
- `bench/run.sh` gained, per engine: a warm-up pass (discarded), a Solr-only
  cache flush by core RELOAD, a cold pass over the 48 distinct terms, then
  today's unchanged warm pass with its existing memory sampling.
- Cache-counter assertions bracket both passes and abort the run if a pass
  did not have the cache behaviour it claims.
- `bench/query_result_cache_stat.py` (new): reads the metrics body on stdin,
  `<core> <stat>` as args, prints the counter. Extracted from a bash heredoc
  specifically so it could be tested.
- `bench/src/results.rs` / `render_report.rs`: the single p95 row became two,
  warm-cache and cold-cache, each stating its cache condition in the
  Measurement path cell.

## Commits (8, after rebase)

- `39daae1` test(bench): red tests for the cold/warm p95 split
- `78fad75` perf(bench): report cold and warm p95 separately
- `162fd50` test(bench): fix fake call-site ordering guards, pin
  queryResultCache metrics parse, catch zero-lookups cold pass
- `dc1a72a` fix(bench): read queryResultCache counters from admin/metrics,
  fail fast
- `eb95e87` docs(solr-ref): finding 119, admin endpoints need a
  response-writer param
- `da6c775` docs(bench): regenerate 2M results with the cold/warm p95 split
- `97e0503` docs(solr-ref): clarify finding 119, metrics nests where mbeans
  flattens
- `465c303` test(bench): pin assert_cache_pass_behavior call sites, anchor
  the Solr warm-up ordering guard, and pin the metrics writer param

## Results (2M docs, seed 42)

| p95 | Solr | Wayfinder |
|---|---|---|
| warm cache (facet+filter+highlight) | 9.79 ms | 9.73 ms |
| cold cache (distinct queries) | 18.90 ms | 15.20 ms |

Committed in `docs/benchmarks.md`. The cold row independently corroborates
the scratch experiment that motivated the issue (19.46 / 16.32 for the same
shape, from #246's attribution pass). **On uncached queries Wayfinder is
ahead.** The cold/warm gap is the `queryResultCache` specifically: the
`filterCache` was measured separately (in #246) at +0.50 ms mean and is
negligible.

## The interesting part: three real bugs the green suite did not catch

1. **The counter read never worked against a real Solr.** Round 1 shipped
   green with 948 tests passing; the first real 2M run aborted an hour in
   with `query_result_cache_stat: no queryResultCache hits counter in the
   mbeans response`. Two independent causes: the mbeans URL lacked a
   response-writer param, and the stats key was missing the `.searcher`
   scope. Round 1's mutation test had stubbed `curl`, so it validated the
   shape of the abort path while leaving the actual request unexercised.
2. **`wt=json` alone is not enough on Solr's admin endpoints.** Both
   `admin/mbeans` and `admin/metrics` return HTTP 200 with a
   *type-signature* body — unquoted keys, values literally `int` — that is
   not valid JSON and carries no statistics. Any recognised writer param
   (`indent=true`, `indent=false`, `json.nl=map`) fixes it; an unrecognised
   one does not. Captured as finding 119 in `docs/solr-ref-findings.md`.
   This corrected a diagnosis I gave the implementor myself: I stated
   `admin/metrics` returned plain JSON, and it did not.
3. **The source-order guards were vacuous.** `call_lines` matched the
   function name anywhere, including inside the function's own body, so the
   tests pinned the order of *definitions*, not call sites. Moving the cold
   pass to run before the RELOAD — exactly the regression the guard existed
   to prevent — passed; a no-op reword of an error string failed. Fixed by
   stripping function bodies before matching.

A green suite is evidence about the tests, not about the system. All three
of these were found by running the real thing or by mutating it, not by the
suite. The tests were written before the implementation (red-first) and
still missed all three, because they tested text rather than behaviour.

## Verification evidence

- Two mutations of the real `run.sh` against a real Solr 9 at 50k docs, both
  aborting correctly:
  - flush skipped -> `assert_cache_pass_behavior: cold pass took 48 cache
    hits over 48 lookups, expected 0 -- the searcher was not actually
    flushed, or the term list repeated a query, so these are not cold
    numbers`
  - empty terms file -> `assert_cache_pass_behavior: cold pass took 0
    queryResultCache lookups, so no query reached Solr's searcher -- these
    are not cold numbers, they are no numbers`
  These matter because a passing 2M run only exercises the success branch;
  it does not show the assertion aborts a bad run.
- Guard mutations (scratch copies, reverted): semantic regressions caught,
  no-op refactors pass, deleting both `assert_cache_pass_behavior` call
  sites now fails.
- Gates on the rebased state: root `cargo test` 948 passed / 0 failed;
  `bench/` 65 passed / 0 failed; `cargo fmt --check` and
  `cargo clippy --all-targets -- -D warnings` clean in both crates;
  `shellcheck bench/run.sh` clean.

## Review

Three rounds with an independent reviewer (Opus). Round 1 BOUNCED with four
must-fixes (the broken counter read; the vacuous guards; no test for the
parse; the read happening an hour into the run rather than at startup).
Round 2 APPROVED, holding one item for my decision, and added follow-ups.
Round 3's items were closed by the guard work in `465c303`. **This exceeded
the default two-round cap** — recorded here rather than left unstated.

## The item the reviewer held for my decision, and how it resolved

The reviewer observed Solr's index-size row dropping 599.3 -> 389.5 MB
between consecutive 2M runs against a byte-identical Wayfinder control, and
suspected the new core RELOAD. Investigated and **rejected**:

- A 200k probe measured `du` immediately before and immediately after the
  RELOAD: byte-identical (89896KB both). The RELOAD does not change index
  size.
- Across five 2M runs of unmodified code the row reads 583.2, 506.6, 498.1,
  599.3, 389.5 MB while Wayfinder is 330.8 MB every time. The row was
  already swinging ~20%.
- Mechanism: `du` targets `data/`, and `tlog` is 62% of it (56 MB of 90 MB
  at 200k). The row measures transaction-log retention and momentary merge
  state, not index size.

Conclusion: not a #251 regression, but the row is close to meaningless as
written. Recorded as a follow-up below.

## Follow-ups to record

1. **The PRD does not say which p95 the `<= baseline` target means** — the
   warm row (cached Solr vs uncached Wayfinder) or the cold row.
   `docs/benchmarks.md` deliberately declines to declare either met or
   missed. This is an open product decision reserved for the user; not
   resolved here, and no resolution is implied.
2. Solr's index-size row should measure `data/index` rather than `data/`, or
   state that it includes `tlog`. See the five-run spread above.
3. The rendered memory note says Wayfinder's RSS increase happened "during
   query load", but ~96 warm-up and cold-pass queries now precede that
   sampling window, so the sentence misattributes where the growth
   occurred. The memory rows themselves are unchanged and still comparable;
   the note's wording is not.
4. `wait_for_ping` cannot yet move inside `flush_solr_caches`: the
   body-stripping guard deletes the call when it moves, so
   `solr_flushes_caches_with_a_core_reload_and_pings_afterward_not_update_commit_true`
   fails. The test needs reworking first.
5. A `terms.txt` truncated to 1-2 terms would still pass the `lookups > 0`
   assertion and render a 2-sample p95 as the headline cold number.
   Closable by comparing `wc -l` of the cold latency file against
   `terms.txt`.
6. `query_result_cache_stat.py` exits via traceback rather than a clean
   message for three malformed-input cases (registry-as-list, non-numeric
   and null stat values). Loud and non-zero, so cosmetic.

## Gates (re-ran independently)

`cargo test`: 948 passed / 0 failed at the repo root; `bench/` 65 passed /
0 failed. `cargo fmt --check` clean. `cargo clippy --all-targets -- -D
warnings` clean in both crates. `shellcheck bench/run.sh` clean.
