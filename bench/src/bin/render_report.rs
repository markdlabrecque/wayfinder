//! Takes raw measurements captured by `bench/run.sh` and renders the
//! `docs/benchmarks.md` table (issue #13). Plain positional CLI args plus
//! two latency-sample files (one float per line) -- no JSON dependency
//! needed for a handful of scalars.
//!
//! Usage:
//!   render_report <solr_idle_mb> <solr_load_mb> <solr_cold_ms> \
//!                 <solr_image_mb> <solr_index_mb> <solr_latencies_file> \
//!                 <wf_idle_mb> <wf_load_mb> <wf_cold_ms> \
//!                 <wf_image_mb> <wf_index_mb> <wf_latencies_file> \
//!                 <out_markdown_path>

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
    if args.len() != 13 {
        eprintln!(
            "usage: render_report <solr_idle_mb> <solr_load_mb> <solr_cold_ms> \
             <solr_image_mb> <solr_index_mb> <solr_latencies_file> \
             <wf_idle_mb> <wf_load_mb> <wf_cold_ms> <wf_image_mb> <wf_index_mb> \
             <wf_latencies_file> <out_markdown_path>"
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
            resident_mem_idle_mb: f(0),
            resident_mem_load_mb: f(1),
            cold_start_ms: f(2),
            image_size_mb: f(3),
            index_size_mb: f(4),
            query_latencies_ms: read_latencies(&args[5]),
        },
        wayfinder: EngineMeasurements {
            resident_mem_idle_mb: f(6),
            resident_mem_load_mb: f(7),
            cold_start_ms: f(8),
            image_size_mb: f(9),
            index_size_mb: f(10),
            query_latencies_ms: read_latencies(&args[11]),
        },
    };

    let table = render_markdown_table(&results);
    let doc = format!(
        "# Wayfinder vs Solr 9 -- benchmark results (issue #13)\n\n\
         Measured against PRD §8 targets. 50k-doc corpus, generated deterministically by \
         `bench/src/corpus.rs` (seed 42). See `bench/run.sh` for the exact measurement \
         procedure and `bench/README.md` for how to reproduce, including the 2M-doc run.\n\n\
         {table}\n\n\
         ## Notes\n\n\
         - **\"Resident memory, 2M docs under query load\" is measured from the 50k run**, \
         not a real 2M-doc run: the 2M corpus is not automated (see `bench/README.md`), so \
         this row's measured columns are the 50k under-load numbers, not the PRD's 2M \
         scenario. Real 2M numbers require running `bench/run.sh 42 2000000` and are not \
         yet captured.\n\
         - Measured on a local Docker Desktop/OrbStack host, not dedicated hardware; absolute \
         numbers (especially Solr cold start, which benefits from a warm image cache and \
         may not reflect a cold pull) will vary by machine. Reproduce locally with \
         `bench/run.sh` for numbers specific to your environment.\n\
         - Wayfinder's resident memory is host-process RSS (`ps -o rss=`); Solr's is \
         `docker stats`' cgroup memory accounting. Both reflect real resident memory, but via \
         different measurement paths, since Wayfinder runs as a native binary and Solr runs in \
         a container in this harness.\n"
    );
    std::fs::write(&args[12], doc).expect("write benchmarks.md");
    println!("wrote {}", args[12]);
}
