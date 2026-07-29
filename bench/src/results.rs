//! Turns raw measurements into the `docs/benchmarks.md` table (issue #13,
//! PRD §8). Pure formatting logic -- no measurement, no I/O.

/// Nearest-rank percentile, 1-indexed: `sorted[ceil(0.95 * n) - 1]`.
/// `samples` need not be pre-sorted; this sorts a copy.
pub fn p95(samples: &[f64]) -> f64 {
    assert!(
        !samples.is_empty(),
        "p95 of an empty sample set is undefined"
    );
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("samples must not be NaN"));
    let n = sorted.len();
    let rank = ((0.95 * n as f64).ceil() as usize).max(1);
    sorted[rank - 1]
}

/// One engine's raw measurements for a single benchmark run.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineMeasurements {
    pub resident_mem_idle_mb: f64,
    pub resident_mem_load_mb: f64,
    pub cold_start_ms: f64,
    pub query_latencies_ms: Vec<f64>,
    pub image_size_mb: f64,
    pub index_size_mb: f64,
}

/// Both engines' measurements for one benchmark run, ready to render.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchmarkResults {
    pub solr: EngineMeasurements,
    pub wayfinder: EngineMeasurements,
}

/// Renders the exact 5-column markdown table pinned by
/// `bench/tests/results_table.rs`: header + separator + the six PRD §8
/// metric rows, in PRD order. PRD's baseline/target text is reproduced
/// ASCII-only (`2-4 GB`, `10-30 s`, `<=`, `1.2x`) per this repo's
/// ASCII-only convention for committed text.
pub fn render_markdown_table(results: &BenchmarkResults) -> String {
    let solr = &results.solr;
    let wf = &results.wayfinder;

    let solr_p95 = p95(&solr.query_latencies_ms);
    let wf_p95 = p95(&wf.query_latencies_ms);

    let mut out = String::new();
    out.push_str(
        "| Metric | Solr baseline | Wayfinder target | Solr measured | Wayfinder measured |\n",
    );
    out.push_str("|---|---|---|---|---|\n");
    out.push_str(&format!(
        "| Resident memory, idle | ~1 GB | < 50 MB | {:.1} MB | {:.1} MB |\n",
        solr.resident_mem_idle_mb, wf.resident_mem_idle_mb
    ));
    out.push_str(&format!(
        "| Resident memory, 2M docs under query load | 2-4 GB | < 500 MB | {:.1} MB | {:.1} MB |\n",
        solr.resident_mem_load_mb, wf.resident_mem_load_mb
    ));
    out.push_str(&format!(
        "| Cold start to first query served | 10-30 s | < 1 s | {:.2} s | {:.2} s |\n",
        solr.cold_start_ms / 1000.0,
        wf.cold_start_ms / 1000.0
    ));
    out.push_str(&format!(
        "| p95 query latency (facet+filter+highlight, 50k docs) | baseline | <= baseline | {solr_p95:.2} ms | {wf_p95:.2} ms |\n"
    ));
    out.push_str(&format!(
        "| Container image size | ~500 MB | < 30 MB | {:.1} MB | {:.1} MB |\n",
        solr.image_size_mb, wf.image_size_mb
    ));
    out.push_str(&format!(
        "| Index size on disk | baseline | <= 1.2x baseline | {:.1} MB | {:.1} MB |",
        solr.index_size_mb, wf.index_size_mb
    ));

    out
}
