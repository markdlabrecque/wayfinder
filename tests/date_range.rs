//! Issue #341 -- the server half of `solr.DateRangeField`: a new `date_range`
//! field type with `Intersects`/`Contains`/`Within`/`IsWithin` interval
//! predicates, millisecond-precision expansion, multiValued union semantics,
//! date math, and the 400/500 error split.
//!
//! Ground truth is `solr-ref/responses/dr341_*.json`, captured against a
//! dedicated `daterange` Solr core (`solr-ref/capture.sh`'s `#341` block) and
//! indexed in `solr-ref/manifest-errors.tsv` (own core, so not a
//! `content`-core-relative GET). Findings 165-172 in
//! `docs/solr-ref-findings.md` are the authoritative prose; every assertion
//! below traces to one of them, cited per test.
//!
//! This file builds its own dedicated schema/corpus (named `content` so
//! `common::get`/`common::post_docs`/`common::CORE` address it unchanged --
//! the same trick `tests/grouping.rs`/`tests/stats.rs`/`tests/spatial.rs` use)
//! rather than wiring a `daterange_app` into the generic
//! `tests/differential.rs` manifest-errors dispatcher: the five `dr341_err_*`
//! 500-error fixtures carry a full Java stack trace under `error.trace` that
//! this compatibility contract does not pin at all (finding 10: only
//! `error.code`/HTTP status, plus here `error.msg` per finding 170, are in
//! scope) -- a generic byte-for-byte differ against those fixtures could never
//! pass even for a correct implementation, so they need the narrower,
//! trace-blind comparison this file's `assert_error_matches` provides.

// The `dead_code` allow for the shared helpers is an inner attribute inside
// `tests/common/mod.rs`; do not add a second one here (clippy rejects it under
// `-D warnings`).
mod common;

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;

use common::{app_with_schema, assert_matches_fixture, fixture, get, post_docs};

/// A single-valued `drs_x` (`date_range`) and a multiValued `drm_x`
/// (`date_range` + `multi_valued = true`), both `stored = true` and NOT
/// `fast` -- the design decision this issue makes (mirroring the captured
/// Drupal configset: `indexed="true" stored="true"`, no `docValues`). Named
/// `content` so `common::get`/`common::CORE` need no change.
const DATE_RANGE_SCHEMA_TOML: &str = r#"
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
name = "drs_x"
type = "date_range"
stored = true

[[fields]]
name = "drm_x"
type = "date_range"
multi_valued = true
stored = true
"#;

/// The exact 9-doc corpus `solr-ref/capture.sh`'s `#341` block indexes (see
/// the module doc on `capdr341`/the corpus comment there). d1/d2 are bare
/// literals at year/month precision; d3 an explicit closed interval; d4 a
/// full-instant literal (whole second); d5/d6 open-ended; d7 fully open; d8
/// a multiValued field with two disjoint members (a hole covering 2021); d9
/// the same field with a single member.
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

async fn date_range_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app =
        app_with_schema(dir.path(), DATE_RANGE_SCHEMA_TOML).expect("date_range app must build");
    let (status, body) = post_docs(&app, &date_range_corpus()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the date-range corpus must succeed, got {body}"
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

/// Compares only HTTP status, `error.code`, and `error.msg` against
/// `fixture_name` -- never `error.metadata`/`error.trace`, which are
/// Java-internal class names and a JVM stack trace Wayfinder cannot and need
/// not reproduce (finding 10: only `error.code`/HTTP status is part of the
/// compatibility contract). `error.msg` is asserted verbatim here
/// specifically because finding 170 pins these exact strings as the
/// discriminating evidence for the 400/500 split, the same reasoning
/// `tests/sort.rs::direction_error_messages_match_solr_verbatim_including_pos`
/// uses for the sort direction message.
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
}

// --- storage: verbatim round-trip (finding 165) -----------------------------

/// `"2020"` round-trips as `"2020"`, not an expanded interval; `drm_x`
/// returns its members in input order. Pinned against the exact captured
/// envelope (`dr341_roundtrip.json`), params included.
#[tokio::test]
async fn roundtrip_stores_the_value_verbatim() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&fl=id,drs_x,drm_x&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_roundtrip");
}

// --- single-valued predicates (finding 167) ---------------------------------

/// Plain `drs_x:[a TO b]` is `Intersects` by default -> d1,d2,d3,d4,d7 (d5
/// ends in 2019, d6 starts in 2021, both excluded).
#[tokio::test]
async fn plain_range_query_is_intersects_by_default() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A%5B2020-05-01T00%3A00%3A00Z%20TO%202020-07-01T00%3A00%3A00Z%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_intersects_plain");
}

