//! Server config TOML + tuning knobs (issue #12, PRD §6).
//!
//! Every knob is optional with a sane default and a missing file means all
//! defaults, so the two-argument `wayfinder::app` stays the defaults path and
//! `app_with_config` is the only new entry point. Reuses the tracer-bullet
//! schema and corpus from `tests/common` so behaviour changes here are visible
//! against the same 5 documents the reference fixtures were captured from.

// The dead-code allow for partially-used shared helpers is an inner attribute
// inside `tests/common/mod.rs`; repeating it here is a clippy error under
// `-D warnings`.
mod common;

use std::path::{Path, PathBuf};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

use common::{CORE, corpus, get};

/// Writes `schema.toml` plus an optional `wayfinder.toml` into a temp dir,
/// builds the app through `app_with_config` (or `app` when `config` is
/// `None`), indexes `common::corpus()`, and returns the router, the temp dir
/// guard, and the data dir (so tests can inspect Tantivy's `meta.json`).
async fn indexed_app_with_config(config: Option<&str>) -> (Router, TempDir, PathBuf) {
    let (app, dir, data_dir) = build_app_with_config(config).expect("app must build");
    let req = Request::builder()
        .method("POST")
        .uri(format!("/solr/{CORE}/update?commit=true"))
        .header("content-type", "application/json")
        .body(Body::from(corpus().to_string()))
        .unwrap();
    let resp = app
        .clone()
        .oneshot(req)
        .await
        .expect("update must not fail");
    assert_eq!(resp.status(), StatusCode::OK, "indexing must succeed");
    (app, dir, data_dir)
}

/// Builds the app without indexing, surfacing the `anyhow::Result` so tests
/// can assert on config-rejection paths.
fn build_app_with_config(config: Option<&str>) -> anyhow::Result<(Router, TempDir, PathBuf)> {
    let dir = TempDir::new().expect("create temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, common::SCHEMA_TOML).expect("write schema.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");

    let app = match config {
        Some(toml) => {
            let config_path = dir.path().join("wayfinder.toml");
            std::fs::write(&config_path, toml).expect("write wayfinder.toml");
            wayfinder::app_with_config(&schema_path, &data_dir, &config_path)?
        }
        None => wayfinder::app(&schema_path, &data_dir)?,
    };
    Ok((app, dir, data_dir))
}

/// Builds the app against a config path that deliberately does not exist.
fn build_app_with_missing_config() -> anyhow::Result<(Router, TempDir, PathBuf)> {
    let dir = TempDir::new().expect("create temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, common::SCHEMA_TOML).expect("write schema.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let absent = dir.path().join("no-such-wayfinder.toml");
    let app = wayfinder::app_with_config(&schema_path, &data_dir, &absent)?;
    Ok((app, dir, data_dir))
}

fn docs(body: &Value) -> &Vec<Value> {
    body.pointer("/response/docs")
        .and_then(|d| d.as_array())
        .expect("response.docs must be an array")
}

fn meta_settings(data_dir: &Path) -> Value {
    let raw = std::fs::read_to_string(data_dir.join("meta.json")).expect("read meta.json");
    let meta: Value = serde_json::from_str(&raw).expect("meta.json must be valid JSON");
    meta.get("index_settings")
        .cloned()
        .expect("meta.json must carry index_settings")
}

// --- defaults --------------------------------------------------------------

#[tokio::test]
async fn missing_config_file_means_all_defaults() {
    let (app, _dir, _data) = build_app_with_missing_config().expect("missing config must be OK");
    let req = Request::builder()
        .method("POST")
        .uri(format!("/solr/{CORE}/update?commit=true"))
        .header("content-type", "application/json")
        .body(Body::from(corpus().to_string()))
        .unwrap();
    let resp = app
        .clone()
        .oneshot(req)
        .await
        .expect("update must not fail");
    assert_eq!(resp.status(), StatusCode::OK);

    // Default `strict_params = false` (Solr ignores unknown params, finding 8)
    // and the default `rows` of 10 is under the default `rows_limit`.
    let (status, body) = get(&app, "select?q=*:*&notaparam=1&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(docs(&body).len(), 5, "all 5 docs returned with defaults");
}

#[tokio::test]
async fn empty_config_file_means_all_defaults() {
    let (app, _dir, _data) = indexed_app_with_config(Some("")).await;
    let (status, body) = get(&app, "select?q=*:*&notaparam=1&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(docs(&body).len(), 5);
}

// --- unknown keys are an operator error ------------------------------------

#[tokio::test]
async fn unknown_top_level_key_is_rejected_by_name() {
    let err = build_app_with_config(Some("strictparams = true\n"))
        .expect_err("a config typo must not silently no-op");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("strictparams"),
        "error must name the offending key, got: {msg}"
    );
}

#[tokio::test]
async fn unknown_key_inside_a_section_is_rejected_by_name() {
    let err = build_app_with_config(Some("[indexing]\nwriter_heep = 33554432\n"))
        .expect_err("a config typo must not silently no-op");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("writer_heep"),
        "error must name the offending key, got: {msg}"
    );
}

#[tokio::test]
async fn unknown_section_is_rejected_by_name() {
    let err = build_app_with_config(Some("[indexingg]\nwriter_heap = 33554432\n"))
        .expect_err("a config typo must not silently no-op");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("indexingg"),
        "error must name the offending section, got: {msg}"
    );
}

// --- strict_params (PRD open question 3) -----------------------------------

#[tokio::test]
async fn strict_params_rejects_unknown_param_with_solr_error_envelope() {
    let (app, _dir, _data) = indexed_app_with_config(Some("strict_params = true\n")).await;
    let (status, body) = get(&app, "select?q=*:*&notaparam=1&wt=json").await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body.pointer("/error/code").and_then(|c| c.as_i64()),
        Some(400),
        "error.code must mirror the HTTP status (finding 10)"
    );
    assert_eq!(
        body.pointer("/responseHeader/status")
            .and_then(|c| c.as_i64()),
        Some(400),
        "responseHeader.status must mirror the HTTP status (finding 10)"
    );
    let msg = body
        .pointer("/error/msg")
        .and_then(|m| m.as_str())
        .expect("error.msg must be present");
    assert!(
        msg.contains("notaparam"),
        "error.msg must name the unknown param, got: {msg}"
    );
}

