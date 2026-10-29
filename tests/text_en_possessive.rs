//! Issue #478: Solr's `text_en` runs `EnglishPossessiveFilter`, which strips a
//! trailing `'s` (or `’s`). Without it, `airline` missed a page that only says
//! "airline's" and `parkinson` missed "Parkinson's" in the 20,000-page
//! benchmark. Missing a page that exists is the worse direction.

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
async fn searching_airline_finds_a_doc_that_only_says_airlines_with_apostrophe_s() {
    let docs = json!([{"id": "d1", "body": "the luxury airline's campaign"}]);
    assert_eq!(
        hits(docs, "airline").await,
        1,
        "`airline` must match `airline's`"
    );
}

#[tokio::test]
async fn searching_parkinson_finds_a_capitalised_possessive() {
    let docs = json!([{"id": "d1", "body": "Parkinson's disease research"}]);
    assert_eq!(
        hits(docs, "parkinson").await,
        1,
        "`parkinson` must match `Parkinson's`"
    );
}

#[tokio::test]
async fn searching_airline_finds_a_curly_apostrophe_possessive() {
    let docs = json!([{"id": "d1", "body": "the luxury airline\u{2019}s campaign"}]);
    assert_eq!(
        hits(docs, "airline").await,
        1,
        "`airline` must match `airline’s`"
    );
}

#[tokio::test]
async fn searching_the_possessive_form_finds_the_plain_word() {
    let docs = json!([{"id": "d1", "body": "a new airline launched"}]);
    let q = "%22airline%27s%22";
    assert_eq!(hits(docs, q).await, 1, "`airline's` must match `airline`");
}
