//! Issue #442: once the varied-query benchmark has been run end to end at
//! 20,000 docs and its mismatches triaged, BENCHMARK_PLAN.md records it: the
//! Phase 2 "terms sampled" box and the Phase 4 warm latency and CPU boxes are
//! ticked, and the "not yet run" caveat is gone.

use std::fs;
use std::path::Path;

fn plan_line(needle: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("BENCHMARK_PLAN.md");
    let text = fs::read_to_string(&path).expect("BENCHMARK_PLAN.md readable");
    text.lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no plan line containing {needle:?}"))
        .to_string()
}

fn assert_ticked(needle: &str) {
    let line = plan_line(needle);
    assert!(
        line.trim_start().starts_with("- [x]"),
        "plan box must be ticked after the 20,000-doc varied run: {line}"
    );
}

#[test]
fn terms_sampled_box_is_ticked_and_no_longer_marked_unrun() {
    let needle = "Query terms sampled from the corpus with a fixed seed";
    assert_ticked(needle);
    let line = plan_line(needle);
    assert!(
        !line.contains("not yet run"),
        "the 'not yet run' caveat must go once the run has happened: {line}"
    );
}

#[test]
fn warm_latency_box_is_ticked() {
    assert_ticked("Latency p50 / p95 / p99, warm");
}

#[test]
fn cpu_per_1k_queries_box_is_ticked() {
    assert_ticked("CPU seconds per 1,000 queries");
}
