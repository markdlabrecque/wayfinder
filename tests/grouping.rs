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

use common::{app_with_schema, assert_matches_fixture, fixture, get, post_docs};

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
/// `strict_params` -- a real module request would otherwise break. Their TRUE
/// semantics are now fixture-backed (findings 159/160/161, issue #338): this
/// replaces the old "accepted but no-op" parity guard with a real assertion
/// against `g338_groupfacet_truncate` (`group.truncate=true&group.facet=true`
/// together, with `facet=true`), so a dropped-from-`SELECT_PARAMS` regression
/// AND a regressed-to-no-op regression both fail this test.
#[tokio::test]
async fn grouping_truncate_and_facet_params_are_accepted_under_strict_params() {
    let (app, _dir) = strict_grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.truncate=true&group.facet=true&group.ngroups=true&facet=true&facet.field=type&facet.field=category&fl=id&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "group.truncate/group.facet are sent by the module (finding 130) and must \
         be accepted, not 400 under strict_params, got {body}"
    );
    // Real semantics, not the no-op parity guard this replaces:
    // `g338_groupfacet_truncate` -- type article=1/page=1, category news=2/blog=0.
    assert_matches_fixture(body, "g338_groupfacet_truncate");
}

// ===========================================================================
// Issue #338: facet_counts / stats / highlighting alongside `grouped`, and
// real `group.truncate` / `group.facet` semantics.
//
// Every expected value below comes from a committed `solr-ref/responses/
// g338_*.json` fixture (findings 159/160/161) -- named in each test's doc
// comment -- never from what Wayfinder happens to produce. Two exceptions
// (explicitly marked) assert a structural self-consistency property named by
// the task spec rather than a captured Solr number, because no g338 fixture
// captures that exact param combination.
// ===========================================================================

// --- section 1: component blocks alongside `grouped` -----------------------

/// A grouped request with `facet=true` and `stats=true` gets `facet_counts`
/// and `stats` blocks alongside `grouped`, in top-level key order
/// `responseHeader, grouped, facet_counts, stats` (`highlighting` absent
/// since `hl` was not requested). `g338_all`: `type` article=3/page=2,
/// `category` blog=2/news=2, `stats.field=popularity` count=6/min=5/max=40/
/// sum=120.
#[tokio::test]
async fn grouping_facet_and_stats_blocks_render_alongside_grouped() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.ngroups=true&facet=true&facet.field=type&facet.field=category&stats=true&stats.field=popularity&fl=id&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "g338_all");
    let keys: Vec<&str> = body
        .as_object()
        .expect("top-level body must be an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        vec!["responseHeader", "grouped", "facet_counts", "stats"],
        "top-level key order must be responseHeader, grouped, facet_counts, \
         stats -- the same order the ungrouped path already uses, got {body}"
    );
    assert_eq!(
        body.pointer("/facet_counts/facet_fields/type"),
        Some(&json!(["article", 3, "page", 2])),
        "got {body}"
    );
    assert_eq!(
        body.pointer("/facet_counts/facet_fields/category"),
        Some(&json!(["blog", 2, "news", 2])),
        "got {body}"
    );
    assert_eq!(
        body.pointer("/stats/stats_fields/popularity/count"),
        Some(&json!(6)),
        "got {body}"
    );
}

/// `facet=true` alone (no `stats`, no `hl`): `facet_counts` renders and
/// `stats`/`highlighting` are absent. `g338_facet`.
#[tokio::test]
async fn grouping_facet_block_alone_omits_stats_and_highlighting() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.ngroups=true&facet=true&facet.field=type&facet.field=category&fl=id&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "g338_facet");
    assert!(body.get("stats").is_none(), "got {body}");
    assert!(body.get("highlighting").is_none(), "got {body}");
}

/// `stats=true` alone (no `facet`, no `hl`): `stats` renders and
/// `facet_counts`/`highlighting` are absent. `g338_stats`.
#[tokio::test]
async fn grouping_stats_block_alone_omits_facet_and_highlighting() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.ngroups=true&stats=true&stats.field=popularity&fl=id&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "g338_stats");
    assert!(body.get("facet_counts").is_none(), "got {body}");
    assert!(body.get("highlighting").is_none(), "got {body}");
}

