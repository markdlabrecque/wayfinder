//! Faceting completion + real fast-field aggregation (issue #3, PRD §5).
//!
//! Every expected value here comes from a committed fixture in
//! `solr-ref/responses/` — `facet_*.json` for the tracer-bullet corpus and the
//! `facet_range_*` / `facet_non_docvalues_*` captures added by this issue.
//! Nothing is derived from what Wayfinder happens to produce.
//!
//! Two things in here are deliberately *not* fixture comparisons:
//!
//! - **The dictionary-enumeration property.** Solr's `facet.field` enumerates
//!   the whole term dictionary of the field, not the hit set: a query matching
//!   one document still reports every other term at 0 (`facet_zero.json`,
//!   `facet_subset.json`). Those assertions are written out term-by-term as
//!   well as diffed against the fixture, because it is the one property a
//!   hardcoded zero-fill could fake against a single fixture but not across
//!   hit sets of different sizes.
//!
//! - **Unfacetable fields.** Captured Solr contradicts the issue premise here:
//!   `facet.field` on a non-docValues text field, on a stored-only field, and
//!   on a field that does not exist at all are all HTTP 200 with an empty
//!   array (`facet_non_docvalues_text.json`, `facet_stored_only_field.json`,
//!   `facet_unknown_field.json`) — the exact silent-empty-counts behaviour
//!   tracer-bullet review follow-up 1 calls a bug. Wayfinder diverges
//!   deliberately: Tantivy cannot aggregate a non-`fast` field, so it is a hard
//!   400 with the Solr error envelope. Those fixtures are therefore in
//!   `manifest-errors.tsv` (ground truth for the divergence) rather than
//!   `manifest.tsv` (a target the differential harness must match).

// The `dead_code` allow for partially-used shared helpers is an inner attribute
// inside `tests/common/mod.rs`; repeating it here is a clippy error under
// `-D warnings`.
mod common;

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;

use common::{assert_matches_fixture, corpus, get, indexed_app, post_docs};

// --- local helpers ----------------------------------------------------------

/// A second core's schema, so `facet.range` gets numeric and date fields
/// without touching `common::SCHEMA_TOML` (adding a field there would rewrite
/// ground truth for every doc-returning fixture). Mirrors the `facets` core
/// `solr-ref/capture.sh` creates for the `facet_range_*` captures:
/// `views` (int, fast), `created` (date, fast), `note` (string, stored only —
/// *not* fast, so unfacetable).
///
/// The core is named `content` so `common::get` addresses it unchanged;
/// Wayfinder's core name is independent of the Solr core the fixtures came
/// from.
const RANGE_SCHEMA_TOML: &str = r#"
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
name = "views"
type = "int"
stored = true
fast = true

[[fields]]
name = "created"
type = "date"
stored = true
fast = true

[[fields]]
name = "note"
type = "string"
stored = true
"#;

/// The 4-doc corpus `capture.sh` indexes into the `facets` core.
fn range_corpus() -> Value {
    json!([
        {"id":"r1","views":5, "created":"2020-01-02T00:00:00Z","note":"alpha"},
        {"id":"r2","views":15,"created":"2020-01-03T00:00:00Z","note":"beta"},
        {"id":"r3","views":25,"created":"2020-01-03T00:00:00Z","note":"alpha"},
        {"id":"r4","views":35,"created":"2020-01-05T00:00:00Z"}
    ])
}

/// Builds an app on `RANGE_SCHEMA_TOML` and indexes `range_corpus()`.
async fn range_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), RANGE_SCHEMA_TOML).expect("app must build");
    let (status, body) = post_docs(&app, &range_corpus()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the range corpus must succeed, got {body}"
    );
    (app, dir)
}

