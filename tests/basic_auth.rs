//! Issue #229 — optional HTTP Basic authentication.
//!
//! This initial tracer slice configures auth, protects the destructive update
//! endpoint, admits the configured credentials, and leaves both health checks
//! public. Existing integration tests already cover the no-config open path.

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

const AUTH_CONFIG: &str = r#"
[auth]
username = "operator"
password = "secret"
"#;
const BASIC_CREDENTIALS: &str = "Basic b3BlcmF0b3I6c2VjcmV0";

fn authenticated_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("create temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, common::SCHEMA_TOML).expect("write schema.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let config_path = dir.path().join("wayfinder.toml");
    std::fs::write(&config_path, AUTH_CONFIG).expect("write wayfinder.toml");

    let app = wayfinder::app_with_config(&schema_path, &data_dir, &config_path)
        .expect("non-empty [auth] config must build an authenticated app");
    (app, dir)
}

async fn send(
    app: &Router,
    method: &str,
    path: &str,
    authorization: Option<&str>,
    body: Body,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(value) = authorization {
        request = request.header(header::AUTHORIZATION, value);
    }
    let response = app
        .clone()
        .oneshot(request.body(body).expect("build request"))
        .await
        .expect("request must not fail at the transport level");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("response body must be readable")
        .to_bytes()
        .to_vec();
    (status, headers, bytes)
}

async fn assert_unauthorized(app: &Router, method: &str, path: &str, authorization: Option<&str>) {
    let (status, headers, bytes) = send(app, method, path, authorization, Body::empty()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{path} must require auth");
    assert_eq!(
        headers
            .get(header::WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok()),
        Some("Basic realm=\"wayfinder\"")
    );
    let body: Value =
        serde_json::from_slice(&bytes).expect("401 must be a JSON Solr error envelope");
    assert_eq!(body.pointer("/error/code"), Some(&json!(401)));
    assert_eq!(body.pointer("/responseHeader/status"), Some(&json!(401)));
}

#[tokio::test]
async fn ping_exemption_is_limited_to_the_configured_core_and_ui_ping() {
    let (app, _dir) = authenticated_app();

    for ping in ["/wayfinder/content/admin/ping", "/ui/ping"] {
        let (status, _headers, _bytes) = send(&app, "GET", ping, None, Body::empty()).await;
        assert_eq!(status, StatusCode::OK, "{ping} must remain unauthenticated");
    }
    for path in [
        "/wayfinder/other/admin/ping",
        "/wayfinder/content/admin/ping/extra",
    ] {
        assert_unauthorized(&app, "GET", path, None).await;
    }
}

#[tokio::test]
async fn auth_protects_select_and_admin_ui() {
    let (app, _dir) = authenticated_app();
    for path in ["/wayfinder/content/select?q=*:*", "/ui"] {
        assert_unauthorized(&app, "GET", path, None).await;
    }
}

#[tokio::test]
async fn basic_auth_requires_well_formed_matching_credentials() {
    let (app, _dir) = authenticated_app();
    let update = "/wayfinder/content/update?commit=true";

    for authorization in [
        "Basic b3BlcmF0b3I6d3Jvbmc=",
        "Basic !not-base64!",
        "Bearer b3BlcmF0b3I6c2VjcmV0",
    ] {
        assert_unauthorized(&app, "POST", update, Some(authorization)).await;
    }

    let lowercase_basic = BASIC_CREDENTIALS.replacen("Basic", "basic", 1);
    let (status, _headers, _bytes) = send(
        &app,
        "POST",
        update,
        Some(&lowercase_basic),
        Body::from(json!([{"id": "protected", "body": "authenticated"}]).to_string()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a lowercase Basic scheme with matching credentials must authenticate"
    );
}
