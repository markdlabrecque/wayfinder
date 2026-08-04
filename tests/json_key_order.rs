//! JSON object **key order** compatibility (issue #25).
//!
//! Solr emits `SimpleOrderedMap`/`NamedList`, so every object in its envelope
//! has a meaningful order: `responseHeader, response, facet_counts` at the top,
//! `status, QTime, params` in the header, `numFound, start, numFoundExact, docs`
//! in the response, `metadata, msg, code` in an error, `counts, gap, start, end`
//! in a range facet, and — under `json.nl=map` — the *facet* order (numeric
//! bucket order for a range, count-descending or index order for a field) as the
//! object's key order.
//!
//! `serde_json` without the `preserve_order` feature backs `Map` with a
//! `BTreeMap`, which alphabetises every object Wayfinder emits, and the existing
//! harness cannot see it: `assert_matches_fixture` and `tests/common/diff.rs`
//! both compare parsed `Value`s, and parsing throws the order away. These tests
//! read the order out of the raw response *text* instead — see
//! `tests/common/key_order.rs` for why that is not optional.
//!
//! `responseHeader.params` is exempt everywhere: Solr's echoed-param order is
//! Java `HashMap` iteration order, not request order and not alphabetical, so it
//! is not reproducible and not a contract (findings fact 6). The exemption and
//! its reason live in `key_order::EXEMPT_PATHS`.

mod common;

use std::path::{Path, PathBuf};

use axum::Router;
use axum::http::StatusCode;
use common::diff::load_manifest_errors;
use common::key_order::{
    KeyOrder, assert_same_key_order, assert_same_key_order_texts, fixture_key_order, get_text,
    is_alphabetical,
};
use common::{CORE, indexed_app, post_docs};
use serde_json::{Value, json};
use tempfile::TempDir;

fn manifest_errors_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("solr-ref/manifest-errors.tsv")
}

/// Looks up `name`'s row in `solr-ref/manifest-errors.tsv` and returns its
/// URL with the `keyorder/` core prefix stripped, so the three query
/// constants below are read out of the manifest at runtime instead of
/// hand-copied (issue #31 follow-up: "editing the manifest must not silently
/// desynchronise them").
fn keyorder_query_from_manifest(name: &str) -> String {
    let entries = load_manifest_errors(&manifest_errors_path());
    let entry = entries
        .iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("manifest-errors.tsv has no row named `{name}`"));
    entry
        .url
        .strip_prefix("keyorder/")
        .unwrap_or_else(|| {
            panic!(
                "row `{name}`'s url `{}` must start with `keyorder/`",
                entry.url
            )
        })
        .to_string()
}

// --- 0. helper self-tests -------------------------------------------------
//
// If `KeyOrder` ever stops reading the document and starts reporting some map's
// iteration order, every assertion below silently becomes a tautology. These
// four pin it. They live here rather than in `tests/common/key_order.rs` so they
// run once, not once per integration binary.

