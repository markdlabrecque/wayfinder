//! Issue #388 — global `text_en` / `text_general` / `_dynamic_text` analyzer
//! bounds, on `/select`.
//!
//! Wayfinder's global analyzed text chains diverge from the shipped
//! `search_api_solr` configset (`solr-ref/search-api/configset/schema_extra_types.xml`)
//! in three ways, all pinned by the twelve fixtures this file compares against:
//!
//! 1. **No minimum token length.** Every text field type in the configset
//!    carries `LengthFilterFactory min="2" max="100"`, index and query side.
//!    Wayfinder has no lower bound, so a one-character query token survives
//!    where Solr drops it.
//! 2. **Wrong lowercasing.** Lucene's `LowerCaseFilterFactory` is Java's
//!    `Character.toLowerCase`, Unicode's *simple* (1:1) mapping. Tantivy's
//!    `LowerCaser` is Rust's `str::to_lowercase`, the *full* mapping. They
//!    disagree on U+0130 LATIN CAPITAL LETTER I WITH DOT ABOVE: Java folds it
//!    to a bare `i`, Rust to `i` + U+0307 COMBINING DOT ABOVE.
//! 3. **Wrong upper bound.** Wayfinder uses `RemoveLongFilter::limit(40)`
//!    (bytes); the configset's only upper bound is `LengthFilterFactory
//!    max="100"` (characters, inclusive).
//!
//! Ground truth: `solr-ref/responses/select_analyzer_*.json`, captured from a
//! real `solr:9` running the `search-api` configset (commit 9e4c8d6, capture
//! block `--only '^select_analyzer_'` at the end of the dedicated Solr capture).
//! **Do not re-run the dedicated Solr capture.**
//!
//! Fixture-only, like `tests/select_fl_wildcard.rs`: no the captured fixture request set row,
//! because the fixture comparison suite's single tracer-bullet app cannot serve
//! `tm_X3b_en_*`/`tm_*` dynamic field types. Every test here requests
//! `fl=id&sort=id asc&rows=10&omitHeader=true` against the schema below and
//! compares the whole normalised envelope to the named fixture.
//!
//! Scope correction the issue text gets wrong: every analyzed **dynamic** rule
//! (`ts_*`, `tm_*`, `tm_X3b_en_*`, ...) indexes and queries through the single
//! `_dynamic_text` catch-all regardless of the type the rule declares, so this
//! file's schema deliberately uses *dynamic* rules (`tm_X3b_en_*` -> `text_en`,
//! `tm_*` -> `text_general`) rather than static fields, to exercise the
//! catch-all path a realistic Drupal index actually uses.

mod common;

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;

/// Core `content`, `id` the string unique key, and the two dynamic rules the
/// capture corpus was indexed under: `tm_X3b_en_*` (configset `text_en`) and
/// `tm_*` (configset `text_und`, which Wayfinder's `text_general` preset
/// stands in for). `tm_X3b_en_*` is the longer/more specific pattern, so it
/// wins over `tm_*` for any name matching both (Solr's longest-pattern-wins
/// rule, already covered by `tests/schema_layer.rs`).
const ANALYZER_BOUNDS_SCHEMA_TOML: &str = r#"
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

[[dynamic_fields]]
pattern = "tm_X3b_en_*"
type = "text_en"
multi_valued = true

[[dynamic_fields]]
pattern = "tm_*"
type = "text_general"
multi_valued = true
"#;

