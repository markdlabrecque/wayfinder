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
//! Round 2 (independent reviewer finding): the round-1 ordering guards
//! matched ANY source line containing a function's name, which resolves to
//! an error-message or comment line *inside that function's own body* just
//! as readily as to a real call site -- so they were checking the order of
//! function *definitions*, never of call sites. Proven by three mutations
//! against a scratch copy of run.sh:
//!   - moving the Solr cold-pass call to before the cache-flush call (the
//!     exact regression these guards exist to catch) still PASSED;
//!   - moving the `flush_solr_caches` definition above `warm_up_pass` (a
//!     pure no-op refactor) FAILED;
//!   - rewording `run_cold_query_pass`'s error string FAILED.
//!
//! Fixed by `support::strip_function_bodies`, which blanks every top-level
//! function's body (definition line through matching close-brace) before
//! any of the line-number scans below run, so a match can only ever be a
//! real call site or a genuine standalone occurrence -- never text quoted
//! inside a function's own definition. `call_lines` is applied to the
//! stripped source everywhere ordering matters; `extract_bash_function`
//! (which needs a function's real body) still reads the unstripped source.
//!
//! This also fixes a latent anchor bug: `flush_solr_caches`'s call to
//! `admin/cores?action=RELOAD` is text *inside that function's own body*,
//! so anchoring order checks on "the line containing `action=RELOAD`"
//! anchors on the function's *definition* (near the top of the file, before
//! any top-level flow), which is vacuously before everything regardless of
//! how the top-level calls get reordered. The round-2 tests anchor on the
//! call site of `flush_solr_caches` itself instead.
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
//!     `CACHE.searcher.queryResultCache.{hits,lookups}` counter.
//!   - `assert_cache_pass_behavior kind hits lookups n_queries` -- `kind` is
//!     `cold` or `warm`; fails loudly (non-zero exit, message on stderr)
//!     when the pass didn't have the cache behavior it claims: cold pass
//!     hits must be exactly 0 AND lookups must be nonzero (round 2, item C
//!     -- a pass where no query reached Solr at all must not be accepted as
//!     a clean cold measurement just because it also reported 0 hits); warm
//!     pass hits must be `>= n_queries - 2`.
//!
//! None of these exist in `run.sh` today, so every test below is red for a
//! clear "missing behavior" reason (an `Option::expect` panic naming what's
//! missing), matching `run_sh_schema_check.rs`'s established pattern for a
//! not-yet-added seam function.

mod support;

use support::{
    extract_bash_function, fresh_scratch_dir, run_bash, run_sh_source, strip_function_bodies,
};

