//! Issue #13: unit tests for the metrics-to-markdown-table logic in
//! isolation -- fixed, invented raw numbers in, exact `docs/benchmarks.md`
//! table text out. No real benchmark run involved.
//!
//! Table shape: six columns -- `Metric | Solr baseline | Wayfinder target |
//! Solr measured | Wayfinder measured | Measurement path`. Resident memory
//! has startup-idle, post-index/pre-query, and query-load phases. Formatting:
//! memory and image/index sizes as `{:.1} MB`, cold start as `{:.2} s`, p95
//! latency as `{:.2} ms`. PRD target/baseline text is reproduced ASCII-only
//! (`2-4 GB`, `10-30 s`) rather than with the PRD's en-dashes, per this repo's
//! ASCII-only convention for committed text.
//!
//! Issue #251: the single p95 row splits into two -- a warm-cache row (the
//! existing 100-sample series, same query repeated `N_QUERIES` times) and a
//! cold-cache row (one sample per distinct query term; 48 samples here,
//! matching the corpus's true count of distinct query terms after
//! excluding the 8 that parse to the same empty query -- see
//! `bench/tests/query_terms.rs`). `EngineMeasurements::query_latencies_ms`
//! is renamed to `query_latencies_warm_ms` and gains a sibling
//! `query_latencies_cold_ms`; this is a naming decision made at stage 1 (see
//! the test-writer handoff), not implied verbatim by the spec, so it is a
//! **compile-time** red until the implementor adds both fields under these
//! exact names.

use wayfinder_bench::results::{BenchmarkResults, EngineMeasurements, p95, render_markdown_table};

/// Measurement-path text for the two latency rows. Duplicated here (not
/// imported) because these are private formatting choices inside
/// `render_markdown_table`, pinned by string equality like every other cell
/// in this file.
const WARM_LATENCY_PATH: &str = "Solr: HTTP to the Docker container's published port, served from Solr's queryResultCache. Wayfinder: HTTP to the native process's bound port; Wayfinder has no query result cache.";
const COLD_LATENCY_PATH: &str = "Solr: HTTP to the Docker container's published port, after a core RELOAD flushed Solr's caches. Wayfinder: HTTP to the native process's bound port. Every query in this pass is distinct.";

fn sample_results(corpus_size: u64) -> BenchmarkResults {
    BenchmarkResults {
        solr: EngineMeasurements {
            resident_mem_startup_idle_mb: 987.0,
            resident_mem_post_index_mb: 2400.0,
            resident_mem_load_mb: 3200.0,
            cold_start_ms: 18_000.0,
            query_latencies_warm_ms: (1..=100).map(|v| v as f64).collect(),
            query_latencies_cold_ms: (1..=48).map(|v| v as f64).collect(),
            image_size_mb: 512.0,
            index_size_mb: 200.0,
        },
        wayfinder: EngineMeasurements {
            resident_mem_startup_idle_mb: 42.0,
            resident_mem_post_index_mb: 215.0,
            resident_mem_load_mb: 410.0,
            cold_start_ms: 350.0,
            query_latencies_warm_ms: (1..=100).map(|v| (v as f64) * 0.9).collect(),
            query_latencies_cold_ms: (1..=48).map(|v| (v as f64) * 0.9).collect(),
            image_size_mb: 24.0,
            index_size_mb: 230.0,
        },
        corpus_size,
    }
}

// --- p95 ---------------------------------------------------------------
//
// Interpretation (ambiguity flagged, see handoff): p95 is the nearest-rank
// percentile over the sorted samples, 1-indexed: `sorted[ceil(0.95 * n) - 1]`.

#[test]
fn p95_of_1_to_100_is_95() {
    let samples: Vec<f64> = (1..=100).map(|v| v as f64).collect();
    assert_eq!(p95(&samples), 95.0);
}

