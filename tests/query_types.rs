//! Issue #8 — query types beyond the stock parser: fuzzy (`term~`, `term~N`),
//! wildcard/prefix (`te?t`, `test*`, `*mals`, `field:*`), regex (`/pattern/`),
//! ranges (`[a TO b]`, `{a TO b}`, `[a TO b}`, `*` endpoints) on string,
//! numeric and date fields, and boosts (`term^2`, `field:term^2.5`,
//! `"phrase"^2`, `fuzzy~1^3`) — PRD §5 `q` row, Lucene parser only.
//!
//! Every expected value comes from a committed fixture in `solr-ref/responses/`
//! (CLAUDE.md: fixtures are ground truth) — none is invented. Findings 56-59 in
//! `docs/solr-ref-findings.md` are the annotated map of which fixture proves
//! which fact; the section headers below mirror that map.
//!
//! Two things here are NOT fixture comparisons:
//! - The numeric/date range + numeric-field error tests reuse the `facets`
//!   schema/corpus that `tests/differential.rs::FACETS_SCHEMA_TOML`/
//!   `facets_corpus` already builds for `manifest-errors.tsv`'s `facets/...`
//!   rows — duplicated here (not shared, `tests/common/` cannot be shared
//!   across integration-test binaries; same precedent as that file's own
//!   comment) since `tests/common/mod.rs` stays append-only-if-needed and this
//!   change needs no new shared helper.
//! - The dynamic-field-rewrite-inside-quotes regression test (finding 59's
//!   corollary) has no Solr fixture at all: it pins a Wayfinder-only bug in
//!   `rewrite_dynamic_fields`'s `<ident>:` scan (documented in its own
//!   `ponytail:` comment in `src/core_index.rs`), not a Solr-compatibility
//!   fact. The expected token shapes it relies on (that `SimpleTokenizer` +
//!   `LowerCaser` + English stopword removal + English `Stemmer` — built-in
//!   `text_en`'s versioned analyzer — split
//!   `count_i` into `["count", "i"]` and `_dynamic.count_i` into
//!   `["dynam", "count", "i"]`) were verified against Wayfinder's own
//!   tokenizer pipeline before writing the test, not assumed.

// The `dead_code` allow for partially-used shared helpers is an inner attribute
// inside `tests/common/mod.rs`; repeating it here is a clippy error under
// `-D warnings`.
mod common;

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;

use common::{assert_matches_fixture, fixture, get, indexed_app, post_docs};

/// Asserts a query-parse/query-execution error matches the named fixture on
/// the contract the task spec names: HTTP status and `error.code`, mirrored
/// by `responseHeader.status`. `error.msg` is free text (Solr's Java wording
/// vs. Wayfinder's Tantivy-derived wording are not comparable) and is only
/// checked non-empty — same rule `tests/sort.rs::assert_sort_error` uses.
fn assert_query_error(status: StatusCode, body: &Value, fixture_name: &str) {
    let expected = fixture(fixture_name);
    let want_code = expected["error"]["code"]
        .as_i64()
        .unwrap_or_else(|| panic!("fixture {fixture_name} has no error.code"));

    assert_eq!(
        status.as_u16() as i64,
        want_code,
        "HTTP status must equal the fixture's error.code ({fixture_name})"
    );
    assert_eq!(
        body["error"]["code"].as_i64(),
        Some(want_code),
        "error.code must match the fixture ({fixture_name})"
    );
    assert_eq!(
        body["responseHeader"]["status"].as_i64(),
        Some(want_code),
        "responseHeader.status must mirror error.code ({fixture_name})"
    );
    assert!(
        body["error"]["msg"].as_str().is_some_and(|m| !m.is_empty()),
        "error.msg must be a non-empty string ({fixture_name})"
    );
    assert!(
        body.get("response").is_none(),
        "a rejected query must not also return a result set ({fixture_name})"
    );
}

/// The ordered `id` list in a response envelope.
fn ids(envelope: &Value) -> Vec<String> {
    envelope["response"]["docs"]
        .as_array()
        .expect("response.docs must be an array")
        .iter()
        .map(|d| {
            d["id"]
                .as_str()
                .expect("every doc must have a string id")
                .to_string()
        })
        .collect()
}

