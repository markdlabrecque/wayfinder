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
//!   the whole term dictionary of a **string** field, not the hit set: a query
//!   matching one document still reports every other term at 0
//!   (`facet_zero.json`, `facet_subset.json`). That is a property of Solr's
//!   own string columns — a Points-based (numeric/date) field has no term
//!   dictionary to enumerate, in Solr as much as in Wayfinder (issue #24,
//!   `facet_field_numeric_all.json`, `facet_field_string_control_subset.json`)
//!   — not a Wayfinder limitation. Those assertions are written out
//!   term-by-term as well as diffed against the fixture, because it is the one
//!   property a hardcoded zero-fill could fake against a single fixture but
//!   not across hit sets of different sizes.
//!
//! - **Unfacetable fields.** Captured Solr contradicts the issue premise here:
//!   `facet.field` on a non-docValues text field, on a stored-only field, and
//!   on a field that does not exist at all are all HTTP 200 with an empty
//!   array (`facet_non_docvalues_text.json`, `facet_stored_only_field.json`,
//!   `facet_unknown_field.json`) — the exact silent-empty-counts behaviour
//!   tracer-bullet review follow-up 1 calls a bug. Wayfinder diverges
//!   deliberately: Tantivy cannot aggregate a non-`fast` field, so it is a hard
//!   400 with the Solr error envelope. Those fixtures are therefore in
//!   the captured fixture request set (ground truth for the divergence) rather than
//!   the captured fixture request set (a target the fixture comparison suite must match).

// The `dead_code` allow for partially-used shared helpers is an inner attribute
// inside `tests/common/mod.rs`; repeating it here is a clippy error under
// `-D warnings`.
mod common;

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;

use common::{assert_matches_fixture, corpus, fixture, get, indexed_app, post_docs};

// --- local helpers ----------------------------------------------------------

/// A second core's schema, so `facet.range` gets numeric and date fields
/// without touching `common::SCHEMA_TOML` (adding a field there would rewrite
/// ground truth for every doc-returning fixture). Mirrors the `facets` core
/// the dedicated Solr capture creates for the `facet_range_*` captures:
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

/// The 4-doc corpus the dedicated Solr capture indexes into the `facets` core.
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
    /// The field is `fast` (so `check_facetable` passes) but is `Text`-kinded,
    /// and `facet.range` needs a numeric or date value (issue #33 item 5).
    TextRange,
}

