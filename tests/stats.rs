//! Solr `stats` component (issue #5, PRD §5): `stats=true` / `stats.field`
//! (repeatable) over numeric fast fields -> the `stats.stats_fields.<field>`
//! response block.
//!
//! Every expected value here comes from a committed fixture in
//! `solr-ref/responses/`, captured against a dedicated `stats` Solr core
//! (the dedicated Solr capture's issue-#5 block, container `wayfinder-solr-5`, port
//! 8992) — never from what Wayfinder happens to produce. See
//! `docs/solr-ref-findings.md` finding 51 for what that capture found.
//!
//! **Premise check (documented in the task spec):** issue #3's `facets` core
//! (`views`/`created`, corpus `r1..r4`) has a value on *every* doc, so it
//! cannot exercise "missing on some docs" — the whole point of `missing` and
//! of "min/max/sum/etc. computed only over docs where the field has a value".
//! This suite therefore uses its own dedicated corpus (`st1..st6`, mirroring
//! the capture) rather than reusing `facets`.
//!
//! Fixture floats (`sum`, `sumOfSquares`, `mean`, `stddev`) are compared with
//! `common::diff`'s tolerance mechanism (`score_tolerance()`, extended by
//! this issue to the `stats_fields` subtree) rather than
//! `common::assert_matches_fixture`'s exact equality, for the same
//! Tantivy-vs-Solr float-summation-order reason `score` already needed it.
//! `min`/`max`/`count`/`missing` stay exact.

mod common;

use axum::Router;
use serde_json::Value;
use tempfile::TempDir;

use common::diff::{diff, normalize};
use common::{app_with_schema, fixture, get, post_docs};

/// A dedicated core so `stats.field` gets numeric fast fields with an actual
/// missing-value gap, without touching `common::SCHEMA_TOML` (which would
/// rewrite ground truth for every doc-returning fixture). Named `content` so
/// `common::get`/`common::CORE` address it unchanged — Wayfinder's core name
/// is independent of the Solr core the fixtures came from (same trick as
/// `tests/faceting.rs::RANGE_SCHEMA_TOML`).
const STATS_SCHEMA_TOML: &str = r#"
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
name = "views"
type = "int"
stored = true
fast = true

[[fields]]
name = "price"
type = "double"
stored = true
fast = true
"#;

/// The exact 6-doc corpus the dedicated Solr capture's issue-#5 block indexes into
/// the `stats` Solr core: `views` missing on `st6`, `price` missing on `st5`
/// — two independent gaps.
fn stats_corpus() -> Value {
    serde_json::json!([
        {"id":"st1","views":10,"price":1.5},
        {"id":"st2","views":20,"price":2.5},
        {"id":"st3","views":30,"price":3.5},
        {"id":"st4","views":40,"price":4.5},
        {"id":"st5","views":50},
        {"id":"st6","price":5.5}
    ])
}

/// Builds an app on `STATS_SCHEMA_TOML` and indexes `stats_corpus()`.
async fn stats_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = app_with_schema(dir.path(), STATS_SCHEMA_TOML).expect("app must build");
    let (status, body) = post_docs(&app, &stats_corpus()).await;
    assert_eq!(
        status,
        axum::http::StatusCode::OK,
        "indexing the stats corpus must succeed, got {body}"
    );
    (app, dir)
}

/// Asserts `actual` matches the named fixture using `common::diff`'s
/// tolerance-aware differ (not `common::assert_matches_fixture`'s strict
/// equality), since stats fixtures carry irrational `stddev` floats that
/// legitimately differ in the last bits between Tantivy's and Solr/Lucene's
/// summation order — exactly the reason `score` already needed tolerance.
fn assert_matches_fixture_with_tolerance(actual: Value, fixture_name: &str) {
    let expected = normalize(fixture(fixture_name));
    let actual = normalize(actual);
    let report = diff(&expected.value, &actual.value);
    assert!(
        report.diffs.is_empty(),
        "response for fixture `{fixture_name}` did not match (modulo QTime / tolerance): {:#?}\n\
         touched paths: {:?}",
        report.diffs,
        report.touched,
    );
}

