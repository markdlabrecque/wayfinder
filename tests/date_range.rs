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
//! `error.code`/HTTP status, plus here `error.msg` per finding 184, are in
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
/// specifically because finding 184 pins these exact strings as the
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
        "{fixture_name}: error.msg must match Solr verbatim (finding 184), body: {body}"
    );
    // The presence or absence of a `response` block is part of the captured
    // envelope, not decoration: every `dr341_err_*` fixture has keys
    // `["responseHeader","error"]` only, and `dr341_err_stats` is the fixture
    // that makes it load-bearing -- Solr raises the stats refusal *before*
    // running the base query, which is the whole reason
    // `stats::PreQueryStatsError` exists (`src/stats.rs`, `src/lib.rs`'s
    // `stats_result` arm). Without this assertion that machinery can be
    // deleted with the suite still green.
    assert_eq!(
        body.get("response").is_some(),
        expected.get("response").is_some(),
        "{fixture_name}: a `response` block must be present exactly when the \
         captured envelope has one, body: {body}"
    );
}

// --- storage: verbatim round-trip (finding 179) -----------------------------

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

// --- single-valued predicates (finding 181) ---------------------------------

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

// --- bare-literal precision expansion (finding 180) -------------------------

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

// --- exclusive-brace syntax is accepted and ignored (finding 183) -----------

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

// --- multiValued: union-of-members set relations (finding 182) -------------

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

/// End-to-end counterpart of `date_range::tests::
/// contains_merges_adjacent_members_into_one_run`: `Contains` is a relation
/// against the UNION of the members (finding 182), so two millisecond-adjacent
/// members form ONE contiguous run and a query straddling their boundary is
/// contained by it -- while the same query against members with a real hole
/// between them is not. The committed corpus has no adjacent-member document,
/// so this indexes its own two docs.
///
/// NOT fixture-derived: no `dr341_*` capture exercises adjacent members. The
/// expectation comes from Lucene's `ContainsPrefixTreeQuery`, which for a
/// multi-valued shape sets `multiOverlappingIndexedShapes` and therefore tests
/// containment against the merged union rather than member by member. Recorded
/// as inferred, not captured.
#[tokio::test]
async fn multivalued_contains_merges_adjacent_members_into_one_run() {
    let dir = TempDir::new().expect("temp dir");
    let app =
        app_with_schema(dir.path(), DATE_RANGE_SCHEMA_TOML).expect("date_range app must build");
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

// --- date math (finding 185) -------------------------------------------------

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

// --- facet/sort/stats asymmetry (finding 186) -------------------------------

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
/// message naming the field type -- the asymmetry finding 186 calls out:
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

// --- error surface: 400 (unparseable) vs 500 (unimplemented op) (finding 184)

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
/// handle. This is the finding-184 case that most easily gets miscategorised
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

// --- a date_range leaf is one CLAUSE, not the whole query string ------------
//
// Round-2 review: detection used to be a whole-`q` special case (split on the
// first `:`, hand the rest to the interval parser), so any `date_range` clause
// that was not the entire query string was either a spurious 400 or a silently
// wrong term query against the verbatim stored text -- the exact failure mode
// `parse_query`'s own doc comment already records for another feature. Every
// expected id list below is the set-algebra of fixture-pinned leaf results over
// the 9-doc corpus; each test names its derivation.

/// `drs_x:2020 AND id:d5` -- `dr341_single_year` is d1,d2,d3,d4,d7, which does
/// not contain d5, so the conjunction is empty. The old whole-string special
/// case 400ed here (`Couldn't parse date ...: 2020 AND id:d5`).
#[tokio::test]
async fn date_range_leaf_conjoined_with_another_clause() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A2020%20AND%20id%3Ad5&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        ids(&body),
        Vec::<String>::new(),
        "d5 is not in dr341_single_year's d1,d2,d3,d4,d7, so the AND is empty"
    );
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A2020%20AND%20id%3Ad3&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        ids(&body),
        vec!["d3".to_string()],
        "d3 IS in dr341_single_year, so the AND keeps it -- the interval \
         predicate must really have run, not matched everything"
    );
}