impl Refusal {
    fn fragment(&self) -> &'static str {
        match self {
            Refusal::NotFast => "fast values (docValues)",
            Refusal::Undefined => "undefined field",
            Refusal::TextRange => "can not range facet on the text field",
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
async fn facet_explicit_json_nl_flat_is_alternating_and_matches_fixture() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&json.nl=flat&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let expected = fixture("select_json_nl_flat");
    assert_eq!(
        flat_facet(&body, "category"),
        flat_facet(&expected, "category"),
        "json.nl=flat must render facet buckets as Solr's alternating [term, count, ...] array"
    );
    assert_matches_fixture(body, "select_json_nl_flat");
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

// --- 14. facet.field on numeric/date fields (issue #24) --------------------
//
// Eleven fixtures captured against the same `facets` core / 4-doc corpus
// `range_app()` mirrors:
//
//     r1 views=5  created=2020-01-02T00:00:00Z note=alpha
//     r2 views=15 created=2020-01-03T00:00:00Z note=beta
//     r3 views=25 created=2020-01-03T00:00:00Z note=alpha
//     r4 views=35 created=2020-01-05T00:00:00Z
//
// The queries below are copied verbatim from the corresponding row in
// the captured fixture request set (these are `facets`-core GETs, so the
// fixture comparison suite does not pick them up).
//
// Ground truth establishes two things:
//
// 1. **The ticket premise is wrong.** Solr 9 does not enumerate a numeric or
//    date term dictionary for `facet.field`: `pint`/`pdate` are point fields
//    with no term dictionary to walk. `facet_field_string_control_subset`
//    (same container, corpus, hit set, but `facet.field=id`, a string field)
//    proves this is field-type-driven, not a broken capture — `id` *does*
//    enumerate. Wayfinder's current hit-set-only behaviour for numeric/date
//    is therefore already correct and must be pinned, not "fixed" into a
//    fabricated zero-fill.
// 2. **There is a real bug: ordering.** Solr orders numeric/date facet terms
//    by *value*, not by the rendered string. `src/facet.rs`'s
//    `facet_fields` sorts with `a.0.cmp(&b.0)` on the rendered string, which
//    is lexical: `"15"`, `"25"`, `"35"`, `"5"` — wrong for `views`.

#[tokio::test]
async fn facet_field_numeric_all_matches_fixture() {
    let (app, _dir) = range_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=views&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "facet_field_numeric_all");
}

#[tokio::test]
async fn facet_field_numeric_subset_matches_fixture() {
    let (app, _dir) = range_app().await;
    let (status, body) = get(
        &app,
        "select?q=id:r1&rows=0&facet=true&facet.field=views&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "facet_field_numeric_subset");
}

#[tokio::test]
async fn facet_field_date_all_matches_fixture() {
    let (app, _dir) = range_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=created&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "facet_field_date_all");
}

#[tokio::test]
async fn facet_field_date_subset_matches_fixture() {
    let (app, _dir) = range_app().await;
    let (status, body) = get(
        &app,
        "select?q=id:r1&rows=0&facet=true&facet.field=created&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "facet_field_date_subset");
}

#[tokio::test]
async fn facet_field_numeric_sort_index_matches_fixture() {
    let (app, _dir) = range_app().await;
    let (status, body) = get(
        &app,
        "select?q=id:r1&rows=0&facet=true&facet.field=views&facet.sort=index&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "facet_field_numeric_sort_index");
}

#[tokio::test]
async fn facet_field_numeric_sort_count_matches_fixture() {
    let (app, _dir) = range_app().await;
    let (status, body) = get(
        &app,
        "select?q=id:r1&rows=0&facet=true&facet.field=views&facet.sort=count&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "facet_field_numeric_sort_count");
}

#[tokio::test]
async fn facet_field_numeric_sort_index_all_matches_fixture() {
    let (app, _dir) = range_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=views&facet.sort=index&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    // Value order (5, 15, 25, 35), not lexical order (15, 25, 35, 5) — the
    // ordering bug this issue exists to pin.
    assert_eq!(
        flat_facet(&body, "views"),
        expect_flat(&[
            (Some("5"), 1),
            (Some("15"), 1),
            (Some("25"), 1),
            (Some("35"), 1),
        ]),
        "facet.sort=index on a numeric field orders by value, not by the rendered string"
    );
    assert_matches_fixture(body, "facet_field_numeric_sort_index_all");
}

#[tokio::test]
async fn facet_field_date_sort_index_all_matches_fixture() {
    let (app, _dir) = range_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=created&facet.sort=index&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    // Chronological order. Unlike `views`, lexical ISO-8601 string order
    // happens to agree with chronological order here, so this assertion by
    // itself does not discriminate a lexical-vs-value ordering bug for
    // dates — it is still asserted, per the fixture, but the numeric
    // assertions above are the ones that actually separate the two.
    assert_eq!(
        flat_facet(&body, "created"),
        expect_flat(&[
            (Some("2020-01-02T00:00:00Z"), 1),
            (Some("2020-01-03T00:00:00Z"), 2),
            (Some("2020-01-05T00:00:00Z"), 1),
        ])
    );
    assert_matches_fixture(body, "facet_field_date_sort_index_all");
}

#[tokio::test]
async fn facet_field_numeric_mincount_one_matches_fixture() {
    let (app, _dir) = range_app().await;
    let (status, body) = get(
        &app,
        "select?q=id:r1&rows=0&facet=true&facet.field=views&facet.mincount=1&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "facet_field_numeric_mincount_one");
}

#[tokio::test]
async fn facet_field_numeric_json_nl_map_matches_fixture() {
    let (app, _dir) = range_app().await;
    let (status, body) = get(
        &app,
        "select?q=id:r1&rows=0&facet=true&facet.field=views&json.nl=map&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "facet_field_numeric_json_nl_map");
}

#[tokio::test]
async fn facet_field_string_control_subset_matches_fixture() {
    // The control: on the *same* core, corpus and hit set as the numeric/date
    // cases above, `facet.field` on a string field still enumerates the whole
    // dictionary (`r2`/`r3`/`r4` at 0) — proof that the absent-zero-fill
    // behaviour above is field-type-driven, not a broken capture, and the
    // regression guard that a fix to numeric/date ordering must not disable
    // string term-dictionary enumeration.
    let (app, _dir) = range_app().await;
    let (status, body) = get(
        &app,
        "select?q=id:r1&rows=0&facet=true&facet.field=id&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        flat_facet(&body, "id"),
        expect_flat(&[
            (Some("r1"), 1),
            (Some("r2"), 0),
            (Some("r3"), 0),
            (Some("r4"), 0),
        ]),
        "a string field must still zero-fill terms outside the hit set"
    );
    assert_matches_fixture(body, "facet_field_string_control_subset");
}

// --- 15. no fabricated zero-fill for numeric/date, at two hit-set sizes ----
//
// Written out value-by-value (not only diffed against a fixture) because it
// is the one property a fixture-shaped implementation could fake against a
// single hit set but not across hit sets of different sizes — mirroring
// section 2's treatment of the string dictionary-enumeration property.

#[tokio::test]
async fn numeric_facet_field_has_no_fabricated_zero_fill_one_hit() {
    let (app, _dir) = range_app().await;
    let (status, body) = get(
        &app,
        "select?q=id:r1&rows=0&facet=true&facet.field=views&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["response"]["numFound"], 1, "hit set is one doc");
    let flat = flat_facet(&body, "views");
    assert_eq!(
        flat,
        expect_flat(&[(Some("5"), 1)]),
        "views must contain exactly the one observed term, got {flat:?}"
    );
    assert!(
        !flat.contains(&json!("15"))
            && !flat.contains(&json!("25"))
            && !flat.contains(&json!("35")),
        "15/25/35 are reachable only through non-matching documents and must be absent \
         entirely, not present at 0 — got {flat:?}"
    );
}

#[tokio::test]
async fn numeric_facet_field_has_no_fabricated_zero_fill_two_hits() {
    // A second hit-set size, derived from the corpus rather than a fixture
    // (no fixture was captured for this exact query): `id:r1 OR id:r3`
    // matches r1 (views=5) and r3 (views=25), so an implementation that
    // hardcoded the one-hit shape above cannot also satisfy this one.
    let (app, _dir) = range_app().await;
    let (status, body) = get(
        &app,
        "select?q=id:r1%20OR%20id:r3&rows=0&facet=true&facet.field=views&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["response"]["numFound"], 2, "hit set is two docs");
    let flat = flat_facet(&body, "views");
    // Membership, not order: which terms are present/absent is the property
    // under test here, and is independent of the ordering bug pinned by
    // `facet_field_numeric_sort_index_all_matches_fixture` et al.
    assert_eq!(
        flat.len(),
        4,
        "exactly two terms (four flat entries: term, count, term, count), got {flat:?}"
    );
    assert!(
        flat.contains(&json!("5")) && flat.contains(&json!("25")),
        "the two observed terms must both be present, got {flat:?}"
    );
    assert!(
        !flat.contains(&json!("15")) && !flat.contains(&json!("35")),
        "15/35 belong to documents outside this hit set and must be absent entirely, \
         got {flat:?}"
    );
    for count in [flat[1].clone(), flat[3].clone()] {
        assert_eq!(
            count,
            json!(1),
            "each observed term has count 1, got {flat:?}"
        );
    }
}

#[tokio::test]
async fn date_facet_field_has_no_fabricated_zero_fill_one_hit() {
    let (app, _dir) = range_app().await;
    let (status, body) = get(
        &app,
        "select?q=id:r1&rows=0&facet=true&facet.field=created&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["response"]["numFound"], 1, "hit set is one doc");
    let flat = flat_facet(&body, "created");
    assert_eq!(
        flat,
        expect_flat(&[(Some("2020-01-02T00:00:00Z"), 1)]),
        "created must contain exactly the one observed date, got {flat:?}"
    );
    assert!(
        !flat.contains(&json!("2020-01-03T00:00:00Z"))
            && !flat.contains(&json!("2020-01-05T00:00:00Z")),
        "2020-01-03/2020-01-05 belong to documents outside this hit set and must be \
         absent entirely, got {flat:?}"
    );
}

#[tokio::test]
async fn date_facet_field_counts_the_hit_set_not_the_whole_corpus() {
    // `2020-01-03T00:00:00Z` has a corpus-wide count of 2 (r2 + r3). This
    // query's hit set contains only r3, so the count for that same date must
    // be 1, not the corpus-wide 2 — proof that even a term that *is* present
    // is counted from the hit set, not fabricated from the dictionary.
    let (app, _dir) = range_app().await;
    let (status, body) = get(
        &app,
        "select?q=id:r1%20OR%20id:r3&rows=0&facet=true&facet.field=created&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["response"]["numFound"], 2, "hit set is two docs");
    assert_eq!(
        flat_facet(&body, "created"),
        expect_flat(&[
            (Some("2020-01-02T00:00:00Z"), 1),
            (Some("2020-01-03T00:00:00Z"), 1),
        ]),
        "2020-01-03T00:00:00Z must be 1 (hit-set count), not 2 (corpus-wide count)"
    );
}

#[tokio::test]
async fn date_facet_field_omits_a_dictionary_value_entirely_outside_the_hit_set() {
    // r1 (2020-01-02) and r4 (2020-01-05) match; r2/r3 (2020-01-03, corpus
    // count 2) do not. `2020-01-03T00:00:00Z` must be wholly absent, not
    // present at 0 and not present at its corpus-wide count.
    let (app, _dir) = range_app().await;
    let (status, body) = get(
        &app,
        "select?q=id:r1%20OR%20id:r4&rows=0&facet=true&facet.field=created&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["response"]["numFound"], 2, "hit set is two docs");
    let flat = flat_facet(&body, "created");
    assert_eq!(
        flat,
        expect_flat(&[
            (Some("2020-01-02T00:00:00Z"), 1),
            (Some("2020-01-05T00:00:00Z"), 1),
        ]),
        "created must contain exactly the two observed dates, got {flat:?}"
    );
    assert!(
        !flat.contains(&json!("2020-01-03T00:00:00Z")),
        "2020-01-03T00:00:00Z must be absent entirely, not present at 0 — got {flat:?}"
    );
}

// --- 16. the date facet key must be Solr's RFC3339 string, not a raw i64 --

#[tokio::test]
async fn date_facet_field_key_is_rendered_as_solr_rfc3339_not_a_raw_i64() {
    // Tantivy's date fast-field column is i64 nanoseconds since the epoch.
    // `CoreIndex::term_facet` renders keys from the aggregation bucket's
    // `key_as_string`/`Key`; if the date branch falls through to the
    // generic `Key::I64`/`Key::U64` arm the key comes out as a nanosecond
    // count, not Solr's `2020-01-02T00:00:00Z`.
    let (app, _dir) = range_app().await;
    let (status, body) = get(
        &app,
        "select?q=id:r1&rows=0&facet=true&facet.field=created&wt=json",
    )
    .await;

    assert_eq!(status, 200);
    let flat = flat_facet(&body, "created");
    assert_eq!(
        flat.len(),
        2,
        "exactly one term for a one-hit query, got {flat:?}"
    );
    assert_eq!(
        flat[0],
        json!("2020-01-02T00:00:00Z"),
        "the key must be Solr's exact RFC3339 form — not an i64, not a nanosecond count, \
         and not an offset form like `+00:00` — got {flat:?}"
    );
    assert_eq!(flat[1], json!(1));
}

// --- 17. issue #33 facet-debt schema + corpus (error precedence, float/date
//     rendering, unpinned semantics) -------------------------------------
//
// A third local schema, separate from `common::SCHEMA_TOML` and
// `RANGE_SCHEMA_TOML`: the debt items need field types neither of those
// carries (`pdouble`, `pfloat`, a millisecond-precision `pdate`, plus a
// stored-only field to combine with a broken `facet.query`). Mirrors the
// `facets33` core / 5-doc corpus in the dedicated Solr capture's issue-33 block
// exactly (see the task spec's "Ground truth" section): `views` (Solr
// `pint`) -> `int`, `price` (`pdouble`) -> `double`, `rating` (`pfloat`) ->
// `float`, `stamp` (`pdate`, millisecond values) -> `date`, `tag` (`string`,
// docValues, absent from r4/r5) -> `string` with `fast = true`, `note`
// (`string`, stored-only) -> `string` with no `fast`. `id`/`body` are not in
// the captured corpus but are needed for `unique_key`/`default_field`.
const DEBT_SCHEMA_TOML: &str = r#"
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
name = "price"
type = "double"
stored = true
fast = true

[[fields]]
name = "rating"
type = "float"
stored = true
fast = true

[[fields]]
name = "stamp"
type = "date"
stored = true
fast = true

[[fields]]
name = "tag"
type = "string"
stored = true
fast = true

[[fields]]
name = "note"
type = "string"
stored = true
"#;

/// The 5-doc corpus the dedicated Solr capture's issue-33 block indexes into `facets33`.
fn debt_corpus() -> Value {
    json!([
        {"id":"r1","views":5, "price":5.0, "rating":5.0,
         "stamp":"2020-01-02T00:00:00.123Z","tag":"apple","note":"alpha"},
        {"id":"r2","views":15,"price":7.5, "rating":7.5,
         "stamp":"2020-01-02T00:00:00.456Z","tag":"apple"},
        {"id":"r3","views":25,"price":5.0, "rating":5.0,
         "stamp":"2020-01-03T12:34:56.789Z","tag":"banana"},
        {"id":"r4","views":35,"price":12.0,"stamp":"2020-01-05T00:00:00Z"},
        {"id":"r5","views":45,"price":0.25}
    ])
}

/// Builds an app on `DEBT_SCHEMA_TOML` and indexes `debt_corpus()`.
async fn debt_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), DEBT_SCHEMA_TOML).expect("app must build");
    let (status, body) = post_docs(&app, &debt_corpus()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the facet-debt corpus must succeed, got {body}"
    );
    (app, dir)
}

/// The class of a facet error, read out of its `error.msg` — independent of
/// exact wording on either side (Solr's or Wayfinder's own), so a fixture
/// decides *which family's error* won a precedence contest without freezing
/// either engine's phrasing. Mirrors `tests/sort.rs::sort_error_class`
/// (read-only reference, not modified here).
///
/// `Other` is not one of the three named families — it exists so an
/// unrelated error (e.g. Wayfinder's own "not fast" refusal for an
/// unfacetable field, which is not any of Solr's three broken-param
/// families) still compares *unequal* to whichever family the fixture
/// expects, carrying the real message so a failing assertion shows exactly
/// what Wayfinder said instead of panicking blind.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FacetErrorClass {
    /// Solr: "Unable to range facet on field:...". Wayfinder: "can not range
    /// facet on the text field ...".
    RangeUnfacetable,
    /// Solr: "org.apache.solr.search.SyntaxError: ...". Wayfinder: "could not
    /// parse query ...".
    QuerySyntax,
    /// Solr: 'undefined field: "...."'. Wayfinder: "can not facet on
    /// undefined field: ...".
    UndefinedField,
    /// Anything else, carrying the message verbatim.
    Other(String),
}

fn facet_error_class(msg: &str) -> FacetErrorClass {
    if msg.contains("SyntaxError") || msg.contains("could not parse query") {
        FacetErrorClass::QuerySyntax
    } else if msg.contains("Unable to range facet") || msg.contains("can not range facet") {
        FacetErrorClass::RangeUnfacetable
    } else if msg.contains("undefined field") {
        FacetErrorClass::UndefinedField
    } else {
        FacetErrorClass::Other(msg.to_string())
    }
}

/// Asserts a facet error is a 400 with `error.code: 400` (finding 10), and
/// that its error class (per `facet_error_class`) equals the *fixture's*
/// error class — i.e. the same family of broken param answered the request
/// on both engines, without pinning either engine's exact wording.
fn assert_facet_error_class(status: StatusCode, body: &Value, fixture_name: &str) {
    let expected = fixture(fixture_name);
    let want_msg = expected
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("fixture {fixture_name} has no error.msg"));
    let want_class = facet_error_class(want_msg);

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a broken facet param must be a 400 ({fixture_name}), got {status} / {body}"
    );
    assert_eq!(
        body.pointer("/error/code").and_then(Value::as_i64),
        Some(400),
        "error.code must be 400 ({fixture_name}), got {body}"
    );
    let got_msg = body
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("response has no error.msg, got {body}"));
    let got_class = facet_error_class(got_msg);
    assert_eq!(
        got_class, want_class,
        "error class must match the fixture `{fixture_name}` \
         (Solr said: `{want_msg}`; Wayfinder said: `{got_msg}`)"
    );
}

// --- 18. error precedence among broken facet params (#30, finding 38) -----
//
// Solr reports exactly one error when multiple facet params are broken at
// once, in the precedence order facet.range > facet.query > facet.field.
// Wayfinder's `facet_counts` currently computes `facet_fields` first
// (`src/facet.rs` line ~83), then `facet_queries`, then `facet_ranges` inside
// the `json!` macro — fields -> queries -> ranges, exactly backwards. The
// combo tests below are red today for that reason and must turn green once
// evaluation is reordered to ranges -> queries -> fields (the *emitted key
// order* of `facet_counts` — queries, fields, ranges, intervals, heatmaps —
// must not change; that is a separate, already-tested contract in
// `tests/json_key_order.rs`, read-only here).

#[tokio::test]
async fn facet_query_single_error_is_query_syntax_class() {
    let (app, _dir) = debt_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.query=views:[bad&wt=json",
    )
    .await;
    assert_facet_error_class(status, &body, "facet_err_query_single");
}

#[tokio::test]
async fn facet_field_single_error_is_undefined_field_class() {
    let (app, _dir) = debt_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=nosuchfield&wt=json",
    )
    .await;
    assert_facet_error_class(status, &body, "facet_err_field_single");
}

#[tokio::test]
async fn facet_range_single_error_is_range_unfacetable_class() {
    let (app, _dir) = debt_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.range=tag\
         &facet.range.start=0&facet.range.end=10&facet.range.gap=5&wt=json",
    )
    .await;
    assert_facet_error_class(status, &body, "facet_err_range_single");
}

#[tokio::test]
async fn query_and_field_broken_together_the_query_error_wins() {
    // Today: fields are evaluated before queries, so Wayfinder answers the
    // *field* (undefined-field) error here — the fixture says the query's
    // SyntaxError wins. Red until the reorder.
    let (app, _dir) = debt_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true\
         &facet.query=views:[bad&facet.field=nosuchfield&wt=json",
    )
    .await;
    assert_facet_error_class(status, &body, "facet_err_query_field");
}

#[tokio::test]
async fn query_and_range_broken_together_the_range_error_wins() {
    // Today: queries are evaluated before ranges, so Wayfinder answers the
    // query's SyntaxError — the fixture says the range error wins. Red until
    // the reorder.
    let (app, _dir) = debt_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true\
         &facet.query=views:[bad&facet.range=tag\
         &facet.range.start=0&facet.range.end=10&facet.range.gap=5&wt=json",
    )
    .await;
    assert_facet_error_class(status, &body, "facet_err_query_range");
}

#[tokio::test]
async fn field_and_range_broken_together_the_range_error_wins() {
    // Today: fields are evaluated before ranges, so Wayfinder answers the
    // undefined-field error — the fixture says the range error wins. Red
    // until the reorder.
    let (app, _dir) = debt_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true\
         &facet.field=nosuchfield&facet.range=tag\
         &facet.range.start=0&facet.range.end=10&facet.range.gap=5&wt=json",
    )
    .await;
    assert_facet_error_class(status, &body, "facet_err_field_range");
}

#[tokio::test]
async fn all_three_broken_together_the_range_error_wins() {
    // Today: fields are evaluated first, so Wayfinder answers the
    // undefined-field error — the fixture says the range error wins even
    // with all three broken at once. Red until the reorder.
    let (app, _dir) = debt_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true\
         &facet.query=views:[bad&facet.field=nosuchfield&facet.range=tag\
         &facet.range.start=0&facet.range.end=10&facet.range.gap=5&wt=json",
    )
    .await;
    assert_facet_error_class(status, &body, "facet_err_all_three");
}

#[tokio::test]
async fn invalid_query_beats_an_unfacetable_field_the_hash_30_shape() {
    // The #30 shape verbatim: an invalid facet.query plus a stored-only
    // (unfacetable-in-Wayfinder, by ratified divergence 2) facet.field=note.
    // In Solr the field half is not an error at all — it is Wayfinder that
    // makes the field half a 400 too — but with `range > query > field`
    // precedence Wayfinder must still answer with the *query* error, because
    // the query is evaluated before the field.
    //
    // Today fields are evaluated before queries, so Wayfinder answers its own
    // "not fast" refusal for `note` — a `FacetErrorClass::Other`, which
    // cannot equal `QuerySyntax` — making this red for the same underlying
    // reason as the other combos, verified via the class comparison rather
    // than assumed.
    let (app, _dir) = debt_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.query=views:[bad&facet.field=note&wt=json",
    )
    .await;
    assert_facet_error_class(status, &body, "facet_err_query_vs_unfacetable");
}

// --- 19. pdouble/pfloat facet keys render as Java Double.toString (finding
//     39): an integral double is "5.0", never "5" -----------------------
//
// `CoreIndex::term_facet` renders an `F64` aggregation key via
// `v.to_string()` (`src/core_index.rs`, the `(None, Key::F64(v))` arm),
// which is Rust's formatting, not Java's — and Tantivy normalises an
// exactly-integral double to a `U64`/`I64` *key variant* besides, so the fix
// has to come from the schema's declared `ValueKind::F64`, not from the
// aggregation key's variant or value. Every test below is red today because
// of that `"5"` vs `"5.0"` rendering, not because of ordering — the ordering
// itself is already correct (`FacetOrderKey::cmp`'s mismatched-variant
// fallback in `src/core_index.rs` already normalises U64/I64/F64 keys to the
// same `f64` for comparison).

#[tokio::test]
async fn facet_field_double_all_matches_fixture() {
    let (app, _dir) = debt_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=price&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "facet_field_double_all");
}