/// The exact 5-doc corpus the dedicated Solr capture's `select_analyzer_*` block
/// indexed, byte-identical (long tokens built with `"a".repeat(n)` rather than
/// pasted). Every value is written to *both* `tm_X3b_en_title` and `tm_title`,
/// so an `en`/`und` fixture pair over the same doc isolates the field type as
/// the only variable.
fn corpus() -> Value {
    let a45 = "a".repeat(45);
    let b100 = "b".repeat(100);
    let c101 = "c".repeat(101);
    json!([
        {"id": "a1", "tm_X3b_en_title": ["count i seven"], "tm_title": ["count i seven"]},
        {"id": "a2", "tm_X3b_en_title": ["İstanbul airport"], "tm_title": ["İstanbul airport"]},
        {"id": "a3", "tm_X3b_en_title": [a45.clone()], "tm_title": [a45]},
        {"id": "a4", "tm_X3b_en_title": [b100.clone()], "tm_title": [b100]},
        {"id": "a5", "tm_X3b_en_title": [c101.clone()], "tm_title": [c101]}
    ])
}

/// Builds a fresh app against `ANALYZER_BOUNDS_SCHEMA_TOML`, indexes and
/// commits `corpus()`. Returns the router plus the `TempDir` guard — keep it
/// alive for the lifetime of the test.
async fn analyzer_bounds_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app =
        common::app_with_schema(dir.path(), ANALYZER_BOUNDS_SCHEMA_TOML).expect("app must build");
    let (status, body) = common::post_docs(&app, &corpus()).await;
    assert_eq!(status, StatusCode::OK, "indexing must succeed, got {body}");
    (app, dir)
}

/// Issues the fixture's query against the analyzer-bounds corpus and asserts
/// the normalised response body equals `common::fixture(fixture_name)`.
async fn assert_select_matches_fixture(query_field_and_term: &str, fixture_name: &str) {
    let (app, _dir) = analyzer_bounds_app().await;
    let path = format!(
        "select?q={query_field_and_term}&wt=json&fl=id&sort=id+asc&rows=10&omitHeader=true"
    );
    let (status, body) = common::get(&app, &path).await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    common::assert_matches_fixture(body, fixture_name);
}

// --- 1. LengthFilterFactory min="2" -----------------------------------------

/// Discriminates divergence 1 (no lower bound): `i` is in neither stopword
/// list, so a hit here can only come from the missing `LengthFilterFactory
/// min="2"`. Predicted RED: Wayfinder has no lower bound today, so `q=...:i`
/// still analyzes to a token and matches doc `a1`.
#[tokio::test]
async fn select_analyzer_onechar_en_matches_fixture() {
    assert_select_matches_fixture("tm_X3b_en_title:i", "select_analyzer_onechar_en").await;
}

/// As `onechar_en`, over the `text_general`-typed `tm_*` dynamic rule (which
/// still resolves through the same `_dynamic_text` catch-all). Predicted RED.
#[tokio::test]
async fn select_analyzer_onechar_und_matches_fixture() {
    assert_select_matches_fixture("tm_title:i", "select_analyzer_onechar_und").await;
}

/// Control for `onechar_en`: same doc, same field, a token of two-or-more
/// characters. Must hit regardless of the length bound. Predicted GREEN
/// already (a length bound only ever *removes* matches, never adds one).
#[tokio::test]
async fn select_analyzer_onechar_control_en_matches_fixture() {
    assert_select_matches_fixture(
        "tm_X3b_en_title:count",
        "select_analyzer_onechar_control_en",
    )
    .await;
}

/// As `onechar_control_en`, over `tm_*`/`text_general`. Predicted GREEN.
#[tokio::test]
async fn select_analyzer_onechar_control_und_matches_fixture() {
    assert_select_matches_fixture("tm_title:count", "select_analyzer_onechar_control_und").await;
}

// --- 2. U+0130 simple-vs-full lowercasing -----------------------------------

/// Discriminates divergence 2: an all-ASCII `istanbul` query against an
/// indexed `İstanbul` (U+0130). Lucene's simple fold makes the indexed token
/// `istanbul`, a byte-for-byte match; Rust's full fold appends a trailing
/// U+0307 COMBINING DOT ABOVE, so the query token never matches the indexed
/// one. Predicted RED.
#[tokio::test]
async fn select_analyzer_dotted_i_en_matches_fixture() {
    assert_select_matches_fixture("tm_X3b_en_title:istanbul", "select_analyzer_dotted_i_en").await;
}

