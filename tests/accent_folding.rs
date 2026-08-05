//! Unicode accent folding contract (issue #389, Phase 2).
//!
//! These assertions deliberately do not use the `an389_*` responses as expected
//! values: Phase 2 is a search-quality divergence from the captured Solr chain.

mod common;

use axum::http::StatusCode;
use common::{app_with_schema, get, post_docs};
use serde_json::{Value, json};
use tempfile::TempDir;
use wayfinder::schema;

const ACCENT_SCHEMA_TOML: &str = r#"
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

async fn accent_app() -> (axum::Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = app_with_schema(dir.path(), ACCENT_SCHEMA_TOML).expect("accent schema must load");
    let (status, body) = post_docs(
        &app,
        &json!([
            {"id": "cafe-precomposed", "body": "Café Central", "twm_suggest": ["Café Central"]},
            {"id": "cafe-nfd", "body": "Cafe\u{301} Central"},
            {"id": "cafe-ascii", "body": "Cafe Central"},
            {"id": "sharp-s", "body": "Straße"},
            {"id": "sharp-s-ascii", "body": "Strasse"},
            {"id": "ae", "body": "æther"},
            {"id": "ae-ascii", "body": "aether"},
            {"id": "c-acute", "body": "Ćwik"},
            {"id": "c-acute-ascii", "body": "Cwik"}
        ]),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing accent corpus must succeed: {body}"
    );
    (app, dir)
}

fn num_found(body: &Value) -> u64 {
    body.pointer("/response/numFound")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("response.numFound must be a number, got {body}"))
}

async fn assert_select_count(app: &axum::Router, query: &str, expected: u64) {
    let (status, body) = get(app, query).await;
    assert_eq!(status, StatusCode::OK, "/{query} must answer 200: {body}");
    assert_eq!(num_found(&body), expected, "/{query} returned {body}");
}

#[tokio::test]
async fn select_folds_accents_symmetrically_across_unicode_normalization_forms() {
    let (app, _dir) = accent_app().await;

    // The ASCII query proves index-side folding. The precomposed and NFD query
    // forms prove query-side folding, including equivalence between all three
    // spellings of the indexed word.
    assert_select_count(&app, "select?q=cafe", 3).await;
    assert_select_count(&app, "select?q=caf%C3%A9", 3).await;
    assert_select_count(&app, "select?q=cafe%CC%81", 3).await;
}

#[tokio::test]
async fn select_folds_non_decomposing_sharp_s_and_ae_ligature_in_both_directions() {
    let (app, _dir) = accent_app().await;

    // NFKD alone does not decompose either character, so these are the
    // explicit expansion-table contract, not merely normalization coverage.
    assert_select_count(&app, "select?q=strasse", 2).await;
    assert_select_count(&app, "select?q=stra%C3%9Fe", 2).await;
    assert_select_count(&app, "select?q=aether", 2).await;
    assert_select_count(&app, "select?q=%C3%A6ther", 2).await;
}

#[tokio::test]
async fn select_folds_capital_c_acute_to_ascii_c() {
    let (app, _dir) = accent_app().await;

    // Intentional divergence from `an389_terms_accent_en` / finding 195:
    // Solr leaves Ć unfolded because its source table's malformed `\U0106`
    // escape installs a literal rule. Wayfinder must fold Ć -> C correctly.
    assert_select_count(&app, "select?q=cwik", 2).await;
    assert_select_count(&app, "select?q=%C4%86wik", 2).await;
}

#[tokio::test]
async fn suggest_dictionary_folds_accents_for_lookup() {
    let (app, _dir) = accent_app().await;
    let (status, body) = get(&app, "suggest?suggest.dictionary=en&suggest.q=cafe&wt=json").await;
    assert_eq!(status, StatusCode::OK, "/suggest must answer 200: {body}");
    assert_eq!(
        body.pointer("/suggest/en/cafe/numFound")
            .and_then(Value::as_u64),
        Some(1),
        "the suggest dictionary must fold its indexed phrase and lookup query: {body}"
    );
}

#[test]
fn pre_folding_analyzer_contract_requires_reindex_after_the_term_format_change() {
    const PRE_FOLDING_CONTRACT: &str = "text_en_porter_compatible_v2";

    // A term-format change must not silently adopt an index whose marker names
    // the pre-folding v2 contract.
    assert_ne!(
        schema::ANALYZER_CONTRACT,
        PRE_FOLDING_CONTRACT,
        "Phase 2 changes indexed text terms, so ANALYZER_CONTRACT must move past v2"
    );

    let dir = TempDir::new().expect("temp dir");
    let app = app_with_schema(dir.path(), ACCENT_SCHEMA_TOML).expect("fresh index must build");
    drop(app);
    std::fs::write(
        dir.path().join("data/wayfinder-analyzer-contract"),
        PRE_FOLDING_CONTRACT,
    )
    .expect("write pre-folding analyzer-contract marker");

    let err = app_with_schema(dir.path(), ACCENT_SCHEMA_TOML)
        .expect_err("a pre-folding text index must require reindexing");
    assert!(
        format!("{err:#}").to_lowercase().contains("reindex"),
        "the pre-folding marker refusal must require reindexing, got: {err:#}"
    );
}
