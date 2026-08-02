//! Issue #253: Solr's transaction log is not Lucene index storage, so the
//! index-size comparison must measure Solr's `data/index` directory rather
//! than its enclosing `data` directory. Wayfinder's measurement remains its
//! native Tantivy data directory, including its tiny schema/analyzer metadata.

mod support;

use support::run_sh_source;
use wayfinder_bench::results::{BenchmarkResults, EngineMeasurements, render_markdown_table};

const INDEX_SIZE_PATH: &str = "Solr: Lucene `data/index` directory inside the Docker container (`docker exec du`); excludes tlog. Wayfinder: Tantivy/native data directory on the host (`du`), including tiny Wayfinder schema/analyzer metadata.";

fn sample_results() -> BenchmarkResults {
    BenchmarkResults {
        solr: EngineMeasurements {
            resident_mem_startup_idle_mb: 1.0,
            resident_mem_post_index_mb: 2.0,
            resident_mem_load_mb: 3.0,
            cold_start_ms: 4.0,
            query_latencies_warm_ms: vec![5.0],
            query_latencies_cold_ms: vec![6.0],
            image_size_mb: 7.0,
            index_size_mb: 8.0,
        },
        wayfinder: EngineMeasurements {
            resident_mem_startup_idle_mb: 9.0,
            resident_mem_post_index_mb: 10.0,
            resident_mem_load_mb: 11.0,
            cold_start_ms: 12.0,
            query_latencies_warm_ms: vec![13.0],
            query_latencies_cold_ms: vec![14.0],
            image_size_mb: 15.0,
            index_size_mb: 16.0,
        },
        corpus_size: 50_000,
    }
}

#[test]
fn run_sh_measures_solrs_lucene_index_not_its_data_directory_or_tlog() {
    let source = run_sh_source();
    let solr_measurement = source
        .lines()
        .find(|line| line.starts_with("SOLR_INDEX_KB=$("))
        .expect("run.sh must assign SOLR_INDEX_KB from Solr's on-container index measurement");

    assert!(
        solr_measurement.contains("du -sk \"/var/solr/data/$SOLR_CORE/data/index\""),
        "SOLR_INDEX_KB must measure the Lucene data/index directory, excluding Solr's tlog; got:\n{solr_measurement}"
    );
    assert!(
        source.contains("WF_INDEX_KB=$(du -sk \"$WF_DATA\""),
        "WF_INDEX_KB must continue measuring Wayfinder's native Tantivy data directory"
    );
}

#[test]
fn rendered_index_size_path_names_engines_storage_and_comparison_scope() {
    let table = render_markdown_table(&sample_results());
    let index_size_row = table
        .lines()
        .find(|line| line.starts_with("| Index size on disk |"))
        .unwrap_or_else(|| panic!("expected an index-size row, got:\n{table}"));

    assert_eq!(
        index_size_row,
        format!(
            "| Index size on disk | baseline | <= 1.2x baseline | 8.0 MB | 16.0 MB | {INDEX_SIZE_PATH} |"
        ),
        "the Measurement path must name Solr's Lucene data/index and Wayfinder's Tantivy/native data directory, while disclosing tlog exclusion and Wayfinder metadata scope"
    );
}
