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

fn sample_results() -> BenchmarkResults {
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

#[test]
fn render_markdown_table_matches_the_expected_shape() {
    let table = render_markdown_table(&sample_results());

    let expected = "\
| Metric | Solr baseline | Wayfinder target | Solr measured | Wayfinder measured |
|---|---|---|---|---|
| Resident memory, idle | ~1 GB | < 50 MB | 987.0 MB | 42.0 MB |
| Resident memory, 2M docs under query load | 2-4 GB | < 500 MB | 3200.0 MB | 410.0 MB |
| Cold start to first query served | 10-30 s | < 1 s | 18.00 s | 0.35 s |
| p95 query latency (facet+filter+highlight, 50k docs) | baseline | <= baseline | 95.00 ms | 85.50 ms |
| Container image size | ~500 MB | < 30 MB | 512.0 MB | 24.0 MB |
| Index size on disk | baseline | <= 1.2x baseline | 200.0 MB | 230.0 MB |";

    assert_eq!(table.trim_end(), expected);
}

#[test]
fn render_markdown_table_has_exactly_the_header_plus_six_metric_rows() {
    let table = render_markdown_table(&sample_results());
    let lines: Vec<&str> = table.lines().collect();
    assert_eq!(
        lines.len(),
        8,
        "expected 1 header + 1 separator + 6 metric rows, got {} lines:\n{}",
        lines.len(),
        table
    );
}
