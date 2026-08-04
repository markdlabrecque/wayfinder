//! `f.<field>.facet.{limit,mincount,sort}` and the local-param form of
//! `facet.limit`/`facet.mincount`/`facet.missing`/`facet.sort` on
//! `facet.field` (issue #296).
//!
//! `f.<field>.facet.missing` already works (issue #140) -- see
//! `tests/facet_field_missing_override.rs`, the worked example this file
//! follows. #296 as filed asked only for the three remaining per-field names.
//! Findings 147-151 (`docs/solr-ref-findings.md`) settle that this is only
//! half the feature:
//!
//! - finding 147: `f.<X>.facet.*` always resolves `X` against the field being
//!   faceted, never a `{!key=}` response label.
//! - finding 148: every one of `facet.limit`/`facet.mincount`/`facet.missing`/
//!   `facet.sort` can also be carried as a local param on `facet.field`
//!   (`{!key=cat facet.limit=1}category`), with or without a `key`.
//! - finding 149: the local-param form is the *only* way to give two facets
//!   on one field different settings -- the shape #299's delta-keyed facets
//!   need, and the reason #296 cannot be built from `f.<field>.facet.*` alone.
//! - finding 150: `facet.limit` is applied after `{!ex=...}` exclusion, like
//!   `facet.mincount`/`facet.missing` already are (finding 140).
//! - finding 151: precedence is `f.<field>.facet.X` > local param on
//!   `facet.field` > global `facet.X` -- the per-field param beats the local
//!   one, the opposite of "local params shadow the request".
//!
//! Fixtures: `solr-ref/responses/facet_perfield_*.json` (23 rows,
//! `manifest.tsv`, `content` core/corpus -- same schema as
//! `tests/common::indexed_app`) and `solr-ref/responses/pf296_sort_*.json` (8
//! rows, `manifest-errors.tsv`, dedicated `pf296` core/corpus). The `content`
//! corpus's `category` counts (animals 2, classic 2, garden 1, misc 1) tie
//! count order and index order together, so it cannot pin `facet.sort` on its
//! own -- the `pf296` corpus (`topic`: zebra 3, mango 2, apple 1) exists
//! specifically to break that tie; its schema/corpus are duplicated here from
//! `tests/differential.rs`'s `PF296_SCHEMA_TOML`/`pf296_corpus`, same
//! duplication precedent as `sortdebt`/`facets33` in that file.

// The `dead_code` allow for partially-used shared helpers is an inner attribute
// inside `tests/common/mod.rs`; repeating it here is a clippy error under
// `-D warnings`.
mod common;

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;

use common::{assert_matches_fixture, corpus, fixture, get, indexed_app, post_docs};

/// `facet_counts.facet_fields.<label>` as the flat alternating array Solr
/// uses, or `None` when the label is absent entirely.
fn facet_bucket(body: &Value, label: &str) -> Option<Vec<Value>> {
    body.pointer(&format!("/facet_counts/facet_fields/{label}"))
        .map(|v| {
            v.as_array()
                .unwrap_or_else(|| {
                    panic!("facet_counts.facet_fields.{label} must be a flat array, got: {body}")
                })
                .clone()
        })
}

/// The flat counts array a fixture recorded under `label`.
fn fixture_bucket(fixture_name: &str, label: &str) -> Vec<Value> {
    facet_bucket(&fixture(fixture_name), label).unwrap_or_else(|| {
        panic!("fixture {fixture_name} has no facet_counts.facet_fields.{label}")
    })
}

/// A flat `term, count, term, count` array as an order-independent set of
/// `(term, count)` pairs, for the assertions that are about *which* buckets
/// survive rather than what order they come back in.
fn bucket_set(flat: &[Value]) -> Vec<(String, i64)> {
    let mut pairs: Vec<(String, i64)> = flat
        .chunks(2)
        .map(|pair| {
            (
                pair[0].as_str().unwrap_or("<null>").to_string(),
                pair[1].as_i64().expect("a bucket count must be an integer"),
            )
        })
        .collect();
    pairs.sort();
    pairs
}

