//! Throughput under load for `wf-bench` (issue #447): searches per second with
//! a fixed number of users searching at once. Each user is a std thread making
//! blocking requests back to back, and every user walks the same URL sequence.

use std::sync::Barrier;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::cold::percentiles;

/// Searches in flight at once, one run per level.
pub const LEVELS: [usize; 3] = [1, 8, 32];

/// One level's result. Only successful searches count toward `requests`,
/// `rps`, and the latency percentiles; failures are counted in `errors`.
#[derive(Debug, Clone, PartialEq)]
pub struct LevelResult {
    pub concurrency: usize,
    pub requests: usize,
    pub errors: usize,
    pub rps: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
}

/// Runs `concurrency` users for `duration`, each walking `urls` in order from
/// the start and wrapping around. A search in flight at the deadline is let
/// finish, and the elapsed time includes it.
pub fn run_level(urls: &[String], concurrency: usize, duration: Duration) -> LevelResult {
    assert!(!urls.is_empty(), "load run with no URLs");
    assert!(concurrency > 0, "load run with no users");
    let start = Barrier::new(concurrency);
    let t0 = Instant::now();
    let deadline = t0 + duration;
    let runs: Vec<(Vec<f64>, usize)> = thread::scope(|s| {
        let workers: Vec<_> = (0..concurrency)
            .map(|_| s.spawn(|| worker(urls, &start, deadline)))
            .collect();
        workers
            .into_iter()
            .map(|w| w.join().expect("load worker"))
            .collect()
    });
    let elapsed = t0.elapsed().as_secs_f64();
    let errors = runs.iter().map(|(_, e)| e).sum();
    let lat: Vec<f64> = runs.into_iter().flat_map(|(l, _)| l).collect();
    let (p50_ms, p95_ms, p99_ms) = if lat.is_empty() {
        (0.0, 0.0, 0.0)
    } else {
        let p = percentiles(&lat);
        (p.p50, p.p95, p.p99)
    };
    LevelResult {
        concurrency,
        requests: lat.len(),
        errors,
        rps: lat.len() as f64 / elapsed,
        p50_ms,
        p95_ms,
        p99_ms,
    }
}

/// One user: latencies in ms of its successful searches, and its error count.
fn worker(urls: &[String], start: &Barrier, deadline: Instant) -> (Vec<f64>, usize) {
    let (mut lat, mut errors) = (Vec::new(), 0);
    start.wait();
    for url in urls.iter().cycle() {
        if Instant::now() >= deadline {
            break;
        }
        let t0 = Instant::now();
        // ureq reports 4xx/5xx as errors. The body is read so the transfer is
        // part of the timed search.
        match ureq::get(url).call().map(|r| r.into_string()) {
            Ok(Ok(_)) => lat.push(t0.elapsed().as_secs_f64() * 1000.0),
            _ => errors += 1,
        }
    }
    (lat, errors)
}

/// Runs each level in turn, for `duration` each.
pub fn run_levels(urls: &[String], levels: &[usize], duration: Duration) -> Vec<LevelResult> {
    levels
        .iter()
        .map(|&c| run_level(urls, c, duration))
        .collect()
}

/// One JSON row per level, in input order: `concurrency`, `requests`,
/// `errors`, `rps`, `p50`, `p95`, `p99`.
pub fn to_json(results: &[LevelResult]) -> Value {
    results
        .iter()
        .map(|r| {
            json!({
                "concurrency": r.concurrency,
                "requests": r.requests,
                "errors": r.errors,
                "rps": r.rps,
                "p50": r.p50_ms,
                "p95": r.p95_ms,
                "p99": r.p99_ms,
            })
        })
        .collect()
}
