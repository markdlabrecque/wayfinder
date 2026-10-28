//! Footprint measurement for `wf-bench` (issue #451): an engine's index size on
//! disk, and the time from container start to its first successful search.

use std::fs;
use std::io;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

/// Total bytes of every regular file under `dir`, recursively. A missing `dir`
/// is an error, so a wrong path can't pass for an empty index.
pub fn dir_size_bytes(dir: &Path) -> io::Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            total += dir_size_bytes(&entry.path())?;
        } else if kind.is_file() {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

/// Polls `url` until it answers 200, and returns the milliseconds elapsed since
/// `since` (the container start, not this call). Refused connections and
/// non-200 answers are retried; no 200 within `timeout` of `since` is an error.
pub fn time_to_first_search(url: &str, since: Instant, timeout: Duration) -> io::Result<f64> {
    let deadline = since + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("no 200 from {url} within {timeout:?}"),
            ));
        }
        // ureq reports 4xx/5xx as errors, so anything but Ok(200) is a retry.
        if matches!(ureq::get(url).timeout(left).call(), Ok(r) if r.status() == 200) {
            return Ok(since.elapsed().as_secs_f64() * 1000.0);
        }
        thread::sleep(Duration::from_millis(20).min(left));
    }
}
