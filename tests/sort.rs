//! Issue #2 — the `sort` request parameter.
//!
//! Every expected value here is derived from a committed fixture in
//! `solr-ref/responses/` (CLAUDE.md: fixtures are ground truth). The fixtures
//! for this suite were captured by the issue-#2 block at the end of
//! `solr-ref/capture.sh` against the same `solr:9` container and the same 5-doc
//! corpus as everything else, so no expected ordering below is invented.
//!
//! Two captured facts are worth reading before the tests, because they
//! contradict a reasonable prior:
//!
//! 1. **Sorting on a multiValued docValues field is a 200 in Solr 9, not an
//!    error.** `select_sort_mv_asc.json` / `select_sort_mv_desc.json` show
//!    Lucene's `SortedSetSortField` selector semantics: `asc` orders by each
//!    doc's *minimum* value, `desc` by its *maximum*, and a doc with no value
//!    sorts **last in both directions**.
//! 2. **A sort clause with a missing or unrecognised direction token is a
//!    400.** `err_sort_no_direction.json` / `err_sort_bad_direction.json`:
//!    `sort=id` and `sort=id sideways` both fail with "Can't determine a Sort
//!    Order (asc or desc) in sort spec ...". Both are now rejected: this issue
//!    added the direction check that #11's field-only validation lacked.
//!
//! Sort *validation* of the field itself (undefined field, non-fast field)
//! already landed with #11 and is covered in `tests/error_shapes.rs`; it is not
//! duplicated here. What is new here is a bad clause sitting among valid ones,
//! and the direction-token cases above.

// The `dead_code` allow for the shared helpers is an inner attribute inside
// `tests/common/mod.rs`; do not add a second one here (clippy rejects it under
// `-D warnings`).
mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

use common::{assert_matches_fixture, fixture, get, indexed_app};

/// The ordered `id` list in a response or fixture envelope.
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

/// The ordered `id` list captured in `solr-ref/responses/<name>.json`.
fn fixture_ids(name: &str) -> Vec<String> {
    ids(&fixture(name))
}

/// Which *class* of sort error a message is. Solr's wording and Wayfinder's
/// differ for the field cases (Solr talks about `docValues`, Wayfinder about
/// `fast` values), so the verbatim text is not comparable — but the class is, and
/// the class is what says *which problem in the spec Solr reported first*.
///
/// Deliberately coarse: it keys off the one prefix Solr uses for a direction
/// failure and treats everything else as a field failure. That is enough to
/// discriminate the two orderings without freezing either side's wording.
fn sort_error_class(msg: &str) -> &'static str {
    if msg.starts_with("Can't determine a Sort Order") {
        "direction"
    } else {
        "field"
    }
}

/// Asserts a sort error matches the named fixture on the contract the task spec
/// names: HTTP status and `error.code`, mirrored by `responseHeader.status`.
/// `error.msg` is free text and only checked non-empty (same rule as
/// `tests/error_shapes.rs`).
fn assert_sort_error(status: axum::http::StatusCode, body: &Value, fixture_name: &str) {
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
        "a rejected sort must not also return a result set ({fixture_name})"
    );
}

// --- direction: asc / desc actually order the results -----------------------

#[tokio::test]
async fn sort_field_desc_orders_results() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=*:*&sort=id+desc&rows=3&wt=json").await;

    assert_eq!(status, 200);
    assert_matches_fixture(body.clone(), "select_sort");
    assert_eq!(
        ids(&body),
        fixture_ids("select_sort"),
        "sort=id desc must return the captured descending order"
    );
    // Spelled out so a regression reads as an ordering bug, not a fixture diff.
    assert_eq!(ids(&body), vec!["doc5", "doc4", "doc3"]);
}

#[tokio::test]
async fn sort_field_asc_orders_results() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=*:*&sort=id+asc&rows=3&wt=json").await;

    assert_eq!(status, 200);
    assert_matches_fixture(body.clone(), "select_sort_asc");
    assert_eq!(ids(&body), fixture_ids("select_sort_asc"));
    assert_eq!(ids(&body), vec!["doc1", "doc2", "doc3"]);
}

#[tokio::test]
async fn sort_asc_and_desc_are_exact_reverses_of_each_other() {
    let (app, _dir) = indexed_app().await;

    let (_, asc) = get(&app, "select?q=*:*&sort=id+asc&rows=10&wt=json").await;
    let (_, desc) = get(&app, "select?q=*:*&sort=id+desc&rows=10&wt=json").await;

    let mut reversed = ids(&asc);
    reversed.reverse();
    assert_eq!(
        ids(&desc),
        reversed,
        "with a single-valued unique sort key the two directions must be exact reverses"
    );
}

// --- score sorting ----------------------------------------------------------

#[tokio::test]
async fn sort_score_desc_matches_the_unsorted_default_order() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=*:*&sort=score+desc&rows=10&wt=json").await;

    assert_eq!(status, 200);
    assert_matches_fixture(body.clone(), "select_sort_score_all");
    // `score desc` is Solr's default sort, so the captured order is identical to
    // the no-`sort` capture (`select_all.json`).
    assert_eq!(
        ids(&body),
        fixture_ids("select_all"),
        "sort=score desc must equal the default (unsorted) order"
    );
}