/// Builds an app on an arbitrary schema *and* an arbitrary server config, and
/// indexes `docs`. `common::app_with_schema` always uses `ServerConfig`
/// defaults, and `facet_limit_max` / `strict_params` are config knobs.
async fn app_with_schema_and_config(
    schema_toml: &str,
    config_toml: &str,
    docs: &Value,
) -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, schema_toml).expect("write schema.toml");
    let config_path = dir.path().join("wayfinder.toml");
    std::fs::write(&config_path, config_toml).expect("write wayfinder.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let app =
        wayfinder::app_with_config(&schema_path, &data_dir, &config_path).expect("app must build");
    let (status, body) = post_docs(&app, docs).await;
    assert_eq!(status, StatusCode::OK, "indexing must succeed, got {body}");
    (app, dir)
}

/// `facet_counts.facet_fields.<field>` as a flat alternating array.
fn flat_facet(body: &Value, field: &str) -> Vec<Value> {
    body.pointer(&format!("/facet_counts/facet_fields/{field}"))
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| {
            panic!("facet_counts.facet_fields.{field} must be a flat array, got: {body}")
        })
        .clone()
}

/// Builds the flat alternating array Solr uses, from `(term, count)` pairs.
/// `None` is the `facet.missing` bucket's literal `null` key (findings fact 2).
fn expect_flat(pairs: &[(Option<&str>, i64)]) -> Vec<Value> {
    let mut out = Vec::with_capacity(pairs.len() * 2);
    for (term, count) in pairs {
        out.push(match term {
            Some(t) => Value::from(*t),
            None => Value::Null,
        });
        out.push(Value::from(*count));
    }
    out
}

/// Why a facet was refused, so each case can be pinned to *Wayfinder's own*
/// wording rather than to any 400 that happens to mention the field.
///
/// Asserting only "a 400 naming the field" is not enough to mutation-test the
/// guard: Tantivy's own aggregation error (`Field "body" is not configured as
/// fast field`) also names the field, so deleting the guard's non-`fast` arm
/// leaves such an assertion green. That would let a future Tantivy upgrade —
/// one that turned the non-`fast` error into an empty result — go undetected,
/// which is the exact failure this issue exists to prevent. These fragments come
/// from `facet::check_facetable`, mirroring finding 11's `sort` wording.
enum Refusal {
    /// The field is declared but not `fast`, so there is no column to aggregate.
    NotFast,
    /// The field is not declared at all.
    Undefined,
}

impl Refusal {
    fn fragment(&self) -> &'static str {
        match self {
            Refusal::NotFast => "fast values (docValues)",
            Refusal::Undefined => "undefined field",
        }
    }
}

/// Asserts a 400 with the Solr error envelope (finding 10), and that no facet
/// block was emitted instead — the whole point is that an unfacetable field is
/// never a silent empty array.
fn assert_facet_400(status: StatusCode, body: &Value, must_name: &str, why: Refusal) {
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an unfacetable field must be a 400, not a silent empty array; got {status} / {body}"
    );
    assert_eq!(
        body.pointer("/error/code").and_then(Value::as_i64),
        Some(400),
        "error.code must mirror the HTTP status (finding 10), got {body}"
    );
    assert_eq!(
        body.pointer("/responseHeader/status")
            .and_then(Value::as_i64),
        Some(400),
        "responseHeader.status must mirror the HTTP status (finding 10), got {body}"
    );
    assert!(
        body.pointer("/responseHeader/params").is_some(),
        "/select errors echo params (finding 13), got {body}"
    );
    let msg = body
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("error.msg must be present, got {body}"));
    assert!(
        msg.contains(must_name),
        "error.msg must name the offending field `{must_name}`, got: {msg}"
    );
    let fragment = why.fragment();
    assert!(
        msg.contains(fragment),
        "error.msg must be Wayfinder's own refusal, containing `{fragment}` — a 400 that merely \
         mentions the field can come from Tantivy instead, which would let the guard be deleted \
         without a test noticing. Got: {msg}"
    );
    assert!(
        body.get("facet_counts").is_none(),
        "an error response must not carry a facet_counts block at all, got {body}"
    );
}

// --- 1. the nine pre-existing facet fixtures, reproduced exactly ------------

#[tokio::test]
async fn facet_basic_matches_fixture() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "facet_basic");
}