// =============================================================================
// Fuzzy (`term~`, `term~N`) — finding 56
// =============================================================================

/// Bare `~` is distance 2: it hits a distance-1 term (`animols`) AND a
/// distance-2 term (`animblz`), which is the fact `fuzzy_dist1_miss` (explicit
/// `~1`, below) shows is NOT true of distance 1 alone.
#[tokio::test]
async fn fuzzy_bare_tilde_defaults_to_distance_two() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=category:animols~&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "fuzzy_default_dist1");

    let (status, body) = get(&app, "select?q=category:animblz~&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "fuzzy_default_dist2");
}

/// Explicit `~1` discriminates distance 1 from distance 2, which the default
/// (above) cannot: `animols` (distance 1 from `animals`) hits, `animblz`
/// (distance 2) misses.
#[tokio::test]
async fn fuzzy_explicit_distance_one_excludes_distance_two() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=category:animols~1&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "fuzzy_dist1_hit");

    let (status, body) = get(&app, "select?q=category:animblz~1&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "fuzzy_dist1_miss");
}

#[tokio::test]
async fn fuzzy_explicit_distance_two_hits() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=category:animblz~2&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "fuzzy_dist2");
}

#[tokio::test]
async fn fuzzy_explicit_distance_zero_is_exact() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=category:animals~0&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "fuzzy_dist0_exact");
}

/// Out-of-range distances (`~3`, `~0.8`) are 200s with the exact-match set —
/// NEVER syntax errors, contrary to a reasonable prior that an
/// out-of-Lucene-range distance would 400.
#[tokio::test]
async fn fuzzy_out_of_range_distances_are_200s_not_syntax_errors() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=category:animals~3&wt=json").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an edit distance of 3 must be a 200, not a syntax error"
    );
    assert_matches_fixture(body, "err_fuzzy_dist3");

    let (status, body) = get(&app, "select?q=category:animals~0.8&wt=json").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a fractional edit distance must be a 200, not a syntax error"
    );
    assert_matches_fixture(body, "err_fuzzy_fractional");
}

/// Fuzzy terms are lowercased but never stemmed: `lazy~0` (exact) misses the
/// indexed (stemmed) `lazi`, `lazy~1` hits it by coincidental edit distance,
/// and `LAZY~1` hits too (case-insensitive via lowercasing, not stemming).
#[tokio::test]
async fn fuzzy_terms_are_lowercased_but_never_stemmed() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=body:lazy~0&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "fuzzy_analyzed_dist0");

    let (status, body) = get(&app, "select?q=body:lazy~1&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "fuzzy_analyzed_dist1");

    let (status, body) = get(&app, "select?q=body:LAZY~1&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "fuzzy_analyzed_case");
}

/// **Fuzzy matches are scored, not constant-score.** `fuzzy_analyzed_dist1`
/// returns `doc2, doc1` — NOT insertion order (`doc1` is inserted before
/// `doc2`) — so a constant-score fuzzy (Tantivy's `FuzzyTermQuery` default)
/// would diverge on ordering even with the right match set. This is checked
/// twice deliberately: `assert_matches_fixture` above already pins the whole
/// envelope including order, but this test isolates the ordering fact by name
/// so a future regression that gets the *match set* right but the *order*
/// wrong fails with an obviously-about-ordering assertion.
#[tokio::test]
async fn fuzzy_matches_are_scored_doc2_before_doc1_not_insertion_order() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=body:lazy~1&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        ids(&body),
        vec!["doc2".to_string(), "doc1".to_string()],
        "fuzzy hits must be scored (doc2 before doc1), not returned in insertion order"
    );
}

// =============================================================================
// Wildcard / prefix (`te?t`, `test*`, `*mals`, `an*ls`, `field:*`) — finding 57
// =============================================================================

#[tokio::test]
async fn wildcard_trailing_prefix_hits() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=category:anim*&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "wildcard_prefix");
}

#[tokio::test]
async fn wildcard_single_char_qmark_hits() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=category:anima?s&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "wildcard_qmark");
}

