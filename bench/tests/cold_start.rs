//! Issue #446: cold-start measurement for `wf-bench`.
//!
//! The Docker restart itself is side-effectful, so these hermetic tests pin the
//! seams around it: the percentile maths, the per-search-type JSON record, and a
//! page-cache eviction that works over a data dir without root.

mod support;

use std::fs;
use std::path::Path;

use support::fresh_scratch_dir;
use wayfinder_bench::cold::{evict_page_cache, percentiles, summarize};

fn write(path: &Path, bytes: usize) {
    fs::create_dir_all(path.parent().unwrap()).expect("create parent");
    fs::write(path, vec![b'x'; bytes]).expect("write file");
}

#[test]
fn percentiles_use_nearest_rank_on_unsorted_samples() {
    let mut samples: Vec<f64> = (1..=100).map(f64::from).collect();
    samples.reverse();
    let p = percentiles(&samples);
    assert_eq!((p.p50, p.p95, p.p99), (50.0, 95.0, 99.0));
}

#[test]
fn percentiles_of_a_small_sample_round_up_to_the_slowest_run() {
    let samples = [4.0, 1.0, 3.0, 2.0, 10.0, 9.0, 8.0, 7.0, 6.0, 5.0];
    let p = percentiles(&samples);
    assert_eq!((p.p50, p.p95, p.p99), (5.0, 10.0, 10.0));
}

#[test]
fn summary_records_p50_p95_p99_for_every_search_type() {
    let runs = vec![
        ("term".to_string(), vec![10.0, 20.0, 30.0, 40.0]),
        ("phrase".to_string(), vec![5.0, 6.0]),
    ];
    let v = summarize(&runs);
    let rows = v
        .as_array()
        .expect("summary is an array, one row per search type");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["query"], "term");
    assert_eq!(rows[0]["n"], 4);
    assert_eq!(rows[0]["p50"], 20.0);
    assert_eq!(rows[0]["p95"], 40.0);
    assert_eq!(rows[0]["p99"], 40.0);
    assert_eq!(rows[1]["query"], "phrase");
    assert_eq!(rows[1]["n"], 2);
    assert_eq!(rows[1]["p50"], 5.0);
    assert_eq!(rows[1]["p99"], 6.0);
}

#[test]
fn eviction_visits_every_index_file_including_nested_ones() {
    let dir = fresh_scratch_dir("cold-evict");
    write(&dir.join("a.idx"), 4096);
    write(&dir.join("segments/b.term"), 10_000);
    write(&dir.join("segments/deep/c.store"), 1);
    let n = evict_page_cache(&dir).expect("eviction must work without root");
    assert_eq!(n, 3, "one eviction per regular file");
}

#[test]
fn eviction_leaves_file_contents_intact() {
    let dir = fresh_scratch_dir("cold-evict-intact");
    let f = dir.join("seg.idx");
    write(&f, 8192);
    evict_page_cache(&dir).expect("evict");
    assert_eq!(fs::read(&f).expect("read back"), vec![b'x'; 8192]);
}

#[test]
fn eviction_of_a_missing_data_dir_is_an_error_not_a_silent_warm_run() {
    let dir = fresh_scratch_dir("cold-evict-missing");
    assert!(evict_page_cache(&dir.join("nope")).is_err());
}

#[test]
fn cold_path_needs_no_root() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for file in ["src/cold.rs", "src/bin/wf-bench.rs"] {
        let src = fs::read_to_string(root.join(file))
            .unwrap_or_else(|e| panic!("{file} must exist for the cold-start path: {e}"));
        for banned in ["drop_caches", "sudo"] {
            assert!(
                !src.contains(banned),
                "{file} must not use `{banned}`: cold runs may not need root"
            );
        }
    }
}