#[tokio::test]
async fn facet_limit_matches_fixture() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&facet.limit=1&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "facet_limit");
}

#[tokio::test]
async fn facet_mincount_matches_fixture() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&facet.mincount=2&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "facet_mincount");
}

#[tokio::test]
async fn facet_missing_matches_fixture() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&facet.missing=true&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "facet_missing");
}

#[tokio::test]
async fn facet_sort_index_matches_fixture() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&facet.sort=index&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "facet_sort_index");
}

#[tokio::test]
async fn facet_query_matches_fixture() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.query=category:animals&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "facet_query");
}

#[tokio::test]
async fn facet_json_nl_map_matches_fixture() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&json.nl=map&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "facet_json_nl_map");
}

#[tokio::test]
async fn facet_zero_matches_fixture() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=zzzznope&df=body&rows=0&facet=true&facet.field=category&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "facet_zero");
}

#[tokio::test]
async fn facet_all_filtered_matches_fixture() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&facet.mincount=99&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "facet_all_filtered");
}

// --- 2. the dictionary-enumeration property, independent of the fixtures ----

#[tokio::test]
async fn counts_come_from_the_term_dictionary_not_the_hit_set_one_doc() {
    let (app, _dir) = indexed_app().await;

    // `doc2` is the only `garden` doc. `animals`, `classic` and `misc` are
    // reachable only through documents this query does *not* match, and must
    // still be listed, at 0 (`facet_subset.json`).
    let (status, body) = get(
        &app,
        "select?q=id:doc2&rows=0&facet=true&facet.field=category&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["response"]["numFound"], 1, "hit set is one doc");
    assert_eq!(
        flat_facet(&body, "category"),
        expect_flat(&[
            (Some("garden"), 1),
            (Some("animals"), 0),
            (Some("classic"), 0),
            (Some("misc"), 0),
        ]),
        "every dictionary term must appear, count desc then term asc"
    );
    assert_matches_fixture(body, "facet_subset");
}

#[tokio::test]
async fn counts_come_from_the_term_dictionary_not_the_hit_set_two_docs() {
    let (app, _dir) = indexed_app().await;

    // A second hit-set size, so a hardcoded zero-fill cannot satisfy both:
    // `quick` matches doc1 (animals, classic) and doc3 (misc, classic), so
    // `classic` outranks the singletons and `garden` is only reachable through
    // a non-matching doc (`facet_sort_count_tiebreak.json`).
    let (status, body) = get(
        &app,
        "select?q=quick&df=body&rows=0&facet=true&facet.field=category&facet.sort=count&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["response"]["numFound"], 2, "hit set is two docs");
    assert_eq!(
        flat_facet(&body, "category"),
        expect_flat(&[
            (Some("classic"), 2),
            (Some("animals"), 1),
            (Some("misc"), 1),
            (Some("garden"), 0),
        ])
    );
    assert_matches_fixture(body, "facet_sort_count_tiebreak");
}

// --- 3. repeatable facet.field ---------------------------------------------

#[tokio::test]
async fn repeated_facet_field_gives_each_field_its_own_key() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&facet.field=id&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(
        flat_facet(&body, "category"),
        expect_flat(&[
            (Some("animals"), 2),
            (Some("classic"), 2),
            (Some("garden"), 1),
            (Some("misc"), 1),
        ])
    );
    assert_eq!(
        flat_facet(&body, "id"),
        expect_flat(&[
            (Some("doc1"), 1),
            (Some("doc2"), 1),
            (Some("doc3"), 1),
            (Some("doc4"), 1),
            (Some("doc5"), 1),
        ]),
        "each facet.field is counted independently"
    );
    assert_matches_fixture(body, "facet_multi_field");
}

// --- 4. repeatable facet.query --------------------------------------------

#[tokio::test]
async fn repeated_facet_query_is_keyed_by_the_verbatim_query_string() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true\
         &facet.query=category:animals&facet.query=category:garden&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    let queries = body
        .pointer("/facet_counts/facet_queries")
        .and_then(|v| v.as_object())
        .unwrap_or_else(|| panic!("facet_queries must be an object, got {body}"));
    assert_eq!(queries.get("category:animals"), Some(&json!(2)));
    assert_eq!(queries.get("category:garden"), Some(&json!(1)));
    assert_eq!(queries.len(), 2, "one key per facet.query, no extras");
    assert_matches_fixture(body, "facet_query_multi");
}