/// Leading wildcards need no opt-in in Solr 9, so Wayfinder must not reject
/// (or require any flag for) `*mals`.
#[tokio::test]
async fn wildcard_leading_star_hits_with_no_opt_in() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=category:*mals&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "wildcard_leading");
}

#[tokio::test]
async fn wildcard_infix_star_hits() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=category:an*ls&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "wildcard_infix");
}

/// Wildcard terms are lowercased but never stemmed — the same rule as fuzzy
/// (finding 56/57): `laz*` hits the stemmed `lazi`, `lazy*` misses it, `LAZ*`
/// hits (lowercased).
#[tokio::test]
async fn wildcard_terms_are_lowercased_but_never_stemmed() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=body:laz*&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "wildcard_analyzed_hit");

    let (status, body) = get(&app, "select?q=body:lazy*&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "wildcard_analyzed_stem");

    let (status, body) = get(&app, "select?q=body:LAZ*&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "wildcard_analyzed_case");
}

/// A bare (non-fielded) wildcard against `df` goes through the same analysis
/// path as a fielded one.
#[tokio::test]
async fn wildcard_bare_term_against_df_hits() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=laz*&df=body&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "wildcard_bare_df");
}

/// `field:*` is the field-exists idiom: every doc that has ANY `category`
/// value (4 of the 5 corpus docs — `doc5` has none).
#[tokio::test]
async fn wildcard_bare_star_is_field_exists_idiom() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=category:*&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["response"]["numFound"],
        json!(4),
        "category:* must match exactly the 4 docs that carry a category value"
    );
    assert_matches_fixture(body, "wildcard_field_exists");
}

// =============================================================================
// Regex (`/pattern/`) — finding 57/59
// =============================================================================

/// Regex is anchored whole-term matching, NOT substring: `/animals/` hits,
/// `/anim/` (a substring of the indexed term, not the whole term) misses.
#[tokio::test]
async fn regex_is_anchored_whole_term_not_substring() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=category:/animals/&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "regex_full");

    let (status, body) = get(&app, "select?q=category:/anim/&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "regex_substring");
}

#[tokio::test]
async fn regex_dotstar_and_charclass_hit() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=category:/anim.*/&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "regex_dotstar");

    let (status, body) = get(&app, "select?q=category:/anim[a-z]ls/&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "regex_charclass");
}

/// Regex is case-sensitive with no analysis at all: `/ANIMALS/` misses the
/// lowercase-indexed `animals`, unlike fuzzy/wildcard which lowercase.
#[tokio::test]
async fn regex_is_case_sensitive_no_analysis() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=category:/ANIMALS/&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "regex_uppercase");
}

/// Regex still runs over the indexed (stemmed) term on an analyzed field:
/// `/laz./` hits the stemmed `lazi`.
#[tokio::test]
async fn regex_runs_over_indexed_stemmed_terms() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=body:/laz./&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "regex_analyzed");
}

/// A regex that parses as a query but fails automaton compilation
/// (unbalanced `[`) is Solr's one captured 500 — whose error object is
/// `msg, code` with NO `metadata` key, unlike every other (400) error here.
#[tokio::test]
async fn regex_bad_char_class_is_a_500_with_no_metadata_key() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=category:/anim[/&wt=json").await;
    assert_query_error(status, &body, "err_regex_bad_class");
    assert!(
        body["error"].get("metadata").is_none(),
        "a regex-compile 500 must have no error.metadata key at all, unlike a 400 \
         SyntaxError — got {body}"
    );
    // The differential harness's normaliser drops `error.trace` on both
    // sides before comparing (finding 10/59's rationale — free text no
    // other engine can reproduce), so the hermetic differential suite
    // cannot by itself prove Wayfinder ever actually emits the key at all;
    // this test is the one place that shape is pinned directly.
    assert!(
        body["error"]["trace"]
            .as_str()
            .is_some_and(|t| !t.is_empty()),
        "a regex-compile 500 must carry a non-empty error.trace — got {body}"
    );
}