/// `highlighting` is keyed by unique-key value and covers only the documents
/// the doclists actually returned, not every doc that matched `q`.
/// `g338_hl`: `q=lazy` with `group.limit=2` matches only g1 (article) and g2
/// (page), so `highlighting` has exactly those two keys.
#[tokio::test]
async fn grouping_highlighting_covers_only_rendered_doclist_docs() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=lazy&df=body&group=true&group.field=type&group.ngroups=true&group.limit=2&hl=true&hl.fl=body&fl=id&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "g338_hl");
    let mut keys: Vec<&str> = body
        .pointer("/highlighting")
        .and_then(Value::as_object)
        .expect("highlighting must be an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        vec!["g1", "g2"],
        "highlighting must contain exactly the rendered doclist docs, got {body}"
    );
}

/// A zero-match grouped request still emits the full empty `facet_counts`
/// shape (all five sub-keys, counts 0) and the empty `stats` shape (min/max
/// null, mean "NaN", stddev 0.0) next to an empty `grouped` block.
/// `g338_zero`.
#[tokio::test]
async fn grouping_zero_matches_still_emits_full_empty_facet_and_stats_shape() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=zzznomatch&df=body&group=true&group.field=type&group.ngroups=true&facet=true&facet.field=type&facet.field=category&stats=true&stats.field=popularity&fl=id&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "g338_zero");
    assert_eq!(
        body.pointer("/grouped/type"),
        Some(&json!({"matches": 0, "ngroups": 0, "groups": []})),
        "got {body}"
    );
    assert_eq!(
        body.pointer("/facet_counts/facet_fields/type"),
        Some(&json!(["article", 0, "page", 0])),
        "got {body}"
    );
    assert_eq!(
        body.pointer("/stats/stats_fields/popularity/min"),
        Some(&Value::Null),
        "got {body}"
    );
    assert_eq!(
        body.pointer("/stats/stats_fields/popularity/mean"),
        Some(&json!("NaN")),
        "got {body}"
    );
    assert_eq!(
        body.pointer("/stats/stats_fields/popularity/stddev"),
        Some(&json!(0.0)),
        "got {body}"
    );
}

// --- section 2: group.truncate ----------------------------------------------

/// `group.truncate=true` computes facets over the collapsed group set (one
/// doc per group, the group's `group.sort`-first doc): `{g1, g2, g6}`. `type`
/// becomes article=1/page=1; `category` becomes news=2/blog=0. The `grouped`
/// block itself stays untouched (matches=6, ngroups=3). `g338_truncate`.
#[tokio::test]
async fn grouping_truncate_collapses_facets_to_one_doc_per_group() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.ngroups=true&group.truncate=true&facet=true&facet.field=type&facet.field=category&fl=id&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "g338_truncate");
    assert_eq!(
        body.pointer("/facet_counts/facet_fields/type"),
        Some(&json!(["article", 1, "page", 1])),
        "got {body}"
    );
    assert_eq!(
        body.pointer("/facet_counts/facet_fields/category"),
        Some(&json!(["news", 2, "blog", 0])),
        "got {body}"
    );
    assert_eq!(
        body.pointer("/grouped/type/matches")
            .and_then(Value::as_u64),
        Some(6),
        "the grouped block's matches must stay the full match count, got {body}"
    );
    assert_eq!(
        body.pointer("/grouped/type/ngroups")
            .and_then(Value::as_u64),
        Some(3),
        "the grouped block's ngroups must stay untouched by truncate, got {body}"
    );
}

/// `group.truncate=true` also collapses `stats`: `stats.field=popularity`
/// over `{g1, g2, g6}` (popularity 10/20/15) is count=3/min=10/max=20/sum=45/
/// mean=15/stddev=5/sumOfSquares=725. `g338_truncate_stats`.
#[tokio::test]
async fn grouping_truncate_collapses_stats_too() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.ngroups=true&group.truncate=true&stats=true&stats.field=popularity&fl=id&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "g338_truncate_stats");
    let stats = body
        .pointer("/stats/stats_fields/popularity")
        .expect("stats.stats_fields.popularity must be present");
    assert_eq!(stats.get("count"), Some(&json!(3)), "got {body}");
    assert_eq!(stats.get("min"), Some(&json!(10.0)), "got {body}");
    assert_eq!(stats.get("max"), Some(&json!(20.0)), "got {body}");
    assert_eq!(stats.get("sum"), Some(&json!(45.0)), "got {body}");
    assert_eq!(stats.get("mean"), Some(&json!(15.0)), "got {body}");
    assert_eq!(stats.get("stddev"), Some(&json!(5.0)), "got {body}");
    assert_eq!(stats.get("sumOfSquares"), Some(&json!(725.0)), "got {body}");
}

