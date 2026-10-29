//! Issue #477: `text_en` must not miss pages that Solr's classic Porter chain
//! finds. Snowball English keeps the final `s` of `-us` words and stems
//! `-ally` words differently from Porter, so in the 20,000-page benchmark
//! `argue` missed `Argus` (edismax: 1064 vs 1087) and `institutionalized`
//! missed `institutionally` (deep_page: 56 vs 63). Missing a page that exists
//! is the worse direction, so these are fixed rather than waived.

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
async fn searching_argue_finds_a_doc_that_only_says_argus() {
    let docs = json!([{"id": "d1", "body": "the hundred-eyed Argus kept watch"}]);
    assert_eq!(hits(docs, "argue").await, 1, "`argue` must match `Argus`");
}

#[tokio::test]
async fn searching_institutionalized_finds_a_doc_that_only_says_institutionally() {
    let docs = json!([{"id": "d1", "body": "treated institutionally by the state"}]);
    assert_eq!(
        hits(docs, "institutionalized").await,
        1,
        "`institutionalized` must match `institutionally`"
    );
}

#[tokio::test]
async fn searching_institutionally_finds_a_doc_that_only_says_institutionalized() {
    let docs = json!([{"id": "d1", "body": "many were institutionalized early"}]);
    assert_eq!(
        hits(docs, "institutionally").await,
        1,
        "`institutionally` must match `institutionalized`"
    );
}
