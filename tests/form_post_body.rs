//! `application/x-www-form-urlencoded` POST bodies on the `any_method` search
//! routes (issue #350).
//!
//! Solarium's `postbigrequest` plugin (loaded by `search_api_solr` whenever
//! `http_method === 'AUTO'`, the default) silently turns any GET whose query
//! string exceeds `maxquerystringlength` (default 1024) into a POST with
//! `Content-Type: application/x-www-form-urlencoded` and the query string
//! moved into the raw body. Finding 189 pins the merge model these tests
//! encode: query string + form body are merged with NO precedence -- query
//! params first, body params appended, exactly like repeated query params --
//! so a single-valued read takes the FIRST value and `echo` renders both.
//!
//! The fixture comparison suite exercises the captured `form_post_*` fixtures
//! (`solr-ref/manifest(-errors).tsv`); these tests pin the behaviour directly
//! and cover the edges the fixtures do not (strict_params validation of body
//! params, the content-type gate, `/mlt` and `/terms` intake).

mod common;

use axum::http::StatusCode;
use common::{indexed_app, request_full_with_content_type};
use serde_json::Value;

/// The core bug (issue #350): a `q` arriving only in a form-encoded POST body
/// must be answered, not silently dropped to an empty result set. `q=lazy`
/// over the shared `body` default field matches doc1 and doc2.
#[tokio::test]
async fn select_reads_q_from_a_form_encoded_post_body() {
    let (app, _dir) = indexed_app().await;
    let (status, body) =
        request_full_with_content_type(&app, "POST", "content/select?fl=id", Some("q=lazy"), FORM)
            .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let mut ids: Vec<&str> = body["response"]["docs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap())
        .collect();
    ids.sort();
    assert_eq!(ids, vec!["doc1", "doc2"]);
}

/// Finding 189: a param in BOTH query string and body merges like a repeated
/// query param -- `echo` lists both, and a single-valued read (`rows`) takes
/// the FIRST (query-string) value. `rows=2` in the query wins over `rows=10`
/// in the body, so 2 docs come back, not all 5.
#[tokio::test]
async fn select_merges_query_and_form_body_first_value_wins() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = request_full_with_content_type(
        &app,
        "POST",
        "content/select?q=*:*&rows=2&fl=id",
        Some("rows=10"),
        FORM,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["response"]["numFound"], 5,
        "the merge filters nothing; all 5 docs match"
    );
    assert_eq!(
        body["response"]["docs"].as_array().unwrap().len(),
        2,
        "rows=2 (the query-string value) wins over rows=10 (body)"
    );
    assert_eq!(
        body["responseHeader"]["params"]["rows"],
        Value::Array(vec![Value::String("2".into()), Value::String("10".into())]),
        "echo must list both, query value first -- the merged form"
    );
}

/// The content-type gate: a form-SHAPED body sent as `application/json` is
/// NOT parsed into params. This is what keeps a JSON body on `/select` from
/// being misread, and is also why the harness sends the captured `postbigrequest`
/// rows with the form content-type. `q` is absent from the query string, so an
/// un-parsed body leaves the request with no `q` -- matches nothing (200/empty).
#[tokio::test]
async fn select_does_not_parse_a_json_content_typed_body_as_params() {
    let (app, _dir) = indexed_app().await;
    // Form-shaped body, but the wrong content-type.
    let (status, body) = request_full_with_content_type(
        &app,
        "POST",
        "content/select?fl=id",
        Some("q=lazy"),
        "application/json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["response"]["numFound"], 0,
        "the body was not parsed, so q is absent and nothing matches"
    );
}

/// A `; charset=...` suffix (and case variation) must not defeat the form
/// match -- media types are parameterised and case-insensitive.
#[tokio::test]
async fn select_parses_the_body_under_a_charset_suffixed_form_content_type() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = request_full_with_content_type(
        &app,
        "POST",
        "content/select?fl=id",
        Some("q=lazy"),
        "Application/x-www-form-urlencoded; charset=utf-8",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["response"]["numFound"], 2,
        "charset suffix and case must not block the form merge"
    );
}

/// `strict_params` must validate body params identically to query-string
/// params, or a POSTed unknown param becomes a silent pass where the GET 400s.
/// Issue #350's own scope calls this out.
#[tokio::test]
async fn strict_params_rejects_an_unknown_form_body_param() {
    let (app, _dir) = strict_content_app().await;
    // `q=*:*` is in the query string (known); the unknown param is in the body.
    let (status, body) = request_full_with_content_type(
        &app,
        "POST",
        "content/select?q=*:*&fl=id",
        Some("notaparam=1"),
        FORM,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "strict_params must 400 an unknown param arriving in the body, got: {body}"
    );
    assert_eq!(body["error"]["code"], 400);
}

/// `/mlt` shares the form-body intake (issue #350 scope). `q` in the body must
/// resolve the seed document just as it would in the query string.
#[tokio::test]
async fn mlt_reads_q_from_a_form_encoded_post_body() {
    let (app, _dir) = indexed_app().await;
    // `q=id:doc1` in the body; mlt.fl over the default field set. A parsed `q`
    // resolves a seed (doc1), so `match` carries one doc; an un-parsed `q`
    // leaves `match.numFound: 0`.
    let (status, body) = request_full_with_content_type(
        &app,
        "POST",
        "content/mlt?fl=id",
        Some("q=id:doc1&mlt.fl=body"),
        FORM,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["match"]["numFound"], 1,
        "the body's q resolved the seed doc; an un-parsed body would be 0"
    );
    assert_eq!(body["match"]["docs"][0]["id"], "doc1");
}

/// `/terms` shares the form-body intake (issue #350 scope). `terms.fl` in the
/// body must drive the term listing exactly as in the query string.
#[tokio::test]
async fn terms_reads_terms_fl_from_a_form_encoded_post_body() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = request_full_with_content_type(
        &app,
        "POST",
        "content/terms?terms=true&wt=json",
        Some("terms.fl=category"),
        FORM,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        body["terms"].get("category").is_some_and(|v| !v.is_null()),
        "the body's terms.fl must drive the listing; an un-parsed body leaves \
         no field key. got terms={}",
        body["terms"]
    );
}

// --- helpers ----------------------------------------------------------------

/// The one content-type Solarium's `postbigrequest` sends.
const FORM: &str = "application/x-www-form-urlencoded";

/// A `content`-core app built with `strict_params = true`, so unknown params
/// 400. Mirrors `tests/admin_info_system.rs`'s `build_app_with_config` pattern.
async fn strict_content_app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, common::SCHEMA_TOML).expect("write schema.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let config_path = dir.path().join("wayfinder.toml");
    std::fs::write(&config_path, "strict_params = true\n").expect("write wayfinder.toml");
    let app =
        wayfinder::app_with_config(&schema_path, &data_dir, &config_path).expect("app must build");
    // Index the shared corpus so `q=lazy` etc. resolve exactly as `indexed_app`.
    common::post_docs(&app, &common::corpus()).await;
    (app, dir)
}
