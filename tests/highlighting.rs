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

/// A Search API field name resolved solely through the shipped preset's
/// `tm_X3b_en_*` dynamic rule must be a valid highlighting field, not just a
/// queryable/indexable field.
#[tokio::test]
async fn hl_fl_accepts_and_highlights_a_search_api_dynamic_text_field() {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), include_str!("../presets/search-api.toml"))
        .expect("the Search API preset must build");
    let docs = json!([{
        "id": "dynamic-hl",
        "tm_X3b_en_body": ["Wombat Forest"]
    }]);
    let (status, indexed) = post_docs(&app, &docs).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "dynamic preset field must index: {indexed}"
    );

    let (status, body) = get(
        &app,
        "select?q=tm_X3b_en_body:wombat&hl=true&hl.fl=tm_X3b_en_body&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a dynamic Search API text field must be accepted by hl.fl: {body}"
    );
    assert_eq!(
        body.pointer("/response/numFound"),
        Some(&json!(1)),
        "the dynamic-field query must find the indexed document: {body}"
    );
    let snippet = body
        .pointer("/highlighting/dynamic-hl/tm_X3b_en_body/0")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("dynamic hl.fl must produce a snippet: {body}"));
    assert!(
        snippet.contains("<em>Wombat</em>"),
        "the matching term in the dynamic field must be highlighted: {body}"
    );
}

/// Dynamic fields share a JSON catch-all, so query terms must be decoded from
/// their JSON-path term representation even when the query and highlighted
/// field use different paths. Solr's default `hl.requireFieldMatch=false`
/// allows this cross-field highlight; `true` remains the strict control.
#[tokio::test]
async fn hl_dynamic_default_require_field_match_highlights_another_dynamic_field() {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), include_str!("../presets/search-api.toml"))
        .expect("the Search API preset must build");
    let docs = json!([{
        "id": "dynamic-cross-hl",
        "tm_X3b_en_title": ["Wombat title"],
        "tm_X3b_en_body": ["Wombat body"]
    }]);
    let (status, indexed) = post_docs(&app, &docs).await;
    assert_eq!(status, StatusCode::OK, "indexing must succeed: {indexed}");

    let (status, body) = get(
        &app,
        "select?q=tm_X3b_en_title:wombat&hl=true&hl.fl=tm_X3b_en_body&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "cross-dynamic highlighting must succeed: {body}"
    );
    assert_eq!(
        body.pointer("/highlighting/dynamic-cross-hl/tm_X3b_en_body/0"),
        Some(&json!("<em>Wombat</em> body")),
        "absent hl.requireFieldMatch must decode the title JSON term and highlight body: {body}"
    );

    let (status, body) = get(
        &app,
        "select?q=tm_X3b_en_title:wombat&hl=true&hl.fl=tm_X3b_en_body\
         &hl.requireFieldMatch=true&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "strict dynamic highlighting must succeed: {body}"
    );
    assert_eq!(
        body.pointer("/highlighting/dynamic-cross-hl"),
        Some(&json!({})),
        "hl.requireFieldMatch=true must not cross dynamic JSON paths: {body}"
    );
}

/// `hl.preserveMulti=true` is resolved from dynamic pattern metadata, not a
/// static `[[fields]]` entry. Each stored value must retain its own snippet.
#[tokio::test]
async fn hl_dynamic_preserve_multi_returns_one_snippet_per_value() {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), include_str!("../presets/search-api.toml"))
        .expect("the Search API preset must build");
    let docs = json!([{
        "id": "dynamic-preserve-hl",
        "tm_X3b_en_body": ["Wombat Forest", "quiet meadow"]
    }]);
    let (status, indexed) = post_docs(&app, &docs).await;
    assert_eq!(status, StatusCode::OK, "indexing must succeed: {indexed}");

    let (status, body) = get(
        &app,
        "select?q=tm_X3b_en_body:wombat&hl=true&hl.fl=tm_X3b_en_body\
         &hl.method=original&hl.preserveMulti=true&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "dynamic preserveMulti must succeed: {body}"
    );
    assert_eq!(
        body.pointer("/highlighting/dynamic-preserve-hl/tm_X3b_en_body"),
        Some(&json!(["<em>Wombat</em> Forest", "quiet meadow"])),
        "dynamic preserveMulti must preserve both values in indexed order: {body}"
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

    // Issue #139: `hl.mergeContiguous` and `hl.requireFieldMatch` join the
    // rest of the implemented `hl.*` surface here. `hl.fl=*` is exercised
    // separately below (it interacts with field resolution, not just
    // `SELECT_PARAMS` membership), so this request keeps the explicit
    // `hl.fl=body` the rest of this guard already used. Issue #353 adds the
    // five `setHighlighting()` params: `hl.preserveMulti` is a no-op here
    // (`body` is single-valued), `hl.fragmenter=gap` is the default, and the
    // other three are admitted-and-inert -- all must clear `strict_params`.
    let (status, body) = get(
        &app,
        "select?q=lazy&df=body&hl=true&hl.fl=body&hl.snippets=2&hl.fragsize=50\
         &hl.method=original&hl.simple.pre=%3Cb%3E&hl.simple.post=%3C%2Fb%3E\
         &hl.mergeContiguous=false&hl.requireFieldMatch=false\
         &hl.preserveMulti=true&hl.fragmenter=gap&hl.maxAnalyzedChars=51200\
         &hl.usePhraseHighlighter=false&hl.highlightMultiTerm=false&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "every implemented hl.* param must pass strict mode, got body: {body}"
    );
}