/// `{!field f=drs_x op=Intersects}` is the exact same query as the plain form
/// above.
#[tokio::test]
async fn explicit_op_intersects_matches_plain_form() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrs_x%20op%3DIntersects%7D%5B2020-05-01T00%3A00%3A00Z%20TO%202020-07-01T00%3A00%3A00Z%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_op_intersects");
}

/// `{!field f=drs_x}` with NO `op` at all also defaults to `Intersects`.
#[tokio::test]
async fn field_query_parser_with_no_op_defaults_to_intersects() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrs_x%7D2020-06&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_op_default");
}

/// `Contains` (doc interval covers the whole query) -> d1,d3,d7. Not the
/// complement of `Within` over the same query (see the next test).
#[tokio::test]
async fn op_contains_requires_the_doc_to_cover_the_query_interval() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrs_x%20op%3DContains%7D%5B2020-05-01T00%3A00%3A00Z%20TO%202020-07-01T00%3A00%3A00Z%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_op_contains");
}

/// `Within` (doc interval fits inside the query) -> d2,d4 -- disjoint from
/// `Contains`'s d1,d3,d7 over the identical query interval, proving the two
/// ops are not complements.
#[tokio::test]
async fn op_within_requires_the_doc_to_fit_inside_the_query_interval() {
    let (app, _dir) = date_range_app().await;
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

/// `IsWithin` is an accepted alias of `Within` -- identical result set.
#[tokio::test]
async fn op_iswithin_is_an_alias_of_within() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrs_x%20op%3DIsWithin%7D%5B2020-05-01T00%3A00%3A00Z%20TO%202020-07-01T00%3A00%3A00Z%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_op_iswithin");
}

/// The `op` value is matched case-INSENSITIVELY: `op=contains` behaves
/// exactly like `op=Contains`.
#[tokio::test]
async fn op_value_is_matched_case_insensitively() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrs_x%20op%3Dcontains%7D%5B2020-05-01T00%3A00%3A00Z%20TO%202020-07-01T00%3A00%3A00Z%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_op_lowercase");
}

// --- bare-literal precision expansion (finding 166) -------------------------

/// A bare `drs_x:2020` is an interval query at year precision, not a term
/// query -- it intersects the same five docs as the explicit May-Jul 2020
/// range above.
#[tokio::test]
async fn bare_year_literal_expands_to_the_whole_year_interval() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A2020&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_single_year");
}

/// Same rule at month precision: `drs_x:2020-06`.
#[tokio::test]
async fn bare_month_literal_expands_to_the_whole_month_interval() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A2020-06&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_single_month");
}

/// `[* TO *]` matches every doc that HAS the field: d1-d7 (not d8/d9, which
/// only carry `drm_x`).
#[tokio::test]
async fn fully_open_interval_matches_every_doc_with_the_field() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A%5B*%20TO%20*%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_star_both");
}

/// Millisecond-resolution, end-INCLUSIVE precision. d2's `"2020-06"` expands
/// to end `.999Z`: a `Within` query ending exactly there contains it, but one
/// ending one millisecond earlier (`.998Z`) does not. This pair is what pins
/// the expansion to millisecond rather than second resolution.
#[tokio::test]
async fn within_query_ending_exactly_at_ms_boundary_contains_the_bare_month_literal() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrs_x%20op%3DWithin%7D%5B2020-06-01T00%3A00%3A00Z%20TO%202020-06-30T23%3A59%3A59.999Z%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&body), vec!["d2".to_string(), "d4".to_string()]);
    assert_matches_fixture(body, "dr341_within_ms_exact");
}

#[tokio::test]
async fn within_query_one_ms_short_of_the_boundary_excludes_the_bare_month_literal() {
    let (app, _dir) = date_range_app().await;
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

/// The same rule applied to an interval ENDPOINT, not just a bare literal:
/// d5 is `[* TO 2019-12-31T23:59:59Z]`, i.e. that whole second, so a query
/// starting exactly at that second still intersects it, AND one starting one
/// millisecond past it also still intersects (both fixtures -> d1,d5,d7).
#[tokio::test]
async fn interval_endpoint_precision_touches_the_whole_stated_second() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A%5B2019-12-31T23%3A59%3A59Z%20TO%202020-01-01T00%3A00%3A00Z%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_touch_endpoint");
}