/// `group.truncate=true` also collapses `facet.query` and `facet.range`:
/// `facet.query=category:blog` 2 -> 0 (blog is on g3/g4, neither collapsed
/// in); `facet.range` popularity `[0:4, 25:2]` -> `[0:3, 25:0]`.
/// `g338_truncate_qr`.
#[tokio::test]
async fn grouping_truncate_collapses_facet_query_and_range() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.ngroups=true&group.truncate=true&facet=true&facet.query=category:blog&facet.range=popularity&f.popularity.facet.range.start=0&f.popularity.facet.range.end=50&f.popularity.facet.range.gap=25&fl=id&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "g338_truncate_qr");
    assert_eq!(
        body.pointer("/facet_counts/facet_queries/category:blog"),
        Some(&json!(0)),
        "got {body}"
    );
    assert_eq!(
        body.pointer("/facet_counts/facet_ranges/popularity/counts"),
        Some(&json!(["0", 3, "25", 0])),
        "got {body}"
    );
}

/// Paging-independent: `rows=1` returns only one group in `grouped`, but the
/// facet block is still computed over all three collapsed docs, identical to
/// the unpaged `g338_truncate` counts. `g338_truncate_rows`.
#[tokio::test]
async fn grouping_truncate_facets_are_paging_independent() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.ngroups=true&group.truncate=true&rows=1&facet=true&facet.field=type&facet.field=category&fl=id&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "g338_truncate_rows");
    assert_eq!(
        body.pointer("/grouped/type/groups")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1),
        "rows=1 must still return only one group, got {body}"
    );
    assert_eq!(
        body.pointer("/facet_counts/facet_fields/type"),
        Some(&json!(["article", 1, "page", 1])),
        "the facet block must be paging-independent -- same counts as the \
         unpaged g338_truncate fixture, got {body}"
    );
    assert_eq!(
        body.pointer("/facet_counts/facet_fields/category"),
        Some(&json!(["news", 2, "blog", 0])),
        "got {body}"
    );
}

/// With two `group.field` values, truncate collapses on the FIRST
/// (`type`'s 3-doc collapse), not the second (`popularity`'s 6 singletons,
/// which would leave every facet count unchanged). `g338_truncate_multi`.
#[tokio::test]
async fn grouping_truncate_collapses_on_the_first_group_field() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.field=popularity&group.ngroups=true&group.truncate=true&facet=true&facet.field=type&facet.field=category&fl=id&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "g338_truncate_multi");
    assert_eq!(
        body.pointer("/facet_counts/facet_fields/type"),
        Some(&json!(["article", 1, "page", 1])),
        "truncate must collapse on the first group.field (type), not the \
         second (popularity, which would leave counts at article=3/page=2), \
         got {body}"
    );
    assert_eq!(
        body.pointer("/facet_counts/facet_fields/category"),
        Some(&json!(["news", 2, "blog", 0])),
        "got {body}"
    );
    // Both grouped.<field> blocks still render, untouched.
    assert_eq!(
        body.pointer("/grouped/popularity/ngroups")
            .and_then(Value::as_u64),
        Some(6),
        "got {body}"
    );
}

/// `group.sort=id desc` moves the collapsed set from `{g1, g2, g6}` to
/// `{g4, g5, g6}` (each group's LAST doc by id, since group.sort reverses
/// within-group order): `category` becomes blog=1/news=0.
/// `g338_truncate_groupsort`.
#[tokio::test]
async fn grouping_truncate_collapse_follows_group_sort_not_main_sort() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.ngroups=true&group.truncate=true&group.sort=id+desc&facet=true&facet.field=type&facet.field=category&fl=id&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "g338_truncate_groupsort");
    assert_eq!(
        body.pointer("/facet_counts/facet_fields/category"),
        Some(&json!(["blog", 1, "news", 0])),
        "group.sort, not the main sort, must decide which doc each group \
         collapses to, got {body}"
    );
    let groups = groups_of(&body, "type");
    assert_eq!(
        groups,
        vec![
            (json!("article"), vec!["g4".into()], None),
            (json!("page"), vec!["g5".into()], None),
            (json!(null), vec!["g6".into()], None),
        ],
        "got {body}"
    );
}

