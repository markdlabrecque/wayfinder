//! Issue #332 — `{!geofilt}`/`{!bbox}` filters and `geodist()` in `sort`.
//!
//! The wire-compatibility evidence (Solr's circle-vs-square distinction, the
//! `sort=geodist() asc` ordering) lives in the fixture comparison suite against
//! captured `geo_*` fixtures. This file covers the two behaviours no fixture
//! exercises: a `{!geofilt}` excluding a document that has *no* `location`
//! point at all, and a local-param block overriding the request params
//! (`{!geofilt sfield=loc ...}` beats `sfield=...` on the request). Both are
//! correctness guards — break either and a real geo query silently returns the
//! wrong set — so each is shaped to stay red under the mutation that breaks it.

// The `dead_code` allow for the shared helpers is an inner attribute inside
// `tests/common/mod.rs`; do not add a second one here (clippy rejects it under
// `-D warnings`).
mod common;

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;

use common::{app_with_schema, get, post_docs};

/// A `content` core with a single stored `location` field `loc`.
const LOC_SCHEMA_TOML: &str = r#"
[core]
name = "content"
unique_key = "id"
default_field = "id"

[[fields]]
name = "id"
type = "string"
stored = true
required = true
fast = true

[[fields]]
name = "loc"
type = "location"
stored = true
"#;

async fn loc_app(docs: &Value) -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = app_with_schema(dir.path(), LOC_SCHEMA_TOML).expect("loc app must build");
    let (status, body) = post_docs(&app, docs).await;
    assert_eq!(status, StatusCode::OK, "indexing must succeed, got {body}");
    (app, dir)
}

fn ids(body: &Value) -> Vec<String> {
    body["response"]["docs"]
        .as_array()
        .expect("docs array")
        .iter()
        .map(|d| d["id"].as_str().expect("id").to_string())
        .collect()
}

/// A `{!geofilt}` excludes a document with no `location` point: g8 has no
/// `loc`, so it must never match even when its (missing) point would read back
/// as the origin `(0,0)` -- exactly where `pt` sits. Drop the scorer's `exists`
/// gate and g8 falls inside the `d`-circle around `(0,0)`, so this stays red
/// under that mutation.
#[tokio::test]
async fn geofilt_excludes_a_doc_with_no_location_point() {
    let docs = json!([
        {"id":"g1","loc":"0,0"},   // the origin: distance 0, inside the circle
        {"id":"g8"},               // no point: must be excluded
    ]);
    let (app, _dir) = loc_app(&docs).await;
    let (status, body) = get(
        &app,
        "select?q=*:*&fq=%7B%21geofilt%7D&sfield=loc&pt=0,0&d=10&fl=id&sort=id%20asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&body), vec!["g1".to_string()], "g8 has no point");
}

/// The same missing-point rule holds for `{!bbox}`: a doc with no point is not
/// inside any rectangle, so g8 stays out.
#[tokio::test]
async fn bbox_excludes_a_doc_with_no_location_point() {
    let docs = json!([
        {"id":"g1","loc":"0,0"},
        {"id":"g8"},
    ]);
    let (app, _dir) = loc_app(&docs).await;
    let (status, body) = get(
        &app,
        "select?q=*:*&fq=%7B%21bbox%7D&sfield=loc&pt=0,0&d=10&fl=id&sort=id%20asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&body), vec!["g1".to_string()], "g8 has no point");
}

/// A `{!geofilt}` block's own local params override the request params (Solr's
/// QParser fallback): with request `pt=0,0` but block `pt=40,-74 d=130`, the
/// filter uses the block's origin. g6 `(41,-73)` is ~140 km from `(40,-74)` --
/// outside the circle -- so a filter that read the request's `pt=0,0` (which
/// would put every doc thousands of km away and match nothing) would return an
/// empty set instead of the grid.
#[tokio::test]
async fn geofilt_local_params_override_the_request_params() {
    let docs = json!([
        {"id":"g1","loc":"40.0,-74.0"},
        {"id":"g6","loc":"41.0,-73.0"},
        {"id":"g2","loc":"41.0,-74.0"},
    ]);
    let (app, _dir) = loc_app(&docs).await;
    // Request-level `pt=0,0&d=1` would match nothing; the block overrides both.
    let (status, body) = get(
        &app,
        "select?q=*:*&fq=%7B%21geofilt%20sfield%3Dloc%20pt%3D40,-74%20d%3D130%7D&sfield=loc&pt=0,0&d=1&fl=id&sort=id%20asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // g1 (0 km) and g2 (~111 km) are inside the 130-km circle; g6 (~140 km) is not.
    assert_eq!(ids(&body), vec!["g1".to_string(), "g2".to_string()]);
}
