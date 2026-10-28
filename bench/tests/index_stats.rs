//! Issue #449: indexing throughput for the full corpus, recorded per engine.
//!
//! `wf-bench` times indexing from the first batch to the final commit, then
//! turns the raw counts into the JSON fields written to `bench/out/<ts>.json`.
//! That conversion is the seam tested here: `wayfinder_bench::index_stats`.
//! It is a compile-time red until the function exists with this signature.
//! (These tests live in the `bench` crate and run under
//! `cargo test --manifest-path bench/Cargo.toml`, as CI does.)

use wayfinder_bench::index_stats::index_stats;

#[test]
fn records_docs_per_second_and_bytes_per_second() {
    // 2,000 docs, 50 MB, 4 s from first batch to final commit.
    let stats = index_stats(2_000, 50_000_000, 4.0);

    assert_eq!(stats["index_docs"].as_u64(), Some(2_000));
    assert_eq!(stats["index_bytes"].as_u64(), Some(50_000_000));
    assert_eq!(stats["index_secs"].as_f64(), Some(4.0));
    assert_eq!(stats["index_docs_per_s"].as_f64(), Some(500.0));
    assert_eq!(stats["index_bytes_per_s"].as_f64(), Some(12_500_000.0));
}

#[test]
fn the_same_work_in_the_same_time_gives_the_same_rate_for_either_engine() {
    // Both engines are fed the same docs in the same batches, so equal
    // inputs must produce equal, directly comparable numbers.
    let solr = index_stats(10_000, 8_000_000, 2.5);
    let wayfinder = index_stats(10_000, 8_000_000, 2.5);
    assert_eq!(solr, wayfinder);
    assert_eq!(solr["index_docs_per_s"].as_f64(), Some(4_000.0));
}