#[tokio::test]
async fn sort_score_desc_on_a_relevance_query_orders_by_descending_score() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=lazy&df=body&sort=score+desc&rows=5&wt=json").await;

    assert_eq!(status, 200);
    assert_matches_fixture(body.clone(), "select_sort_score_desc");
    assert_eq!(ids(&body), fixture_ids("select_sort_score_desc"));
    // doc2's `body` is shorter, so it scores higher on `lazy` than doc1's.
    assert_eq!(ids(&body), vec!["doc2", "doc1"]);
}

#[tokio::test]
async fn sort_score_asc_on_a_relevance_query_orders_by_ascending_score() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=lazy&df=body&sort=score+asc&rows=5&wt=json").await;

    assert_eq!(status, 200);
    assert_matches_fixture(body.clone(), "select_sort_score_asc");
    assert_eq!(ids(&body), fixture_ids("select_sort_score_asc"));
    assert_eq!(ids(&body), vec!["doc1", "doc2"]);
}

#[tokio::test]
async fn sort_score_asc_is_the_reverse_of_sort_score_desc() {
    let (app, _dir) = indexed_app().await;

    let (_, asc) = get(&app, "select?q=lazy&df=body&sort=score+asc&rows=5&wt=json").await;
    let (_, desc) = get(&app, "select?q=lazy&df=body&sort=score+desc&rows=5&wt=json").await;

    let mut reversed = ids(&asc);
    reversed.reverse();
    assert_eq!(
        ids(&desc),
        reversed,
        "the two score directions must be exact reverses on a two-hit, no-tie query"
    );
}

// --- multiple comma-separated clauses ---------------------------------------

#[tokio::test]
async fn sort_multi_clause_second_clause_breaks_ties_ascending() {
    let (app, _dir) = indexed_app().await;

    // Under `q=*:*` every doc scores identically, so the first clause is a total
    // tie and the second clause alone decides the order — which is exactly what
    // proves a second clause is honoured rather than dropped.
    let (status, body) = get(&app, "select?q=*:*&sort=score+desc,id+asc&rows=5&wt=json").await;

    assert_eq!(status, 200);
    assert_matches_fixture(body.clone(), "select_sort_multi_asc");
    assert_eq!(ids(&body), fixture_ids("select_sort_multi_asc"));
    assert_eq!(ids(&body), vec!["doc1", "doc2", "doc3", "doc4", "doc5"]);
}

#[tokio::test]
async fn sort_multi_clause_second_clause_direction_is_honoured() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=*:*&sort=score+desc,id+desc&rows=5&wt=json").await;

    assert_eq!(status, 200);
    assert_matches_fixture(body.clone(), "select_sort_multi_desc");
    assert_eq!(ids(&body), fixture_ids("select_sort_multi_desc"));
    assert_eq!(ids(&body), vec!["doc5", "doc4", "doc3", "doc2", "doc1"]);
}

#[tokio::test]
async fn multi_clause_sort_differs_by_trailing_clause_direction_only() {
    let (app, _dir) = indexed_app().await;

    let (_, asc) = get(&app, "select?q=*:*&sort=score+desc,id+asc&rows=5&wt=json").await;
    let (_, desc) = get(&app, "select?q=*:*&sort=score+desc,id+desc&rows=5&wt=json").await;

    assert_ne!(
        ids(&asc),
        ids(&desc),
        "flipping only the trailing clause's direction must change the order — \
         if these are equal the trailing clause is being ignored"
    );
}

// --- sort + start/rows pagination -------------------------------------------

#[tokio::test]
async fn sort_holds_under_start_and_rows_pagination() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=*:*&sort=id+desc&rows=2&start=2&wt=json").await;

    assert_eq!(status, 200);
    assert_matches_fixture(body.clone(), "select_sort_paged");
    assert_eq!(ids(&body), fixture_ids("select_sort_paged"));
    assert_eq!(ids(&body), vec!["doc3", "doc2"]);
    assert_eq!(
        body["response"]["numFound"], 5,
        "numFound is the full match count, not the page size"
    );
    assert_eq!(body["response"]["start"], 2);
}

#[tokio::test]
async fn sorted_pagination_past_the_end_returns_an_empty_page() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=*:*&sort=id+desc&rows=2&start=99&wt=json").await;

    assert_eq!(status, 200);
    assert_matches_fixture(body.clone(), "select_sort_paged_past_end");
    assert_eq!(body["response"]["numFound"], 5);
    assert_eq!(body["response"]["start"], 99);
    assert!(ids(&body).is_empty());
}

#[tokio::test]
async fn sorted_pages_are_windows_onto_one_consistent_total_order() {
    let (app, _dir) = indexed_app().await;

    // Two committed fixtures overlap: `select_sort` is rows=3&start=0 and
    // `select_sort_paged` is rows=2&start=2. Walking one row at a time must
    // reproduce the same total order both fixtures are windows of.
    let (_, full) = get(&app, "select?q=*:*&sort=id+desc&rows=10&wt=json").await;
    let full_ids = ids(&full);
    assert_eq!(full_ids.len(), 5, "rows=10 must return every match");

    assert_eq!(
        &full_ids[0..3],
        fixture_ids("select_sort").as_slice(),
        "the rows=3&start=0 window must be the head of the total order"
    );
    assert_eq!(
        &full_ids[2..4],
        fixture_ids("select_sort_paged").as_slice(),
        "the rows=2&start=2 window must be the same slice of the total order"
    );

    let mut walked = Vec::new();
    for start in 0..5 {
        let (_, page) = get(
            &app,
            &format!("select?q=*:*&sort=id+desc&rows=1&start={start}&wt=json"),
        )
        .await;
        walked.extend(ids(&page));
    }
    assert_eq!(
        walked, full_ids,
        "single-row pages must tile the same sorted order as one whole-result page"
    );
}

