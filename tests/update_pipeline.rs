//! Update-pipeline completion (issue #9): `commitWithin`/`overwrite`/
//! `softCommit`, the command-object body form (`add`/`delete`/`commit`),
//! GET `/update`, single-valued-field enforcement, copy-field enforcement,
//! the dynamic `*_dt` round trip, and autocommit config consumption.
//!
//! Ground truth is `solr-ref/FINDINGS.md` findings 46-49 and the
//! fixtures they cite in `solr-ref/responses/update_*` / `ping_unknown_core*`,
//! captured against a self-contained `update9` core (the dedicated Solr capture's
//! tail block). This file mirrors that core's schema and `u1..u5` seed corpus
//! locally — `tests/common/` is compiled once per integration-test binary and
//! is hardcoded to the tracer-bullet `content` schema/corpus (`CORE`), so a
//! second schema/corpus needs its own copy here, the same precedent
//! `feature tests' SORTDEBT_SCHEMA_TOML` and
//! `tests/sort.rs::SORTDEBT_SCHEMA_TOML` already establish. Request/response
//! helpers below are local for the same reason `tests/error_shapes.rs` gives:
//! `common`'s `get`/`post_docs`/`request` are hardcoded to `common::CORE`
//! (`"content"`), which does not fit a core literally named `update9`.
//!
//! Error-shape comparisons here follow `tests/error_shapes.rs`'s established
//! contract: HTTP status, `error.code`, `responseHeader` (and `params`)
//! presence are asserted; `error.msg` is asserted non-empty but never
//! verbatim. That is doubly true for the single-valued-field and copy-field
//! validation errors, where the task spec says outright that "message may be
//! Wayfinder-honest; only status/code are contract" — those tests must not
//! pin the fixture's exact Java-flavoured message text.
//!
//! Timing tests poll-until-visible with a bounded, generous timeout rather
//! than a bare sleep-then-assert (task spec, "Timing tests MUST be
//! deterministic in outcome"). `commitWithin=60000`/no-commit-param cases use
//! a window far larger than any test could plausibly run past, so the
//! "invisible" assertion is safe to make immediately with no sleep at all.

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;

use common::{assert_matches_fixture, request_full};

/// Mirrors the dedicated Solr capture's `update9` core schema exactly: `id`
/// (string, required, fast, stored, unique key), `body` (text_en, stored),
/// `category` (string, stored, fast, multi-valued), `title`/`nick`/`alias`
/// (string, stored, fast, single-valued), `nick` -> `alias` copy field, and a
/// `*_dt` dynamic date rule (mirroring the `_default` configset's built-in
/// `pdate` dynamic field the capture relies on).
const UPDATE9_SCHEMA_TOML: &str = r#"
[core]
name = "update9"
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
name = "category"
type = "string"
stored = true
fast = true
multi_valued = true

[[fields]]
name = "title"
type = "string"
stored = true
fast = true

[[fields]]
name = "nick"
type = "string"
stored = true
fast = true

[[fields]]
name = "alias"
type = "string"
stored = true
fast = true

[[copy_fields]]
source = "nick"
dest = "alias"

[[dynamic_fields]]
pattern = "*_dt"
type = "date"
stored = true
fast = true
"#;

/// The exact `u1..u5` seed corpus the dedicated Solr capture's reset step reseeds before
/// every run.
fn update9_corpus() -> Value {
    json!([
        {"id":"u1","body":"quick brown fox","category":["keep"]},
        {"id":"u2","body":"lazy dog","category":["temp"]},
        {"id":"u3","body":"lazy afternoon","category":["temp"]},
        {"id":"u4","body":"garden path","category":["keep"]},
        {"id":"u5","body":"nothing much here","category":["temp","keep"]}
    ])
}

/// `POST /wayfinder/update9/<query>` with `body` as the request body.
async fn post9(app: &Router, query: &str, body: &str) -> (StatusCode, Value) {
    request_full(app, "POST", &format!("update9/{query}"), Some(body)).await
}

/// `GET /wayfinder/update9/<query>`.
async fn get9(app: &Router, query: &str) -> (StatusCode, Value) {
    request_full(app, "GET", &format!("update9/{query}"), None).await
}

/// Arbitrary method against `/wayfinder/update9/<query>`.
async fn method9(
    app: &Router,
    method: &str,
    query: &str,
    body: Option<&str>,
) -> (StatusCode, Value) {
    request_full(app, method, &format!("update9/{query}"), body).await
}

/// Builds a fresh `update9`-schema app and seeds `update9_corpus()` via
/// `commit=true`, mirroring the dedicated Solr capture's reset-and-reseed step.
async fn update9_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), UPDATE9_SCHEMA_TOML).expect("app must build");
    let (status, body) = post9(&app, "update?commit=true", &update9_corpus().to_string()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "seeding the update9 corpus must succeed: {body}"
    );
    (app, dir)
}

/// As `update9_app`, but with no seed corpus and a server config TOML — for
/// the autocommit tests, which need an empty index so `numFound` counts only
/// the docs the test itself adds.
fn build_update9_app_with_config(dir: &Path, config_toml: &str) -> anyhow::Result<Router> {
    let schema_path = dir.join("schema.toml");
    std::fs::write(&schema_path, UPDATE9_SCHEMA_TOML).expect("write schema.toml");
    let data_dir = dir.join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let config_path = dir.join("wayfinder.toml");
    std::fs::write(&config_path, config_toml).expect("write wayfinder.toml");
    wayfinder::app_with_config(&schema_path, &data_dir, &config_path)
}

