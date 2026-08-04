//! `/wayfinder/{core}/update/extract` Solr-Cell indexing path (issue #259).
//!
//! The companion to `extract_route.rs`: where that file covers #258's
//! `extractOnly=true` extraction response, this one covers #259's
//! `extractOnly` absent/false indexing path — `literal.*`/`fmap.*`/`uprefix`/
//! `lowernames`/`captureAttr` applied to the extraction, indexed through the
//! same commit path `/update` uses, answering the bare `responseHeader`
//! envelope (`extract_html_index.json`).
//!
//! ## The documented body/links divergence
//!
//! The indexed document's `body` and `links` come from Wayfinder's OWN
//! extractors and so diverge from the captured select fixture
//! (`extract_html_select.json`):
//!
//! - **`body`**: Wayfinder's `body_text` (its HTML text form), not Tika's
//!   content-field serialization (which keeps title + a structure-dependent
//!   whitespace layout Wayfinder does not replicate).
//! - **`links`**: the real `<a>` attribute values only. PRD divergence 10
//!   forbids fabricating Tika's injected `shape="rect"`, so `links` lacks the
//!   `"rect"` the captured fixture carries.
//!
//! Both are asserted against Wayfinder's documented behaviour below, and each
//! test that pins a value also proves the captured fixture still genuinely
//! differs — so the divergence can never silently start matching without a
//! test failing and naming it.

mod common;

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;

use common::diff::{diff, normalize};
use common::{fixture, request, request_multipart};

/// The schema the Search-API configset indexes extracted content against:
/// `id` (unique key), `body` (the extracted text via `fmap.content=body`),
/// `links` (captured `<a>` attributes via `fmap.a=links`), and `category`
/// (an extra multi-valued field for the `literal.*` test). Mirrors the
/// `add-field` calls in `solr-ref/capture.sh`'s #258 block, plus `category`.
const INDEX_SCHEMA_TOML: &str = r#"
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
name = "links"
type = "string"
stored = true
multi_valued = true

[[fields]]
name = "category"
type = "string"
stored = true
multi_valued = true
"#;

fn extract_inputs_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("solr-ref/extract-inputs")
}

fn input_bytes(name: &str) -> Vec<u8> {
    let path = extract_inputs_dir().join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read extract input {}: {e}", path.display()))
}

async fn index_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("create temp dir");
    let app = common::app_with_schema(dir.path(), INDEX_SCHEMA_TOML).expect("app must build");
    (app, dir)
}

/// Wayfinder's `body_text` for `sample.html`, derived from the
/// `extract_html_only_text` fixture rather than typed by hand: that fixture's
/// `file` is `"\n" * 13 + title + "\n\n" + body_text` (see
/// `ExtractRender::text`), so `body_text` is the tail after the title block.
/// CLAUDE.md's "expected values come from fixtures" rule is honoured by
/// deriving it here instead of asserting what the implementation happens to
/// emit today.
fn sample_html_body_text() -> String {
    let text_fixture = fixture("extract_html_only_text");
    let file = text_fixture["file"]
        .as_str()
        .expect("extract_html_only_text must have a file string");
    let title = text_fixture["file_metadata"]
        .as_array()
        .and_then(|arr| {
            arr.windows(2)
                .find(|w| w[0].as_str() == Some("dc:title"))
                .and_then(|w| w[1].as_array())
                .and_then(|vals| vals.first())
                .and_then(Value::as_str)
        })
        .expect("extract_html_only_text must carry a dc:title");
    // Strip the 13 leading newlines, then the `title + "\n\n"` head block; the
    // remainder is exactly the HTML extractor's `body_text`.
    let after_newlines = file
        .strip_prefix(&"\n".repeat(13))
        .expect("extract_html_only_text file must open with 13 newlines");
    after_newlines
        .strip_prefix(&format!("{title}\n\n"))
        .expect("the title block must follow the leading newlines")
        .to_string()
}

// --- the captured index + select pair ---------------------------------------