// --- multiValued sort field: a 200, per the captured fixtures ---------------

#[tokio::test]
async fn sort_asc_on_a_multi_valued_field_uses_each_docs_minimum_value() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=*:*&sort=category+asc&wt=json").await;

    // Not an error: Solr 9 answers 200 here (`select_sort_mv_asc.json`).
    assert_eq!(
        status, 200,
        "sorting on a multiValued docValues field is a 200 in Solr, not a 400"
    );
    assert!(body.get("error").is_none());
    assert_matches_fixture(body.clone(), "select_sort_mv_asc");
    assert_eq!(ids(&body), fixture_ids("select_sort_mv_asc"));
    // min values: doc1=animals, doc4=animals, doc3=classic, doc2=garden,
    // doc5=<none>. Ties (doc1/doc4) break by ascending doc order.
    assert_eq!(ids(&body), vec!["doc1", "doc4", "doc3", "doc2", "doc5"]);
}

#[tokio::test]
async fn sort_desc_on_a_multi_valued_field_uses_each_docs_maximum_value() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=*:*&sort=category+desc&wt=json").await;

    assert_eq!(status, 200);
    assert_matches_fixture(body.clone(), "select_sort_mv_desc");
    assert_eq!(ids(&body), fixture_ids("select_sort_mv_desc"));
    // max values: doc3=misc, doc2=garden, doc1=classic, doc4=animals,
    // doc5=<none>.
    assert_eq!(ids(&body), vec!["doc3", "doc2", "doc1", "doc4", "doc5"]);
}

#[tokio::test]
async fn docs_missing_the_sort_field_come_last_in_both_directions() {
    let (app, _dir) = indexed_app().await;

    // doc5 has no `category`. Both captured fixtures put it last, so "missing
    // last" is not a consequence of the direction.
    let (_, asc) = get(&app, "select?q=*:*&sort=category+asc&wt=json").await;
    let (_, desc) = get(&app, "select?q=*:*&sort=category+desc&wt=json").await;

    assert_eq!(
        ids(&asc).last().map(String::as_str),
        Some("doc5"),
        "a doc with no value for the sort field sorts last under asc"
    );
    assert_eq!(
        ids(&desc).last().map(String::as_str),
        Some("doc5"),
        "a doc with no value for the sort field sorts last under desc too"
    );
}

// --- error paths new to this issue ------------------------------------------

#[tokio::test]
async fn a_bad_clause_among_valid_ones_rejects_the_whole_sort() {
    let (app, _dir) = indexed_app().await;

    // `id asc` is fine, `body desc` is not (text_en, no docValues). Solr fails
    // the request rather than sorting on the valid prefix.
    let (status, body) = get(&app, "select?q=*:*&sort=id+asc,body+desc&wt=json").await;

    assert_sort_error(status, &body, "err_sort_bad_clause_among_good");
    assert_eq!(
        body["responseHeader"]["params"]["sort"], "id asc,body desc",
        "the rejected sort spec must still be echoed verbatim"
    );
}

#[tokio::test]
async fn a_clause_with_no_direction_is_a_400() {
    let (app, _dir) = indexed_app().await;

    // `sort=id` — Solr requires an explicit asc/desc and does not default it.
    let (status, body) = get(&app, "select?q=*:*&sort=id&wt=json").await;

    assert_sort_error(status, &body, "err_sort_no_direction");
}

#[tokio::test]
async fn a_clause_with_an_unrecognised_direction_is_a_400() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=*:*&sort=id+sideways&wt=json").await;

    assert_sort_error(status, &body, "err_sort_bad_direction");
}

#[tokio::test]
async fn a_bad_direction_is_rejected_even_on_a_score_clause() {
    let (app, _dir) = indexed_app().await;

    // Now has a fixture of its own (`err_sort_score_bad_direction.json`,
    // captured during issue-#2 review): the direction check is what fails.
    //
    // Scope, precisely: this establishes that `score` is not exempt from the
    // direction check. It does NOT establish that `score` skips field
    // resolution — under direction-first, a bad direction errors either way
    // (review verified: stubbing the `score` branch to `false` leaves this test
    // green). The special-casing is covered by the `sort_score_*` ordering
    // tests, which need `score` to resolve to a ranking key at all.
    let (status, body) = get(&app, "select?q=*:*&sort=score+sideways&wt=json").await;

    assert_sort_error(status, &body, "err_sort_score_bad_direction");

    // The discriminating half: an implementation that resolved `score` as a
    // field would answer "undefined field: score" — also a 400, but the wrong
    // class. The fixture says Solr reports the direction error.
    let expected = fixture("err_sort_score_bad_direction");
    assert_eq!(
        sort_error_class(body["error"]["msg"].as_str().expect("msg must be a string")),
        sort_error_class(expected["error"]["msg"].as_str().expect("fixture msg")),
        "a `score` clause with a bad direction must report the direction error, \
         not an undefined-field error"
    );
}

