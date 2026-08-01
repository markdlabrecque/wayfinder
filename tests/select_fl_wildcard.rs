//! Issue #188 — `fl=*` is a wildcard on `/select`, not a literal field name.
//!
//! `CoreIndex::render_doc` (`src/core_index.rs`, *not* `src/lib.rs` — the
//! ticket text misplaces it) filters `fl` as a literal-name allowlist:
//! `.filter(|f| fl.is_none_or(|fl| fl.iter().any(|name| name == &f.name)))`.
//! No schema field is ever named `*`, so `fl=*` matches nothing and `fl=*,score`
//! comes back as `{"score": ...}` with every real field dropped.
//!
//! ## Ground truth, and what each expectation is derived from
//!
//! There is no `solr-ref/responses/select_fl_*wildcard*.json`, so nothing here
//! is derived from a *dedicated* `/select` capture. Three committed artifacts
//! settle it between them:
//!
//! 1. **`select_all.json`** (`select?q=*:*&rows=10&wt=json`, no `fl`) is real
//!    Solr's every-stored-field rendering of the canonical 5-doc corpus, in
//!    Solr's own key order (`id, body, category, _version_, _root_` — the last
//!    two are the internal fields Wayfinder deliberately lacks, findings fact 9
//!    / PRD §7, dropped by `common::normalize_envelope`). Solr's `fl=*` means
//!    "every field the default rendering would return", so `fl=*` must produce
//!    exactly this response.
//! 2. **`solr-ref/responses/mlt_fl_wildcard_score.json`** (captured for #141,
//!    `fl=*,score`) shows the composition: every stored field *plus* `score`,
//!    with `score` last — after `_version_`/`_root_`, i.e. after everything.
//! 3. **`solr-ref/search-api/trace/00010.json`** is the `/select`-side witness
//!    for the same shape (`fl=%2A%2Cscore`): every stored field, including
//!    every *dynamic* field (`ss_field_sku`, `sm_field_keywords`,
//!    `its_field_rating`, ...), and `score` last. This is why the wildcard has
//!    to reach `render_doc`'s dynamic-field loop too, not just its declared
//!    `[[fields]]` loop.
//! 4. **`select_fl_reversed.json`** (`fl=body,id`) settles the ordering
//!    question the task spec asks about: Solr's doc key order is the schema's,
//!    *not* `fl`'s — `fl=body,id` still renders `id` before `body`. See
//!    `tests/json_key_order.rs::select_fl_reversed_fixture_discriminates_input_order_from_fl_order`.
//!    So `fl=score,*` cannot differ from `fl=*,score`, and this file asserts
//!    the two are interchangeable rather than guessing at a wildcard-position
//!    rule. **No fixture sends `fl=score,*` literally**; that specific
//!    permutation is an inference from `select_fl_reversed` (fl order is not
//!    doc key order) plus finding 24 (Solr appends its pseudo-fields last), not
//!    a capture.
//!
//! Sibling coverage: the `/mlt` half of the same `render_doc` gap, and the
//! fixture assertion for `mlt_fl_wildcard_score`, live in `tests/mlt.rs`. The
//! false-positive-green coverage probe lives in `src/coverage.rs`'s own test
//! module.

mod common;

use axum::Router;
use axum::http::StatusCode;
use common::key_order::{KeyOrder, get_text, is_alphabetical};
use common::{CORE, app_with_schema, fixture, get, normalize_envelope, post_docs};
use serde_json::{Value, json};
use tempfile::TempDir;

/// `select_all.json`'s `/response` object, with `_version_`/`_root_` stripped
/// from every doc — real Solr's "every stored field" rendering of the canonical
/// corpus, and therefore what `fl=*` must return.
fn select_all_response() -> Value {
    normalize_envelope(fixture("select_all"))
        .get("response")
        .cloned()
        .expect("select_all.json must have a `response` object")
}

/// `select_all.json`'s `response.docs[0]` keys in captured order, minus the
/// internal fields Wayfinder has no equivalent for. `["id", "body", "category"]`
/// — deliberately read out of the fixture text rather than hardcoded, so a
/// re-capture that reordered them moves the expectation instead of failing it.
fn select_all_doc_keys(index: usize) -> Vec<String> {
    let keys = common::key_order::fixture_key_order("select_all")
        .keys_at(&format!("response.docs[{index}]"), "select_all doc keys");
    keys.into_iter()
        .filter(|k| k != "_version_" && k != "_root_")
        .collect()
}

/// `response.docs[<index>]` keys of a raw response body, in document order.
fn doc_keys(text: &str, index: usize, what: &str) -> Vec<String> {
    KeyOrder::parse(text).keys_at(&format!("response.docs[{index}]"), what)
}

