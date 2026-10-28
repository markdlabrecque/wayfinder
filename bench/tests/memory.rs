//! Issue #448: memory each engine really holds, without the operating
//! system's disk cache, at idle, after indexing and at peak under load.
//!
//! Docker is side-effectful, so these hermetic tests pin the seams around it:
//! reading `anon` and `file` out of a cgroup `memory.stat`, the JSON the
//! three phases are recorded as, and that `wf-bench` stops using
//! `memory.current` (which counts page cache).

use std::fs;
use std::path::Path;

use wayfinder_bench::memory::{MemStat, parse_memory_stat, to_json};

const MB: u64 = 1_048_576;

// Prefixed keys come first on purpose: `anon_thp` and `file_mapped` must not
// be mistaken for `anon` and `file`.
const STAT: &str = "\
anon_thp 4194304
file_mapped 7000
file_dirty 12
kernel 999
anon 104857600
file 2516582400
shmem 0
slab 5000
";

#[test]
fn parses_anon_and_file_by_exact_key() {
    let s = parse_memory_stat(STAT).expect("parse");
    assert_eq!(s.anon, 104_857_600);
    assert_eq!(s.file, 2_516_582_400);
}

#[test]
fn stat_without_anon_is_an_error_not_zero() {
    assert!(parse_memory_stat("file 100\nshmem 0\n").is_none());
}

#[test]
fn stat_without_file_is_an_error_not_zero() {
    assert!(parse_memory_stat("anon 100\nshmem 0\n").is_none());
}

#[test]
fn stat_with_a_non_numeric_value_is_an_error() {
    assert!(parse_memory_stat("anon lots\nfile 1\n").is_none());
}

#[test]
fn json_reports_anon_as_the_memory_number_and_file_cache_separately() {
    let idle = MemStat { anon: 100 * MB, file: 2400 * MB };
    let post = MemStat { anon: 300 * MB, file: 2500 * MB };
    let v = to_json(&idle, &post, 900 * MB);
    assert_eq!(v["mem_idle_mb"].as_f64(), Some(100.0));
    assert_eq!(v["mem_idle_file_mb"].as_f64(), Some(2400.0));
    assert_eq!(v["mem_post_index_mb"].as_f64(), Some(300.0));
    assert_eq!(v["mem_post_index_file_mb"].as_f64(), Some(2500.0));
    assert_eq!(v["mem_peak_mb"].as_f64(), Some(900.0));
}

#[test]
fn wf_bench_reads_memory_stat_and_peak_and_not_memory_current() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = fs::read_to_string(root.join("src/bin/wf-bench.rs")).expect("read wf-bench.rs");
    assert!(src.contains("\"memory.stat\""), "wf-bench must read memory.stat");
    assert!(src.contains("\"memory.peak\""), "wf-bench must read memory.peak");
    assert!(
        !src.contains("\"memory.current\""),
        "memory.current counts disk cache; wf-bench must not use it"
    );
    assert!(src.contains("parse_memory_stat"), "wf-bench must use memory::parse_memory_stat");
}
