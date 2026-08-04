//! Issue #341, the DYNAMIC half: the same `solr.DateRangeField` semantics
//! reached through a `[[dynamic_fields]]` rule instead of a declared static
//! `[[fields]]` entry (spec decision 1: "both static and dynamic, dynamic via
//! nested JSON"). `tests/date_range.rs` covers the static half; this file is
//! that file's schema-swapped twin.
//!
//! Why the fixtures apply verbatim to the dynamic schema: in the captured Solr
//! core, `drs_x`/`drm_x` are THEMSELVES dynamic. `solr-ref/capture.sh`'s `#341`
//! block declares them with `add-dynamic-field` for `drs_*`/`drm_*`
//! (`{"add-dynamic-field": {"name":"drs_*", "type":"date_range", ...}}`), never
//! as static fields, exactly as the captured Drupal configset does
//! (`solr-ref/search-api/configset/schema.xml:340-341`). So every `dr341_*`
//! fixture IS a dynamic-field capture, and its expected id list is ground truth
//! for a Wayfinder schema whose only `date_range` reach is a pair of
//! `[[dynamic_fields]]` rules. Same fixtures, second schema -- no expected
//! value here is invented.
//!
//! The multiValued union block (finding 168) matters MORE here than on the
//! static path: the dynamic path has no `fast` columns of its own and the naive
//! `_dynamic.<name>.start`/`.end` term-range implementation is precisely the
//! pairing-blind one that `dr341_multi_gap`, `dr341_multi_no_contains`,
//! `dr341_multi_within_one` and `dr341_multi_contains_one` were captured to
//! catch. Those four are not softened.
//!
//! Findings 165-172 in `docs/solr-ref-findings.md` are the authoritative prose;
//! each test cites its fixture and finding.

// The `dead_code` allow for the shared helpers is an inner attribute inside
// `tests/common/mod.rs`; do not add a second one here (clippy rejects it under
// `-D warnings`).
mod common;

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;

use common::{app_with_schema, assert_matches_fixture, fixture, get, post_docs};

/// NO static `drs_x`/`drm_x` declaration -- the only `date_range` reach is two
/// `[[dynamic_fields]]` rules, `drs_*` (single-valued) and `drm_*`
/// (`multi_valued = true`). Both `stored = true` and `fast` left unset, i.e.
/// false: the captured configset gives these prefixes
/// `indexed="true" stored="true"` with NO `docValues`
/// (`solr-ref/search-api/configset/schema.xml:340-341`), which is the same
/// convention `presets/search-api.toml` follows for them. Core named `content`
/// so `common::get`/`common::post_docs`/`common::CORE` address it unchanged.
const DATE_RANGE_DYNAMIC_SCHEMA_TOML: &str = r#"
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
pattern = "drs_*"
type = "date_range"
stored = true

[[dynamic_fields]]
pattern = "drm_*"
type = "date_range"
multi_valued = true
stored = true
"#;

/// The exact 9-doc corpus `solr-ref/capture.sh`'s `#341` block indexes --
/// copied from `tests/date_range.rs::date_range_corpus` so the two schemas are
/// compared over identical input. d1/d2 are bare literals at year/month
/// precision; d3 an explicit closed interval; d4 a full-instant literal (whole
/// second); d5/d6 open-ended; d7 fully open; d8 two disjoint members (a hole
/// covering 2021); d9 the same field with a single member.
fn date_range_corpus() -> Value {
    json!([
        {"id":"d1","drs_x":"2020"},
        {"id":"d2","drs_x":"2020-06"},
        {"id":"d3","drs_x":"[2020-03-01T00:00:00Z TO 2020-09-30T00:00:00Z]"},
        {"id":"d4","drs_x":"2020-06-15T12:00:00Z"},
        {"id":"d5","drs_x":"[* TO 2019-12-31T23:59:59Z]"},
        {"id":"d6","drs_x":"[2021-01-01T00:00:00Z TO *]"},
        {"id":"d7","drs_x":"[* TO *]"},
        {"id":"d8","drm_x":["2020","2022-05"]},
        {"id":"d9","drm_x":["2020"]}
    ])
}

async fn dynamic_date_range_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = app_with_schema(dir.path(), DATE_RANGE_DYNAMIC_SCHEMA_TOML)
        .expect("dynamic date_range app must build");
    let (status, body) = post_docs(&app, &date_range_corpus()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the date-range corpus into the DYNAMIC schema must succeed, got {body}"
    );
    (app, dir)
}

fn ids(body: &Value) -> Vec<String> {
    body["response"]["docs"]
        .as_array()
        .expect("docs array")
        .iter()
        .map(|d| d["id"].as_str().expect("id").to_string())
        .collect()
}

