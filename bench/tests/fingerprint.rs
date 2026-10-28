//! Answer fingerprints (issue #445): each run saves a digest of Wayfinder's
//! answers per query shape, and a run whose answers differ from the last
//! accepted run's says so, even when it passes.

mod support;

use std::collections::BTreeMap;

use serde_json::json;
use support::fresh_scratch_dir;
use wayfinder_bench::fingerprint::{
    answers_changed_line, changed_shapes, digest, load_accepted, save_accepted,
};

fn answer(qtime: u64, id: &str) -> serde_json::Value {
    json!({
        "responseHeader": {"status": 0, "QTime": qtime},
        "response": {"numFound": 1, "docs": [{"id": id}]},
    })
}

#[test]
fn the_digest_ignores_query_time() {
    assert_eq!(
        digest(&[answer(3, "a"), answer(4, "b")]),
        digest(&[answer(90, "a"), answer(1, "b")])
    );
}

#[test]
fn the_digest_catches_a_changed_id() {
    assert_ne!(
        digest(&[answer(3, "a"), answer(4, "b")]),
        digest(&[answer(3, "a"), answer(4, "c")])
    );
}

#[test]
fn the_digest_catches_a_changed_order() {
    assert_ne!(
        digest(&[answer(3, "a"), answer(3, "b")]),
        digest(&[answer(3, "b"), answer(3, "a")])
    );
}

fn digests(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn saved_fingerprints_round_trip_and_none_means_nothing_accepted() {
    let path = fresh_scratch_dir("fp").join("wf-answers.json");
    assert_eq!(load_accepted(&path).unwrap(), None);
    let d = digests(&[("term", "aa"), ("phrase", "bb")]);
    save_accepted(&path, &d).unwrap();
    assert_eq!(load_accepted(&path).unwrap(), Some(d));
}

#[test]
fn a_corrupt_fingerprint_file_is_an_error() {
    let path = fresh_scratch_dir("fp-corrupt").join("wf-answers.json");
    std::fs::write(&path, "not json").unwrap();
    assert!(load_accepted(&path).is_err());
}

#[test]
fn only_the_shapes_whose_digest_differs_are_reported() {
    let accepted = digests(&[("term", "aa"), ("phrase", "bb"), ("facet", "cc")]);
    let now = digests(&[("term", "aa"), ("phrase", "XX"), ("facet", "YY")]);
    assert_eq!(changed_shapes(&accepted, &now), vec!["facet", "phrase"]);
    assert!(changed_shapes(&accepted, &accepted).is_empty());
}

#[test]
fn a_shape_missing_from_either_side_counts_as_changed() {
    let accepted = digests(&[("term", "aa")]);
    let now = digests(&[("term", "aa"), ("phrase", "bb")]);
    assert_eq!(changed_shapes(&accepted, &now), vec!["phrase"]);
    assert_eq!(changed_shapes(&now, &accepted), vec!["phrase"]);
}

#[test]
fn the_changed_line_names_the_shapes_and_is_silent_when_nothing_changed() {
    let line = answers_changed_line(&["facet".to_string(), "phrase".to_string()]).unwrap();
    assert!(!line.contains('\n'), "{line}");
    assert!(line.starts_with("answers changed: "), "{line}");
    assert!(line.contains("facet") && line.contains("phrase"), "{line}");
    assert_eq!(answers_changed_line(&[]), None);
}
