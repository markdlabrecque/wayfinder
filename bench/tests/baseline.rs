//! Saved baselines (issue #454): Solr is measured once per dataset and settings
//! and reused; the last accepted Wayfinder score is saved so each run can print
//! its change from it.

mod support;

use support::fresh_scratch_dir;
use wayfinder_bench::baseline::{SolrKey, delta_line, load_solr, load_wf, save_solr, save_wf};

fn key() -> SolrKey {
    SolrKey {
        corpus_docs: 100_000,
        queries_hash: "abc123".to_string(),
        solr_image_digest: "sha256:aaaa".to_string(),
        limits: "cpus=2 mem=4g".to_string(),
    }
}

fn metrics() -> Vec<(String, f64)> {
    vec![("p50_ms".to_string(), 12.5), ("qps".to_string(), 340.0)]
}

#[test]
fn solr_numbers_saved_for_a_key_are_reused_for_the_same_key() {
    let path = fresh_scratch_dir("solr-same").join("solr-baseline.json");
    save_solr(&path, &key(), &metrics()).unwrap();
    assert_eq!(load_solr(&path, &key()).unwrap(), Some(metrics()));
}

#[test]
fn no_saved_solr_file_means_it_must_be_measured() {
    let path = fresh_scratch_dir("solr-none").join("solr-baseline.json");
    assert_eq!(load_solr(&path, &key()).unwrap(), None);
}

#[test]
fn a_changed_dataset_or_setting_makes_the_saved_solr_numbers_stale() {
    let path = fresh_scratch_dir("solr-stale").join("solr-baseline.json");
    save_solr(&path, &key(), &metrics()).unwrap();

    let mut k = key();
    k.corpus_docs = 200_000;
    assert_eq!(load_solr(&path, &k).unwrap(), None, "corpus size");

    let mut k = key();
    k.queries_hash = "def456".to_string();
    assert_eq!(load_solr(&path, &k).unwrap(), None, "query file hash");

    let mut k = key();
    k.solr_image_digest = "sha256:bbbb".to_string();
    assert_eq!(load_solr(&path, &k).unwrap(), None, "solr image digest");

    let mut k = key();
    k.limits = "cpus=4 mem=4g".to_string();
    assert_eq!(load_solr(&path, &k).unwrap(), None, "limits");
}

#[test]
fn a_corrupt_solr_file_is_an_error_not_silently_remeasured() {
    let path = fresh_scratch_dir("solr-corrupt").join("solr-baseline.json");
    std::fs::write(&path, "not json").unwrap();
    assert!(load_solr(&path, &key()).is_err());
}

#[test]
fn the_accepted_wayfinder_score_round_trips() {
    let path = fresh_scratch_dir("wf").join("wf-baseline.json");
    assert_eq!(load_wf(&path).unwrap(), None);
    save_wf(&path, 0.8).unwrap();
    assert_eq!(load_wf(&path).unwrap(), Some(0.8));
    save_wf(&path, 0.72).unwrap();
    assert_eq!(load_wf(&path).unwrap(), Some(0.72));
}

#[test]
fn the_delta_line_shows_an_improvement_as_a_negative_percent() {
    let line = delta_line(Some(0.80), 0.72);
    assert!(!line.contains('\n'), "{line}");
    assert!(line.contains("-10.0%"), "{line}");
}

#[test]
fn the_delta_line_shows_a_regression_as_a_positive_percent() {
    let line = delta_line(Some(0.80), 0.88);
    assert!(!line.contains('\n'), "{line}");
    assert!(line.contains("+10.0%"), "{line}");
}

#[test]
fn the_delta_line_says_so_when_nothing_has_been_accepted_yet() {
    let line = delta_line(None, 0.72);
    assert!(!line.contains('\n'), "{line}");
    assert!(line.contains("no accepted baseline"), "{line}");
}