/// Two `date_range` leaves OR-ed together: the union of `dr341_single_year`
/// (d1,d2,d3,d4,d7) and `dr341_touch_endpoint` (d1,d5,d7), both fixture-pinned,
/// so the expected set is purely fixture-derived.
#[tokio::test]
async fn two_date_range_leaves_disjoined() {
    let (app, _dir) = date_range_app().await;
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
        "union of dr341_single_year (d1,d2,d3,d4,d7) and dr341_touch_endpoint (d1,d5,d7)"
    );
}

/// A parenthesised leaf. The grammar flattens `(drs_x:2020)` to the same single
/// leaf, so the answer must be `dr341_single_year` unchanged -- the old
/// whole-string special case fell through to the Lucene grammar here and built
/// a term query against the raw stored text, matching only d1 (whose stored
/// string is literally `2020`).
#[tokio::test]
async fn parenthesised_date_range_leaf() {
    let (app, _dir) = date_range_app().await;
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

/// A `+`-prefixed required leaf as an `fq`. Same set as the bare form; the old
/// code path split the field name as `+drs_x`, found no `date_range` field, and
/// fell through to the raw-text term query (d1 alone).
#[tokio::test]
async fn required_occur_date_range_leaf_as_fq() {
    let (app, _dir) = date_range_app().await;
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
        "dr341_single_year's set, reached as a `+`-prefixed fq clause"
    );
}

/// A `-`-prefixed excluded leaf as an `fq`: the complement of
/// `dr341_single_year` over the 9-doc corpus, i.e. d5,d6,d8,d9.
#[tokio::test]
async fn excluded_occur_date_range_leaf_as_fq() {
    let (app, _dir) = date_range_app().await;
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
        "the 9-doc corpus minus dr341_single_year's d1,d2,d3,d4,d7 -- a \
         negated interval clause must exclude all five, not just the doc whose \
         stored text is literally `2020`"
    );
}

/// Under `defType=edismax` a *fielded* leaf still routes through the same
/// per-leaf builder, so a `date_range` clause answers identically to the
/// lucene parser's (`dr341_single_year`).
#[tokio::test]
async fn date_range_leaf_under_edismax() {
    let (app, _dir) = date_range_app().await;
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

/// A field-LESS leaf whose field comes from `df`: `q=2020&df=drs_x` is the same
/// interval query as `q=drs_x:2020` (`dr341_single_year`).
#[tokio::test]
async fn date_range_leaf_via_the_default_field_param() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=2020&df=drs_x&fl=id&sort=id%20asc&rows=20&wt=json",
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
        "dr341_single_year's set, reached with the field supplied by `df`"
    );
}

/// The same per-leaf reach for the multiValued field, inside a compound query:
/// `dr341_multi_gap` (0 hits, d8's hole) AND-ed with nothing else still 0, and
/// `dr341_multi_intersects` (d8) OR-ed with an id clause adds d9.
#[tokio::test]
async fn multivalued_date_range_leaf_inside_a_compound_query() {
    let (app, _dir) = date_range_app().await;
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

// --- must-fix 1: a non-ASCII byte after `NOW` must not panic -----------------

/// A `NOW`-prefixed value whose next character is multi-byte UTF-8 is the
/// finding-184 parse 400, never a panic. On the INDEX path the panic fired
/// while the index-writer lock was held, poisoning it and bricking every later
/// write to the core -- so the clean doc indexed *after* the rejected one is the
/// regression this test exists for.
#[tokio::test]
async fn non_ascii_date_math_on_the_index_path_is_400_and_leaves_the_core_writable() {
    let (app, _dir) = date_range_app().await;
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
        "a clean doc indexed after the rejected one must still succeed -- a \
         panic under the index-writer lock poisons it for the process lifetime: {body}"
    );
    let (status, body) = get(&app, "select?q=id%3Aok&fl=id&rows=20&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&body), vec!["ok".to_string()], "{body}");
}

/// The same value on the QUERY path is the finding-184 400 with the date-math
/// message shape, not the 500 a caught panic produced.
#[tokio::test]
async fn non_ascii_date_math_on_the_query_path_is_400() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(&app, "select?q=drs_x%3ANOW%C3%A9&fl=id&wt=json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"].as_u64(), Some(400), "{body}");
    assert_eq!(
        body["error"]["msg"].as_str(),
        Some("Invalid Date Math String:'NOW\u{e9}'"),
        "finding 184: an unparseable date-math expression is a 400 quoting the \
         whole expression, body: {body}"
    );
}

// --- must-fix 3: out-of-representable-range endpoints clamp ------------------