// --- Issue #353: hl.preserveMulti + hl.fragmenter admission ---------------
//
// `SearchApiSolrBackend::setHighlighting()` emits five `hl.*` params only
// when non-default; under `strict_params = true` each 400'd a request the
// client legitimately sends. `hl.preserveMulti` is the one with wire-visible
// semantics: under `hl.method=original` it returns one snippet PER VALUE of a
// multi-valued field (matching highlighted, non-matching plain), where the
// default merges the values into one stream. `hl.fragmenter=gap` is Solr's
// own default original-method fragmenter, so admitting it changes nothing.
// Both are pinned by the `hl353_*` fixtures captured against the same 5-doc
// `category` corpus the rest of this suite uses.

#[tokio::test]
async fn hl_preserve_multi_on_returns_one_snippet_per_value_in_order() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=category:animals&hl=true&hl.fl=category&hl.method=original\
         &hl.preserveMulti=true&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "hl.preserveMulti=true must not 400 under strict_params, got {body}"
    );
    let expected = common::fixture("hl353_preserve_multi_on")
        .pointer("/highlighting")
        .cloned()
        .expect("hl353_preserve_multi_on carries a highlighting block");
    assert_eq!(
        body.pointer("/highlighting"),
        Some(&expected),
        "hl.preserveMulti=true under hl.method=original must return one snippet \
         per value in indexed order -- matching values highlighted, \
         non-matching values plain -- got {body}"
    );
}

#[tokio::test]
async fn hl_preserve_multi_off_merges_values_into_one_stream() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=category:animals&hl=true&hl.fl=category&hl.method=original&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let expected = common::fixture("hl353_preserve_multi_off")
        .pointer("/highlighting")
        .cloned()
        .expect("hl353_preserve_multi_off carries a highlighting block");
    assert_eq!(
        body.pointer("/highlighting"),
        Some(&expected),
        "hl.preserveMulti absent must merge the values into one stream and \
         return only matching fragments, got {body}"
    );
}

#[tokio::test]
async fn hl_fragmenter_gap_is_byte_identical_to_the_default_fragmenter() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=body:lazy&hl=true&hl.fl=body&hl.method=original&hl.fragsize=20\
         &hl.fragmenter=gap&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "hl.fragmenter=gap must not 400, got {body}"
    );
    let expected = common::fixture("hl353_fragmenter_gap")
        .pointer("/highlighting")
        .cloned()
        .expect("hl353_fragmenter_gap carries a highlighting block");
    assert_eq!(
        body.pointer("/highlighting"),
        Some(&expected),
        "hl.fragmenter=gap is Solr's default original-method fragmenter, so it \
         must match the default gap behaviour exactly, got {body}"
    );
}