/// The index request is `extract_html_index.json`'s exact capture. Its
/// response is the bare `{"responseHeader":{"status":0,"QTime":…}}` envelope,
/// which Wayfinder matches exactly (the divergence is only in the follow-up
/// select's document values).
#[tokio::test]
async fn index_response_matches_extract_html_index_fixture() {
    let (app, _dir) = index_app().await;
    let bytes = input_bytes("sample.html");
    let (status, body) = request_multipart(
        &app,
        "content/update/extract?literal.id=extract-html-captured&fmap.content=body&commit=true&resource.name=sample.html&wt=json",
        "file",
        "sample.html",
        "text/html",
        &bytes,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the indexing path must answer 200, got {status}: {body}"
    );
    let expected = normalize(fixture("extract_html_index"));
    let actual = normalize(body);
    let report = diff(&expected.value, &actual.value);
    assert!(
        report.diffs.is_empty(),
        "the index response must be the bare responseHeader matching extract_html_index.json \
         (after normalize()), diffs: {:?}",
        report.diffs
    );
    // An `/update` path never echoes params — the responseHeader has no
    // `params` key, matching the fixture.
    assert!(
        actual.value["responseHeader"].get("params").is_none(),
        "the index responseHeader must not echo params, got: {actual:?}"
    );
}

/// Index then select: the indexed doc carries the literal `id`, the extracted
/// content under `body`, and the captured `<a>` `href` under `links`. The
/// `body`/`links` values are Wayfinder's own (see the file-level divergence
/// note); the assertions prove both that the doc indexed correctly AND that
/// the captured fixture still genuinely differs.
#[tokio::test]
async fn index_then_select_returns_the_extracted_document() {
    let (app, _dir) = index_app().await;
    let bytes = input_bytes("sample.html");
    let (status, _) = request_multipart(
        &app,
        "content/update/extract?literal.id=extract-html-captured&fmap.content=body&commit=true&resource.name=sample.html&wt=json",
        "file",
        "sample.html",
        "text/html",
        &bytes,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, sel) = request(
        &app,
        "GET",
        "select?q=id:extract-html-captured&fl=id,body,links&wt=json",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "select after index: {sel}");
    assert_eq!(
        sel["response"]["numFound"], 1,
        "the indexed doc must be found"
    );
    let doc = &sel["response"]["docs"][0];
    assert_eq!(
        doc["id"], "extract-html-captured",
        "id comes from literal.id"
    );

    // `body` is Wayfinder's body_text (fmap.content=body), derived from the
    // extract_html_only_text fixture.
    assert_eq!(
        doc["body"],
        json!(sample_html_body_text()),
        "body is Wayfinder's extracted text, not Tika's content-field serialization"
    );

    // `links` is the real <a> href only — captureAttr + fmap.a=links with NO
    // fabricated shape="rect" (PRD divergence 10).
    assert_eq!(
        doc["links"],
        json!(["https://example.test/doc"]),
        "links carries the real href values only"
    );

    // Prove the divergence from the captured select is real, not hidden: the
    // fixture's body and links genuinely differ. If Wayfinder ever matched,
    // these would fail and name the divergence for review.
    let fixture_doc = &fixture("extract_html_select")["response"]["docs"][0];
    assert_ne!(
        fixture_doc["body"], doc["body"],
        "the captured body must still differ from Wayfinder's, or PRD divergence 10's \
         body half is stale and the test's divergence claim is void"
    );
    assert_ne!(
        fixture_doc["links"], doc["links"],
        "the captured links (with Tika's shape=\"rect\") must still differ from \
         Wayfinder's, or this divergence entry is stale"
    );
}

// --- literal.* / fmap.* / captureAttr / uprefix semantics -------------------

/// `literal.<field>` becomes a document field value, exactly as Solr Cell
/// applies it. `literal.category=attachments` lands in the multi-valued
/// `category` field and is returned by a follow-up select.
#[tokio::test]
async fn literal_field_becomes_a_document_field() {
    let (app, _dir) = index_app().await;
    let bytes = input_bytes("sample.html");
    let (status, _) = request_multipart(
        &app,
        "content/update/extract?literal.id=lit-1&literal.category=attachments&fmap.content=body&commit=true&wt=json",
        "file",
        "sample.html",
        "text/html",
        &bytes,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, sel) = request(
        &app,
        "GET",
        "select?q=id:lit-1&fl=id,category&wt=json",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sel}");
    let doc = &sel["response"]["docs"][0];
    assert_eq!(doc["id"], "lit-1");
    assert_eq!(
        doc["category"],
        json!(["attachments"]),
        "literal.category must index into the category field"
    );
}