/// An unclosed `/regex` is an ordinary 400 SyntaxError, not the 500 above and
/// not a silent no-op parse (today Wayfinder answers 200 with 0 hits, which
/// is itself a divergence this test must catch).
#[tokio::test]
async fn regex_unclosed_is_a_400_syntax_error() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=category:/animals&wt=json").await;
    assert_query_error(status, &body, "err_regex_unclosed");
}

// =============================================================================
// Ranges (`[a TO b]`, `{a TO b}`, `[a TO b}`, `*` endpoints) — finding 58
// =============================================================================

#[tokio::test]
async fn range_str_inclusive_exclusive_and_half_open_endpoints() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=category:[animals+TO+garden]&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "range_str_incl");

    let (status, body) = get(&app, "select?q=category:{animals+TO+garden}&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "range_str_excl");

    let (status, body) = get(&app, "select?q=category:[animals+TO+garden}&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "range_str_half_open");
}

#[tokio::test]
async fn range_str_star_endpoints_either_or_both_sides() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=category:[garden+TO+*]&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "range_str_star_upper");

    let (status, body) = get(&app, "select?q=category:[*+TO+classic]&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "range_str_star_lower");

    // `[* TO *]` on a string field is the same field-exists idiom as
    // `category:*`, 4 docs.
    let (status, body) = get(&app, "select?q=category:[*+TO+*]&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["response"]["numFound"], json!(4));
    assert_matches_fixture(body, "range_str_star_both");
}

/// A reversed range (`[garden TO animals]`) is a 200 with 0 hits, not a 400 —
/// contrary to a reasonable prior that a backwards range is malformed.
#[tokio::test]
async fn range_str_reversed_is_200_with_zero_hits() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=category:[garden+TO+animals]&wt=json").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a reversed range must not be an error"
    );
    assert_matches_fixture(body, "range_str_reversed");
}

/// An unclosed range and a lowercase `to` (case-sensitive keyword) are both
/// 400 SyntaxErrors.
#[tokio::test]
async fn range_str_unclosed_and_lowercase_to_are_syntax_errors() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=category:[animals+TO&wt=json").await;
    assert_query_error(status, &body, "err_range_unclosed_q");

    let (status, body) = get(&app, "select?q=category:[animals+to+garden]&wt=json").await;
    assert_query_error(status, &body, "err_range_lowercase_to");
}

// --- numeric/date ranges + numeric-field errors, against the `facets` corpus
// (issue #33/#31's mirror schema; duplicated here per the task spec since
// `tests/common/` cannot be shared across integration-test binaries) --------

/// Mirrors `tests/differential.rs::FACETS_SCHEMA_TOML` exactly: `views` (pint)
/// and `created` (pdate) are the fields these tests range/fuzzy/wildcard over.
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

/// Mirrors `tests/differential.rs::facets_corpus` exactly: `views` r1=5,
/// r2=15, r3=25, r4=35; `created` r1=01-02, r2/r3=01-03 (two ties), r4=01-05.
fn numeric_corpus() -> Value {
    json!([
        {"id":"r1","views":5, "created":"2020-01-02T00:00:00Z","note":"alpha"},
        {"id":"r2","views":15,"created":"2020-01-03T00:00:00Z","note":"beta"},
        {"id":"r3","views":25,"created":"2020-01-03T00:00:00Z","note":"alpha"},
        {"id":"r4","views":35,"created":"2020-01-05T00:00:00Z"}
    ])
}

async fn numeric_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), NUMERIC_SCHEMA_TOML).expect("app must build");
    let (status, body) = post_docs(&app, &numeric_corpus()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the numeric/date corpus must succeed, got {body}"
    );
    (app, dir)
}

