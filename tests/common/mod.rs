//! Shared test helpers for the tracer-bullet integration suite.
//!
//! Builds a `wayfinder::app` in-process (no network, no spawned binary) against
//! the shared tracer-bullet schema (PRD §7), indexes the same 5-doc
//! corpus used to capture `solr-ref/responses/*.json`, and provides thin
//! request/response helpers plus fixture-comparison normalisation.
//!
//! Each integration test file is its own crate that pulls this module in via
//! `mod common;`, and no single test file calls every helper here —
//! `differential.rs` never calls `normalize_envelope`/`assert_matches_fixture`,
//! `tracer_bullet.rs` never calls anything in `diff`, and neither of those calls
//! `app_with_schema`/`post_docs`. `dead_code` is suppressed at the module root
//! rather than per-item because which helpers are "unused" depends on which
//! binary is compiling this module, not on the code itself.
#![allow(dead_code)]

use std::path::Path;

pub mod diff;
pub mod key_order;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

pub const CORE: &str = "content";

/// The tracer-bullet schema per PRD §7: `id` (string, fast, stored, unique key),
/// `body` (text_en, stored), `category` (string, fast, multi_valued, stored),
/// and `views` (int, fast, not stored).
///
/// `id` is `fast = true` to mirror the reference Solr, not as a convenience:
/// Solr's `_default` configset gives its `string` type `docValues="true"`, so
/// real Solr sorts on `id` happily — `select_sort.json` is exactly that. A
/// mirror schema without `fast` on `id` would make `sort=id desc` a 400 here
/// while the fixture is a 200, which is a divergence in the *test* schema, not
/// in Wayfinder. Added by issue #2 (sort); `body` stays non-fast so
/// `err_bad_sort.json`'s 400 still reproduces.
/// Issue #3 relies on the same property for the other reason: `facet.field=id`
/// works on real Solr for exactly that configset reason
/// (`facet_multi_field.json`). `fast` changes no stored output, so no existing
/// fixture moves.
pub const SCHEMA_TOML: &str = r#"
[core]
name = "content"
unique_key = "id"
default_field = "body"

[[fields]]
name = "id"
type = "string"
stored = true
required = true
fast = true

[[fields]]
name = "body"
type = "text_en"
stored = true

[[fields]]
name = "category"
type = "string"
stored = true
fast = true
multi_valued = true

# Unstored and absent from the corpus, so this declaration cannot change any
# tracer-bullet response fixture; it exists only to distinguish numeric schema
# resolution from a local-params response label (issue #150).
[[fields]]
name = "views"
type = "int"
fast = true
"#;

/// The exact 5-doc corpus indexed by `solr-ref/capture.sh`, so fixture JSON
/// under `solr-ref/responses/` is ground truth for the same corpus here.
pub fn corpus() -> Value {
    json!([
        {"id":"doc1","body":"the quick brown fox jumps over the lazy dog","category":["animals","classic"]},
        {"id":"doc2","body":"a lazy afternoon in the garden","category":["garden"]},
        {"id":"doc3","body":"quick thinking saves the day","category":["misc","classic"]},
        {"id":"doc4","body":"dogs and cats living together","category":["animals"]},
        {"id":"doc5","body":"nothing much here at all"}
    ])
}

/// Builds a fresh Wayfinder app against a temp schema file + temp data dir,
/// indexes `corpus()`, and commits. Returns the router plus the `TempDir`
/// guard — keep it alive for the lifetime of the test (dropping it deletes
/// the schema file and data dir).
pub async fn indexed_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("create temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, SCHEMA_TOML).expect("write schema.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");

    let app = wayfinder::app(&schema_path, &data_dir).expect("wayfinder::app must build");

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
        .expect("update request must not fail at the transport level");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "indexing the reference corpus must succeed"
    );

    (app, dir)
}

/// Builds an app against an arbitrary schema TOML in a caller-owned directory,
/// indexing nothing. Returns the router; `dir` must outlive it.
///
/// Separate from `indexed_app()` because the schema-layer tests (issue #10) need
/// their own schemas and their own corpora, and need to reopen the *same* data
/// dir with a changed schema to exercise the startup compatibility check.
pub fn app_with_schema(dir: &Path, schema_toml: &str) -> anyhow::Result<Router> {
    let schema_path = dir.join("schema.toml");
    std::fs::write(&schema_path, schema_toml).expect("write schema.toml");
    let data_dir = dir.join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    wayfinder::app(&schema_path, &data_dir)
}

/// `POST /solr/<core>/update?commit=true` with `docs` as the body.
pub async fn post_docs(app: &Router, docs: &Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri(format!("/solr/{CORE}/update?commit=true"))
        .header("content-type", "application/json")
        .body(Body::from(docs.to_string()))
        .unwrap();
    let resp = app
        .clone()
        .oneshot(req)
        .await
        .expect("update request must not fail at the transport level");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("response body must be readable")
        .to_bytes();
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response body must be valid JSON")
    };
    (status, body)
}