#[tokio::test]
async fn facet_query_matching_nothing_is_zero_not_an_omitted_key() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.query=category:nosuchvalue&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(
        body.pointer("/facet_counts/facet_queries/category:nosuchvalue"),
        Some(&json!(0)),
        "a facet query with no matches keeps its key at 0, got {body}"
    );
    assert_matches_fixture(body, "facet_query_zero");
}

#[tokio::test]
async fn facet_query_is_intersected_with_q_and_every_fq() {
    let (app, _dir) = indexed_app().await;

    // `category:animals` matches doc1 + doc4 on its own, but only doc1 is also
    // `category:classic` (`facet_query_with_fq.json`).
    let (status, body) = get(
        &app,
        "select?q=*:*&fq=category:classic&rows=0&facet=true\
         &facet.query=category:animals&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["response"]["numFound"], 2);
    assert_eq!(
        body.pointer("/facet_counts/facet_queries/category:animals"),
        Some(&json!(1)),
        "facet.query counts docs matching q AND every fq AND the facet query"
    );
    assert_matches_fixture(body, "facet_query_with_fq");
}

// --- 5. facet.limit boundaries --------------------------------------------

#[tokio::test]
async fn facet_limit_zero_returns_an_empty_array() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&facet.limit=0&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(
        flat_facet(&body, "category"),
        Vec::<Value>::new(),
        "facet.limit=0 keeps the key, empty"
    );
    assert_matches_fixture(body, "facet_limit_zero");
}

#[tokio::test]
async fn facet_limit_minus_one_is_unlimited() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&facet.limit=-1&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(
        flat_facet(&body, "category").len(),
        8,
        "facet.limit=-1 returns every term (4 terms = 8 flat entries)"
    );
    assert_matches_fixture(body, "facet_limit_unlimited");
}

#[tokio::test]
async fn facet_limit_above_facet_limit_max_is_capped() {
    // `query.facet_limit_max` is a Wayfinder cap with no Solr equivalent, so
    // (like `rows_limit`) an over-limit request is clamped rather than
    // rejected — a clamp keeps a client that asks for too much working.
    let (app, _dir) = app_with_schema_and_config(
        common::SCHEMA_TOML,
        "[query]\nfacet_limit_max = 2\n",
        &corpus(),
    )
    .await;

    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&facet.limit=100&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(
        flat_facet(&body, "category"),
        expect_flat(&[(Some("animals"), 2), (Some("classic"), 2)]),
        "facet.limit must be capped at query.facet_limit_max"
    );
}

#[tokio::test]
async fn facet_limit_unlimited_is_also_capped_by_facet_limit_max() {
    let (app, _dir) = app_with_schema_and_config(
        common::SCHEMA_TOML,
        "[query]\nfacet_limit_max = 2\n",
        &corpus(),
    )
    .await;

    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&facet.limit=-1&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(
        flat_facet(&body, "category").len(),
        4,
        "`-1` means unlimited, but the server cap still applies"
    );
}

// --- 6. facet.mincount ----------------------------------------------------

#[tokio::test]
async fn facet_mincount_defaults_to_zero_and_keeps_zero_count_terms() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=id:doc2&rows=0&facet=true&facet.field=category&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    let flat = flat_facet(&body, "category");
    assert!(
        flat.contains(&json!(0)),
        "the default facet.mincount of 0 keeps zero-count terms, got {flat:?}"
    );
    assert_eq!(flat.len(), 8, "all four dictionary terms are present");
}