/// A deliberate copy of `tests/date_range.rs::assert_error_matches` -- Rust
/// integration tests are separate binaries, so a helper cannot be shared
/// between them without promoting it into `tests/common/mod.rs` (a hot file for
/// parallel work, and this helper is #341-specific). Compares only HTTP status,
/// `error.code`, and `error.msg` against `fixture_name`; never
/// `error.metadata`/`error.trace`, which are Java-internal class names and a
/// JVM stack trace Wayfinder cannot and need not reproduce (finding 10: only
/// `error.code`/HTTP status is in the compatibility contract). `error.msg` is
/// asserted verbatim because finding 170 pins these exact strings as the
/// discriminating evidence for the 400/500 split.
fn assert_error_matches(status: StatusCode, body: &Value, fixture_name: &str) {
    let expected = fixture(fixture_name);
    let expected_status = expected["responseHeader"]["status"]
        .as_u64()
        .unwrap_or_else(|| panic!("{fixture_name}: fixture must carry responseHeader.status"));
    assert_eq!(
        status.as_u16() as u64,
        expected_status,
        "{fixture_name}: HTTP status, body: {body}"
    );
    assert_eq!(
        body["error"]["code"].as_u64(),
        expected["error"]["code"].as_u64(),
        "{fixture_name}: error.code, body: {body}"
    );
    assert_eq!(
        body["error"]["msg"].as_str(),
        expected["error"]["msg"].as_str(),
        "{fixture_name}: error.msg must match Solr verbatim (finding 170), body: {body}"
    );
    // Presence/absence of the `response` block is part of the captured envelope
    // (see the same assertion in `tests/date_range.rs`): every `dr341_err_*`
    // fixture has keys `["responseHeader","error"]` only, and
    // `dr341_err_stats`'s is what makes `stats::PreQueryStatsError` load-bearing.
    assert_eq!(
        body.get("response").is_some(),
        expected.get("response").is_some(),
        "{fixture_name}: a `response` block must be present exactly when the \
         captured envelope has one, body: {body}"
    );
}

// --- storage: verbatim round-trip through the dynamic path (finding 165) -----

/// Fixture `dr341_roundtrip`, finding 165. The dynamic path is the one that
/// most easily loses this: whatever nested `_dynamic.<name>.start`/`.end`
/// representation carries the parsed endpoints, `fl` must still render exactly
/// what was sent -- `"2020"` as `"2020"` (not an expanded interval), `"[* TO
/// *]"` as `"[* TO *]"`, and `drm_x` as its original member array in input
/// order. Pinned against the whole captured envelope, params included.
#[tokio::test]
async fn dynamic_roundtrip_stores_the_value_verbatim() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&fl=id,drs_x,drm_x&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_roundtrip");
}

// --- bare literal / bracket interval Intersects (findings 166, 167) ----------

/// Fixture `dr341_intersects_plain`, finding 167: a plain `drs_x:[a TO b]` on a
/// DYNAMIC `date_range` field is an Intersects interval query, not a term query
/// and not the `_dynamic` catch-all's string comparison -> d1,d2,d3,d4,d7.
#[tokio::test]
async fn dynamic_plain_range_query_is_intersects_by_default() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A%5B2020-05-01T00%3A00%3A00Z%20TO%202020-07-01T00%3A00%3A00Z%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_intersects_plain");
}

/// Fixture `dr341_single_year`, finding 166: a bare `drs_x:2020` denotes the
/// whole year interval, so it intersects the same five docs as the explicit
/// May-Jul 2020 range above.
#[tokio::test]
async fn dynamic_bare_year_literal_expands_to_the_whole_year_interval() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A2020&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_single_year");
}

/// Fixture `dr341_single_month`, finding 166: the same rule at month precision.
#[tokio::test]
async fn dynamic_bare_month_literal_expands_to_the_whole_month_interval() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A2020-06&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_single_month");
}

/// Fixture `dr341_star_both`, finding 166: `[* TO *]` matches every doc that
/// HAS the field -- d1-d7, and not d8/d9, which only carry `drm_x`. On the
/// dynamic path this also pins that the two prefixes stay distinct sub-paths
/// inside the shared `_dynamic` catch-all rather than merging.
#[tokio::test]
async fn dynamic_fully_open_interval_matches_every_doc_with_the_field() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A%5B*%20TO%20*%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_star_both");
}

/// Fixture `dr341_touch_endpoint`, finding 166: the precision rule applies to
/// interval ENDPOINTS too. d5 is `[* TO 2019-12-31T23:59:59Z]`, whose end is
/// that whole second, so a query starting exactly at that second intersects it
/// -> d1,d5,d7.
#[tokio::test]
async fn dynamic_interval_endpoint_precision_touches_the_whole_stated_second() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A%5B2019-12-31T23%3A59%3A59Z%20TO%202020-01-01T00%3A00%3A00Z%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_touch_endpoint");
}