#[tokio::test]
async fn facet_field_double_subset_matches_fixture() {
    let (app, _dir) = debt_app().await;
    let (status, body) = get(
        &app,
        "select?q=id:r1&rows=0&facet=true&facet.field=price&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "facet_field_double_subset");
}

#[tokio::test]
async fn facet_field_double_sort_index_all_matches_fixture() {
    let (app, _dir) = debt_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=price&facet.sort=index&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    // Value order throughout: 0.25 < 5.0 < 7.5 < 12.0 (finding 39) — already
    // correct; only the rendered keys are wrong today.
    assert_eq!(
        flat_facet(&body, "price"),
        expect_flat(&[
            (Some("0.25"), 1),
            (Some("5.0"), 2),
            (Some("7.5"), 1),
            (Some("12.0"), 1),
        ]),
        "facet.sort=index on a pdouble field orders by value, rendered as Double.toString"
    );
    assert_matches_fixture(body, "facet_field_double_sort_index_all");
}

#[tokio::test]
async fn facet_field_float_all_matches_fixture() {
    let (app, _dir) = debt_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=rating&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "facet_field_float_all");
}

#[tokio::test]
async fn integral_double_renders_with_a_trailing_zero_but_the_same_int_value_does_not() {
    // The regression a value-sniffing fix would reintroduce: r1's `views` is
    // the int `5` and its `price` is the double `5.0` — the *same* number,
    // two different schema kinds. `views` must keep rendering `"5"` (already
    // correct, pinned by `facet_field_numeric_all.json` via `range_app`'s
    // `views`, and re-checked here on the debt schema's own `views`); `price`
    // must render `"5.0"` (`facet_field_double_subset.json`) — the fix must
    // be driven by `ValueKind::F64`, not by whether the value happens to be
    // integral.
    let (app, _dir) = debt_app().await;
    let (status, body) = get(
        &app,
        "select?q=id:r1&rows=0&facet=true&facet.field=views&facet.field=price&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        flat_facet(&body, "views"),
        expect_flat(&[(Some("5"), 1)]),
        "an int field renders an integral value with no trailing `.0`"
    );
    assert_eq!(
        flat_facet(&body, "price"),
        expect_flat(&[(Some("5.0"), 1)]),
        "a double field renders the SAME value `5` as `5.0` — schema-driven, not value-driven"
    );
}