/// `group.truncate=false` is byte-identical to omitting `group.truncate`
/// entirely: facet counts stay at the full, uncollapsed article=3/page=2,
/// blog=2/news=2. `g338_truncate_false`.
#[tokio::test]
async fn grouping_truncate_false_is_identical_to_omitted() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.ngroups=true&group.truncate=false&facet=true&facet.field=type&facet.field=category&fl=id&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "g338_truncate_false");
    assert_eq!(
        body.pointer("/facet_counts/facet_fields/type"),
        Some(&json!(["article", 3, "page", 2])),
        "group.truncate=false must behave exactly as if omitted, got {body}"
    );
}

// --- section 3: group.facet --------------------------------------------------

/// `group.facet=true` counts distinct matching GROUPS, not documents, using
/// the first `group.field`'s grouping. `category` blog is on g3/g4, both
/// `article`, so blog becomes 1 (was 2 documents); news is on g1(article)/
/// g2(page), two distinct groups, so news stays 2. Faceting on the group
/// field itself gives 1 per value: `type` article=1/page=1. `g338_groupfacet`.
#[tokio::test]
async fn grouping_group_facet_counts_distinct_groups_not_documents() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.ngroups=true&group.facet=true&facet=true&facet.field=type&facet.field=category&fl=id&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "g338_groupfacet");
    assert_eq!(
        body.pointer("/facet_counts/facet_fields/type"),
        Some(&json!(["article", 1, "page", 1])),
        "got {body}"
    );
    assert_eq!(
        body.pointer("/facet_counts/facet_fields/category"),
        Some(&json!(["news", 2, "blog", 1])),
        "blog (g3,g4, both article) must count as 1 distinct group, not 2 \
         documents, got {body}"
    );
}

/// `group.facet=true` leaves `stats` untouched -- it still reports the full,
/// ungrouped figures (count=6/min=5/max=40/mean=20/etc), matching plain
/// `stats=true` (`g338_stats`). `g338_groupfacet_stats`.
#[tokio::test]
async fn grouping_group_facet_leaves_stats_untouched() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.ngroups=true&group.facet=true&stats=true&stats.field=popularity&fl=id&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "g338_groupfacet_stats");
    let stats = body
        .pointer("/stats/stats_fields/popularity")
        .expect("stats.stats_fields.popularity must be present");
    assert_eq!(stats.get("count"), Some(&json!(6)), "got {body}");
    assert_eq!(stats.get("min"), Some(&json!(5.0)), "got {body}");
    assert_eq!(stats.get("max"), Some(&json!(40.0)), "got {body}");
    assert_eq!(stats.get("mean"), Some(&json!(20.0)), "got {body}");
}

/// `group.facet=true` also regroups `facet.query`: `category:blog` matches
/// documents g3 and g4 (both `article`), so the plain document count is 2
/// (`g338_facet_blog`) but the group-facet count is 1 distinct group
/// (`g338_groupfacet_blog`).
#[tokio::test]
async fn grouping_group_facet_regroups_facet_query() {
    let (app, _dir) = grouping_app().await;

    let (status, plain) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.ngroups=true&facet=true&facet.query=category:blog&facet.range=popularity&f.popularity.facet.range.start=0&f.popularity.facet.range.end=50&f.popularity.facet.range.gap=25&fl=id&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {plain}");
    assert_matches_fixture(plain.clone(), "g338_facet_blog");
    assert_eq!(
        plain.pointer("/facet_counts/facet_queries/category:blog"),
        Some(&json!(2)),
        "got {plain}"
    );

    let (status, grouped) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.ngroups=true&group.facet=true&facet=true&facet.query=category:blog&facet.range=popularity&f.popularity.facet.range.start=0&f.popularity.facet.range.end=50&f.popularity.facet.range.gap=25&fl=id&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {grouped}");
    assert_matches_fixture(grouped.clone(), "g338_groupfacet_blog");
    assert_eq!(
        grouped.pointer("/facet_counts/facet_queries/category:blog"),
        Some(&json!(1)),
        "group.facet must regroup facet.query counts to distinct groups (1), \
         not documents (2), got {grouped}"
    );
}