/// Fixture `dr341_touch_past_ms`, finding 166: one millisecond past that
/// second's start still intersects d5 -- same result set as
/// `dr341_touch_endpoint`, which is what proves the endpoint was expanded
/// rather than treated as an instant.
#[tokio::test]
async fn dynamic_interval_endpoint_precision_still_touches_one_ms_past_the_second() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A%5B2019-12-31T23%3A59%3A59.001Z%20TO%202020-01-01T00%3A00%3A00Z%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        ids(&body),
        vec!["d1".to_string(), "d5".to_string(), "d7".to_string()],
        "d5's end (2019-12-31T23:59:59Z) is that whole SECOND, so a query \
         starting a millisecond past the second's start still intersects it"
    );
    assert_matches_fixture(body, "dr341_touch_past_ms");
}

// --- `{!field f= op=}` predicates, aliases and casing (finding 167) ----------

/// Fixture `dr341_op_default`, finding 167: `{!field f=drs_x}` with NO `op` at
/// all defaults to Intersects, on a dynamic field.
#[tokio::test]
async fn dynamic_field_query_parser_with_no_op_defaults_to_intersects() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrs_x%7D2020-06&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_op_default");
}

/// Fixture `dr341_op_intersects`, finding 167: an explicit `op=Intersects` is
/// the same query as the plain bracket form.
#[tokio::test]
async fn dynamic_explicit_op_intersects_matches_plain_form() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrs_x%20op%3DIntersects%7D%5B2020-05-01T00%3A00%3A00Z%20TO%202020-07-01T00%3A00%3A00Z%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_op_intersects");
}

/// Fixture `dr341_op_contains`, finding 167: `Contains` -> d1,d3,d7.
#[tokio::test]
async fn dynamic_op_contains_requires_the_doc_to_cover_the_query_interval() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrs_x%20op%3DContains%7D%5B2020-05-01T00%3A00%3A00Z%20TO%202020-07-01T00%3A00%3A00Z%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_op_contains");
}

/// Fixture `dr341_op_within`, finding 167: `Within` -> d2,d4 over the identical
/// query interval, disjoint from `Contains`'s d1,d3,d7 -- the two ops are not
/// complements.
#[tokio::test]
async fn dynamic_op_within_requires_the_doc_to_fit_inside_the_query_interval() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrs_x%20op%3DWithin%7D%5B2020-05-01T00%3A00%3A00Z%20TO%202020-07-01T00%3A00%3A00Z%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        ids(&body),
        vec!["d2".to_string(), "d4".to_string()],
        "Within must not be Contains's complement"
    );
    assert_matches_fixture(body, "dr341_op_within");
}

/// Fixture `dr341_op_iswithin`, finding 167: `IsWithin` is an accepted alias of
/// `Within` -- identical result set.
#[tokio::test]
async fn dynamic_op_iswithin_is_an_alias_of_within() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrs_x%20op%3DIsWithin%7D%5B2020-05-01T00%3A00%3A00Z%20TO%202020-07-01T00%3A00%3A00Z%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_op_iswithin");
}

/// Fixture `dr341_op_lowercase`, finding 167: the `op` value is matched
/// case-INSENSITIVELY, so `op=contains` behaves exactly like `op=Contains`.
#[tokio::test]
async fn dynamic_op_value_is_matched_case_insensitively() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrs_x%20op%3Dcontains%7D%5B2020-05-01T00%3A00%3A00Z%20TO%202020-07-01T00%3A00%3A00Z%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_op_lowercase");
}

// --- millisecond-boundary discrimination (finding 166) ----------------------

/// Fixture `dr341_within_ms_exact`, finding 166: d2's `"2020-06"` ends
/// `.999Z`, so a `Within` query ending exactly there contains it -> d2,d4.
#[tokio::test]
async fn dynamic_within_query_ending_exactly_at_ms_boundary_contains_the_bare_month_literal() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrs_x%20op%3DWithin%7D%5B2020-06-01T00%3A00%3A00Z%20TO%202020-06-30T23%3A59%3A59.999Z%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&body), vec!["d2".to_string(), "d4".to_string()]);
    assert_matches_fixture(body, "dr341_within_ms_exact");
}

/// Fixture `dr341_within_ms_short`, finding 166: one millisecond earlier
/// (`.998Z`) drops d2. This pair is what pins the expansion to millisecond
/// rather than second resolution -- and on the dynamic path, that the nested
/// endpoint representation keeps millisecond precision.
#[tokio::test]
async fn dynamic_within_query_one_ms_short_of_the_boundary_excludes_the_bare_month_literal() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrs_x%20op%3DWithin%7D%5B2020-06-01T00%3A00%3A00Z%20TO%202020-06-30T23%3A59%3A59.998Z%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        ids(&body),
        vec!["d4".to_string()],
        "d2 (\"2020-06\", ending .999Z) must NOT fit inside a query ending .998Z"
    );
    assert_matches_fixture(body, "dr341_within_ms_short");
}