/// `9999-12-31T23:59:59Z` is the standard Solr / Search API open-ended
/// sentinel. `tantivy::DateTime` is i64 NANOseconds, so it cannot represent
/// that instant at all; `MIN_MS`/`MAX_MS` clamp such an endpoint to the
/// representable bound rather than rejecting the value, which is what the
/// constants' own doc comment promises and what keeps the sentinel
/// interoperable. NOT fixture-derived -- Solr has no such limit, so there is
/// nothing to capture this against; it pins Wayfinder's documented clamp.
#[tokio::test]
async fn out_of_range_endpoints_clamp_on_the_index_path() {
    let dir = TempDir::new().expect("temp dir");
    let app =
        app_with_schema(dir.path(), DATE_RANGE_SCHEMA_TOML).expect("date_range app must build");
    let (status, body) = post_docs(
        &app,
        &json!([
            {"id":"s1","drs_x":"[* TO 9999-12-31T23:59:59Z]"},
            {"id":"s2","drs_x":"0001-01-01T00:00:00Z"}
        ]),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an out-of-range endpoint must clamp, not fail the update: {body}"
    );
    let (status, body) = get(
        &app,
        "select?q=*:*&fl=id,drs_x&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["response"]["docs"][0]["drs_x"].as_str(),
        Some("[* TO 9999-12-31T23:59:59Z]"),
        "finding 179: the sentinel still round-trips verbatim, {body}"
    );
}

/// The query-path half: a far-future sentinel upper bound clamps to the same
/// open bound `*` resolves to, so the interval answers exactly what
/// `dr341_star_both` pins for `[* TO *]` (d1-d7).
#[tokio::test]
async fn far_future_sentinel_endpoint_answers_as_the_open_bound() {
    let (app, _dir) = date_range_app().await;
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
}

/// And the too-EARLY endpoint: `0001-01-01T00:00:00Z` clamps to `MIN_MS`, so
/// `[0001-01-01T00:00:00Z TO 2019-12-31T23:59:59Z]` answers as
/// `[* TO 2019-12-31T23:59:59Z]` does -- d5 (which ends in that second) and d7
/// (fully open). Derived from the 9-doc corpus plus finding 180, not from a
/// fixture of its own.
#[tokio::test]
async fn far_past_sentinel_endpoint_answers_as_the_open_bound() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A%5B0001-01-01T00%3A00%3A00Z%20TO%202019-12-31T23%3A59%3A59Z%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        ids(&body),
        vec!["d5".to_string(), "d7".to_string()],
        "clamped to MIN_MS, so identical to `[* TO 2019-12-31T23:59:59Z]`"
    );
}

// --- mixed inclusive/exclusive brackets (inferred, not captured) -------------

/// `[a TO b}` -- a mixed bracket pair. Finding 169 pins that `DateRangeField`
/// ignores endpoint exclusivity entirely (`{a TO b}` == `[a TO b]`), and the
/// classic Lucene grammar accepts a mixed pair, so the mixed form must answer
/// exactly what the closed form does.
///
/// This test encodes an INFERRED rule, not a captured one: no `dr341_*` fixture
/// sends a mixed pair. It is derived from finding 183 (exclusivity is silently
/// ignored) plus the grammar's own acceptance of the mixed form, and its
/// expected value is `dr341_intersects_plain`'s.
#[tokio::test]
async fn mixed_bracket_pair_behaves_like_the_closed_form() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A%5B2020-05-01T00%3A00%3A00Z%20TO%202020-07-01T00%3A00%3A00Z%7D&fl=id&sort=id%20asc&rows=20&wt=json",
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
        "must be identical to dr341_intersects_plain's closed-bracket result set"
    );
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

// --- round-3 review: three defects the round-2 fixes left reachable ---------

