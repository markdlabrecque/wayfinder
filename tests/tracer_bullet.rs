//! Tracer-bullet integration tests (PRD §7).
//!
//! End to end, in-process (tower `oneshot`, no network, no spawned binary):
//! build `wayfinder::app` from the three-field TOML schema, index the same
//! 5-doc corpus `solr-ref/capture.sh` used against real Solr, then assert the
//! JSON responses against the captured fixtures in `solr-ref/responses/`.
//!
//! Fixtures are compared modulo `QTime` and `_version_`/`_root_` (Wayfinder's
//! explicit default-`fl` decision — see `common::normalize_envelope`).
//! `params` key order is not normalised separately because JSON objects
//! already compare order-independently.

mod common;

use common::{assert_matches_fixture, get, indexed_app};
use serde_json::Value;

#[tokio::test]
async fn ping_reports_ok() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "admin/ping?wt=json").await;

    assert_eq!(status, 200);
    // Match solr-ref/responses/ping.json's essential shape (task spec: "match
    // ping.json shape (status: OK)"). Solr's ping envelope also carries
    // health-check-internal params (rid, echoParams, a synthetic q/df) that
    // are an artifact of Solr's ping handler running a real query
    // internally, not part of the wire contract Wayfinder needs to
    // reproduce, so those are not asserted here.
    assert_eq!(body["status"], "OK");
    assert_eq!(body["responseHeader"]["status"], 0);
}

#[tokio::test]
async fn select_all_returns_all_docs_with_default_fl_and_no_internal_fields() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=*:*&rows=10&wt=json").await;

    assert_eq!(status, 200);
    assert_matches_fixture(body.clone(), "select_all");

    // Belt-and-braces on the default-fl decision itself (PRD §7 / findings
    // fact 9): no internal fields leak into an unrestricted result, even
    // though the fixture (real Solr) has them.
    for doc in body["response"]["docs"].as_array().unwrap() {
        assert!(
            doc.get("_version_").is_none(),
            "doc must not include _version_"
        );
        assert!(doc.get("_root_").is_none(), "doc must not include _root_");
    }
}

#[tokio::test]
async fn select_with_fq_filters_results() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=*:*&fq=category:animals&wt=json").await;

    assert_eq!(status, 200);
    assert_matches_fixture(body, "select_fq");
}

#[tokio::test]
async fn select_pagination_start_and_rows() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=*:*&rows=2&start=3&wt=json").await;

    assert_eq!(status, 200);
    assert_matches_fixture(body, "select_paged");
}

#[tokio::test]
async fn select_rows_zero_returns_empty_docs_but_correct_num_found() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=*:*&rows=0&wt=json").await;

    assert_eq!(status, 200);
    assert_matches_fixture(body.clone(), "select_rows_zero");
    assert_eq!(body["response"]["numFound"], 5);
    assert_eq!(body["response"]["docs"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn select_pagination_past_the_end_returns_empty_docs() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=*:*&rows=10&start=999&wt=json").await;

    assert_eq!(status, 200);
    assert_matches_fixture(body.clone(), "select_past_end");
    assert_eq!(body["response"]["start"], 999);
    assert_eq!(body["response"]["numFound"], 5);
    assert_eq!(body["response"]["docs"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn select_zero_results_has_correct_envelope() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=zzzznope&df=body&wt=json").await;

    assert_eq!(status, 200);
    assert_matches_fixture(body.clone(), "select_zero");
    assert_eq!(body["response"]["numFound"], 0);
    assert_eq!(body["response"]["numFoundExact"], true);
    assert_eq!(body["response"]["docs"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn facet_on_multi_valued_field_matches_flat_alternating_array_shape() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    assert_matches_fixture(body.clone(), "facet_basic");

    // Findings facts 1 & 3: facet_fields is a flat alternating [name, count,
    // ...] array (not an object) by default, and facet_counts always carries
    // all five sub-objects, empty when unused.
    let facet_counts = &body["facet_counts"];
    assert!(
        facet_counts.is_object(),
        "facet_counts must be present when facet=true"
    );
    for key in [
        "facet_queries",
        "facet_fields",
        "facet_ranges",
        "facet_intervals",
        "facet_heatmaps",
    ] {
        assert!(
            facet_counts.get(key).is_some(),
            "facet_counts.{key} must always be present"
        );
    }
    let category = &facet_counts["facet_fields"]["category"];
    assert!(
        category.is_array(),
        "facet_fields.category must be a flat array, not an object"
    );
    let flat: Vec<Value> = category.as_array().unwrap().clone();
    assert_eq!(
        flat,
        vec![
            Value::from("animals"),
            Value::from(2),
            Value::from("classic"),
            Value::from(2),
            Value::from("garden"),
            Value::from(1),
            Value::from("misc"),
            Value::from(1),
        ]
    );
}

#[tokio::test]
async fn select_without_facet_param_has_no_facet_counts_key() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=*:*&rows=10&wt=json").await;

    assert_eq!(status, 200);
    // Findings fact 4 / PRD §2: facet_counts is absent entirely (not
    // present-and-empty) when facet was not requested.
    assert!(
        body.as_object().unwrap().get("facet_counts").is_none(),
        "facet_counts must be absent when facet was not requested"
    );
}

#[tokio::test]
async fn select_unknown_fl_field_is_silently_dropped() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=*:*&rows=1&fl=id,nosuchfield&wt=json").await;

    assert_eq!(status, 200);
    assert_matches_fixture(body.clone(), "select_fl_missing");

    let doc = &body["response"]["docs"][0];
    assert_eq!(doc["id"], "doc1");
    assert!(
        doc.get("nosuchfield").is_none(),
        "unknown fl field must be silently dropped, not erroring"
    );
    assert!(
        doc.get("body").is_none(),
        "fl restricts to the requested fields only"
    );
}

#[tokio::test]
async fn select_unknown_param_is_ignored_but_echoed() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=*:*&notaparam=1&wt=json").await;

    // Findings fact 7 / PRD open question 3: unknown params are silently
    // ignored (status: 0 / HTTP 200), not rejected.
    assert_eq!(status, 200);
    assert_matches_fixture(body.clone(), "err_unknown_param");
    assert_eq!(body["responseHeader"]["status"], 0);
    assert_eq!(body["responseHeader"]["params"]["notaparam"], "1");
    assert_eq!(body["response"]["numFound"], 5);
}

#[tokio::test]
async fn select_doc_with_no_value_for_optional_multi_valued_field_omits_key() {
    let (app, _dir) = indexed_app().await;

    // doc5 has no `category` in the corpus.
    let (status, body) = get(&app, "select?q=id:doc5&wt=json").await;

    assert_eq!(status, 200);
    assert_matches_fixture(body.clone(), "select_doc_no_field");

    let doc = &body["response"]["docs"][0];
    assert_eq!(doc["id"], "doc5");
    assert!(
        doc.get("category").is_none(),
        "a field with no value must be omitted, not present as null/empty array"
    );
}