// --- brace-equivalence (finding 169) ----------------------------------------

/// Fixture `dr341_excl_braces`, finding 169: `{a TO b}` is accepted and
/// returns exactly what `[a TO b]` returns. An implementation that routes the
/// clause through a Lucene-style range parser before consulting the dynamic
/// rule's type gets this wrong.
#[tokio::test]
async fn dynamic_exclusive_brace_syntax_is_silently_treated_as_inclusive() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A%7B2020-05-01T00%3A00%3A00Z%20TO%202020-07-01T00%3A00%3A00Z%7D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        ids(&body),
        vec![
            "d1".to_string(),
            "d2".to_string(),
            "d3".to_string(),
            "d4".to_string(),
            "d7".to_string()
        ],
        "must be identical to the closed-bracket form's result set"
    );
    assert_matches_fixture(body, "dr341_excl_braces");
}

// --- date math (finding 171) -------------------------------------------------

/// Fixture `dr341_datemath_year`, finding 171: `[NOW/YEAR TO NOW/YEAR+1YEAR]`
/// resolves -> d6,d7. Stable for every `NOW` through 2100, per the capture
/// site's own recorded expiry.
#[tokio::test]
async fn dynamic_date_math_now_slash_year_resolves_in_a_date_range_query() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A%5BNOW%2FYEAR%20TO%20NOW%2FYEAR%2B1YEAR%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&body), vec!["d6".to_string(), "d7".to_string()]);
    assert_matches_fixture(body, "dr341_datemath_year");
}

/// Fixture `dr341_datemath_now`, finding 171: `[NOW-100YEARS TO NOW]` resolves
/// -> all 7 single-valued docs. Stable until 2119, per the capture site.
#[tokio::test]
async fn dynamic_date_math_now_minus_years_resolves_in_a_date_range_query() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A%5BNOW-100YEARS%20TO%20NOW%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        ids(&body),
        vec![
            "d1".to_string(),
            "d2".to_string(),
            "d3".to_string(),
            "d4".to_string(),
            "d5".to_string(),
            "d6".to_string(),
            "d7".to_string()
        ]
    );
    assert_matches_fixture(body, "dr341_datemath_now");
}

// --- multiValued: union-of-members set relations (finding 168) ---------------
//
// This whole block is the trap, and it is sharper on the dynamic path than the
// static one: with no per-field fast columns, the obvious dynamic
// implementation is a term range over `_dynamic.drm_x.start` /
// `_dynamic.drm_x.end`, which loses which start pairs with which end. Every
// assertion below is a fixture-derived id list; none may be relaxed.

/// Fixture `dr341_multi_intersects`, finding 168: a query matching exactly one
/// of d8's members -> only d8 (d9 has no `2022-05` member).
#[tokio::test]
async fn dynamic_multivalued_intersects_a_query_matching_one_member() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drm_x%3A2022-05&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_multi_intersects");
}

/// Fixture `dr341_multi_contains`, finding 168: `Contains 2022-05` -> only d8.
#[tokio::test]
async fn dynamic_multivalued_contains_a_query_matching_one_member() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrm_x%20op%3DContains%7D2022-05&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_multi_contains");
}

/// Fixture `dr341_multi_gap`, finding 168: **0 hits**. A query landing entirely
/// inside d8's hole (2021, between its `2020` and `2022-05` members) intersects
/// neither doc. A start/end column pair that has lost member pairing collapses
/// d8 to the single span `2020-01-01 .. 2022-05-31` and wrongly matches it --
/// this is the fixture that catches exactly that, and the dynamic path is where
/// that shortcut is most tempting.
#[tokio::test]
async fn dynamic_multivalued_intersects_is_hole_sensitive_not_a_min_max_span() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drm_x%3A%5B2021-01%20TO%202021-06%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        ids(&body),
        Vec::<String>::new(),
        "a query landing entirely inside d8's hole must match nothing, not d8 \
         via a min-start/max-end collapse"
    );
    assert_matches_fixture(body, "dr341_multi_gap");
}

/// Fixture `dr341_multi_no_contains`, finding 168: **0 hits**. `Contains` over
/// a query spanning d8's hole matches neither doc -- the query interval sits
/// inside d8's merged SPAN but inside no real member, so this rules out both
/// the span collapse and an "any member overlaps the query" reading.
#[tokio::test]
async fn dynamic_multivalued_contains_spanning_the_hole_matches_neither_doc() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrm_x%20op%3DContains%7D%5B2020-06%20TO%202022-01%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&body), Vec::<String>::new());
    assert_matches_fixture(body, "dr341_multi_no_contains");
}