#[tokio::test]
async fn a_sort_spec_with_two_problems_is_still_a_single_400() {
    let (app, _dir) = indexed_app().await;

    // `body desc` is a bad *field* (clause 1), `id sideways` is a bad
    // *direction* (clause 2). Solr answers the field error, which is how
    // `err_sort_field_before_direction.json` shows that clauses are processed
    // left to right and the first bad one wins.
    //
    // Scope note: this says nothing about the order of checks *within* a clause
    // — clause ordering alone explains it, because clause 1's direction is fine.
    // The within-clause order is pinned separately, below.
    let (status, body) = get(&app, "select?q=*:*&sort=body+desc,id+sideways&wt=json").await;

    assert_sort_error(status, &body, "err_sort_field_before_direction");

    // The 400 alone does not discriminate the two designs: a
    // validate-all-directions-first implementation also answers 400 here, just
    // with the *other* error. So compare the error class against the fixture's.
    // The fixture decides which is right; neither side's wording is hardcoded,
    // so this pins the ordering without freezing Wayfinder's field-error text.
    let expected = fixture("err_sort_field_before_direction");
    assert_eq!(
        sort_error_class(body["error"]["msg"].as_str().expect("msg must be a string")),
        sort_error_class(expected["error"]["msg"].as_str().expect("fixture msg")),
        "an earlier clause's field error must win over a later clause's direction \
         error, as `err_sort_field_before_direction.json` shows Solr doing"
    );
}

#[tokio::test]
async fn within_one_clause_the_direction_is_checked_before_the_field() {
    let (app, _dir) = indexed_app().await;

    // `body sideways` is a *single* clause that is bad in both ways at once:
    // `body` is not fast, and `sideways` is not a direction. That makes it the
    // only captured spec that separates the two within-clause orders — every
    // other one answers identically under either. Solr reports the DIRECTION
    // error (`err_sort_direction_before_field.json`), so the direction is
    // checked first.
    let (status, body) = get(&app, "select?q=*:*&sort=body+sideways&wt=json").await;

    assert_sort_error(status, &body, "err_sort_direction_before_field");

    let expected = fixture("err_sort_direction_before_field");
    assert_eq!(
        sort_error_class(body["error"]["msg"].as_str().expect("msg must be a string")),
        sort_error_class(expected["error"]["msg"].as_str().expect("fixture msg")),
        "a clause bad in both ways must report the direction error, not the field \
         error — swapping the two checks inside a clause is otherwise invisible"
    );
}

#[tokio::test]
async fn direction_error_messages_match_solr_verbatim_including_pos() {
    let (app, _dir) = indexed_app().await;

    // Unlike the *field* errors — where Wayfinder deliberately keeps its own
    // wording (`fast values` rather than Solr's `docValues ... Uninversion`) —
    // the direction message is one Wayfinder reproduces byte for byte. `pos` is
    // computed arithmetic, so it can be silently wrong while every
    // status/code/class assertion still passes; this is the one place it is
    // frozen. Four fixtures, four different field-name lengths, so a constant
    // offset error cannot hide.
    for (fixture_name, query) in [
        ("err_sort_no_direction", "select?q=*:*&sort=id&wt=json"),
        (
            "err_sort_bad_direction",
            "select?q=*:*&sort=id+sideways&wt=json",
        ),
        (
            "err_sort_score_bad_direction",
            "select?q=*:*&sort=score+sideways&wt=json",
        ),
        (
            "err_sort_direction_before_field",
            "select?q=*:*&sort=body+sideways&wt=json",
        ),
    ] {
        let (_, body) = get(&app, query).await;
        assert_eq!(
            body["error"]["msg"].as_str(),
            fixture(fixture_name)["error"]["msg"].as_str(),
            "the direction error must match `{fixture_name}` verbatim, `pos` included"
        );
    }
}

// --- regression guard on the unsorted path ----------------------------------

#[tokio::test]
async fn omitting_sort_keeps_the_deterministic_score_then_doc_order_tie_break() {
    let (app, _dir) = indexed_app().await;

    // The property the 12 tracer-bullet tests pin: with no `sort`, equal scores
    // break by ascending doc (insertion) order, matching `select_all.json`.
    let (status, body) = get(&app, "select?q=*:*&rows=10&wt=json").await;

    assert_eq!(status, 200);
    assert_eq!(
        ids(&body),
        fixture_ids("select_all"),
        "adding sort support must not change the unsorted order"
    );
    assert_eq!(ids(&body), vec!["doc1", "doc2", "doc3", "doc4", "doc5"]);
}

#[tokio::test]
async fn an_empty_sort_param_behaves_as_no_sort() {
    let (app, _dir) = indexed_app().await;

    // No fixture: `check_sort` already treats an empty clause as a no-op, and
    // this pins that adding ordering does not turn it into a 400 or a reorder.
    let (status, body) = get(&app, "select?q=*:*&sort=&rows=10&wt=json").await;

    assert_eq!(status, 200);
    assert_eq!(ids(&body), fixture_ids("select_all"));
}