/// Round 2 fixed the `split_at(1)` panic on a multi-byte character after `NOW`,
/// but not the panic *class* it was an instance of. `time`'s
/// `Duration::days/weeks/hours/minutes` are `checked_mul(...).expect(...)`
/// internally, so an unbounded user-supplied count panics -- in release builds
/// too. On the index path that panic fires while the index-writer lock is held,
/// poisoning it: one such `/update` and every subsequent write to the core
/// fails for the process lifetime. It must be the finding-184 400 instead, and
/// the core must still be writable afterwards.
///
/// Not fixture-derived: Solr's own limit is a `NumberFormatException` on a
/// count this size, so the shared truth here is only "a 400, and the server
/// survives". The second `/update` is the actual regression assertion.
#[tokio::test]
async fn date_math_overflow_on_the_index_path_is_400_and_leaves_the_core_writable() {
    let dir = TempDir::new().expect("temp dir");
    let app =
        app_with_schema(dir.path(), DATE_RANGE_SCHEMA_TOML).expect("date_range app must build");
    for expr in [
        "NOW+9223372036854775807DAYS",
        "NOW-9223372036854775807DAYS",
        "NOW+9223372036854775807WEEKS",
        "NOW+9223372036854775807HOURS",
        "NOW+9223372036854775807MINUTES",
        "NOW+9223372036854775807MONTHS",
    ] {
        let (status, body) = post_docs(&app, &json!([{"id":"bad","drs_x":expr}])).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "`{expr}` must be a 400, not a panic: {body}"
        );
        let (status, body) = post_docs(&app, &json!([{"id":"ok","drs_x":"2022"}])).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the index writer must survive `{expr}`; a poisoned lock bricks the core: {body}"
        );
    }
}

/// The query-path half of the same defect: a 400 quoting the whole expression
/// (finding 184), never the 500 a caught panic produces.
#[tokio::test]
async fn date_math_overflow_on_the_query_path_is_400() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A%5BNOW%20TO%20NOW%2B9223372036854775807DAYS%5D&fl=id&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body["error"]["msg"].as_str(),
        Some("Invalid Date Math String:'NOW+9223372036854775807DAYS'"),
        "finding 184's message shape, quoting the whole expression: {body}"
    );
}