/// Fixture `dr341_multi_within_one`, finding 168: **only d9**. `Within`
/// requires EVERY member to fit: d8's `2020` member fits the 2020-only query
/// perfectly, but its `2022-05` member does not, so d8 is excluded. This is the
/// case that rules out "any member fits".
#[tokio::test]
async fn dynamic_multivalued_within_requires_every_member_to_fit() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrm_x%20op%3DWithin%7D%5B2020-01-01T00%3A00%3A00Z%20TO%202020-12-31T23%3A59%3A59.999Z%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        ids(&body),
        vec!["d9".to_string()],
        "d8's 2022-05 member does not fit a 2020-only query, so Within must exclude it"
    );
    assert_matches_fixture(body, "dr341_multi_within_one");
}

/// Fixture `dr341_multi_within_both`, finding 168: widening the query until
/// d8's whole union fits brings d8 back alongside d9 -- which makes
/// `dr341_multi_within_one`'s exclusion of d8 attributable to the union rule
/// rather than to multiValued `Within` being broken generally.
#[tokio::test]
async fn dynamic_multivalued_within_matches_both_once_the_query_covers_the_whole_union() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrm_x%20op%3DWithin%7D%5B2019-01-01T00%3A00%3A00Z%20TO%202023-12-31T23%3A59%3A59.999Z%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&body), vec!["d8".to_string(), "d9".to_string()]);
    assert_matches_fixture(body, "dr341_multi_within_both");
}

/// Fixture `dr341_multi_contains_one`, finding 168: `Contains 2020-06` matches
/// BOTH d8 and d9 -- `Contains` is satisfied when the union covers the query,
/// which one member alone can do; it does not demand all members (unlike
/// `Within`).
#[tokio::test]
async fn dynamic_multivalued_contains_inside_one_member_matches_both_docs() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrm_x%20op%3DContains%7D2020-06&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&body), vec!["d8".to_string(), "d9".to_string()]);
    assert_matches_fixture(body, "dr341_multi_contains_one");
}

/// Dynamic mirror of
/// `date_range::multivalued_contains_merges_adjacent_members_into_one_run`.
/// Worth pinning separately: on the dynamic path the members live in two JSON
/// sub-path fast columns, so the merge runs over ordinally paired column values
/// rather than a declared field's two synthetic columns.
///
/// NOT fixture-derived -- see the static twin for the Lucene
/// `ContainsPrefixTreeQuery` reasoning; recorded as inferred, not captured.
#[tokio::test]
async fn dynamic_multivalued_contains_merges_adjacent_members_into_one_run() {
    let dir = TempDir::new().expect("temp dir");
    let app = app_with_schema(dir.path(), DATE_RANGE_DYNAMIC_SCHEMA_TOML)
        .expect("dynamic date_range app must build");
    let (status, body) = post_docs(
        &app,
        &json!([
            {"id":"adj","drm_x":["2010","2011"]},
            {"id":"hole","drm_x":["2010","2012"]}
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrm_x%20op%3DContains%7D%5B2010-06%20TO%202011-06%5D\
         &fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        ids(&body),
        vec!["adj".to_string()],
        "2010 and 2011 are millisecond-adjacent, so their union is one run that \
         contains [2010-06 TO 2011-06]; 2010 + 2012 leaves a 2011 hole the query \
         falls into, {body}"
    );
}

// --- facet / sort / stats asymmetry (finding 172) ----------------------------
//
// Three surfaces, three behaviours: facet 200-with-no-buckets, sort 400, stats
// 400 with its own distinct message. All three are worth pinning on the DYNAMIC
// schema separately from the static one, because in Wayfinder a dynamic name is
// a JSON sub-path inside the `_dynamic` catch-all rather than a top-level field,
// so sort and stats reach it through a different code path than a declared
// `[[fields]]` entry does.
//
// The `error.msg` in `dr341_err_sort`/`dr341_err_stats` names `drs_x` -- and in
// the captured Solr core `drs_x` IS a dynamic field (`capture.sh`'s `#341` block
// declares it with `add-dynamic-field`), so those messages were produced ON this
// path and apply here verbatim. That is exactly why the pair belongs in this
// file and not only in `tests/date_range.rs`.

/// Fixture `dr341_facet_empty`, finding 172: `facet.field` on a `date_range`
/// field is NOT an error -- HTTP 200 with an EMPTY bucket list, over 9 matching
/// docs. On the dynamic path this is the interesting one, because the `_dynamic`
/// catch-all IS fast, so a `date_range` dynamic field must be excluded from
/// faceting deliberately rather than by accident of having no fast column.
#[tokio::test]
async fn dynamic_facet_field_on_date_range_is_200_with_empty_buckets() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=drs_x&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_facet_empty");
}

/// Fixture `dr341_err_sort`, finding 172: `sort` on a `date_range` field is a
/// 400 with Solr's exact spatial-field wording (`Sorting not supported on
/// SpatialField: drs_x, instead try sorting by query.`). The message names
/// `drs_x`, and in the captured core `drs_x` is itself a dynamic field, so this
/// message was produced on the dynamic path -- which is why the assertion holds
/// verbatim here even though Wayfinder reaches a dynamic name as a
/// `_dynamic.<name>` JSON sub-path rather than a top-level field.
#[tokio::test]
async fn dynamic_sort_on_date_range_is_400() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(&app, "select?q=*:*&fl=id&sort=drs_x%20asc&rows=20&wt=json").await;
    assert_error_matches(status, &body, "dr341_err_sort");
}