// --- 20. millisecond-precision pdate facet (finding 40) --------------------
//
// Wayfinder's fast date column is `DateOptions::default()` = seconds
// precision (`src/schema.rs` ~line 494). `stamp` carries two values inside
// the same second (`r1` at `.123`, `r2` at `.456`) so seconds precision
// collapses them into one bucket of count 2 at the truncated
// `"2020-01-02T00:00:00Z"`, and even the *subset* case (a single doc) still
// renders wrong because the fraction is truncated away entirely — every test
// below is red for that reason, not for ordering (which the fixtures show is
// already chronological once the values are distinct).
//
// The companion "sort key becomes exact nanoseconds" half of finding 40
// (`FacetOrderKey`, `src/core_index.rs`) is not separately unit-tested here:
// `FacetOrderKey` is `pub` but `core_index` is a private module with no
// `pub use` re-export in `src/lib.rs`, so it is unreachable from an
// integration test — the ms-ordering fixture tests below are the coverage
// for it, per the task spec's own fallback instruction.

#[tokio::test]
async fn facet_field_date_ms_all_matches_fixture() {
    let (app, _dir) = debt_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=stamp&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "facet_field_date_ms_all");
}

#[tokio::test]
async fn facet_field_date_ms_subset_matches_fixture() {
    let (app, _dir) = debt_app().await;
    let (status, body) = get(
        &app,
        "select?q=id:r1&rows=0&facet=true&facet.field=stamp&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "facet_field_date_ms_subset");
}

#[tokio::test]
async fn facet_field_date_ms_sort_index_all_matches_fixture() {
    let (app, _dir) = debt_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=stamp&facet.sort=index&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "facet_field_date_ms_sort_index_all");
}

