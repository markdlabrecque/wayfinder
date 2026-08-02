//! Issue #251: `bench/run.sh` gains a cold pass (one query per distinct,
//! provably-in-corpus term, against a cache freshly flushed by a Solr core
//! RELOAD) alongside the existing warm pass (the same query repeated
//! `N_QUERIES` times), so the p95 row can report each separately instead of
//! judging Wayfinder's uncached numbers against Solr's `queryResultCache`
//! hits.
//!
//! `run.sh` has Docker/HTTP side effects and is not run in CI, so -- per
//! `bench/tests/run_sh_memory_phases.rs`'s established pattern -- these are
//! source-order guards over `run.sh`'s text, not a live run.
//!
//! Interface assumed (ambiguity flagged; the spec names *what* must happen,
//! not function names -- if this contract doesn't fit, escalate rather than
//! editing this file):
//!   - `warm_up_pass base_url core terms_file` -- GETs every term in
//!     `terms_file`, discards results. Called once per engine.
//!   - `flush_solr_caches` -- POSTs
//!     `admin/cores?action=RELOAD&core=$SOLR_CORE`, then calls
//!     `wait_for_ping` again (the core is briefly unavailable). Solr only.
//!   - `run_cold_query_pass base_url core terms_file out_latency_file` --
//!     one query per term in `terms_file`, latencies to `out_latency_file`.
//!     Same query shape as `run_query_load`, `q=<term>` varying.
//!   - `query_result_cache_hits base_url core` / `query_result_cache_lookups
//!     base_url core` -- each echoes the current
//!     `CACHE.searcher.queryResultCache.{hits,lookups}` counter, parsed from
//!     `admin/mbeans?cat=CACHE&stats=true&wt=json`.
//!   - `assert_cache_pass_behavior kind hits lookups n_queries` -- `kind` is
//!     `cold` or `warm`; fails loudly (non-zero exit, message on stderr)
//!     when the pass didn't have the cache behavior it claims: cold pass
//!     hits must be exactly 0; warm pass hits must be `>= n_queries - 2`.
//!
//! None of these exist in `run.sh` today, so every test below is red for a
//! clear "missing behavior" reason (an `Option::expect` panic naming what's
//! missing), matching `run_sh_schema_check.rs`'s established pattern for a
//! not-yet-added seam function.

mod support;

use support::{extract_bash_function, fresh_scratch_dir, run_bash, run_sh_source};

fn call_lines<'a>(source: &'a str, function: &str) -> Vec<(usize, &'a str)> {
    source
        .lines()
        .enumerate()
        .filter_map(|(line_no, line)| {
            let trimmed = line.trim_start();
            (trimmed.contains(function)
                && !trimmed.starts_with('#')
                && !trimmed.starts_with(&format!("{function}()"))
                && !trimmed.starts_with(&format!("function {function}")))
            .then_some((line_no, line))
        })
        .collect()
}

// --- source-order: warm-up, RELOAD, ping, cold pass, warm pass ---------

#[test]
fn per_engine_runs_a_discarded_warm_up_pass_over_every_term() {
    let source = run_sh_source();
    let warm_ups = call_lines(&source, "warm_up_pass");
    assert!(
        warm_ups.len() >= 2,
        "expected a warm_up_pass call for each engine (Wayfinder and Solr), issue #251; found \
         {warm_ups:?} in run.sh"
    );
    for (_, line) in &warm_ups {
        assert!(
            line.contains("terms.txt") || line.contains("TERMS"),
            "warm_up_pass must be called with the terms file gen_corpus wrote, not a second \
             hardcoded word list; got call: {line}"
        );
    }
}

#[test]
fn solr_flushes_caches_with_a_core_reload_and_pings_afterward_not_update_commit_true() {
    let source = run_sh_source();

    let reloads: Vec<(usize, &str)> = source
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains("action=RELOAD"))
        .collect();
    assert!(
        !reloads.is_empty(),
        "expected run.sh to flush Solr's caches with a core RELOAD \
         (admin/cores?action=RELOAD&core=$SOLR_CORE), issue #251; \
         `update?commit=true` does not work here (Solr skips the commit when nothing \
         changed, so no new searcher opens and the caches survive) -- found no RELOAD call \
         in run.sh"
    );

    let pings = call_lines(&source, "wait_for_ping");
    let reload_line = reloads[0].0;
    let ping_after_reload = pings.iter().find(|(line_no, _)| *line_no > reload_line);
    assert!(
        ping_after_reload.is_some(),
        "expected a wait_for_ping call after the core RELOAD -- the core is briefly \
         unavailable during a reload -- found RELOAD at line {reload_line} with no \
         subsequent wait_for_ping call; pings at {pings:?}"
    );
}