/// Captured: `hl.preserveMulti` is a no-op under the default (`hl.method=unified`)
/// highlighter -- real Solr returns identical output with and without it. This
/// guards that Wayfinder does not activate the per-value path off the original
/// method, where the unified highlighter is itself a ponytail (finding 55).
#[tokio::test]
async fn hl_preserve_multi_is_a_noop_under_the_default_unified_method() {
    let (app, _dir) = indexed_app().await;
    let (status, off) = get(
        &app,
        "select?q=category:animals&hl=true&hl.fl=category&wt=json",
    )
    .await;
    let (status_on, on) = get(
        &app,
        "select?q=category:animals&hl=true&hl.fl=category&hl.preserveMulti=true&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {off}");
    assert_eq!(status_on, StatusCode::OK, "got {on}");
    assert_eq!(
        off.pointer("/highlighting"),
        on.pointer("/highlighting"),
        "hl.preserveMulti must be a no-op under the default hl.method"
    );
}

// --- Issue #139: hl.fl=*, hl.mergeContiguous, hl.requireFieldMatch --------
//
// Every captured `search_api_solr` request asks for highlighting the same
// way (`docs/solr-ref-findings.md`'s highlighting section, issue #139's
// brief): `hl.fl=*`, `hl.requireFieldMatch=false`, `hl.mergeContiguous=false`,
// alongside the already-implemented `hl.snippets`/`hl.fragsize`/
// `hl.simple.pre`/`hl.simple.post`. `hl.fl=*` appears in 19 of the 28 traces
// under `solr-ref/search-api/trace/`.
//
// The dedicated `hl_wildcard_stored_string` Solr capture settles the
// previously ambiguous stored-string case: `hl.fl=*` includes `category` and
// yields the same snippets as explicit `hl.fl=category` for this request.

/// The baseline case: `common::SCHEMA_TOML` has exactly one text field
/// (`body`, also `default_field`), so `hl.fl=*` must reproduce `hl_basic`'s
/// `highlighting` block exactly -- same fixture as `hl_basic_matches_fixture`
/// above, only `hl.fl` differs (`*` instead of the explicit `body`), so the
/// full-envelope `assert_matches_fixture` helper cannot be reused as-is (its
/// captured `responseHeader.params.hl.fl` says `"body"`, not `"*"`); the
/// `/highlighting` block is compared directly instead, which is unaffected
/// by that difference.
#[tokio::test]
async fn hl_wildcard_fl_matches_hl_basic_fixtures_highlighting_block() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=lazy&df=body&hl=true&hl.fl=*&wt=json").await;
    assert_eq!(status, StatusCode::OK, "hl.fl=* must not 400 -- got {body}");
    let expected = common::fixture("hl_basic")
        .pointer("/highlighting")
        .cloned()
        .expect("hl_basic fixture carries a highlighting block");
    assert_eq!(
        body.pointer("/highlighting"),
        Some(&expected),
        "hl.fl=* over a single-text-field schema must resolve to exactly that \
         field, reproducing hl_basic's highlighting block, got {body}"
    );
}

/// Ground truth from `hl_wildcard_stored_string`: a wildcard field list
/// includes the matching stored string field. The explicit request is the
/// paired equivalence check, so both spellings keep the same `/highlighting`.
#[tokio::test]
async fn hl_wildcard_fl_matches_stored_string_fixture_and_explicit_field() {
    let (app, _dir) = indexed_app().await;
    let expected = common::fixture("hl_wildcard_stored_string")
        .pointer("/highlighting")
        .cloned()
        .expect("hl_wildcard_stored_string fixture carries a highlighting block");

    let (wildcard_status, wildcard) =
        get(&app, "select?q=category:animals&hl=true&hl.fl=*&wt=json").await;
    assert_eq!(
        wildcard_status,
        StatusCode::OK,
        "hl.fl=* must succeed, got {wildcard}"
    );
    assert_eq!(
        wildcard.pointer("/highlighting"),
        Some(&expected),
        "hl.fl=* must include stored string `category`, matching the Solr fixture, got {wildcard}"
    );

    let (explicit_status, explicit) = get(
        &app,
        "select?q=category:animals&hl=true&hl.fl=category&wt=json",
    )
    .await;
    assert_eq!(
        explicit_status,
        StatusCode::OK,
        "explicit hl.fl=category must succeed, got {explicit}"
    );
    assert_eq!(
        explicit.pointer("/highlighting"),
        Some(&expected),
        "wildcard and explicit hl.fl=category must produce the same highlighting, got {explicit}"
    );
}