#[tokio::test]
async fn facet_mincount_one_drops_the_zero_count_terms() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=id:doc2&rows=0&facet=true&facet.field=category&facet.mincount=1&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(
        flat_facet(&body, "category"),
        expect_flat(&[(Some("garden"), 1)]),
        "mincount=1 keeps only the terms the hit set actually has"
    );
    assert_matches_fixture(body, "facet_mincount_one");
}

#[tokio::test]
async fn facet_mincount_above_every_count_leaves_an_empty_array() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&facet.mincount=99&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(
        flat_facet(&body, "category"),
        Vec::<Value>::new(),
        "the key stays, present-and-empty"
    );
}

// --- 7. facet.sort --------------------------------------------------------

#[tokio::test]
async fn facet_sort_count_breaks_ties_on_term_ascending() {
    let (app, _dir) = indexed_app().await;

    // Count order and term order genuinely differ here: `classic` (2) leads
    // even though it sorts after `animals`, and the two 1s are term-ascending.
    let (status, body) = get(
        &app,
        "select?q=quick&df=body&rows=0&facet=true&facet.field=category&facet.sort=count&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(
        flat_facet(&body, "category"),
        expect_flat(&[
            (Some("classic"), 2),
            (Some("animals"), 1),
            (Some("misc"), 1),
            (Some("garden"), 0),
        ]),
        "facet.sort=count is count desc, term asc on ties"
    );
}

#[tokio::test]
async fn facet_sort_index_is_term_ascending_regardless_of_count() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=quick&df=body&rows=0&facet=true&facet.field=category&facet.sort=index&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(
        flat_facet(&body, "category"),
        expect_flat(&[
            (Some("animals"), 1),
            (Some("classic"), 2),
            (Some("garden"), 0),
            (Some("misc"), 1),
        ]),
        "facet.sort=index ignores counts entirely"
    );
    assert_matches_fixture(body, "facet_sort_index_subset");
}

// --- 8. facet.missing is hit-set-based ------------------------------------

#[tokio::test]
async fn facet_missing_counts_only_docs_in_the_hit_set() {
    let (app, _dir) = indexed_app().await;

    // doc5 is the only document with no `category`, and it is outside this hit
    // set, so the `null` bucket is 0 — not the 1 that a corpus-wide count would
    // give (`facet_missing_no_hit.json`).
    let (status, body) = get(
        &app,
        "select?q=id:doc2&rows=0&facet=true&facet.field=category&facet.missing=true&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(
        flat_facet(&body, "category"),
        expect_flat(&[
            (Some("garden"), 1),
            (Some("animals"), 0),
            (Some("classic"), 0),
            (Some("misc"), 0),
            (None, 0),
        ]),
        "the literal null key comes last, and its count is hit-set-based"
    );
    assert_matches_fixture(body, "facet_missing_no_hit");
}

// --- 9. json.nl=map ------------------------------------------------------

#[tokio::test]
async fn json_nl_map_switches_every_facet_field_to_an_object() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&facet.field=id&json.nl=map&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(
        body.pointer("/facet_counts/facet_fields/category"),
        Some(&json!({"animals": 2, "classic": 2, "garden": 1, "misc": 1}))
    );
    assert_eq!(
        body.pointer("/facet_counts/facet_fields/id"),
        Some(&json!({"doc1": 1, "doc2": 1, "doc3": 1, "doc4": 1, "doc5": 1})),
        "json.nl=map applies to every field, not just the first"
    );
    assert_matches_fixture(body, "facet_json_nl_map_multi");
}

#[tokio::test]
async fn json_nl_map_switches_range_counts_to_an_object() {
    let (app, _dir) = range_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.range=views\
         &facet.range.start=0&facet.range.end=40&facet.range.gap=10&json.nl=map&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(
        body.pointer("/facet_counts/facet_ranges/views/counts"),
        Some(&json!({"0": 1, "10": 1, "20": 1, "30": 1})),
        "json.nl=map turns facet_ranges.<name>.counts into an object too"
    );
    assert_matches_fixture(body, "facet_range_json_nl_map");
}

// --- 10. facet.range ------------------------------------------------------

