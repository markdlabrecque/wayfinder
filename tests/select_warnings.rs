//! Warnings for accepted parameters that would otherwise silently change results (issue #232).
//!
//! No committed Solr fixture sends `bf` or a function-query `boost`, so these
//! assertions define Wayfinder's deliberate honesty extension rather than a
//! fixture comparison. Numeric `boost` and `bq` are implemented and must not
//! receive this warning.

// The `dead_code` allow for partially-used shared helpers is an inner attribute
// inside `tests/common/mod.rs`; repeating it here is a clippy error under
// `-D warnings`.
mod common;

use axum::http::StatusCode;
use serde_json::{Value, json};

use common::{get, indexed_app};

fn ignored_function_query_warning(param: &str) -> Value {
    json!(format!(
        "Ignoring function-query parameter `{param}`: function queries are not implemented."
    ))
}

#[tokio::test]
async fn bf_and_function_query_boost_warn_that_their_function_is_ignored() {
    let (app, _dir) = indexed_app().await;

    for (name, request, param) in [
        // `bf` is warned on by presence, not by recognizing a particular
        // function-query spelling.
        (
            "bf",
            "select?q=lazy&defType=edismax&qf=body&bf=1&wt=json",
            "bf",
        ),
        (
            "function-query boost",
            "select?q=lazy&defType=edismax&qf=body&boost=recip(rating,1,1000,1000)&wt=json",
            "boost",
        ),
    ] {
        let (status, body) = get(&app, request).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{name} request must succeed: {body}"
        );
        assert_eq!(
            body.pointer("/responseHeader/warnings"),
            Some(&json!([ignored_function_query_warning(param)])),
            "{name} must say exactly why it was accepted but ignored: {body}"
        );
    }
}

#[tokio::test]
async fn numeric_boost_and_bq_do_not_claim_to_be_ignored() {
    let (app, _dir) = indexed_app().await;

    for (name, request) in [
        (
            "numeric boost",
            "select?q=lazy&defType=edismax&qf=body&boost=2&wt=json",
        ),
        (
            "bq",
            "select?q=lazy&defType=edismax&qf=body&bq=body:dog^2&wt=json",
        ),
    ] {
        let (status, body) = get(&app, request).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{name} request must succeed: {body}"
        );
        assert!(
            body.pointer("/responseHeader/warnings").is_none(),
            "{name} is implemented and must not receive an ignored-function warning: {body}"
        );
    }
}

#[tokio::test]
async fn ignored_parameter_warning_precedes_and_coexists_with_facet_warning() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=views&bf=recip(rating,1,1000,1000)&wt=json",
    )
    .await;

    assert_eq!(status, StatusCode::OK, "request must succeed: {body}");
    assert_eq!(
        body.pointer("/responseHeader/warnings"),
        Some(&json!([
            ignored_function_query_warning("bf"),
            "Raising facet.mincount from 0 to 1, because field views is Points-based."
        ])),
        "the ignored-parameter warning must not replace or reorder the established facet warning: {body}"
    );
}
