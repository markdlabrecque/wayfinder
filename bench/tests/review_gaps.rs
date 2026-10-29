//! Small benchmark tool gaps found in review (issue #481): `--cold 0`, Solr
//! baselines saved from a busy machine, and a noise file that forgets the
//! settings it was measured under.

mod support;

use support::fresh_scratch_dir;
use wayfinder_bench::baseline::{SolrKey, load_solr, save_solr_if_quiet};
use wayfinder_bench::cold::validate_cold_runs;
use wayfinder_bench::noise::{NoiseSettings, Spread, load_noise_for, save_noise_for};

fn key() -> SolrKey {
    SolrKey {
        corpus_docs: 1000,
        queries_hash: "abc123".to_string(),
        solr_image_digest: "sha256:aaaa".to_string(),
        limits: "cpus=2 mem=4g".to_string(),
        corpus_hash: "c0ffee".to_string(),
    }
}

fn metrics() -> Vec<(String, f64)> {
    vec![("p50_ms".to_string(), 12.5)]
}

#[test]
fn zero_cold_runs_is_rejected_up_front_and_one_is_fine() {
    assert!(validate_cold_runs(0).is_err());
    assert!(validate_cold_runs(1).is_ok());
    assert!(validate_cold_runs(20).is_ok());
}

#[test]
fn a_solr_baseline_measured_on_a_busy_machine_is_not_saved() {
    let path = fresh_scratch_dir("busy").join("solr-baseline.json");
    let saved = save_solr_if_quiet(&path, &key(), &metrics(), Some("load average 6.2")).unwrap();
    assert!(!saved);
    assert!(
        !path.exists(),
        "busy run must not leave a baseline to reuse"
    );
    assert_eq!(load_solr(&path, &key()).unwrap(), None);
}

#[test]
fn a_solr_baseline_measured_on_a_quiet_machine_is_saved_and_reused() {
    let path = fresh_scratch_dir("quiet").join("solr-baseline.json");
    assert!(save_solr_if_quiet(&path, &key(), &metrics(), None).unwrap());
    assert_eq!(load_solr(&path, &key()).unwrap(), Some(metrics()));
}

fn settings() -> NoiseSettings {
    NoiseSettings {
        docs: 1000,
        iters: 50,
        cold: 5,
        solr_key: key(),
    }
}

fn spread() -> Vec<(String, Spread)> {
    vec![(
        "p50_ms".to_string(),
        Spread {
            min: 10.0,
            max: 13.0,
        },
    )]
}

#[test]
fn noise_is_reused_under_the_settings_it_was_measured_with() {
    let path = fresh_scratch_dir("noise-same").join("noise.json");
    save_noise_for(&path, &settings(), &spread()).unwrap();
    assert_eq!(load_noise_for(&path, &settings()).unwrap(), Some(spread()));
}

#[test]
fn noise_measured_under_other_settings_is_not_reused() {
    let path = fresh_scratch_dir("noise-stale").join("noise.json");
    save_noise_for(&path, &settings(), &spread()).unwrap();

    let mut s = settings();
    s.docs = 2000;
    assert_eq!(load_noise_for(&path, &s).unwrap(), None, "docs");

    let mut s = settings();
    s.iters = 100;
    assert_eq!(load_noise_for(&path, &s).unwrap(), None, "iters");

    let mut s = settings();
    s.cold = 0;
    assert_eq!(load_noise_for(&path, &s).unwrap(), None, "cold");

    let mut s = settings();
    s.solr_key.corpus_hash = "decaf0".to_string();
    assert_eq!(
        load_noise_for(&path, &s).unwrap(),
        None,
        "solr baseline key"
    );
}
