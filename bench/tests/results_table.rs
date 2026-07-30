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

    // Issue #63: the p95 row's corpus-size label must reflect the actual
    // `corpus_size` measured, not a hardcoded "50k docs" -- a 2M-doc run's
    // label must say "2000000 docs".
    let expected = "\
| Metric | Solr baseline | Wayfinder target | Solr measured | Wayfinder measured | Measurement path |
|---|---|---|---|---|---|
| Resident memory, idle | ~1 GB | < 50 MB | 987.0 MB | 42.0 MB | Solr: Docker container (`docker stats`). Wayfinder: native process (`ps -o rss=`). |
| Resident memory, 2M docs under query load | 2-4 GB | < 500 MB | 3200.0 MB | 410.0 MB | Solr: Docker container (`docker stats`). Wayfinder: native process (`ps -o rss=`). |
| Cold start to first query served | 10-30 s | < 1 s | 18.00 s | 0.35 s | Solr: Docker container (`docker run` to first successful ping). Wayfinder: native process (binary launch to first successful ping). |
| p95 query latency (facet+filter+highlight, 2000000 docs) | baseline | <= baseline | 95.00 ms | 85.50 ms | Solr: HTTP to the Docker container's published port. Wayfinder: HTTP to the native process's bound port. |
| Container image size | ~500 MB | < 30 MB | 512.0 MB | 24.0 MB | Both: Docker image size (`docker inspect`), not a running-container measurement. |
| Index size on disk | baseline | <= 1.2x baseline | 200.0 MB | 230.0 MB | Solr: size inside the Docker container's data volume (`docker exec du`). Wayfinder: size of the native process's data directory on the host (`du`). |";

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
// only when corpus_size < 2_000_000, since a 2M run's own dedicated row
// already carries the same information under the "2M docs" label -- an
// extra row would be a duplicate, not a new disclosure.

#[test]
fn p95_label_reflects_the_actual_corpus_size_for_a_sub_2m_run() {
    let table = render_markdown_table(&sample_results(50_000));

    assert!(
        table.contains("p95 query latency (facet+filter+highlight, 50000 docs)"),
        "expected the p95 label to interpolate the run's actual corpus_size (50000), got:\n{table}"
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
        table.contains("p95 query latency (facet+filter+highlight, 500000 docs)"),
        "expected the p95 label to say '500000 docs' for a 500k run, got:\n{table}"
    );
}

#[test]
fn sub_2m_run_surfaces_its_real_resident_memory_under_load_measurement() {
    let table = render_markdown_table(&sample_results(50_000));

    let expected_row = "| Resident memory, 50000 docs under query load | 2-4 GB | < 500 MB | 3200.0 MB | 410.0 MB | Solr: Docker container (`docker stats`). Wayfinder: native process (`ps -o rss=`). |";
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
        "| Resident memory, 5000000 docs under query load | 2-4 GB | < 500 MB | 3200.0 MB | 410.0 MB | Solr: Docker container (`docker stats`). Wayfinder: native process (`ps -o rss=`). |"
    );
}

#[test]
fn render_markdown_table_has_exactly_the_header_plus_six_metric_rows_for_a_literal_2m_run() {
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

// Issue #63 round-2 review, item 4: pin the sub-2M table's full shape, not
// just that specific strings appear somewhere in it -- a sub-2M (or >2M)
// run has a seventh metric row (the honest memory-under-load row), so the
// table is 1 header + 1 separator + 7 metric rows = 9 lines.
#[test]
fn render_markdown_table_has_exactly_the_header_plus_seven_metric_rows_for_a_sub_2m_run() {
    let table = render_markdown_table(&sample_results(50_000));
    let lines: Vec<&str> = table.lines().collect();
    assert_eq!(
        lines.len(),
        9,
        "expected 1 header + 1 separator + 7 metric rows (six PRD rows plus the honest \
         memory-under-load row), got {} lines:\n{}",
        lines.len(),
        table
    );

    let expected = "\
| Metric | Solr baseline | Wayfinder target | Solr measured | Wayfinder measured | Measurement path |
|---|---|---|---|---|---|
| Resident memory, idle | ~1 GB | < 50 MB | 987.0 MB | 42.0 MB | Solr: Docker container (`docker stats`). Wayfinder: native process (`ps -o rss=`). |
| Resident memory, 2M docs under query load | 2-4 GB | < 500 MB | not measured | not measured | Not measured: this run indexed 50000 docs, not 2M. |
| Resident memory, 50000 docs under query load | 2-4 GB | < 500 MB | 3200.0 MB | 410.0 MB | Solr: Docker container (`docker stats`). Wayfinder: native process (`ps -o rss=`). |
| Cold start to first query served | 10-30 s | < 1 s | 18.00 s | 0.35 s | Solr: Docker container (`docker run` to first successful ping). Wayfinder: native process (binary launch to first successful ping). |
| p95 query latency (facet+filter+highlight, 50000 docs) | baseline | <= baseline | 95.00 ms | 85.50 ms | Solr: HTTP to the Docker container's published port. Wayfinder: HTTP to the native process's bound port. |
| Container image size | ~500 MB | < 30 MB | 512.0 MB | 24.0 MB | Both: Docker image size (`docker inspect`), not a running-container measurement. |
| Index size on disk | baseline | <= 1.2x baseline | 200.0 MB | 230.0 MB | Solr: size inside the Docker container's data volume (`docker exec du`). Wayfinder: size of the native process's data directory on the host (`du`). |";

    assert_eq!(table.trim_end(), expected);
}
