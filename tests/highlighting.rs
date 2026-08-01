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

    // Issue #139: `hl.mergeContiguous` and `hl.requireFieldMatch` join the
    // rest of the implemented `hl.*` surface here. `hl.fl=*` is exercised
    // separately below (it interacts with field resolution, not just
    // `SELECT_PARAMS` membership), so this request keeps the explicit
    // `hl.fl=body` the rest of this guard already used.
    let (status, body) = get(
        &app,
        "select?q=lazy&df=body&hl=true&hl.fl=body&hl.snippets=2&hl.fragsize=50\
         &hl.method=original&hl.simple.pre=%3Cb%3E&hl.simple.post=%3C%2Fb%3E\
         &hl.mergeContiguous=false&hl.requireFieldMatch=false&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "every implemented hl.* param must pass strict mode, got body: {body}"
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
// **Open question 1 (`hl.fl=*`'s expansion):** the captured traces alone
// cannot fully disambiguate "every *text*-typed field" from "the query's
// `qf`/`df` set", because every text field the traced `search_api_solr`
// schema ever populates (`tm_X3b_en_body`, `tm_X3b_en_title`) is also always
// part of that query's `qf` -- see `solr-ref/search-api/trace/00002.json`
// (`hl.fl=%2A` alongside a `q` built directly from
// `tm_X3b_en_body`/`tm_X3b_und_body`/`tm_X3b_en_title`/`tm_X3b_und_title`,
// with the response's `highlighting` block keying only the two `en_` fields
// -- but that document simply has no `und_` fields stored at all, so their
// absence proves nothing either way) and every `q=*:*` trace with
// `hl.fl=*` (e.g. `00013.json`) has no term overlap in any field, so every
// doc's entry is `{}` regardless of which fields the wildcard resolved to.
// Two facts settle it anyway, independent of that ambiguity:
//   (a) The traced core *does* set a `df`, to `id`
//       (`solr-ref/search-api/configset/solrconfig_extra.xml:113`, the
//       `<requestHandler name="/select">` defaults block -- not
//       `solrconfig_query.xml`, which is the file it would be natural to
//       look in). So a real `df` is in force on every one of these requests,
//       and yet no wildcard trace ever keys `highlighting` on `id`. That is
//       *stronger* than an absent `df` would have been: the fallback
//       candidate exists and is still never used, which rules the "defaults
//       to df" reading out rather than merely leaving it untested. See
//       finding 94 in `docs/solr-ref-findings.md`.
//   (b) Real Solr's own wildcard expansion (`DefaultSolrHighlighter`'s
//       `getHighlightFields`, `SolrPluginUtils.expandWildcardsInField`)
//       matches `*` against the schema's field *names*, not the query's
//       `qf`/`df` set -- fields that come back from that expansion but
//       cannot be analyzed (non-text) simply never produce a snippet, which
//       is indistinguishable on the wire from "no term overlap" (finding
//       52's `{}` shape covers both). That silent-skip, not a 400, is the
//       behaviour worth pinning: Wayfinder's own explicit-field path 400s a
//       named non-text `hl.fl` field
//       (`hl_non_text_field_is_400`/`hl_undefined_field_is_400_...` above),
//       so a naive `*` expansion that ran every schema field through that
//       same check would 400 on any schema carrying a field that check
//       rejects -- e.g. `numeric_app`'s `views` (long).
//
//       Note this check does *not* reject `common::SCHEMA_TOML`'s `category`:
//       `string` resolves to `ResolvedType::Str`, which maps to
//       `ValueKind::Text` (`src/schema.rs:414`), so `check_highlightable`
//       returns `Ok` for it and `hl.fl=category` is a 200 that really does
//       emit `{"doc4":{"category":["<em>animals</em>"]}}`. Wayfinder's
//       wildcard exclusion of `string`/`keyword` is therefore a deliberate
//       divergence from its own explicit-field path, not a 400 being dodged
//       -- see `highlightable_fields`' doc comment in `src/highlight.rs`.
// So: `hl.fl=*` expands to every *highlightable* (analyzed text) field in the
// schema, never errors on a schema that also has non-text fields, and never
// surfaces a non-analyzed field in `highlighting` even when that field is the
// one that actually matched the base query.

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

/// **Pins an inference, not a captured shape.** No fixture discriminates
/// this: the traced corpus never runs a query term that matches a stored
/// `StrField`'s value under `hl.fl=*` (finding 94's closing note --
/// `sm_context_tags` is the only genuinely stored non-`tm_` field in those
/// docs, and no captured `q` ever hits one of its values). What real Solr
/// would emit here is therefore unsettled by ground truth; this test pins
/// *Wayfinder's* deliberate divergence, and it is the only thing that does.
///
/// The query is `q=category:animals` -- the term is in `q`, deliberately not
/// in an `fq`. That distinction is the whole point. With the term in an `fq`
/// the highlighter sees no query term for `category` at all, so `{}`-per-doc
/// comes out whether or not the wildcard swept `category` up, and the test
/// would pass against an implementation that included `StrField`s. With the
/// term in `q` the two behaviours separate:
///
/// - today (`is_raw_string` excluded from the expansion set):
///   `{"doc1":{},"doc4":{}}`
/// - dropping only that filter, i.e. matching Solr's StrField-inclusive
///   stored-field set: `{"doc1":{},"doc4":{"category":["<em>animals</em>"]}}`
///
/// so asserting no doc carries a `category` key is what makes the exclusion
/// testable at all. (`category` is genuinely highlightable by the
/// explicit-field path -- `hl.fl=category` on this same query is a 200 that
/// emits that very snippet -- so this is not the wildcard dodging a 400.)
/// The `{}`-per-doc shape itself still matches `hl_no_field_match`'s
/// captured block, which is asserted against the fixture rather than a
/// literal.
#[tokio::test]
async fn hl_wildcard_fl_does_not_error_on_a_matched_non_text_field() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=category:animals&hl=true&hl.fl=*&wt=json").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "hl.fl=* must not 400 just because a non-analyzed field (category) is \
         what actually matched the base query, got {body}"
    );
    let highlighting = body
        .pointer("/highlighting")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("response carries a highlighting block, got {body}"));
    for (doc, entry) in highlighting {
        assert!(
            entry.get("category").is_none(),
            "hl.fl=* must never surface the non-analyzed `category` field, but \
             `{doc}` carries one -- the wildcard expansion has stopped \
             filtering out raw string fields, got {body}"
        );
    }
    let expected = common::fixture("hl_no_field_match")
        .pointer("/highlighting")
        .cloned()
        .expect("hl_no_field_match fixture carries a highlighting block");
    assert_eq!(
        body.pointer("/highlighting"),
        Some(&expected),
        "hl.fl=* must reproduce hl_no_field_match's {{}}-per-doc shape, got {body}"
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
        "category is non-text and must never appear as a highlighting key, got {entry:?}"
    );
}

/// Open question 2: is `hl.requireFieldMatch=false`/`hl.mergeContiguous=false`
/// already Wayfinder's behaviour, making this an allowlist-only change?
/// Solr's own documented default for both is `false`
/// (`hl.requireFieldMatch`: a field highlights even without a query match in
/// that field, unless `true`; `hl.mergeContiguous`: adjacent fragments are
/// not merged, unless `true`) -- so the module's explicit `false` values
/// never ask Wayfinder to do anything its current, unconditional behaviour
/// does not already do: `hl_no_field_match_matches_fixture_and_has_empty_object_shape`
/// above already pins that a doc matching through a non-highlighted field
/// still gets an entry (i.e. `requireFieldMatch=false`'s behaviour), and
/// `src/highlight.rs` has no fragment-merging logic to turn off in the first
/// place. So this is an allowlist-only change for the `false` path: passing
/// these params must be a no-op, not a behaviour change. (Open question 5,
/// `hl.requireFieldMatch=true`'s real per-field filtering, has no captured
/// fixture to derive an expected shape from and is not exercised here --
/// see the handoff notes.)
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
/// (`hl.fl=*`, the two now-allowlisted no-op params, `hl.fragsize=0`
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
