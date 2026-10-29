//! Issue #479: `text_en` matches Solr's stock chain. It no longer folds accents
//! and no longer catenates hyphenated words, so these searches agree with Solr.

mod common;

use axum::http::StatusCode;
use common::{app_with_schema, get, post_docs};
use serde_json::{Value, json};
use tempfile::TempDir;

const SCHEMA: &str = r#"
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

[[dynamic_fields]]
pattern = "tm_X3b_en_*"
type = "text_en"
stored = true
multi_valued = true
"#;

async fn app() -> (axum::Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = app_with_schema(dir.path(), SCHEMA).expect("schema must load");
    let (status, body) = post_docs(
        &app,
        &json!([
            {"id": "perce", "body": "Percé Rock", "tm_X3b_en_title": ["Percé Rock"]},
            {"id": "offseason", "body": "off-season rates", "tm_X3b_en_title": ["off-season rates"]}
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "indexing corpus: {body}");
    (app, dir)
}

async fn count(app: &axum::Router, query: &str) -> u64 {
    let (status, body) = get(app, query).await;
    assert_eq!(status, StatusCode::OK, "/{query}: {body}");
    body.pointer("/response/numFound")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("numFound must be numeric: {body}"))
}

#[test]
fn v9_marker_on_text_en_refuses_startup_requiring_reindex() {
    let dir = TempDir::new().expect("temp dir");
    let app = app_with_schema(dir.path(), SCHEMA).expect("fresh index must build");
    drop(app);
    std::fs::write(
        dir.path().join("data/wayfinder-analyzer-contract"),
        "text_presets_porter_stem_v9",
    )
    .expect("write v9 marker");

    let err = app_with_schema(dir.path(), SCHEMA)
        .expect_err("a v9 text_en index holds folded and catenated terms and must be reindexed");
    assert!(
        format!("{err:#}").to_lowercase().contains("reindex"),
        "refusal must require reindexing, got: {err:#}"
    );
}

#[tokio::test]
async fn unaccented_query_does_not_match_accented_text() {
    let (app, _dir) = app().await;
    assert_eq!(count(&app, "select?q=body:perce").await, 0);
    assert_eq!(count(&app, "select?q=tm_X3b_en_title:perce").await, 0);
    // The accented spelling still matches itself, as in Solr.
    assert_eq!(count(&app, "select?q=body:perc%C3%A9").await, 1);
    assert_eq!(count(&app, "select?q=tm_X3b_en_title:perc%C3%A9").await, 1);
}

#[tokio::test]
async fn joined_query_does_not_match_hyphenated_text() {
    let (app, _dir) = app().await;
    assert_eq!(count(&app, "select?q=body:offseason").await, 0);
    assert_eq!(count(&app, "select?q=tm_X3b_en_title:offseason").await, 0);
    // The parts of the hyphenated word still match, as in Solr.
    assert_eq!(count(&app, "select?q=body:season").await, 1);
    assert_eq!(count(&app, "select?q=body:%22off+season%22").await, 1);
}
