//! Issue #13: unit tests for the metrics-to-markdown-table logic in
//! isolation -- fixed, invented raw numbers in, exact `docs/benchmarks.md`
//! table text out. No real benchmark run involved.
//!
//! Table shape (this test's design decision, since the issue only specifies
//! "measured numbers beside the PRD targets"): five columns --
//! `Metric | Solr baseline | Wayfinder target | Solr measured | Wayfinder measured`
//! -- with the six PRD §8 metric rows in PRD order. Formatting: memory and
//! image/index sizes as `{:.1} MB`, cold start as `{:.2} s`, p95 latency as
//! `{:.2} ms`. PRD target/baseline text is reproduced ASCII-only (`2-4 GB`,
//! `10-30 s`) rather than with the PRD's en-dashes, per this repo's
//! ASCII-only convention for committed text.

use wayfinder_bench::results::{BenchmarkResults, EngineMeasurements, p95, render_markdown_table};

fn sample_results(corpus_size: u64) -> BenchmarkResults {
    BenchmarkResults {
        solr: EngineMeasurements {
            resident_mem_idle_mb: 987.0,
            resident_mem_load_mb: 3200.0,
            cold_start_ms: 18_000.0,
            query_latencies_ms: (1..=100).map(|v| v as f64).collect(),
            image_size_mb: 512.0,
            index_size_mb: 200.0,
        },
        wayfinder: EngineMeasurements {
            resident_mem_idle_mb: 42.0,
            resident_mem_load_mb: 410.0,
            cold_start_ms: 350.0,
            query_latencies_ms: (1..=100).map(|v| (v as f64) * 0.9).collect(),
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

// --- render_markdown_table ----------------------------------------------

// Round-2 review (issue #13) found the "2M docs under query load" row always
// rendered whatever corpus was actually benchmarked as if it were a real 2M
// run, and that no row disclosed container-vs-native measurement path. Both
// are now covered below: a sub-2M run must render "not measured" on that
// row, and every row carries an explicit "Measurement path" column.

#[test]
fn render_markdown_table_matches_the_expected_shape_for_a_2m_run() {
    let table = render_markdown_table(&sample_results(2_000_000));

    let expected = "\
| Metric | Solr baseline | Wayfinder target | Solr measured | Wayfinder measured | Measurement path |
|---|---|---|---|---|---|
| Resident memory, idle | ~1 GB | < 50 MB | 987.0 MB | 42.0 MB | Solr: Docker container (`docker stats`). Wayfinder: native process (`ps -o rss=`). |
| Resident memory, 2M docs under query load | 2-4 GB | < 500 MB | 3200.0 MB | 410.0 MB | Solr: Docker container (`docker stats`). Wayfinder: native process (`ps -o rss=`). |
| Cold start to first query served | 10-30 s | < 1 s | 18.00 s | 0.35 s | Solr: Docker container (`docker run` to first successful ping). Wayfinder: native process (binary launch to first successful ping). |
| p95 query latency (facet+filter+highlight, 50k docs) | baseline | <= baseline | 95.00 ms | 85.50 ms | Solr: HTTP to the Docker container's published port. Wayfinder: HTTP to the native process's bound port. |
| Container image size | ~500 MB | < 30 MB | 512.0 MB | 24.0 MB | Both: Docker image size (`docker inspect`), not a running-container measurement. |
| Index size on disk | baseline | <= 1.2x baseline | 200.0 MB | 230.0 MB | Solr: size inside the Docker container's data volume (`docker exec du`). Wayfinder: size of the native process's data directory on the host (`du`). |";

    assert_eq!(table.trim_end(), expected);
}

#[test]
fn render_markdown_table_does_not_fabricate_2m_numbers_for_a_smaller_run() {
    let table = render_markdown_table(&sample_results(50_000));

    let expected_row = "| Resident memory, 2M docs under query load | 2-4 GB | < 500 MB | not measured | not measured | Not measured: this run indexed 50000 docs, not 2M. |";
    assert!(
        table.contains(expected_row),
        "expected the 2M row to say 'not measured' for a 50k run, got:\n{table}"
    );
    assert!(
        !table.contains("3200.0 MB"),
        "must not carry over the 50k load-under-query number into the 2M row"
    );
}

#[test]
fn render_markdown_table_has_exactly_the_header_plus_six_metric_rows() {
    let table = render_markdown_table(&sample_results(2_000_000));
    let lines: Vec<&str> = table.lines().collect();
    assert_eq!(
        lines.len(),
        8,
        "expected 1 header + 1 separator + 6 metric rows, got {} lines:\n{}",
        lines.len(),
        table
    );
}