#[tokio::test]
async fn facet_range_over_a_numeric_field_matches_fixture() {
    let (app, _dir) = range_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.range=views\
         &facet.range.start=0&facet.range.end=40&facet.range.gap=10&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    // The bucket keys are *strings* even for a numeric field, while `gap`,
    // `start` and `end` echo back as JSON *numbers* — captured, not guessed
    // (`facet_range_numeric.json`).
    assert_eq!(
        body.pointer("/facet_counts/facet_ranges/views"),
        Some(&json!({
            "counts": ["0", 1, "10", 1, "20", 1, "30", 1],
            "gap": 10,
            "start": 0,
            "end": 40,
        }))
    );
    assert_matches_fixture(body, "facet_range_numeric");
}

#[tokio::test]
async fn facet_range_over_a_date_field_matches_fixture() {
    let (app, _dir) = range_app().await;
    // `%2B1DAY` is Solr date math: the params echo decodes it back to `+1DAY`.
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.range=created\
         &facet.range.start=2020-01-01T00:00:00Z&facet.range.end=2020-01-06T00:00:00Z\
         &facet.range.gap=%2B1DAY&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    // For a date field `gap`/`start`/`end` are strings, and the gap is echoed
    // verbatim as the date-math expression, not normalised.
    assert_eq!(
        body.pointer("/facet_counts/facet_ranges/created"),
        Some(&json!({
            "counts": [
                "2020-01-01T00:00:00Z", 0,
                "2020-01-02T00:00:00Z", 1,
                "2020-01-03T00:00:00Z", 2,
                "2020-01-04T00:00:00Z", 0,
                "2020-01-05T00:00:00Z", 1,
            ],
            "gap": "+1DAY",
            "start": "2020-01-01T00:00:00Z",
            "end": "2020-01-06T00:00:00Z",
        })),
        "an empty bucket in the middle of the range is still emitted, at 0"
    );
    assert_matches_fixture(body, "facet_range_date");
}

#[tokio::test]
async fn facet_range_overflowing_the_date_range_is_a_400_not_a_panic() {
    let (app, _dir) = range_app().await;

    // Both ends of the bucket walk come from the request, so a start near the
    // end of the representable range plus a day-wide gap must be answered with
    // the error envelope. Adding a `Duration` to a `time` `OffsetDateTime`
    // panics on overflow, and a panicking handler task drops the connection —
    // the client gets no response at all, not even a 500.
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.range=created\
         &facet.range.start=9999-12-31T00:00:00Z&facet.range.end=9999-12-31T12:00:00Z\
         &facet.range.gap=%2B1DAY&wt=json",
    )
    .await;

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a range walk that overflows the date range must be a 400, got {status} / {body}"
    );
    let msg = body
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("error.msg must be present, got {body}"));
    assert!(
        msg.contains("created") && msg.contains("overflows"),
        "error.msg must name the field and say what went wrong, got: {msg}"
    );
}

// --- 11. unfacetable fields are a 400, never a silent empty array ---------

#[tokio::test]
async fn facet_on_a_non_fast_field_is_a_400_not_an_empty_array() {
    let (app, _dir) = indexed_app().await;

    // `body` is text_en, indexed and stored but not `fast`, so Tantivy has no
    // column to aggregate. **Documented divergence:** real Solr answers this
    // 200 with `"body":[]` (`facet_non_docvalues_text.json`) — the silent
    // empty-counts behaviour tracer-bullet follow-up 1 calls a bug. Wayfinder
    // refuses instead.
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=body&wt=json",
    )
    .await;

    assert_facet_400(status, &body, "body", Refusal::NotFast);
}

#[tokio::test]
async fn facet_on_a_stored_only_field_is_a_400_not_an_empty_array() {
    let (app, _dir) = range_app().await;

    // `note` is stored but neither indexed nor `fast` — the case where Solr
    // itself has nothing to read, and still answers 200 with `"note":[]`
    // (`facet_stored_only_field.json`).
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=note&wt=json",
    )
    .await;

    assert_facet_400(status, &body, "note", Refusal::NotFast);
}