/// Not pinned by any captured fixture (no traced request combines `hl.fl=*`
/// with a schema carrying two *text* fields where only one is
/// `default_field` -- see the module-level comment above on why the traces
/// alone are ambiguous here). This is this implementation's own inference
/// from Solr's documented/sourced wildcard-expansion mechanism: `*` expands
/// against the schema's field names, not the query's `qf`/`df` set, so a
/// text field that is not `default_field` and was not named in `q` must
/// still be swept up and highlighted if it has term overlap. A schema where
/// `hl.fl=*` and `hl.fl=default_field` would silently agree cannot catch a
/// regression that narrows the wildcard back down to just `default_field`,
/// which is why this builds its own two-text-field schema rather than
/// reusing `common::SCHEMA_TOML` (whose sole text field already *is*
/// `default_field`).
const TWO_TEXT_FIELD_SCHEMA_TOML: &str = r#"
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
name = "title"
type = "text_en"
stored = true

[[fields]]
name = "category"
type = "string"
stored = true
fast = true
"#;

async fn two_text_field_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app =
        common::app_with_schema(dir.path(), TWO_TEXT_FIELD_SCHEMA_TOML).expect("app must build");
    let docs = json!([{
        "id": "onlytitle",
        "body": "unrelated filler content with no overlap",
        "title": "a zephyr unique headline",
        "category": "misc"
    }]);
    let (status, body) = post_docs(&app, &docs).await;
    assert_eq!(status, StatusCode::OK, "indexing must succeed, got {body}");
    (app, dir)
}

#[tokio::test]
async fn hl_wildcard_fl_highlights_a_non_default_text_field() {
    let (app, _dir) = two_text_field_app().await;
    // `q=title:zephyr` matches only via `title`, never via `body` (the
    // schema's `default_field`), so a highlighter that only ever considered
    // `default_field` for `hl.fl=*` would come back with no `title` snippet
    // at all.
    let (status, body) = get(&app, "select?q=title:zephyr&hl=true&hl.fl=*&wt=json").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "hl.fl=* must not 400 on a two-text-field schema, got {body}"
    );
    let entry = body
        .pointer("/highlighting/onlytitle")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("must carry a highlighting entry for onlytitle, got {body}"));
    let title = entry
        .get("title")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!(
                "hl.fl=* must highlight `title` even though it is not \
                 default_field, got entry {entry:?}"
            )
        });
    assert!(
        title.iter().any(|snippet| snippet
            .as_str()
            .is_some_and(|s| s.contains("<em>zephyr</em>"))),
        "expected a <em>zephyr</em> snippet in title, got {title:?}"
    );
    assert!(
        !entry.contains_key("body"),
        "body has no term overlap for `zephyr` and must be absent, not `[]`, got {entry:?}"
    );
    assert!(
        !entry.contains_key("category"),
        "category has no `zephyr` overlap and must be absent, not `[]`, got {entry:?}"
    );
}

/// On a one-field query/corpus, explicitly selecting Solr's false defaults
/// is observationally identical to omitting both params. The dedicated
/// issue-#181 tests below discriminate each false and true path.
#[tokio::test]
async fn hl_require_field_match_and_merge_contiguous_false_are_no_ops() {
    let (app, _dir) = indexed_app().await;
    let (_, without) = get(&app, "select?q=lazy&df=body&hl=true&hl.fl=body&wt=json").await;
    let (status, with) = get(
        &app,
        "select?q=lazy&df=body&hl=true&hl.fl=body\
         &hl.requireFieldMatch=false&hl.mergeContiguous=false&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {with}");
    assert_eq!(
        with.pointer("/highlighting"),
        without.pointer("/highlighting"),
        "hl.requireFieldMatch=false/hl.mergeContiguous=false must not change \
         the highlighting block at all -- both are already Wayfinder's \
         unconditional behaviour, got with={with} without={without}"
    );
}

/// Open question 4: does the `[HIGHLIGHT]`/`[/HIGHLIGHT]` marker pair the
/// module actually sends round-trip correctly? `hl_custom_markers.json`
/// already proves `hl.simple.pre`/`hl.simple.post` is a real captured,
/// parameterised mechanism (`<b>`/`</b>` in that fixture, not the `<em>`
/// default) -- this reuses the exact same base query (`hl_basic`'s
/// `q=lazy&df=body`) with the module's own marker strings substituted in,
/// which is a mechanical transform of that captured mechanism, not an
/// invented value.
#[tokio::test]
async fn hl_search_api_solr_markers_round_trip() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=lazy&df=body&hl=true&hl.fl=body\
         &hl.simple.pre=%5BHIGHLIGHT%5D&hl.simple.post=%5B%2FHIGHLIGHT%5D&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let expected = json!({
        "doc2": {"body": ["a [HIGHLIGHT]lazy[/HIGHLIGHT] afternoon in the garden"]},
        "doc1": {"body": ["the quick brown fox jumps over the [HIGHLIGHT]lazy[/HIGHLIGHT] dog"]}
    });
    assert_eq!(body.pointer("/highlighting"), Some(&expected), "got {body}");
}

