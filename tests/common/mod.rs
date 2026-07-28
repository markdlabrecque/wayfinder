//! Shared test helpers for the tracer-bullet integration suite.
//!
//! Builds a `wayfinder::app` in-process (no network, no spawned binary) against
//! the three-field tracer-bullet schema (PRD §7), indexes the same 5-doc
//! corpus used to capture `solr-ref/responses/*.json`, and provides thin
//! request/response helpers plus fixture-comparison normalisation.

use std::path::Path;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

pub const CORE: &str = "content";

/// The tracer-bullet schema per PRD §7: `id` (string, stored, unique key),
/// `body` (text_en, stored), `category` (string, fast, multi_valued, stored).
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
