//! Issue #448: the peak is the highest `anon` sampled during the run, not
//! `memory.peak` (which counts disk cache), and the total is reported apart.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use wayfinder_bench::memory::{MemStat, Sampler, to_json_with_total};

const MB: u64 = 1_048_576;

#[test]
fn peak_is_highest_anon_not_highest_anon_plus_file_and_total_is_separate() {
    // The second reading has the most anon; the third has the most anon + file.
    let readings = vec![
        MemStat { anon: 200 * MB, file: 100 * MB },
        MemStat { anon: 700 * MB, file: 100 * MB },
        MemStat { anon: 500 * MB, file: 2400 * MB },
        MemStat { anon: 300 * MB, file: 2400 * MB },
    ];
    let (tx, rx) = std::sync::mpsc::channel();
    let left = Arc::new(Mutex::new(readings.into_iter()));
    let feed = Arc::clone(&left);
    let sampler = Sampler::start(
        move || {
            let mut it = feed.lock().unwrap();
            let s = it.next();
            if it.len() == 0 {
                let _ = tx.send(());
            }
            // Once the series is used up, keep returning a low reading.
            s.unwrap_or(MemStat { anon: MB, file: 3000 * MB })
        },
        Duration::from_millis(1),
    );
    rx.recv_timeout(Duration::from_secs(10)).expect("sampler read every reading");
    let peak = sampler.stop();
    assert_eq!(peak, 700 * MB);

    let idle = MemStat { anon: 100 * MB, file: 2400 * MB };
    let post = MemStat { anon: 300 * MB, file: 2500 * MB };
    let v = to_json_with_total(&idle, &post, peak, 3200 * MB);
    assert_eq!(v["mem_peak_mb"].as_f64(), Some(700.0));
    assert_eq!(v["mem_peak_total_mb"].as_f64(), Some(3200.0));
    assert_eq!(v["mem_idle_mb"].as_f64(), Some(100.0));
    assert_eq!(v["mem_post_index_file_mb"].as_f64(), Some(2500.0));
}

#[test]
fn sampler_reads_once_even_when_stopped_at_once() {
    let sampler = Sampler::start(|| MemStat { anon: 42, file: 9_000 }, Duration::from_secs(60));
    assert_eq!(sampler.stop(), 42);
}