// --- 21. previously-unpinned facet semantics (finding 41) ------------------

// 41(a): facet.missing is exempt from both facet.limit and facet.mincount.
// Wayfinder already matches (the null bucket is appended after `truncate`
// and is never subject to the `mincount` `retain`) — green from birth.

#[tokio::test]
async fn facet_missing_survives_a_limit_that_would_otherwise_drop_it() {
    let (app, _dir) = debt_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=tag\
         &facet.missing=true&facet.limit=1&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "facet_missing_with_limit");
}

#[tokio::test]
async fn facet_missing_survives_a_mincount_at_its_own_count() {
    let (app, _dir) = debt_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=tag\
         &facet.missing=true&facet.mincount=2&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "facet_missing_with_mincount_two");
}

#[tokio::test]
async fn facet_missing_survives_a_mincount_above_its_own_count() {
    let (app, _dir) = debt_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=tag\
         &facet.missing=true&facet.mincount=3&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_matches_fixture(body, "facet_missing_with_mincount_three");
}

// 41(b): a facet.range span not divisible by the gap extends the last bucket
// to the gap boundary (Wayfinder already does — `range_buckets`'s I64 walk
// always steps by a full `gap`), but echoes the *gap-aligned* end (30), not
// the requested one (22). Wayfinder's `echo_bound` echoes the requested
// `end` verbatim — red.

