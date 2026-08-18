//! Request body size limit (issue #64).
//!
//! Axum's `Bytes`/`Json` extractors enforce a bare 2MB request-body cap by
//! default (`axum::extract::DefaultBodyLimit`) unless a `Router` explicitly
//! overrides it. `src/lib.rs`'s `build()` now layers
//! `DefaultBodyLimit::max(config.resources.max_body_size)` onto the router,
//! and `src/config.rs`'s `ServerConfig` carries that knob under
//! `[resources] max_body_size` (bytes), defaulting to `10_000_000` — see the
//! doc comment on `Resources::max_body_size` and finding 79 in
//! `solr-ref/FINDINGS.md` for why that default's value is what it is.
//!
//! These tests cover the shipped behaviour:
//! - `oversized_bulk_update_succeeds_under_the_default_limit` shows the
//!   raised default (10MB) accepting a body that would have tripped axum's
//!   bare 2MB cap.
//! - `configuring_a_larger_max_body_size_allows_the_oversized_update` proves
//!   the `[resources] max_body_size` knob is actually consulted, not just
//!   present: it configures a limit (4MB) that is neither axum's bare
//!   default nor Wayfinder's own default (10MB), and checks a body sized to
//!   clear only that configured value.
//! - `a_body_over_the_configured_limit_is_still_rejected` is the mutation
//!   check for the cap doing its one job: with the same 4MB configured
//!   limit, a body just over it must still 413, so a hypothetical
//!   `DefaultBodyLimit::disable()` (or the config value being ignored
//!   entirely) would be caught here even though the "does raising the limit
//!   work" tests above would stay green under that regression.

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use tempfile::TempDir;
use tower::ServiceExt;

use common::{CORE, get};

/// Builds the app (optionally with a `wayfinder.toml` config) against the
/// tracer-bullet schema, without indexing anything. Mirrors
/// `tests/server_config.rs::build_app_with_config`, duplicated locally since
/// each integration test file is its own crate.
fn build_app_with_config(config: Option<&str>) -> anyhow::Result<(Router, TempDir)> {
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
    Ok((app, dir))
}

/// Builds a single add-doc body whose serialized JSON is `padding_bytes` of
/// filler over the tracer-bullet schema's `body` field (text_en, no
/// fast-field/uniqueness constraints), plus a small fixed amount of JSON
/// envelope overhead. Sizes are chosen per-caller relative to axum's bare 2MB
/// `DefaultBodyLimit`, Wayfinder's 10MB default (`Resources::max_body_size`),
/// and whatever a test configures — see each test's own comment.
fn update_body_with_padding(padding_bytes: usize) -> String {
    let padding = "x".repeat(padding_bytes);
    json!([{"id": "doc-oversized", "body": padding, "category": ["bulk"]}]).to_string()
}

/// ~3MB: comfortably over axum's bare 2MB default, comfortably under
/// Wayfinder's 10MB default and every configured limit these tests use.
fn oversized_update_body() -> String {
    update_body_with_padding(3 * 1024 * 1024)
}

async fn post_update(app: &Router, body: String) -> StatusCode {
    let req = Request::builder()
        .method("POST")
        .uri(format!("/wayfinder/{CORE}/update?commit=true"))
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let resp = app
        .clone()
        .oneshot(req)
        .await
        .expect("request must not fail at the transport level");
    resp.status()
}

#[tokio::test]
async fn oversized_bulk_update_succeeds_under_the_default_limit() {
    let (app, _dir) = build_app_with_config(None).expect("app must build with default config");
    let body = oversized_update_body();
    assert!(
        body.len() > 2 * 1024 * 1024,
        "fixture body must actually exceed axum's 2MB default to test anything"
    );

    // The acceptance bar (issue #64): a bulk-update payload over axum's bare
    // 2MB default must be indexable under Wayfinder's own (larger) default,
    // not rejected.
    let status = post_update(&app, body).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a >2MB bulk update must succeed under Wayfinder's raised default body limit"
    );

    let (status, _body) = get(&app, "select?q=id:doc-oversized&wt=json").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn configuring_a_larger_max_body_size_allows_the_oversized_update() {
    // Deliberately NOT equal to `Resources::max_body_size`'s 10MB default
    // (see `src/config.rs`): if this value matched the default, a body that
    // fits under both would pass whether or not `build()` actually reads the
    // config value at all. 4MB, paired with the ~3MB body below, is smaller
    // than the default but still clears axum's bare 2MB cap, so this proves
    // the knob raises the limit at all — `a_body_over_the_configured_limit_is_still_rejected`
    // below is what proves the *configured* value, specifically, is what's
    // enforced.
    let config_toml = "[resources]\nmax_body_size = 4_000_000\n";
    let build_result = build_app_with_config(Some(config_toml));

    let (app, _dir) = build_result.unwrap_or_else(|e| {
        panic!(
            "expected `resources.max_body_size` to be a recognised, working config knob \
             that raises the request body limit, but building the app failed: {e:#}"
        )
    });

    let body = oversized_update_body();
    assert!(
        body.len() < 4_000_000,
        "fixture body must fit under this test's configured 4MB limit"
    );
    let status = post_update(&app, body).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "with resources.max_body_size raised past the payload size, the oversized update must succeed"
    );
}

#[tokio::test]
async fn a_body_over_the_configured_limit_is_still_rejected() {
    // Mutation-test companion to the success-path tests above (repo
    // convention: code whose whole value is failing correctly gets
    // mutation-tested). Same 4MB configured limit as
    // `configuring_a_larger_max_body_size_allows_the_oversized_update`, but
    // this body (~5MB) clears the *configured* limit while still sitting
    // comfortably under Wayfinder's 10MB default. That gap is deliberate: it
    // catches a `build()` that hardcodes `DefaultBodyLimit::max(10_000_000)`
    // (ignoring the config value) or calls `DefaultBodyLimit::disable()`
    // outright — both would wrongly return 200 here instead of 413. Without
    // this test, the only assertions in this file are "raising/configuring
    // the limit lets a big body through", which stay green under either of
    // those regressions since 5MB < 10MB either way.
    let config_toml = "[resources]\nmax_body_size = 4_000_000\n";
    let (app, _dir) = build_app_with_config(Some(config_toml))
        .expect("resources.max_body_size = 4_000_000 must be a valid config");

    let body = update_body_with_padding(5 * 1024 * 1024);
    assert!(
        body.len() > 4_000_000,
        "fixture body must exceed this test's configured 4MB limit to test anything"
    );
    assert!(
        body.len() < 10_000_000,
        "fixture body must stay under Wayfinder's 10MB default, or a hardcoded-default \
         regression would not be distinguishable from correctly enforcing the config"
    );

    let status = post_update(&app, body).await;
    assert_eq!(
        status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "a body over the configured resources.max_body_size must still be rejected"
    );
}