/// An app on the tracer-bullet schema/corpus but with an arbitrary server
/// config, for the `strict_params` guards below. `common::indexed_app`
/// always uses `ServerConfig` defaults.
async fn indexed_app_with_config(config_toml: &str) -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, common::SCHEMA_TOML).expect("write schema.toml");
    let config_path = dir.path().join("wayfinder.toml");
    std::fs::write(&config_path, config_toml).expect("write wayfinder.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let app =
        wayfinder::app_with_config(&schema_path, &data_dir, &config_path).expect("app must build");
    let (status, body) = post_docs(&app, &corpus()).await;
    assert_eq!(status, StatusCode::OK, "indexing must succeed, got {body}");
    (app, dir)
}

// --- pf296 corpus: the only one on this branch that can pin `facet.sort` ---
// (finding: `content`'s `category` counts tie count order and index order
// together). Duplicated byte-for-byte from `tests/differential.rs`'s
// `PF296_SCHEMA_TOML`/`pf296_corpus` -- same schema `capture.sh`'s pf296
// block indexed into the live `solr:9` container the fixtures came from.

const PF296_SCHEMA_TOML: &str = r#"
[core]
name = "content"
unique_key = "id"
default_field = "topic"

[[fields]]
name = "id"
type = "string"
stored = true
required = true
fast = true

[[fields]]
name = "topic"
type = "string"
stored = true
fast = true
multi_valued = true
"#;

fn pf296_corpus() -> Value {
    json!([
        {"id":"s1","topic":["zebra"]},
        {"id":"s2","topic":["zebra","mango"]},
        {"id":"s3","topic":["zebra","mango","apple"]},
        {"id":"s4"}
    ])
}

async fn pf296_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), PF296_SCHEMA_TOML).expect("pf296 app must build");
    let (status, body) = post_docs(&app, &pf296_corpus()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the pf296 corpus must succeed, got {body}"
    );
    (app, dir)
}

// === 1. `f.<field>.facet.limit` ==============================================

/// `f.category.facet.limit=1` limits only `category`; `id` (no override) gets
/// its full 5-bucket list. Matches `facet_perfield_limit.json`.
#[tokio::test]
async fn per_field_limit_overrides_only_the_named_field() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&facet.field=id\
         &f.category.facet.limit=1&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "category").as_deref(),
        Some(fixture_bucket("facet_perfield_limit", "category").as_slice()),
        "f.category.facet.limit=1 must limit category to its top bucket; got {body}"
    );
    assert_eq!(
        facet_bucket(&body, "id").as_deref(),
        Some(fixture_bucket("facet_perfield_limit", "id").as_slice()),
        "id has no override, so its full bucket list must be untouched; got {body}"
    );
    assert_matches_fixture(body, "facet_perfield_limit");
}

/// `f.category.facet.limit=-1` (unlimited) beats a global `facet.limit=1`,
/// while `id` (no override) still honours the global. Matches
/// `facet_perfield_overrides_global.json`.
#[tokio::test]
async fn per_field_limit_beats_a_conflicting_global() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&facet.field=id\
         &facet.limit=1&f.category.facet.limit=-1&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "category").as_deref(),
        Some(fixture_bucket("facet_perfield_overrides_global", "category").as_slice()),
        "f.category.facet.limit=-1 must beat the global facet.limit=1 for category; got {body}"
    );
    assert_eq!(
        facet_bucket(&body, "id").as_deref(),
        Some(fixture_bucket("facet_perfield_overrides_global", "id").as_slice()),
        "id has no override, so the global facet.limit=1 must still apply to it; got {body}"
    );
    assert_matches_fixture(body, "facet_perfield_overrides_global");
}

/// `f.nosuchfield.facet.limit=1` names a field never passed to `facet.field`
/// -- no error, no effect on `category`. Matches
/// `facet_perfield_unknown_field.json`.
#[tokio::test]
async fn per_field_limit_naming_an_unrequested_field_has_no_effect() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category\
         &f.nosuchfield.facet.limit=1&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "category").as_deref(),
        Some(fixture_bucket("facet_perfield_unknown_field", "category").as_slice()),
        "an override naming an unrequested field must not perturb category; got {body}"
    );
    assert_matches_fixture(body, "facet_perfield_unknown_field");
}