#[test]
fn helper_records_document_order() {
    assert_eq!(
        KeyOrder::parse(r#"{"b":1,"a":2}"#).keys().unwrap(),
        vec!["b".to_string(), "a".to_string()],
        "KeyOrder must report `b` before `a`, exactly as the text does"
    );
}

#[test]
fn helper_is_order_sensitive() {
    let one = KeyOrder::parse(r#"{"outer":{"z":1,"y":2},"tail":[]}"#);
    let two = KeyOrder::parse(r#"{"outer":{"y":2,"z":1},"tail":[]}"#);
    assert_ne!(
        one, two,
        "two texts differing only in key order must not compare equal"
    );
    assert_eq!(one.keys_at("outer", "helper self-test"), vec!["z", "y"]);
    assert_eq!(two.keys_at("outer", "helper self-test"), vec!["y", "z"]);
}

#[test]
fn helper_walks_paths_and_arrays() {
    let k = KeyOrder::parse(r#"{"a":{"b":{"c":[{"z":1,"a":2}]}}}"#);
    assert_eq!(k.keys_at("a.b.c[0]", "helper self-test"), vec!["z", "a"]);
    assert!(
        k.at("a.b.nope").is_none(),
        "a missing path must be None, not a silently empty object"
    );
}

#[test]
fn helper_alphabetical_guard_works() {
    let sorted: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
    let unsorted: Vec<String> = ["b", "a", "c"].iter().map(|s| s.to_string()).collect();
    assert!(is_alphabetical(&sorted));
    assert!(!is_alphabetical(&unsorted));
}

// --- 1. the tripwire ------------------------------------------------------

/// The tripwire for someone dropping `serde_json`'s `preserve_order` feature
/// from `Cargo.toml`. Without it `serde_json::Map` is a `BTreeMap` and this
/// yields `["a","b"]`; with it, `["b","a"]`. Every other test in this file
/// depends on that feature being on, and each would fail with a confusing
/// order-mismatch diff instead of naming the cause — so this one names it.
#[test]
fn serde_json_is_built_with_preserve_order() {
    let v: Value = serde_json::from_str(r#"{"b":1,"a":2}"#).expect("valid JSON");
    let keys: Vec<&str> = v
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        vec!["b", "a"],
        "serde_json must be built with the `preserve_order` feature, so \
         serde_json::Map keeps insertion/document order instead of sorting keys"
    );
}

// --- 2. the key-order core ------------------------------------------------

/// Mirrors the `keyorder` Solr core created at the end of `solr-ref/capture.sh`:
/// `views` (pint, indexed+stored+docValues) and `tag` (string,
/// indexed+stored+docValues+multiValued), on the `_default` configset whose
/// `string`/`pint` types carry `docValues="true"` — hence `fast = true` here, the
/// same reasoning as `common::SCHEMA_TOML`'s `id`.
///
/// The core is named `content` because `common::CORE` is what the request
/// helpers address; the Solr-side core name (`keyorder`) only ever appears in the
/// manifest row. `tests/faceting.rs`'s `RANGE_SCHEMA_TOML` does the same for the
/// `facets` core.
///
/// `core.default_field` must name a declared field, and the captured corpus has
/// no text field, so `id` serves — every query used here is `q=*:*`, which never
/// consults it.
const KEYORDER_SCHEMA_TOML: &str = r#"
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
name = "views"
type = "int"
stored = true
fast = true

[[fields]]
name = "tag"
type = "string"
stored = true
fast = true
multi_valued = true
"#;

/// The exact 8-doc corpus `capture.sh` indexes into the `keyorder` core.
///
/// `views` spans 5..195 so a 0-200-by-10 range facet has bucket keys
/// `0,10,...,190`, where numeric order and alphabetical order differ ("100"
/// sorts before "20"). `tag` counts are apple 5, zebra 5, mango 2, banana 1, so
/// count-descending order (apple, zebra, mango, banana — apple wins the 5-5 tie
/// on index order) differs from alphabetical.
fn keyorder_corpus() -> Value {
    json!([
        {"id":"k1","views":5,  "tag":["zebra","apple"]},
        {"id":"k2","views":15, "tag":["zebra","apple"]},
        {"id":"k3","views":45, "tag":["zebra","mango"]},
        {"id":"k4","views":95, "tag":["zebra","apple"]},
        {"id":"k5","views":105,"tag":["mango","banana"]},
        {"id":"k6","views":155,"tag":["apple"]},
        {"id":"k7","views":195,"tag":["apple"]},
        {"id":"k8","views":125,"tag":["zebra"]}
    ])
}

async fn keyorder_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), KEYORDER_SCHEMA_TOML).expect("app must build");
    let (status, body) = post_docs(&app, &keyorder_corpus()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the key-order corpus must succeed, got {body}"
    );
    (app, dir)
}

/// The query from `keyorder_range_wide_map`'s row in
/// `solr-ref/manifest-errors.tsv`, minus the core prefix — derived at
/// runtime via `keyorder_query_from_manifest`, not hand-copied, so editing
/// the manifest row cannot silently desynchronise this from ground truth.
fn wide_range_query() -> String {
    keyorder_query_from_manifest("keyorder_range_wide_map")
}

/// The query from `keyorder_facet_field_map`'s manifest-errors row.
fn field_map_query() -> String {
    keyorder_query_from_manifest("keyorder_facet_field_map")
}

/// The query from `keyorder_facet_field_map_index`'s manifest-errors row.
fn field_map_index_query() -> String {
    keyorder_query_from_manifest("keyorder_facet_field_map_index")
}

/// Asserts the keys at `path` in `text` match the same path in `fixture`, and
/// (when `must_differ_from_alphabetical`) that the fixture's own order is *not*
/// alphabetical — so the test cannot pass vacuously if the fixture were ever
/// re-captured wrongly, or if `serde_json` reverted to sorting.
fn assert_keys_match_fixture(
    text: &str,
    fixture: &str,
    path: &str,
    must_differ_from_alphabetical: bool,
) {
    let expected = fixture_key_order(fixture).keys_at(path, &format!("fixture {fixture}"));
    if must_differ_from_alphabetical {
        assert!(
            !is_alphabetical(&expected),
            "fixture {fixture} at `{path}` is in alphabetical order ({expected:?}), so \
             comparing against it cannot detect the alphabetising bug - the fixture is wrong"
        );
    }
    let actual = KeyOrder::parse(text).keys_at(path, "Wayfinder response");
    assert_eq!(
        actual, expected,
        "key order at `{path}` must match fixture {fixture}"
    );
}

// --- 3. json.nl=map bucket / term order -----------------------------------

/// The decisive case: a 0-200-by-10 range facet under `json.nl=map`. Solr's
/// bucket keys are `0,10,20,...,190`; alphabetised they would be
/// `0,10,100,110,...,190,20,...`.
#[tokio::test]
async fn wide_range_json_nl_map_bucket_order_matches_solr() {
    let (app, _dir) = keyorder_app().await;
    let (status, text) = get_text(&app, CORE, &wide_range_query()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "wide range facet must be a 200: {text}"
    );
    assert_keys_match_fixture(
        &text,
        "keyorder_range_wide_map",
        "facet_counts.facet_ranges.views.counts",
        true,
    );
}

/// `facet_ranges.<field>` itself: `counts, gap, start, end`, not the alphabetical
/// `counts, end, gap, start`.
#[tokio::test]
async fn facet_range_sub_key_order_matches_solr() {
    let (app, _dir) = keyorder_app().await;
    let (status, text) = get_text(&app, CORE, &wide_range_query()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "wide range facet must be a 200: {text}"
    );
    assert_keys_match_fixture(
        &text,
        "keyorder_range_wide_map",
        "facet_counts.facet_ranges.views",
        true,
    );
}

/// `facet.field` under `json.nl=map` with the default `facet.sort=count`: the
/// object's key order is the count-descending order (apple 5, zebra 5, mango 2,
/// banana 1), the same order the flat array form uses, not alphabetical.
#[tokio::test]
async fn facet_field_json_nl_map_count_order_matches_solr() {
    let (app, _dir) = keyorder_app().await;
    let (status, text) = get_text(&app, CORE, &field_map_query()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "facet.field map must be a 200: {text}"
    );
    assert_keys_match_fixture(
        &text,
        "keyorder_facet_field_map",
        "facet_counts.facet_fields.tag",
        true,
    );
}

/// `facet.sort=index` under `json.nl=map`: term order. This one *is*
/// alphabetical, so it is expected to pass even with the `BTreeMap` backing —
/// it is here to pin that fixing the count case does not break index order, and
/// so it deliberately does not use the not-alphabetical guard.
#[tokio::test]
async fn facet_field_json_nl_map_index_order_matches_solr() {
    let (app, _dir) = keyorder_app().await;
    let (status, text) = get_text(&app, CORE, &field_map_index_query()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "facet.field map with facet.sort=index must be a 200: {text}"
    );
    assert_keys_match_fixture(
        &text,
        "keyorder_facet_field_map_index",
        "facet_counts.facet_fields.tag",
        false,
    );
}

// --- 4. envelope structure ------------------------------------------------

/// `facet_counts`' five sub-objects come back as `facet_queries, facet_fields,
/// facet_ranges, facet_intervals, facet_heatmaps` — alphabetised that would be
/// `facet_fields, facet_heatmaps, facet_intervals, facet_queries, facet_ranges`.
#[tokio::test]
async fn facet_counts_sub_object_order_matches_solr() {
    let (app, _dir) = indexed_app().await;
    let (status, text) = get_text(
        &app,
        CORE,
        "select?q=*:*&rows=0&facet=true&facet.field=category&json.nl=map&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "facet select must be a 200: {text}");
    assert_keys_match_fixture(&text, "facet_json_nl_map", "facet_counts", true);
}

/// Whole-envelope recursive order for a plain `select`: catches the top level
/// (`responseHeader, response`), `responseHeader` (`status, QTime, params`) and
/// `response` (`numFound, start, numFoundExact, docs`), plus each doc.
/// `responseHeader.params` is exempt (Java `HashMap` order — see the module
/// docs); `_version_`/`_root_` are ignored because Wayfinder has no such fields.
///
/// `rows=10` against the 5-doc corpus, so `response.docs` is genuinely
/// non-empty here — asserted explicitly (issue #31 follow-up 5) because
/// `assert_same_key_order` skips short/one-sided arrays by design, so an
/// empty-`docs` response would pass this whole-envelope check *vacuously*,
/// never having compared a single doc's key order.
#[tokio::test]
async fn plain_select_envelope_key_order_matches_solr() {
    let (app, _dir) = indexed_app().await;
    let (status, text) = get_text(&app, CORE, "select?q=*:*&rows=10&wt=json").await;
    assert_eq!(status, StatusCode::OK, "plain select must be a 200: {text}");
    let body: Value = serde_json::from_str(&text).expect("valid JSON");
    assert!(
        body["response"]["docs"]
            .as_array()
            .is_some_and(|d| !d.is_empty()),
        "response.docs must be non-empty, or the doc key-order comparison below is vacuous: {text}"
    );
    assert_same_key_order(&text, "select_all");
}

/// Whole-envelope recursive order for a facet `select`: additionally catches the
/// three-key top level `responseHeader, response, facet_counts`.
#[tokio::test]
async fn facet_select_envelope_key_order_matches_solr() {
    let (app, _dir) = indexed_app().await;
    let (status, text) = get_text(
        &app,
        CORE,
        "select?q=*:*&rows=0&facet=true&facet.field=category&json.nl=map&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "facet select must be a 200: {text}");
    assert_same_key_order(&text, "facet_json_nl_map");
}

/// The error envelope: `responseHeader, error` at the top and
/// `metadata, msg, code` inside — alphabetised the latter is
/// `code, metadata, msg`.
#[tokio::test]
async fn error_envelope_key_order_matches_solr() {
    let (app, _dir) = indexed_app().await;
    let (status, text) = get_text(&app, CORE, "select?q=*:*&fq=category:[unclosed&wt=json").await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "bad fq syntax must be a 400: {text}"
    );
    assert_keys_match_fixture(&text, "err_bad_syntax", "error", true);
    assert_same_key_order(&text, "err_bad_syntax");
}

// --- 5. `responseHeader.warnings` leads, not trails (issue #24) ------------
//
// Solr's `responseHeader` for a Points-based `facet.field` at effective
// `facet.mincount == 0` is `warnings, status, QTime, params`
// (`facet_field_numeric_all.json`) — `warnings` is the *first* key, not the
// last. Building `responseHeader` as a `serde_json::json!` object literal and
// then doing `body["responseHeader"]["warnings"] = ...` afterwards would
// insert it at the end under `preserve_order` (issue #25): this is exactly the
// kind of divergence `assert_matches_fixture` (order-insensitive `Value`
// equality) cannot see, and the reason this suite reads raw bytes instead.

/// Mirrors `tests/faceting.rs::RANGE_SCHEMA_TOML` / `range_corpus`, which is
/// the same schema and corpus the `facets` Solr core in
/// `solr-ref/capture.sh` builds — duplicated locally rather than shared
/// across integration-test binaries, the same choice `keyorder_app` above
/// makes for the `keyorder` core.
const FACETS_SCHEMA_TOML: &str = r#"
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

[[fields]]
name = "created"
type = "date"
stored = true
fast = true

[[fields]]
name = "note"
type = "string"
stored = true
"#;

fn facets_corpus() -> Value {
    json!([
        {"id":"r1","views":5, "created":"2020-01-02T00:00:00Z","note":"alpha"},
        {"id":"r2","views":15,"created":"2020-01-03T00:00:00Z","note":"beta"},
        {"id":"r3","views":25,"created":"2020-01-03T00:00:00Z","note":"alpha"},
        {"id":"r4","views":35,"created":"2020-01-05T00:00:00Z"}
    ])
}

async fn facets_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), FACETS_SCHEMA_TOML).expect("app must build");
    let (status, body) = post_docs(&app, &facets_corpus()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the facets corpus must succeed, got {body}"
    );
    (app, dir)
}