/// Fixture `dr341_err_stats`, finding 172: `stats.field` on a `date_range`
/// field is a 400 too, but with its own distinct message naming the field TYPE
/// rather than the field -- the asymmetry finding 172 calls out. Same dynamic
/// provenance as `dr341_err_sort` above: `drs_x` was a dynamic field in the
/// captured core.
#[tokio::test]
async fn dynamic_stats_on_date_range_is_400() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&stats=true&stats.field=drs_x&wt=json",
    )
    .await;
    assert_error_matches(status, &body, "dr341_err_stats");
}

// --- error surface: 400 (unparseable) vs 500 (unimplemented op) (finding 170)

/// Fixture `dr341_err_bad_date`, finding 170: a value Solr cannot PARSE is a
/// 400, `msg` = `Couldn't parse date because: Improperly formatted datetime:
/// 2020-13`.
#[tokio::test]
async fn dynamic_unparseable_date_literal_is_400() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A%5B2020-13%20TO%202021%5D&fl=id&wt=json",
    )
    .await;
    assert_error_matches(status, &body, "dr341_err_bad_date");
}

/// Fixture `dr341_err_bad_math`, finding 170: an unparseable date-math
/// expression is also a 400 (parse-kind failure), `msg` = `Invalid Date Math
/// String:'NOW/BOGUS'`.
#[tokio::test]
async fn dynamic_unparseable_date_math_is_400() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A%5BNOW%2FBOGUS%20TO%20NOW%5D&fl=id&wt=json",
    )
    .await;
    assert_error_matches(status, &body, "dr341_err_bad_math");
}

/// Fixture `dr341_err_reversed`, finding 170: a structurally valid but reversed
/// interval is a **500**, not a 400 -- the value parses; the ORDER is what the
/// type cannot handle. `msg` = `Wrong order: 2021 TO 2020`.
#[tokio::test]
async fn dynamic_reversed_interval_is_500_not_400() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A%5B2021%20TO%202020%5D&fl=id&wt=json",
    )
    .await;
    assert_error_matches(status, &body, "dr341_err_reversed");
}

/// Fixture `dr341_err_bad_op`, finding 170: an unrecognised `op` is a 500,
/// `msg` = `Unknown Operation: Bogus`.
#[tokio::test]
async fn dynamic_unknown_op_is_500() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrs_x%20op%3DBogus%7D%5B2020%20TO%202021%5D&fl=id&wt=json",
    )
    .await;
    assert_error_matches(status, &body, "dr341_err_bad_op");
}

/// Fixture `dr341_err_disjoint`, finding 170: `IsDisjointTo` is a valid
/// operation name the type does not implement -- 500, bare `msg` = `Disjoint`.
#[tokio::test]
async fn dynamic_op_disjoint_is_500() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrs_x%20op%3DIsDisjointTo%7D%5B2020-05-01T00%3A00%3A00Z%20TO%202020-07-01T00%3A00%3A00Z%5D&fl=id&wt=json",
    )
    .await;
    assert_error_matches(status, &body, "dr341_err_disjoint");
}

/// Fixture `dr341_err_overlaps`, finding 170: 500, bare `msg` = `Overlaps`.
#[tokio::test]
async fn dynamic_op_overlaps_is_500() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrs_x%20op%3DOverlaps%7D%5B2020-05-01T00%3A00%3A00Z%20TO%202020-07-01T00%3A00%3A00Z%5D&fl=id&wt=json",
    )
    .await;
    assert_error_matches(status, &body, "dr341_err_overlaps");
}

/// Fixture `dr341_err_equals`, finding 170: 500, bare `msg` = `Equals`.
#[tokio::test]
async fn dynamic_op_equals_is_500() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrs_x%20op%3DEquals%7D%5B2020-05-01T00%3A00%3A00Z%20TO%202020-07-01T00%3A00%3A00Z%5D&fl=id&wt=json",
    )
    .await;
    assert_error_matches(status, &body, "dr341_err_equals");
}

