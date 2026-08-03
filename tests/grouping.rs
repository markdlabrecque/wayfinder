//! Solr result grouping (issue #290, PRD §5 v3): `group=true` +
//! `group.field` (repeatable) -> the `grouped: {<field>: {matches, ngroups,
//! groups: [{groupValue, doclist}]}}` response envelope.
//!
//! ## Ground truth
//!
//! Every expected value here comes from a committed fixture in
//! `solr-ref/responses/group_*.json`, captured against a dedicated `grouping`
//! Solr core (`solr-ref/capture.sh`'s issue-#290 block, container
//! `wayfinder-solr-290`, port 8997) -- never from what Wayfinder happens to
//! produce. Finding 130 (`docs/solr-ref-findings.md`) is the source sweep that
//! narrowed the param surface: `search_api_solr`'s `setGrouping()` sends
//! exactly `group.field`, `group.ngroups=true` (unconditional), `group.truncate`,
//! `group.facet`, `group.limit` (when set & != 1), `group.offset` (when set),
//! and `group.sort` (a single comma-joined string). `group.format` and
//! `group.main` are NEVER sent, so they are out of scope and deliberately
//! absent from `SELECT_PARAMS` (they 400 under `strict_params`, as they
//! should).
//!
//! The module refuses to group on a fulltext or multiValued field, and so does
//! Solr itself (`can not use FieldCache on multivalued field`) -- so the server
//! side only needs single-valued non-text fields.
//!
//! ## Envelope shape
//!
//! ```text
//! grouped.<field>.matches   = total docs matching q AND every fq
//! grouped.<field>.ngroups   = distinct group count (only when group.ngroups=true)
//! grouped.<field>.groups[]  = ordered by the relevance of each group's top doc
//!   .groupValue             = the field value, or null for the "missing" group
//!   .doclist.numFound       = docs in this group
//!   .doclist.start          = group.offset
//!   .doclist.maxScore       = only when fl includes score
//!   .doclist.numFoundExact  = always true
//!   .doclist.docs[]         = top group.limit docs, ordered by group.sort (default relevance)
//! ```
//!
//! Tantivy has no native grouping collector, so this is a custom collector
//! (`src/grouping.rs`) that buckets each matching doc by its fast-field group
//! value, keeping the top `group.limit` per bucket.
//!
//! `rows`/`start` paginate the *groups* list (not docs); `group.limit`/
//! `group.offset` page within each group. There is no top-level `response`
//! block when `group=true` -- `grouped` replaces it (`group.main` is never
//! sent, so the flat shape is out of scope).

// The `dead_code` allow for partially-used shared helpers is an inner
// attribute inside `tests/common/mod.rs`; a second `#![allow(dead_code)]`
// here would be a clippy error under `-D warnings`.
mod common;

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;

use common::{app_with_schema, assert_matches_fixture, get, post_docs};

/// A dedicated core so `group.field` gets a single-valued string field with
/// repeated values (`type`: article={g1,g3,g4}, page={g2,g5}, plus a null
/// group {g6}), a multiValued string (`category`, for the multivalued 400),
/// and a single-valued numeric field (`popularity`, for grouping on a
/// numeric field) -- without touching `common::SCHEMA_TOML`. Named `content`
/// so `common::get`/`common::CORE` address it unchanged, the same trick
/// `tests/stats.rs::STATS_SCHEMA_TOML` uses.
const GROUPING_SCHEMA_TOML: &str = r#"
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
name = "type"
type = "string"
stored = true
fast = true

[[fields]]
name = "category"
type = "string"
stored = true
fast = true
multi_valued = true

[[fields]]
name = "popularity"
type = "int"
stored = true
fast = true
"#;