// =============================================================================
// Issue #32 — sort follow-up debt: clause grammar, numeric/float/date/mv-numeric
// ordering, multi-segment ordering.
//
// Every expected value below is read from a fixture in `solr-ref/responses/`
// captured against a real `solr:9`'s `sortdebt` core (`docs/solr-ref-findings.md`
// findings 34-37), never invented. The 6-doc corpus (`s1..s6`) is reproduced in
// `sortdebt_doc` below, exactly per the task spec's table.
//
// This section builds its own mirror app rather than reusing `indexed_app`'s
// 5-doc `content`-core corpus, which has no numeric/float/date/multiValued-
// numeric fields to sort on. `common::app_with_schema` is reusable as-is (it
// takes an arbitrary schema TOML), but `common::get`/`common::post_docs` are
// hardcoded to `/solr/{common::CORE}/...` where `CORE == "content"` — and the
// captured fixtures' request paths, and the task spec, want a schema literally
// named `sortdebt` at `/solr/sortdebt/...` (unlike `tests/faceting.rs::range_app`,
// which sidesteps the same hardcoding by naming its mirror core `content`
// instead). So `sortdebt_get`/`sortdebt_post_docs` below are line-for-line
// mirrors of `common::get`/`common::post_docs`, retargeted at the `sortdebt`
// core, rather than edits to `tests/common/mod.rs` (out of scope for this
// suite) or a silent rename back to `content` (which would stop matching the
// spec's explicit core name).
// =============================================================================

/// Mirrors the real `sortdebt` core's schema (issue-#32 block of
/// `solr-ref/capture.sh`): `id` (string, unique key), `category` (string),
/// `views` (int), `weight` (float), `created` (date), `nums` (int,
/// multi_valued). All fast + stored, per the task spec.
const SORTDEBT_SCHEMA_TOML: &str = r#"
[core]
name = "sortdebt"
unique_key = "id"
default_field = "id"

[[fields]]
name = "id"
type = "string"
stored = true
required = true
fast = true

[[fields]]
name = "category"
type = "string"
stored = true
fast = true

[[fields]]
name = "views"
type = "int"
stored = true
fast = true

[[fields]]
name = "weight"
type = "float"
stored = true
fast = true

[[fields]]
name = "created"
type = "date"
stored = true
fast = true

[[fields]]
name = "nums"
type = "int"
stored = true
fast = true
multi_valued = true
"#;

/// One doc of the `s1..s6` corpus (task spec table), by id. `s4` has no
/// `views`, `s5` has no `weight`/`created`/`nums`, matching the per-field gaps
/// finding 36 needs to discriminate missing-as-zero from missing-last.
fn sortdebt_doc(id: &str) -> Value {
    match id {
        "s1" => json!({
            "id": "s1", "category": "alpha", "views": 30, "weight": 1.5,
            "created": "2021-03-01T00:00:00Z", "nums": [10, 90]
        }),
        "s2" => json!({
            "id": "s2", "category": "beta", "views": 10, "weight": 3.5,
            "created": "2021-01-01T00:00:00Z", "nums": [50, 60]
        }),
        "s3" => json!({
            "id": "s3", "category": "gamma", "views": 20, "weight": 2.5,
            "created": "2021-05-01T00:00:00Z", "nums": [20, 80]
        }),
        "s4" => json!({
            "id": "s4", "category": "delta", "weight": 0.5,
            "created": "2021-02-01T00:00:00Z", "nums": [70]
        }),
        "s5" => json!({"id": "s5", "category": "epsilon", "views": 40}),
        "s6" => json!({
            "id": "s6", "category": "zeta", "views": -5, "weight": -1.5,
            "created": "1969-06-01T00:00:00Z", "nums": [-10, 5]
        }),
        other => panic!("no such sortdebt corpus doc: {other}"),
    }
}

/// `POST /solr/sortdebt/update?commit=true` with `docs` as the body. See the
/// section comment above for why this cannot be `common::post_docs`.
async fn sortdebt_post_docs(app: &Router, docs: &Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri("/solr/sortdebt/update?commit=true")
        .header("content-type", "application/json")
        .body(Body::from(docs.to_string()))
        .unwrap();
    let resp = app
        .clone()
        .oneshot(req)
        .await
        .expect("update request must not fail at the transport level");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("response body must be readable")
        .to_bytes();
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response body must be valid JSON")
    };
    (status, body)
}

/// `GET /solr/sortdebt/<path_and_query>`. See the section comment above for
/// why this cannot be `common::get`.
async fn sortdebt_get(app: &Router, path_and_query: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("GET")
        .uri(format!("/solr/sortdebt/{path_and_query}"))
        .body(Body::empty())
        .unwrap();
    let resp = app
        .clone()
        .oneshot(req)
        .await
        .expect("select request must not fail at the transport level");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("response body must be readable")
        .to_bytes();
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response body must be valid JSON")
    };
    (status, body)
}

/// Builds a fresh `sortdebt`-schema app and indexes the given `batches` of ids
/// (each drawn from `sortdebt_doc`), one `sortdebt_post_docs` call — hence one
/// commit, hence one Tantivy segment — per batch. Every grammar/ordering test
/// below that does not care about segmentation uses a single batch of all six
/// ids; the multi-segment tests (issue item 4) split the corpus across two.
async fn sortdebt_app(batches: &[&[&str]]) -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), SORTDEBT_SCHEMA_TOML).expect("app must build");
    for batch in batches {
        let docs: Value = Value::Array(batch.iter().map(|id| sortdebt_doc(id)).collect());
        let (status, body) = sortdebt_post_docs(&app, &docs).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "indexing a sortdebt batch must succeed, got {body}"
        );
    }
    (app, dir)
}

