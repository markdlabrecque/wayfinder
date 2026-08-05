//! `{!tag=...}` on `fq` and `{!ex=...}` on `facet.field`/`facet.query` —
//! multi-select faceting (issue #295). Findings 136-140 in
//! `docs/solr-ref-findings.md` are the spec.
//!
//! Every behaviour here also has a the captured fixture request set row, so the
//! fixture comparison suite replays it verbatim against the committed fixture and
//! diffs the whole envelope. These tests spell the *counts* out explicitly so
//! the acceptance criterion — which filter was excluded, which stayed — is
//! visible without diffing an opaque envelope, and so a regression shows up in
//! this suite next to the change that caused it.
//!
//! The corpus's `category` distribution (5 docs, doc5 has none):
//! unfiltered `animals 2, classic 2, garden 1, misc 1`; filtered by
//! `fq=category:animals` to `animals 2, classic 1, garden 0, misc 0`.

// The `dead_code` allow for partially-used shared helpers is an inner attribute
// inside `tests/common/mod.rs`; repeating it here is a clippy error under
// `-D warnings`.
mod common;

use axum::http::StatusCode;
use serde_json::Value;

use common::{assert_matches_fixture, get, indexed_app};

/// `facet_counts.facet_fields.<label>` as the flat alternating Solr array, or
/// `None` when the label is absent.
fn field_bucket(body: &Value, label: &str) -> Option<Vec<Value>> {
    body.pointer(&format!("/facet_counts/facet_fields/{label}"))
        .and_then(|v| v.as_array().cloned())
}

/// The flat counts array under `label`, as ordered `(term, count)` pairs.
fn counts(body: &Value, label: &str) -> Vec<(String, u64)> {
    field_bucket(body, label)
        .unwrap_or_else(|| panic!("facet_counts.facet_fields.{label} missing: {body}"))
        .chunks(2)
        .map(|pair| {
            (
                pair[0]
                    .as_str()
                    .unwrap_or_else(|| panic!("term must be a string: {pair:?}"))
                    .to_string(),
                pair[1]
                    .as_u64()
                    .unwrap_or_else(|| panic!("count must be u64: {pair:?}")),
            )
        })
        .collect()
}

/// One `facet.query` count by its raw key (which keeps the `{!ex=...}` prefix
/// verbatim — finding 139).
fn query_count(body: &Value, key: &str) -> Option<u64> {
    body.pointer("/facet_counts/facet_queries")
        .and_then(Value::as_object)
        .and_then(|o| o.get(key).and_then(Value::as_u64))
}

const UNFILTERED: &[(&str, u64)] = &[("animals", 2), ("classic", 2), ("garden", 1), ("misc", 1)];
const FILTERED_ANIMALS: &[(&str, u64)] =
    &[("animals", 2), ("classic", 1), ("garden", 0), ("misc", 0)];

fn want(pairs: &[(&str, u64)]) -> Vec<(String, u64)> {
    pairs.iter().map(|(t, c)| (t.to_string(), *c)).collect()
}

// --- control: the un-prefixed path is unchanged ---------------------------

/// A pin, green before and after: plain `fq` + plain `facet.field` counts the
/// filtered set (`facet_extag_baseline`). Establishes the floor the excluded
/// counts are measured against.
#[tokio::test]
async fn baseline_plain_fq_and_facet_count_the_filtered_set() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&fq=category:animals&facet=true&facet.field=category&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        counts(&body, "category"),
        want(FILTERED_ANIMALS),
        "got {body}"
    );
    assert_matches_fixture(body, "facet_extag_baseline");
}

// --- the core: {!ex} excludes the {!tag}-carrying fq ----------------------

/// `facet_extag_excluded`: `fq={!tag=cat}category:animals` still filters the hit
/// list (`numFound` 2), but `facet.field={!ex=cat}category` counts as if that
/// `fq` were absent — the unfiltered distribution. Finding 136.
#[tokio::test]
async fn excluded_facet_counts_against_the_filter_set_minus_the_tagged_fq() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals\
         &facet=true&facet.field=%7B%21ex%3Dcat%7Dcategory&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        body.pointer("/response/numFound").and_then(Value::as_u64),
        Some(2),
        "the tagged fq must still filter the hit list; got {body}"
    );
    assert_eq!(counts(&body, "category"), want(UNFILTERED), "got {body}");
    assert_matches_fixture(body, "facet_extag_excluded");
}

// --- every near-miss is a silent no-op, never an error (finding 136) ------

/// `facet_extag_tag_no_ex`: a `{!tag}` with no matching `{!ex}` is inert — 200
/// with the filtered counts, not a 400.
#[tokio::test]
async fn a_tag_with_no_ex_is_a_silent_noop_returning_filtered_counts() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals\
         &facet=true&facet.field=category&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        counts(&body, "category"),
        want(FILTERED_ANIMALS),
        "got {body}"
    );
}

