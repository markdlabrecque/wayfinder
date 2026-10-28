//! Add-to-searchable time (issue #450): how long after a page is saved it can
//! be found. Each run adds one doc carrying a token no other doc has, then
//! polls `/select` for that token until `numFound` is 1.
//!
//! The add carries `commitWithin`, as `search_api_solr` sends it (finding 76
//! in `solr-ref/FINDINGS.md`): Drupal never sends an explicit commit, and an
//! add with no commit at all is never searchable on Wayfinder's defaults.
//! Anything else the engine does on its own is the caller's `commit_settings`
//! label, which is written into the report as-is.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// `search_api_solr`'s default `commitWithin`, in ms.
pub const COMMIT_WITHIN_MS: u64 = 1000;
const POLL: Duration = Duration::from_millis(5);
const GIVE_UP: Duration = Duration::from_secs(60);

fn num_found(url: &str) -> u64 {
    let body = ureq::get(url)
        .call()
        .unwrap_or_else(|e| panic!("GET {url}: {e}"))
        .into_string()
        .expect("response body");
    let v: Value = serde_json::from_str(&body).expect("JSON response");
    v["response"]["numFound"].as_u64().expect("numFound")
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

/// Times `runs` add-then-find cycles against the core at `base` and returns
/// the samples, p50 and p95 in ms, plus `commit_settings`.
pub fn measure(base: &str, runs: usize, commit_settings: &str) -> Value {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let update = format!("{base}/update?commitWithin={COMMIT_WITHIN_MS}");
    let samples: Vec<f64> = (0..runs)
        .map(|i| {
            // Letters and digits only, so every analyzer keeps it one term.
            let token = format!("wffresh{nonce}r{i}");
            let doc = json!([{"id": token, "title": token, "body": token}]);
            let select = format!("{base}/select?q=body%3A{token}&rows=0&wt=json");
            let t0 = Instant::now();
            if let Err(e) = ureq::post(&update)
                .set("Content-Type", "application/json")
                .send_string(&doc.to_string())
            {
                panic!("POST {update}: {e}");
            }
            while num_found(&select) < 1 {
                assert!(t0.elapsed() < GIVE_UP, "{token} not searchable after {GIVE_UP:?}");
                std::thread::sleep(POLL);
            }
            t0.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    let mut sorted = samples.clone();
    sorted.sort_by(f64::total_cmp);
    json!({
        "commit_settings": commit_settings,
        "runs": runs,
        "samples_ms": samples,
        "p50_ms": pct(&sorted, 0.50),
        "p95_ms": pct(&sorted, 0.95),
    })
}