/// `captureAttr=false` suppresses captured attribute fields: with the default
/// `fmap.a=links`, `links` is populated when `captureAttr` is on (the default)
/// and absent when a request turns it off. Mutation guard for the
/// `captureAttr` gate in `solr_cell_fields`.
#[tokio::test]
async fn capture_attr_false_omits_captured_attribute_fields() {
    let (app, _dir) = index_app().await;
    let bytes = input_bytes("sample.html");

    // Default (captureAttr=true): links is populated.
    let (_, _) = request_multipart(
        &app,
        "content/update/extract?literal.id=cap-on&fmap.content=body&commit=true&wt=json",
        "file",
        "sample.html",
        "text/html",
        &bytes,
    )
    .await;
    let (_, sel_on) = request(&app, "GET", "select?q=id:cap-on&fl=id,links&wt=json", None).await;
    assert_eq!(
        sel_on["response"]["docs"][0]["links"],
        json!(["https://example.test/doc"]),
        "captureAttr default (true) must populate links"
    );

    // captureAttr=false: links is absent (the <a> attributes were not captured).
    let (status, _) = request_multipart(
        &app,
        "content/update/extract?literal.id=cap-off&captureAttr=false&fmap.content=body&commit=true&wt=json",
        "file",
        "sample.html",
        "text/html",
        &bytes,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, sel_off) = request(&app, "GET", "select?q=id:cap-off&fl=id,links&wt=json", None).await;
    assert!(
        sel_off["response"]["docs"][0].get("links").is_none()
            || sel_off["response"]["docs"][0]["links"]
                .as_array()
                .is_some_and(|a| a.is_empty()),
        "captureAttr=false must not populate links, got: {sel_off}"
    );
}

/// With `uprefix` set (the default `ignored_`), an extracted field renamed to
/// a name the schema does not declare is dropped — the `ignored_*` net effect
/// the Search-API configset relies on — rather than erroring. Mutation guard
/// for the `uprefix`-drop branch: removing it makes this 400.
#[tokio::test]
async fn uprefix_drops_extracted_fields_that_do_not_resolve() {
    let (app, _dir) = index_app().await;
    let bytes = input_bytes("sample.html");
    // fmap.content=no_such_field renames the extracted content to a field the
    // schema does not declare; uprefix=ignored_ (default) must drop it.
    let (status, _) = request_multipart(
        &app,
        "content/update/extract?literal.id=up-1&fmap.content=no_such_field&commit=true&wt=json",
        "file",
        "sample.html",
        "text/html",
        &bytes,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an uprefix'd unknown field must be dropped, not error"
    );

    let (_, sel) = request(&app, "GET", "select?q=id:up-1&fl=id,body&wt=json", None).await;
    let doc = &sel["response"]["docs"][0];
    assert_eq!(doc["id"], "up-1");
    assert!(
        doc.get("body").is_none(),
        "the dropped field must not appear, got: {sel}"
    );
}

/// With `uprefix` unset (empty), an unknown field is NOT dropped — it passes
/// through to the index path, which errors exactly as strict Solr
/// (`-Dupdate.autoCreateFields=false`) does. Mutation guard for the
/// `uprefix_set` condition: its inverse is the test above.
#[tokio::test]
async fn uprefix_unset_errors_on_an_unknown_field() {
    let (app, _dir) = index_app().await;
    let bytes = input_bytes("sample.html");
    let (status, body) = request_multipart(
        &app,
        "content/update/extract?literal.id=up-2&fmap.content=no_such_field&uprefix=&commit=true&wt=json",
        "file",
        "sample.html",
        "text/html",
        &bytes,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "uprefix unset + an unknown field must 400, got {status}: {body}"
    );
    assert_eq!(body["error"]["code"].as_i64(), Some(400));
}

// --- commit semantics (same path as /update) --------------------------------

/// Without `commit`/`softCommit`, the indexed document is pending and not yet
/// visible to a follow-up select — exactly as `/update` behaves before a
/// commit lands. Mutation guard for routing through the shared commit path.
#[tokio::test]
async fn index_without_commit_is_not_yet_visible() {
    let (app, _dir) = index_app().await;
    let bytes = input_bytes("sample.html");
    let (status, _) = request_multipart(
        &app,
        "content/update/extract?literal.id=nocommit-1&fmap.content=body&wt=json",
        "file",
        "sample.html",
        "text/html",
        &bytes,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, sel) = request(&app, "GET", "select?q=id:nocommit-1&wt=json", None).await;
    assert_eq!(
        sel["response"]["numFound"], 0,
        "a doc indexed without commit must not be visible yet, got: {sel}"
    );
}