/// Lines in `source` that mention `function` as other than its own
/// definition line. Callers pass a body-stripped source (see
/// `strip_function_bodies`) so a match can only be a real call site or a
/// standalone occurrence, never text quoted inside `function`'s own body.
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
    let stripped = strip_function_bodies(&source);
    let warm_ups = call_lines(&stripped, "warm_up_pass");
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
    let stripped = strip_function_bodies(&source);

    // Existence only (not order-dependent): Solr's caches must be flushed
    // with a core RELOAD somewhere in run.sh.
    assert!(
        source.contains("action=RELOAD"),
        "expected run.sh to flush Solr's caches with a core RELOAD \
         (admin/cores?action=RELOAD&core=$SOLR_CORE), issue #251; \
         `update?commit=true` does not work here (Solr skips the commit when nothing \
         changed, so no new searcher opens and the caches survive) -- found no RELOAD call \
         in run.sh"
    );

    // Order-dependent part anchors on the *call site* of flush_solr_caches,
    // not on the line containing `action=RELOAD` text -- that text lives
    // inside flush_solr_caches's own body, which (being a function
    // definition) sits near the top of the file regardless of where the
    // function gets *called*, and so is a vacuous anchor for ordering.
    let flush_calls = call_lines(&stripped, "flush_solr_caches");
    assert!(
        !flush_calls.is_empty(),
        "expected a call to a `flush_solr_caches` function in run.sh's top-level flow \
         (issue #251's Solr cache-flush step); none found"
    );

    // The ping belongs inside the flush helper, immediately after its RELOAD:
    // otherwise a caller can use `flush_solr_caches` and begin a cold pass
    // before the new searcher serves requests. Keeping the readiness wait with
    // the state transition also means the top-level flow need not retain a
    // duplicate, separately ordered ping.
    let flush_function = extract_bash_function(&source, "flush_solr_caches").expect(
        "expected run.sh to define flush_solr_caches; its RELOAD and readiness wait must be \n         one atomic cache-flush operation",
    );
    let reload_line = flush_function
        .lines()
        .position(|line| !line.trim_start().starts_with('#') && line.contains("action=RELOAD"))
        .expect("flush_solr_caches must issue admin/cores?action=RELOAD");
    let ping_line = flush_function
        .lines()
        .position(|line| {
            !line.trim_start().starts_with('#')
                && line.contains("wait_for_ping")
                && !line.trim_start().starts_with("wait_for_ping()")
        })
        .expect(
            "flush_solr_caches must wait_for_ping after RELOAD before returning; a separate \n             top-level ping does not make the flush helper safe for every caller"
        );
    assert!(
        reload_line < ping_line,
        "flush_solr_caches must call wait_for_ping only after its RELOAD, got function body:\n\
         {flush_function}"
    );
}

#[test]
fn commit_true_is_not_used_as_the_solr_cache_flush() {
    let source = run_sh_source();
    let stripped = strip_function_bodies(&source);

    // The existing `index_corpus` helper already legitimately uses
    // `update?commit=true` to finalize indexing -- that's not the cache
    // flush and must stay untouched. The guard is scoped to the flush
    // step: in the top-level window from the `flush_solr_caches` call site
    // to the next `wait_for_ping` call site, no sibling
    // `update?commit=true` call should also appear pretending to be the
    // flush mechanism.
    let flush_calls = call_lines(&stripped, "flush_solr_caches");
    let flush_line = flush_calls
        .first()
        .unwrap_or_else(|| {
            panic!(
                "expected a call to a `flush_solr_caches` function in run.sh's top-level flow \
                 (issue #251's cache flush); none found yet -- this guard becomes meaningful \
                 once the call exists"
            )
        })
        .0;

    let pings = call_lines(&stripped, "wait_for_ping");
    let lines: Vec<&str> = source.lines().collect();
    let next_ping_line = pings
        .iter()
        .find(|(line_no, _)| *line_no > flush_line)
        .map(|(line_no, _)| *line_no)
        .unwrap_or(lines.len().saturating_sub(1));

    let flush_window =
        lines[flush_line..=next_ping_line.min(lines.len().saturating_sub(1))].join("\n");

    assert!(
        !flush_window.contains("update?commit=true"),
        "the cache-flush step (the top-level window from the flush_solr_caches call to the \
         next wait_for_ping call) must not also call `update?commit=true` -- that call does \
         not flush Solr's caches (observed: Solr skips the commit when nothing changed, so no \
         new searcher opens and the caches survive), got flush window:\n{flush_window}"
    );
}

