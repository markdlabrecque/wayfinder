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

use common::{assert_matches_fixture, get, indexed_app};

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