/// The tracer-bullet integration test: the exact parameter combination every
/// captured `search_api_solr` request sends (`docs/solr-ref-findings.md`'s
/// highlighting section; issue #139's brief), replayed end-to-end. Every
/// individual piece is pinned by a narrower test above/in this file
/// (`hl.fl=*`, both highlighting booleans on their false/default paths, `hl.fragsize=0`
/// already covered by `hl_fragsize_zero_whole_field_matches_fixture`,
/// `hl.snippets=3`, and the marker pair) -- this just confirms they compose
/// without a 400 or a surprising interaction, using `long_field_app`'s
/// isolated single-doc corpus so `hl.fragsize=0`'s whole-field behaviour is
/// actually observable (finding 81; the shared 5-doc corpus is too short to
/// tell "whole field" from "barely truncated" per the module docs above).
#[tokio::test]
async fn hl_search_api_solr_request_shape_end_to_end() {
    let (app, _dir) = long_field_app().await;
    let (status, body) = get(
        &app,
        "select?q=body:quick&hl=true&hl.fl=*&hl.requireFieldMatch=false\
         &hl.snippets=3&hl.fragsize=0&hl.mergeContiguous=false\
         &hl.simple.pre=%5BHIGHLIGHT%5D&hl.simple.post=%5B%2FHIGHLIGHT%5D&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the exact search_api_solr highlighting param combination must not 400, got {body}"
    );
    let whole_field_em = common::fixture("hl_fragsize_zero_whole_field")
        .pointer("/highlighting/long1/body/0")
        .and_then(Value::as_str)
        .expect("hl_fragsize_zero_whole_field fixture carries long1's whole-field snippet")
        .to_string();
    let expected_snippet = whole_field_em
        .replace("<em>", "[HIGHLIGHT]")
        .replace("</em>", "[/HIGHLIGHT]");
    assert_eq!(
        body.pointer("/highlighting/long1/body/0"),
        Some(&Value::String(expected_snippet)),
        "got {body}"
    );
}

// Dedicated schema and corpus from the issue #181 capture. The true paths
// need a second text field and spaced matches, neither of which the shared
// corpus supplies.
const TRUE_PATHS_SCHEMA_TOML: &str = r#"
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
name = "title"
type = "text_en"
stored = true

[[fields]]
name = "body"
type = "text_en"
stored = true
"#;

async fn true_paths_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), TRUE_PATHS_SCHEMA_TOML).expect("app must build");
    let docs = json!([
        {"id":"rfm1","title":"quick launch","body":"quick fox"},
        {"id":"rfm2","title":"quiet launch","body":"quick fox"},
        {"id":"merge1","title":"merge probe","body":"alpha one two three four five six seven eight nine ten eleven twelve beta thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty gamma"},
        {"id":"empty1","title":"emptyprobe","body":""}
    ]);
    let (status, body) = post_docs(&app, &docs).await;
    assert_eq!(status, StatusCode::OK, "indexing must succeed, got {body}");
    (app, dir)
}