#[test]
fn p95_is_order_independent() {
    let mut samples: Vec<f64> = (1..=20).map(|v| v as f64).collect();
    samples.reverse();
    // ceil(0.95 * 20) = 19 -> 19th smallest value, 1-indexed.
    assert_eq!(p95(&samples), 19.0);
}

#[test]
fn p95_of_a_single_sample_is_that_sample() {
    assert_eq!(p95(&[7.5]), 7.5);
}

// Premise 3 (issue #251 spec): the cold pass's sample size is one per
// distinct query term (48 here per `query_terms.rs`'s confirmed true
// distinct-query count, after excluding the 8 terms that collide on
// Solr's parsed-empty-query cache key), much smaller than the warm pass's
// 100-200. Confirm the existing nearest-rank `p95` has no divide-by-zero /
// empty-slice surprise at that size, and produces the expected rank.
#[test]
fn p95_over_a_cold_pass_sized_sample_is_still_meaningful() {
    let samples: Vec<f64> = (1..=48).map(|v| v as f64).collect();
    // ceil(0.95 * 48) = 46 -> 46th smallest value, 1-indexed.
    assert_eq!(p95(&samples), 46.0);
}

// --- render_markdown_table ----------------------------------------------

// Round-2 review (issue #13) found the "2M docs under query load" row always
// rendered whatever corpus was actually benchmarked as if it were a real 2M
// run, and that no row disclosed container-vs-native measurement path. Both
// are now covered below: a sub-2M run must render "not measured" on that
// row, and every row carries an explicit "Measurement path" column.

#[test]
fn render_markdown_table_matches_the_expected_shape_for_a_2m_run() {
    let table = render_markdown_table(&sample_results(2_000_000));

    // Issue #63: the p95 row's corpus-size label must reflect the actual
    // `corpus_size` measured, not a hardcoded "50k docs" -- a 2M-doc run's
    // label must say "2000000 docs".
    let expected = format!(
        "\
| Metric | Solr baseline | Wayfinder target | Solr measured | Wayfinder measured | Measurement path |
|---|---|---|---|---|---|
| Resident memory, startup idle | ~1 GB | < 50 MB | 987.0 MB | 42.0 MB | Solr: Docker container (`docker stats`). Wayfinder: native process (`ps -o rss=`). RSS includes allocator-resident memory plus mmap-backed index pages. |
| Resident memory, post-index before query load (2000000 docs) | No PRD baseline | No PRD target | 2400.0 MB | 215.0 MB | Solr: Docker container (`docker stats`). Wayfinder: native process (`ps -o rss=`). RSS includes allocator-resident memory plus mmap-backed index pages. |
| Resident memory, 2M docs under query load | 2-4 GB | < 500 MB | 3200.0 MB | 410.0 MB | Solr: Docker container (`docker stats`). Wayfinder: native process (`ps -o rss=`). RSS includes allocator-resident memory plus mmap-backed index pages. |
| Cold start to first query served | 10-30 s | < 1 s | 18.00 s | 0.35 s | Solr: Docker container (`docker run` to first successful ping). Wayfinder: native process (binary launch to first successful ping). |
| p95 query latency, warm cache (facet+filter+highlight, 2000000 docs) | baseline | <= baseline | 95.00 ms | 85.50 ms | {WARM_LATENCY_PATH} |
| p95 query latency, cold cache (distinct queries, 2000000 docs) | baseline | <= baseline | 46.00 ms | 41.40 ms | {COLD_LATENCY_PATH} |
| Container image size | ~500 MB | < 30 MB | 512.0 MB | 24.0 MB | Both: Docker image size (`docker inspect`), not a running-container measurement. |
| Index size on disk | baseline | <= 1.2x baseline | 200.0 MB | 230.0 MB | Solr: size inside the Docker container's data volume (`docker exec du`). Wayfinder: size of the native process's data directory on the host (`du`). |"
    );

    assert_eq!(table.trim_end(), expected);
}