/// Issues `GET /solr/<core>/<path_and_query>` against `app` and returns the
/// HTTP status plus parsed JSON body.
pub async fn get(app: &Router, path_and_query: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("GET")
        .uri(format!("/solr/{CORE}/{path_and_query}"))
        .body(Body::empty())
        .unwrap();
    let resp = app
        .clone()
        .oneshot(req)
        .await
        .expect("select/ping request must not fail at the transport level");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("response body must be readable")
        .to_bytes();
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response body must be valid JSON")
    };
    (status, body)
}

/// Issues an arbitrary method/full-path request against `app` and returns
/// the HTTP status plus parsed JSON body (or `Value::Null` for an empty
/// body). "Full-path" means `path_and_query` is everything after `/solr/`,
/// including the core segment — e.g. `content/update?commit=true&wt=json`
/// or `nosuchcore/select?q=*:*&wt=json`. This is the variant the
/// `manifest-errors.tsv` runner (`tests/differential.rs`) needs, since its
/// rows name their own core, sometimes one Wayfinder does not have at all.
///
/// Consolidated from `tests/error_shapes.rs`'s local `request()` (issue #31
/// follow-up — deferred there only to avoid colliding with the
/// then-concurrent #1 branch, which owns this file).
pub async fn request_full(
    app: &Router,
    method: &str,
    path_and_query: &str,
    body: Option<&str>,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(format!("/solr/{path_and_query}"))
        .header("content-type", "application/json")
        .body(match body {
            Some(b) => Body::from(b.to_string()),
            None => Body::empty(),
        })
        .unwrap();
    let resp = app
        .clone()
        .oneshot(req)
        .await
        .expect("request must not fail at the transport level");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("response body must be readable")
        .to_bytes();
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response body must be valid JSON")
    };
    (status, body)
}

/// Thin core-relative wrapper over `request_full`, for callers (like
/// `tests/error_shapes.rs`) that only ever address `CORE`. Mirrors `get()`'s
/// relationship to a full-path request, but for arbitrary methods/bodies.
pub async fn request(
    app: &Router,
    method: &str,
    core_relative_path_and_query: &str,
    body: Option<&str>,
) -> (StatusCode, Value) {
    request_full(
        app,
        method,
        &format!("{CORE}/{core_relative_path_and_query}"),
        body,
    )
    .await
}

/// Issues `GET <path>` against `app` and returns the HTTP status, response
/// headers, and raw UTF-8 text body — for routes outside the JSON `/solr/*`
/// wire API (e.g. the admin UI, issue #94), where the response is HTML, not
/// JSON, so `get()`'s `serde_json::from_slice` would fail on a valid
/// response. `path` is the full path (no `/solr/` prefix implied), unlike
/// `get()`.
pub async fn get_text(app: &Router, path: &str) -> (StatusCode, axum::http::HeaderMap, String) {
    let req = Request::builder()
        .method("GET")
        .uri(path)
        .body(Body::empty())
        .unwrap();
    let resp = app
        .clone()
        .oneshot(req)
        .await
        .expect("request must not fail at the transport level");
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("response body must be readable")
        .to_bytes();
    let text = String::from_utf8(bytes.to_vec()).expect("response body must be valid utf-8");
    (status, headers, text)
}

/// Loads a captured reference fixture from `solr-ref/responses/<name>.json`.
pub fn fixture(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("solr-ref/responses")
        .join(format!("{name}.json"));
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse fixture {}: {e}", path.display()))
}

/// Normalises a response envelope (actual or expected-from-fixture) before
/// comparison:
/// - drops `responseHeader.QTime` (always variable, per findings fact — not
///   asserted per the task spec).
/// - drops `_version_` / `_root_` from each doc in `response.docs` (Wayfinder
///   has no such internal fields; explicit default-`fl` decision, findings
///   fact 9 / PRD §7).
///
/// `params` key order is not normalised because `serde_json::Value::Object`
/// compares as a map regardless of key order already.
pub fn normalize_envelope(mut v: Value) -> Value {
    if let Some(header) = v.get_mut("responseHeader").and_then(|h| h.as_object_mut()) {
        header.remove("QTime");
    }
    if let Some(docs) = v
        .pointer_mut("/response/docs")
        .and_then(|d| d.as_array_mut())
    {
        for doc in docs.iter_mut() {
            if let Some(obj) = doc.as_object_mut() {
                obj.remove("_version_");
                obj.remove("_root_");
            }
        }
    }
    v
}

/// Asserts `actual` equals the named fixture, modulo `normalize_envelope`.
pub fn assert_matches_fixture(actual: Value, fixture_name: &str) {
    let expected = normalize_envelope(fixture(fixture_name));
    let actual = normalize_envelope(actual);
    assert_eq!(
        actual, expected,
        "response for fixture `{fixture_name}` did not match (modulo QTime / _version_ / _root_)"
    );
}