#[tokio::test]
async fn facet_range_not_divisible_by_gap_echoes_the_gap_aligned_end() {
    let (app, _dir) = debt_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.range=views\
         &facet.range.start=0&facet.range.end=22&facet.range.gap=10&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    // The bucket walk itself is already right (`[20,30)` counts r3's 25);
    // written out explicitly because it is the fixture's least obvious part.
    assert_eq!(
        body.pointer("/facet_counts/facet_ranges/views"),
        Some(&json!({
            "counts": ["0", 1, "10", 1, "20", 1],
            "gap": 10,
            "start": 0,
            "end": 30,
        })),
        "end must echo the gap-aligned boundary (30), not the requested value (22)"
    );
    assert_matches_fixture(body, "facet_range_end_not_gap_aligned");
}

// 41(c): json.nl=arrarr / json.nl=arrmap. `render_buckets` only special-cases
// `json.nl=map`; any other value (including `arrarr`/`arrmap`) falls through
// to the flat alternating array today — red.

#[tokio::test]
async fn json_nl_arrarr_renders_each_bucket_as_a_two_element_array() {
    let (app, _dir) = debt_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=tag&json.nl=arrarr&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        body.pointer("/facet_counts/facet_fields/tag"),
        Some(&json!([["apple", 2], ["banana", 1]]))
    );
    assert_matches_fixture(body, "facet_json_nl_arrarr");
}