#[tokio::test]
async fn hl_true_paths_require_field_match_false_matches_fixture_highlighting_block() {
    let (app, _dir) = true_paths_app().await;
    let (status, body) = get(
        &app,
        "select?q=title:quick%20OR%20body:fox&fl=id&sort=id%20asc&hl=true\
         &hl.fl=title,body&hl.requireFieldMatch=false&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let expected = common::fixture("hl_require_field_match_false")
        .pointer("/highlighting")
        .cloned()
        .expect("fixture carries a highlighting block");
    assert_eq!(
        body.pointer("/highlighting"),
        Some(&expected),
        "hl.requireFieldMatch=false must allow cross-field query terms, got {body}"
    );
}

#[tokio::test]
async fn hl_true_paths_require_field_match_absent_defaults_to_false() {
    let (app, _dir) = true_paths_app().await;
    let (status, body) = get(
        &app,
        "select?q=title:quick%20OR%20body:fox&fl=id&sort=id%20asc&hl=true\
         &hl.fl=title,body&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let expected = common::fixture("hl_require_field_match_false")
        .pointer("/highlighting")
        .cloned()
        .expect("fixture carries a highlighting block");
    assert_eq!(
        body.pointer("/highlighting"),
        Some(&expected),
        "absent hl.requireFieldMatch must use Solr's false default, got {body}"
    );
}

#[tokio::test]
async fn hl_true_paths_require_field_match_matches_fixture_highlighting_block() {
    let (app, _dir) = true_paths_app().await;
    let (status, body) = get(
        &app,
        "select?q=title:quick%20OR%20body:fox&fl=id&sort=id%20asc&hl=true\
         &hl.fl=title,body&hl.requireFieldMatch=true&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let expected = common::fixture("hl_require_field_match_true")
        .pointer("/highlighting")
        .cloned()
        .expect("fixture carries a highlighting block");
    assert_eq!(
        body.pointer("/highlighting"),
        Some(&expected),
        "hl.requireFieldMatch=true must filter query terms by highlighted field, got {body}"
    );
}

#[tokio::test]
async fn hl_true_paths_merge_contiguous_false_matches_fixture_highlighting_block() {
    let (app, _dir) = true_paths_app().await;
    let (status, body) = get(
        &app,
        "select?q=body:(alpha%20beta%20gamma)&fq=id:merge1&fl=id&hl=true&hl.fl=body\
         &hl.method=original&hl.fragsize=20&hl.snippets=5&hl.mergeContiguous=false&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let expected = common::fixture("hl_merge_contiguous_false")
        .pointer("/highlighting")
        .cloned()
        .expect("fixture carries a highlighting block");
    assert_eq!(
        body.pointer("/highlighting"),
        Some(&expected),
        "hl.mergeContiguous=false must preserve separate original-highlighter fragments, got {body}"
    );
}

#[tokio::test]
async fn hl_true_paths_merge_contiguous_empty_field_is_not_a_panic() {
    let (app, _dir) = true_paths_app().await;
    let (status, body) = get(
        &app,
        "select?q=title:emptyprobe&fl=id&hl=true&hl.fl=body&hl.method=original\
         &hl.requireFieldMatch=false&hl.mergeContiguous=true&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "empty highlighted fields must not panic, got {body}"
    );
    assert_eq!(
        body.pointer("/highlighting/empty1"),
        Some(&json!({})),
        "got {body}"
    );
}

#[tokio::test]
async fn hl_true_paths_merge_contiguous_matches_fixture_highlighting_block() {
    let (app, _dir) = true_paths_app().await;
    let (status, body) = get(
        &app,
        "select?q=body:(alpha%20beta%20gamma)&fq=id:merge1&fl=id&hl=true&hl.fl=body\
         &hl.method=original&hl.fragsize=20&hl.snippets=5&hl.mergeContiguous=true&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let expected = common::fixture("hl_merge_contiguous_true")
        .pointer("/highlighting")
        .cloned()
        .expect("fixture carries a highlighting block");
    assert_eq!(
        body.pointer("/highlighting"),
        Some(&expected),
        "hl.mergeContiguous=true must merge adjacent original-highlighter fragments, got {body}"
    );
}

#[tokio::test]
async fn hl_original_preserves_case_sensitive_custom_analyzer_terms() {
    const SCHEMA: &str = r#"
[core]
name = "content"
unique_key = "id"
default_field = "body"

[[field_types]]
name = "case_text"
tokenizer = "simple"

[[fields]]
name = "id"
type = "string"
stored = true
required = true

[[fields]]
name = "title"
type = "case_text"
stored = true

[[fields]]
name = "body"
type = "case_text"
stored = true
"#;
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), SCHEMA).expect("app must build");
    let (status, body) = post_docs(
        &app,
        &json!([{"id":"case1","title":"Alpha","body":"Alpha"}]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "indexing must succeed, got {body}");

    let (status, body) = get(
        &app,
        "select?q=title:Alpha&fl=id&hl=true&hl.fl=body&hl.method=original\
         &hl.requireFieldMatch=false&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        body.pointer("/highlighting/case1/body/0"),
        Some(&json!("<em>Alpha</em>")),
        "original highlighting must preserve analyzer case, got {body}"
    );
}