/// The exact 6-doc corpus `solr-ref/capture.sh`'s issue-#290 block indexes:
/// `type` article on g1/g3/g4, page on g2/g5, missing on g6 (the null group).
fn grouping_corpus() -> Value {
    json!([
        {"id":"g1","type":"article","category":["news"],"body":"lazy dog brown","popularity":10},
        {"id":"g2","type":"page","category":["news"],"body":"lazy garden afternoon","popularity":20},
        {"id":"g3","type":"article","category":["blog"],"body":"quick thinking saves","popularity":30},
        {"id":"g4","type":"article","category":["blog"],"body":"dogs cats together","popularity":5},
        {"id":"g5","type":"page","body":"nothing here","popularity":40},
        {"id":"g6","body":"orphan ungrouped","popularity":15}
    ])
}

/// Builds an app on `GROUPING_SCHEMA_TOML` and indexes `grouping_corpus()`.
async fn grouping_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = app_with_schema(dir.path(), GROUPING_SCHEMA_TOML).expect("app must build");
    let (status, body) = post_docs(&app, &grouping_corpus()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the grouping corpus must succeed, got {body}"
    );
    (app, dir)
}

/// `(groupValue, [doc ids], maxScore)` for each group in the response, in
/// order -- the projection almost every test below reasons about.
fn groups_of(body: &Value, field: &str) -> Vec<(Value, Vec<String>, Option<f64>)> {
    body.pointer(&format!("/grouped/{field}/groups"))
        .and_then(Value::as_array)
        .expect("grouped.<field>.groups must be an array")
        .iter()
        .map(|g| {
            let gv = g.get("groupValue").cloned().unwrap_or(Value::Null);
            let docs: Vec<String> = g
                .pointer("/doclist/docs")
                .and_then(Value::as_array)
                .expect("doclist.docs must be an array")
                .iter()
                .map(|d| {
                    d.get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_string()
                })
                .collect();
            let max = g.pointer("/doclist/maxScore").and_then(Value::as_f64);
            (gv, docs, max)
        })
        .collect()
}

// --- the captured envelope shape ------------------------------------------

/// The canonical envelope, pinned byte-for-byte (modulo QTime) against
/// `group_basic.json`: matches=6, ngroups=3, three groups in relevance-then-
/// doc-address order (article/page/null), each with one doc (the default
/// `group.limit=1`).
#[tokio::test]
async fn grouping_basic_envelope_matches_fixture() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.ngroups=true&fl=id&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body, "group_basic");
}

/// `group.limit` defaults to 1: every group carries exactly one doc even
/// though `article` has three. This is finding 130's "group.limit only when
/// set & != 1" -- the module omits it precisely because Solr's default is 1.
#[tokio::test]
async fn grouping_default_group_limit_is_one() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.ngroups=true&fl=id&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    for (_gv, docs, _) in groups_of(&body, "type") {
        assert_eq!(
            docs.len(),
            1,
            "default group.limit=1 must cap each group at one doc, got {body}"
        );
    }
}

/// The whole `response` block is absent under `group=true`: `grouped`
/// replaces it. (`group.main` would merge groups back into `response`, but
/// the module never sends it -- finding 130 -- so that shape is out of scope.)
#[tokio::test]
async fn grouping_replaces_the_response_block() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.ngroups=true&fl=id&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert!(
        body.get("response").is_none(),
        "group=true must replace the top-level response block with grouped, got {body}"
    );
    assert!(body.get("grouped").is_some(), "got {body}");
}

// --- group.ngroups ---------------------------------------------------------

/// `ngroups` is present only when `group.ngroups=true` (which the module sends
/// unconditionally -- finding 130). With the param absent, the key is absent
/// too, not zero.
#[tokio::test]
async fn grouping_ngroups_key_absent_when_not_requested() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&fl=id&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "group_ngroups_off");
    assert!(
        body.pointer("/grouped/type/ngroups").is_none(),
        "ngroups must be absent when group.ngroups is not set, got {body}"
    );
    // `matches` is still there either way.
    assert_eq!(
        body.pointer("/grouped/type/matches")
            .and_then(Value::as_u64),
        Some(6),
        "matches is independent of group.ngroups, got {body}"
    );
}