/// The whole 6-doc corpus in one commit — one Tantivy segment.
async fn sortdebt_app_single() -> (Router, TempDir) {
    sortdebt_app(&[&["s1", "s2", "s3", "s4", "s5", "s6"]]).await
}

// --- A. clause grammar (finding 34) -----------------------------------------

#[tokio::test]
async fn extra_whitespace_token_after_a_direction_is_a_400_not_silently_dropped() {
    let (app, _dir) = sortdebt_app_single().await;

    // Currently Wayfinder answers 200 and silently drops `garbage` — this test
    // must be red until the clause grammar is rewritten to require a comma.
    let (status, body) = sortdebt_get(&app, "select?q=*:*&sort=id+asc+garbage&wt=json").await;

    assert_sort_error(status, &body, "sort_clause_trailing_garbage");
    assert_eq!(
        body["error"]["msg"].as_str(),
        fixture("sort_clause_trailing_garbage")["error"]["msg"].as_str(),
        "the direction error is verbatim, pos included"
    );
}

#[tokio::test]
async fn space_separated_second_clause_without_a_comma_is_a_400() {
    let (app, _dir) = sortdebt_app_single().await;

    // Currently Wayfinder parses one clause (`id asc`) and drops the rest —
    // 200 today, must be red.
    let (status, body) = sortdebt_get(&app, "select?q=*:*&sort=id+asc+category+desc&wt=json").await;

    assert_sort_error(status, &body, "sort_clause_space_separated");
    assert_eq!(
        body["error"]["msg"].as_str(),
        fixture("sort_clause_space_separated")["error"]["msg"].as_str(),
        "the direction error is verbatim, pos included"
    );
}

#[tokio::test]
async fn trailing_valid_field_token_without_a_comma_is_a_400() {
    let (app, _dir) = sortdebt_app_single().await;

    // `category` alone is a perfectly valid field for a *next* clause — but
    // there is no comma introducing one, so it is garbage after `id asc`'s
    // direction token, same as the nonsense-token case above. Currently 200
    // (one clause, `category` dropped) — must be red.
    let (status, body) = sortdebt_get(&app, "select?q=*:*&sort=id+asc+category&wt=json").await;

    assert_sort_error(status, &body, "sort_clause_trailing_valid_field");
    assert_eq!(
        body["error"]["msg"].as_str(),
        fixture("sort_clause_trailing_valid_field")["error"]["msg"].as_str(),
        "the direction error is verbatim, pos included"
    );
}

#[tokio::test]
async fn a_trailing_comma_after_the_last_clause_is_fine() {
    let (app, _dir) = sortdebt_app_single().await;

    // Comma handling is asymmetric (finding 34): trailing is fine, leading is
    // not. This is a green pin, not a red case.
    let (status, body) = sortdebt_get(&app, "select?q=*:*&sort=id+asc,&wt=json").await;

    assert_eq!(status, 200);
    assert_eq!(ids(&body), fixture_ids("sort_clause_trailing_comma"));
}

#[tokio::test]
async fn an_empty_sort_value_on_sortdebt_is_the_default_order() {
    let (app, _dir) = sortdebt_app_single().await;

    let (status, body) = sortdebt_get(&app, "select?q=*:*&sort=&wt=json").await;

    assert_eq!(status, 200);
    assert_eq!(ids(&body), fixture_ids("sort_clause_empty"));
}

#[tokio::test]
async fn whitespace_before_the_clause_comma_is_tolerated() {
    let (app, _dir) = sortdebt_app_single().await;

    let (status, body) =
        sortdebt_get(&app, "select?q=*:*&sort=id+asc+,+category+desc&wt=json").await;

    assert_eq!(status, 200);
    assert_eq!(ids(&body), fixture_ids("sort_clause_space_before_comma"));
}

#[tokio::test]
async fn whitespace_after_the_clause_comma_is_tolerated() {
    let (app, _dir) = sortdebt_app_single().await;

    let (status, body) =
        sortdebt_get(&app, "select?q=*:*&sort=id+asc,+category+desc&wt=json").await;

    assert_eq!(status, 200);
    assert_eq!(ids(&body), fixture_ids("sort_clause_space_after_comma"));
}

#[tokio::test]
async fn a_leading_comma_glues_onto_the_next_token_and_is_a_field_error() {
    let (app, _dir) = sortdebt_app_single().await;

    // `sort=,id asc` — Solr fails *field resolution* on the glued token `,id`,
    // not the direction check; classified a field error, not frozen verbatim
    // (finding 34, error-class rule from issue #2/finding 20). Currently
    // Wayfinder skips the empty leading clause and answers 200 — must be red.
    let (status, body) = sortdebt_get(&app, "select?q=*:*&sort=,id+asc&wt=json").await;

    assert_sort_error(status, &body, "sort_clause_leading_comma");
    let expected = fixture("sort_clause_leading_comma");
    assert_eq!(
        sort_error_class(body["error"]["msg"].as_str().expect("msg must be a string")),
        "field",
        "a leading comma must be a field-class error"
    );
    assert_eq!(
        sort_error_class(body["error"]["msg"].as_str().expect("msg must be a string")),
        sort_error_class(expected["error"]["msg"].as_str().expect("fixture msg")),
        "the response's error class must match the fixture's"
    );
}