/// `responseHeader` itself: `warnings, status, QTime, params`, not the
/// alphabetical `QTime, params, status, warnings` and not
/// `status, QTime, params, warnings` either (the order a naive
/// `body["responseHeader"]["warnings"] = ...` append would produce).
///
/// No `response.docs` non-empty guard here (contrast
/// `plain_select_envelope_key_order_matches_solr`, issue #31 follow-up 5):
/// this query is `rows=0`, so an empty `docs` is legitimate, not a vacuity
/// bug — the whole point of the query is the facet counts, and
/// `facet_field_numeric_all`'s own capture (`capture.sh`) is `rows=0` too.
#[tokio::test]
async fn response_header_warnings_leads_not_trails() {
    let (app, _dir) = facets_app().await;
    let (status, text) = get_text(
        &app,
        CORE,
        "select?q=*:*&rows=0&facet=true&facet.field=views&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "numeric facet.field must be a 200: {text}"
    );
    assert_keys_match_fixture(&text, "facet_field_numeric_all", "responseHeader", true);
    assert_same_key_order(&text, "facet_field_numeric_all");
}

// --- 6. `IGNORED_KEYS` scoping (issue #31 follow-up 1) ---------------------
//
// `_version_`/`_root_` are keys Wayfinder deliberately never emits at all
// (findings fact 9), so they only ever appear on the *fixture* side. Today
// `key_order::compare` strips them from both sides' key lists at *any*
// depth before comparing, via `filtered_keys`. That is broader than the
// intent stated in the module docs ("Keys only real Solr emits... Ignored
// on both sides so a whole-envelope order comparison is about order, not
// about doc field membership") — the whole point only applies inside
// `response.docs[<i>]`, where Wayfinder's own omission is the documented
// divergence. This regression proves a `_version_` key *outside* that path
// is not silently ignored: an object with `_version_` at the top level,
// diffed against one missing it, must fail the key-order comparison, not
// pass because both sides get the key filtered away first.