/// `facet_extag_ex_unknown_tag`: `{!ex=nosuch}` names a tag nobody set, so no
/// exclusion applies — filtered counts, 200.
#[tokio::test]
async fn an_ex_naming_an_unset_tag_applies_no_exclusion() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals\
         &facet=true&facet.field=%7B%21ex%3Dnosuch%7Dcategory&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        counts(&body, "category"),
        want(FILTERED_ANIMALS),
        "got {body}"
    );
}

/// `facet_extag_ex_empty`: `{!ex=}` is an empty exclusion list — no exclusion,
/// filtered counts.
#[tokio::test]
async fn an_empty_ex_degrades_to_no_exclusion() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals\
         &facet=true&facet.field=%7B%21ex%3D%7Dcategory&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        counts(&body, "category"),
        want(FILTERED_ANIMALS),
        "got {body}"
    );
}

/// `facet_extag_tag_empty`: `{!tag=}` carries no tag, so `{!ex=cat}` matches
/// nothing — the filter stays applied, filtered counts.
#[tokio::test]
async fn an_empty_tag_makes_the_fq_unexcludable() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&fq=%7B%21tag%3D%7Dcategory:animals\
         &facet=true&facet.field=%7B%21ex%3Dcat%7Dcategory&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        counts(&body, "category"),
        want(FILTERED_ANIMALS),
        "got {body}"
    );
}

/// `facet_extag_tag_on_q`: `{!tag=cat}` on `q` is accepted and inert (there is
/// no `fq` to exclude), so `q` still matches all 5 docs and the facet counts the
/// unfiltered distribution. Finding 136.
#[tokio::test]
async fn a_tag_on_q_is_accepted_and_inert() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21tag%3Dcat%7D*:*&rows=0&facet=true\
         &facet.field=%7B%21ex%3Dcat%7Dcategory&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        body.pointer("/response/numFound").and_then(Value::as_u64),
        Some(5),
        "the tag on q is inert, so q=*:* still matches everything; got {body}"
    );
    assert_eq!(counts(&body, "category"), want(UNFILTERED), "got {body}");
}

// --- per-fq exclusion and comma-list tags (finding 137) -------------------

/// `facet_extag_two_fq_one_tagged`: an untagged sibling `fq=category:classic`
/// is never excludable — excluding `cat` drops only the tagged `fq`, leaving
/// `classic` in force (`classic 2, animals 1, misc 1, garden 0`).
#[tokio::test]
async fn an_untagged_sibling_fq_stays_applied_when_the_tagged_one_is_excluded() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals&fq=category:classic\
         &facet=true&facet.field=%7B%21ex%3Dcat%7Dcategory&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        counts(&body, "category"),
        want(&[("classic", 2), ("animals", 1), ("misc", 1), ("garden", 0)]),
        "got {body}"
    );
}

/// `facet_extag_ex_one_of_two`: `{!ex=a}` between two tagged filters drops only
/// `a`, leaving `b` (`classic`) applied.
#[tokio::test]
async fn ex_drops_only_the_named_tag_among_two_tagged_fqs() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0\
         &fq=%7B%21tag%3Da%7Dcategory:animals&fq=%7B%21tag%3Db%7Dcategory:classic\
         &facet=true&facet.field=%7B%21ex%3Da%7Dcategory&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        counts(&body, "category"),
        want(&[("classic", 2), ("animals", 1), ("misc", 1), ("garden", 0)]),
        "got {body}"
    );
}

/// `facet_extag_ex_two_tags`: `{!ex=a,b}` is a comma list and drops both tagged
/// filters — the fully unfiltered distribution.
#[tokio::test]
async fn a_comma_list_ex_drops_every_named_tagged_filter() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0\
         &fq=%7B%21tag%3Da%7Dcategory:animals&fq=%7B%21tag%3Db%7Dcategory:classic\
         &facet=true&facet.field=%7B%21ex%3Da,b%7Dcategory&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(counts(&body, "category"), want(UNFILTERED), "got {body}");
}

/// `facet_extag_multi_tag`: one `fq` may carry several tags (`{!tag=a,b}`), and
/// `{!ex=b}` matches it by set-intersection — not string equality — so it is
/// excluded.
#[tokio::test]
async fn one_fq_carrying_two_tags_is_excluded_by_intersection() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&fq=%7B%21tag%3Da,b%7Dcategory:animals\
         &facet=true&facet.field=%7B%21ex%3Db%7Dcategory&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(counts(&body, "category"), want(UNFILTERED), "got {body}");
}

// --- composition with {!key} (finding 138) --------------------------------

