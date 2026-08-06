//! Search-quality contract for issue #389's UAX #29 tokenizer and Phase 4
//! word-delimiter expansion.
//!
//! These assertions deliberately do not use the `an389_*` responses: the
//! premise change makes their Solr token stream reference material, not the
//! contract. In particular, single-codepoint CJK terms and one-character word
//! segments are meaningful and must survive analysis.

mod common;

use std::collections::BTreeSet;
use std::path::Path;

use axum::http::StatusCode;
use common::{app_with_schema, get, post_docs};
use serde_json::{Value, json};
use tempfile::TempDir;

const UAX29_SCHEMA: &str = r#"
[core]
name = "content"
unique_key = "id"
default_field = "general"

[[fields]]
name = "id"
type = "string"
stored = true
required = true
fast = true

[[fields]]
name = "general"
type = "text_general"
stored = true

[[fields]]
name = "english"
type = "text_en"
stored = true

[[fields]]
name = "twm_suggest"
type = "text_en"
stored = true
multi_valued = true
"#;

const UAX29_SAMPLE: &str = "don't www.example.com 3.14 3 foo_bar e-mail a@b.com 東京都 !!!";

async fn uax29_app() -> (axum::Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = app_with_schema(dir.path(), UAX29_SCHEMA).expect("UAX #29 schema must load");
    let (status, body) = post_docs(
        &app,
        &json!([
            {
                "id": "tokenized",
                "general": UAX29_SAMPLE,
                "english": UAX29_SAMPLE,
                "twm_suggest": ["東京都 travel guide"]
            },
            {
                "id": "sentinel",
                "general": "unrelated material",
                "english": "unrelated material"
            }
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "indexing tokenizer corpus: {body}");
    (app, dir)
}

fn num_found(body: &Value) -> u64 {
    body.pointer("/response/numFound")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("response.numFound must be a number: {body}"))
}

async fn assert_field_term(app: &axum::Router, field: &str, term: &str, expected: u64) {
    // Quoting makes the request a literal term/phrase query rather than query
    // syntax; `term` is already percent-encoded where it contains non-ASCII
    // or a URL-reserved character.
    let query = format!("select?q={field}%3A%22{term}%22");
    let (status, body) = get(app, &query).await;
    assert_eq!(status, StatusCode::OK, "/{query} must answer 200: {body}");
    assert_eq!(num_found(&body), expected, "/{query} returned {body}");
}

fn term_set<'a>(body: &'a Value, field: &str) -> BTreeSet<&'a str> {
    body.pointer(&format!("/terms/{field}"))
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("/terms/{field} must be a flat term array: {body}"))
        .iter()
        .step_by(2)
        .map(|term| {
            term.as_str()
                .unwrap_or_else(|| panic!("term must be a string: {term}"))
        })
        .collect()
}

#[tokio::test]
async fn text_presets_use_uax29_word_boundaries_without_discarding_meaningful_singletons() {
    let (app, _dir) = uax29_app().await;

    // UAX #29 keeps apostrophes, dots, and underscores inside their applicable
    // word classes; it breaks at hyphen and `@`. Phase 4 catenates parts of
    // those delimiter compounds, never independent whitespace-separated words.
    let (status, body) = get(&app, "terms?terms=true&terms.fl=general&terms.limit=100").await;
    assert_eq!(status, StatusCode::OK, "/terms must answer 200: {body}");
    assert_eq!(
        term_set(&body, "general"),
        BTreeSet::from([
            "14",
            "3",
            "3.14",
            "314",
            "a",
            "abcom",
            "b",
            "b.com",
            "bar",
            "bcom",
            "com",
            "don",
            "don't",
            "dont",
            "e",
            "email",
            "example",
            "foo",
            "foo_bar",
            "foobar",
            "mail",
            "material",
            "t",
            "unrelated",
            "www",
            "www.example.com",
            "wwwexamplecom",
            "京",
            "東",
            "都",
        ]),
        "the indexed vocabulary must retain compounds without joining independent words: {body}"
    );

    // Search must use the same analyzer as indexing for both built-in text
    // presets. The sentinel ensures a discarded one-character/CJK query cannot
    // accidentally pass by becoming an empty query that matches everything.
    for term in [
        "don%27t",
        "www.example.com",
        "3",
        "3.14",
        "foo_bar",
        "e",
        "mail",
        "a",
        "b.com",
        "%E6%9D%B1%E4%BA%AC%E9%83%BD",
    ] {
        assert_field_term(&app, "general", term, 1).await;
    }
    // English chains retain Tantivy's complete stopword semantics: UAX #29
    // removes only the blind min=2 cutoff, not the stopword filter.
    for term in [
        "don%27t",
        "www.example.com",
        "3",
        "3.14",
        "foo_bar",
        "e",
        "mail",
        "b.com",
        "%E6%9D%B1%E4%BA%AC%E9%83%BD",
    ] {
        assert_field_term(&app, "english", term, 1).await;
    }
    assert_field_term(&app, "english", "a", 0).await;

    // Phase 4 deliberately makes delimiter parts independently searchable.
    for field in ["general", "english"] {
        for fragment in ["don", "example", "14", "foo", "com"] {
            assert_field_term(&app, field, fragment, 1).await;
        }
    }
}

#[tokio::test]
async fn search_api_dynamic_text_and_suggest_use_the_uax29_chain() {
    let dir = TempDir::new().expect("temp dir");
    let preset = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("presets/search-api.toml"),
    )
    .expect("Search API preset must be readable");
    let app = app_with_schema(dir.path(), &preset).expect("Search API preset must load");
    let (status, body) = post_docs(
        &app,
        &json!([
            {"id": "dynamic-tokenized", "tm_X3b_en_body": [UAX29_SAMPLE], "twm_suggest": ["東京都 travel guide"]},
            {"id": "dynamic-sentinel", "tm_X3b_en_body": ["unrelated material"]}
        ]),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing dynamic tokenizer corpus: {body}"
    );

    for term in [
        "don%27t",
        "www.example.com",
        "3",
        "3.14",
        "foo_bar",
        "e",
        "%E6%9D%B1%E4%BA%AC%E9%83%BD",
    ] {
        assert_field_term(&app, "tm_X3b_en_body", term, 1).await;
    }
    assert_field_term(&app, "tm_X3b_en_body", "a", 0).await;
    for fragment in ["don", "example", "14", "foo"] {
        assert_field_term(&app, "tm_X3b_en_body", fragment, 1).await;
    }

    let (status, body) = get(
        &app,
        "suggest?suggest.dictionary=en&suggest.q=%E6%9D%B1%E4%BA%AC%E9%83%BD&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "/suggest must answer 200: {body}");
    assert_eq!(
        body.pointer("/suggest/en/東京都/numFound")
            .and_then(Value::as_u64),
        Some(1),
        "the suggest analyzer must preserve CJK word segments at index and query time: {body}"
    );
}