// --- 1. normal case: whole corpus, one stats.field with a real gap ---------

#[tokio::test]
async fn stats_views_matches_fixture() {
    let (app, _dir) = stats_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&stats=true&stats.field=views&wt=json",
    )
    .await;
    assert_eq!(status, 200, "stats=true must not 400, got {body}");
    assert_matches_fixture_with_tolerance(body, "stats_views");
}

/// The `stats` block is keyed exactly `min`, `max`, `count`, `missing`,
/// `sum`, `sumOfSquares`, `mean`, `stddev` per field (task spec's envelope
/// shape) and its values come only from the 5 docs that actually have
/// `views` — `st6` (no `views`) must not pull `min` down or `count` up.
#[tokio::test]
async fn stats_views_values_are_computed_over_present_docs_only() {
    let (app, _dir) = stats_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&stats=true&stats.field=views&wt=json",
    )
    .await;
    assert_eq!(status, 200, "stats=true must not 400, got {body}");
    let views = body
        .pointer("/stats/stats_fields/views")
        .unwrap_or_else(|| panic!("stats.stats_fields.views must be present, got {body}"));
    assert_eq!(views.get("count").and_then(Value::as_i64), Some(5));
    assert_eq!(views.get("missing").and_then(Value::as_i64), Some(1));
    assert_eq!(views.get("min").and_then(Value::as_f64), Some(10.0));
    assert_eq!(views.get("max").and_then(Value::as_f64), Some(50.0));
    assert_eq!(views.get("sum").and_then(Value::as_f64), Some(150.0));
}

// --- 2. repeatable stats.field, two independent missing-value gaps --------

#[tokio::test]
async fn stats_multi_fields_matches_fixture() {
    let (app, _dir) = stats_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&stats=true&stats.field=views&stats.field=price&wt=json",
    )
    .await;
    assert_eq!(status, 200, "stats=true must not 400, got {body}");
    assert_matches_fixture_with_tolerance(body, "stats_multi_fields");
}

#[tokio::test]
async fn stats_multi_fields_missing_is_per_field_not_shared() {
    let (app, _dir) = stats_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&stats=true&stats.field=views&stats.field=price&wt=json",
    )
    .await;
    assert_eq!(status, 200, "stats=true must not 400, got {body}");
    let views = body
        .pointer("/stats/stats_fields/views")
        .unwrap_or_else(|| panic!("stats.stats_fields.views must be present, got {body}"));
    let price = body
        .pointer("/stats/stats_fields/price")
        .unwrap_or_else(|| panic!("stats.stats_fields.price must be present, got {body}"));
    // views is missing on st6, price is missing on st5 -- two different docs.
    assert_eq!(views.get("missing").and_then(Value::as_i64), Some(1));
    assert_eq!(price.get("missing").and_then(Value::as_i64), Some(1));
    assert_eq!(price.get("min").and_then(Value::as_f64), Some(1.5));
    assert_eq!(price.get("max").and_then(Value::as_f64), Some(5.5));
}

// --- 3. zero matching docs, via q and via fq -------------------------------