#[tokio::test]
async fn numeric_range_inclusive_exclusive_half_open_and_star() {
    let (app, _dir) = numeric_app().await;

    let (status, body) = get(&app, "select?q=views:[10+TO+30]&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "qrange_int_incl");

    let (status, body) = get(&app, "select?q=views:[5+TO+35]&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "qrange_int_incl_endpoints");

    let (status, body) = get(&app, "select?q=views:{5+TO+35}&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "qrange_int_excl");

    let (status, body) = get(&app, "select?q=views:[5+TO+35}&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "qrange_int_half_open");

    let (status, body) = get(&app, "select?q=views:[25+TO+*]&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "qrange_int_star_upper");
}

/// Numeric endpoints must parse as the field's own type: a float endpoint or
/// an alphabetic endpoint on a `pint` field are both 400 `Invalid Number` —
/// no truncation, no lexical fallback.
#[tokio::test]
async fn numeric_range_endpoints_are_typed_float_and_alpha_are_400() {
    let (app, _dir) = numeric_app().await;

    let (status, body) = get(&app, "select?q=views:[10.5+TO+30]&wt=json").await;
    assert_query_error(status, &body, "qrange_int_float_endpoint");

    let (status, body) = get(&app, "select?q=views:[a+TO+b]&wt=json").await;
    assert_query_error(status, &body, "qrange_int_alpha_endpoint");
}

/// Bare numeric terms parse numerically, not lexically: `views:015` matches
/// `15`, not a string `"015"` (which would match nothing).
#[tokio::test]
async fn numeric_bare_term_leading_zero_parses_numerically() {
    let (app, _dir) = numeric_app().await;
    let (status, body) = get(&app, "select?q=views:015&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "qterm_int_leading_zero");
}

/// A prefix/wildcard query on a Points-based (numeric) field is a 400 —
/// Tantivy has no term dictionary to walk there, matching Solr's own
/// rejection (`Can't run prefix queries on numeric fields`).
#[tokio::test]
async fn numeric_wildcard_is_a_400() {
    let (app, _dir) = numeric_app().await;
    let (status, body) = get(&app, "select?q=views:1*&wt=json").await;
    assert_query_error(status, &body, "qwild_int");
}

/// Fuzzy on a Points-based field is a 200 with 0 hits, not an error — fuzzy
/// syntax is accepted everywhere, it just never matches a non-string field.
#[tokio::test]
async fn numeric_fuzzy_is_200_with_zero_hits() {
    let (app, _dir) = numeric_app().await;
    let (status, body) = get(&app, "select?q=views:15~1&wt=json").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "fuzzy syntax on a numeric field must not be a syntax error"
    );
    assert_matches_fixture(body, "qfuzzy_int");
}

#[tokio::test]
async fn date_range_inclusive_and_exclusive() {
    let (app, _dir) = numeric_app().await;

    let (status, body) = get(
        &app,
        "select?q=created:[2020-01-02T00:00:00Z+TO+2020-01-03T00:00:00Z]&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "qrange_date_incl");

    let (status, body) = get(
        &app,
        "select?q=created:{2020-01-02T00:00:00Z+TO+2020-01-05T00:00:00Z}&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "qrange_date_excl");
}

// =============================================================================
// Boosts (`term^2`, `field:term^2.5`, `"phrase"^2`, `fuzzy~1^3`) — finding 59
// =============================================================================

/// A boost reorders BM25 scoring exactly as expected: the baseline `quick
/// garden` ranks the rarer `garden` doc first (`doc2, doc3, doc1`); boosting
/// `quick` flips the order to `doc3, doc1, doc2`. Bare, fielded and float
/// boost syntax all agree.
#[tokio::test]
async fn boost_reorders_scoring_bare_fielded_and_float_agree() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=quick+garden&df=body&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "boost_baseline");

    let (status, body) = get(&app, "select?q=quick^10+garden&df=body&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "boost_term");

    let (status, body) = get(&app, "select?q=body:quick^10+body:garden&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "boost_fielded_term");

    let (status, body) = get(&app, "select?q=body:quick^2.5+body:garden&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "boost_float");
}

#[tokio::test]
async fn boost_composes_with_phrase() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=%22lazy+dog%22^2&df=body&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "boost_phrase");
}

/// Boost composes with fuzzy too — this is the combined-feature case: it can
/// only pass once fuzzy itself is real (fixture is 2 hits, `doc1, doc4`).
#[tokio::test]
async fn boost_composes_with_fuzzy() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=category:animols~1^3&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "boost_fuzzy_combo");
}

/// A non-numeric boost (`^bad`) is a 400 SyntaxError.
#[tokio::test]
async fn boost_non_numeric_is_a_syntax_error() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=body:quick^bad&wt=json").await;
    assert_query_error(status, &body, "err_boost_bad");
}

// =============================================================================
// Field-query scoring model + quoted-phrase-with-colon — finding 59
// =============================================================================

/// **Scoring-model finding**: an unquoted control (`category:animals`) on a
/// multivalued `string` (unanalyzed, `omitNorms=true` in Solr) field must
/// return `doc1, doc4` in that order — insertion order, tie-broken by equal
/// scores — NOT `doc4, doc1`, which is what a length-normed scorer (favouring
/// the doc with fewer `category` values) would produce. `string`/`keyword`
/// fields must not carry/use fieldnorms in scoring, matching Solr.
#[tokio::test]
async fn select_q_field_term_string_field_has_no_length_norm() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=category:animals&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        ids(&body),
        vec!["doc1".to_string(), "doc4".to_string()],
        "an unanalyzed string field must not length-norm — doc1 (2 category values) must \
         rank no worse than doc4 (1 value), matching Solr's insertion-order tie-break on \
         equal (omitNorms) scores"
    );
    assert_matches_fixture(body, "select_q_field_term");
}

/// A colon inside a quoted phrase is NOT a field query — `q="category:animals"`
/// is a literal phrase searched against `df` (`body`), which matches nothing
/// in this corpus, unlike the unquoted control above.
#[tokio::test]
async fn quoted_phrase_containing_colon_is_a_phrase_not_a_field_query() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=%22category:animals%22&df=body&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["response"]["numFound"], json!(0));
    assert_matches_fixture(body, "phrase_with_colon");
}