#[tokio::test]
async fn json_nl_arrmap_renders_each_bucket_as_a_one_entry_object() {
    let (app, _dir) = debt_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=tag&json.nl=arrmap&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        body.pointer("/facet_counts/facet_fields/tag"),
        Some(&json!([{"apple": 2}, {"banana": 1}]))
    );
    assert_matches_fixture(body, "facet_json_nl_arrmap");
}

#[tokio::test]
async fn json_nl_arrarr_applies_to_facet_ranges_counts_too() {
    // No fixture covers `facet.range` under `json.nl=arrarr` (the capture
    // only exercised `arrarr`/`arrmap` on `facet.field=tag`) — this test is
    // Wayfinder-only, asserting the same nested-pair shape `json.nl=map`
    // already gets for `facet_ranges.<field>.counts`
    // (`json_nl_map_switches_range_counts_to_an_object`,
    // `facet_range_json_nl_map.json`), with `gap`/`start`/`end` left as plain
    // numbers either way. Red today for the same reason as the
    // `facet.field` case: `render_buckets` does not special-case `arrarr`.
    let (app, _dir) = range_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.range=views\
         &facet.range.start=0&facet.range.end=40&facet.range.gap=10&json.nl=arrarr&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        body.pointer("/facet_counts/facet_ranges/views"),
        Some(&json!({
            "counts": [["0", 1], ["10", 1], ["20", 1], ["30", 1]],
            "gap": 10,
            "start": 0,
            "end": 40,
        })),
        "json.nl=arrarr must nest facet_ranges counts the same way it nests facet_fields, \
         leaving gap/start/end untouched"
    );
}