/// Solr's zero-matching-docs shape (`solr-ref/responses/stats_zero.json`):
/// `min`/`max` are JSON `null`, `count`/`missing` are `0`, `sum`/`sumOfSquares`
/// are `0.0`, `mean` is the *string* `"NaN"` (not `null`, not a bare `NaN`
/// literal), and `stddev` is `0.0`. This is deliberately asserted field-by-
/// field, not just diffed against the fixture, because `"NaN"`-as-a-string is
/// exactly the kind of surprising, easy-to "normalise away" shape a less
/// literal implementation could get wrong without a fixture diff catching it
/// (a native `f64::NAN` serialises as JSON `null` via `serde_json`, not the
/// string `"NaN"` -- see docs/solr-ref-findings.md finding 51).
#[tokio::test]
async fn stats_zero_matching_docs_matches_fixture() {
    let (app, _dir) = stats_app().await;
    let (status, body) = get(
        &app,
        "select?q=id:nosuchdoc&rows=0&stats=true&stats.field=views&wt=json",
    )
    .await;
    assert_eq!(status, 200, "zero hits must not 400, got {body}");
    assert_matches_fixture_with_tolerance(body.clone(), "stats_zero");

    let views = body
        .pointer("/stats/stats_fields/views")
        .unwrap_or_else(|| panic!("stats.stats_fields.views must be present, got {body}"));
    assert!(
        views.get("min").is_some_and(Value::is_null),
        "min must be JSON null on zero matching docs, got {views}"
    );
    assert!(
        views.get("max").is_some_and(Value::is_null),
        "max must be JSON null on zero matching docs, got {views}"
    );
    assert_eq!(views.get("count").and_then(Value::as_i64), Some(0));
    assert_eq!(views.get("missing").and_then(Value::as_i64), Some(0));
    assert_eq!(views.get("sum").and_then(Value::as_f64), Some(0.0));
    assert_eq!(views.get("sumOfSquares").and_then(Value::as_f64), Some(0.0));
    assert_eq!(
        views.get("mean").and_then(Value::as_str),
        Some("NaN"),
        "mean must be the literal string \"NaN\" on zero matching docs, got {views}"
    );
    assert_eq!(views.get("stddev").and_then(Value::as_f64), Some(0.0));
}

/// Same zero-matching-docs shape reached via `fq` narrowing the base `q`
/// rather than `q` itself matching nothing (task spec: "or an `fq` narrows to
/// nothing").
#[tokio::test]
async fn stats_zero_matching_docs_via_fq_matches_fixture() {
    let (app, _dir) = stats_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&fq=id:nosuchdoc&rows=0&stats=true&stats.field=views&wt=json",
    )
    .await;
    assert_eq!(status, 200, "zero hits via fq must not 400, got {body}");
    assert_matches_fixture_with_tolerance(body, "stats_zero_fq");
}

// --- 4. request-shape sanity, independent of the fixtures ------------------

/// Without `stats=true`, the `stats` key must be entirely absent, mirroring
/// `facet_counts`'s "absent, not present-and-empty" rule (findings fact 4) --
/// no fixture directly proves this for `stats` since none of the captured
/// requests omit `stats=true`, but it is the same wire-shape convention the
/// task spec's envelope section describes stats as following, and matches
/// `select_all.json`/`facet_basic.json`'s already-covered absence pattern.
#[tokio::test]
async fn stats_key_absent_without_stats_true() {
    let (app, _dir) = stats_app().await;
    let (status, body) = get(&app, "select?q=*:*&rows=0&wt=json").await;
    assert_eq!(status, 200, "plain select must not 400, got {body}");
    assert!(
        body.get("stats").is_none(),
        "stats key must be absent (not present-and-empty) when stats was not requested, got {body}"
    );
}

/// `stats.field` must be repeatable, exactly like `facet.field` -- named
/// separately from `stats_multi_fields_matches_fixture` so a regression that
/// only handles a *single* `stats.field` still gets one assertion result
/// naming exactly what broke (both fields present).
#[tokio::test]
async fn stats_field_is_repeatable() {
    let (app, _dir) = stats_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&stats=true&stats.field=views&stats.field=price&wt=json",
    )
    .await;
    assert_eq!(status, 200, "repeated stats.field must not 400, got {body}");
    assert!(
        body.pointer("/stats/stats_fields/views").is_some(),
        "stats.stats_fields.views must be present, got {body}"
    );
    assert!(
        body.pointer("/stats/stats_fields/price").is_some(),
        "stats.stats_fields.price must be present, got {body}"
    );
}