#[tokio::test]
async fn interval_endpoint_precision_still_touches_one_ms_past_the_second() {
    let (app, _dir) = date_range_app().await;
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

// --- exclusive-brace syntax is accepted and ignored (finding 169) -----------

/// `{a TO b}` is accepted and returns EXACTLY what `[a TO b]` returns --
/// `DateRangeField` parses the interval string itself and has no notion of
/// an exclusive endpoint. An implementation that routes the clause through a
/// Lucene-style range parser before consulting the field type would get this
/// wrong (e.g. by 400ing on the brace, or silently excluding an endpoint).
#[tokio::test]
async fn exclusive_brace_syntax_is_silently_treated_as_inclusive() {
    let (app, _dir) = date_range_app().await;
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

// --- multiValued: union-of-members set relations (finding 168) -------------

/// A single member's intersect (`drm_x:2022-05`) -> only d8 (d9 has no
/// `2022-05` member).
#[tokio::test]
async fn multivalued_intersects_a_query_matching_one_member() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drm_x%3A2022-05&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_multi_intersects");
}

#[tokio::test]
async fn multivalued_contains_a_query_matching_one_member() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrm_x%20op%3DContains%7D2022-05&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_multi_contains");
}

/// The decisive case: a query landing in d8's HOLE (the gap between its two
/// members, 2021) must intersect NEITHER doc -- 0 results. A "min(start)..
/// max(end)" span collapse would wrongly treat d8 as one continuous interval
/// covering the hole and match it; this is the fixture that catches that bug.
#[tokio::test]
async fn multivalued_intersects_is_hole_sensitive_not_a_min_max_span() {
    let (app, _dir) = date_range_app().await;
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

/// `Contains` spanning d8's hole must match NEITHER doc -- proves `Contains`
/// is not satisfied by "any member contains part of the query", and not by a
/// min/max span collapse either (the query interval is inside the SPAN
/// [2020-01,2022-05] but not inside either real member).
#[tokio::test]
async fn multivalued_contains_spanning_the_hole_matches_neither_doc() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrm_x%20op%3DContains%7D%5B2020-06%20TO%202022-01%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&body), Vec::<String>::new());
    assert_matches_fixture(body, "dr341_multi_no_contains");
}

/// `Contains` on a query that fits inside ONE member matches BOTH d8 and d9
/// -- confirms `Contains` does not require every member to contain the
/// query, just at least one (unlike `Within`, tested next).
#[tokio::test]
async fn multivalued_contains_inside_one_member_matches_both_docs() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrm_x%20op%3DContains%7D2020-06&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&body), vec!["d8".to_string(), "d9".to_string()]);
    assert_matches_fixture(body, "dr341_multi_contains_one");
}

/// `Within` requires EVERY member to fit inside the query: a 2020-only query
/// matches d9 alone even though d8's `2020` member fits perfectly (d8's OTHER
/// member, `2022-05`, does not) -- the case that rules out "any member
/// fits".
#[tokio::test]
async fn multivalued_within_requires_every_member_to_fit() {
    let (app, _dir) = date_range_app().await;
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

/// Widening the query until d8's WHOLE union fits brings d8 back alongside
/// d9 -- confirms `Within` reduces to `min(start) >= qStart AND max(end) <=
/// qEnd` over the doc's member set.
#[tokio::test]
async fn multivalued_within_matches_both_once_the_query_covers_the_whole_union() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrm_x%20op%3DWithin%7D%5B2019-01-01T00%3A00%3A00Z%20TO%202023-12-31T23%3A59%3A59.999Z%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&body), vec!["d8".to_string(), "d9".to_string()]);
    assert_matches_fixture(body, "dr341_multi_within_both");
}

// --- date math (finding 171) -------------------------------------------------

/// `NOW/YEAR` and `NOW/YEAR+1YEAR` resolve, and the resulting interval
/// matches d6 (`[2021-01-01T00:00:00Z TO *]`) and d7 (fully open) -- true for
/// any `NOW` through the year 2100, per the fixture's own choice of corpus
/// (see `capture.sh`'s comment on this pair).
#[tokio::test]
async fn date_math_now_slash_year_resolves_in_a_date_range_query() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A%5BNOW%2FYEAR%20TO%20NOW%2FYEAR%2B1YEAR%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&body), vec!["d6".to_string(), "d7".to_string()]);
    assert_matches_fixture(body, "dr341_datemath_year");
}

/// `NOW-100YEARS TO NOW` resolves and matches all 7 single-valued docs --
/// true until the year 2119, per the fixture's own choice.
#[tokio::test]
async fn date_math_now_minus_years_resolves_in_a_date_range_query() {
    let (app, _dir) = date_range_app().await;
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

// --- facet/sort/stats asymmetry (finding 172) -------------------------------

/// `facet.field` on a `date_range` field is NOT an error -- 200 with an
/// EMPTY bucket list, over 9 matching docs.
#[tokio::test]
async fn facet_field_on_date_range_is_200_with_empty_buckets() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=drs_x&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "dr341_facet_empty");
}

/// `sort` on a `date_range` field is a 400, with Solr's exact spatial-field
/// wording.
#[tokio::test]
async fn sort_on_date_range_is_400() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(&app, "select?q=*:*&fl=id&sort=drs_x%20asc&rows=20&wt=json").await;
    assert_error_matches(status, &body, "dr341_err_sort");
}