/// Fails today: `_version_` at the top level (not inside
/// `response.docs[<i>]`) is filtered out of both sides by `IGNORED_KEYS`
/// before the key lists are compared, so this mismatch is invisible. Once
/// `IGNORED_KEYS` is scoped to `response.docs[<i>]` only, the comparison
/// sees `_version_` as a real key present on one side and not the other,
/// and fails as it should.
#[test]
fn version_key_outside_response_docs_is_not_ignored() {
    let expected_text = r#"{"top":1,"_version_":2}"#;
    let actual_text = r#"{"top":1}"#;

    let result = std::panic::catch_unwind(|| {
        assert_same_key_order_texts(
            actual_text,
            expected_text,
            "version_key_outside_response_docs_is_not_ignored",
        )
    });

    assert!(
        result.is_err(),
        "a top-level `_version_` present on only one side must be a real key-order \
         mismatch, not silently ignored by IGNORED_KEYS — IGNORED_KEYS must be scoped \
         to `response.docs[<i>]` only"
    );
}

/// Companion regression: a `_version_`/`_root_` mismatch genuinely *inside*
/// `response.docs[<i>]` must stay ignored — that is the one path where
/// Wayfinder's deliberate omission is the documented divergence, and this
/// must not regress into a false failure once the scoping lands.
#[test]
fn version_key_inside_response_docs_is_still_ignored() {
    let expected_text = r#"{"response":{"docs":[{"id":"a","_version_":2}]}}"#;
    let actual_text = r#"{"response":{"docs":[{"id":"a"}]}}"#;

    assert_same_key_order_texts(
        actual_text,
        expected_text,
        "version_key_inside_response_docs_is_still_ignored",
    );
}