// --- group.limit -----------------------------------------------------------

/// `group.limit=2` returns up to two docs per group, within-group ordered by
/// relevance then doc address: article [g1, g3], page [g2, g5]. The null
/// group has only g6.
#[tokio::test]
async fn grouping_group_limit_returns_multiple_docs_per_group() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.limit=2&group.ngroups=true&fl=id&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "group_limit");
    let groups = groups_of(&body, "type");
    assert_eq!(
        groups,
        vec![
            (json!("article"), vec!["g1".into(), "g3".into()], None),
            (json!("page"), vec!["g2".into(), "g5".into()], None),
            (json!(null), vec!["g6".into()], None),
        ],
        "group.limit=2 must return up to two docs per group in within-group order, got {body}"
    );
}

// --- group.offset ----------------------------------------------------------

/// `group.offset=1` skips the first doc of each group and sets
/// `doclist.start=1`. The null group (one doc) becomes an empty `docs` array
/// but keeps `numFound: 1`.
#[tokio::test]
async fn grouping_group_offset_skips_within_group() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.offset=1&group.ngroups=true&fl=id&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "group_offset");
    let groups = groups_of(&body, "type");
    assert_eq!(
        groups,
        vec![
            (json!("article"), vec!["g3".into()], None),
            (json!("page"), vec!["g5".into()], None),
            (json!(null), vec![], None),
        ],
        "group.offset=1 must skip the first doc of each group, got {body}"
    );
    // doclist.start mirrors group.offset.
    assert_eq!(
        body.pointer("/grouped/type/groups/0/doclist/start")
            .and_then(Value::as_u64),
        Some(1),
        "doclist.start must equal group.offset, got {body}"
    );
    // numFound is unaffected by offset -- it is the whole group's count.
    assert_eq!(
        body.pointer("/grouped/type/groups/0/doclist/numFound")
            .and_then(Value::as_u64),
        Some(3),
        "numFound must stay the group's full count regardless of offset, got {body}"
    );
}

// --- rows / start paginate GROUPS -----------------------------------------

/// `rows`/`start` paginate the *groups* list, not docs: `rows=2&start=1`
/// drops the first group (article) and returns the next two (page, null).
#[tokio::test]
async fn grouping_rows_and_start_paginate_the_groups_list() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&rows=2&start=1&group.ngroups=true&fl=id&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "group_rows_start");
    let group_values: Vec<Value> = groups_of(&body, "type")
        .into_iter()
        .map(|(gv, _, _)| gv)
        .collect();
    assert_eq!(
        group_values,
        vec![json!("page"), json!(null)],
        "rows=2&start=1 must return the 2nd and 3rd groups (page, null), skipping article, got {body}"
    );
}

// --- group.sort ------------------------------------------------------------

/// `group.sort` orders docs *within* each group: `id desc` gives article
/// [g4, g3], page [g5, g2].
#[tokio::test]
async fn grouping_group_sort_orders_within_group() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.sort=id+desc&group.limit=2&group.ngroups=true&fl=id&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "group_sort");
    let groups = groups_of(&body, "type");
    assert_eq!(
        groups,
        vec![
            (json!("article"), vec!["g4".into(), "g3".into()], None),
            (json!("page"), vec!["g5".into(), "g2".into()], None),
            (json!(null), vec!["g6".into()], None),
        ],
        "group.sort=id desc must order within-group docs by id descending, got {body}"
    );
}

// --- multiple group.field --------------------------------------------------