/// `softCommit=true` makes the doc visible immediately, exactly as `/update`
/// (Wayfinder's softCommit is a hard commit + reload; wire-visible behaviour
/// matches Solr). Pairs with the test above to prove the commit param, not
/// something else, gates visibility.
#[tokio::test]
async fn soft_commit_makes_the_doc_visible() {
    let (app, _dir) = index_app().await;
    let bytes = input_bytes("sample.html");
    let (status, _) = request_multipart(
        &app,
        "content/update/extract?literal.id=soft-1&fmap.content=body&softCommit=true&wt=json",
        "file",
        "sample.html",
        "text/html",
        &bytes,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, sel) = request(&app, "GET", "select?q=id:soft-1&wt=json", None).await;
    assert_eq!(
        sel["response"]["numFound"], 1,
        "softCommit=true must make the doc visible, got: {sel}"
    );
}

/// An invalid commit boolean 400s (issue #187's shared parser), rather than
/// being silently read as `false` — same validation `/update` does at entry.
#[tokio::test]
async fn an_invalid_commit_boolean_is_a_400() {
    let (app, _dir) = index_app().await;
    let bytes = input_bytes("sample.html");
    let (status, body) = request_multipart(
        &app,
        "content/update/extract?literal.id=b-1&commit=maybe&fmap.content=body&wt=json",
        "file",
        "sample.html",
        "text/html",
        &bytes,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "commit=maybe must 400, got {status}: {body}"
    );
    assert_eq!(body["error"]["code"].as_i64(), Some(400));
}

// --- param allowlist under strict_params ------------------------------------

/// Under `strict_params=true`, the `literal.*` and `fmap.*` prefix families
/// are accepted (not 400'd as unknown params), while a genuinely unknown param
/// still is. Mutation guard for `check_params`'s trailing-dot prefix match.
#[tokio::test]
async fn strict_params_accepts_literal_and_fmap_families() {
    let dir = TempDir::new().expect("temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, INDEX_SCHEMA_TOML).expect("write schema");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("data dir");
    let config_path = dir.path().join("wayfinder.toml");
    std::fs::write(&config_path, "strict_params = true\n").expect("write config");
    let app = wayfinder::app_with_config(&schema_path, &data_dir, &config_path)
        .expect("app must build with strict_params=true");

    let bytes = input_bytes("sample.html");
    let (status, body) = request_multipart(
        &app,
        "content/update/extract?literal.id=sp-1&fmap.content=body&commit=true&captureAttr=true&uprefix=ignored_&wt=json",
        "file",
        "sample.html",
        "text/html",
        &bytes,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "strict_params must accept literal.*/fmap.*/captureAttr/uprefix, got {status}: {body}"
    );

    // A genuinely unknown param still 400s.
    let (status, body) = request_multipart(
        &app,
        "content/update/extract?literal.id=sp-2&fmap.content=body&commit=true&bogus=1&wt=json",
        "file",
        "sample.html",
        "text/html",
        &bytes,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an unknown param must still 400 under strict_params, got {status}: {body}"
    );
    assert_eq!(body["error"]["code"].as_u64(), Some(400));
}

// --- regression: the extractOnly path still works alongside indexing --------

/// Adding the indexing branch must not disturb #258's `extractOnly=true`
/// response. The same fixture-derived match `extract_route.rs` makes, asserted
/// here too so this file proves both modes coexist on the one route.
#[tokio::test]
async fn extract_only_path_still_returns_the_extract_envelope() {
    let (app, _dir) = index_app().await;
    let bytes = input_bytes("sample.html");
    let (status, body) = request_multipart(
        &app,
        "content/update/extract?extractOnly=true&resource.name=sample.html&wt=json",
        "file",
        "sample.html",
        "text/html",
        &bytes,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "extractOnly must still 200, got {body}"
    );
    assert!(
        body.get("file").is_some(),
        "the extractOnly response must still carry the extracted file, got: {body}"
    );
    assert!(
        body.get("file_metadata").is_some(),
        "the extractOnly response must still carry file_metadata, got: {body}"
    );
}
