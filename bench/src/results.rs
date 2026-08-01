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
    /// RSS sampled after the engine's health check and before corpus indexing.
    pub resident_mem_startup_idle_mb: f64,
    /// RSS sampled after indexing commits and before query load begins.
    pub resident_mem_post_index_mb: f64,
    /// Maximum RSS sampled while query load runs.
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
    /// Number of docs actually indexed for this run. Only a run with
    /// `corpus_size == 2_000_000` (the literal PRD scenario) may report real
    /// numbers on the "2M docs under query load" row -- anything else
    /// renders "not measured" there rather than passing off a
    /// load-under-this-corpus number as the PRD's 2M scenario (see issue
    /// #13 round-2 review, item 4). Every corpus size other than exactly 2M
    /// -- above or below -- also gets its own honest
    /// "{corpus_size} docs under query load" row so the real measurement
    /// is never silently discarded (see issue #63 round-2 review, item 1).
    pub corpus_size: u64,
}

/// Renders the exact 6-column markdown table pinned by
/// `bench/tests/results_table.rs`. Each engine has three distinct resident
/// memory phases: startup idle, post-index before query load, and under query
/// load. The post-index row uses the actual corpus size and deliberately has
/// no PRD baseline or target. A non-2M run also retains the fixed-2M row as
/// "not measured" and renders its actual under-load row. PRD baseline/target
/// text is ASCII-only (`2-4 GB`, `10-30 s`, `<=`, `1.2x`).
pub fn render_markdown_table(results: &BenchmarkResults) -> String {
    let solr = &results.solr;
    let wf = &results.wayfinder;

    let solr_p95 = p95(&solr.query_latencies_ms);
    let wf_p95 = p95(&wf.query_latencies_ms);

    const MEM_PATH: &str = "Solr: Docker container (`docker stats`). Wayfinder: native process (`ps -o rss=`). RSS includes allocator-resident memory plus mmap-backed index pages.";
    const COLD_START_PATH: &str = "Solr: Docker container (`docker run` to first successful ping). Wayfinder: native process (binary launch to first successful ping).";
    const LATENCY_PATH: &str = "Solr: HTTP to the Docker container's published port. Wayfinder: HTTP to the native process's bound port.";
    const IMAGE_PATH: &str =
        "Both: Docker image size (`docker inspect`), not a running-container measurement.";
    const INDEX_PATH: &str = "Solr: size inside the Docker container's data volume (`docker exec du`). Wayfinder: size of the native process's data directory on the host (`du`).";

    let mut out = String::new();
    out.push_str(
        "| Metric | Solr baseline | Wayfinder target | Solr measured | Wayfinder measured | Measurement path |\n",
    );
    out.push_str("|---|---|---|---|---|---|\n");
    out.push_str(&format!(
        "| Resident memory, startup idle | ~1 GB | < 50 MB | {:.1} MB | {:.1} MB | {MEM_PATH} |\n",
        solr.resident_mem_startup_idle_mb, wf.resident_mem_startup_idle_mb
    ));
    out.push_str(&format!(
        "| Resident memory, post-index before query load ({} docs) | No PRD baseline | No PRD target | {:.1} MB | {:.1} MB | {MEM_PATH} |\n",
        results.corpus_size, solr.resident_mem_post_index_mb, wf.resident_mem_post_index_mb
    ));
    if results.corpus_size == 2_000_000 {
        out.push_str(&format!(
            "| Resident memory, 2M docs under query load | 2-4 GB | < 500 MB | {:.1} MB | {:.1} MB | {MEM_PATH} |\n",
            solr.resident_mem_load_mb, wf.resident_mem_load_mb
        ));
    } else {
        out.push_str(&format!(
            "| Resident memory, 2M docs under query load | 2-4 GB | < 500 MB | not measured | not measured | Not measured: this run indexed {} docs, not 2M. |\n",
            results.corpus_size
        ));
        out.push_str(&format!(
            "| Resident memory, {} docs under query load | 2-4 GB | < 500 MB | {:.1} MB | {:.1} MB | {MEM_PATH} |\n",
            results.corpus_size, solr.resident_mem_load_mb, wf.resident_mem_load_mb
        ));
    }
    out.push_str(&format!(
        "| Cold start to first query served | 10-30 s | < 1 s | {:.2} s | {:.2} s | {COLD_START_PATH} |\n",
        solr.cold_start_ms / 1000.0,
        wf.cold_start_ms / 1000.0
    ));
    out.push_str(&format!(
        "| p95 query latency (facet+filter+highlight, {} docs) | baseline | <= baseline | {solr_p95:.2} ms | {wf_p95:.2} ms | {LATENCY_PATH} |\n",
        results.corpus_size
    ));
    out.push_str(&format!(
        "| Container image size | ~500 MB | < 30 MB | {:.1} MB | {:.1} MB | {IMAGE_PATH} |\n",
        solr.image_size_mb, wf.image_size_mb
    ));
    out.push_str(&format!(
        "| Index size on disk | baseline | <= 1.2x baseline | {:.1} MB | {:.1} MB | {INDEX_PATH} |",
        solr.index_size_mb, wf.index_size_mb
    ));

    out
}