/// `facet_extag_ex_with_key` / `facet_extag_key_before_ex`: `{!ex=cat key=...}`
/// and `{!key=... ex=cat}` produce byte-identical bodies — local-param order
/// carries no meaning — with the excluded counts under the key label.
#[tokio::test]
async fn ex_and_key_compose_in_either_order() {
    let (app, _dir) = indexed_app().await;
    let url_a = "select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals\
                 &facet=true&facet.field=%7B%21ex%3Dcat%20key%3Dunfiltered%7Dcategory&wt=json";
    let url_b = "select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals\
                 &facet=true&facet.field=%7B%21key%3Dunfiltered%20ex%3Dcat%7Dcategory&wt=json";
    let (status_a, body_a) = get(&app, url_a).await;
    let (status_b, body_b) = get(&app, url_b).await;
    assert_eq!(status_a, StatusCode::OK, "got {body_a}");
    assert_eq!(status_b, StatusCode::OK, "got {body_b}");
    assert_eq!(
        counts(&body_a, "unfiltered"),
        want(UNFILTERED),
        "got {body_a}"
    );
    assert_eq!(
        counts(&body_b, "unfiltered"),
        want(UNFILTERED),
        "got {body_b}"
    );
    // The two orders produce identical facet output (finding 138) — the only
    // difference between the two whole responses is the echoed
    // `params.facet.field` string, which is the raw request, not computed
    // output, and each fixture carries its own.
    assert_eq!(
        body_a.pointer("/facet_counts"),
        body_b.pointer("/facet_counts"),
        "local-param order must carry no meaning in the facet output"
    );
    assert_matches_fixture(body_a, "facet_extag_ex_with_key");
}

/// `facet_extag_both_facets`: two `facet.field` values on one field — one
/// plain-keyed (`filtered`, full filter set) and one excluded-and-keyed
/// (`unfiltered`) — both appear with different counts. This is also the fixture
/// #299 needs on the Drupal side.
#[tokio::test]
async fn two_facets_on_one_field_get_independent_filter_sets() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals&facet=true\
         &facet.field=%7B%21key%3Dfiltered%7Dcategory\
         &facet.field=%7B%21ex%3Dcat%20key%3Dunfiltered%7Dcategory&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        counts(&body, "filtered"),
        want(FILTERED_ANIMALS),
        "got {body}"
    );
    assert_eq!(counts(&body, "unfiltered"), want(UNFILTERED), "got {body}");
    assert_matches_fixture(body, "facet_extag_both_facets");
}

// --- facet.query accepts {!ex} (finding 139) ------------------------------

/// `facet_extag_facet_query_ex`: `facet.query={!ex=cat}category:classic` counts
/// with the tagged `fq` excluded (2, both classic docs) while keying the bucket
/// on the raw parameter value, `{!ex=cat}` prefix included.
#[tokio::test]
async fn facet_query_ex_counts_the_excluded_set_under_the_raw_key() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals&facet=true\
         &facet.query=%7B%21ex%3Dcat%7Dcategory:classic&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        query_count(&body, "{!ex=cat}category:classic"),
        Some(2),
        "the tagged fq must be excluded from the facet.query count (2, not 1); got {body}"
    );
    assert_matches_fixture(body, "facet_extag_facet_query_ex");
}

// --- facet.mincount / facet.missing run after exclusion (finding 140) -----

/// `facet_extag_mincount`: `facet.mincount=2` filters the post-exclusion counts
/// — keeps `animals 2, classic 2`, drops the unfiltered `1`s.
#[tokio::test]
async fn mincount_filters_the_post_exclusion_counts() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals&facet=true\
         &facet.field=%7B%21ex%3Dcat%7Dcategory&facet.mincount=2&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        counts(&body, "category"),
        want(&[("animals", 2), ("classic", 2)]),
        "got {body}"
    );
}

/// `facet_extag_missing`: `facet.missing=true` appends its `null` bucket to the
/// post-exclusion counts — doc5, which has no `category`.
#[tokio::test]
async fn missing_appends_its_bucket_to_the_post_exclusion_counts() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals&facet=true\
         &facet.field=%7B%21ex%3Dcat%7Dcategory&facet.missing=true&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    // The trailing JSON `null`/1 bucket is what `facet.missing` adds over the
    // unfiltered distribution (finding 140, post-exclusion).
    let mut expected: Vec<Value> = Vec::new();
    for (term, count) in UNFILTERED {
        expected.push(Value::String(term.to_string()));
        expected.push(Value::Number((*count).into()));
    }
    expected.push(Value::Null);
    expected.push(Value::Number(1.into()));
    assert_eq!(
        field_bucket(&body, "category").as_deref(),
        Some(expected.as_slice()),
        "got {body}"
    );
    assert_matches_fixture(body, "facet_extag_missing");
}