#[test]
fn cold_pass_reads_terms_txt_and_runs_after_the_solr_warm_up_and_cache_flush() {
    let source = run_sh_source();
    let stripped = strip_function_bodies(&source);
    let cold_calls = call_lines(&stripped, "run_cold_query_pass");
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

    let warm_ups = call_lines(&stripped, "warm_up_pass");
    let flush_calls = call_lines(&stripped, "flush_solr_caches");
    let flush = flush_calls.first().unwrap_or_else(|| {
        panic!(
            "expected a call to a `flush_solr_caches` function in run.sh's top-level flow \
             (issue #251's Solr cache-flush step); none found"
        )
    });

    // Only Solr flushes (Wayfinder has no query result cache and runs its
    // cold pass with nothing to flush), so these are existence checks
    // relative to the flush's position, not checks on `.first()`/`.last()`
    // of either list -- the earliest cold-pass call is Wayfinder's, which
    // legitimately runs before Solr's flush ever happens.
    //
    // Round 3 (reviewer finding, item 2): `warm_ups.iter().any(|n| n <
    // flush.0)` is satisfied by *Wayfinder's* warm-up call, which is nowhere
    // near the Solr flush -- so the previous assertion didn't check what its
    // own message claimed ("the Solr warm-up pass runs before the Solr cache
    // flush"). Anchor on the `# --- Solr ---` section marker so the warm-up
    // call being checked is provably the Solr one, not Wayfinder's earlier
    // call that happens to also be numerically before `flush.0`.
    let solr_section_line = stripped
        .lines()
        .position(|l| l.contains("--- Solr ---"))
        .unwrap_or_else(|| {
            panic!(
                "expected a `# --- Solr ---` section marker in run.sh separating the Wayfinder \
                 and Solr phases; found none -- this guard needs it to tell the Solr warm-up \
                 call apart from Wayfinder's earlier one"
            )
        });
    assert!(
        warm_ups
            .iter()
            .any(|(line_no, _)| *line_no > solr_section_line && *line_no < flush.0),
        "expected the Solr warm-up pass (a warm_up_pass call between the `# --- Solr ---` \
         section marker at line {solr_section_line} and the flush_solr_caches call at line {}) \
         to run before the Solr cache flush; got warm_up_pass calls at {warm_ups:?}",
        flush.0
    );
    assert!(
        cold_calls.iter().any(|(line_no, _)| *line_no > flush.0),
        "expected the Solr cold pass (run_cold_query_pass) to run after the Solr cache flush \
         at line {}, got cold pass calls at {cold_calls:?}",
        flush.0
    );
}