#[test]
fn commit_true_is_not_used_as_the_solr_cache_flush() {
    let source = run_sh_source();

    // The existing `index_corpus` helper already legitimately uses
    // `update?commit=true` to finalize indexing -- that's not the cache
    // flush and must stay untouched. The guard is scoped to the flush
    // step: within a window around any `action=RELOAD` call, no sibling
    // `update?commit=true` call should also appear pretending to be the
    // flush mechanism.
    let lines: Vec<&str> = source.lines().collect();
    let reload_idx = lines
        .iter()
        .position(|l| l.contains("action=RELOAD"))
        .expect(
            "expected a `action=RELOAD` call in run.sh (issue #251's cache flush); none found \
             yet -- this guard becomes meaningful once the RELOAD call exists",
        );

    // A generous window: from the RELOAD call to the next wait_for_ping
    // call (the flush-then-ping step described in the spec).
    let next_ping_idx = lines
        .iter()
        .enumerate()
        .find(|(i, l)| *i > reload_idx && l.contains("wait_for_ping"))
        .map(|(i, _)| i)
        .unwrap_or(lines.len());
    let flush_window = lines[reload_idx..next_ping_idx].join("\n");

    assert!(
        !flush_window.contains("update?commit=true"),
        "the cache-flush step (around the RELOAD call) must not also call \
         `update?commit=true` -- that call does not flush Solr's caches (observed: Solr \
         skips the commit when nothing changed, so no new searcher opens and the caches \
         survive), got flush window:\n{flush_window}"
    );
}

#[test]
fn cold_pass_reads_terms_txt_and_runs_after_the_warm_up_and_reload_ping_sequence() {
    let source = run_sh_source();
    let cold_calls = call_lines(&source, "run_cold_query_pass");
    assert!(
        !cold_calls.is_empty(),
        "expected a `run_cold_query_pass`-style call in run.sh reading terms.txt exactly once \
         per term (issue #251); found none"
    );
    for (_, line) in &cold_calls {
        assert!(
            line.contains("terms.txt") || line.contains("TERMS"),
            "the cold pass must read the terms file gen_corpus wrote, not a second hardcoded \
             word list; got call: {line}"
        );
    }

    let warm_ups = call_lines(&source, "warm_up_pass");
    let reloads: Vec<(usize, &str)> = source
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains("action=RELOAD"))
        .collect();
    if let (Some(warm_up), Some(reload), Some(cold)) =
        (warm_ups.first(), reloads.first(), cold_calls.first())
    {
        assert!(
            warm_up.0 < reload.0,
            "the warm-up pass must run before the cache flush, got warm_up at {}, RELOAD at \
             {}",
            warm_up.0,
            reload.0
        );
        assert!(
            reload.0 < cold.0,
            "the cold pass must run after the cache flush, got RELOAD at {}, cold pass at {}",
            reload.0,
            cold.0
        );
    }
}

#[test]
fn warm_pass_runs_after_the_cold_pass_and_keeps_the_existing_run_query_load_call() {
    let source = run_sh_source();
    let cold_calls = call_lines(&source, "run_cold_query_pass");
    let warm_calls = call_lines(&source, "run_query_load");

    assert!(
        warm_calls.len() >= 2,
        "the existing run_query_load warm pass must remain, unchanged, once per engine \
         (hard constraint: memory sampling stays attached to it exactly as today); found \
         {warm_calls:?}"
    );

    if let (Some(cold), Some(warm)) = (cold_calls.first(), warm_calls.first()) {
        assert!(
            cold.0 < warm.0,
            "the cold pass must run before the (existing) warm pass within an engine's \
             sequence, got cold pass at {}, warm pass at {}",
            cold.0,
            warm.0
        );
    }
}

// --- cache-counter assertions bracket both passes ------------------------

#[test]
fn both_passes_are_bracketed_by_query_result_cache_counter_reads() {
    let source = run_sh_source();
    let cache_reads: Vec<(usize, &str)> = source
        .lines()
        .enumerate()
        .filter(|(_, l)| {
            l.contains("query_result_cache_hits") || l.contains("query_result_cache_lookups")
        })
        .collect();

    assert!(
        cache_reads.len() >= 4,
        "expected at least 4 queryResultCache counter reads in run.sh -- around the cold \
         pass and around the warm pass -- so the cold/warm split is self-checking rather \
         than trusted blindly (issue #251); found {cache_reads:?}"
    );

    let cold_calls = call_lines(&source, "run_cold_query_pass");
    let warm_calls = call_lines(&source, "run_query_load");
    if let (Some(cold), Some(warm)) = (cold_calls.first(), warm_calls.first()) {
        let reads_before_cold = cache_reads.iter().filter(|(n, _)| *n < cold.0).count();
        let reads_after_cold = cache_reads
            .iter()
            .filter(|(n, _)| *n > cold.0 && *n < warm.0)
            .count();
        let reads_after_warm = cache_reads.iter().filter(|(n, _)| *n > warm.0).count();
        assert!(
            reads_before_cold >= 1,
            "expected at least one cache-counter read before the cold pass to compute the \
             cold-pass hits delta, got reads at {cache_reads:?}, cold pass at {}",
            cold.0
        );
        assert!(
            reads_after_cold >= 1,
            "expected at least one cache-counter read after the cold pass to compute the \
             cold-pass hits delta, got reads at {cache_reads:?}, cold pass at {}, warm pass \
             at {}",
            cold.0,
            warm.0
        );
        assert!(
            reads_after_warm >= 1,
            "expected at least one cache-counter read after the warm pass to compute the \
             warm-pass hits delta, got reads at {cache_reads:?}, warm pass at {}",
            warm.0
        );
    }
}