#[tokio::test]
async fn facet_on_an_undefined_field_is_a_400_not_an_empty_array() {
    let (app, _dir) = indexed_app().await;

    // Also a documented divergence: Solr answers 200 with
    // `"nosuchfield":[]` (`facet_unknown_field.json`).
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=nosuchfield&wt=json",
    )
    .await;

    assert_facet_400(status, &body, "nosuchfield", Refusal::Undefined);
}

#[tokio::test]
async fn facet_range_on_a_non_fast_field_is_a_400() {
    let (app, _dir) = range_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.range=note\
         &facet.range.start=0&facet.range.end=40&facet.range.gap=10&wt=json",
    )
    .await;

    assert_facet_400(status, &body, "note", Refusal::NotFast);
}

// --- 12. the facet_counts envelope ---------------------------------------

#[tokio::test]
async fn facet_counts_is_absent_unless_facet_is_true() {
    let (app, _dir) = indexed_app().await;

    // Findings fact 4: absent entirely, not present-and-empty. `facet.field`
    // alone does not turn faceting on.
    for query in [
        "select?q=*:*&rows=0&wt=json",
        "select?q=*:*&rows=0&facet.field=category&wt=json",
        "select?q=*:*&rows=0&facet=false&facet.field=category&wt=json",
    ] {
        let (status, body) = get(&app, query).await;
        assert_eq!(status, 200, "`{query}` must still be a normal query");
        assert!(
            body.get("facet_counts").is_none(),
            "`{query}` must not produce a facet_counts block, got {body}"
        );
    }
}

#[tokio::test]
async fn facet_counts_always_carries_all_five_sub_objects() {
    let (app, _dir) = indexed_app().await;

    // Findings fact 3, checked on a request that uses only facet.query — the
    // four unused sub-objects must still be there, and empty.
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.query=category:animals&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    let counts = body
        .get("facet_counts")
        .and_then(|v| v.as_object())
        .unwrap_or_else(|| panic!("facet_counts must be an object, got {body}"));
    for key in [
        "facet_queries",
        "facet_fields",
        "facet_ranges",
        "facet_intervals",
        "facet_heatmaps",
    ] {
        assert!(
            counts.contains_key(key),
            "facet_counts.{key} must always be present"
        );
    }
    assert_eq!(counts.len(), 5, "and no sixth key");
    for key in [
        "facet_fields",
        "facet_ranges",
        "facet_intervals",
        "facet_heatmaps",
    ] {
        assert_eq!(
            counts.get(key),
            Some(&json!({})),
            "unused facet_counts.{key} must be an empty object"
        );
    }
}

// --- 13. SELECT_PARAMS regression guard ----------------------------------

#[tokio::test]
async fn strict_params_accepts_every_implemented_facet_param() {
    // This is the `SELECT_PARAMS` guard: it is easy to implement a param and
    // forget to list it, and `strict_params = true` then 400s a param
    // Wayfinder actually supports. Run against the range schema so
    // `facet.range` has a `fast` numeric field to work with.
    let (app, _dir) =
        app_with_schema_and_config(RANGE_SCHEMA_TOML, "strict_params = true\n", &range_corpus())
            .await;

    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true\
         &facet.field=id&facet.query=id:r1&facet.query=id:r2\
         &facet.limit=10&facet.mincount=0&facet.sort=count&facet.missing=true\
         &facet.range=views&facet.range.start=0&facet.range.end=40&facet.range.gap=10\
         &json.nl=map&wt=json",
    )
    .await;

    // A param Wayfinder implements must never be the thing that fails: if this
    // 400s with `unknown request parameter`, the param is missing from
    // `SELECT_PARAMS`.
    let msg = body
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        !msg.contains("unknown request parameter"),
        "every implemented facet param must be in SELECT_PARAMS, got: {msg}"
    );
    assert_eq!(
        status,
        StatusCode::OK,
        "every implemented facet param must pass strict mode, got {body}"
    );
}