/// As `dotted_i_en`, over `tm_*`/`text_general`. Predicted RED.
#[tokio::test]
async fn select_analyzer_dotted_i_und_matches_fixture() {
    assert_select_matches_fixture("tm_title:istanbul", "select_analyzer_dotted_i_und").await;
}

/// The query-side control for divergence 2, written with the UPPERCASE U+0130
/// itself rather than plain ASCII: both the indexed token and the query token
/// go through the *same* folding, so this must hit under either mapping.
/// Predicted GREEN already: today's full-Unicode fold is at least internally
/// consistent between index and query time.
#[tokio::test]
async fn select_analyzer_dotted_i_upper_en_matches_fixture() {
    assert_select_matches_fixture(
        "tm_X3b_en_title:İstanbul",
        "select_analyzer_dotted_i_upper_en",
    )
    .await;
}

/// The pure-ASCII control token on the same doc as the dotted-I cases, so a
/// miss on `istanbul` above is legible as "the mapping disagrees", not "the
/// field is unsearchable". Predicted GREEN already.
#[tokio::test]
async fn select_analyzer_dotted_i_control_en_matches_fixture() {
    assert_select_matches_fixture(
        "tm_X3b_en_title:airport",
        "select_analyzer_dotted_i_control_en",
    )
    .await;
}

// --- 3. Upper bound: LengthFilterFactory max="100" (inclusive), NOT
//        RemoveLongFilter::limit(40) bytes ----------------------------------

/// Discriminates divergence 3 in the direction where Wayfinder drops a token
/// Solr keeps: 45 characters (== 45 bytes, all-ASCII) is inside the
/// configset's `max="100"` but outside `RemoveLongFilter::limit(40)`.
/// Predicted RED.
#[tokio::test]
async fn select_analyzer_long45_en_matches_fixture() {
    let term = "a".repeat(45);
    assert_select_matches_fixture(
        &format!("tm_X3b_en_title:{term}"),
        "select_analyzer_long45_en",
    )
    .await;
}

/// As `long45_en`, over `tm_*`/`text_general`. Predicted RED.
#[tokio::test]
async fn select_analyzer_long45_und_matches_fixture() {
    let term = "a".repeat(45);
    assert_select_matches_fixture(&format!("tm_title:{term}"), "select_analyzer_long45_und").await;
}

/// The upper bound is inclusive: exactly 100 characters (== 100 bytes) must
/// survive. 100 > `RemoveLongFilter::limit(40)`'s cut, so this is also
/// discriminating today. Predicted RED.
#[tokio::test]
async fn select_analyzer_long100_en_matches_fixture() {
    let term = "b".repeat(100);
    assert_select_matches_fixture(
        &format!("tm_X3b_en_title:{term}"),
        "select_analyzer_long100_en",
    )
    .await;
}

/// 101 characters, one over the inclusive `max="100"` bound, so Solr drops it
/// -- `numFound: 0`. **This one is predicted GREEN today, but for the WRONG
/// reason**: Wayfinder already drops this token, just because 101 bytes is
/// also over `RemoveLongFilter::limit(40)` bytes. The two bounds (100
/// characters vs. 40 bytes) agree here purely by coincidence -- neither the
/// current 40-byte cut nor the target 100-character cut is being exercised
/// specifically by this fixture in isolation; `long45_en`/`long100_en` above
/// are what actually discriminate the two bounds. After the fix, this test
/// must still be green, but for the RIGHT reason (100-character inclusive
/// max, not a 40-byte cut) -- nothing in this file alone can tell those apart
/// for THIS fixture; the other three long-token fixtures are what would
/// notice if the bound regressed back to a byte-based one.
#[tokio::test]
async fn select_analyzer_long101_en_matches_fixture() {
    let term = "c".repeat(101);
    assert_select_matches_fixture(
        &format!("tm_X3b_en_title:{term}"),
        "select_analyzer_long101_en",
    )
    .await;
}
