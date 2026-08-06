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
            {"id": "static-compound", "body": "SKU-42 next WiFi2000"},
            {"id": "static-independent", "body": "day boy"},
            {"id": "stopword-gap", "body": "quick the fox"},
            {"id": "dynamic-compound", "tm_X3b_en_code": ["SKU-42 wifi_router"]},
            {"id": "dynamic-independent", "tm_X3b_en_code": ["happy sky"]},
            {"id": "suggest-delimiter", "twm_suggest": ["foo_bar"]},
            {"id": "emoji-compound", "body": "😀-foo raven", "twm_suggest": ["😀-foo"]},
            {"id": "combining-compound", "body": "\u{301}-bar zebra"}
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
async fn word_delimiter_parts_and_catenations_match_symmetrically_for_static_and_dynamic_text() {
    let (app, _dir) = delimiter_app().await;

    // Both static and dynamic text must retain each compound's parts and its
    // catenation. The explicit term-dictionary checks are mutation-sensitive:
    // a query-only workaround can otherwise make these lookups pass while
    // failing to index the symmetric forms.
    for field in ["body", "tm_X3b_en_code"] {
        let terms = field_terms(&app, field).await;
        for expected in ["sku", "42", "sku42"] {
            assert!(
                terms.contains(&expected.to_owned()),
                "{field} is missing {expected:?}: {terms:?}"
            );
        }
    }
    let static_terms = field_terms(&app, "body").await;
    let dynamic_terms = field_terms(&app, "tm_X3b_en_code").await;
    assert!(
        !static_terms.contains(&"dayboy".to_owned())
            && !dynamic_terms.contains(&"happyski".to_owned()),
        "whitespace-separated words must never be catenated: static={static_terms:?}, dynamic={dynamic_terms:?}"
    );

    // The same forms must be usable from the query side. `SKU-42` crosses two
    // UAX tokens, whereas `WiFi2000`/`wifi_router` split inside one token.
    assert_field_query_count(&app, "body", "sku", 1).await;
    assert_field_query_count(&app, "body", "sku42", 1).await;
    assert_field_query_count(&app, "body", "wifi", 1).await;
    assert_field_query_count(&app, "body", "wifi2000", 1).await;
    assert_field_query_count(&app, "tm_X3b_en_code", "sku42", 1).await;
    assert_field_query_count(&app, "tm_X3b_en_code", "wifi", 1).await;
    assert_field_query_count(&app, "tm_X3b_en_code", "wifirouter", 1).await;
}

/// A delimiter typed after a word is still an in-progress token, not a reason
/// to turn autocomplete into an exact lookup. This is mutation-sensitive: the
/// deliberately broken `last_is_prefix = false` rule makes this return zero,
/// even if a pre-Phase-4 `foo_` happened to pass through a tokenizer unchanged.
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
    let (status, body) = post_docs(
        &app,
        &json!([{"id": "split-stopword-gap", "body": "SKU42 the fox"}]),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "index split stopword corpus: {body}"
    );

    // `SKU-42` crosses UAX tokens. Its parts must occupy positions 0 and 1,
    // while `sku42` spans both and leaves `next` at position 2. Flattening
    // every form at one position either loses the parts phrase or places the
    // following token one position too early.
    assert_field_query_count(&app, "body", "%22day+boy%22", 1).await;
    assert_field_query_count(&app, "body", "%22sku+42+next%22", 1).await;
    assert_field_query_count(&app, "body", "%22sku42+next%22", 1).await;

    // Stopword removal leaves a real incoming position gap. The delimiter
    // graph must preserve it, so the analyzed phrase crosses that gap while a
    // compact quoted phrase cannot bridge it.
    assert_field_query_count(&app, "body", "%22quick+the+fox%22", 1).await;
    assert_field_query_count(&app, "body", "%22quick+fox%22", 0).await;

    // Splitting one upstream word into `sku`/`42` expands its graph width by
    // one position. The following stopword gap must be mapped after that
    // shift, rather than compacted by the expanded graph.
    assert_field_query_count(&app, "body", "%22sku42+fox%22", 0).await;
}

#[tokio::test]
async fn punctuation_linked_symbols_and_combining_marks_keep_safe_graph_positions() {
    let (app, _dir) = delimiter_app().await;

    // Emoji remains a meaningful Phase-3 UAX part when punctuation links it
    // to a word. It is sequential with `foo`, never catenated into `😀foo`.
    assert_field_query_count(&app, "body", "%22%F0%9F%98%80+foo+raven%22", 1).await;
    let terms = field_terms(&app, "body").await;
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
    assert_field_query_count(&app, "body", "%22bar+zebra%22", 1).await;

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
