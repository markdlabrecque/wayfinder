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

#[tokio::test]
async fn configured_auth_protects_update_but_not_health_checks() {
    let (app, _dir) = authenticated_app();
    let update_body = json!([{"id": "protected", "body": "must require credentials"}]).to_string();

    let (status, headers, bytes) = send(
        &app,
        "POST",
        "/solr/content/update?commit=true",
        None,
        Body::from(update_body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        headers
            .get(header::WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok()),
        Some("Basic realm=\"solr\"")
    );
    let body: Value =
        serde_json::from_slice(&bytes).expect("401 must be a JSON Solr error envelope");
    assert_eq!(body.pointer("/error/code"), Some(&json!(401)));
    assert_eq!(body.pointer("/responseHeader/status"), Some(&json!(401)));
    assert!(
        body["error"]["msg"]
            .as_str()
            .is_some_and(|msg| !msg.is_empty())
    );

    let (status, _headers, _bytes) = send(
        &app,
        "POST",
        "/solr/content/update?commit=true",
        Some(BASIC_CREDENTIALS),
        Body::from(update_body),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "correct Basic credentials must allow /update"
    );

    for ping in ["/solr/content/admin/ping", "/ui/ping"] {
        let (status, _headers, _bytes) = send(&app, "GET", ping, None, Body::empty()).await;
        assert_eq!(status, StatusCode::OK, "{ping} must remain unauthenticated");
    }
}
