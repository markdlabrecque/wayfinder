//! Phase 4 word-delimiter and autocomplete contract (issue #389).
//!
//! Issue #479 removed the word delimiter from `text_en` and the dynamic text
//! catch-all so they match Solr (see `tests/text_en_solr_parity.rs`).
//! `text_general` and the suggest dictionary keep their delimiter handling,
//! which these assertions cover.

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

[[fields]]
name = "general"
type = "text_general"
stored = true
"#;

async fn delimiter_app() -> (axum::Router, TempDir) {
    let dir = TempDir::new().expect("create temp dir");
    let app = app_with_schema(dir.path(), DELIMITER_SCHEMA).expect("delimiter schema must load");
    let (status, body) = post_docs(
        &app,
        &json!([
            {"id": "static-compound", "general": "SKU-42 next WiFi2000"},
            {"id": "static-independent", "general": "day boy"},
            {"id": "suggest-delimiter", "twm_suggest": ["foo_bar"]},
            {"id": "emoji-compound", "body": "😀-foo raven", "general": "😀-foo raven", "twm_suggest": ["😀-foo"]},
            {"id": "combining-compound", "general": "\u{301}-bar zebra"}
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

async fn field_terms(app: &axum::Router, field: &str) -> Vec<String> {
    let (status, body) = get(
        app,
        &format!("terms?terms=true&terms.fl={field}&terms.limit=100"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "/terms for {field}: {body}");
    body.pointer(&format!("/terms/{field}"))
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("terms for {field} must be a flat array: {body}"))
        .iter()
        .step_by(2)
        .map(|value| value.as_str().expect("term must be a string").to_owned())
        .collect()
}

#[tokio::test]
async fn word_delimiter_parts_and_catenations_match_symmetrically_on_text_general() {
    let (app, _dir) = delimiter_app().await;

    // The explicit term-dictionary checks are mutation-sensitive: a query-only
    // workaround can otherwise make these lookups pass while failing to index
    // the symmetric forms.
    let terms = field_terms(&app, "general").await;
    for expected in ["sku", "42", "sku42"] {
        assert!(
            terms.contains(&expected.to_owned()),
            "general is missing {expected:?}: {terms:?}"
        );
    }
    assert!(
        !terms.contains(&"dayboy".to_owned()),
        "whitespace-separated words must never be catenated: {terms:?}"
    );

    // The same forms must be usable from the query side. `SKU-42` crosses two
    // UAX tokens, whereas `WiFi2000` splits inside one token.
    assert_field_query_count(&app, "general", "sku", 1).await;
    assert_field_query_count(&app, "general", "sku42", 1).await;
    assert_field_query_count(&app, "general", "wifi", 1).await;
    assert_field_query_count(&app, "general", "wifi2000", 1).await;
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
async fn delimiter_compounds_are_real_position_graphs_for_sequential_and_catenated_phrases() {
    let (app, _dir) = delimiter_app().await;

    // `SKU-42` crosses UAX tokens. Its parts must occupy positions 0 and 1,
    // while `sku42` spans both and leaves `next` at position 2. Flattening
    // every form at one position either loses the parts phrase or places the
    // following token one position too early.
    assert_field_query_count(&app, "general", "%22day+boy%22", 1).await;
    assert_field_query_count(&app, "general", "%22sku+42+next%22", 1).await;
    assert_field_query_count(&app, "general", "%22sku42+next%22", 1).await;
}

#[tokio::test]
async fn punctuation_linked_symbols_and_combining_marks_keep_safe_graph_positions() {
    let (app, _dir) = delimiter_app().await;

    // Emoji remains a meaningful Phase-3 UAX part when punctuation links it
    // to a word. It is sequential with `foo`, never catenated into `😀foo`.
    assert_field_query_count(&app, "general", "%22%F0%9F%98%80+foo+raven%22", 1).await;
    let terms = field_terms(&app, "general").await;
    assert!(
        terms.contains(&"😀".to_owned()),
        "emoji term missing: {terms:?}"
    );
    assert!(
        !terms.contains(&"😀foo".to_owned()),
        "symbols and words must not be catenated: {terms:?}"
    );

    // A combining-only source disappears during accent folding, so its linked
    // lexical part has a narrower graph width than the upstream source span.
    // The following word must remain visible at the next graph position.
    assert_field_query_count(&app, "general", "%22bar+zebra%22", 1).await;
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