/// `response.docs[0]` keys of `solr-ref/search-api/trace/00010.json`, in
/// captured order. That trace is a real Drupal `search_api_solr` `/select` with
/// `fl=*,score` against a corpus full of *dynamic* fields, so unlike
/// `select_all.json` / `mlt_fl_wildcard_score.json` (whose corpora have no
/// dynamic fields at all) it is the one committed artifact that discriminates
/// `score`-appended-last from `score`-before-the-dynamic-fields.
///
/// The trace stores the Solr response as a JSON *string* under
/// `response.body`, so the order has to be recovered in two passes: parse the
/// envelope for the string, then `KeyOrder::parse` the string itself.
fn trace_00010_doc_keys() -> Vec<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("solr-ref/search-api/trace/00010.json");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let trace: Value = serde_json::from_str(&text).expect("trace 00010 must be valid JSON");
    let body = trace
        .pointer("/response/body")
        .and_then(Value::as_str)
        .expect("trace 00010 must carry `response.body` as a string");
    doc_keys(body, 0, "trace/00010 response body")
}

// --- `fl=*` alone ----------------------------------------------------------

#[tokio::test]
async fn select_fl_star_alone_returns_every_stored_field() {
    let (app, _dir) = common::indexed_app().await;
    let (status, body) = get(&app, "select?q=*:*&rows=10&fl=*&wt=json").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        normalize_envelope(body).get("response"),
        Some(&select_all_response()),
        "issue #188: `fl=*` must expand to every stored field, producing exactly the response \
         real Solr gives for the same query with no `fl` at all (`select_all.json`). Today \
         `render_doc` treats `*` as a literal field name that matches nothing, so every doc \
         comes back as `{{}}`"
    );
}

#[tokio::test]
async fn select_fl_star_alone_keeps_solrs_doc_key_order() {
    // Value equality above cannot see this: `serde_json` is built with
    // `preserve_order`, whose `Map` compares as a map regardless of key order
    // (see `common::normalize_envelope`'s doc comment), so the order has to be
    // read off the raw response text -- what `common::key_order` exists for.
    let expected = select_all_doc_keys(0);
    assert!(
        !is_alphabetical(&expected),
        "vacuity guard: select_all.json's doc key order ({expected:?}) must not be alphabetical, \
         or `actual == fixture` would prove nothing about ordering"
    );

    let (app, _dir) = common::indexed_app().await;
    let (status, text) = get_text(&app, CORE, "select?q=*:*&rows=10&fl=*&wt=json").await;
    assert_eq!(status, StatusCode::OK, "got {text}");
    assert_eq!(
        doc_keys(&text, 0, "fl=* response"),
        expected,
        "`fl=*` must render fields in the schema order real Solr uses (`select_all.json`), \
         got: {text}"
    );
}

// --- `fl=*,score` ----------------------------------------------------------

#[tokio::test]
async fn select_fl_star_plus_score_returns_every_stored_field_plus_score() {
    let (app, _dir) = common::indexed_app().await;
    let (status, body) = get(&app, "select?q=*:*&rows=10&fl=*,score&wt=json").await;
    assert_eq!(status, StatusCode::OK, "got {body}");

    let normalized = normalize_envelope(body);
    let docs = normalized
        .pointer("/response/docs")
        .and_then(Value::as_array)
        .expect("response.docs must be an array");
    let expected_docs = select_all_response();
    let expected_docs = expected_docs
        .get("docs")
        .and_then(Value::as_array)
        .expect("select_all docs");
    assert_eq!(
        docs.len(),
        expected_docs.len(),
        "`fl=*,score` must return the same doc set as `fl=*`, got: {normalized}"
    );

    for (actual, expected) in docs.iter().zip(expected_docs) {
        let mut stripped = actual.clone();
        let score = stripped
            .as_object_mut()
            .expect("each doc must be an object")
            .remove("score");
        assert!(
            score.is_some_and(|s| s.is_number()),
            "`fl=*,score` must still carry a numeric `score` on every doc \
             (`mlt_fl_wildcard_score.json`, `trace/00010.json`), got: {actual}"
        );
        assert_eq!(
            &stripped, expected,
            "issue #188: with `score` removed, `fl=*,score` must be byte-equal to real Solr's \
             every-stored-field rendering (`select_all.json`). Today `render_doc` drops every \
             field but `score`"
        );
    }
}