// --- a dynamic date_range leaf is one CLAUSE, not the whole query string -----
//
// The dynamic mirror of `tests/date_range.rs`'s per-leaf block. This half is the
// one that failed hardest before per-leaf detection: `rewrite_dynamic_fields`
// turns `drs_x:` into `_dynamic.drs_x:` before the grammar runs, so a clause
// that missed the whole-query-string special case did not even reach the raw
// stored text -- `fq=+drs_x:2020` answered 0 hits. Expected id lists are the
// set-algebra of fixture-pinned leaf results over the same 9-doc corpus.

/// `drs_x:2020 AND id:d5` -- `dr341_single_year` is d1,d2,d3,d4,d7, so the
/// conjunction with d5 is empty and the one with d3 keeps d3.
#[tokio::test]
async fn dynamic_date_range_leaf_conjoined_with_another_clause() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A2020%20AND%20id%3Ad5&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&body), Vec::<String>::new(), "{body}");
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A2020%20AND%20id%3Ad3&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&body), vec!["d3".to_string()], "{body}");
}

/// Two dynamic `date_range` leaves OR-ed: the union of `dr341_single_year`
/// (d1,d2,d3,d4,d7) and `dr341_touch_endpoint` (d1,d5,d7).
#[tokio::test]
async fn dynamic_two_date_range_leaves_disjoined() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A2020%20OR%20drs_x%3A%5B2019-12-31T23%3A59%3A59Z%20TO%202020-01-01T00%3A00%3A00Z%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        ids(&body),
        vec![
            "d1".to_string(),
            "d2".to_string(),
            "d3".to_string(),
            "d4".to_string(),
            "d5".to_string(),
            "d7".to_string()
        ],
        "union of dr341_single_year and dr341_touch_endpoint"
    );
}

/// A parenthesised dynamic leaf answers `dr341_single_year` unchanged.
#[tokio::test]
async fn dynamic_parenthesised_date_range_leaf() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%28drs_x%3A2020%29&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Only the echoed `q` differs from `dr341_single_year`'s envelope (the
    // parentheses), so the id list is asserted rather than the whole fixture.
    assert_eq!(
        ids(&body),
        vec![
            "d1".to_string(),
            "d2".to_string(),
            "d3".to_string(),
            "d4".to_string(),
            "d7".to_string()
        ],
        "dr341_single_year's set, parenthesised"
    );
    assert_eq!(body["response"]["numFound"].as_u64(), Some(5), "{body}");
}

/// `fq=+drs_x:2020` on the dynamic path -- the row that answered 0 hits before,
/// because the `+`-prefixed name missed the whole-query special case and the
/// rewritten `_dynamic.drs_x` JSON path holds the interval endpoints, not the
/// raw text.
#[tokio::test]
async fn dynamic_required_occur_date_range_leaf_as_fq() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&fq=%2Bdrs_x%3A2020&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        ids(&body),
        vec![
            "d1".to_string(),
            "d2".to_string(),
            "d3".to_string(),
            "d4".to_string(),
            "d7".to_string()
        ],
        "dr341_single_year's set, reached as a `+`-prefixed dynamic fq clause"
    );
}

/// `fq=-drs_x:2020` on the dynamic path: the corpus minus `dr341_single_year`.
#[tokio::test]
async fn dynamic_excluded_occur_date_range_leaf_as_fq() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&fq=-drs_x%3A2020&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        ids(&body),
        vec![
            "d5".to_string(),
            "d6".to_string(),
            "d8".to_string(),
            "d9".to_string()
        ],
        "the 9-doc corpus minus dr341_single_year's five docs"
    );
}

/// A dynamic `date_range` leaf under `defType=edismax`.
#[tokio::test]
async fn dynamic_date_range_leaf_under_edismax() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?defType=edismax&q=drs_x%3A2020&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        ids(&body),
        vec![
            "d1".to_string(),
            "d2".to_string(),
            "d3".to_string(),
            "d4".to_string(),
            "d7".to_string()
        ],
        "dr341_single_year's set, reached through the edismax parser"
    );
}

/// The multiValued dynamic field inside a compound query: the union of
/// `dr341_multi_intersects` (d8) and `id:d9`.
#[tokio::test]
async fn dynamic_multivalued_date_range_leaf_inside_a_compound_query() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drm_x%3A2022-05%20OR%20id%3Ad9&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        ids(&body),
        vec!["d8".to_string(), "d9".to_string()],
        "union of dr341_multi_intersects (d8) and id:d9"
    );
}

// --- must-fix 1 / must-fix 3 on the dynamic path ------------------------------

