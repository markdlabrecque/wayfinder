//! Spellcheck compatibility (issue #223).
//!
//! The fixtures use capture.sh's dedicated `spellcheck_223` corpus: `en` has
//! `quick`/`rocket`, while `und` has `quack`/`garden`. Their disagreement makes
//! repeated `spellcheck.dictionary` precedence observable.

mod common;

use axum::http::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;

const SPELLCHECK_223_SCHEMA_TOML: &str = r#"
[core]
name = "spellcheck_223"
unique_key = "id"
default_field = "spellcheck_en"

[[fields]]
name = "id"
type = "string"
stored = true
required = true
fast = true

[[fields]]
name = "spellcheck_en"
type = "text_en"
stored = true
multi_valued = true

[[fields]]
name = "spellcheck_und"
type = "text_en"
stored = true
multi_valued = true
"#;

async fn spellcheck_223_app() -> (axum::Router, TempDir) {
    let dir = TempDir::new().expect("create temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, SPELLCHECK_223_SCHEMA_TOML).expect("write schema.toml");
    let config_path = dir.path().join("wayfinder.toml");
    std::fs::write(&config_path, "strict_params = true\n").expect("write wayfinder.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let app = wayfinder::app_with_config(&schema_path, &data_dir, &config_path)
        .expect("spellcheck app must build");
    let corpus = json!([
        {"id":"s1","spellcheck_en":["quick quick quick rocket rocket"],"spellcheck_und":["quack quack quack garden"]},
        {"id":"s2","spellcheck_en":["quick brown fox"],"spellcheck_und":["quack garden"]}
    ]);
    let (status, body) = common::request_full(
        &app,
        "POST",
        "spellcheck_223/update?commit=true&wt=json",
        Some(&corpus.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seed spellcheck corpus: {body}");
    (app, dir)
}

async fn spellcheck_response(app: &axum::Router, query: &str) -> Value {
    let (status, body) =
        common::request_full(app, "GET", &format!("spellcheck_223/select?{query}"), None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "spellcheck query must pass strict mode: {body}"
    );
    body
}

#[tokio::test]
async fn spellcheck_flat_named_list_includes_offsets_and_collation() {
    let (app, _dir) = spellcheck_223_app().await;
    let actual = spellcheck_response(
        &app,
        "q=*:*&rows=0&wt=json&omitHeader=true&spellcheck=true&spellcheck.q=qwick%20roket&spellcheck.dictionary=en&spellcheck.collate=true&json.nl=flat",
    )
    .await;

    common::assert_matches_fixture(actual, "spellcheck_flat");
}

#[tokio::test]
async fn spellcheck_map_named_list_includes_offsets_and_collation() {
    let (app, _dir) = spellcheck_223_app().await;
    let actual = spellcheck_response(
        &app,
        "q=*:*&rows=0&wt=json&omitHeader=true&spellcheck=true&spellcheck.q=qwick%20roket&spellcheck.dictionary=en&spellcheck.collate=true&json.nl=map",
    )
    .await;

    common::assert_matches_fixture(actual, "spellcheck_map");
}

#[tokio::test]
async fn spellcheck_offsets_are_java_utf16_code_units_not_utf8_bytes() {
    let (app, _dir) = spellcheck_223_app().await;
    let actual = spellcheck_response(
        &app,
        "q=*:*&rows=0&wt=json&omitHeader=true&spellcheck=true&spellcheck.q=%C3%A9%20qwick&spellcheck.dictionary=en&spellcheck.collate=true&json.nl=flat",
    )
    .await;

    common::assert_matches_fixture(actual, "spellcheck_unicode_offsets");
}

#[tokio::test]
async fn first_repeated_spellcheck_dictionary_wins_when_en_is_first() {
    let (app, _dir) = spellcheck_223_app().await;
    let actual = spellcheck_response(
        &app,
        "q=*:*&rows=0&wt=json&omitHeader=true&spellcheck=true&spellcheck.q=qwick&spellcheck.dictionary=en&spellcheck.dictionary=und&spellcheck.collate=true&json.nl=flat",
    )
    .await;

    common::assert_matches_fixture(actual, "spellcheck_dictionary_en_first");
}

#[tokio::test]
async fn first_repeated_spellcheck_dictionary_wins_when_und_is_first() {
    let (app, _dir) = spellcheck_223_app().await;
    let actual = spellcheck_response(
        &app,
        "q=*:*&rows=0&wt=json&omitHeader=true&spellcheck=true&spellcheck.q=qwick&spellcheck.dictionary=und&spellcheck.dictionary=en&spellcheck.collate=true&json.nl=flat",
    )
    .await;

    common::assert_matches_fixture(actual, "spellcheck_dictionary_und_first");
}

#[tokio::test]
async fn spellcheck_is_absent_when_disabled_or_absent() {
    let (app, _dir) = spellcheck_223_app().await;

    for query in ["q=qwick", "q=qwick&spellcheck=false"] {
        let actual = spellcheck_response(&app, query).await;
        assert!(
            actual.get("spellcheck").is_none(),
            "spellcheck must be absent unless spellcheck=true: {actual}"
        );
    }
}
