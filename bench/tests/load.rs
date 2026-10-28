//! Issue #447: searches per second with several users at once, at fixed
//! levels (1, 8, 32), recorded for both engines by `wf-bench`.
//!
//! Docker is side-effectful, so these hermetic tests drive the load runner
//! against a local server that takes a fixed time per request and tracks how
//! many requests it is serving at once.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use wayfinder_bench::load::{LEVELS, run_level, run_levels, to_json};

const DELAY_MS: u64 = 30;

struct Server {
    base: String,
    peak: Arc<AtomicUsize>,
    hits: Arc<Mutex<HashMap<String, usize>>>,
}

/// One thread per connection; each request takes `DELAY_MS` and answers
/// `status`. Records per-path hit counts and the peak number in flight.
fn serve(status: u16) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap();
    let peak = Arc::new(AtomicUsize::new(0));
    let hits: Arc<Mutex<HashMap<String, usize>>> = Arc::default();
    let (p, h) = (peak.clone(), hits.clone());
    let live = Arc::new(AtomicUsize::new(0));
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.expect("accept");
            let (p, h, live) = (p.clone(), h.clone(), live.clone());
            thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let line = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                p.fetch_max(now, Ordering::SeqCst);
                *h.lock().unwrap().entry(path).or_default() += 1;
                thread::sleep(Duration::from_millis(DELAY_MS));
                live.fetch_sub(1, Ordering::SeqCst);
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
                );
                let _ = stream.write_all(resp.as_bytes());
            });
        }
    });
    Server {
        base: format!("http://{addr}"),
        peak,
        hits,
    }
}

fn urls(base: &str) -> Vec<String> {
    ["/select?q=alpha", "/select?q=beta", "/select?q=gamma"]
        .iter()
        .map(|p| format!("{base}{p}"))
        .collect()
}

#[test]
fn levels_are_one_eight_and_thirty_two() {
    assert_eq!(LEVELS, [1, 8, 32]);
}

#[test]
fn level_runs_that_many_searches_at_once() {
    let s = serve(200);
    let r = run_level(&urls(&s.base), 8, Duration::from_millis(600));
    assert_eq!(r.concurrency, 8);
    assert!(r.requests > 0 && r.errors == 0, "{r:?}");
    assert_eq!(
        s.peak.load(Ordering::SeqCst),
        8,
        "exactly 8 in flight at peak"
    );
}

#[test]
fn one_at_a_time_never_overlaps() {
    let s = serve(200);
    let r = run_level(&urls(&s.base), 1, Duration::from_millis(400));
    assert!(r.requests > 0, "{r:?}");
    assert_eq!(s.peak.load(Ordering::SeqCst), 1);
}

#[test]
fn rps_reflects_concurrency() {
    let s = serve(200);
    let one = run_level(&urls(&s.base), 1, Duration::from_millis(600));
    let eight = run_level(&urls(&s.base), 8, Duration::from_millis(600));
    // A fixed-latency server scales with users, so 8 at once is well over 3x.
    assert!(one.rps > 5.0, "{one:?}");
    assert!(eight.rps > one.rps * 3.0, "1: {one:?} 8: {eight:?}");
}

#[test]
fn latency_percentiles_are_recorded_and_ordered() {
    let s = serve(200);
    let r = run_level(&urls(&s.base), 4, Duration::from_millis(500));
    assert!(r.p50_ms >= DELAY_MS as f64, "{r:?}");
    assert!(r.p50_ms <= r.p95_ms && r.p95_ms <= r.p99_ms, "{r:?}");
}

#[test]
fn every_worker_walks_the_same_varied_query_sequence() {
    let s = serve(200);
    let u = urls(&s.base);
    run_level(&u, 4, Duration::from_millis(500));
    let hits = s.hits.lock().unwrap();
    for url in &u {
        let path = url.strip_prefix(&s.base).unwrap();
        let n = hits.get(path).copied().unwrap_or(0);
        assert!(n >= 4, "each of 4 workers must reach {path}, got {n}");
    }
}

#[test]
fn failed_searches_are_errors_not_throughput() {
    let s = serve(503);
    let r = run_level(&urls(&s.base), 2, Duration::from_millis(300));
    assert!(r.errors > 0, "{r:?}");
    assert_eq!(r.requests, 0, "{r:?}");
    assert_eq!(r.rps, 0.0, "{r:?}");
}

#[test]
fn run_levels_gives_one_json_row_per_level_in_order() {
    let s = serve(200);
    let results = run_levels(&urls(&s.base), &LEVELS, Duration::from_millis(150));
    let rows = to_json(&results);
    let rows = rows.as_array().expect("array");
    assert_eq!(rows.len(), 3);
    for (row, level) in rows.iter().zip([1, 8, 32]) {
        assert_eq!(row["concurrency"], level);
        for key in ["rps", "p50", "p95", "p99", "errors"] {
            assert!(row.get(key).is_some(), "row missing {key}: {row}");
        }
    }
}

#[test]
fn wf_bench_records_load_for_each_engine() {
    // run_engine is shared by Solr and Wayfinder, so one record covers both.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = fs::read_to_string(root.join("src/bin/wf-bench.rs")).expect("read wf-bench.rs");
    for key in ["\"load\"", "run_levels", "LEVELS"] {
        assert!(src.contains(key), "wf-bench must record/use {key}");
    }
}