/// Finding 147: with `{!key=cat}category`, `f.category.facet.limit` (the
/// *field* name) is honoured -- the bucket is limited under the label `cat`.
/// Matches `facet_perfield_key_by_field.json`.
#[tokio::test]
async fn per_field_limit_keys_off_the_field_not_the_local_label() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true\
         &facet.field=%7B%21key%3Dcat%7Dcategory&f.category.facet.limit=1&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "cat").as_deref(),
        Some(fixture_bucket("facet_perfield_key_by_field", "cat").as_slice()),
        "f.category.facet.limit must apply under the cat label, since category is the field \
         actually being faceted; got {body}"
    );
    assert_matches_fixture(body, "facet_perfield_key_by_field");
}

/// The mirror case: `f.cat.facet.limit=1` names the local-params *key*, not a
/// real field, so it has no effect -- the full 4-bucket list comes back under
/// `cat`. Matches `facet_perfield_key_by_key.json`.
#[tokio::test]
async fn per_field_limit_naming_the_local_label_has_no_effect() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true\
         &facet.field=%7B%21key%3Dcat%7Dcategory&f.cat.facet.limit=1&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "cat").as_deref(),
        Some(fixture_bucket("facet_perfield_key_by_key", "cat").as_slice()),
        "f.cat.facet.limit must have no effect -- cat is a response label, not a field; \
         got {body}"
    );
    assert_matches_fixture(body, "facet_perfield_key_by_key");
}

// === 2. `f.<field>.facet.mincount` ===========================================

/// `f.category.facet.mincount=2` drops garden/misc from `category` only;
/// `id` (no override, all counts are 1) is untouched by the global default of
/// 0. Matches `facet_perfield_mincount.json`.
#[tokio::test]
async fn per_field_mincount_overrides_only_the_named_field() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&facet.field=id\
         &f.category.facet.mincount=2&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "category").as_deref(),
        Some(fixture_bucket("facet_perfield_mincount", "category").as_slice()),
        "f.category.facet.mincount=2 must drop garden/misc from category; got {body}"
    );
    assert_eq!(
        facet_bucket(&body, "id").as_deref(),
        Some(fixture_bucket("facet_perfield_mincount", "id").as_slice()),
        "id has no override, so its full bucket list must be untouched; got {body}"
    );
    assert_matches_fixture(body, "facet_perfield_mincount");
}

// === 3. `f.<field>.facet.sort` (pf296 corpus -- see module doc) ==============

/// `f.topic.facet.sort=index` reorders `topic`'s buckets to apple, mango,
/// zebra -- the reverse of the default count order. Matches
/// `pf296_sort_field.json`.
#[tokio::test]
async fn per_field_sort_reorders_only_the_named_field() {
    let (app, _dir) = pf296_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=topic&f.topic.facet.sort=index&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "topic").as_deref(),
        Some(fixture_bucket("pf296_sort_field", "topic").as_slice()),
        "f.topic.facet.sort=index must reorder topic's buckets to apple, mango, zebra; \
         got {body}"
    );
    assert_matches_fixture(body, "pf296_sort_field");
}

/// A per-field `facet.sort=index` beats a conflicting global
/// `facet.sort=count`, with `facet.limit=1` telling the two orders apart:
/// `apple` (index order) rather than `zebra` (count order). Matches
/// `pf296_sort_field_wins.json`.
#[tokio::test]
async fn per_field_sort_beats_a_conflicting_global_sort() {
    let (app, _dir) = pf296_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=topic\
         &facet.sort=count&f.topic.facet.sort=index&facet.limit=1&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "topic").as_deref(),
        Some(fixture_bucket("pf296_sort_field_wins", "topic").as_slice()),
        "the per-field facet.sort=index must beat the global facet.sort=count; got {body}"
    );
    assert_matches_fixture(body, "pf296_sort_field_wins");
}