// 41(d): json.nl=map + facet.missing keys the null bucket as the empty
// string. Wayfinder already does this (`render_buckets`'s
// `term.clone().unwrap_or_default()`) — green from birth.

#[tokio::test]
async fn json_nl_map_keys_the_missing_bucket_as_the_empty_string() {
    let (app, _dir) = debt_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=tag\
         &facet.missing=true&json.nl=map&wt=json",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        body.pointer("/facet_counts/facet_fields/tag"),
        Some(&json!({"apple": 2, "banana": 1, "": 2}))
    );
    assert_matches_fixture(body, "facet_json_nl_map_missing");
}

// --- 22. ValueKind::Text range-facet bail is reachable (issue #33 item 5) --
//
// `src/facet.rs`'s `ValueKind::Text` bail in `range_buckets` is unreachable
// via a non-`fast` text field (`check_facetable` refuses it first, with a
// different message — `Refusal::NotFast`). `tag` is exactly a `fast` string
// field, so `facet.range=tag` passes `check_facetable` and reaches the
// `Text` bail. Already implemented — green from birth. Solr's counterpart
// (`facet_err_range_single.json`) is also a 400, in the same
// `RangeUnfacetable` class (finding 38) but with different wording — the
// wording is not frozen here, only Wayfinder's own fragment is (per the
// `Refusal` pattern above), and `facet_range_single_error_is_range_unfacetable_class`
// in section 18 already pins the cross-engine class match.

#[tokio::test]
async fn facet_range_on_a_fast_text_field_is_a_400_with_wayfinders_own_message() {
    let (app, _dir) = debt_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.range=tag\
         &facet.range.start=0&facet.range.end=10&facet.range.gap=5&wt=json",
    )
    .await;
    assert_facet_400(status, &body, "tag", Refusal::TextRange);
}

// --- 23. facet.query/facet.field errors carry the base query's `response`
//     block (issue #35) ------------------------------------------------------
//
// Solr detects a `facet.range` error before running the base query at all, so
// its error response has no `response` key (`facet_err_range_single.json`,
// covered by section 18/22 above — unaffected by this issue). A
// `facet.query`/`facet.field` error is detected *after* the base query has
// already run, so Solr's fixture carries the base query's real `response`
// block (`numFound`/`start`/`numFoundExact`/`docs`) alongside `error`
// (`facet_unknown_field.json`, `facet_err_query_single.json`,
// `facet_err_field_single.json`). Wayfinder's `select` handler currently
// `?`-propagates the facet error before the `response` block is ever built,
// so it is missing entirely — these are red until issue #35 lands.

/// Asserts the base query's real `response` block is present alongside
/// `error`, matching `expect_num_found` (never zeroed out just because the
/// request errored) — the exact shape Solr's fixtures for a post-query facet
/// failure carry.
fn assert_response_block_alongside_error(body: &Value, expect_num_found: i64) {
    assert_eq!(
        body.pointer("/response/numFound").and_then(Value::as_i64),
        Some(expect_num_found),
        "response.numFound must be the base query's real count, not zeroed out \
         because faceting failed; got {body}"
    );
    assert_eq!(
        body.pointer("/response/start").and_then(Value::as_i64),
        Some(0),
        "response.start must be present, got {body}"
    );
    assert_eq!(
        body.pointer("/response/numFoundExact")
            .and_then(Value::as_bool),
        Some(true),
        "response.numFoundExact must be present, got {body}"
    );
    assert_eq!(
        body.pointer("/response/docs")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(0),
        "response.docs must be an empty array (rows=0), got {body}"
    );
    assert!(
        body.get("error").is_some(),
        "the error block must still be present alongside response, got {body}"
    );
}

#[tokio::test]
async fn facet_field_error_still_carries_the_base_querys_response_block() {
    // Mirrors `facet_unknown_field.json`'s exact request against the same
    // 5-doc corpus the fixture was captured against.
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=nosuchfield&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_response_block_alongside_error(&body, 5);
}

#[tokio::test]
async fn facet_query_error_still_carries_the_base_querys_response_block() {
    // Mirrors `facet_err_query_single.json`'s exact request against
    // `debt_app()`'s 5-doc corpus, which the fixture was captured against.
    let (app, _dir) = debt_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.query=views:[bad&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_response_block_alongside_error(&body, 5);
}