/// Repeatable `group.field` produces one keyed block per field, in request
/// order. `type` (2 real groups + null) and `id` (every doc its own group).
#[tokio::test]
async fn grouping_multiple_group_fields_produce_separate_keys() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.field=id&group.ngroups=true&fl=id&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "group_multi_field");
    let keys: Vec<&str> = body
        .pointer("/grouped")
        .and_then(Value::as_object)
        .expect("grouped must be an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        vec!["type", "id"],
        "grouped must carry one key per group.field in request order, got {body}"
    );
    assert_eq!(
        body.pointer("/grouped/id/ngroups").and_then(Value::as_u64),
        Some(6),
        "grouping on the unique-key id yields one group per doc (6), got {body}"
    );
}

// --- numeric group field ---------------------------------------------------

/// Grouping on a numeric field (`popularity`): `groupValue` is the number,
/// and with `q=*:*` groups follow top-doc-address order (g1..g6).
#[tokio::test]
async fn grouping_numeric_group_field() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=popularity&group.ngroups=true&fl=id&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "group_numeric");
    let groups = groups_of(&body, "popularity");
    assert_eq!(
        groups,
        vec![
            (json!(10), vec!["g1".into()], None),
            (json!(20), vec!["g2".into()], None),
            (json!(30), vec!["g3".into()], None),
            (json!(5), vec!["g4".into()], None),
            (json!(40), vec!["g5".into()], None),
            (json!(15), vec!["g6".into()], None),
        ],
        "grouping on a numeric field must report numeric groupValues, got {body}"
    );
}

// --- fq interacts with grouping -------------------------------------------

/// `fq` narrows the match set *before* grouping: `fq=type:article` leaves
/// matches=3 and a single `article` group.
#[tokio::test]
async fn grouping_fq_filters_before_grouping() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&fq=type:article&group=true&group.field=type&group.ngroups=true&fl=id&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "group_fq");
    assert_eq!(
        body.pointer("/grouped/type/matches")
            .and_then(Value::as_u64),
        Some(3),
        "matches must reflect the fq-narrowed doc set, got {body}"
    );
    assert_eq!(
        body.pointer("/grouped/type/ngroups")
            .and_then(Value::as_u64),
        Some(1),
        "only one distinct group value survives the fq, got {body}"
    );
}

// --- fl=score -> maxScore --------------------------------------------------

/// `fl=id,score` turns scoring on inside each group: `doclist.maxScore`
/// appears and each doc carries `score`. With `q=*:*` every score is 1.0.
#[tokio::test]
async fn grouping_fl_score_adds_maxscore_to_each_doclist() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.ngroups=true&fl=id,score&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "group_fl_score");
    for (_gv, _docs, max) in groups_of(&body, "type") {
        assert_eq!(
            max,
            Some(1.0),
            "fl=id,score must populate doclist.maxScore (1.0 for q=*:*), got {body}"
        );
    }
}

// --- zero matches ----------------------------------------------------------

/// A query matching nothing yields matches=0, ngroups=0, and an empty groups
/// array -- not an error.
#[tokio::test]
async fn grouping_zero_matches_yields_empty_groups() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=zzznomatch&df=body&group=true&group.field=type&group.ngroups=true&fl=id&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "group_zero");
    assert_eq!(
        body.pointer("/grouped/type/matches")
            .and_then(Value::as_u64),
        Some(0),
        "got {body}"
    );
    assert_eq!(
        body.pointer("/grouped/type/ngroups")
            .and_then(Value::as_u64),
        Some(0),
        "got {body}"
    );
    assert_eq!(groups_of(&body, "type").len(), 0, "got {body}");
}

// --- the null group --------------------------------------------------------

/// A document missing the group field lands in a `groupValue: null` group.
/// g6 has no `type`, so the third group is the null group.
#[tokio::test]
async fn grouping_docs_missing_the_field_form_a_null_group() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.ngroups=true&fl=id&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let group_values: Vec<Value> = groups_of(&body, "type")
        .into_iter()
        .map(|(gv, _, _)| gv)
        .collect();
    assert!(
        group_values.contains(&Value::Null),
        "a doc missing the group field must form a groupValue:null group, got {body}"
    );
}