/// Finding 147 for `facet.sort`: with `{!key=k}topic`, `f.topic.facet.sort`
/// (the field name) is honoured. Matches `pf296_sort_key_by_field.json`.
#[tokio::test]
async fn per_field_sort_keys_off_the_field_not_the_local_label() {
    let (app, _dir) = pf296_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true\
         &facet.field=%7B%21key%3Dk%7Dtopic&f.topic.facet.sort=index&facet.limit=1&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "k").as_deref(),
        Some(fixture_bucket("pf296_sort_key_by_field", "k").as_slice()),
        "f.topic.facet.sort=index must apply under the k label; got {body}"
    );
    assert_matches_fixture(body, "pf296_sort_key_by_field");
}

/// The mirror case: `f.k.facet.sort=index` names the local-params key, not a
/// real field, so it has no effect and count order (`zebra` first) survives.
/// Matches `pf296_sort_key_by_key.json`.
#[tokio::test]
async fn per_field_sort_naming_the_local_label_has_no_effect() {
    let (app, _dir) = pf296_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true\
         &facet.field=%7B%21key%3Dk%7Dtopic&f.k.facet.sort=index&facet.limit=1&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "k").as_deref(),
        Some(fixture_bucket("pf296_sort_key_by_key", "k").as_slice()),
        "f.k.facet.sort must have no effect -- k is a response label, not a field; got {body}"
    );
    assert_matches_fixture(body, "pf296_sort_key_by_key");
}

// === 4. facet settings as local params on `facet.field` (finding 148) =======

/// `{!key=cat facet.limit=1}category` limits that facet via the local param
/// alone, with no `f.<field>.facet.limit` in sight. Matches
/// `facet_perfield_lp_limit.json`.
#[tokio::test]
async fn local_param_facet_limit() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true\
         &facet.field=%7B%21key%3Dcat%20facet.limit%3D1%7Dcategory&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "cat").as_deref(),
        Some(fixture_bucket("facet_perfield_lp_limit", "cat").as_slice()),
        "facet.limit=1 as a local param must limit this facet; got {body}"
    );
    assert_matches_fixture(body, "facet_perfield_lp_limit");
}

/// `{!key=cat facet.mincount=2}category` drops garden/misc via the local
/// param alone. Matches `facet_perfield_lp_mincount.json`.
#[tokio::test]
async fn local_param_facet_mincount() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true\
         &facet.field=%7B%21key%3Dcat%20facet.mincount%3D2%7Dcategory&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "cat").as_deref(),
        Some(fixture_bucket("facet_perfield_lp_mincount", "cat").as_slice()),
        "facet.mincount=2 as a local param must drop garden/misc from this facet; got {body}"
    );
    assert_matches_fixture(body, "facet_perfield_lp_mincount");
}

/// `{!key=cat facet.missing=true}category` adds the null bucket via the local
/// param alone -- the `f.<field>.facet.missing` form of this setting already
/// works (issue #140); the local-param form is new. Matches
/// `facet_perfield_lp_missing.json`.
#[tokio::test]
async fn local_param_facet_missing() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true\
         &facet.field=%7B%21key%3Dcat%20facet.missing%3Dtrue%7Dcategory&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "cat").as_deref(),
        Some(fixture_bucket("facet_perfield_lp_missing", "cat").as_slice()),
        "facet.missing=true as a local param must add the null bucket; got {body}"
    );
    assert_matches_fixture(body, "facet_perfield_lp_missing");
}

/// `{!key=k facet.sort=index}topic` reorders via the local param alone, on
/// the pf296 corpus where sort order is observable. Matches
/// `pf296_sort_lp.json`.
#[tokio::test]
async fn local_param_facet_sort() {
    let (app, _dir) = pf296_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true\
         &facet.field=%7B%21key%3Dk%20facet.sort%3Dindex%7Dtopic&facet.limit=1&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "k").as_deref(),
        Some(fixture_bucket("pf296_sort_lp", "k").as_slice()),
        "facet.sort=index as a local param must reorder this facet; got {body}"
    );
    assert_matches_fixture(body, "pf296_sort_lp");
}

/// `{!facet.limit=1}category` -- no `key` at all -- still sets the limit, and
/// the facet keeps its field name as its label. Matches
/// `facet_perfield_lp_no_key.json`.
#[tokio::test]
async fn local_param_with_no_key_still_sets_the_limit() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=%7B%21facet.limit%3D1%7Dcategory&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "category").as_deref(),
        Some(fixture_bucket("facet_perfield_lp_no_key", "category").as_slice()),
        "a keyless local param must still set the limit, under the field name as the label; \
         got {body}"
    );
    assert_matches_fixture(body, "facet_perfield_lp_no_key");
}