#[test]
fn render_markdown_table_does_not_fabricate_2m_numbers_for_a_smaller_run() {
    let table = render_markdown_table(&sample_results(50_000));

    let expected_row = "| Resident memory, 2M docs under query load | 2-4 GB | < 500 MB | not measured | not measured | Not measured: this run indexed 50000 docs, not 2M. |";
    let two_m_row = table
        .lines()
        .find(|line| line.starts_with("| Resident memory, 2M docs under query load"))
        .unwrap_or_else(|| panic!("expected a '2M docs under query load' row, got:\n{table}"));
    assert_eq!(
        two_m_row, expected_row,
        "the row specifically labeled '2M docs' must still say 'not measured' for a 50k run"
    );
    assert!(
        !two_m_row.contains("3200.0 MB"),
        "must not carry over the 50k load-under-query number into the '2M docs' row specifically"
    );
}

// --- issue #63: corpus-size mislabeling in the results table -----------
//
// #13's round-2 review left two real bugs here:
//   1. the p95 row's label hardcodes "(..., 50k docs)" regardless of the
//      actual `corpus_size` measured.
//   2. for sub-2M runs, the real resident-memory-under-load number is
//      computed but discarded -- only "not measured" is ever shown for
//      any corpus smaller than 2M, even though a real number exists.
//
// Interpretation (ambiguity flagged, see handoff): the new honest
// "Resident memory, {corpus_size} docs under query load" row is emitted
// when corpus_size != 2_000_000 (above or below), since a literal 2M
// run's own dedicated row already carries the same information under
// the "2M docs" label -- an extra row would be a duplicate, not a new
// disclosure.

#[test]
fn p95_label_reflects_the_actual_corpus_size_for_a_sub_2m_run() {
    let table = render_markdown_table(&sample_results(50_000));

    assert!(
        table.contains("p95 query latency, warm cache (facet+filter+highlight, 50000 docs)"),
        "expected the warm p95 label to interpolate the run's actual corpus_size (50000), got:\n{table}"
    );
    assert!(
        table.contains("p95 query latency, cold cache (distinct queries, 50000 docs)"),
        "expected the cold p95 label to interpolate the run's actual corpus_size (50000), got:\n{table}"
    );
    assert!(
        !table.contains("50k docs"),
        "must not hardcode '50k docs' regardless of the run's actual corpus_size, got:\n{table}"
    );
}

#[test]
fn p95_label_reflects_the_actual_corpus_size_for_an_arbitrary_run() {
    let table = render_markdown_table(&sample_results(500_000));

    assert!(
        table.contains("p95 query latency, warm cache (facet+filter+highlight, 500000 docs)"),
        "expected the warm p95 label to say '500000 docs' for a 500k run, got:\n{table}"
    );
    assert!(
        table.contains("p95 query latency, cold cache (distinct queries, 500000 docs)"),
        "expected the cold p95 label to say '500000 docs' for a 500k run, got:\n{table}"
    );
}

#[test]
fn sub_2m_run_surfaces_its_real_resident_memory_under_load_measurement() {
    let table = render_markdown_table(&sample_results(50_000));

    let expected_row = "| Resident memory, 50000 docs under query load | 2-4 GB | < 500 MB | 3200.0 MB | 410.0 MB | Solr: Docker container (`docker stats`). Wayfinder: native process (`ps -o rss=`). RSS includes allocator-resident memory plus mmap-backed index pages. |";
    let honest_row = table
        .lines()
        .find(|line| line.starts_with("| Resident memory, 50000 docs under query load"))
        .unwrap_or_else(|| panic!("expected an honest 'Resident memory, 50000 docs under query load' row, got:\n{table}"));
    assert_eq!(
        honest_row, expected_row,
        "the honest row must also carry the Measurement path column, not just the four number \
         columns"
    );
}

#[test]
fn a_2m_run_does_not_duplicate_the_resident_memory_row() {
    let table = render_markdown_table(&sample_results(2_000_000));

    assert!(
        !table.contains("Resident memory, 2000000 docs under query load"),
        "a 2M run's dedicated '2M docs' row already discloses this measurement; a duplicate \
         '2000000 docs' row should not also be emitted, got:\n{table}"
    );
}