// --- group ordering by relevance ------------------------------------------

/// Groups are ordered by the relevance of their top document. `q=lazy garden`
/// (OR, the parser's default) matches g1 (article, hits `lazy`) and g2 (page,
/// hits `lazy` + `garden`); g2 matches more query terms, so it scores higher
/// under BM25 and the `page` group ranks before `article`. Only score *order*
/// is asserted -- BM25 magnitude is a ratified Tantivy-vs-Solr divergence
/// (PRD §5), not pinned here.
///
/// (Earlier this used `q=lazy`, but g1 and g2 both carry `lazy` once in
/// 3-token bodies, so their scores tie and the doc-address tiebreak -- not
/// relevance -- decided the order. That was a wrong test premise; the
/// corrected query makes the score gap real.)
#[tokio::test]
async fn grouping_orders_groups_by_top_doc_relevance() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=lazy+garden&df=body&group=true&group.field=type&group.limit=1&group.ngroups=true&fl=id&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        body.pointer("/grouped/type/matches")
            .and_then(Value::as_u64),
        Some(2),
        "q=lazy garden matches exactly g1 and g2, got {body}"
    );
    let group_values: Vec<Value> = groups_of(&body, "type")
        .into_iter()
        .map(|(gv, _, _)| gv)
        .collect();
    assert_eq!(
        group_values,
        vec![json!("page"), json!("article")],
        "the page group (g2, matches both terms, higher BM25) must rank before article (g1), got {body}"
    );
}

// --- error shapes ----------------------------------------------------------

/// `group=true` with no `group.field` is a 400 (Solr: "Specify at least one
/// field, function or query to group by."). The differential harness
/// normalises error.msg/metadata away, so this dedicated test pins the message.
#[tokio::test]
async fn grouping_no_group_field_is_a_400() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(&app, "select?q=*:*&group=true&fl=id&wt=json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert_eq!(
        body.pointer("/error/code").and_then(Value::as_u64),
        Some(400),
        "got {body}"
    );
    assert!(
        body.get("grouped").is_none(),
        "a rejected grouping must not render a grouped block, got {body}"
    );
}

/// An unknown `group.field` is a 400 (Solr: `undefined field: "nosuchfield"`).
#[tokio::test]
async fn grouping_unknown_group_field_is_a_400() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=nosuchfield&fl=id&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert_eq!(
        body.pointer("/error/code").and_then(Value::as_u64),
        Some(400),
        "got {body}"
    );
    let msg = body
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("no /error/msg in {body}"));
    assert!(
        msg.contains("nosuchfield"),
        "error.msg should name the offending field, got {msg:?}"
    );
}

/// A multiValued `group.field` is a 400 (Solr: "can not use FieldCache on
/// multivalued field"). The module also refuses to send one (finding 130),
/// so this guards the server side against a directly-issued request.
#[tokio::test]
async fn grouping_multivalued_group_field_is_a_400() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=category&fl=id&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert_eq!(
        body.pointer("/error/code").and_then(Value::as_u64),
        Some(400),
        "got {body}"
    );
    let msg = body
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("no /error/msg in {body}"));
    assert!(
        msg.contains("category") && msg.contains("multivalued"),
        "error.msg should name the field and say it is multivalued, got {msg:?}"
    );
}

/// Mutation guard for the multivalued rejection above: if the check were
/// removed (grouping silently proceeded on `category`), this must fail. A
/// correct implementation 400s, so an `article`/`animals` groupValue must
/// never appear.
#[tokio::test]
async fn grouping_multivalued_rejection_is_not_lossy() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=category&fl=id&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert!(
        body.get("grouped").is_none(),
        "a multivalued group.field must not produce a grouped block at all -- \
         a check that proceeds and returns category groups would be the bug \
         this guards, got {body}"
    );
    assert!(
        !body.to_string().contains("animals"),
        "no category value should leak into the response, got {body}"
    );
}