#[test]
fn warm_pass_runs_after_the_cold_pass_and_keeps_the_existing_run_query_load_call() {
    let source = run_sh_source();
    let stripped = strip_function_bodies(&source);
    let cold_calls = call_lines(&stripped, "run_cold_query_pass");
    let warm_calls = call_lines(&stripped, "run_query_load");

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
    let stripped = strip_function_bodies(&source);
    let cache_reads: Vec<(usize, &str)> = stripped
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

    // Anchored on the Solr-specific cold/warm calls (the ones after the
    // cache flush), not `.first()` of either list -- Wayfinder's cold pass
    // legitimately has no cache reads around it (it has no query result
    // cache to flush or read), so anchoring on whichever cold/warm call
    // comes first in the file picks up Wayfinder's and wrongly demands
    // cache reads that were never going to exist there.
    let flush_calls = call_lines(&stripped, "flush_solr_caches");
    let cold_calls = call_lines(&stripped, "run_cold_query_pass");
    let warm_calls = call_lines(&stripped, "run_query_load");
    if let Some(flush) = flush_calls.first() {
        let cold = cold_calls
            .iter()
            .find(|(n, _)| *n > flush.0)
            .unwrap_or_else(|| {
                panic!(
                    "expected a run_cold_query_pass call after the flush_solr_caches call at \
                     line {}, got cold pass calls at {cold_calls:?}",
                    flush.0
                )
            });
        let warm = warm_calls
            .iter()
            .find(|(n, _)| *n > cold.0)
            .unwrap_or_else(|| {
                panic!(
                    "expected a run_query_load call after the Solr cold pass at line {}, got \
                     warm pass calls at {warm_calls:?}",
                    cold.0
                )
            });
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
fn cold_pass_assertion_succeeds_when_hits_are_zero_and_lookups_are_nonzero() {
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
        "a cold pass with 0 hits and nonzero lookups must pass; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn cold_pass_assertion_fails_when_lookups_are_zero_even_though_hits_are_also_zero() {
    // Round 2, item C: the cold branch takes `lookups` and never read it,
    // so a pass in which NO query reached Solr at all (every request
    // errored out or was skipped before ever hitting the searcher) reports
    // hits=0, lookups=0, and was accepted as a clean cold measurement.
    // Zero lookups is not evidence of a cold cache -- it's evidence nothing
    // was measured. Choosing `lookups == 0` (rather than "below the term
    // count") as the assertion here: the value `assert_cache_pass_behavior`
    // actually receives for its 4th argument on the cold call in run.sh is
    // `N_QUERIES` (the *warm*-pass count, 200), not the term count (48/49)
    // -- see run.sh's `assert_cache_pass_behavior cold ... "$N_QUERIES"`
    // call -- so a "lookups >= term count" check has no term count
    // available to check against without a signature change this issue
    // doesn't ask for. `lookups == 0` needs no such extra information and
    // still closes the exact hole the spec names: "a pass in which NO
    // queries reached Solr reports 0 hits and is accepted as clean."
    let source = run_sh_source();
    let func = extract_bash_function(&source, "assert_cache_pass_behavior").expect(
        "run.sh should define an `assert_cache_pass_behavior(kind, hits, lookups, n_queries)` \
         function; it does not exist in run.sh yet",
    );

    let dir = fresh_scratch_dir("cache-assert-cold-no-lookups");
    let script = format!("{func}\nassert_cache_pass_behavior cold 0 0 200\n");
    let out = run_bash(&script, &dir, &[]);

    assert!(
        !out.status.success(),
        "a cold pass reporting 0 lookups must fail loudly, not pass silently just because hits \
         are also 0 -- 0 lookups means no query reached Solr at all, not that the cache was \
         cleanly flushed; stdout: {:?} stderr: {:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
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

// --- round 3 (independent reviewer findings) -----------------------------

#[test]
fn assert_cache_pass_behavior_is_actually_called_after_both_the_solr_cold_and_warm_passes() {
    // Round 3, item 1 (the reviewer called this the highest-value finding):
    // every test above pins `assert_cache_pass_behavior`'s *behaviour* and
    // its *definition*, but nothing pins that run.sh actually *calls* it.
    // Deleting both call sites from run.sh (while leaving the function
    // defined) left all other tests in this file green -- the self-checking
    // property that is the entire point of issue #251 could be silently
    // removed. This guard pins the call sites themselves, anchored on the
    // Solr cold/warm passes so it can't be satisfied by, say, a call left
    // over in a comment or a stray call somewhere unrelated.
    let source = run_sh_source();
    let stripped = strip_function_bodies(&source);

    let asserts = call_lines(&stripped, "assert_cache_pass_behavior");
    assert!(
        asserts.len() >= 2,
        "expected at least 2 calls to assert_cache_pass_behavior in run.sh's top-level flow \
         (one after the Solr cold pass, one after the Solr warm pass) -- found {} at {asserts:?}; \
         without these calls the cold/warm split's self-check (issue #251's whole point) can be \
         deleted without any test noticing",
        asserts.len()
    );

    let flush_calls = call_lines(&stripped, "flush_solr_caches");
    let flush = flush_calls.first().unwrap_or_else(|| {
        panic!(
            "expected a call to a `flush_solr_caches` function in run.sh's top-level flow \
             (issue #251's Solr cache-flush step); none found"
        )
    });
    let cold_calls = call_lines(&stripped, "run_cold_query_pass");
    let cold = cold_calls
        .iter()
        .find(|(n, _)| *n > flush.0)
        .unwrap_or_else(|| {
            panic!(
                "expected a run_cold_query_pass call after the flush_solr_caches call at line \
                 {}, got cold pass calls at {cold_calls:?}",
                flush.0
            )
        });
    let warm_calls = call_lines(&stripped, "run_query_load");
    let warm = warm_calls
        .iter()
        .find(|(n, _)| *n > cold.0)
        .unwrap_or_else(|| {
            panic!(
                "expected a run_query_load call after the Solr cold pass at line {}, got warm \
                 pass calls at {warm_calls:?}",
                cold.0
            )
        });

    let assert_after_cold = asserts.iter().any(|(n, _)| *n > cold.0 && *n < warm.0);
    assert!(
        assert_after_cold,
        "expected an assert_cache_pass_behavior call between the Solr cold pass (line {}) and \
         the Solr warm pass (line {}) checking the cold pass's cache counters; got calls at \
         {asserts:?}",
        cold.0, warm.0
    );

    let assert_after_warm = asserts.iter().any(|(n, _)| *n > warm.0);
    assert!(
        assert_after_warm,
        "expected an assert_cache_pass_behavior call after the Solr warm pass (line {}) \
         checking the warm pass's cache counters; got calls at {asserts:?}",
        warm.0
    );
}

#[test]
fn metrics_url_carries_a_recognized_response_writer_param() {
    // Round 3, item 3: `wt=json` alone on `admin/metrics` is documented
    // (solr-ref/FINDINGS.md, finding 119, live-verified against a real
    // solr:9 container) to return HTTP 200 with a type-signature body --
    // unquoted keys, values literally `int` -- which is not valid JSON and
    // carries no statistics. Any recognised response-writer param fixes it
    // (`indent=true`, `indent=false`, `json.nl=map`); an unrecognised one
    // (or none) does not. This does not pin the URL as a literal string --
    // that would only freeze whichever URL happens to be typed today, which
    // is the standing objection to the round-1 guards -- it pins that the
    // URL query_result_cache_stat builds carries *one of* the recognised
    // params.
    let source = run_sh_source();
    let func = extract_bash_function(&source, "query_result_cache_stat").expect(
        "run.sh should define a `query_result_cache_stat` function that GETs admin/metrics; \
         none found in run.sh",
    );
    let metrics_line = func
        .lines()
        .find(|l| l.contains("admin/metrics") && !l.trim_start().starts_with('#'))
        .unwrap_or_else(|| {
            panic!(
                "expected query_result_cache_stat to GET .../admin/metrics; found no such \
                 (non-comment) line in its body:\n{func}"
            )
        });

    let recognized = ["indent=true", "indent=false", "json.nl=map"];
    assert!(
        recognized.iter().any(|p| metrics_line.contains(p)),
        "solr-ref/FINDINGS.md finding 119: Solr's admin/metrics endpoint with `wt=json` \
         alone returns HTTP 200 but a type-signature body (unquoted keys, values literally \
         `int`/`float`) -- not valid JSON, no statistics -- so a recognised response-writer \
         param (`indent=true`, `indent=false`, or `json.nl=map`) must be present on the URL; \
         got metrics URL line: {metrics_line}"
    );
}

#[test]
fn strip_function_bodies_blanks_only_the_named_functions_body_preserving_line_count() {
    // Direct unit coverage of the helper the ordering guards above all lean
    // on: it must blank a top-level function's body (and only that body),
    // and it must not change the line count (every ordering guard above
    // uses `.enumerate()` line numbers against the stripped source and
    // expects them to still line up with the original file).
    let source = "before\nfoo() {\n  echo foo\n  bar\n}\nafter\n";
    let stripped = strip_function_bodies(source);

    assert_eq!(
        stripped.lines().count(),
        source.lines().count(),
        "stripping a function's body must preserve line count so line numbers still match the \
         original source; got:\n{stripped}"
    );
    assert!(
        stripped.contains("before"),
        "text before the function must survive stripping"
    );
    assert!(
        stripped.contains("after"),
        "text after the function must survive stripping"
    );
    assert!(
        !stripped.contains("echo foo") && !stripped.contains("bar"),
        "the function's own body must be blanked, not just its definition line; got:\n{stripped}"
    );
}