// Issue #63 round-2 review, item 1: the old `corpus_size >= 2_000_000` gate
// meant a run LARGER than 2M (e.g. 5M) still got the fixed "2M docs" label
// on its dedicated row while its p95 row correctly said "5000000 docs" --
// the exact bug #63 exists to kill, just relocated. Only a run of exactly
// 2,000,000 docs may use the fixed "2M docs" row; every other size,
// above or below, gets the honest interpolated row instead.
#[test]
fn a_run_larger_than_2m_gets_the_honest_row_not_the_fixed_2m_label() {
    let table = render_markdown_table(&sample_results(5_000_000));

    let two_m_row = table
        .lines()
        .find(|line| line.starts_with("| Resident memory, 2M docs under query load"))
        .unwrap_or_else(|| panic!("expected a '2M docs under query load' row, got:\n{table}"));
    assert_eq!(
        two_m_row,
        "| Resident memory, 2M docs under query load | 2-4 GB | < 500 MB | not measured | not measured | Not measured: this run indexed 5000000 docs, not 2M. |",
        "a 5M run must not claim its numbers under the fixed '2M docs' label"
    );

    let honest_row = table
        .lines()
        .find(|line| line.starts_with("| Resident memory, 5000000 docs under query load"))
        .unwrap_or_else(|| panic!("expected an honest 'Resident memory, 5000000 docs under query load' row, got:\n{table}"));
    assert_eq!(
        honest_row,
        "| Resident memory, 5000000 docs under query load | 2-4 GB | < 500 MB | 3200.0 MB | 410.0 MB | Solr: Docker container (`docker stats`). Wayfinder: native process (`ps -o rss=`). RSS includes allocator-resident memory plus mmap-backed index pages. |"
    );
}

// Issue #251: the single p95 row split into two (warm cache, cold cache),
// so a literal 2M run now has 8 metric rows, not 7.
#[test]
fn render_markdown_table_has_exactly_the_header_plus_eight_metric_rows_for_a_literal_2m_run() {
    let table = render_markdown_table(&sample_results(2_000_000));
    let lines: Vec<&str> = table.lines().collect();
    assert_eq!(
        lines.len(),
        10,
        "expected 1 header + 1 separator + 8 metric rows (the p95 row split into warm-cache \
         and cold-cache rows, issue #251), got {} lines:\n{}",
        lines.len(),
        table
    );
}