// --- 7. finding 24's `fl`-order half, pinned by a committed fixture --------
//
// `select_fl_reversed` (`fl=body,id`, reversed from every other multi-field
// `fl` capture) discriminates input order from `fl` order: `select_term`'s
// `fl=id,body` cannot, because those two happen to coincide there. Doc key
// order must be `id, body` (input order), not `body, id` (`fl` order) — the
// vacuity guard below asserts the fixture itself actually discriminates,
// i.e. its doc keys are *not* in `fl` order.

/// The fixture's own doc keys must not equal `fl`'s order (`["body","id"]`)
/// — otherwise this fixture cannot tell input order apart from `fl` order,
/// and the pin below would be vacuous.
#[test]
fn select_fl_reversed_fixture_discriminates_input_order_from_fl_order() {
    let doc_keys = fixture_key_order("select_fl_reversed").keys_at(
        "response.docs[0]",
        "select_fl_reversed fixture vacuity guard",
    );
    assert_ne!(
        doc_keys,
        vec!["body".to_string(), "id".to_string()],
        "select_fl_reversed's fixture doc keys must not be in fl order (fl=body,id) — \
         if they are, capture.sh's premise for this fixture is wrong and it cannot pin \
         finding 24's fl-order half"
    );
}

/// Wayfinder's doc key order for `fl=body,id` matches the fixture — pinning
/// finding 24's "doc field order is input order, not `fl` order" half on a
/// committed fixture instead of a live probe only.
#[tokio::test]
async fn select_fl_reversed_doc_key_order_matches_solr() {
    let (app, _dir) = indexed_app().await;
    let (status, text) = get_text(&app, CORE, "select?q=*:*&rows=2&fl=body,id&wt=json").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "select with fl=body,id must be a 200: {text}"
    );
    assert_keys_match_fixture(&text, "select_fl_reversed", "response.docs[0]", false);
    assert_same_key_order(&text, "select_fl_reversed");
}

