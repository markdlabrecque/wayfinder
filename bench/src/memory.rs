//! Memory for `wf-bench` (issue #448): what each engine really holds, without
//! the operating system's disk cache. A cgroup's `memory.current` counts page
//! cache, so the main number is `anon` from `memory.stat`, with `file` (the
//! cache) reported on its own. `memory.peak` has the same problem, so the peak
//! is the highest `anon` a `Sampler` sees, and `memory.peak` is reported apart
//! as the total.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::{Value, json};

const MB: f64 = 1_048_576.0;

/// Bytes from one read of a cgroup's `memory.stat`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MemStat {
    /// Anonymous memory: heap, stacks, and everything not backed by a file.
    pub anon: u64,
    /// Page cache charged to the cgroup.
    pub file: u64,
}

/// Reads `anon` and `file` out of a `memory.stat` by exact key, so `anon_thp`
/// and `file_mapped` don't count. A missing key or non-numeric value is
/// `None`, so a bad read can't pass for zero.
pub fn parse_memory_stat(text: &str) -> Option<MemStat> {
    let get = |key: &str| {
        text.lines()
            .find_map(|l| {
                let (k, v) = l.split_once(' ')?;
                (k == key).then(|| v.trim().parse::<u64>())
            })?
            .ok()
    };
    Some(MemStat {
        anon: get("anon")?,
        file: get("file")?,
    })
}

/// The memory fields of an engine's stats, in MB. `mem_*_mb` is anonymous
/// memory; `mem_*_file_mb` is the disk cache, kept apart.
pub fn to_json(idle: &MemStat, post_index: &MemStat, peak_bytes: u64) -> Value {
    json!({
        "mem_idle_mb": idle.anon as f64 / MB,
        "mem_idle_file_mb": idle.file as f64 / MB,
        "mem_post_index_mb": post_index.anon as f64 / MB,
        "mem_post_index_file_mb": post_index.file as f64 / MB,
        "mem_peak_mb": peak_bytes as f64 / MB,
    })
}

/// `to_json` plus `mem_peak_total_mb`: `memory.peak`, the cgroup's high-water
/// mark with disk cache, kept apart from the anon peak in `mem_peak_mb`.
pub fn to_json_with_total(
    idle: &MemStat,
    post_index: &MemStat,
    peak_anon: u64,
    peak_total: u64,
) -> Value {
    let mut v = to_json(idle, post_index, peak_anon);
    v["mem_peak_total_mb"] = json!(peak_total as f64 / MB);
    v
}

/// Reads `memory.stat` on a background thread every `every` until stopped,
/// keeping the highest `anon` seen.
pub struct Sampler {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<u64>,
}

impl Sampler {
    /// Starts sampling with one read straight away. A panic in `read` comes
    /// back out of `stop`.
    pub fn start(mut read: impl FnMut() -> MemStat + Send + 'static, every: Duration) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            let mut peak = 0;
            // Reads once more after the stop, so the peak covers up to it.
            loop {
                peak = peak.max(read().anon);
                if flag.load(Ordering::Relaxed) {
                    return peak;
                }
                thread::sleep(every);
            }
        });
        Sampler { stop, handle }
    }

    /// Stops sampling and returns the highest `anon` seen, in bytes.
    pub fn stop(self) -> u64 {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.join().expect("memory sampler")
    }
}
