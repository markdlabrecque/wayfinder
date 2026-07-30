//! Highlighting (issue #4, PRD §5 "Highlighting" row: `hl`, `hl.fl`,
//! `hl.snippets`, `hl.fragsize`, `hl.simple.pre`/`post`, Tantivy
//! `SnippetGenerator`).
//!
//! Every expected value here comes from a committed fixture in
//! `solr-ref/responses/hl_*.json`, captured against a dedicated Solr
//! container (`wayfinder-solr-4`, port 8991) running the same schema and
//! 5-doc "quick brown fox" corpus as the canonical `content` core
//! (`common::SCHEMA_TOML` / `common::corpus()`). See
//! `docs/solr-ref-findings.md` findings 52-55 for the narrative.
//!
//! Two shapes here are the crux of the issue and are asserted structurally,
//! not just via the whole-envelope fixture diff, so a future refactor cannot
//! quietly widen them back to a guess:
//!
//! - **A doc that matches the query through a field other than the
//!   highlighted one still gets a `highlighting` entry — an empty object,
//!   never an absent key and never `{"field": []}`** (`hl_no_field_match`,
//!   finding 52).
//! - **A field with no term overlap for a doc that *does* have a
//!   `highlighting` entry is simply absent from that doc's per-field
//!   map** (`hl_multi_field_comma`/`hl_multi_field_space`, finding 52).

mod common;

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;

use common::{assert_matches_fixture, get, indexed_app, post_docs};

#[tokio::test]
async fn hl_basic_matches_fixture() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=lazy&df=body&hl=true&hl.fl=body&wt=json").await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "hl_basic");
}

#[tokio::test]
async fn hl_snippets_two_matches_fixture() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=quick&df=body&hl=true&hl.fl=body&hl.snippets=2&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "hl_snippets_two");
}

#[tokio::test]
async fn hl_custom_markers_matches_fixture() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=lazy&df=body&hl=true&hl.fl=body&hl.simple.pre=%3Cb%3E&hl.simple.post=%3C%2Fb%3E&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "hl_custom_markers");
}

/// Under Solr's default `hl.method=unified`, a short punctuation-free field
/// is never truncated regardless of `hl.fragsize` (finding 55) -- a
/// no-truncation control, not the truncation assertion itself.
#[tokio::test]
async fn hl_fragsize_small_matches_fixture() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=quick&df=body&hl=true&hl.fl=body&hl.fragsize=18&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "hl_fragsize_small");
}

/// `hl.method=original` DOES truncate to `hl.fragsize` -- this is the shape
/// Tantivy's `SnippetGenerator` char-budget truncation actually resembles
/// (finding 55), so this is the truncation assertion's source fixture.
#[tokio::test]
async fn hl_fragsize_truncated_matches_fixture() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=quick&df=body&hl=true&hl.fl=body&hl.method=original&hl.fragsize=10&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "hl_fragsize_truncated");
}

/// The shared 5-doc corpus (`common::corpus()`) is too short to distinguish
/// "whole field" from "barely truncated" -- `doc1`'s `body` is only four
/// words, so any highlighter returns essentially the same text whether it
/// fragments or not. These two tests build an isolated single-doc app
/// against a ~310-char body long enough to make that distinction observable,
/// matching a dedicated Solr 9 capture (issue #104): `hl.fragsize=0` returns
/// the *entire* field as one unfragmented snippet, for both the default
/// `hl.method` (unified) and `hl.method=original`.
async fn long_field_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), common::SCHEMA_TOML).expect("app must build");
    let docs = json!([{
        "id": "long1",
        "body": "quick prototype notes from the engineering standup this morning. the team \
                 reviewed the roadmap for the next quarter and discussed several open risks \
                 around supply chain timing. afterwards everyone broke for lunch and \
                 reconvened at two in the afternoon to continue the planning session for the \
                 rest of the week."
    }]);
    let (status, body) = post_docs(&app, &docs).await;
    assert_eq!(status, StatusCode::OK, "indexing must succeed, got {body}");
    (app, dir)
}

/// Default `hl.method` (unified) with `hl.fragsize=0` returns the whole field
/// as a single unfragmented snippet, per real Solr 9 (issue #104). Wayfinder
/// currently falls back to Tantivy's 150-char default fragment instead.
#[tokio::test]
async fn hl_fragsize_zero_whole_field_matches_fixture() {
    let (app, _dir) = long_field_app().await;
    let (status, body) = get(
        &app,
        "select?q=body:quick&hl=true&hl.fl=body&hl.fragsize=0&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "hl_fragsize_zero_whole_field");
}

/// `hl.method=original` with `hl.fragsize=0` also returns the whole field
/// unfragmented -- byte-identical to the default-method case above, per real
/// Solr 9 (issue #104). Wayfinder currently treats `hl.fragsize=0` as unset
/// under `hl.method=original` and falls back to `DEFAULT_FRAGSIZE` (100
/// chars), truncating instead.
#[tokio::test]
async fn hl_fragsize_zero_whole_field_method_original_matches_fixture() {
    let (app, _dir) = long_field_app().await;
    let (status, body) = get(
        &app,
        "select?q=body:quick&hl=true&hl.fl=body&hl.method=original&hl.fragsize=0&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "hl_fragsize_zero_whole_field_method_original");
}

#[tokio::test]
async fn hl_multi_field_comma_matches_fixture() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=lazy&df=body&hl=true&hl.fl=body,category&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "hl_multi_field_comma");
}