#[tokio::test]
async fn strict_params_allows_every_implemented_param() {
    let (app, _dir, _data) = indexed_app_with_config(Some("strict_params = true\n")).await;
    let (status, body) = get(
        &app,
        "select?q=lazy&df=body&fq=category:garden&fl=id,body&rows=2&start=0\
         &facet=true&facet.field=category&sort=score+desc&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "implemented params must pass strict mode, got body: {body}"
    );
}

#[tokio::test]
async fn strict_params_still_accepts_the_commit_param_on_update() {
    let (app, _dir, _data) = indexed_app_with_config(Some("strict_params = true\n")).await;
    // indexed_app_with_config already POSTed /update?commit=true; a second
    // commit-only update must also pass strict mode.
    let req = Request::builder()
        .method("POST")
        .uri(format!("/solr/{CORE}/update?commit=true"))
        .header("content-type", "application/json")
        .body(Body::from("[]"))
        .unwrap();
    let resp = app
        .clone()
        .oneshot(req)
        .await
        .expect("update must not fail");
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn strict_params_rejects_unknown_param_on_update() {
    let (app, _dir, _data) = indexed_app_with_config(Some("strict_params = true\n")).await;
    let req = Request::builder()
        .method("POST")
        .uri(format!("/solr/{CORE}/update?commit=true&notaparam=1"))
        .header("content-type", "application/json")
        .body(Body::from("[]"))
        .unwrap();
    let resp = app
        .clone()
        .oneshot(req)
        .await
        .expect("update must not fail");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        body.pointer("/error/code").and_then(|c| c.as_i64()),
        Some(400)
    );
}

// --- query caps ------------------------------------------------------------

#[tokio::test]
async fn rows_limit_clamps_a_larger_requested_rows() {
    let (app, _dir, _data) = indexed_app_with_config(Some("[query]\nrows_limit = 2\n")).await;
    let (status, body) = get(&app, "select?q=*:*&rows=10&wt=json").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(docs(&body).len(), 2, "rows must be clamped to rows_limit");
    assert_eq!(
        body.pointer("/response/numFound").and_then(|n| n.as_i64()),
        Some(5),
        "clamping the page must not change numFound"
    );
}