// =============================================================================
// Dynamic-field-rewrite-inside-quotes regression (Wayfinder-only; no Solr
// fixture — see the module doc comment for why and how the expected token
// shapes were derived) — corollary of finding 59, retiring schema-layer
// follow-up 5's open question.
// =============================================================================

/// A schema with a `*_i` dynamic int rule, default field `body`. One doc
/// whose `body` contains the literal token sequence `count i seven` (which is
/// exactly what the phrase `"count_i: seven"` tokenizes to on built-in
/// `text_en`'s versioned analyzer — `SimpleTokenizer` splits on the non-alphanumeric `_`
/// and `:`), plus a `count_i` value the unquoted control query exercises.
const DYNAMIC_QUOTE_SCHEMA_TOML: &str = r#"
[core]
name = "content"
unique_key = "id"
default_field = "body"

[[fields]]
name = "id"
type = "string"
stored = true
required = true

[[fields]]
name = "body"
type = "text_en"
stored = true

[[dynamic_fields]]
pattern = "*_i"
type = "int"
stored = true
fast = true
"#;

async fn dynamic_quote_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app =
        common::app_with_schema(dir.path(), DYNAMIC_QUOTE_SCHEMA_TOML).expect("app must build");
    let (status, body) = post_docs(
        &app,
        &json!([{"id": "d1", "body": "the count i seven report", "count_i": 7}]),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the dynamic-quote regression doc must succeed, got {body}"
    );
    (app, dir)
}

/// Control: an UNQUOTED `count_i:7` must still be rewritten to the dynamic
/// container and match — this is the existing, already-working behaviour
/// (mirrors `tests/schema_layer.rs::doc_field_matching_a_dynamic_pattern_is_indexed_and_returned`)
/// that the fix below must not break.
#[tokio::test]
async fn dynamic_field_rewrite_still_applies_unquoted() {
    let (app, _dir) = dynamic_quote_app().await;
    let (status, body) = get(&app, "select?q=count_i:7&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["response"]["numFound"],
        json!(1),
        "an unquoted count_i:7 must still be rewritten to the dynamic container and match: \
         {body}"
    );
}

