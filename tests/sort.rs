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

use serde_json::Value;

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