/// Round 2's clamp guards the low side of a truncated literal's end
/// (`.max(MIN_MS)`) but not the high side, so a literal whose *next* unit
/// exceeds `MAX_MS` gets `end = MAX_MS - 1` while `start` is already `MAX_MS` --
/// an inverted interval, which then reports the finding-184 `Wrong order` error
/// for a correctly ordered query. Year 9999 happened to escape it (its end
/// clamps to exactly `MAX_MS`); everything from 2263 to 9998 did not.
///
/// Expected set is the clamp's own consequence, derived from the 9-doc corpus:
/// both endpoints collapse to `MAX_MS`, so the query is the point interval at
/// the upper bound, which d6 (`[2021-01-01 TO *]`) and d7 (`[* TO *]`) contain
/// and no other doc reaches. Solr answers d6/d7 for `[3000 TO 3001]` too.
#[tokio::test]
async fn far_future_truncated_endpoints_do_not_invert_the_interval() {
    let (app, _dir) = date_range_app().await;
    for query in [
        "drs_x%3A%5B3000%20TO%203001%5D",
        "drs_x%3A%5B2300%20TO%202400%5D",
        "drs_x%3A%5B2262-05%20TO%202262-06%5D",
        "drs_x%3A%5B3000-06-15T12%3A00%3A00Z%20TO%209998%5D",
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

/// The index-path half: a document Solr accepts must not be rejected because
/// its endpoints clamped into an inverted interval.
#[tokio::test]
async fn far_future_truncated_endpoints_index_without_inverting() {
    let dir = TempDir::new().expect("temp dir");
    let app =
        app_with_schema(dir.path(), DATE_RANGE_SCHEMA_TOML).expect("date_range app must build");
    let (status, body) = post_docs(
        &app,
        &json!([
            {"id":"f1","drs_x":"[3000 TO 3001]"},
            {"id":"f2","drs_x":"2300"},
            {"id":"f3","drs_x":"2262-05"}
        ]),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a far-future literal must clamp, not fail the update: {body}"
    );
}

/// A field-LESS literal under `edismax`/`dismax` never reaches `build_leaf` --
/// it is routed to the `qf` disjunction, which builds a term query against the
/// raw text field carrying the verbatim string (finding 179). So
/// `qf=drs_x&q=2020` matched only the doc whose stored string is literally
/// `2020`, exactly the silently-wrong-term-query class round 2 was meant to
/// close. Solr routes a `qf` field through `FieldType::getFieldQuery`, i.e. the
/// interval query, so the answer is `dr341_single_year`'s set.
///
/// `defType=dismax` is deliberately NOT exercised: this repo implements only
/// `edismax` (`src/lib.rs:3213` is the sole `defType` branch), so a `dismax`
/// request falls through to the plain Lucene parser and answers `q=2020`
/// against `df` -- a pre-existing gap unrelated to #341, not a `date_range`
/// defect. Its 0 hits are that gap, not a wrong interval query.
#[tokio::test]
async fn field_less_literal_in_qf_is_an_interval_query() {
    let (app, _dir) = date_range_app().await;
    let expected = vec![
        "d1".to_string(),
        "d2".to_string(),
        "d3".to_string(),
        "d4".to_string(),
        "d7".to_string(),
    ];
    for query in [
        "defType=edismax&qf=drs_x&q=2020",
        "defType=edismax&qf=drs_x%20id&q=2020",
        "defType=edismax&qf=drs_x&q=2020&tie=0.3",
    ] {
        let (status, body) = get(
            &app,
            &format!("select?{query}&fl=id&sort=id%20asc&rows=20&wt=json"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "`{query}`: {body}");
        assert_eq!(
            ids(&body),
            expected,
            "`{query}` must be the interval query, not a term query on the raw text: {body}"
        );
    }
}

// --- round-4 review: `qf` alongside free text, and the reversed far-future
// --- interval the clamp collapses ------------------------------------------

/// A `qf` naming a `date_range` field **and** an ordinary field must still
/// answer the ordinary field: a literal that is not a date makes the
/// `date_range` disjunct unanswerable, not the whole request invalid.
///
/// This follows the ceiling issue #84 already ratified for typed `qf` fields
/// (`field_target`'s ponytail, `src/core_index.rs:2106-2113`): a `qf` field
/// whose type cannot encode the literal contributes a clause that cannot match
/// rather than failing the request, and "raising this ceiling means encoding
/// [the typed] terms here, not restoring the 400". `int` and `date` fields in
/// `qf` already behave exactly this way, so `date_range` matching them is the
/// consistent answer. Not fixture-derived: no capture sends a `qf` naming a
/// `DateRangeField`.
#[tokio::test]
async fn qf_naming_a_date_range_field_alongside_another_still_answers_the_other() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?defType=edismax&qf=id%20drs_x&q=d3&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a non-date literal must not 400 the whole request: {body}"
    );
    assert_eq!(
        ids(&body),
        vec!["d3".to_string()],
        "the `id` disjunct must still answer; only the date_range one drops: {body}"
    );
}

/// And the degenerate case the same ceiling covers: a `qf` naming *only* a
/// `date_range` field, with a literal that is not a date, is the quiet
/// `numFound: 0` issue #84 chose over a 400 -- the identical outcome a `qf`
/// naming only a numeric dynamic field produces today.
#[tokio::test]
async fn qf_naming_only_a_date_range_field_with_a_non_date_literal_is_zero_hits() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?defType=edismax&qf=drs_x&q=hello&fl=id&rows=20&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["response"]["numFound"].as_u64(),
        Some(0),
        "issue #84's ratified trade: a quiet 0 hits, not a 400: {body}"
    );
}

/// The documented consequence of clamping both ends: when BOTH endpoints land
/// past `MAX_MS` they collapse to the same instant, so an interval written in
/// reversed order stops being finding 184's `Wrong order` 500 and answers as
/// the point interval at the bound. Pinned here so it is a recorded choice
/// rather than a side effect -- reversal is still detected whenever at least
/// one endpoint is representable (`drs_x:[2022 TO 1677]`,
/// `drs_x:[2262-04-12 TO 2262-04-11]` both still 500).
#[tokio::test]
async fn reversed_interval_past_the_upper_bound_collapses_instead_of_erroring() {
    let (app, _dir) = date_range_app().await;
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A%5B9999%20TO%202263%5D&fl=id&sort=id%20asc&rows=20&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "both endpoints clamp to MAX_MS, so there is no order left to be wrong: {body}"
    );
    assert_eq!(
        ids(&body),
        vec!["d6".to_string(), "d7".to_string()],
        "the point interval at MAX_MS: {body}"
    );
    // The other direction is untouched: with a representable endpoint, a
    // reversed interval is still the finding-184 500.
    let (status, body) = get(
        &app,
        "select?q=drs_x%3A%5B2022%20TO%201677%5D&fl=id&rows=20&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a reversed interval inside the representable range still 500s: {body}"
    );
}
