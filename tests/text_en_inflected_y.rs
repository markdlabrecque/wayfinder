//! Issue #443: `text_en` stems every inflected form of a word ending in a
//! vowel + `y` to the same term, as Solr's Porter chain does. Before v8 the
//! y-to-i rewrite only saw a word-final `y`, so `day` indexed as `dai` while
//! `days` indexed as `day`, and searching one form missed the other.

mod common;

use axum::http::StatusCode;
use common::{app_with_schema, get, post_docs};
use serde_json::{Value, json};
use tempfile::TempDir;

const SCHEMA_TOML: &str = r#"
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
"#;

fn num_found(body: &Value) -> u64 {
    body["response"]["numFound"]
        .as_u64()
        .unwrap_or_else(|| panic!("response.numFound must be a number, got {body}"))
}

async fn hits(docs: Value, q: &str) -> u64 {
    let dir = TempDir::new().expect("temp dir");
    let app = app_with_schema(dir.path(), SCHEMA_TOML).expect("schema loads");
    let (status, body) = post_docs(&app, &docs).await;
    assert_eq!(status, StatusCode::OK, "indexing must succeed: {body}");
    let (status, body) = get(&app, &format!("select?q={q}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    num_found(&body)
}

#[tokio::test]
async fn searching_day_finds_a_doc_that_only_says_days() {
    let docs = json!([{"id": "d1", "body": "three days later"}]);
    assert_eq!(hits(docs, "day").await, 1, "`day` must match `days`");
}

#[tokio::test]
async fn searching_days_finds_a_doc_that_only_says_day() {
    let docs = json!([{"id": "d1", "body": "one day later"}]);
    assert_eq!(hits(docs, "days").await, 1, "`days` must match `day`");
}

#[tokio::test]
async fn searching_play_finds_played_and_playing() {
    let docs = json!([
        {"id": "d1", "body": "she played well"},
        {"id": "d2", "body": "they are playing now"}
    ]);
    assert_eq!(
        hits(docs, "play").await,
        2,
        "`play` must match `played` and `playing`"
    );
}