#[tokio::test]
async fn a_double_comma_glues_onto_the_next_token_and_is_a_field_error() {
    let (app, _dir) = sortdebt_app_single().await;

    // `sort=id asc,,category desc` — the empty clause between the two commas
    // glues the second comma onto `category`, producing the field error on
    // `,category`. Currently Wayfinder skips the empty clause and parses
    // `category desc` as a valid second clause — 200 today, must be red.
    let (status, body) =
        sortdebt_get(&app, "select?q=*:*&sort=id+asc,,category+desc&wt=json").await;

    assert_sort_error(status, &body, "sort_clause_double_comma");
    let expected = fixture("sort_clause_double_comma");
    assert_eq!(
        sort_error_class(body["error"]["msg"].as_str().expect("msg must be a string")),
        "field",
        "a doubled comma must be a field-class error"
    );
    assert_eq!(
        sort_error_class(body["error"]["msg"].as_str().expect("msg must be a string")),
        sort_error_class(expected["error"]["msg"].as_str().expect("fixture msg")),
        "the response's error class must match the fixture's"
    );
}

// --- B. multi-clause / whitespace `pos` (finding 35) -------------------------
//
// These are pins, not red cases: the arithmetic in `check_sort` was already
// right (finding 20's note). They must survive the grammar rewrite that fixes
// section A.

#[tokio::test]
async fn direction_error_pos_in_a_second_clause_is_absolute_in_the_whole_spec() {
    let (app, _dir) = sortdebt_app_single().await;

    let (status, body) = sortdebt_get(&app, "select?q=*:*&sort=id+asc,id+sideways&wt=json").await;

    assert_sort_error(status, &body, "err_sort_second_clause_bad_direction");
    assert_eq!(
        body["error"]["msg"].as_str(),
        fixture("err_sort_second_clause_bad_direction")["error"]["msg"].as_str(),
        "pos=9 is past `id` in the second clause, not past `id` in the first"
    );
}

#[tokio::test]
async fn direction_error_pos_for_a_missing_second_clause_direction_is_past_the_whole_field_token() {
    let (app, _dir) = sortdebt_app_single().await;

    let (status, body) = sortdebt_get(&app, "select?q=*:*&sort=id+asc,category&wt=json").await;

    assert_sort_error(status, &body, "err_sort_second_clause_no_direction");
    assert_eq!(
        body["error"]["msg"].as_str(),
        fixture("err_sort_second_clause_no_direction")["error"]["msg"].as_str(),
        "pos=15 is past the whole `category` token, not just past `id asc,`"
    );
}

#[tokio::test]
async fn leading_whitespace_in_the_spec_counts_toward_pos() {
    let (app, _dir) = sortdebt_app_single().await;

    let (status, body) = sortdebt_get(&app, "select?q=*:*&sort=%20%20id+sideways&wt=json").await;

    assert_sort_error(status, &body, "err_sort_leading_whitespace");
    assert_eq!(
        body["error"]["msg"].as_str(),
        fixture("err_sort_leading_whitespace")["error"]["msg"].as_str(),
        "pos=4 and the echoed spec keeps its leading spaces"
    );
}

// --- C. numeric / float / date / mv-numeric / string ordering (findings 36-37) --

#[tokio::test]
async fn sort_int_asc() {
    let (app, _dir) = sortdebt_app_single().await;

    // A missing int value sorts as 0 (finding 36) — s6(-5) < s4(missing->0) <
    // s2(10). Wayfinder currently sorts missing last for every type, so this
    // must be red.
    let (status, body) =
        sortdebt_get(&app, "select?q=*:*&sort=views+asc&fl=id,views&wt=json").await;

    assert_eq!(status, 200);
    assert_eq!(ids(&body), fixture_ids("sort_int_asc"));
}

#[tokio::test]
async fn sort_int_desc() {
    let (app, _dir) = sortdebt_app_single().await;

    let (status, body) =
        sortdebt_get(&app, "select?q=*:*&sort=views+desc&fl=id,views&wt=json").await;

    assert_eq!(status, 200);
    assert_eq!(ids(&body), fixture_ids("sort_int_desc"));
}

#[tokio::test]
async fn sort_float_asc() {
    let (app, _dir) = sortdebt_app_single().await;

    // Missing float sorts as 0.0, between s6(-1.5) and s4(0.5) — red today.
    let (status, body) =
        sortdebt_get(&app, "select?q=*:*&sort=weight+asc&fl=id,weight&wt=json").await;

    assert_eq!(status, 200);
    assert_eq!(ids(&body), fixture_ids("sort_float_asc"));
}

#[tokio::test]
async fn sort_float_desc() {
    let (app, _dir) = sortdebt_app_single().await;

    let (status, body) =
        sortdebt_get(&app, "select?q=*:*&sort=weight+desc&fl=id,weight&wt=json").await;

    assert_eq!(status, 200);
    assert_eq!(ids(&body), fixture_ids("sort_float_desc"));
}

#[tokio::test]
async fn sort_date_asc() {
    let (app, _dir) = sortdebt_app_single().await;

    // Missing date sorts as the epoch, between the pre-epoch s6 (1969) and
    // s2 (2021-01-01) — red today.
    let (status, body) =
        sortdebt_get(&app, "select?q=*:*&sort=created+asc&fl=id,created&wt=json").await;

    assert_eq!(status, 200);
    assert_eq!(ids(&body), fixture_ids("sort_date_asc"));
}