// --- 8. `json.facet` (issue #343): top-level slot + `facets` sub-object order --
//
// `facets` is a top-level sibling emitted *after* `facet_counts` and *before*
// `stats` (`jf343_with_classic_stats.json`'s top level is `responseHeader,
// response, facet_counts, facets, stats`). Its own sub-object order —
// `count` first, then each aggregation/terms key in request order, with
// nested `buckets` before an inline sub-facet as siblings of `val`/`count` —
// comes from `jf343_deep_max.json`.
//
// Schema/corpus duplicated locally from `tests/json_facet.rs::JF_SCHEMA_TOML`
// / `jf_corpus`, per this suite's own precedent (`FACETS_SCHEMA_TOML` above)
// of not sharing schema/corpus helpers across integration-test binaries.

const JF_SCHEMA_TOML: &str = r#"
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
name = "hash"
type = "string"
stored = true
fast = true

[[fields]]
name = "index_id"
type = "string"
stored = true
fast = true

[[fields]]
name = "ss_search_api_datasource"
type = "string"
stored = true
fast = true

[[fields]]
name = "popularity"
type = "int"
stored = true
fast = true
"#;

fn jf_corpus() -> Value {
    json!([
        {"id":"jf1","hash":"siteA","index_id":"index_a","ss_search_api_datasource":"entity:node","popularity":10,"body":"alpha"},
        {"id":"jf2","hash":"siteA","index_id":"index_a","ss_search_api_datasource":"entity:node","popularity":30,"body":"beta"},
        {"id":"jf3","hash":"siteA","index_id":"index_a","ss_search_api_datasource":"entity:user","popularity":20,"body":"gamma"},
        {"id":"jf4","hash":"siteA","index_id":"index_b","ss_search_api_datasource":"entity:node","popularity":40,"body":"delta"},
        {"id":"jf5","hash":"siteB","index_id":"index_c","ss_search_api_datasource":"entity:node","popularity":50,"body":"epsilon"},
        {"id":"jf6","popularity":60,"body":"zeta"}
    ])
}