/// The dynamic mirror of the index-path panic regression: a `NOW`-prefixed value
/// whose next character is multi-byte UTF-8 is a 400, and the core stays
/// writable afterwards (the panic used to fire under the index-writer lock).
#[tokio::test]
async fn dynamic_non_ascii_date_math_on_the_index_path_is_400_and_leaves_the_core_writable() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = post_docs(&app, &json!([{"id":"bad","drs_x":"NOW\u{e9}"}])).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an unparseable date-math value must be a 400, not a panic: {body}"
    );
    let (status, body) = post_docs(&app, &json!([{"id":"ok","drs_x":"2022"}])).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a clean doc indexed after the rejected one must still succeed: {body}"
    );
    let (status, body) = get(&app, "select?q=id%3Aok&fl=id&rows=20&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&body), vec!["ok".to_string()], "{body}");
}

/// The same value on the dynamic QUERY path is the finding-170 400.
#[tokio::test]
async fn dynamic_non_ascii_date_math_on_the_query_path_is_400() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(&app, "select?q=drs_x%3ANOW%C3%A9&fl=id&wt=json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"].as_u64(), Some(400), "{body}");
    assert_eq!(
        body["error"]["msg"].as_str(),
        Some("Invalid Date Math String:'NOW\u{e9}'"),
        "{body}"
    );
}

/// The far-future open-ended sentinel clamps on the dynamic path too, both as an
/// indexed value (still round-tripping verbatim, finding 165) and as a query
/// endpoint (answering `dr341_star_both`'s d1-d7).
#[tokio::test]
async fn dynamic_out_of_range_endpoints_clamp() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A%5B*%20TO%209999-12-31T23%3A59%3A59Z%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        ids(&body),
        vec![
            "d1".to_string(),
            "d2".to_string(),
            "d3".to_string(),
            "d4".to_string(),
            "d5".to_string(),
            "d6".to_string(),
            "d7".to_string()
        ],
        "clamped to MAX_MS, so identical to dr341_star_both's `[* TO *]`"
    );

    let dir = TempDir::new().expect("temp dir");
    let app = app_with_schema(dir.path(), DATE_RANGE_DYNAMIC_SCHEMA_TOML)
        .expect("dynamic date_range app must build");
    let (status, body) = post_docs(
        &app,
        &json!([{"id":"s1","drs_x":"[* TO 9999-12-31T23:59:59Z]"}]),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an out-of-range endpoint must clamp, not fail the update: {body}"
    );
    let (status, body) = get(&app, "select?q=*:*&fl=id,drs_x&rows=20&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["response"]["docs"][0]["drs_x"].as_str(),
        Some("[* TO 9999-12-31T23:59:59Z]"),
        "finding 165: the sentinel still round-trips verbatim, {body}"
    );
}

// --- round-3 review: the `qf` path on a dynamic date_range field ------------

/// The dynamic mirror of
/// `tests/date_range.rs::field_less_literal_in_qf_is_an_interval_query`. A
/// field-less literal under `edismax` is routed to the `qf` disjunction rather
/// than to `build_leaf`, so #341's per-leaf interception does not cover it; on
/// the dynamic path the term query it built instead went against the
/// `_dynamic_text` JSON container and matched nothing at all. Solr routes a `qf`
/// field through `FieldType::getFieldQuery`, i.e. the interval query, so this is
/// `dr341_single_year`'s set.
#[tokio::test]
async fn dynamic_field_less_literal_in_qf_is_an_interval_query() {
    let (app, _dir) = dynamic_date_range_app().await;
    let (status, body) = get(
        &app,
        "select?defType=edismax&qf=drs_x&q=2020&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        ids(&body),
        vec![
            "d1".to_string(),
            "d2".to_string(),
            "d3".to_string(),
            "d4".to_string(),
            "d7".to_string()
        ],
        "a dynamic date_range field in `qf` must be the interval query: {body}"
    );
}

/// The dynamic mirror of
/// `tests/date_range.rs::far_future_truncated_endpoints_do_not_invert_the_interval`:
/// the endpoint clamp is applied when the interval is parsed, so it must hold
/// identically on the dynamic path, which reads its endpoints back out of the
/// `_dynamic` JSON columns rather than a declared field's own.
#[tokio::test]
async fn dynamic_far_future_truncated_endpoints_do_not_invert_the_interval() {
    let (app, _dir) = dynamic_date_range_app().await;
    for query in [
        "drs_x%3A%5B3000%20TO%203001%5D",
        "drs_x%3A%5B2300%20TO%202400%5D",
        "drs_x%3A%5B2262-05%20TO%202262-06%5D",
    ] {
        let (status, body) = get(
            &app,
            &format!("select?q={query}&fl=id&sort=id%20asc&rows=20&wt=json"),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "`{query}` is correctly ordered and must not report Wrong order: {body}"
        );
        assert_eq!(
            ids(&body),
            vec!["d6".to_string(), "d7".to_string()],
            "`{query}` clamps to the point interval at MAX_MS: {body}"
        );
    }
}
