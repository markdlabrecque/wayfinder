//! Issue #11 — Solr error shapes.
//!
//! Every expected value here comes from a committed fixture in
//! `solr-ref/responses/` (`docs/solr-ref-findings.md` finding 10). Per the task
//! spec the comparison contract is deliberately narrow:
//!
//! - `error.code` and the HTTP status must match the fixture exactly;
//! - `responseHeader.status` mirrors them;
//! - `error.metadata` matches Solr's *shape* (flat alternating array, keys
//!   `error-class` / `root-error-class`) — the values are Java class names in
//!   Solr and Wayfinder-honest strings here, so they are not compared;
//!   - `error.msg` is free text: asserted non-empty, never verbatim.
//!
//! Whether `responseHeader` (and `params` inside it) is present at all is part
//! of the shape, because Solr varies it per endpoint — see the three flavours
//! in `err_bad_syntax.json`, `err_update_bad_json.json`, `err_update_put.json`.

// `tests/common` is shared with the tracer-bullet suite; this file uses only
// part of it. The allow that covers that now lives inside `tests/common/mod.rs`
// as an inner attribute (added by #10/#1), so it is not repeated here — clippy
// rejects the duplicate under `-D warnings`.
mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

use common::{CORE, fixture, get, indexed_app};

/// Issues an arbitrary method/path/body against `app`. `common::get` only does
/// GET, and these tests need POST/PUT/DELETE. Kept local rather than added to
/// `tests/common/mod.rs` to avoid colliding with the concurrent #1 harness
/// branch, which owns that file.
async fn request(
    app: &Router,
    method: &str,
    path_and_query: &str,
    body: Option<&str>,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(format!("/solr/{CORE}/{path_and_query}"))
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let resp = app.clone().oneshot(req).await.expect("transport-level ok");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("error responses must be valid JSON")
    };
    (status, body)
}

/// Asserts an error response matches the named fixture on the contract above.
fn assert_error_shape(status: StatusCode, body: &Value, fixture_name: &str) {
    let expected = fixture(fixture_name);
    let want_code = expected["error"]["code"]
        .as_i64()
        .unwrap_or_else(|| panic!("fixture {fixture_name} has no error.code"));

    assert_eq!(
        status.as_u16() as i64,
        want_code,
        "HTTP status must equal the fixture's error.code ({fixture_name})"
    );
    assert_eq!(
        body["error"]["code"].as_i64(),
        Some(want_code),
        "error.code must match the fixture ({fixture_name})"
    );

    // responseHeader presence, and params-inside-it presence, are part of the
    // shape: Solr omits both on an unsupported method, and omits params on
    // /update errors.
    let want_header = expected.get("responseHeader");
    match want_header {
        Some(want_header) => {
            let got_header = body
                .get("responseHeader")
                .unwrap_or_else(|| panic!("{fixture_name}: responseHeader must be present"));
            assert_eq!(
                got_header["status"].as_i64(),
                Some(want_code),
                "responseHeader.status must mirror error.code ({fixture_name})"
            );
            assert_eq!(
                got_header.get("params").is_some(),
                want_header.get("params").is_some(),
                "responseHeader.params presence must match the fixture ({fixture_name})"
            );
        }
        None => assert!(
            body.get("responseHeader").is_none(),
            "{fixture_name}: Solr omits responseHeader here, so Wayfinder must too"
        ),
    }

    // metadata: flat alternating array, same keys, values not compared.
    let want_meta = expected["error"]["metadata"]
        .as_array()
        .unwrap_or_else(|| panic!("fixture {fixture_name} has no error.metadata array"));
    let got_meta = body["error"]["metadata"]
        .as_array()
        .unwrap_or_else(|| panic!("{fixture_name}: error.metadata must be a flat array"));
    assert_eq!(
        got_meta.len(),
        want_meta.len(),
        "error.metadata length must match the fixture ({fixture_name})"
    );
    for (i, want) in want_meta.iter().enumerate().step_by(2) {
        assert_eq!(
            got_meta[i].as_str(),
            want.as_str(),
            "error.metadata key at index {i} must match the fixture ({fixture_name})"
        );
        assert!(
            got_meta[i + 1].as_str().is_some_and(|v| !v.is_empty()),
            "error.metadata value at index {} must be a non-empty string ({fixture_name})",
            i + 1
        );
    }

    assert!(
        body["error"]["msg"].as_str().is_some_and(|m| !m.is_empty()),
        "error.msg must be a non-empty string, but is never compared verbatim ({fixture_name})"
    );
}

#[tokio::test]
async fn bad_query_syntax_matches_solr_error_shape() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=*:*&fq=category:[unclosed&wt=json").await;
    assert_error_shape(status, &body, "err_bad_syntax");
}