/// Polls `GET /wayfinder/update9/<select_query>` every ~50ms until
/// `response.numFound` is nonzero or `timeout` elapses, then returns the last
/// result either way. Never a bare sleep-then-assert (task spec).
async fn poll_until_visible(
    app: &Router,
    select_query: &str,
    timeout: Duration,
) -> (StatusCode, Value) {
    let start = Instant::now();
    loop {
        let (status, body) = get9(app, select_query).await;
        let found = body
            .pointer("/response/numFound")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if found > 0 || start.elapsed() >= timeout {
            return (status, body);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

// --- error-shape helpers, mirroring tests/error_shapes.rs's contract -------

/// `/update` errors: `responseHeader` present, status mirrors `error.code`,
/// but `params` is never echoed (finding 46/47's `NoParams` envelope).
fn assert_update_error(status: StatusCode, body: &Value, want_code: u16) {
    assert_eq!(status.as_u16(), want_code, "HTTP status, body: {body}");
    let header = body
        .get("responseHeader")
        .unwrap_or_else(|| panic!("/update errors carry responseHeader, got: {body}"));
    assert_eq!(header["status"].as_u64(), Some(want_code as u64));
    assert!(
        header.get("params").is_none(),
        "/update errors never echo params, got: {body}"
    );
    assert_eq!(body["error"]["code"].as_i64(), Some(want_code as i64));
    assert!(
        body["error"]["msg"].as_str().is_some_and(|m| !m.is_empty()),
        "error.msg must be a non-empty string, never compared verbatim, got: {body}"
    );
}

/// `/admin/ping` errors: `responseHeader` present, `params` IS echoed
/// (`WithParams` envelope, unlike `/update`).
fn assert_ping_error(status: StatusCode, body: &Value, want_code: u16) {
    assert_eq!(status.as_u16(), want_code, "HTTP status, body: {body}");
    let header = body
        .get("responseHeader")
        .unwrap_or_else(|| panic!("/admin/ping errors carry responseHeader, got: {body}"));
    assert_eq!(header["status"].as_u64(), Some(want_code as u64));
    assert!(
        header.get("params").is_some(),
        "/admin/ping errors echo params (WithParams envelope), got: {body}"
    );
    assert_eq!(body["error"]["code"].as_i64(), Some(want_code as i64));
    assert!(body["error"]["msg"].as_str().is_some_and(|m| !m.is_empty()));
}

/// Order-insensitive counterpart to `assert_matches_fixture`, scoped to
/// exactly one fixture: `update_select_overwrite_false`. Two live docs share
/// uniqueKey `u7` there (a deliberate `overwrite=false` duplicate), and no
/// query field can tie-break them — orchestrator ruling, issue #9, recorded
/// in `solr-ref/FINDINGS.md`'s finding-46-49 block: even with no
/// background merges, tantivy 0.26.1's `SegmentRegister`
/// (`src/indexer/segment_register.rs`) holds segments in a
/// `std::collections::HashMap<SegmentId, SegmentEntry>`, so segment
/// ordinals — and therefore `AllScoredHits`'s ascending-`DocAddress`
/// tie-break for two docs with an identical relevance score — are
/// per-process-random, not insertion order. Solr's own captured order for
/// this exact pair is equally a Lucene-internals accident, not a
/// Solr-guaranteed wire contract.
///
/// Nothing contractual is hidden by this relaxation: `responseHeader`,
/// `response.numFound`/`start`/`numFoundExact`, and the full field content
/// of both docs are still asserted exactly — only the *sequence* of
/// `response.docs` is compared as a set (both sides sorted identically
/// before the equality check) rather than positionally.
fn assert_matches_fixture_ignoring_doc_order(actual: Value, fixture_name: &str) {
    let mut expected = common::normalize_envelope(common::fixture(fixture_name));
    let mut actual = common::normalize_envelope(actual);
    for v in [&mut expected, &mut actual] {
        if let Some(docs) = v
            .pointer_mut("/response/docs")
            .and_then(Value::as_array_mut)
        {
            docs.sort_by_key(ToString::to_string);
        }
    }
    assert_eq!(
        actual, expected,
        "response for fixture `{fixture_name}` did not match as a doc SET (order-insensitive \
         only for this one fixture, per the orchestrator's SegmentRegister-HashMap ordering \
         ruling)"
    );
}

/// PUT `/update`: no `responseHeader` at all (`Bare` envelope,
/// `err_update_put.json`).
fn assert_bare_error(status: StatusCode, body: &Value, want_code: u16) {
    assert_eq!(status.as_u16(), want_code, "HTTP status, body: {body}");
    assert!(
        body.get("responseHeader").is_none(),
        "PUT /update has no responseHeader at all, got: {body}"
    );
    assert_eq!(body["error"]["code"].as_i64(), Some(want_code as i64));
}

// --- ground truth 1/2/8: envelope shapes + visibility -----------------------

#[tokio::test]
async fn add_without_commit_is_invisible_until_committed() {
    let (app, _dir) = update9_app().await;
    let (status, body) = post9(
        &app,
        "update?wt=json",
        &json!([{"id":"u6","body":"pending doc","category":["pending"]}]).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_matches_fixture(body, "update_add_nocommit");

    let (status, body) = get9(&app, "select?q=id:u6&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "update_select_uncommitted");
}

#[tokio::test]
async fn add_with_commit_true_is_immediately_visible() {
    let (app, _dir) = update9_app().await;
    let (status, body) = post9(
        &app,
        "update?commit=true&wt=json",
        &json!([{"id":"u7","body":"committed doc","category":["keep"]}]).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_matches_fixture(body, "update_add_commit");

    let (status, body) = get9(&app, "select?q=id:u7&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "update_select_committed");
}

#[tokio::test]
async fn delete_by_id_of_a_nonexistent_id_is_still_200() {
    let (app, _dir) = update9_app().await;
    let (status, body) = post9(
        &app,
        "update?commit=true&wt=json",
        r#"{"delete":{"id":"nosuch"}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_matches_fixture(body, "update_delete_id_missing");
}

// --- ground truth 4a/4b/4c: overwrite + delete-by-id, chained ---------------

/// Chains overwrite-default -> overwrite=false -> delete-by-id against a
/// single `u7`, since the fixtures pin exact successive states of that one
/// document (finding 48 a-c): replace keeps `numFound` at 1 with the new
/// body, `overwrite=false` really duplicates, and deleting by id removes
/// BOTH live docs sharing the key.
#[tokio::test]
async fn overwrite_default_then_overwrite_false_then_delete_by_id_removes_all_duplicates() {
    let (app, _dir) = update9_app().await;

    let (status, body) = post9(
        &app,
        "update?commit=true&wt=json",
        &json!([{"id":"u7","body":"committed doc","category":["keep"]}]).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "setup add u7: {body}");

    // overwrite=true (default): replaces, numFound stays 1.
    let (status, body) = post9(
        &app,
        "update?commit=true&wt=json",
        &json!([{"id":"u7","body":"replaced body","category":["keep"]}]).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_matches_fixture(body, "update_overwrite_default");
    let (status, body) = get9(&app, "select?q=id:u7&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "update_select_overwritten");

    // overwrite=false: a second live doc with the same uniqueKey.
    let (status, body) = post9(
        &app,
        "update?commit=true&overwrite=false&wt=json",
        &json!([{"id":"u7","body":"duplicate body","category":["dup"]}]).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_matches_fixture(body, "update_overwrite_false");
    let (status, body) = get9(&app, "select?q=id:u7&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture_ignoring_doc_order(body, "update_select_overwrite_false");

    // delete-by-id (object form): removes BOTH duplicates.
    let (status, body) = post9(
        &app,
        "update?commit=true&wt=json",
        r#"{"delete":{"id":"u7"}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_matches_fixture(body, "update_delete_id_obj");
    let (status, body) = get9(&app, "select?q=id:u7&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "update_select_after_delete_id");
}

// --- ground truth 2/4d: delete-by-id list, delete-by-query analyzed
// semantics, and the mixed-command body, chained -----------------------------

/// Chains delete-by-id (list form) -> delete-by-query (analyzed text) ->
/// mixed-command body, since `update_select_after_delete_query.json` and
/// `update_select_after_mixed.json` pin exact successive corpus states
/// (finding 46/48d).
///
/// The three corpus-state `select`s below carry `sort=id+asc` (orchestrator
/// ruling on the issue #9 escalation, commit 99b394a re-captured these three
/// fixtures with an explicit sort): an equally-scored `q=*:*` match's tie
/// order across a mutation sequence is Lucene segment/merge history, not a
/// stable wire contract, so the fixtures were re-pinned to a query that
/// removes the tie entirely rather than trying to match it incidentally.
#[tokio::test]
async fn delete_id_list_then_delete_by_query_then_mixed_commands_sequence() {
    let (app, _dir) = update9_app().await;

    // Setup: add u6 so the corpus is u1..u6, matching the state the list
    // delete's fixture was captured against.
    let (status, body) = post9(
        &app,
        "update?commit=true&wt=json",
        &json!([{"id":"u6","body":"pending doc","category":["pending"]}]).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "setup add u6: {body}");

    // Delete-by-id, list form: u1 and u4 go. Corpus is now u2,u3,u5,u6.
    let (status, body) = post9(
        &app,
        "update?commit=true&wt=json",
        r#"{"delete":["u1","u4"]}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_matches_fixture(body, "update_delete_id_list");
    let (status, body) = get9(&app, "select?q=*:*&fl=id&rows=20&sort=id+asc&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "update_select_after_delete_list");

    // Delete-by-query on a TEXT field, analyzed the same way /select is
    // (finding 48d): "lazy" matches u2 "lazy dog" and u3 "lazy afternoon".
    // Corpus is now u5,u6.
    let (status, body) = post9(
        &app,
        "update?commit=true&wt=json",
        r#"{"delete":{"query":"body:lazy"}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_matches_fixture(body, "update_delete_query");
    let (status, body) = get9(&app, "select?q=*:*&fl=id&rows=20&sort=id+asc&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "update_select_after_delete_query");

    // Mixed-command body: add u8, delete u6, commit — all in one request.
    let (status, body) = post9(
        &app,
        "update?wt=json",
        r#"{"add":{"doc":{"id":"u8","body":"mixed add","category":["keep"]}},"delete":{"id":"u6"},"commit":{}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_matches_fixture(body, "update_mixed_commands");
    let (status, body) = get9(&app, "select?q=*:*&fl=id&rows=20&sort=id+asc&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "update_select_after_mixed");
}

// --- ground truth 3: GET /update ---------------------------------------------

#[tokio::test]
async fn get_update_with_no_action_is_400_missing_content_stream() {
    let (app, _dir) = update9_app().await;
    let (status, body) = get9(&app, "update?wt=json").await;
    assert_update_error(status, &body, 400);
}

#[tokio::test]
async fn get_update_with_commit_true_is_200_and_really_commits() {
    let (app, _dir) = update9_app().await;

    // Make a doc pending first (no commit param), confirm it is invisible.
    let (status, body) = post9(
        &app,
        "update?wt=json",
        &json!([{"id":"u6","body":"pending doc","category":["pending"]}]).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "setup add u6: {body}");
    let (status, body) = get9(&app, "select?q=id:u6&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.pointer("/response/numFound"),
        Some(&json!(0)),
        "u6 must be invisible before any commit: {body}"
    );

    // GET /update?commit=true really commits.
    let (status, body) = get9(&app, "update?commit=true&wt=json").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_matches_fixture(body, "update_get_commit");

    let (status, body) = get9(&app, "select?q=id:u6&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.pointer("/response/numFound"),
        Some(&json!(1)),
        "GET /update?commit=true must have made u6 visible: {body}"
    );
}

#[tokio::test]
async fn put_on_update_stays_bare_envelope_400() {
    let (app, _dir) = update9_app().await;
    let (status, body) = method9(&app, "PUT", "update?wt=json", Some("[]")).await;
    assert_bare_error(status, &body, 400);
}

// --- ground truth 6: single-valued field / copy-field enforcement ----------

#[tokio::test]
async fn single_valued_field_given_more_than_one_value_is_400() {
    let (app, _dir) = update9_app().await;
    let (status, body) = post9(
        &app,
        "update?commit=true&wt=json",
        &json!([{"id":"u11","title":["one","two"]}]).to_string(),
    )
    .await;
    // Message text is Wayfinder-honest, not the fixture's Java-flavoured
    // wording (task spec) — only status/code are the contract.
    assert_update_error(status, &body, 400);
}

#[tokio::test]
async fn single_valued_field_given_a_one_element_array_is_unwrapped_and_stored() {
    let (app, _dir) = update9_app().await;
    let (status, body) = post9(
        &app,
        "update?commit=true&wt=json",
        &json!([{"id":"u15","title":["only"]}]).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_matches_fixture(body, "update_single_valued_array_one");

    let (status, body) = get9(&app, "select?q=id:u15&fl=id,title&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "update_select_single_valued_array_one");
}

#[tokio::test]
async fn copy_field_landing_a_second_value_in_a_single_valued_dest_is_400() {
    let (app, _dir) = update9_app().await;
    let (status, body) = post9(
        &app,
        "update?commit=true&wt=json",
        &json!([{"id":"u12","nick":"nn","alias":"aa"}]).to_string(),
    )
    .await;
    // Same message caveat as the single-valued-array case above.
    assert_update_error(status, &body, 400);
}

#[tokio::test]
async fn copy_field_with_one_value_is_stored_and_copied() {
    let (app, _dir) = update9_app().await;
    let (status, body) = post9(
        &app,
        "update?commit=true&wt=json",
        &json!([{"id":"u13","nick":"solo"}]).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_matches_fixture(body, "update_copyfield_single_ok");

    let (status, body) = get9(&app, "select?q=id:u13&fl=id,nick,alias&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "update_select_copyfield_dest");
}

// --- ground truth 7: dynamic *_dt date round trip ---------------------------

#[tokio::test]
async fn dynamic_dt_field_round_trips_and_is_range_queryable() {
    let (app, _dir) = update9_app().await;
    let (status, body) = post9(
        &app,
        "update?commit=true&wt=json",
        &json!([{"id":"u14","when_dt":"2021-06-01T12:30:45Z"}]).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_matches_fixture(body, "update_dynamic_date");

    let (status, body) = get9(
        &app,
        "select?q=when_dt:%5B2021-01-01T00:00:00Z%20TO%202022-01-01T00:00:00Z%5D&fl=id,when_dt&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_matches_fixture(body, "update_select_dynamic_date");
}

// --- ground truth 8: commitWithin / softCommit visibility -------------------

#[tokio::test]
async fn commitwithin_makes_the_doc_visible_after_the_window() {
    let (app, _dir) = update9_app().await;
    let (status, body) = post9(
        &app,
        "update?commitWithin=500&wt=json",
        &json!([{"id":"u9","body":"commit within doc","category":["keep"]}]).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_matches_fixture(body, "update_commitwithin");

    let (status, body) =
        poll_until_visible(&app, "select?q=id:u9&wt=json", Duration::from_secs(5)).await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "update_select_commitwithin_visible");
}

#[tokio::test]
async fn commitwithin_with_a_large_window_is_invisible_immediately() {
    let (app, _dir) = update9_app().await;
    let (status, body) = post9(
        &app,
        "update?commitWithin=60000&wt=json",
        &json!([{"id":"u9","body":"commit within doc","category":["keep"]}]).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    // A window this large can never elapse during a test run, so asserting
    // "invisible" immediately, with no sleep, cannot flake (task spec).
    let (status, body) = get9(&app, "select?q=id:u9&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.pointer("/response/numFound"),
        Some(&json!(0)),
        "a 60s commitWithin must not have fired yet: {body}"
    );
}

#[tokio::test]
async fn softcommit_true_with_no_commit_param_is_immediately_visible() {
    let (app, _dir) = update9_app().await;
    let (status, body) = post9(
        &app,
        "update?softCommit=true&wt=json",
        &json!([{"id":"u10","body":"soft committed doc","category":["keep"]}]).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_matches_fixture(body, "update_softcommit");

    // softCommit/commit are synchronous — no polling.
    let (status, body) = get9(&app, "select?q=id:u10&wt=json").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_matches_fixture(body, "update_select_softcommit_visible");
}

// --- ground truth 9: unknown core, endpoint-agnostic ------------------------

#[tokio::test]
async fn unknown_core_on_post_update_stays_json_404() {
    let (app, _dir) = update9_app().await;
    let (status, body) = request_full(
        &app,
        "POST",
        "nosuchcore/update?commit=true&wt=json",
        Some(&json!([{"id":"x","body":"y"}]).to_string()),
    )
    .await;
    // Solr's fixture (`update_unknown_core.json`) is the 404 HTML easter egg;
    // Wayfinder keeps its ratified JSON-404 divergence (finding 49), so the
    // shape check below is against Wayfinder's own NoParams envelope, not a
    // literal fixture comparison.
    assert_update_error(status, &body, 404);
}

#[tokio::test]
async fn unknown_core_on_get_admin_ping_stays_json_404() {
    let (app, _dir) = update9_app().await;
    let (status, body) = request_full(&app, "GET", "nosuchcore/admin/ping?wt=json", None).await;
    assert_ping_error(status, &body, 404);
}

#[tokio::test]
async fn delete_on_unknown_core_admin_ping_stays_json_404_not_the_solr_405() {
    let (app, _dir) = update9_app().await;
    // Solr answers a Jetty-level 405 with an empty body here
    // (`ping_unknown_core_delete.json`); Wayfinder stays method-agnostic and
    // serves its normal JSON 404, same divergence family as the unknown-core
    // 404-vs-HTML case above (finding 49) — noted, not matched.
    let (status, body) = request_full(&app, "DELETE", "nosuchcore/admin/ping?wt=json", None).await;
    assert_ping_error(status, &body, 404);
}

// --- UPDATE_PARAMS coverage --------------------------------------------------

#[tokio::test]
async fn strict_params_accepts_commitwithin_overwrite_and_softcommit() {
    let dir = TempDir::new().expect("temp dir");
    let app = build_update9_app_with_config(dir.path(), "strict_params = true\n")
        .expect("app must build");
    let (status, body) = post9(
        &app,
        "update?commitWithin=500&overwrite=false&softCommit=true&wt=json",
        &json!([{"id":"u1","body":"x"}]).to_string(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "commitWithin/overwrite/softCommit must be in UPDATE_PARAMS, got: {body}"
    );
}

#[tokio::test]
async fn strict_params_accepts_json_nl_flat_on_empty_json_update() {
    let dir = TempDir::new().expect("temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, common::SCHEMA_TOML).expect("write schema.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let config_path = dir.path().join("wayfinder.toml");
    std::fs::write(&config_path, "strict_params = true\n").expect("write wayfinder.toml");
    let app =
        wayfinder::app_with_config(&schema_path, &data_dir, &config_path).expect("app must build");
    let (status, body) =
        request_full(&app, "POST", "content/update?json.nl=flat", Some("{}")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "json.nl=flat must be accepted on JSON updates under strict_params, got: {body}"
    );
}

// --- autocommit config (no fixture — config behaviour per spec) ------------

#[tokio::test]
async fn autocommit_max_docs_commits_on_the_nth_uncommitted_doc() {
    let dir = TempDir::new().expect("temp dir");
    let app = build_update9_app_with_config(dir.path(), "[commit]\nautocommit_max_docs = 3\n")
        .expect("app must build");

    let (status, body) = post9(
        &app,
        "update?wt=json",
        &json!([{"id":"a1","body":"one"}]).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let (status, body) = post9(
        &app,
        "update?wt=json",
        &json!([{"id":"a2","body":"two"}]).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let (status, body) = get9(&app, "select?q=*:*&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.pointer("/response/numFound"),
        Some(&json!(0)),
        "the first two uncommitted docs must stay invisible before the threshold: {body}"
    );

    let (status, body) = post9(
        &app,
        "update?wt=json",
        &json!([{"id":"a3","body":"three"}]).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let (status, body) =
        poll_until_visible(&app, "select?q=*:*&wt=json", Duration::from_secs(5)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.pointer("/response/numFound"),
        Some(&json!(3)),
        "the 3rd uncommitted doc must trigger autocommit_max_docs=3, making all 3 visible: {body}"
    );
}

#[tokio::test]
async fn autocommit_max_time_commits_after_the_deadline() {
    let dir = TempDir::new().expect("temp dir");
    let app = build_update9_app_with_config(dir.path(), "[commit]\nautocommit_max_time = 200\n")
        .expect("app must build");

    let (status, body) = post9(
        &app,
        "update?wt=json",
        &json!([{"id":"b1","body":"pending"}]).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let (status, body) =
        poll_until_visible(&app, "select?q=*:*&wt=json", Duration::from_secs(5)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.pointer("/response/numFound"),
        Some(&json!(1)),
        "autocommit_max_time=200ms must make the pending doc visible within the poll window: {body}"
    );
}

/// Review round 1, must-fix: a batch of `[{good doc}, {invalid doc}]` used to
/// return the validation error from `add_documents` before the
/// arm-deadline/autocommit follow-through for the good doc (already written
/// to the writer) ever ran — so with only `autocommit_max_time` configured,
/// that doc stayed pending forever (no deadline armed, and no LATER add
/// could arm one either, since the pending counter was already nonzero).
/// This pins the fix: the good doc in a rejected batch still gets its
/// `autocommit_max_time` deadline armed and becomes visible within the poll
/// window, even though the request itself is a 400.
#[tokio::test]
async fn autocommit_max_time_arms_even_when_a_later_doc_in_the_batch_is_invalid() {
    let dir = TempDir::new().expect("temp dir");
    let app = build_update9_app_with_config(dir.path(), "[commit]\nautocommit_max_time = 200\n")
        .expect("app must build");

    // `title` is single-valued; the second doc's array of two values is
    // exactly the fixture-shaped 400 from `update_single_valued_array.json`.
    let (status, body) = post9(
        &app,
        "update?wt=json",
        &json!([
            {"id":"c1","body":"pending doc"},
            {"id":"c2","title":["one","two"]}
        ])
        .to_string(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "the batch as a whole must still 400 on the invalid second doc: {body}"
    );

    let (status, body) =
        poll_until_visible(&app, "select?q=id:c1&wt=json", Duration::from_secs(5)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.pointer("/response/numFound"),
        Some(&json!(1)),
        "the good doc that landed in the writer before the batch's invalid doc must still get \
         its autocommit_max_time deadline armed and become visible: {body}"
    );
}

// --- issue #154: repeated `add` command keys in one body -------------------
//
// `search_api_solr`'s real `/update` body (`solr-ref/search-api/trace/00001.json`)
// is Solr's *command* JSON format with the top-level `add` key repeated once
// per document — six times in that trace, no `delete`/`commit` keys at all
// (that capture's `commit` came from `commitWithin` on the query string).
// `serde_json::Value`'s object map collapses duplicate keys to the last
// occurrence (verified empirically: parsing
// `{"add":{"doc":{"id":"first"}},"add":{"doc":{"id":"second"}}}` as a
// `Value` yields only `"second"`), so `parse_update_commands`'s current
// `Value`-based parse — which the function's own doc comment says is
// deliberately "out of scope" for this exact shape — drops every add but
// the last. These tests pin the fix: every `add` in a body must survive,
// not just the last (or the first).
//
// The pre-existing fixtures (`update_add_commit.json` /
// `update_mixed_commands.json` / etc.) are all single-command bodies and
// repeat no key, and trace 00001 is only client-side evidence. Stage 2 closed
// that gap: the dedicated Solr capture's issue-#154 block captured the repeated-`add`
// shapes against a real `solr:9` (`update_repeated_add_*.json` and their
// `update_select_after_repeated_add_*.json` corpus states, finding 96), and
// the `..._from_fixtures` tests further down are derived from them. The four
// tests immediately below predate that capture and remain as written; the
// fixtures agree with every one of them.

/// Three duplicate `add` keys, no other command key, mirroring trace 00001's
/// shape (repeated `add`, nothing else) with a distinguishing `title` per
/// doc so a parser that keeps only one occurrence — first OR last — fails,
/// and a parser that cross-contaminates fields between docs also fails.
#[tokio::test]
async fn repeated_add_command_keys_index_every_doc_not_just_one() {
    let (app, _dir) = update9_app().await;

    let body = r#"{"add":{"doc":{"id":"u9","body":"first added","title":"alpha"}},"add":{"doc":{"id":"u10","body":"second added","title":"bravo"}},"add":{"doc":{"id":"u11","body":"third added","title":"charlie"}}}"#;
    let (status, body) = post9(&app, "update?commit=true&wt=json", body).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a repeated-add command body must be accepted, got: {body}"
    );

    // Each doc individually findable, with its own (not another add's)
    // field content — this is what tells apart "kept only the last add"
    // (u9/u10 vanish), "kept only the first add" (u10/u11 vanish), and
    // "cross-contaminated fields" (e.g. u9 stored with title "charlie")
    // from the correct "all three, each with its own fields" outcome.
    for (id, want_title) in [("u9", "alpha"), ("u10", "bravo"), ("u11", "charlie")] {
        let (status, body) = get9(&app, &format!("select?q=id:{id}&wt=json")).await;
        assert_eq!(status, StatusCode::OK, "select for {id}: {body}");
        assert_eq!(
            body.pointer("/response/numFound"),
            Some(&json!(1)),
            "doc {id} from a repeated-add body must be indexed exactly once: {body}"
        );
        assert_eq!(
            body.pointer("/response/docs/0/title/0")
                .or_else(|| body.pointer("/response/docs/0/title")),
            Some(&json!(want_title)),
            "doc {id} must keep its own title, not another add's: {body}"
        );
    }

    // The corpus is the u1..u5 seed plus these three adds — a doc-count
    // check that fails just as hard if only one of the three landed.
    let (status, body) = get9(&app, "select?q=*:*&rows=0&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.pointer("/response/numFound"),
        Some(&json!(8)),
        "seed corpus (5) + all three repeated adds (3) = 8: {body}"
    );
}

/// Two duplicate `add` keys followed by a trailing `commit` command in the
/// same object, with no `?commit=true` query param. The two `numFound == 1`
/// assertions discriminate this from last-add-wins or first-add-wins parsing.
/// This combination is not backed by a `solr-ref/responses/` fixture or trace
/// 00001, which has no command-body `commit` key.
#[tokio::test]
async fn repeated_add_with_trailing_commit_key_indexes_both_docs() {
    let (app, _dir) = update9_app().await;

    let duplicate = r#"{"add":{"doc":{"id":"u9","body":"first"}},"add":{"doc":{"id":"u10","body":"second"}},"commit":{}}"#;
    let (status, body) = post9(&app, "update?wt=json", duplicate).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the coverage probe's exact body must be accepted, got: {body}"
    );

    // No separate ?commit=true call: the body's own `commit` key must have
    // taken effect, exactly as the coverage probe relies on.
    let (status, body) = get9(&app, "select?q=id:u9&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.pointer("/response/numFound"),
        Some(&json!(1)),
        "{body}"
    );

    let (status, body) = get9(&app, "select?q=id:u10&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.pointer("/response/numFound"),
        Some(&json!(1)),
        "{body}"
    );
}

/// A repeated `add` alongside an unknown command key must still 400 —
/// regression guard that a duplicate-key-tolerant parse of `add` does not
/// accidentally start ignoring (rather than rejecting) commands it does not
/// recognise. Unfixtured combination (no capture repeats a command key at
/// all), so the expectation is drawn from the existing single-command
/// convention: an unrecognised top-level key is a 400 with the `/update`
/// no-params error envelope, exactly as `{"id":"x"}` already is
/// (`tests/error_shapes.rs::update_with_a_non_array_body_is_a_400_error_envelope`,
/// pinned to `err_update_bad_json.json`).
#[tokio::test]
async fn repeated_add_with_an_unknown_command_key_is_still_a_400() {
    let (app, _dir) = update9_app().await;
    let body = r#"{"add":{"doc":{"id":"u9","body":"x"}},"add":{"doc":{"id":"u10","body":"y"}},"frobnicate":{}}"#;
    let (status, body) = post9(&app, "update?wt=json", body).await;
    assert_update_error(status, &body, 400);
}

/// One of several repeated `add` commands with no `doc` key must still 400
/// — same "unfixtured, drawn from the existing single-add convention" note
/// as above: a single `{"add":{}}` already 400s today
/// (`parse_update_commands`'s `"\"add\" command is missing \"doc\""`
/// message), and that must hold when it is one of several `add` keys, not
/// just when it is the only one.
#[tokio::test]
async fn one_of_several_repeated_adds_missing_doc_is_still_a_400() {
    let (app, _dir) = update9_app().await;
    let body = r#"{"add":{"doc":{"id":"u9","body":"x"}},"add":{}}"#;
    let (status, body) = post9(&app, "update?wt=json", body).await;
    assert_update_error(status, &body, 400);
}

/// An empty top-level object is a no-op, not an error — no fixture pins this
/// (no capture ever sends a bare `{}`), so this is a regression guard on
/// today's existing `Value`-based behaviour (an empty object's `for (key,
/// val) in map` loop never executes, so `commands` stays default and the
/// handler answers its ordinary 200 success envelope) that a
/// duplicate-key-tolerant rewrite must preserve rather than a pinned Solr
/// fact.
#[tokio::test]
async fn empty_object_body_is_a_200_no_op() {
    let (app, _dir) = update9_app().await;
    let (status, body) = post9(&app, "update?wt=json", "{}").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "empty object body must be a no-op 200: {body}"
    );

    let (status, body) = get9(&app, "select?q=*:*&rows=0&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.pointer("/response/numFound"),
        Some(&json!(5)),
        "an empty-object update body must not add or remove anything from the seed corpus: {body}"
    );
}

// --- issue #154, captured ground truth: repeated command keys --------------
//
// Captured for this issue against a one-off `solr:9` (port 8992, same
// `update9` schema and `u1..u5` seed as the rest of this file; the block is
// appended at the end of the dedicated Solr capture and the container was removed
// afterwards). Every expectation below is read out of those fixtures, not out
// of what Wayfinder produces. Finding 76 records what they settle:
//
//   1. Every repeated `add` executes -- `update_select_after_repeated_add_batch`
//      has BOTH `r1`/alpha and `r2`/bravo, so the body is not last-wins.
//      (The corpus selects are scoped to the ids each body touches, not
//      `q=*:*`: the captured fixture request set rows replay in sequence against one
//      accumulated hermetic core in the retained fixture tests, so a whole-corpus
//      count would pin this capture's fresh-core state and nothing else's.)
//   2. Commands execute in BODY ORDER, not grouped by kind:
//      `update_repeated_add_delete_before` deletes `r4` and then re-adds it in
//      the same body, and `r4`/echo survives. An adds-then-deletes execution
//      loses it.
//   3. A malformed command aborts the whole body: the valid add preceding a
//      doc-less `add` (or an unknown command key) never lands, `numFound` 0.

/// Fixtures 1-3 of the capture block, chained: they pin successive corpus
/// states of one `update9` core, so they are replayed in capture order.
#[tokio::test]
async fn repeated_add_command_sequence_from_fixtures() {
    let (app, _dir) = update9_app().await;

    // Two repeated adds + a delete of a seed doc + a trailing `commit` key,
    // no `?commit` param: `update_repeated_add_batch` is a 200 and the corpus
    // select shows r1/alpha, r2/bravo and u2 gone.
    let (status, body) = post9(
        &app,
        "update?wt=json",
        r#"{"add":{"doc":{"id":"r1","body":"first repeated add","title":"alpha"}},"add":{"doc":{"id":"r2","body":"second repeated add","title":"bravo"}},"delete":{"id":"u2"},"commit":{}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_matches_fixture(body, "update_repeated_add_batch");
    let (status, body) = get9(
        &app,
        "select?q=id:r1+OR+id:r2+OR+id:u2&fl=id,title&rows=20&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "update_select_after_repeated_add_batch");

    // A `delete` BETWEEN two adds sees the add that precedes it: r3 is added
    // and then deleted in one body, r4 survives.
    let (status, body) = post9(
        &app,
        "update?wt=json",
        r#"{"add":{"doc":{"id":"r3","body":"third","title":"charlie"}},"delete":{"id":"r3"},"add":{"doc":{"id":"r4","body":"fourth","title":"delta"}},"commit":{}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_matches_fixture(body, "update_repeated_add_delete_between");
    let (status, body) = get9(
        &app,
        "select?q=id:r3+OR+id:r4&fl=id,title&rows=20&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "update_select_after_repeated_add_delete_between");

    // The reverse: a `delete` BEFORE an add of the same id does not consume
    // it. r4 comes back with the new title `echo` -- the case an
    // adds-then-deletes execution order gets wrong.
    let (status, body) = post9(
        &app,
        "update?wt=json",
        r#"{"delete":{"id":"r4"},"add":{"doc":{"id":"r4","body":"re-added","title":"echo"}},"commit":{}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_matches_fixture(body, "update_repeated_add_delete_before");
    let (status, body) = get9(
        &app,
        "select?q=id:r4&fl=id,title&rows=20&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "update_select_after_repeated_add_delete_before");
}

/// `update_repeated_add_same_id` / `update_select_after_repeated_add_same_id`:
/// two repeated adds of the SAME id in one body leave exactly one doc, the
/// LAST one (`body` "same id second", `title` golf). Corpus-independent (the
/// select is id-scoped), so it starts from the plain seed.
#[tokio::test]
async fn repeated_add_of_the_same_id_keeps_the_last_from_fixtures() {
    let (app, _dir) = update9_app().await;
    let (status, body) = post9(
        &app,
        "update?wt=json",
        r#"{"add":{"doc":{"id":"r5","body":"same id first","title":"foxtrot"}},"add":{"doc":{"id":"r5","body":"same id second","title":"golf"}},"commit":{}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_matches_fixture(body, "update_repeated_add_same_id");
    let (status, body) = get9(&app, "select?q=id:r5&fl=id,title,body&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "update_select_after_repeated_add_same_id");
}

/// `update_repeated_add_missing_doc` is a 400 ("Missing solr document at
/// [66]"), and `update_select_after_repeated_add_missing_doc` shows
/// `numFound` 0 for `r6` -- the VALID add that preceded the doc-less one
/// never landed, even though the request carried `?commit=true`. Message text
/// is Wayfinder's own (this file's stated contract); status and the corpus
/// effect are the pinned parts.
#[tokio::test]
async fn one_of_several_repeated_adds_missing_doc_indexes_nothing_from_fixtures() {
    let (app, _dir) = update9_app().await;
    let (status, body) = post9(
        &app,
        "update?commit=true&wt=json",
        r#"{"add":{"doc":{"id":"r6","body":"valid","title":"hotel"}},"add":{}}"#,
    )
    .await;
    assert_update_error(status, &body, 400);
    let (status, body) = get9(&app, "select?q=id:r6&fl=id,title&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "update_select_after_repeated_add_missing_doc");
}

/// `update_repeated_add_unknown_key` is a 400 ("Unknown command 'frobnicate'
/// at [129]") and neither preceding add lands
/// (`update_select_after_repeated_add_unknown_key`, `numFound` 0): an
/// unrecognised key rejects the body rather than being skipped.
#[tokio::test]
async fn repeated_add_with_unknown_command_key_indexes_nothing_from_fixtures() {
    let (app, _dir) = update9_app().await;
    let (status, body) = post9(
        &app,
        "update?commit=true&wt=json",
        r#"{"add":{"doc":{"id":"r7","body":"valid","title":"india"}},"add":{"doc":{"id":"r8","body":"valid","title":"juliett"}},"frobnicate":{}}"#,
    )
    .await;
    assert_update_error(status, &body, 400);
    let (status, body) = get9(
        &app,
        "select?q=id:r7+OR+id:r8&fl=id,title&sort=id+asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "update_select_after_repeated_add_unknown_key");
}

/// A `commit` key commits what PRECEDES it and nothing after: an `add` that
/// follows the `commit` in the same body is a separate, still-uncommitted
/// batch, invisible until something else commits. This is the one behavioural
/// claim the in-order execution makes that no captured fixture reaches, and
/// it is not equivalent to deferring the body's commit to the end of the
/// request -- a deferred-commit implementation makes `c2` visible here too,
/// and passes every other test in the suite (reviewer-confirmed surviving
/// mutant). Hence this guard.
///
/// ponytail: unfixtured, deliberately. No `solr-ref/responses/` capture puts
/// an `add` AFTER a `commit` key in one body, no `search_api_solr` trace sends
/// that shape (trace 00001 has no body `commit` at all), and a fresh `solr:9`
/// round trip was judged not worth it for this one case. The expectation is
/// inferred from Solr's JSON update format being a command STREAM executed in
/// order -- the same premise finding 96's captured delete/add ordering
/// confirms directly (`update_repeated_add_delete_before.json`: a `delete`
/// before an add of the same id does not consume it, so commands really do
/// take effect where they sit). A capture of
/// `{"add":...,"commit":{},"add":...}` against a real `solr:9` settles it for
/// certain; until then this pins Wayfinder's behaviour, not Solr's, and a
/// capture that disagrees is the fixture's win.
#[tokio::test]
async fn an_add_after_a_body_commit_key_stays_uncommitted() {
    let (app, _dir) = update9_app().await;

    let body = r#"{"add":{"doc":{"id":"c1","body":"before the commit key"}},"commit":{},"add":{"doc":{"id":"c2","body":"after the commit key"}}}"#;
    let (status, body) = post9(&app, "update?wt=json", body).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "commit-then-add is a well-formed command body: {body}"
    );

    let (status, body) = get9(&app, "select?q=id:c1&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.pointer("/response/numFound"),
        Some(&json!(1)),
        "the add BEFORE the body's `commit` key must be committed by it: {body}"
    );

    let (status, body) = get9(&app, "select?q=id:c2&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.pointer("/response/numFound"),
        Some(&json!(0)),
        "the add AFTER the body's `commit` key must still be uncommitted -- a \
         commit deferred to the end of the request would make it visible: {body}"
    );

    // ...and it really was indexed, just uncommitted: a later commit reveals
    // it. Without this leg the test above would also pass if the trailing add
    // had been dropped outright.
    let (status, body) = get9(&app, "update?commit=true&wt=json").await;
    assert_eq!(status, StatusCode::OK, "bare commit: {body}");
    let (status, body) = get9(&app, "select?q=id:c2&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.pointer("/response/numFound"),
        Some(&json!(1)),
        "the trailing add was buffered, not dropped: {body}"
    );
}