async fn jf_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), JF_SCHEMA_TOML).expect("app must build");
    let (status, body) = post_docs(&app, &jf_corpus()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the jf343 corpus must succeed, got {body}"
    );
    (app, dir)
}

/// Percent-encodes every byte outside `A-Za-z0-9-_.~`, duplicated from
/// `tests/json_facet.rs` for the same "no cross-binary sharing" reason —
/// `json.facet`'s JSON value is not legal raw query-string text.
fn jf_percent_encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() * 3);
    for b in raw.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn jf_query(v: &Value) -> String {
    format!(
        "select?q=*:*&rows=0&facet=true&facet.field=hash&stats=true&stats.field=popularity&json.facet={}&wt=json",
        jf_percent_encode(&v.to_string())
    )
}

/// Top-level slot: `facets` sits after `facet_counts` and before `stats`
/// (`jf343_with_classic_stats.json`). Exercises `facet=true` + `stats=true`
/// + `json.facet` together, the same three-way coexistence
/// `tests/json_facet.rs::coexists_with_classic_faceting_and_stats` checks by
/// value — this test checks their *order* instead, which a parsed `Value`
/// comparison cannot see (module docs above).
#[tokio::test]
async fn json_facet_top_level_slot_matches_solr() {
    let (app, _dir) = jf_app().await;
    let v = json!({"maxPopularity": "max(popularity)"});
    let (status, text) = get_text(&app, CORE, &jf_query(&v)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "facet + stats + json.facet must be a 200: {text}"
    );
    assert_keys_match_fixture(&text, "jf343_with_classic_stats", "", false);
}

/// `facets`' own sub-object order: `count` first (the implicit scalar every
/// `json.facet` response carries), then each requested key in request order.
#[tokio::test]
async fn json_facet_count_leads_facets_object() {
    let (app, _dir) = jf_app().await;
    let v = json!({"maxPopularity": "max(popularity)"});
    let (status, text) = get_text(&app, CORE, &jf_query(&v)).await;
    assert_eq!(status, StatusCode::OK, "json.facet must be a 200: {text}");
    assert_keys_match_fixture(&text, "jf343_with_classic_stats", "facets", false);
}

/// The full 4-level nested shape's sub-object order: inside a terms bucket,
/// `val, count` lead and an inline sub-facet trails as a further sibling key
/// (`jf343_deep_max.json`) — not `val` alone, and not the sub-facet split out
/// into some other top-level container.
#[tokio::test]
async fn json_facet_deep_bucket_sub_object_order_matches_solr() {
    let (app, _dir) = jf_app().await;
    let v = json!({
        "maxPopularity": "max(popularity)",
        "siteHashes": {
            "limit": -1, "field": "hash", "type": "terms",
            "facet": {
                "indexes": {
                    "limit": -1, "field": "index_id", "type": "terms",
                    "facet": {
                        "dataSources": {
                            "limit": -1, "field": "ss_search_api_datasource", "type": "terms",
                            "facet": {"maxPopularityPerDataSource": "max(popularity)"}
                        }
                    }
                }
            }
        }
    });
    let (status, text) = get_text(
        &app,
        CORE,
        &format!(
            "select?q=*:*&rows=0&json.facet={}&wt=json",
            jf_percent_encode(&v.to_string())
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "4-level json.facet must be a 200: {text}"
    );
    assert_keys_match_fixture(&text, "jf343_deep_max", "facets", false);
    assert_keys_match_fixture(&text, "jf343_deep_max", "facets.siteHashes", false);
    assert_keys_match_fixture(
        &text,
        "jf343_deep_max",
        "facets.siteHashes.buckets[0]",
        false,
    );
    assert_keys_match_fixture(
        &text,
        "jf343_deep_max",
        "facets.siteHashes.buckets[0].indexes.buckets[0].dataSources.buckets[0]",
        false,
    );
    assert_same_key_order(&text, "jf343_deep_max");
}
