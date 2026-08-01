//! Spellcheck compatibility (issue #222).
//!
//! `solr-ref/search-api/trace/00021.json` shows that Search API sends two
//! `spellcheck.dictionary` values and receives an empty spellcheck envelope
//! after highlighting when `qwick` has no matches.

mod common;

use axum::http::StatusCode;
use common::key_order::{KeyOrder, get_text};
use common::{CORE, post_docs};
use serde_json::{Value, json};
use tempfile::TempDir;

/// Expiring ceiling guard: delete this test when real spellcheck suggestion
/// generation lands. Until then, `qwick` must stay empty even though the
/// indexed corpus contains `quick`; 75/75 measures this envelope, not spelling
/// correction.
#[tokio::test]
async fn delete_this_empty_ceiling_guard_when_real_spellcheck_suggestions_land() {
    let dir = TempDir::new().expect("create temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, common::SCHEMA_TOML).expect("write schema.toml");
    let config_path = dir.path().join("wayfinder.toml");
    std::fs::write(&config_path, "strict_params = true\n").expect("write wayfinder.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let app =
        wayfinder::app_with_config(&schema_path, &data_dir, &config_path).expect("app must build");
    let (index_status, index_body) = post_docs(&app, &common::corpus()).await;
    assert_eq!(
        index_status,
        StatusCode::OK,
        "index reference corpus: {index_body}"
    );

    let (status, text) = get_text(
        &app,
        CORE,
        "select?q=qwick&hl=true&omitHeader=true&spellcheck=true&spellcheck.q=qwick\
         &spellcheck.dictionary=en&spellcheck.dictionary=und&spellcheck.collate=true\
         &json.nl=flat&wt=json",
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "the captured spellcheck params must pass strict mode, got {text}"
    );
    let body: Value = serde_json::from_str(&text).expect("spellcheck response must be JSON");
    assert_eq!(
        body.pointer("/spellcheck"),
        Some(&json!({"suggestions": [], "collations": []})),
        "the captured no-match request must emit Solr's empty spellcheck envelope, got {text}"
    );
    assert_eq!(
        KeyOrder::parse(&text)
            .keys()
            .expect("response must be an object"),
        vec!["response", "highlighting", "spellcheck"],
        "spellcheck must follow highlighting in the response body, got {text}"
    );

    for query in ["select?q=qwick", "select?q=qwick&spellcheck=false"] {
        let (status, text) = get_text(&app, CORE, query).await;
        assert_eq!(status, StatusCode::OK, "spellcheck gate request: {text}");
        let body: Value = serde_json::from_str(&text).expect("gated response must be JSON");
        assert!(
            body.get("spellcheck").is_none(),
            "spellcheck must be absent unless spellcheck=true, got {text}"
        );
    }
}
