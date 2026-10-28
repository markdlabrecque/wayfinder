//! Keeping the `wf-bench` machine steady (issue #455): engines and the load
//! generator on separate fixed cores, the machine and both engine versions
//! recorded in every result, and a refusal (or warning with `--force`) when
//! the machine is already busy. Parsing is kept apart from the `/proc` reads
//! so it can be tested without Docker.

use serde_json::{Value, json};

/// Expands a Linux cpu list (`2-5`, `0,1,6-7`) into core numbers.
pub fn parse_cpu_list(list: &str) -> Result<Vec<usize>, String> {
    let mut cpus = Vec::new();
    for part in list.split(',') {
        let part = part.trim();
        let num = |s: &str| {
            s.trim()
                .parse::<usize>()
                .map_err(|_| format!("bad cpu list {list:?}: {s:?} is not a core number"))
        };
        let (lo, hi) = match part.split_once('-') {
            Some((lo, hi)) => (num(lo)?, num(hi)?),
            None => (num(part)?, num(part)?),
        };
        if lo > hi {
            return Err(format!("bad cpu list {list:?}: range {part:?} runs backwards"));
        }
        cpus.extend(lo..=hi);
    }
    Ok(cpus)
}

/// Engines and the load generator must not share a core, or the driver's
/// own work shows up as engine latency.
pub fn validate_cores(engines: &str, driver: &str) -> Result<(), String> {
    let engine_cpus = parse_cpu_list(engines)?;
    let shared: Vec<usize> = parse_cpu_list(driver)?
        .into_iter()
        .filter(|c| engine_cpus.contains(c))
        .collect();
    if shared.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "engine cores {engines} and driver cores {driver} share cores {shared:?}"
        ))
    }
}

/// The 1-minute figure from `/proc/loadavg`.
pub fn parse_loadavg(text: &str) -> Option<f64> {
    text.split_whitespace().next()?.parse().ok()
}

/// `Ok(None)` when the machine is quiet. Over `threshold` it is an error,
/// unless `force`, in which case the run goes ahead with a warning.
pub fn check_busy(load: f64, threshold: f64, force: bool) -> Result<Option<String>, String> {
    if load <= threshold {
        return Ok(None);
    }
    let busy = format!("machine is busy: 1-min load average {load:.2} is over {threshold:.2}");
    if force {
        Ok(Some(format!("{busy}; running anyway (--force), results may be noisy")))
    } else {
        Err(format!("{busy}; wait for it to settle or pass --force"))
    }
}

/// The `model name` line from `/proc/cpuinfo`.
pub fn parse_cpu_model(cpuinfo: &str) -> Option<String> {
    cpuinfo
        .lines()
        .filter(|l| l.starts_with("model name"))
        .find_map(|l| Some(l.split_once(':')?.1.trim().to_string()))
        .filter(|m| !m.is_empty())
}

/// `lucene.solr-spec-version` from Solr's `admin/info/system` response.
pub fn parse_solr_version(info: &str) -> Option<String> {
    let v: Value = serde_json::from_str(info).ok()?;
    v["lucene"]["solr-spec-version"].as_str().map(str::to_string)
}

/// What a result needs to be compared fairly with another one.
pub struct EnvInfo {
    pub kernel: String,
    pub cpu_model: String,
    pub engine_cpuset: String,
    pub driver_cpuset: String,
    pub wayfinder_git_sha: String,
    pub solr_version: String,
    pub solr_image: String,
    pub solr_heap: String,
}

pub fn env_json(info: &EnvInfo) -> Value {
    json!({
        "machine": {"kernel": info.kernel, "cpu_model": info.cpu_model},
        "cores": {"engines": info.engine_cpuset, "driver": info.driver_cpuset},
        "wayfinder": {"git_sha": info.wayfinder_git_sha},
        "solr": {"version": info.solr_version, "image": info.solr_image, "heap": info.solr_heap},
    })
}

unsafe extern "C" {
    fn sched_setaffinity(pid: i32, cpusetsize: usize, mask: *const u64) -> i32;
}

/// Pins the calling thread, and every thread or process it starts after
/// this, to `cpus`.
pub fn pin_to(cpus: &[usize]) -> Result<(), String> {
    // glibc's cpu_set_t: 1024 bits.
    let mut mask = [0u64; 16];
    for &c in cpus {
        if c >= 1024 {
            return Err(format!("core {c} is out of range"));
        }
        mask[c / 64] |= 1 << (c % 64);
    }
    // SAFETY: `mask` is a cpu_set_t-sized buffer that outlives the call, and
    // pid 0 means the calling thread.
    let rc = unsafe { sched_setaffinity(0, std::mem::size_of_val(&mask), mask.as_ptr()) };
    if rc == 0 {
        Ok(())
    } else {
        Err(format!(
            "pin to cores {cpus:?}: {}",
            std::io::Error::last_os_error()
        ))
    }
}