// === 5. finding 149: two facets on one field, only local params tell them apart ==

/// `{!key=a facet.limit=1}category` and `{!key=b facet.limit=3}category` --
/// two facets on the SAME field, each with its own limit. Matches
/// `facet_perfield_two_lp.json`. This is the shape #299's delta-keyed facets
/// produce, and the reason #296 cannot be built from `f.<field>.facet.*`
/// alone -- see the mirror case right below.
#[tokio::test]
async fn two_facets_on_one_field_local_params_give_each_its_own_limit() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true\
         &facet.field=%7B%21key%3Da%20facet.limit%3D1%7Dcategory\
         &facet.field=%7B%21key%3Db%20facet.limit%3D3%7Dcategory&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "a").as_deref(),
        Some(fixture_bucket("facet_perfield_two_lp", "a").as_slice()),
        "facet a must be limited to 1; got {body}"
    );
    assert_eq!(
        facet_bucket(&body, "b").as_deref(),
        Some(fixture_bucket("facet_perfield_two_lp", "b").as_slice()),
        "facet b must be limited to 3; got {body}"
    );
    assert_matches_fixture(body, "facet_perfield_two_lp");
}

/// The mirror case: `{!key=a}category`/`{!key=b}category` with
/// `f.a.facet.limit=1`/`f.b.facet.limit=3` -- both name the local-params key,
/// so both are silently ignored and both facets get the full 4-bucket list.
/// The per-field form cannot express "two facets on one field, different
/// settings" (finding 149). Matches `facet_perfield_two_by_key.json`.
#[tokio::test]
async fn two_facets_on_one_field_per_field_key_form_cannot_differ() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true\
         &facet.field=%7B%21key%3Da%7Dcategory&facet.field=%7B%21key%3Db%7Dcategory\
         &f.a.facet.limit=1&f.b.facet.limit=3&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "a").as_deref(),
        Some(fixture_bucket("facet_perfield_two_by_key", "a").as_slice()),
        "f.a.facet.limit names a response label, not a field, so a must be unlimited; got {body}"
    );
    assert_eq!(
        facet_bucket(&body, "b").as_deref(),
        Some(fixture_bucket("facet_perfield_two_by_key", "b").as_slice()),
        "f.b.facet.limit names a response label, not a field, so b must be unlimited; got {body}"
    );
    assert_matches_fixture(body, "facet_perfield_two_by_key");
}

// === 6. finding 150: `facet.limit` applies after `{!ex=...}` exclusion ======

/// `f.category.facet.limit=1` against an `{!ex=cat}` facet -- the limit
/// applies to the post-exclusion (full, unfiltered) bucket list. Matches
/// `facet_perfield_ex_limit.json`.
#[tokio::test]
async fn per_field_limit_applies_after_exclusion() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals&facet=true\
         &facet.field=%7B%21ex%3Dcat%20key%3Dun%7Dcategory&f.category.facet.limit=1&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "un").as_deref(),
        Some(fixture_bucket("facet_perfield_ex_limit", "un").as_slice()),
        "the limit must apply to the excluded (unfiltered) bucket list; got {body}"
    );
    assert_matches_fixture(body, "facet_perfield_ex_limit");
}

/// The local-param form of the row above: `{!ex=cat key=un
/// facet.limit=1}category`. Matches `facet_perfield_ex_lp_limit.json`.
#[tokio::test]
async fn local_param_limit_applies_after_exclusion() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals&facet=true\
         &facet.field=%7B%21ex%3Dcat%20key%3Dun%20facet.limit%3D1%7Dcategory&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "un").as_deref(),
        Some(fixture_bucket("facet_perfield_ex_lp_limit", "un").as_slice()),
        "the local-param limit must apply to the excluded (unfiltered) bucket list; got {body}"
    );
    assert_matches_fixture(body, "facet_perfield_ex_lp_limit");
}

