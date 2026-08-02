//! Takes raw measurements captured by `bench/run.sh` and renders the
//! `docs/benchmarks.md` table (issue #13). Plain positional CLI args plus
//! two latency-sample files (one float per line) -- no JSON dependency
//! needed for a handful of scalars.
//!
//! Usage:
//!   render_report <solr_startup_idle_mb> <solr_post_index_mb> <solr_load_mb> \
//!                 <solr_cold_ms> <solr_image_mb> <solr_index_mb> \
//!                 <solr_latencies_warm_file> <solr_latencies_cold_file> \
//!                 <wf_startup_idle_mb> <wf_post_index_mb> <wf_load_mb> \
//!                 <wf_cold_ms> <wf_image_mb> <wf_index_mb> \
//!                 <wf_latencies_warm_file> <wf_latencies_cold_file> \
//!                 <corpus_size> <out_markdown_path>
//!
//! Each engine's cold-latency file sits immediately after that engine's warm
//! one, so each engine's arguments stay contiguous (issue #251).

use wayfinder_bench::results::{BenchmarkResults, EngineMeasurements, render_markdown_table};

fn read_latencies(path: &str) -> Vec<f64> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("reading latencies file {path}: {e}"))
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            l.trim()
                .parse::<f64>()
                .unwrap_or_else(|e| panic!("bad latency {l:?}: {e}"))
        })
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 18 {
        eprintln!(
            "usage: render_report <solr_startup_idle_mb> <solr_post_index_mb> <solr_load_mb> \
             <solr_cold_ms> <solr_image_mb> <solr_index_mb> <solr_latencies_warm_file> \
             <solr_latencies_cold_file> \
             <wf_startup_idle_mb> <wf_post_index_mb> <wf_load_mb> <wf_cold_ms> \
             <wf_image_mb> <wf_index_mb> <wf_latencies_warm_file> <wf_latencies_cold_file> \
             <corpus_size> <out_markdown_path>"
        );
        std::process::exit(2);
    }

    let f = |i: usize| {
        args[i]
            .parse::<f64>()
            .unwrap_or_else(|e| panic!("bad number {}: {e}", args[i]))
    };

    let results = BenchmarkResults {
        solr: EngineMeasurements {
            resident_mem_startup_idle_mb: f(0),
            resident_mem_post_index_mb: f(1),
            resident_mem_load_mb: f(2),
            cold_start_ms: f(3),
            image_size_mb: f(4),
            index_size_mb: f(5),
            query_latencies_warm_ms: read_latencies(&args[6]),
            query_latencies_cold_ms: read_latencies(&args[7]),
        },
        wayfinder: EngineMeasurements {
            resident_mem_startup_idle_mb: f(8),
            resident_mem_post_index_mb: f(9),
            resident_mem_load_mb: f(10),
            cold_start_ms: f(11),
            image_size_mb: f(12),
            index_size_mb: f(13),
            query_latencies_warm_ms: read_latencies(&args[14]),
            query_latencies_cold_ms: read_latencies(&args[15]),
        },
        corpus_size: args[16]
            .parse::<u64>()
            .unwrap_or_else(|e| panic!("bad corpus_size {}: {e}", args[16])),
    };

    let table = render_markdown_table(&results);
    let corpus_size = results.corpus_size;
    let startup_idle_mb = results.wayfinder.resident_mem_startup_idle_mb;
    let startup_target_outcome = if startup_idle_mb < 50.0 {
        format!(
            "- Wayfinder met the PRD's <50 MB startup-idle resident-memory target at \
             {startup_idle_mb:.1} MB.\n"
        )
    } else {
        format!(
            "- Wayfinder missed the PRD's <50 MB startup-idle resident-memory target at \
             {startup_idle_mb:.1} MB.\n"
        )
    };
    let corpus_note = if corpus_size == 2_000_000 {
        let wayfinder_load_mb = results.wayfinder.resident_mem_load_mb;
        let wayfinder_post_index_mb = results.wayfinder.resident_mem_post_index_mb;
        let wayfinder_rss_delta_mb = wayfinder_load_mb - wayfinder_post_index_mb;
        let rss_delta_note = if wayfinder_rss_delta_mb >= 0.0 {
            format!(
                "RSS increased by {wayfinder_rss_delta_mb:.1} MB between that sample and the \
                 {wayfinder_load_mb:.1} MB maximum sampled during query load"
            )
        } else {
            format!(
                "RSS decreased by {:.1} MB between that sample and the \
                 {wayfinder_load_mb:.1} MB maximum sampled during query load",
                -wayfinder_rss_delta_mb
            )
        };
        let query_load_target_outcome = if wayfinder_load_mb < 500.0 {
            format!(
                "- Wayfinder met the PRD's <500 MB query-load resident-memory target at \
                 {wayfinder_load_mb:.1} MB; {rss_delta_note}.\n"
            )
        } else {
            format!(
                "- Wayfinder missed the PRD's <500 MB query-load resident-memory target: \
                 {wayfinder_post_index_mb:.1} MB was resident at the post-index sample, and \
                 {rss_delta_note}.\n"
            )
        };
        format!(
            "- This is a long manual local run outside CI; indexing is expected to dominate wall \
             time.\n\
             {startup_target_outcome}\
             {query_load_target_outcome}\
             - The harness does not distinguish allocator-resident memory from mmap-backed index \
             pages.\n"
        )
    } else {
        format!(
            "- **\"Resident memory, 2M docs under query load\" is only ever populated by a run \
             with a 2M-doc corpus** (see the row's own \"not measured\" state above otherwise, \
             and the \"Measurement path\" column for how each number was captured). The 2M corpus \
             is not automated (see `bench/README.md`); real 2M numbers require running \
             `bench/run.sh 42 2000000`.\n\
             {startup_target_outcome}"
        )
    };
    let doc = format!(
        "# Wayfinder vs Solr 9 -- benchmark results (issue #13)\n\n\
         Measured against PRD §8 targets, on a corpus of {corpus_size} docs generated \
         deterministically by `bench/src/corpus.rs` (seed 42). See `bench/run.sh` for the exact \
         measurement procedure and `bench/README.md` for how to reproduce, including the 2M-doc \
         run.\n\n\
         {table}\n\n\
         ## Notes\n\n\
         {corpus_note}\
         - The two latency rows measure different cache conditions, and the PRD does not say \
         which of the two p95 rows its `<= baseline` target refers to: the warm row compares a \
         cached Solr against an uncached Wayfinder (Solr serves it from its queryResultCache; \
         Wayfinder has no query result cache), while the cold row runs distinct queries against \
         caches flushed by a core RELOAD. Neither row is reported here as meeting or missing a \
         target, because which comparison the target means is an open product decision -- \
         #251 tracks settling it.\n\
         - Measured on a local Docker Desktop/OrbStack host, not dedicated hardware; absolute \
         numbers (especially Solr cold start, which benefits from a warm image cache and \
         may not reflect a cold pull) will vary by machine. Reproduce locally with \
         `bench/run.sh` for numbers specific to your environment.\n\
         - Every row's \"Measurement path\" column states, per engine, whether the number came \
         from a Docker container or a native host process; Solr always runs in a Docker \
         container in this harness and Wayfinder always runs as a native binary (except its \
         image-size row, which measures the built image, not a running process), so the two \
         engines' numbers are not directly comparable on overhead alone.\n"
    );
    std::fs::write(&args[17], doc).expect("write benchmarks.md");
    println!("wrote {}", args[17]);
}