// Issue #63 round-2 review, item 4: pin the sub-2M table's full shape, not
// just that specific strings appear somewhere in it -- a sub-2M (or >2M)
// run has eight metric rows (startup, post-index, fixed-2M not-measured,
// and actual memory-under-load plus the four non-memory metrics), so the
// table is 1 header + 1 separator + 8 metric rows = 10 lines.
// Issue #251: the single p95 row split into two (warm cache, cold cache),
// so a sub-2M run now has 9 metric rows, not 8.
#[test]
fn render_markdown_table_has_exactly_the_header_plus_nine_metric_rows_for_a_sub_2m_run() {
    let table = render_markdown_table(&sample_results(50_000));
    let lines: Vec<&str> = table.lines().collect();
    assert_eq!(
        lines.len(),
        11,
        "expected 1 header + 1 separator + 9 metric rows (startup, post-index, fixed-2M \
         not-measured, actual under-load memory, cold start, warm p95, cold p95, image, index), \
         got {} lines:\n{}",
        lines.len(),
        table
    );

    let expected = format!(
        "\
| Metric | Solr baseline | Wayfinder target | Solr measured | Wayfinder measured | Measurement path |
|---|---|---|---|---|---|
| Resident memory, startup idle | ~1 GB | < 50 MB | 987.0 MB | 42.0 MB | Solr: Docker container (`docker stats`). Wayfinder: native process (`ps -o rss=`). RSS includes allocator-resident memory plus mmap-backed index pages. |
| Resident memory, post-index before query load (50000 docs) | No PRD baseline | No PRD target | 2400.0 MB | 215.0 MB | Solr: Docker container (`docker stats`). Wayfinder: native process (`ps -o rss=`). RSS includes allocator-resident memory plus mmap-backed index pages. |
| Resident memory, 2M docs under query load | 2-4 GB | < 500 MB | not measured | not measured | Not measured: this run indexed 50000 docs, not 2M. |
| Resident memory, 50000 docs under query load | 2-4 GB | < 500 MB | 3200.0 MB | 410.0 MB | Solr: Docker container (`docker stats`). Wayfinder: native process (`ps -o rss=`). RSS includes allocator-resident memory plus mmap-backed index pages. |
| Cold start to first query served | 10-30 s | < 1 s | 18.00 s | 0.35 s | Solr: Docker container (`docker run` to first successful ping). Wayfinder: native process (binary launch to first successful ping). |
| p95 query latency, warm cache (facet+filter+highlight, 50000 docs) | baseline | <= baseline | 95.00 ms | 85.50 ms | {WARM_LATENCY_PATH} |
| p95 query latency, cold cache (distinct queries, 50000 docs) | baseline | <= baseline | 46.00 ms | 41.40 ms | {COLD_LATENCY_PATH} |
| Container image size | ~500 MB | < 30 MB | 512.0 MB | 24.0 MB | Both: Docker image size (`docker inspect`), not a running-container measurement. |
| Index size on disk | baseline | <= 1.2x baseline | 200.0 MB | 230.0 MB | Solr: size inside the Docker container's data volume (`docker exec du`). Wayfinder: size of the native process's data directory on the host (`du`). |"
    );

    assert_eq!(table.trim_end(), expected);
}

// Issue #243: startup RSS, post-index/pre-query RSS, and query-load RSS are
// separate phases. The table must make that distinction visible rather than
// presenting a generic "idle" number whose collection point is unclear.
#[test]
fn memory_rows_name_the_startup_and_post_index_phases_and_explain_rss_scope() {
    let table = render_markdown_table(&sample_results(2_000_000));
    let memory_rows: Vec<&str> = table
        .lines()
        .filter(|line| line.starts_with("| Resident memory,"))
        .collect();

    let startup_row = memory_rows
        .iter()
        .find(|line| line.contains("startup idle"))
        .unwrap_or_else(|| {
            panic!(
                "expected a distinct startup-idle RSS row, got memory rows:\n{}",
                memory_rows.join("\n")
            )
        });
    let post_index_row = memory_rows
        .iter()
        .find(|line| line.contains("post-index before query load"))
        .unwrap_or_else(|| {
            panic!(
                "expected a distinct post-index-before-query-load RSS row, got memory rows:\n{}",
                memory_rows.join("\n")
            )
        });
    let query_load_row = memory_rows
        .iter()
        .find(|line| line.contains("under query load"))
        .unwrap_or_else(|| {
            panic!(
                "expected the query-load RSS row to remain distinct, got memory rows:\n{}",
                memory_rows.join("\n")
            )
        });

    assert_eq!(
        memory_rows.len(),
        3,
        "a literal 2M run must render one memory row per phase (startup idle, post-index/pre-query, and query load)"
    );
    assert!(
        startup_row.contains("987.0 MB | 42.0 MB"),
        "startup row must use startup-idle measurements, got:\n{startup_row}"
    );
    assert!(
        post_index_row.contains("2400.0 MB | 215.0 MB"),
        "post-index row must use distinct post-index measurements, got:\n{post_index_row}"
    );
    assert!(
        query_load_row.contains("3200.0 MB | 410.0 MB"),
        "query-load row must retain load measurements, got:\n{query_load_row}"
    );
    for row in memory_rows {
        assert!(
            row.contains("RSS includes allocator-resident memory plus mmap-backed index pages."),
            "every resident-memory row must disclose RSS's allocator and mmap scope, got:\n{row}"
        );
    }
}
