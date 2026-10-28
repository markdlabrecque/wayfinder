//! The wf-bench correctness gate (issue #440). Its whole job is to fail when
//! Wayfinder's answer stops making sense, so each case here is a way it must
//! fail, next to the nearest way it must still pass.

use serde_json::{Value, json};
use wayfinder_bench::check::check;

fn docs(num_found: u64, ids: &[&str]) -> Value {
    json!({"response": {"numFound": num_found,
        "docs": ids.iter().map(|id| json!({"id": id})).collect::<Vec<_>>()}})
}

fn with(mut base: Value, key: &str, value: Value) -> Value {
    base[key] = value;
    base
}

#[test]
fn ranked_allows_reordering_but_not_a_different_hit_count() {
    let solr = docs(9, &["a", "b", "c", "d", "e"]);
    assert!(check("ranked", &docs(9, &["e", "d", "c", "b", "a"]), &solr).is_ok());
    assert!(check("ranked", &docs(8, &["a", "b", "c", "d", "e"]), &solr).is_err());
}

#[test]
fn ranked_fails_when_top_results_mostly_differ() {
    let solr = docs(9, &["a", "b", "c", "d", "e"]);
    assert!(check("ranked", &docs(9, &["a", "b", "c", "d", "x"]), &solr).is_ok());
    assert!(check("ranked", &docs(9, &["a", "b", "c", "x", "y"]), &solr).is_err());
}

#[test]
fn exact_requires_the_same_order() {
    let solr = docs(3, &["a", "b", "c"]);
    assert!(check("exact", &docs(3, &["a", "b", "c"]), &solr).is_ok());
    assert!(check("exact", &docs(3, &["a", "c", "b"]), &solr).is_err());
}

#[test]
fn facet_counts_must_match() {
    let facets = |field: u64, range: u64| {
        json!({"facet_fields": {"initial": ["A", field, "B", 2]},
            "facet_ranges": {"len": {"counts": ["0", range, "10", 1], "gap": 10}}})
    };
    let answer = |field, range| with(docs(9, &[]), "facet_counts", facets(field, range));
    let solr = answer(7, 5);
    assert!(check("facet", &answer(7, 5), &solr).is_ok());
    assert!(check("facet", &answer(6, 5), &solr).is_err());
    assert!(check("facet", &answer(7, 4), &solr).is_err());
}

#[test]
fn stats_tolerate_float_noise_but_not_wrong_values() {
    let stats = |sum: f64| {
        json!({"stats_fields": {"len": {"min": 1.0, "max": 9.0, "count": 3, "missing": 0,
            "sum": sum, "mean": sum / 3.0, "stddev": 1.5, "sumOfSquares": 50.0}}})
    };
    let solr = with(docs(3, &[]), "stats", stats(15.0));
    let close = with(docs(3, &[]), "stats", stats(15.0 + 1e-12));
    assert!(check("stats", &close, &solr).is_ok());
    assert!(check("stats", &with(docs(3, &[]), "stats", stats(16.0)), &solr).is_err());
}

#[test]
fn group_counts_must_match_in_any_order() {
    let grouped = |a: u64, b: u64| {
        json!({"len_bucket": {"matches": a + b, "groups": [
            {"groupValue": "long", "doclist": {"numFound": a, "docs": []}},
            {"groupValue": "short", "doclist": {"numFound": b, "docs": []}}]}})
    };
    let solr = json!({"grouped": grouped(4, 2)});
    let swapped = json!({"grouped": {"len_bucket": {"matches": 6, "groups": [
        {"groupValue": "short", "doclist": {"numFound": 2, "docs": []}},
        {"groupValue": "long", "doclist": {"numFound": 4, "docs": []}}]}}});
    assert!(check("group", &swapped, &solr).is_ok());
    assert!(check("group", &json!({"grouped": grouped(3, 3)}), &solr).is_err());
    let mut wrong_total = solr.clone();
    wrong_total["grouped"]["len_bucket"]["matches"] = json!(7);
    assert!(check("group", &wrong_total, &solr).is_err());
}

#[test]
fn highlight_needs_a_snippet_for_every_returned_doc() {
    let solr = with(
        docs(2, &["a", "b"]),
        "highlighting",
        json!({"a": {"body": ["x"]}, "b": {"body": ["y"]}}),
    );
    let full = solr.clone();
    let missing = with(
        docs(2, &["a", "b"]),
        "highlighting",
        json!({"a": {"body": ["x"]}, "b": {}}),
    );
    assert!(check("hl", &full, &solr).is_ok());
    assert!(check("hl", &missing, &solr).is_err());
}

#[test]
fn unknown_mode_is_an_error_not_a_pass() {
    assert!(check("nope", &docs(1, &["a"]), &docs(1, &["a"])).is_err());
}