/// The full `search_api_solr` OR-facet shape: a plain filtered facet next to
/// an excluded one, with a limit on the excluded facet only (as a local
/// param). Matches `facet_perfield_ex_two_facets.json`.
#[tokio::test]
async fn exclusion_and_limit_together_only_affect_the_facet_that_asked() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals&facet=true\
         &facet.field=%7B%21key%3Dfiltered%7Dcategory\
         &facet.field=%7B%21ex%3Dcat%20key%3Dun%20facet.limit%3D1%7Dcategory&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "filtered").as_deref(),
        Some(fixture_bucket("facet_perfield_ex_two_facets", "filtered").as_slice()),
        "the plain facet must stay filtered by the fq and unlimited; got {body}"
    );
    assert_eq!(
        facet_bucket(&body, "un").as_deref(),
        Some(fixture_bucket("facet_perfield_ex_two_facets", "un").as_slice()),
        "the excluded facet must be unfiltered and limited to 1; got {body}"
    );
    assert_matches_fixture(body, "facet_perfield_ex_two_facets");
}

/// The decisive ordering row (finding 150): with `fq=category:garden`,
/// ranking the *filtered* counts would put `garden` first; ranking the
/// *excluded* (full) counts puts `animals` first. Solr returns `animals`, so
/// `facet.limit` truncates the post-exclusion ranking, not a pre-exclusion
/// one. Matches `facet_perfield_ex_limit_rank.json`.
#[tokio::test]
async fn per_field_limit_ranks_the_excluded_list_not_the_filtered_one() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:garden&facet=true\
         &facet.field=%7B%21ex%3Dcat%20key%3Dun%7Dcategory&f.category.facet.limit=1&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "un").as_deref(),
        Some(fixture_bucket("facet_perfield_ex_limit_rank", "un").as_slice()),
        "the top bucket must be animals (2), from the excluded/unfiltered ranking, not garden \
         (the top of the filtered ranking); got {body}"
    );
    assert_matches_fixture(body, "facet_perfield_ex_limit_rank");
}

/// The local-param form of the row above. Matches
/// `facet_perfield_ex_lp_limit_rank.json`.
#[tokio::test]
async fn local_param_limit_ranks_the_excluded_list_not_the_filtered_one() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:garden&facet=true\
         &facet.field=%7B%21ex%3Dcat%20key%3Dun%20facet.limit%3D1%7Dcategory&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "un").as_deref(),
        Some(fixture_bucket("facet_perfield_ex_lp_limit_rank", "un").as_slice()),
        "the top bucket must be animals (2), from the excluded/unfiltered ranking; got {body}"
    );
    assert_matches_fixture(body, "facet_perfield_ex_lp_limit_rank");
}

// === 7. finding 151: `f.<field>.facet.X` > local param > global =============

/// `{!key=cat facet.limit=1}category` with `f.category.facet.limit=3` --
/// the per-field param beats the local one, even though "local params shadow
/// the request" would suggest the opposite. Matches
/// `facet_perfield_prec_lp_vs_field.json`.
#[tokio::test]
async fn precedence_per_field_beats_a_conflicting_local_param() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true\
         &facet.field=%7B%21key%3Dcat%20facet.limit%3D1%7Dcategory\
         &f.category.facet.limit=3&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "cat").as_deref(),
        Some(fixture_bucket("facet_perfield_prec_lp_vs_field", "cat").as_slice()),
        "f.category.facet.limit=3 must win over the local param's facet.limit=1; got {body}"
    );
    assert_matches_fixture(body, "facet_perfield_prec_lp_vs_field");
}

/// The same local param against a conflicting *global* `facet.limit=3` -- the
/// local param wins here (it only ever shadows the bare global). Matches
/// `facet_perfield_prec_lp_vs_global.json`.
#[tokio::test]
async fn precedence_local_param_beats_a_conflicting_global() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true\
         &facet.field=%7B%21key%3Dcat%20facet.limit%3D1%7Dcategory&facet.limit=3&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "cat").as_deref(),
        Some(fixture_bucket("facet_perfield_prec_lp_vs_global", "cat").as_slice()),
        "the local param's facet.limit=1 must win over the global facet.limit=3; got {body}"
    );
    assert_matches_fixture(body, "facet_perfield_prec_lp_vs_global");
}