#[tokio::test]
async fn select_fl_star_plus_score_puts_score_last() {
    // `mlt_fl_wildcard_score.json`'s doc keys are
    // `id, body, category, _version_, _root_, score` and `trace/00010.json`
    // likewise ends every doc with `score` -- Solr appends its pseudo-fields
    // after the real ones (finding 24). With the two internal fields Wayfinder
    // lacks removed, that is `select_all`'s key order with `score` appended.
    //
    // Note the limit of this corpus: it has no dynamic fields, so this test
    // cannot tell "score last" from "score after the declared fields, before
    // the dynamic ones". `select_fl_star_plus_score_puts_score_after_dynamic_fields`
    // below is the one that discriminates.
    let mut expected = select_all_doc_keys(0);
    expected.push("score".to_string());

    let (app, _dir) = common::indexed_app().await;
    let (status, text) = get_text(&app, CORE, "select?q=*:*&rows=10&fl=*,score&wt=json").await;
    assert_eq!(status, StatusCode::OK, "got {text}");
    assert_eq!(
        doc_keys(&text, 0, "fl=*,score response"),
        expected,
        "`fl=*,score` must render every stored field in schema order and append `score` last, \
         got: {text}"
    );
}

// --- wildcard position, and duplicate suppression --------------------------

#[tokio::test]
async fn select_fl_score_then_star_renders_identically_to_star_then_score() {
    // The ordering question the task spec raises. No fixture sends
    // `fl=score,*`, but `select_fl_reversed.json` (`fl=body,id`, still rendered
    // `id, body`) settles the general rule: `fl`'s own order does not drive doc
    // key order, so no permutation of the same `fl` members may differ.
    let (app, _dir) = common::indexed_app().await;
    let (star_first_status, star_first) =
        get_text(&app, CORE, "select?q=*:*&rows=10&fl=*,score&wt=json").await;
    let (score_first_status, score_first) =
        get_text(&app, CORE, "select?q=*:*&rows=10&fl=score,*&wt=json").await;
    assert_eq!(star_first_status, StatusCode::OK, "got {star_first}");
    assert_eq!(score_first_status, StatusCode::OK, "got {score_first}");

    // Vacuity guard first: pre-#188 both permutations render `{"score": ...}`
    // and nothing else, so bare equality between them passes on the *broken*
    // implementation. Pin the shared shape to the real one before comparing.
    let mut expected = select_all_doc_keys(0);
    expected.push("score".to_string());
    assert_eq!(
        doc_keys(&star_first, 0, "fl=*,score response"),
        expected,
        "vacuity guard: the shape both permutations must agree on is every stored field plus \
         `score`, not the pre-#188 `score`-only doc, got: {star_first}"
    );
    assert_eq!(
        doc_keys(&score_first, 0, "fl=score,* response"),
        doc_keys(&star_first, 0, "fl=*,score response"),
        "`fl=score,*` must render the same keys in the same order as `fl=*,score` -- `fl` order \
         is not doc key order (`select_fl_reversed.json`)"
    );
    assert_eq!(
        normalize_envelope(serde_json::from_str::<Value>(&score_first).expect("valid JSON"))
            .get("response"),
        normalize_envelope(serde_json::from_str::<Value>(&star_first).expect("valid JSON"))
            .get("response"),
        "`fl=score,*` and `fl=*,score` must produce the same `response` object"
    );
}

#[tokio::test]
async fn select_fl_star_plus_a_named_field_does_not_duplicate_it() {
    let (app, _dir) = common::indexed_app().await;
    let (status, text) = get_text(&app, CORE, "select?q=*:*&rows=10&fl=*,body&wt=json").await;
    assert_eq!(status, StatusCode::OK, "got {text}");

    let keys = doc_keys(&text, 0, "fl=*,body response");
    let mut deduped = keys.clone();
    deduped.dedup();
    assert_eq!(
        keys, deduped,
        "`fl=*,body` must not emit `body` twice -- `*` already covers it, got: {text}"
    );
    assert_eq!(
        keys,
        select_all_doc_keys(0),
        "`fl=*,body` must render exactly what `fl=*` renders, in the same order (naming a field \
         the wildcard already covers is a no-op), got: {text}"
    );
}

// --- what `*` must NOT pick up ---------------------------------------------

/// `id` (stored) plus a stored dynamic rule, an unstored declared field, and an
/// unstored dynamic rule. `trace/00010.json` shows real Solr's `fl=*,score`
/// returning dynamic fields (`ss_field_sku`, `sm_field_keywords`, ...) right
/// alongside declared ones, so `*` has to reach `render_doc`'s dynamic-field
/// loop as well as its `[[fields]]` loop. The unstored members pin the other
/// half: `*` is "every *stored* field", the same set `fl`-absent already
/// returns (`render_doc`'s `.filter(|f| f.stored)` and its `rule.stored`
/// check) -- not "every field in the schema".
const WILDCARD_SCHEMA_TOML: &str = r#"
[core]
name = "content"
unique_key = "id"
default_field = "id"

[[fields]]
name = "id"
type = "string"
stored = true
required = true
fast = true

[[fields]]
name = "secret"
type = "text_en"
stored = false

[[dynamic_fields]]
pattern = "ss_*"
type = "string"
stored = true

[[dynamic_fields]]
pattern = "hidden_*"
type = "string"
stored = false
"#;