/// The regression: a QUOTED phrase containing `count_i:` must NOT be rewritten
/// to a `_dynamic.count_i` field query — it is a literal phrase on `df`
/// (`body`), and `body`'s doc contains exactly that token sequence
/// (`count i seven`), so the correct answer is `numFound: 1`.
///
/// Today `rewrite_dynamic_fields`'s `<ident>:` scan has no notion of being
/// inside a quoted string (its own `ponytail:` comment says so explicitly),
/// so it rewrites `count_i:` to `_dynamic.count_i:` even here, changing the
/// phrase's token sequence from `["count", "i", "seven"]` to
/// `["dynam", "count", "i", "seven"]` (verified against Wayfinder's actual
/// `text_en` tokenizer pipeline, not assumed) — which does not occur in the
/// doc's body, so today this answers `numFound: 0`, the wrong-for-the-right-
/// reason failure this test exists to catch.
#[tokio::test]
async fn dynamic_field_rewrite_must_not_apply_inside_a_quoted_phrase() {
    let (app, _dir) = dynamic_quote_app().await;
    let (status, body) = get(&app, "select?q=%22count_i:+seven%22&df=body&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["response"]["numFound"],
        json!(1),
        "a quoted phrase containing `count_i:` must be searched as a literal phrase on `df`, \
         not rewritten to a `_dynamic.count_i` field query: {body}"
    );
    let returned_ids = ids(&body);
    assert_eq!(
        returned_ids,
        vec!["d1".to_string()],
        "the phrase must match doc d1's body (`count i seven`), not be silently swallowed by \
         an incorrect rewrite: {body}"
    );
}

/// Round-1 review item 3 (Wayfinder-only regression; no Solr fixture, same
/// rationale as the quoted-phrase test above): `parse_query`'s own
/// fuzzy/wildcard detection must not run at all on a `[[dynamic_fields]]`
/// catch-all path — `count_i` is not itself a declared field, so this
/// module's `field_or_err` used to reject `count_i:1*`/`count_i:7~1` with
/// "undefined field" before `rewrite_dynamic_fields` (which turns `count_i:`
/// into `_dynamic.count_i:`) ever got a say. Both must now fall through to
/// Tantivy's own per-leaf conversion and succeed (200, 0 hits — a wildcard/
/// fuzzy match against a JSON int sub-path is not expected to hit anything
/// in this corpus, only to not hard-error).
#[tokio::test]
async fn dynamic_field_wildcard_and_fuzzy_do_not_hard_error() {
    let (app, _dir) = dynamic_quote_app().await;

    let (status, body) = get(&app, "select?q=count_i:1*&wt=json").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a wildcard on a dynamic-field path must not 400 just because `count_i` is not itself \
         a declared field: {body}"
    );
    assert_eq!(body["response"]["numFound"], json!(0));

    let (status, body) = get(&app, "select?q=count_i:7~1&wt=json").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "fuzzy on a dynamic-field path must not 400 for the same reason: {body}"
    );
    assert_eq!(body["response"]["numFound"], json!(0));
}

/// `count_i:*` (the field-exists idiom on a dynamic-field path) is not fixed
/// by this module at all: it now correctly falls through to Tantivy's own
/// `Exists`-leaf conversion (see `build_leaf`'s doc comment on why a
/// `_dynamic.*` path is never special-cased here), which has its own
/// separate, already-documented gap — `compute_logical_ast_from_leaf_lenient`
/// discards the field entirely for `UserInputLeaf::Exists`. That gap
/// predates issue #8 (the plain, pre-#8 `QueryParser::parse_query` path hits
/// the exact same tantivy arm for a declared field's `field:*` too, which is
/// finding 57's whole reason `build_field_exists` exists as a Wayfinder-side
/// workaround for *declared* fields) and is out of this fix's minimal scope
/// for an *undeclared* dynamic path with no Solr fixture pinning what the
/// right answer should even be. This test pins today's honest, correctly-
/// classed 400 — not the wrong-for-the-wrong-reason "undefined field" this
/// module used to produce before falling through.
#[tokio::test]
async fn dynamic_field_exists_falls_through_to_tantivys_own_gap_not_ours() {
    let (app, _dir) = dynamic_quote_app().await;
    let (status, body) = get(&app, "select?q=count_i:*&wt=json").await;
    assert_query_error(status, &body, "err_bad_syntax");
}