/// `f.category.facet.limit=1` beats a conflicting global `facet.limit=3` --
/// the ordinary, least surprising direction. Matches
/// `facet_perfield_prec_field_vs_global.json`.
#[tokio::test]
async fn precedence_per_field_beats_a_conflicting_global() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category\
         &f.category.facet.limit=1&facet.limit=3&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "category").as_deref(),
        Some(fixture_bucket("facet_perfield_prec_field_vs_global", "category").as_slice()),
        "f.category.facet.limit=1 must win over the global facet.limit=3; got {body}"
    );
    assert_matches_fixture(body, "facet_perfield_prec_field_vs_global");
}

// === 8. a non-numeric per-field limit is a 400 ==============================

/// `f.category.facet.limit=abc` must 400 -- Solr's own behaviour
/// (`facet_perfield_err_bad_limit.json`), and it pins that the validation
/// lands with the feature rather than being silently swallowed the way the
/// bare global `facet.limit` currently is (`FacetSettings::resolve`
/// defaults a non-numeric global to `DEFAULT_FACET_LIMIT` rather than
/// erroring). The response block must still carry the base query's
/// `numFound` (5), same convention as every other facet error
/// (`facet_field_error_still_carries_the_base_querys_response_block`,
/// `tests/faceting.rs`). Message wording is not pinned byte-for-byte against
/// Solr's `NumberFormatException` text -- only that the bad value is named.
#[tokio::test]
async fn non_numeric_per_field_limit_400s() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&f.category.facet.limit=abc&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a non-numeric f.category.facet.limit must 400, got {body}"
    );
    assert!(
        body.get("error").is_some(),
        "the error block must be present, got {body}"
    );
    assert_eq!(
        body.pointer("/response/numFound").and_then(Value::as_i64),
        Some(5),
        "the base query's response block must still be present alongside the error, got {body}"
    );
    let msg = body
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        msg.contains("abc"),
        "error.msg must name the bad value, got: {msg}"
    );
}

// === 9. `PER_FIELD_PARAMS` gains three names alongside the feature ==========

/// `strict_params = true` must accept `f.<field>.facet.limit`,
/// `.mincount` and `.sort` for any field -- same pattern-matching guard as
/// `strict_params_accepts_the_per_field_missing_override_for_any_field`
/// (`tests/facet_field_missing_override.rs`), extended to the three names
/// this issue adds to `PER_FIELD_PARAMS`.
#[tokio::test]
async fn strict_params_accepts_the_three_new_per_field_overrides() {
    let (app, _dir) = indexed_app_with_config("strict_params = true\n").await;

    for (param, value) in [
        ("f.category.facet.limit", "1"),
        ("f.category.facet.mincount", "2"),
        ("f.category.facet.sort", "index"),
    ] {
        let (status, body) = get(
            &app,
            &format!("select?q=*:*&rows=0&facet=true&facet.field=category&{param}={value}&wt=json"),
        )
        .await;
        let msg = body
            .pointer("/error/msg")
            .and_then(Value::as_str)
            .unwrap_or("");
        assert!(
            !msg.contains("unknown request parameter"),
            "{param} must not 400 as an unknown param under strict_params, got: {msg}"
        );
        assert_eq!(
            status,
            StatusCode::OK,
            "{param} must pass strict mode, got {body}"
        );
    }
}

// === 10. an invalid boolean local param is a 400 ============================

/// `{!facet.missing=nope}category` must 400 with the same message the bare
/// global `facet.missing=nope` produces -- the local-param form goes through
/// the same `parse_bool`, so it must fail the same way rather than being
/// quietly read as `false`. Mirrors
/// `facet_missing_nope_is_invalid_and_the_response_block_survives`
/// (`tests/bool_params.rs`, `bool_facet_missing_invalid.json`), including
/// the `response`-block-carrying envelope: this is read after the base query
/// has already run. No fixture of its own -- Solr's wording for the global
/// case is the ground truth, and this asserts the addressed form does not
/// diverge from Wayfinder's own global behaviour.
#[tokio::test]
async fn local_param_invalid_boolean_400s_like_the_global() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true\
         &facet.field=%7B%21facet.missing%3Dnope%7Dcategory&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an invalid boolean local param must 400, got {body}"
    );
    assert!(
        body.get("response").is_some(),
        "the error envelope must still carry the base query's response block; got {body}"
    );
    assert_eq!(
        body.pointer("/error/msg").and_then(Value::as_str),
        Some("invalid boolean value: nope"),
        "error.msg must name the invalid raw value verbatim, same wording as the global \
         facet.missing=nope; got {body}"
    );
}