/// `stats` on a `date_range` field is a 400 too, but with its own distinct
/// message naming the field type -- the asymmetry finding 172 calls out:
/// three surfaces (facet/sort/stats), three different behaviours.
#[tokio::test]
async fn stats_on_date_range_is_400() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&stats=true&stats.field=drs_x&wt=json",
    )
    .await;
    assert_error_matches(status, &body, "dr341_err_stats");
}

// --- error surface: 400 (unparseable) vs 500 (unimplemented op) (finding 170)

/// A value Solr cannot PARSE at all is a 400.
#[tokio::test]
async fn unparseable_date_literal_is_400() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A%5B2020-13%20TO%202021%5D&fl=id&wt=json",
    )
    .await;
    assert_error_matches(status, &body, "dr341_err_bad_date");
}

/// An unparseable date-math expression is also a 400 (parse-kind failure,
/// same as the bad literal above -- not a 500).
#[tokio::test]
async fn unparseable_date_math_is_400() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A%5BNOW%2FBOGUS%20TO%20NOW%5D&fl=id&wt=json",
    )
    .await;
    assert_error_matches(status, &body, "dr341_err_bad_math");
}

/// A structurally valid but reversed interval (`2021 TO 2020`) is a 500, not
/// a 400 -- the value parses fine, the ORDER is what `DateRangeField` cannot
/// handle. This is the finding-170 case that most easily gets miscategorised
/// as a plain parse error.
#[tokio::test]
async fn reversed_interval_is_500_not_400() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A%5B2021%20TO%202020%5D&fl=id&wt=json",
    )
    .await;
    assert_error_matches(status, &body, "dr341_err_reversed");
}

/// An unrecognised `op` value is a 500, bare `msg` naming it.
#[tokio::test]
async fn unknown_op_is_500() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrs_x%20op%3DBogus%7D%5B2020%20TO%202021%5D&fl=id&wt=json",
    )
    .await;
    assert_error_matches(status, &body, "dr341_err_bad_op");
}

/// `IsDisjointTo` is structurally valid but unimplemented by `DateRangeField`
/// -- 500, bare `msg` = `Disjoint`.
#[tokio::test]
async fn op_disjoint_is_500() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrs_x%20op%3DIsDisjointTo%7D%5B2020-05-01T00%3A00%3A00Z%20TO%202020-07-01T00%3A00%3A00Z%5D&fl=id&wt=json",
    )
    .await;
    assert_error_matches(status, &body, "dr341_err_disjoint");
}

/// `Overlaps` -- 500, bare `msg` = `Overlaps`.
#[tokio::test]
async fn op_overlaps_is_500() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrs_x%20op%3DOverlaps%7D%5B2020-05-01T00%3A00%3A00Z%20TO%202020-07-01T00%3A00%3A00Z%5D&fl=id&wt=json",
    )
    .await;
    assert_error_matches(status, &body, "dr341_err_overlaps");
}

/// `Equals` -- 500, bare `msg` = `Equals`.
#[tokio::test]
async fn op_equals_is_500() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21field%20f%3Ddrs_x%20op%3DEquals%7D%5B2020-05-01T00%3A00%3A00Z%20TO%202020-07-01T00%3A00%3A00Z%5D&fl=id&wt=json",
    )
    .await;
    assert_error_matches(status, &body, "dr341_err_equals");
}

// --- fl=* must not leak the synthetic __start/__end keys --------------------

/// The two synthetic per-doc keys backing the interval columns
/// (`drs_x__start`/`drs_x__end`) must never appear in `fl=*` output --
/// mirroring how `location`'s `__lat`/`__lon` synthetic columns are filtered
/// (`src/core_index.rs:2718`). Not derived from a fixture (Solr has no
/// synthetic columns at all here, so there is nothing to capture this
/// against): this is a Wayfinder-internal storage-leak guard.
#[tokio::test]
async fn fl_star_does_not_leak_synthetic_start_end_keys() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(&app, "select?q=id%3Ad3&fl=*&sort=id%20asc&rows=1&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let doc = &body["response"]["docs"][0];
    let obj = doc.as_object().expect("doc must be a JSON object");
    for key in obj.keys() {
        assert!(
            !key.ends_with("__start") && !key.ends_with("__end"),
            "fl=* must not leak a synthetic column key, got `{key}` in doc: {doc}"
        );
    }
    assert_eq!(
        doc.get("drs_x").and_then(Value::as_str),
        Some("[2020-03-01T00:00:00Z TO 2020-09-30T00:00:00Z]"),
        "the stored field itself must still render, verbatim, in fl=*: {doc}"
    );
}