// --- the assertion fails loudly when the claimed cache behavior is wrong -

#[test]
fn cold_pass_assertion_fails_when_hits_are_not_zero() {
    let source = run_sh_source();
    let func = extract_bash_function(&source, "assert_cache_pass_behavior").expect(
        "run.sh should define an `assert_cache_pass_behavior(kind, hits, lookups, n_queries)` \
         function (issue #251's self-checking guard, the whole point of the issue); it does \
         not exist in run.sh yet",
    );

    let dir = fresh_scratch_dir("cache-assert-cold-nonzero");
    // A cold pass that reports 3 hits out of 49 lookups (48 distinct terms
    // -> 49 lookups, live-verified against Solr 9) is exactly the
    // degenerate case the issue warns about: the RELOAD didn't actually
    // flush the searcher (or a repeated term slipped into terms.txt), and
    // the "cold" pass silently measured some cache hits anyway.
    let script = format!("{func}\nassert_cache_pass_behavior cold 3 49 200\n");
    let out = run_bash(&script, &dir, &[]);

    assert!(
        !out.status.success(),
        "a cold pass reporting nonzero queryResultCache hits must fail loudly, not pass \
         silently -- a cold/warm split that degenerates into measuring the same thing twice \
         is worse than no split; stdout: {:?} stderr: {:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn cold_pass_assertion_succeeds_when_hits_are_zero() {
    let source = run_sh_source();
    let func = extract_bash_function(&source, "assert_cache_pass_behavior").expect(
        "run.sh should define an `assert_cache_pass_behavior(kind, hits, lookups, n_queries)` \
         function; it does not exist in run.sh yet",
    );

    let dir = fresh_scratch_dir("cache-assert-cold-zero");
    // Live-verified end state (issue #251 corrected spec): a cold pass over
    // the real 48-term `query_terms()` reports lookups=49, hits=0.
    let script = format!("{func}\nassert_cache_pass_behavior cold 0 49 200\n");
    let out = run_bash(&script, &dir, &[]);

    assert!(
        out.status.success(),
        "a cold pass with 0 hits must pass; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn warm_pass_assertion_fails_when_hits_are_below_n_queries_minus_two() {
    let source = run_sh_source();
    let func = extract_bash_function(&source, "assert_cache_pass_behavior").expect(
        "run.sh should define an `assert_cache_pass_behavior(kind, hits, lookups, n_queries)` \
         function; it does not exist in run.sh yet",
    );

    let dir = fresh_scratch_dir("cache-assert-warm-low");
    // 190 hits out of 200 queries is well below N_QUERIES - 2 = 198 --
    // the warm pass didn't get the cache hits it claims (e.g. a stray
    // RELOAD or restart in between).
    let script = format!("{func}\nassert_cache_pass_behavior warm 190 200 200\n");
    let out = run_bash(&script, &dir, &[]);

    assert!(
        !out.status.success(),
        "a warm pass reporting fewer than N_QUERIES - 2 hits must fail loudly; stdout: {:?} \
         stderr: {:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn warm_pass_assertion_succeeds_at_the_n_queries_minus_two_boundary() {
    let source = run_sh_source();
    let func = extract_bash_function(&source, "assert_cache_pass_behavior").expect(
        "run.sh should define an `assert_cache_pass_behavior(kind, hits, lookups, n_queries)` \
         function; it does not exist in run.sh yet",
    );

    let dir = fresh_scratch_dir("cache-assert-warm-boundary");
    // First request is always a miss; allow one for slop, per the spec:
    // hits >= N_QUERIES - 2 must pass at exactly that boundary.
    let script = format!("{func}\nassert_cache_pass_behavior warm 198 200 200\n");
    let out = run_bash(&script, &dir, &[]);

    assert!(
        out.status.success(),
        "a warm pass with hits == N_QUERIES - 2 (the documented slop boundary) must pass; \
         stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn warm_pass_assertion_accepts_the_live_verified_end_state_of_200_hits_not_exactly_199() {
    // Live-verified against Solr 9 (issue #251 corrected spec): a warm pass
    // of 200 requests reports hits=200, not 199 -- "rocket" (one of the
    // repeated warm-pass terms) is also one of the cold-pass's 48 terms, so
    // its cache entry already exists by the time the warm pass starts.
    // The spec's `>= N_QUERIES - 2` check must accept this, not assume an
    // exact 199.
    let source = run_sh_source();
    let func = extract_bash_function(&source, "assert_cache_pass_behavior").expect(
        "run.sh should define an `assert_cache_pass_behavior(kind, hits, lookups, n_queries)` \
         function; it does not exist in run.sh yet",
    );

    let dir = fresh_scratch_dir("cache-assert-warm-live-verified");
    let script = format!("{func}\nassert_cache_pass_behavior warm 200 200 200\n");
    let out = run_bash(&script, &dir, &[]);

    assert!(
        out.status.success(),
        "hits=200 (not 199) for a 200-request warm pass must pass, not be rejected for not \
         being exactly N_QUERIES - 1; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