#[tokio::test]
async fn hl_multi_field_space_matches_fixture() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=lazy&df=body&hl=true&hl.fl=body%20category&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "hl_multi_field_space");
}

#[tokio::test]
async fn hl_default_fl_matches_fixture() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=lazy&df=body&hl=true&wt=json").await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "hl_default_fl");
}

/// The crux fact (finding 52): a doc that matches the base query through a
/// field other than the one named in `hl.fl` still gets a `highlighting`
/// entry -- an empty object, not an absent key. Asserted both via the whole-
/// envelope fixture diff above the fold and directly here, so a future
/// implementation that drops empty-match docs (rather than emitting `{}`)
/// fails loudly with a specific message, not just a generic mismatch.
#[tokio::test]
async fn hl_no_field_match_matches_fixture_and_has_empty_object_shape() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&fq=category:animals&hl=true&hl.fl=body&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body.clone(), "hl_no_field_match");

    let highlighting = body
        .get("highlighting")
        .and_then(|h| h.as_object())
        .expect("response must carry a `highlighting` object");
    assert_eq!(
        highlighting.len(),
        2,
        "both doc1 and doc4 matched via `category` and must each get a highlighting entry"
    );
    for doc_id in ["doc1", "doc4"] {
        let entry = highlighting
            .get(doc_id)
            .unwrap_or_else(|| panic!("`highlighting` must carry a key for {doc_id}, not omit it"));
        assert_eq!(
            entry,
            &serde_json::json!({}),
            "{doc_id} has no term overlap in `body`, so its entry must be an empty object, \
             not `{{\"body\":[]}}` and not absent"
        );
    }
}

// --- hl.fl error handling (review round 1 must-fix) --------------------
//
// An undefined or non-text `hl.fl` field is a request-input problem, the
// same class as `check_sort`'s undefined-field error and
// `facet::check_facetable`'s undefined/unfacetable field -- both of which
// are Solr 400s, not 500s. Neither case is pinned by a captured `hl_*`
// fixture (no such request was captured against the reference container),
// so the exact wording and the `.with_response()` shape are this
// implementation's own inference from that sibling precedent, not ground
// truth -- see `docs/solr-ref-findings.md`'s "Not yet captured" section.

#[tokio::test]
async fn hl_undefined_field_is_400_and_carries_the_base_querys_response_block() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=lazy&df=body&hl=true&hl.fl=nosuchfield&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an undefined hl.fl field must 400, not 500, got {body}"
    );
    let msg = body
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        msg.contains("undefined field: nosuchfield"),
        "error.msg must name the undefined field, got: {msg}"
    );
    // Mirrors `facet_unknown_field.json`'s shape (issue #35): the base
    // query already ran by the time `hl.fl` is checked, so its real
    // `response` block stays alongside `error`, not zeroed out.
    assert_eq!(
        body.pointer("/response/numFound").and_then(Value::as_i64),
        Some(2),
        "response block from the base query must still be present alongside error, got {body}"
    );
}

/// A minimal schema with a Points-based (`int`) field alongside the usual
/// `id`/`body`, so a non-text `hl.fl` field actually exists to request.
const NUMERIC_SCHEMA_TOML: &str = r#"
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
name = "views"
type = "int"
stored = true
fast = true
"#;

async fn numeric_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), NUMERIC_SCHEMA_TOML).expect("app must build");
    let docs = json!([{"id":"doc1","body":"the quick brown fox","views":5}]);
    let (status, body) = post_docs(&app, &docs).await;
    assert_eq!(status, StatusCode::OK, "indexing must succeed, got {body}");
    (app, dir)
}

#[tokio::test]
async fn hl_non_text_field_is_400() {
    let (app, _dir) = numeric_app().await;
    let (status, body) = get(&app, "select?q=quick&hl=true&hl.fl=views&wt=json").await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a non-text hl.fl field must 400, not 500 (SnippetGenerator has no tokenizer for it), \
         got {body}"
    );
    let msg = body
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        msg.contains("non-text field: views"),
        "error.msg must name the non-text field, got: {msg}"
    );
}

// --- SELECT_PARAMS regression guard --------------------------------------

#[tokio::test]
async fn strict_params_accepts_every_implemented_highlight_param() {
    // Mirrors `tests/faceting.rs`'s `strict_params_accepts_every_implemented_facet_param`:
    // easy to implement a param and forget to list it in `SELECT_PARAMS`, and
    // `strict_params = true` then 400s a param Wayfinder actually supports.
    let dir = TempDir::new().expect("temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, common::SCHEMA_TOML).expect("write schema.toml");
    let config_path = dir.path().join("wayfinder.toml");
    std::fs::write(&config_path, "strict_params = true\n").expect("write wayfinder.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let app =
        wayfinder::app_with_config(&schema_path, &data_dir, &config_path).expect("app must build");
    let (status, body) = post_docs(&app, &common::corpus()).await;
    assert_eq!(status, StatusCode::OK, "indexing must succeed, got {body}");

    let (status, body) = get(
        &app,
        "select?q=lazy&df=body&hl=true&hl.fl=body&hl.snippets=2&hl.fragsize=50\
         &hl.method=original&hl.simple.pre=%3Cb%3E&hl.simple.post=%3C%2Fb%3E&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "every implemented hl.* param must pass strict mode, got body: {body}"
    );
}