/// `group.facet=true` also regroups `facet.range`: the `0-25` popularity
/// bucket holds documents g1/g2/g4/g6 (4 documents, `g338_facet_blog`), but
/// only 3 distinct groups (article via g1 or g4, page via g2, null via g6),
/// so the group-faceted count is 3 (`g338_groupfacet_blog`); the `25-50`
/// bucket (g3/g5, article+page, 2 distinct groups) stays 2 either way.
#[tokio::test]
async fn grouping_group_facet_regroups_facet_range() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.ngroups=true&group.facet=true&facet=true&facet.query=category:blog&facet.range=popularity&f.popularity.facet.range.start=0&f.popularity.facet.range.end=50&f.popularity.facet.range.gap=25&fl=id&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "g338_groupfacet_blog");
    assert_eq!(
        body.pointer("/facet_counts/facet_ranges/popularity/counts"),
        Some(&json!(["0", 3, "25", 2])),
        "the 0-25 bucket has 4 documents but only 3 distinct groups, got {body}"
    );
}

/// `group.facet=true` is paging-independent: `rows=1` returns only one group
/// in `grouped`, but the group-facet block is still computed over all groups.
/// `g338_groupfacet_rows`.
#[tokio::test]
async fn grouping_group_facet_is_paging_independent() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.ngroups=true&group.facet=true&rows=1&facet=true&facet.field=type&facet.field=category&fl=id&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "g338_groupfacet_rows");
    assert_eq!(
        body.pointer("/grouped/type/groups")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1),
        "rows=1 must still return only one group, got {body}"
    );
    assert_eq!(
        body.pointer("/facet_counts/facet_fields/category"),
        Some(&json!(["news", 2, "blog", 1])),
        "the group-facet block must be paging-independent, got {body}"
    );
}

/// With two `group.field` values, `group.facet=true` counts come from the
/// FIRST field (`type`): identical to the single-field `g338_groupfacet`
/// counts, not derived from `popularity`'s 6 singleton groups (which would
/// leave every count at the plain document count).
/// `g338_groupfacet_multi`.
#[tokio::test]
async fn grouping_group_facet_uses_the_first_group_field() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.field=popularity&group.ngroups=true&group.facet=true&facet=true&facet.field=type&facet.field=category&fl=id&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "g338_groupfacet_multi");
    assert_eq!(
        body.pointer("/facet_counts/facet_fields/category"),
        Some(&json!(["news", 2, "blog", 1])),
        "group.facet must use the first group.field (type), not the second \
         (popularity, whose singleton groups would leave blog at 2), got {body}"
    );
}

/// Combined with `group.truncate=true`: truncation applies first (collapsing
/// to `{g1, g2, g6}`), then group counting runs over that truncated set --
/// every truncated doc is its own group, so the counts equal the
/// truncate-only counts (`g338_truncate`'s facet block). This is the general-
/// implementation check the spec calls for: no special-casing the combined
/// flags. `g338_groupfacet_truncate`.
#[tokio::test]
async fn grouping_group_facet_combined_with_truncate_matches_truncate_only_counts() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.ngroups=true&group.facet=true&group.truncate=true&facet=true&facet.field=type&facet.field=category&fl=id&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body.clone(), "g338_groupfacet_truncate");
    // The spec's explicit cross-check: this must equal g338_truncate's
    // facet_counts block byte-for-byte, proving the combination is derived
    // from a general implementation rather than special-cased.
    let truncate_only = fixture("g338_truncate");
    assert_eq!(
        body.get("facet_counts"),
        truncate_only.get("facet_counts"),
        "group.facet + group.truncate must equal group.truncate alone -- every \
         truncated doc is its own group, got {body}"
    );
}

// --- required extras: facet flags don't turn faceting on; hl union ---------

/// `group.facet=true` and `group.truncate=true` do not themselves turn
/// faceting on: with `facet=false` (the default), there is no `facet_counts`
/// key at all. No g338 fixture captures this combination (Solr was never
/// asked for `facet.field` here) -- this is the spec's explicit "the flags do
/// not turn faceting on" requirement, checked structurally against the
/// response's own top-level keys.
#[tokio::test]
async fn grouping_facet_and_truncate_flags_do_not_themselves_enable_faceting() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&group=true&group.field=type&group.ngroups=true&group.facet=true&group.truncate=true&fl=id&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert!(
        body.get("facet_counts").is_none(),
        "group.facet/group.truncate must not enable facet_counts on their \
         own -- facet=true was never sent, got {body}"
    );
    // Sanity: the grouped envelope still renders normally.
    assert_eq!(
        body.pointer("/grouped/type/matches")
            .and_then(Value::as_u64),
        Some(6),
        "got {body}"
    );
}

