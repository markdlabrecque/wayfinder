//! Phase 4 word-delimiter and autocomplete contract (issue #389).
//!
//! The assertions are deliberately search-quality contracts, not Solr fixture
//! comparisons: both index and query analysis must produce useful parts and a
//! catenated spelling, including for Search API dynamic text fields.

mod common;

use axum::http::StatusCode;
use common::{app_with_schema, get, post_docs};
use serde_json::{Value, json};
use tempfile::TempDir;

const DELIMITER_SCHEMA: &str = r#"
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
name = "twm_suggest"
type = "text_en"
stored = true
multi_valued = true

[[dynamic_fields]]
pattern = "tm_X3b_en_*"
type = "text_en"
stored = true
multi_valued = true
"#;

async fn delimiter_app() -> (axum::Router, TempDir) {
    let dir = TempDir::new().expect("create temp dir");
    let app = app_with_schema(dir.path(), DELIMITER_SCHEMA).expect("delimiter schema must load");
    let (status, body) = post_docs(
        &app,
        &json!([
            {"id": "static-joined", "body": "WiFi2000"},
            {"id": "static-parts", "body": "WiFi 2000"},
            {"id": "dynamic-joined", "tm_X3b_en_code": ["wifi_router"]},
            {"id": "dynamic-parts", "tm_X3b_en_code": ["wifi router"]},
            {"id": "suggest-delimiter", "twm_suggest": ["foo_bar"]}
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "index delimiter corpus: {body}");
    (app, dir)
}

fn num_found(body: &Value) -> u64 {
    body.pointer("/response/numFound")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("response.numFound must be numeric: {body}"))
}

async fn assert_field_query_count(app: &axum::Router, field: &str, term: &str, expected: u64) {
    let query = format!("select?q={field}%3A{term}&rows=0");
    let (status, body) = get(app, &query).await;
    assert_eq!(status, StatusCode::OK, "/{query}: {body}");
    assert_eq!(num_found(&body), expected, "/{query}: {body}");
}

#[tokio::test]
async fn word_delimiter_parts_and_catenations_match_symmetrically_for_static_and_dynamic_text() {
    let (app, _dir) = delimiter_app().await;

    // Part lookup proves index-side splitting: the joined spelling must be
    // found. Catenated lookup against the whitespace-separated spelling proves
    // query-side splitting/catenation. Together, each pair rules out solving
    // this only on one side of the index/query boundary.
    assert_field_query_count(&app, "body", "wifi", 2).await;
    assert_field_query_count(&app, "body", "wifi2000", 2).await;
    assert_field_query_count(&app, "tm_X3b_en_code", "wifi", 2).await;
    assert_field_query_count(&app, "tm_X3b_en_code", "wifirouter", 2).await;
}
