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

use axum::Router;
use axum::http::StatusCode;
use common::key_order::{
    KeyOrder, assert_same_key_order, fixture_key_order, get_text, is_alphabetical,
};
use common::{CORE, indexed_app, post_docs};
use serde_json::{Value, json};
use tempfile::TempDir;

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
/// `solr-ref/manifest-errors.tsv`, minus the core prefix.
const WIDE_RANGE_QUERY: &str = "select?q=*:*&rows=0&facet=true&facet.range=views\
     &facet.range.start=0&facet.range.end=200&facet.range.gap=10&json.nl=map&wt=json";

/// The query from `keyorder_facet_field_map`'s manifest-errors row.
const FIELD_MAP_QUERY: &str = "select?q=*:*&rows=0&facet=true&facet.field=tag&json.nl=map&wt=json";

/// The query from `keyorder_facet_field_map_index`'s manifest-errors row.
const FIELD_MAP_INDEX_QUERY: &str =
    "select?q=*:*&rows=0&facet=true&facet.field=tag&facet.sort=index&json.nl=map&wt=json";

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
    let (status, text) = get_text(&app, CORE, WIDE_RANGE_QUERY).await;
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
    let (status, text) = get_text(&app, CORE, WIDE_RANGE_QUERY).await;
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
    let (status, text) = get_text(&app, CORE, FIELD_MAP_QUERY).await;
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
    let (status, text) = get_text(&app, CORE, FIELD_MAP_INDEX_QUERY).await;
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
#[tokio::test]
async fn plain_select_envelope_key_order_matches_solr() {
    let (app, _dir) = indexed_app().await;
    let (status, text) = get_text(&app, CORE, "select?q=*:*&rows=10&wt=json").await;
    assert_eq!(status, StatusCode::OK, "plain select must be a 200: {text}");
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