// === 11. a negative mincount matches the global path, it does not 400 =======

/// `facet.mincount=-1` on the bare global is accepted today (the parse fails
/// and falls back to the default 0), so neither addressed form may 400 on it.
/// Solr's `getFieldInt` reads mincount as a signed int and never rejects a
/// negative one; a `u64` parse in the addressed path would have made
/// `f.category.facet.mincount=-1` a 400 while the global on the same server
/// stayed a 200. Unfixtured in either direction -- what is asserted is that
/// all three forms agree with each other, and that a negative mincount
/// admits every bucket the way 0 does.
#[tokio::test]
async fn a_negative_mincount_behaves_like_zero_in_every_form() {
    let (app, _dir) = indexed_app().await;
    let baseline = {
        let (status, body) = get(
            &app,
            "select?q=*:*&rows=0&facet=true&facet.field=category&wt=json",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "got {body}");
        facet_bucket(&body, "category").expect("the baseline facet must be present")
    };

    for (label, query) in [
        (
            "global",
            "select?q=*:*&rows=0&facet=true&facet.field=category&facet.mincount=-1&wt=json",
        ),
        (
            "per-field",
            "select?q=*:*&rows=0&facet=true&facet.field=category\
             &f.category.facet.mincount=-1&wt=json",
        ),
        (
            "local param",
            "select?q=*:*&rows=0&facet=true\
             &facet.field=%7B%21facet.mincount%3D-1%7Dcategory&wt=json",
        ),
    ] {
        let (status, body) = get(&app, query).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "a negative facet.mincount in the {label} form must not 400; got {body}"
        );
        assert_eq!(
            facet_bucket(&body, "category").as_deref(),
            Some(baseline.as_slice()),
            "a negative facet.mincount in the {label} form must admit every bucket, \
             exactly as mincount=0 does; got {body}"
        );
    }
}

/// The companion for `facet.limit`: `-1` is meaningful in Solr (as many as
/// the server allows), the global path already reads it as such, and both
/// addressed forms must agree rather than 400. The per-field form is pinned
/// against a fixture in `per_field_limit_beats_a_conflicting_global` above;
/// this covers the local-param form and the global on the same run.
///
/// Bucket *sets* are compared, not the arrays in order: `facet.sort`'s
/// default flips to `index` when the effective limit is non-positive
/// (`BucketShaping::for_field`), so a negative limit legitimately reorders
/// where the unlimited baseline does not. This test is about which buckets
/// survive; order is section 3's business.
#[tokio::test]
async fn a_negative_limit_is_unlimited_in_every_form() {
    let (app, _dir) = indexed_app().await;
    let baseline = {
        let (status, body) = get(
            &app,
            "select?q=*:*&rows=0&facet=true&facet.field=category&wt=json",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "got {body}");
        bucket_set(&facet_bucket(&body, "category").expect("the baseline facet must be present"))
    };

    for (label, query) in [
        (
            "global",
            "select?q=*:*&rows=0&facet=true&facet.field=category&facet.limit=-1&wt=json",
        ),
        (
            "per-field",
            "select?q=*:*&rows=0&facet=true&facet.field=category\
             &f.category.facet.limit=-1&wt=json",
        ),
        (
            "local param",
            "select?q=*:*&rows=0&facet=true\
             &facet.field=%7B%21facet.limit%3D-1%7Dcategory&wt=json",
        ),
    ] {
        let (status, body) = get(&app, query).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "facet.limit=-1 in the {label} form must not 400; got {body}"
        );
        assert_eq!(
            facet_bucket(&body, "category").map(|b| bucket_set(&b)),
            Some(baseline.clone()),
            "facet.limit=-1 in the {label} form must return every bucket; got {body}"
        );
    }
}