fn wildcard_corpus() -> Value {
    json!([
        {"id": "d1", "secret": "not stored", "ss_field_sku": "ART-001", "hidden_note": "also not stored"}
    ])
}

async fn wildcard_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = app_with_schema(dir.path(), WILDCARD_SCHEMA_TOML).expect("app must build");
    let (status, body) = post_docs(&app, &wildcard_corpus()).await;
    assert_eq!(status, StatusCode::OK, "indexing must succeed, got {body}");
    (app, dir)
}

#[tokio::test]
async fn select_fl_star_expands_stored_dynamic_fields_too() {
    let (app, _dir) = wildcard_app().await;
    let (status, body) = get(&app, "select?q=id:d1&fl=*&wt=json").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let doc = body
        .pointer("/response/docs/0")
        .and_then(Value::as_object)
        .expect("one doc must come back");
    assert_eq!(
        doc.get("ss_field_sku"),
        Some(&Value::String("ART-001".to_string())),
        "issue #188: `fl=*` must expand into stored *dynamic* fields too -- \
         `trace/00010.json` returns `ss_field_sku`/`sm_field_keywords`/... for `fl=*,score`. \
         `render_doc` filters its dynamic-field loop by literal `fl` name as well, got: {body}"
    );
}

/// The non-vacuous version of `select_fl_star_plus_score_puts_score_last`.
///
/// That test runs against the canonical 5-doc corpus, which has **no dynamic
/// fields**, so "score last" and "score after the declared fields but before
/// the dynamic ones" are indistinguishable there — it passed on an
/// implementation that inserted `score` between `render_doc`'s two loops.
/// `WILDCARD_SCHEMA_TOML` is the only schema in this suite with a stored
/// dynamic rule, so this is where the position is observable: pre-fix, this
/// request rendered `["id", "score", "ss_field_sku"]`.
#[tokio::test]
async fn select_fl_star_plus_score_puts_score_after_dynamic_fields() {
    // Ground truth for the rule, read out of the trace rather than asserted
    // from memory: `score` is the final key, and the key immediately before it
    // is a *dynamic* field (`ss_search_api_language`, matched by the preset's
    // `ss_*` rule) — so the fixture really does discriminate the two placements.
    let trace_keys = trace_00010_doc_keys();
    assert_eq!(
        trace_keys.last().map(String::as_str),
        Some("score"),
        "trace/00010.json must end its doc with `score`, got: {trace_keys:?}"
    );
    let before_score = trace_keys
        .get(trace_keys.len() - 2)
        .map(String::as_str)
        .expect("trace doc must have more than one key");
    assert!(
        before_score.starts_with("ss_"),
        "vacuity guard: the key before `score` in trace/00010.json must be a dynamic-rule field, \
         or the trace would not discriminate score-last from score-before-dynamic-fields; got \
         {before_score:?} in {trace_keys:?}"
    );

    let (app, _dir) = wildcard_app().await;
    let (status, text) = get_text(&app, CORE, "select?q=id:d1&fl=*,score&wt=json").await;
    assert_eq!(status, StatusCode::OK, "got {text}");
    assert_eq!(
        doc_keys(&text, 0, "wildcard-schema fl=*,score response"),
        vec![
            "id".to_string(),
            "ss_field_sku".to_string(),
            "score".to_string()
        ],
        "`fl=*,score` must render the declared stored fields, then the stored dynamic fields, \
         then `score` last (`solr-ref/search-api/trace/00010.json`) -- not `score` between the \
         two, got: {text}"
    );
}

#[tokio::test]
async fn select_fl_star_omits_unstored_declared_and_dynamic_fields() {
    let (app, _dir) = wildcard_app().await;
    let (status, starred) = get(&app, "select?q=id:d1&fl=*&wt=json").await;
    assert_eq!(status, StatusCode::OK, "got {starred}");
    let doc = starred
        .pointer("/response/docs/0")
        .and_then(Value::as_object)
        .expect("one doc must come back");
    assert!(
        doc.get("secret").is_none(),
        "`*` is every *stored* field, not every schema field: an unstored declared field must \
         stay omitted, got: {starred}"
    );
    assert!(
        doc.get("hidden_note").is_none(),
        "an unstored *dynamic* rule's value must stay omitted under `fl=*` too, got: {starred}"
    );

    // And the positive framing of the same rule: `*` is exactly the `fl`-absent
    // field set. Asserted alongside the omissions above rather than alone,
    // because an implementation that dropped *everything* would satisfy
    // equality with an equally-broken `fl`-absent path.
    let (status, plain) = get(&app, "select?q=id:d1&wt=json").await;
    assert_eq!(status, StatusCode::OK, "got {plain}");
    assert_eq!(
        starred.pointer("/response/docs"),
        plain.pointer("/response/docs"),
        "`fl=*` must return exactly the field set `fl`-absent returns"
    );
}
