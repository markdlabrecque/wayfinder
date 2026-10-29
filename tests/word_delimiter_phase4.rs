//! Phase 4 autocomplete contract (issue #389).
//!
//! Issue #479 removed the word delimiter from `text_en` search analysis so it
//! matches Solr (see `tests/text_en_solr_parity.rs`). The suggest dictionary
//! keeps its own delimiter handling, which these assertions still cover.

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
"#;

async fn delimiter_app() -> (axum::Router, TempDir) {
    let dir = TempDir::new().expect("create temp dir");
    let app = app_with_schema(dir.path(), DELIMITER_SCHEMA).expect("delimiter schema must load");
    let (status, body) = post_docs(
        &app,
        &json!([
            {"id": "suggest-delimiter", "twm_suggest": ["foo_bar"]},
            {"id": "emoji-compound", "body": "😀-foo raven", "twm_suggest": ["😀-foo"]}
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "index delimiter corpus: {body}");
    (app, dir)
}

/// A delimiter typed after a word is still an in-progress token, not a reason
/// to turn autocomplete into an exact lookup.
#[tokio::test]
async fn suggest_keeps_mid_delimiter_input_as_a_prefix() {
    let (app, _dir) = delimiter_app().await;
    let (status, body) = get(&app, "suggest?suggest.dictionary=en&suggest.q=foo_&wt=json").await;
    assert_eq!(status, StatusCode::OK, "/suggest must succeed: {body}");
    assert_eq!(
        body.pointer("/suggest/en/foo_/numFound")
            .and_then(Value::as_u64),
        Some(2),
        "`foo_` must remain a useful prefix for `foo_bar` after adding `😀-foo`: {body}"
    );
}

#[tokio::test]
async fn emoji_linked_suggestion_input_keeps_its_graph_parts() {
    let (app, _dir) = delimiter_app().await;
    let (status, body) = get(
        &app,
        "suggest?suggest.dictionary=en&suggest.q=%F0%9F%98%80-foo&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "/suggest must succeed: {body}");
    assert_eq!(
        body.pointer("/suggest/en/😀-foo/numFound")
            .and_then(Value::as_u64),
        Some(1),
        "emoji-linked suggestion input must retain its graph parts: {body}"
    );
}