#[tokio::test]
async fn sort_date_desc() {
    let (app, _dir) = sortdebt_app_single().await;

    let (status, body) =
        sortdebt_get(&app, "select?q=*:*&sort=created+desc&fl=id,created&wt=json").await;

    assert_eq!(status, 200);
    assert_eq!(ids(&body), fixture_ids("sort_date_desc"));
}

#[tokio::test]
async fn sort_mv_int_asc() {
    let (app, _dir) = sortdebt_app_single().await;

    // Min selector (finding 37) composed with missing-as-zero: s6's min is
    // -10, s5 has no `nums` at all and reads as 0, landing between s6 and
    // s1's min of 10 — red today.
    let (status, body) = sortdebt_get(&app, "select?q=*:*&sort=nums+asc&fl=id,nums&wt=json").await;

    assert_eq!(status, 200);
    assert_eq!(ids(&body), fixture_ids("sort_mv_int_asc"));
}

#[tokio::test]
async fn sort_mv_int_desc() {
    let (app, _dir) = sortdebt_app_single().await;

    // Max selector: s1's max is 90 down to s5's missing-as-zero last. Note
    // this is NOT the reverse of `sort_mv_int_asc` — the corpus was arranged
    // so min-order and max-order disagree (finding 37).
    let (status, body) = sortdebt_get(&app, "select?q=*:*&sort=nums+desc&fl=id,nums&wt=json").await;

    assert_eq!(status, 200);
    assert_eq!(ids(&body), fixture_ids("sort_mv_int_desc"));

    let (_, asc) = sortdebt_get(&app, "select?q=*:*&sort=nums+asc&fl=id,nums&wt=json").await;
    let mut reversed = ids(&asc);
    reversed.reverse();
    assert_ne!(
        ids(&body),
        reversed,
        "min-selector asc and max-selector desc must NOT be exact reverses of \
         each other — that asymmetry is the selector evidence (finding 37)"
    );
}

#[tokio::test]
async fn sort_string_asc() {
    let (app, _dir) = sortdebt_app_single().await;

    // Green pin: single-valued string, all six docs present, so this is
    // unaffected by the missing-as-zero fix.
    let (status, body) = sortdebt_get(
        &app,
        "select?q=*:*&sort=category+asc&fl=id,category&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(ids(&body), fixture_ids("sort_string_asc"));
}

#[tokio::test]
async fn sort_string_desc() {
    let (app, _dir) = sortdebt_app_single().await;

    let (status, body) = sortdebt_get(
        &app,
        "select?q=*:*&sort=category+desc&fl=id,category&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(ids(&body), fixture_ids("sort_string_desc"));

    let mut reversed = ids(&body);
    reversed.reverse();
    assert_eq!(
        reversed,
        fixture_ids("sort_string_asc"),
        "with a single-valued, all-present string key the two directions are \
         exact reverses"
    );
}

// --- D. multi-segment ordering (issue item 4) --------------------------------

#[tokio::test]
async fn string_sort_across_two_segments_matches_the_single_segment_order() {
    // Batch 1 = [s1,s3,s5], batch 2 = [s2,s4,s6], each its own commit, so two
    // Tantivy segments. The correct total order (`sort_string_asc`,
    // s1,s2,s4,s5,s3,s6) interleaves the two batches: b1,b2,b2,b1,b1,b2.
    //
    // A future "defer ordinal resolution" optimisation that compared raw
    // per-segment term ordinals directly (rather than resolving each to its
    // actual string value first) would instead produce s1,s2,s5,s4,s3,s6:
    // segment 1's ordinals are alpha=0, epsilon=1, gamma=2 (s1,s5,s3) and
    // segment 2's are beta=0, delta=1, zeta=2 (s2,s4,s6), and merging by raw
    // ordinal gives ord-0 pair (s1,s2), then ord-1 pair (s5,s4), then ord-2
    // pair (s3,s6) — which differs from the correct order at position 3
    // (s5 vs s4). So this test does not pass by luck: an implementation with
    // that bug fails it, not just an implementation with no cross-segment
    // merge at all.
    let (app, _dir) = sortdebt_app(&[&["s1", "s3", "s5"], &["s2", "s4", "s6"]]).await;

    let (status, body) = sortdebt_get(
        &app,
        "select?q=*:*&sort=category+asc&fl=id,category&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(ids(&body), fixture_ids("sort_string_asc"));
}

#[tokio::test]
async fn a_numeric_column_absent_from_one_segment_still_reads_as_zero() {
    // Batch 1 = [s1,s2,s3,s4,s6] (every doc that has `nums`), batch 2 = [s5]
    // (the one doc that never had `nums` at all). Segment 2 therefore has no
    // `nums` column whatsoever (`SegmentSortColumn::Absent` in
    // `src/collector.rs`, not a column with an empty value for its one doc).
    // This pins that an absent column still reads as 0 under `sort=nums asc`,
    // not as "missing, sorts last" — the same fixture (`sort_mv_int_asc`) as
    // the single-segment case, so any cross-segment special-casing of an
    // absent column would show up as a divergence from it.
    let (app, _dir) = sortdebt_app(&[&["s1", "s2", "s3", "s4", "s6"], &["s5"]]).await;

    let (status, body) = sortdebt_get(&app, "select?q=*:*&sort=nums+asc&fl=id,nums&wt=json").await;

    assert_eq!(status, 200);
    assert_eq!(ids(&body), fixture_ids("sort_mv_int_asc"));
}
