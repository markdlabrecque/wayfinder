//! Cold-start measurement for `wf-bench` (issue #446): drop an engine's index
//! files from the OS page cache without root, and summarise repeated cold runs
//! as nearest-rank percentiles per search type.

use std::fs::{self, File};
use std::io;
use std::path::Path;

use serde_json::{Value, json};

/// Cold-run latency percentiles, in the unit of the samples.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Percentiles {
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
}

/// Nearest-rank p50/p95/p99 of `samples`, in any order. Every value returned
/// is a real sample: with only a few cold runs, p95 and p99 are the slowest.
pub fn percentiles(samples: &[f64]) -> Percentiles {
    assert!(!samples.is_empty(), "percentiles of no samples");
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    // Integer rank, so 95% of 100 is exactly rank 95, not 94.99... rounded.
    let rank = |p: usize| sorted[(p * sorted.len()).div_ceil(100) - 1];
    Percentiles {
        p50: rank(50),
        p95: rank(95),
        p99: rank(99),
    }
}

/// One JSON row per search type, in input order: `query`, `n`, `p50`, `p95`,
/// `p99`.
pub fn summarize(runs: &[(String, Vec<f64>)]) -> Value {
    runs.iter()
        .map(|(query, samples)| {
            let p = percentiles(samples);
            json!({
                "query": query,
                "n": samples.len(),
                "p50": p.p50,
                "p95": p.p95,
                "p99": p.p99,
            })
        })
        .collect()
}

/// Asks the kernel to drop every regular file under `dir` from the page cache
/// with `posix_fadvise(POSIX_FADV_DONTNEED)`. Needs only read access to the
/// files, not root. Pages still mapped by a running process stay resident, so
/// stop the engine first. Returns the number of files evicted; a missing `dir`
/// is an error, so a typo can't pass for a cold run.
pub fn evict_page_cache(dir: &Path) -> io::Result<usize> {
    let mut n = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            n += evict_page_cache(&entry.path())?;
        } else if kind.is_file() {
            evict_file(&entry.path())?;
            n += 1;
        }
    }
    Ok(n)
}

fn evict_file(path: &Path) -> io::Result<()> {
    let file = File::open(path)?;
    // Dirty pages are not dropped, so flush first. fsync works on a read-only fd.
    file.sync_data()?;
    fadvise_dontneed(&file)
}

#[cfg(target_os = "linux")]
fn fadvise_dontneed(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    const POSIX_FADV_DONTNEED: i32 = 4;
    unsafe extern "C" {
        fn posix_fadvise(fd: i32, offset: i64, len: i64, advice: i32) -> i32;
    }
    // Offset 0, length 0: the whole file. Returns the error number directly.
    let rc = unsafe { posix_fadvise(file.as_raw_fd(), 0, 0, POSIX_FADV_DONTNEED) };
    match rc {
        0 => Ok(()),
        e => Err(io::Error::from_raw_os_error(e)),
    }
}

#[cfg(not(target_os = "linux"))]
fn fadvise_dontneed(_file: &File) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "page-cache eviction needs Linux posix_fadvise",
    ))
}