// --- group.format / group.main stay rejected ------------------------------

/// `group.format` is never sent by the module (finding 130) and is out of
/// scope, so it stays absent from `SELECT_PARAMS` and 400s under
/// `strict_params`. Wayfinder must not silently accept a param it does not
/// implement -- that converts a loud 400 into a silently wrong answer.
///
/// `strict_params` is a server config, not a per-request param, so this builds
/// an app with `strict_params = true` (the same shape `tests/mlt.rs`'s
/// `mlt_specific_params_are_not_rejected_as_unknown` uses).
#[tokio::test]
async fn grouping_format_is_rejected_under_strict_params() {
    let (app, _dir) = strict_grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.format=simple&fl=id&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "group.format is unimplemented and must 400 under strict_params, not be \
         silently accepted, got {body}"
    );
    let msg = body
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        msg.contains("group.format"),
        "the rejection should name group.format, got {msg:?}"
    );
}

/// Same for `group.main`.
#[tokio::test]
async fn grouping_main_is_rejected_under_strict_params() {
    let (app, _dir) = strict_grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.main=true&fl=id&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "group.main is unimplemented and must 400 under strict_params, got {body}"
    );
}

/// An app on `GROUPING_SCHEMA_TOML` with `strict_params = true`, indexed with
/// the grouping corpus. Separate from `grouping_app()` because strictness is a
/// server-wide config, not toggleable per request.
async fn strict_grouping_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, GROUPING_SCHEMA_TOML).expect("write schema.toml");
    let config_path = dir.path().join("wayfinder.toml");
    std::fs::write(&config_path, "strict_params = true\n").expect("write wayfinder.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let app =
        wayfinder::app_with_config(&schema_path, &data_dir, &config_path).expect("app must build");
    let (status, body) = post_docs(&app, &grouping_corpus()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the grouping corpus must succeed, got {body}"
    );
    (app, dir)
}

/// As a sanity check that the strict app above really is strict (and so the
/// two rejection tests are meaningful), a genuinely unknown param still 400s.
#[tokio::test]
async fn strict_grouping_app_rejects_a_truly_unknown_param() {
    let (app, _dir) = strict_grouping_app().await;
    let (status, _body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&not_a_real_param=1&fl=id&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "the strict app must still reject a param nothing knows about -- if it \
         does not, the group.format/group.main rejection tests above are \
         testing nothing"
    );
}

/// `group.truncate` and `group.facet` ARE sent by `setGrouping()` (finding
/// 130), so unlike `group.format`/`group.main` they must NOT 400 under
/// `strict_params` -- a real module request would otherwise break. Their
/// TRUE semantics (computing `facet_counts` over collapsed groups) are not
/// fixture-backed yet, so for now both default to a no-op: with `facet`
/// absent, neither has any effect, and this request just returns the normal
/// grouped envelope. This test is the parity guard -- if either param is
/// accidentally dropped from `SELECT_PARAMS`, this 400s and the guard fires.
///
/// ponytail: the ceiling is the group+facet interaction. When a fixture
/// capturing `group.truncate=true`/`group.facet=true` alongside `facet=true`
/// lands, the truncated-facet computation must be implemented in
/// `src/grouping.rs`/`src/facet.rs` and this test broadened to assert it.
#[tokio::test]
async fn grouping_truncate_and_facet_params_are_accepted_under_strict_params() {
    let (app, _dir) = strict_grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.truncate=true&group.facet=true&group.ngroups=true&fl=id&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "group.truncate/group.facet are sent by the module (finding 130) and must \
         be accepted, not 400 under strict_params, got {body}"
    );
    // Sanity: the grouped envelope is still produced and well-formed.
    assert_eq!(
        body.pointer("/grouped/type/matches")
            .and_then(Value::as_u64),
        Some(6),
        "the grouped envelope must still render, got {body}"
    );
}