/// `hl=true` with `group.limit=2`: when a group's doclist renders two docs,
/// `highlighting` covers both. No g338 fixture captures a group with two
/// query-matching docs and `hl=true` together, so this checks the structural
/// invariant the spec names directly: `highlighting`'s key set is exactly the
/// union of every doc id actually rendered across all doclists. `q=lazy quick`
/// (OR) matches g1/g3 (article) and g2 (page); with `group.limit=2` the
/// article doclist renders both g1 and g3.
#[tokio::test]
async fn grouping_hl_with_group_limit_two_covers_both_docs_of_a_group() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=lazy+quick&df=body&group=true&group.field=type&group.ngroups=true&group.limit=2&hl=true&hl.fl=body&fl=id&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let article_docs = groups_of(&body, "type")
        .into_iter()
        .find(|(gv, _, _)| gv == &json!("article"))
        .map(|(_, docs, _)| docs)
        .unwrap_or_default();
    assert_eq!(
        article_docs.len(),
        2,
        "the article group must render both matching docs (g1, g3) under \
         group.limit=2 for this test to be meaningful, got {body}"
    );
    let mut rendered_ids: Vec<String> = groups_of(&body, "type")
        .into_iter()
        .flat_map(|(_, docs, _)| docs)
        .collect();
    rendered_ids.sort();
    let mut hl_keys: Vec<String> = body
        .pointer("/highlighting")
        .and_then(Value::as_object)
        .expect("highlighting must be an object")
        .keys()
        .cloned()
        .collect();
    hl_keys.sort();
    assert_eq!(
        hl_keys, rendered_ids,
        "highlighting's key set must be exactly the union of every rendered \
         doclist's docs, including both docs of the two-doc article group, \
         got {body}"
    );
}

/// Multiple `group.field` blocks with `hl=true`: `highlighting` is the union
/// of every rendered doclist across ALL `group.field` blocks, not just the
/// first. No g338 fixture captures multiple `group.field` values with
/// `hl=true` together, so this checks the same structural invariant as above
/// across two fields. `q=lazy quick` matches g1/g2/g3; `group.field=type`
/// (default group.limit=1) renders one doc per group (2 of the 3 matches);
/// `group.field=popularity` gives every matched doc its own singleton group,
/// so it renders all 3 -- the union across both fields must be all 3, a
/// strict superset of the `type` field alone.
#[tokio::test]
async fn grouping_hl_unions_doclists_across_multiple_group_fields() {
    let (app, _dir) = grouping_app().await;
    let (status, body) = get(
        &app,
        "select?q=lazy+quick&df=body&group=true&group.field=type&group.field=popularity&group.ngroups=true&hl=true&hl.fl=body&fl=id&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let type_ids: Vec<String> = groups_of(&body, "type")
        .into_iter()
        .flat_map(|(_, docs, _)| docs)
        .collect();
    assert!(
        type_ids.len() < 3,
        "the type field alone (default group.limit=1) must render fewer than \
         all 3 matches for this to be a meaningful union check, got {body}"
    );
    let mut rendered_ids: Vec<String> = type_ids;
    rendered_ids.extend(
        groups_of(&body, "popularity")
            .into_iter()
            .flat_map(|(_, docs, _)| docs),
    );
    rendered_ids.sort();
    rendered_ids.dedup();
    assert_eq!(
        rendered_ids,
        vec!["g1".to_string(), "g2".to_string(), "g3".to_string()],
        "the union across both group.field blocks must be all 3 matched docs, \
         got {body}"
    );
    let mut hl_keys: Vec<String> = body
        .pointer("/highlighting")
        .and_then(Value::as_object)
        .expect("highlighting must be an object")
        .keys()
        .cloned()
        .collect();
    hl_keys.sort();
    assert_eq!(
        hl_keys, rendered_ids,
        "highlighting must be the union of doclists across every group.field \
         block, not just the first, got {body}"
    );
}