#[tokio::test]
async fn unknown_field_in_q_matches_solr_error_shape() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=nosuchfield:x&wt=json").await;
    assert_error_shape(status, &body, "err_unknown_field");
}

#[tokio::test]
async fn unknown_field_in_fq_is_a_400_error_envelope() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=*:*&fq=nosuchfield:x&wt=json").await;
    // Same class of error as an unknown field in `q`, so the same fixture shape.
    assert_error_shape(status, &body, "err_unknown_field");
}

#[tokio::test]
async fn sort_on_a_non_fast_field_matches_solr_error_shape() {
    let (app, _dir) = indexed_app().await;
    // `body` is text_en, not fast — Solr 400s rather than silently falling back
    // (finding 11). Only the error shape is in scope here; actually ordering
    // results is issue #2.
    let (status, body) = get(&app, "select?q=*:*&sort=body+desc&wt=json").await;
    assert_error_shape(status, &body, "err_bad_sort");
}

#[tokio::test]
async fn sort_on_an_unknown_field_is_an_error() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=*:*&sort=nosuchfield+desc&wt=json").await;
    assert_error_shape(status, &body, "err_bad_sort");
}

#[tokio::test]
async fn missing_q_returns_zero_results_like_solr() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?wt=json").await;
    let expected = fixture("err_missing_q");

    // Solr answers 200 with an empty result set — it does *not* default to
    // `*:*` (tracer-bullet follow-up 2 resolved against the fixture).
    assert_eq!(status, StatusCode::OK, "missing q is not an error in Solr");
    assert_eq!(
        body["responseHeader"]["status"],
        expected["responseHeader"]["status"]
    );
    assert_eq!(
        body["response"]["numFound"],
        expected["response"]["numFound"]
    );
    assert_eq!(body["response"]["start"], expected["response"]["start"]);
    assert_eq!(
        body["response"]["numFoundExact"],
        expected["response"]["numFoundExact"]
    );
    assert_eq!(
        body["response"]["docs"].as_array().map(Vec::len),
        Some(0),
        "missing q must match no documents"
    );
    assert!(
        body.get("error").is_none(),
        "missing q must not produce an error block"
    );
}

#[tokio::test]
async fn unknown_core_is_404_with_a_json_error_envelope() {
    let (app, _dir) = indexed_app().await;
    let req = Request::builder()
        .method("GET")
        .uri("/solr/nosuchcore/select?q=*:*&wt=json")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.expect("transport-level ok");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let body: Value = serde_json::from_slice(&bytes).expect("must be JSON");

    // Documented divergence: `err_missing_core.json` shows Solr serving an HTML
    // easter-egg page here, not JSON. Wayfinder matches the status code and
    // returns its normal JSON error envelope, which is what clients parse.
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"].as_i64(), Some(404));
    assert_eq!(body["responseHeader"]["status"].as_i64(), Some(404));
    assert!(body["error"]["msg"].as_str().is_some_and(|m| !m.is_empty()));
}

#[tokio::test]
async fn update_with_malformed_json_matches_solr_error_shape() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = request(
        &app,
        "POST",
        "update?commit=true&wt=json",
        Some("{not json"),
    )
    .await;
    // Note the fixture's responseHeader carries no `params` — /update does not
    // echo them (tracer-bullet follow-up 3, confirmed by capture).
    assert_error_shape(status, &body, "err_update_bad_json");
}

#[tokio::test]
async fn update_with_a_non_array_body_is_a_400_error_envelope() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = request(&app, "POST", "update?wt=json", Some("{\"id\":\"x\"}")).await;
    assert_error_shape(status, &body, "err_update_bad_json");
}

#[tokio::test]
async fn update_with_an_unsupported_method_matches_solr_error_shape() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = request(&app, "PUT", "update?wt=json", Some("[]")).await;
    // `err_update_put.json` has *no* responseHeader at all — just `error`.
    assert_error_shape(status, &body, "err_update_put");
}

#[tokio::test]
async fn select_serves_non_get_methods_like_solr() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = request(&app, "DELETE", "select?q=*:*&wt=json", None).await;
    // `err_select_delete.json`: Solr's request handlers are method-agnostic —
    // DELETE /select is served as a normal query, not a 405.
    assert_eq!(
        status,
        StatusCode::OK,
        "Solr answers DELETE /select normally"
    );
    assert_eq!(body["response"]["numFound"].as_u64(), Some(5));
    assert!(body.get("error").is_none());
}

#[tokio::test]
async fn unknown_request_params_are_still_ignored() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=*:*&notaparam=1&wt=json").await;
    let expected = fixture("err_unknown_param");
    // Guard: validating `sort` must not turn into strict param validation
    // (finding 8 — Solr ignores unknown params).
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["responseHeader"]["status"],
        expected["responseHeader"]["status"]
    );
    assert!(body.get("error").is_none());
}