/// `stats=true` with no `stats.field` at all is not a fixture case (every
/// captured request supplies at least one `stats.field`), but matches Solr's
/// own shape: the component runs, finds nothing to compute stats on, and
/// reports an empty `stats_fields` object rather than omitting the whole
/// `stats` key (that omission is reserved for `stats` not being `true` at
/// all -- `stats_key_absent_without_stats_true` above).
#[tokio::test]
async fn stats_true_with_no_stats_field_is_an_empty_stats_fields_object() {
    let (app, _dir) = stats_app().await;
    let (status, body) = get(&app, "select?q=*:*&rows=0&stats=true&wt=json").await;
    assert_eq!(status, 200, "stats=true alone must not 400, got {body}");
    assert_eq!(
        body.pointer("/stats/stats_fields"),
        Some(&serde_json::json!({})),
        "stats.stats_fields must be present and empty with no stats.field, got {body}"
    );
}

// --- 5. stats.field validation (no fixture: mutation-tested per project convention) ---

/// A `stats.field` naming a field that is not declared in the schema at all
/// must 400, not silently answer with an all-zero/all-null stats block --
/// mirrors `facet::check_facetable`'s "undefined field" branch. Mutation-test
/// this by commenting out the `None =>` arm in `check_statable`: without it
/// this request would panic or misbehave inside `CoreIndex::field_stats`
/// instead of cleanly 400ing.
#[tokio::test]
async fn stats_field_on_an_undefined_field_is_400() {
    let (app, _dir) = stats_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&stats=true&stats.field=nosuchfield&wt=json",
    )
    .await;
    assert_eq!(
        status, 400,
        "stats.field on an undefined field must 400, got {body}"
    );
}

/// A `stats.field` naming a `fast` **string** field (`id`, in
/// `STATS_SCHEMA_TOML`) must also 400: `fast` alone does not mean
/// aggregatable for `stats` any more than it does for `facet.range`
/// (`facet::range_buckets`'s `ValueKind::Text` arm makes the same call).
/// Tantivy's `ExtendedStats` aggregation silently substitutes an empty column
/// for a non-numeric/date field rather than erroring, so without this check
/// `stats.field=id` would come back 200 with a dishonest
/// `count: 0, missing: 0, min: null, max: null, mean: "NaN"` even though every
/// doc in the corpus has an `id`. Mutation-test this by deleting the
/// `ValueKind::Text` arm in `check_statable`: this assertion is the one thing
/// that would catch that silent regression.
#[tokio::test]
async fn stats_field_on_a_fast_string_field_is_400_not_a_silent_empty_result() {
    let (app, _dir) = stats_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&stats=true&stats.field=id&wt=json",
    )
    .await;
    assert_eq!(
        status, 400,
        "stats.field on a fast string field must 400, not silently return an empty result, got {body}"
    );
}

// --- 6. SELECT_PARAMS guard ------------------------------------------------

/// The `strict_params` guard for every `stats.*` param this issue adds: a
/// request using both `stats` and `stats.field` under `strict_params = true`
/// must never 400 on "unknown request parameter" -- that would mean one of
/// them was implemented but forgotten in `SELECT_PARAMS`, the same class of
/// bug `tests/faceting.rs::strict_params_accepts_every_implemented_facet_param`
/// guards against for the `facet.*` family.
#[tokio::test]
async fn strict_params_accepts_stats_and_stats_field() {
    let dir = TempDir::new().expect("temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, STATS_SCHEMA_TOML).expect("write schema.toml");
    let config_path = dir.path().join("wayfinder.toml");
    std::fs::write(&config_path, "strict_params = true\n").expect("write wayfinder.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let app =
        wayfinder::app_with_config(&schema_path, &data_dir, &config_path).expect("app must build");
    let (status, body) = post_docs(&app, &stats_corpus()).await;
    assert_eq!(
        status,
        axum::http::StatusCode::OK,
        "seeding must succeed, got {body}"
    );

    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&stats=true&stats.field=views&stats.field=price&wt=json",
    )
    .await;
    let msg = body
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        !msg.contains("unknown request parameter"),
        "stats/stats.field must be in SELECT_PARAMS, got: {msg}"
    );
    assert_eq!(
        status, 200,
        "strict_params must accept stats/stats.field, got {body}"
    );
}