#[tokio::test]
async fn rows_below_the_limit_is_untouched() {
    let (app, _dir, _data) = indexed_app_with_config(Some("[query]\nrows_limit = 100\n")).await;
    let (status, body) = get(&app, "select?q=*:*&rows=3&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(docs(&body).len(), 3);
}

// --- indexing / resource knobs actually reach Tantivy ----------------------

#[tokio::test]
async fn writer_heap_below_tantivys_minimum_is_a_startup_error() {
    // Proves the configured heap really reaches `IndexWriter` rather than
    // being parsed and dropped: Tantivy rejects an arena this small.
    let err = build_app_with_config(Some("[indexing]\nwriter_heap = 1024\n"))
        .expect_err("an impossible writer_heap must fail at startup, not silently");
    let msg = format!("{err:#}");
    assert!(
        msg.to_lowercase().contains("memory") || msg.to_lowercase().contains("arena"),
        "error should explain the heap is too small, got: {msg}"
    );
}

#[tokio::test]
async fn multiple_writer_threads_still_index_and_search() {
    let (app, _dir, _data) = indexed_app_with_config(Some(
        "[indexing]\nwriter_threads = 2\nwriter_heap = 64000000\n",
    ))
    .await;
    let (status, body) = get(&app, "select?q=*:*&rows=10&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.pointer("/response/numFound").and_then(|n| n.as_i64()),
        Some(5)
    );
}

#[tokio::test]
async fn no_merge_policy_is_accepted() {
    let (app, _dir, _data) =
        indexed_app_with_config(Some("[indexing]\nmerge_policy = \"no_merge\"\n")).await;
    let (status, body) = get(&app, "select?q=*:*&rows=10&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.pointer("/response/numFound").and_then(|n| n.as_i64()),
        Some(5)
    );
}

#[tokio::test]
async fn unknown_merge_policy_is_rejected() {
    let err = build_app_with_config(Some("[indexing]\nmerge_policy = \"sometimes\"\n"))
        .expect_err("an unknown merge policy must be rejected");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("sometimes"),
        "error must name the bad value, got: {msg}"
    );
}

#[tokio::test]
async fn doc_store_knobs_reach_the_tantivy_index_settings() {
    let (_app, _dir, data_dir) = indexed_app_with_config(Some(
        "[resources]\ndoc_store_compression = \"none\"\ndoc_store_blocksize = 8192\n",
    ))
    .await;
    let settings = meta_settings(&data_dir);
    assert_eq!(
        settings
            .get("docstore_compression")
            .and_then(|c| c.as_str()),
        Some("none"),
        "configured compressor must reach Tantivy's index settings"
    );
    assert_eq!(
        settings.get("docstore_blocksize").and_then(|b| b.as_i64()),
        Some(8192),
        "configured blocksize must reach Tantivy's index settings"
    );
}

#[tokio::test]
async fn unknown_doc_store_compression_is_rejected() {
    let err = build_app_with_config(Some("[resources]\ndoc_store_compression = \"snappy\"\n"))
        .expect_err("an unsupported compressor must be rejected");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("snappy"),
        "error must name the bad value, got: {msg}"
    );
}

// --- knobs parsed for later consumers -------------------------------------

#[tokio::test]
async fn commit_and_budget_knobs_parse_and_are_exposed() {
    // `autocommit_*` is consumed by issue #9 and `time_allowed` /
    // `facet_limit_max` by the query/facet work; this pins that they parse and
    // are readable now, so those branches have something to consume.
    let config = "\
[commit]
autocommit_max_docs = 1000
autocommit_max_time = 60000

[query]
time_allowed = 250
facet_limit_max = 500

[resources]
searcher_pool_size = 4
";
    let parsed = wayfinder::ServerConfig::parse(config).expect("config must parse");
    assert_eq!(parsed.commit.autocommit_max_docs, Some(1000));
    assert_eq!(parsed.commit.autocommit_max_time, Some(60000));
    assert_eq!(parsed.query.time_allowed, Some(250));
    assert_eq!(parsed.query.facet_limit_max, 500);
    assert_eq!(parsed.resources.searcher_pool_size, 4);

    // And the whole thing still builds a working app.
    let (app, _dir, _data) = indexed_app_with_config(Some(config)).await;
    let (status, _body) = get(&app, "select?q=*:*&wt=json").await;
    assert_eq!(status, StatusCode::OK);
}
