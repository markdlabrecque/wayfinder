//! `json.facet` — the JSON Facet API (issue #343).
//!
//! Every expected *value* here is either lifted straight from a committed
//! `jf343_*.json` fixture under `solr-ref/responses/` (loaded through
//! `common::fixture` and compared by pointer against `body["facets"]` /
//! `body["facet_counts"]` / `body["stats"]`, never against whatever
//! Wayfinder happens to produce), or computed from the real corpus/index
//! this file builds (the `_version_` cases — see below).
//!
//! This suite deliberately does **not** use `common::assert_matches_fixture`
//! for whole-envelope comparison: `responseHeader.params` would have to
//! byte-for-byte match Solarium's own JSON serialisation of `json.facet`
//! (key order, whitespace) for that to pass, which is not a wire contract
//! (`tests/json_key_order.rs`'s module docs — `responseHeader.params` order
//! is Java `HashMap` iteration order, not reproducible, and the same is true
//! of Solarium's own string formatting on the request side). Instead each
//! test builds its own `json.facet` JSON (via `serde_json::json!` + a local
//! percent-encoder — the JSON `{`, `}`, `"`, `:` characters are not legal raw
//! query-string bytes, and `src/params.rs::decode` needs percent-encoding,
//! not `+`-for-space form-encoding, since a facet JSON string can itself
//! contain the literal `+` `%` bytes `src/params.rs` treats specially) and
//! compares the resulting `facets` / `facet_counts` / `stats` subtree
//! against the fixture's own subtree.
//!
//! The corpus below (`jf_corpus`) is deliberately chosen so its `popularity`
//! values are exactly Solr's captured set `{10, 20, 30, 40, 50, 60}` handed
//! out to different documents than the original capture — every classic-facet
//! and `stats` number in `jf343_with_classic_stats.json` (sum 210, mean 35,
//! sumOfSquares 9100, stddev ~18.708) matches byte-for-byte, which is a good
//! sign the corpus really does mirror the captured one and not just its
//! shape.
//!
//! Top-level key order (`facet_counts, facets, stats`) and the `facets`
//! sub-object order are pinned separately in `tests/json_key_order.rs`,
//! which reads raw response *text* rather than a parsed `Value` — the only
//! way to see key order at all (see that file's module docs).

mod common;

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use tantivy::Index;
use tempfile::TempDir;

use common::{fixture, get, post_docs};

// --- schema + corpus --------------------------------------------------------
//
// Mirrors `tests/faceting.rs`'s `RANGE_SCHEMA_TOML`/`range_corpus` pattern:
// a schema local to this file rather than touching `common::SCHEMA_TOML`,
// which would rewrite ground truth for every other fixture in the suite.
//
// Fields per the spec's client trace (§1a): `hash`, `index_id`,
// `ss_search_api_datasource` (all string/fast — real docValues columns a
// terms facet can run over), `popularity` (int/fast, the aggregation
// field), `body` (text_en, stored but *not* fast — the field the two
// deliberate-divergence fixtures `jf343_err_no_docvalues` /
// `jf343_err_agg_text` use).
const JF_SCHEMA_TOML: &str = r#"
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
name = "hash"
type = "string"
stored = true
fast = true

[[fields]]
name = "index_id"
type = "string"
stored = true
fast = true

[[fields]]
name = "ss_search_api_datasource"
type = "string"
stored = true
fast = true

[[fields]]
name = "popularity"
type = "int"
stored = true
fast = true
"#;

/// The 6-doc corpus behind every `jf343_*` assertion in this file.
///
/// - `hash`: siteA -> jf1..jf4 (count 4), siteB -> jf5 (count 1); jf6 has no
///   `hash` at all (mirrors `jf343_terms.json`'s `siteA:4, siteB:1` summing
///   to 5, one short of `numFound:6`).
/// - `index_id` under siteA: index_a -> jf1..jf3 (count 3), index_b -> jf4
///   (count 1); under siteB: index_c -> jf5 (count 1). Mirrors
///   `jf343_terms_sort_index.json` (`index_a:3, index_b:1, index_c:1`).
/// - `ss_search_api_datasource` under index_a: entity:node -> jf1, jf2
///   (count 2), entity:user -> jf3 (count 1); under index_b: entity:node ->
///   jf4; under index_c: entity:node -> jf5. Mirrors `jf343_deep_max.json`.
/// - `popularity`: 10, 30, 20, 40, 50, 60 — i.e. exactly `{10..60 step 10}`
///   handed to jf1..jf6 respectively, so `max(popularity)` over the whole
///   corpus is 60 (jf6, which has no `hash`/`index_id`/`ss_search_api_datasource`
///   — proving the top-level aggregation still covers every document, not
///   just the ones with facetable fields), and `max(popularity)` scoped to
///   entity:node under index_a (jf1, jf2 only) is 30 — neither the global
///   max (60) nor jf1's own value (10). That is exactly the scoping
///   `jf343_deep_max.json` pins.
/// - `body`: alpha/beta/gamma/delta/epsilon/zeta, so
///   `q=body:alpha OR body:beta OR body:zeta` matches exactly jf1, jf2, jf6
///   (mirrors `jf343_terms_q.json`'s `numFound:3`), and `max(body)` over the
///   whole corpus is lexicographically "zeta" (mirrors
///   `jf343_err_agg_text.json` — Wayfinder must 400 instead, see below).
fn jf_corpus() -> Value {
    json!([
        {"id":"jf1","hash":"siteA","index_id":"index_a","ss_search_api_datasource":"entity:node","popularity":10,"body":"alpha"},
        {"id":"jf2","hash":"siteA","index_id":"index_a","ss_search_api_datasource":"entity:node","popularity":30,"body":"beta"},
        {"id":"jf3","hash":"siteA","index_id":"index_a","ss_search_api_datasource":"entity:user","popularity":20,"body":"gamma"},
        {"id":"jf4","hash":"siteA","index_id":"index_b","ss_search_api_datasource":"entity:node","popularity":40,"body":"delta"},
        {"id":"jf5","hash":"siteB","index_id":"index_c","ss_search_api_datasource":"entity:node","popularity":50,"body":"epsilon"},
        {"id":"jf6","popularity":60,"body":"zeta"}
    ])
}

async fn jf_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), JF_SCHEMA_TOML).expect("app must build");
    let (status, body) = post_docs(&app, &jf_corpus()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the jf343 corpus must succeed, got {body}"
    );
    (app, dir)
}

/// Builds an app on `JF_SCHEMA_TOML` plus an arbitrary server config
/// (`strict_params = true`, the sole knob this file needs). Mirrors
/// `tests/faceting.rs::app_with_schema_and_config`, duplicated locally per
/// that file's own precedent of not sharing schema/config helpers across
/// integration-test binaries.
async fn jf_app_with_config(config_toml: &str) -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, JF_SCHEMA_TOML).expect("write schema.toml");
    let config_path = dir.path().join("wayfinder.toml");
    std::fs::write(&config_path, config_toml).expect("write wayfinder.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let app =
        wayfinder::app_with_config(&schema_path, &data_dir, &config_path).expect("app must build");
    let (status, body) = post_docs(&app, &jf_corpus()).await;
    assert_eq!(status, StatusCode::OK, "indexing must succeed, got {body}");
    (app, dir)
}

/// Reads `_version_` straight out of Tantivy's fast-field column, sorted
/// ascending — mirrors `tests/version_field.rs::indexed_versions`, duplicated
/// locally for the same "no cross-binary sharing" reason. `jf_corpus()` is
/// indexed as a single `post_docs` batch, and `tests/version_field.rs` pins
/// that versions increase by exactly 1 per document *in insertion order*
/// within a batch — so `indexed_versions()[i]` is jf<i+1>'s real `_version_`.
fn indexed_versions(dir: &TempDir) -> Vec<i64> {
    let index = Index::open_in_dir(dir.path().join("data")).expect("open Tantivy index");
    let reader = index.reader().expect("open Tantivy reader");
    let searcher = reader.searcher();
    let mut versions: Vec<i64> = searcher
        .segment_readers()
        .iter()
        .flat_map(|segment| {
            let versions = segment
                .fast_fields()
                .i64("_version_")
                .expect("_version_ must be an i64 fast field");
            segment.doc_ids_alive().map(move |doc_id| {
                versions
                    .first(doc_id)
                    .expect("every successfully indexed document must have _version_")
            })
        })
        .collect();
    versions.sort_unstable();
    versions
}

// --- query-string encoding ---------------------------------------------------

/// Percent-encodes every byte outside `A-Za-z0-9-_.~` (RFC 3986 unreserved).
/// `json.facet`'s value is a JSON object — `{`, `}`, `"`, `:`, `,`, and any
/// space inside e.g. `"index asc"` are not legal raw query-string bytes, and
/// `src/params.rs::decode` only understands `%XX` and `+`-for-space, so a
/// raw `+` or `%` inside the JSON text must also be escaped or it would be
/// silently misdecoded.
fn percent_encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() * 3);
    for b in raw.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Percent-encodes a `json.facet` value built with `serde_json::json!`.
fn jf_param(v: &Value) -> String {
    percent_encode(&v.to_string())
}

/// `select?q=*:*&rows=0&json.facet=<v>&wt=json`, the shape every simple case
/// in this file needs.
fn jf_select(v: &Value) -> String {
    format!("select?q=*:*&rows=0&json.facet={}&wt=json", jf_param(v))
}

// --- assertion helpers -------------------------------------------------------

/// The named fixture's `facets` subtree — the ground truth every value
/// assertion in this file ultimately traces back to.
fn fixture_facets(name: &str) -> Value {
    fixture(name)
        .pointer("/facets")
        .unwrap_or_else(|| panic!("fixture `{name}` has no /facets"))
        .clone()
}

/// A parse-time `json.facet` failure (findings §1b): `responseHeader, error`
/// only, no `response` block at all — the query never ran.
fn assert_parse_time_400(status: StatusCode, body: &Value) {
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an invalid json.facet must be a 400, got {status} / {body}"
    );
    assert_eq!(
        body.pointer("/error/code").and_then(Value::as_i64),
        Some(400)
    );
    assert_eq!(
        body.pointer("/responseHeader/status")
            .and_then(Value::as_i64),
        Some(400)
    );
    assert!(
        body.get("response").is_none(),
        "a parse-time json.facet failure must omit the response block entirely \
         (jf343_err_bad_json / jf343_err_bad_type), got {body}"
    );
}

/// A field-resolution `json.facet` failure (findings §1b): `responseHeader,
/// response, error` — the base query ran before the facet field failed to
/// resolve.
fn assert_field_resolution_400(status: StatusCode, body: &Value) {
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an unresolvable json.facet field must be a 400, got {status} / {body}"
    );
    assert_eq!(
        body.pointer("/error/code").and_then(Value::as_i64),
        Some(400)
    );
    assert!(
        body.get("response").is_some(),
        "a field-resolution json.facet failure must still include the response \
         block (jf343_err_unknown_field), got {body}"
    );
}

fn error_msg(body: &Value) -> &str {
    body.pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("error.msg must be present, got {body}"))
}

// === 1. the implicit `count` =================================================

#[tokio::test]
async fn empty_object_still_carries_implicit_count() {
    let (app, _dir) = jf_app().await;
    let (status, body) = get(&app, &jf_select(&json!({}))).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "json.facet={{}} must be a 200: {body}"
    );
    assert_eq!(
        body.get("facets"),
        Some(&fixture_facets("jf343_empty_object")),
        "json.facet={{}} must still carry an implicit scalar count (jf343_empty_object): {body}"
    );
}

#[tokio::test]
async fn absent_json_facet_param_produces_no_facets_key_at_all() {
    let (app, _dir) = jf_app().await;
    let (status, body) = get(&app, "select?q=*:*&rows=0&wt=json").await;
    assert_eq!(status, StatusCode::OK, "plain select must be a 200: {body}");
    assert!(
        body.get("facets").is_none(),
        "json_facets() must self-gate: Ok(None) with no json.facet param, so \
         `facets` must be entirely absent (not present-and-empty), got {body}"
    );
}

// === 2. count tracks q + fq, not the whole index =============================

#[tokio::test]
async fn count_follows_q_not_the_whole_index() {
    let (app, _dir) = jf_app().await;
    let v = json!({"siteHashes":{"limit":-1,"field":"hash","type":"terms"}});
    let (status, body) = get(
        &app,
        &format!(
            "select?q=body:alpha+OR+body:beta+OR+body:zeta&rows=0&json.facet={}&wt=json",
            jf_param(&v)
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "query-scoped json.facet must be a 200: {body}"
    );
    assert_eq!(
        body.pointer("/response/numFound").and_then(Value::as_i64),
        Some(3),
        "test setup: q must match exactly jf1, jf2, jf6, got {body}"
    );
    assert_eq!(
        body.get("facets"),
        Some(&fixture_facets("jf343_terms_q")),
        "facets.count and bucket counts must follow q, not numFound of the whole \
         index (jf343_terms_q): {body}"
    );
}

#[tokio::test]
async fn count_follows_fq_not_the_whole_index() {
    let (app, _dir) = jf_app().await;
    let v = json!({"siteHashes":{"limit":-1,"field":"hash","type":"terms"}});
    let (status, body) = get(
        &app,
        &format!(
            "select?q=*:*&fq=hash:siteA+OR+hash:siteB&rows=0&json.facet={}&wt=json",
            jf_param(&v)
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "fq-scoped json.facet must be a 200: {body}"
    );
    assert_eq!(
        body.pointer("/response/numFound").and_then(Value::as_i64),
        Some(5),
        "test setup: fq must exclude jf6 (no hash field), got {body}"
    );
    assert_eq!(
        body.get("facets"),
        Some(&fixture_facets("jf343_terms_fq")),
        "facets.count and bucket counts must follow fq, not numFound of the whole \
         index (jf343_terms_fq): {body}"
    );
}

// === 3. a single terms facet =================================================

#[tokio::test]
async fn terms_facet_over_whole_index_matches_fixture() {
    let (app, _dir) = jf_app().await;
    let v = json!({"siteHashes":{"limit":-1,"field":"hash","type":"terms"}});
    let (status, body) = get(&app, &jf_select(&v)).await;
    assert_eq!(status, StatusCode::OK, "terms facet must be a 200: {body}");
    assert_eq!(body.get("facets"), Some(&fixture_facets("jf343_terms")));
}

#[tokio::test]
async fn terms_facet_limit_two_matches_fixture() {
    let (app, _dir) = jf_app().await;
    let v = json!({"siteHashes":{"limit":2,"field":"index_id","type":"terms"}});
    let (status, body) = get(&app, &jf_select(&v)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "limit:2 terms facet must be a 200: {body}"
    );
    assert_eq!(
        body.get("facets"),
        Some(&fixture_facets("jf343_terms_limit")),
        "limit:2 must return only the top two buckets by count desc, index_b \
         winning the count:1 tie over index_c alphabetically: {body}"
    );
}

#[tokio::test]
async fn terms_facet_mincount_zero_matches_fixture() {
    let (app, _dir) = jf_app().await;
    let v = json!({"siteHashes":{"limit":-1,"field":"hash","type":"terms","mincount":0}});
    let (status, body) = get(&app, &jf_select(&v)).await;
    assert_eq!(status, StatusCode::OK, "mincount:0 must be a 200: {body}");
    assert_eq!(
        body.get("facets"),
        Some(&fixture_facets("jf343_terms_mincount0"))
    );
}

#[tokio::test]
async fn terms_facet_sort_index_asc_matches_fixture() {
    let (app, _dir) = jf_app().await;
    let v = json!({"siteHashes":{"limit":-1,"field":"index_id","type":"terms","sort":"index asc"}});
    let (status, body) = get(&app, &jf_select(&v)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "sort:index asc must be a 200: {body}"
    );
    assert_eq!(
        body.get("facets"),
        Some(&fixture_facets("jf343_terms_sort_index")),
        "sort:'index asc' must give lexicographic order (index_a, index_b, \
         index_c), not count desc: {body}"
    );
}

// === 4. nesting via the `facet` key ==========================================

#[tokio::test]
async fn two_level_nested_terms_matches_fixture() {
    let (app, _dir) = jf_app().await;
    let v = json!({
        "siteHashes": {
            "limit": -1, "field": "hash", "type": "terms",
            "facet": {
                "numDocsPerIndex": {"limit": -1, "field": "index_id", "type": "terms"}
            }
        }
    });
    let (status, body) = get(&app, &jf_select(&v)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "nested terms facet must be a 200: {body}"
    );
    assert_eq!(
        body.get("facets"),
        Some(&fixture_facets("jf343_terms_nested")),
        "sub-facets must appear inline inside each bucket object, as siblings \
         of val/count (jf343_terms_nested): {body}"
    );
}

#[tokio::test]
async fn max_aggregation_over_popularity_matches_fixture() {
    let (app, _dir) = jf_app().await;
    let v = json!({"maxPopularity": "max(popularity)"});
    let (status, body) = get(&app, &jf_select(&v)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "max(popularity) must be a 200: {body}"
    );
    assert_eq!(body.get("facets"), Some(&fixture_facets("jf343_agg_max")));
}

/// The deepest shape the real client ever sends (§1a): a top-level
/// aggregation plus three terms levels plus a leaf aggregation. This is the
/// test that catches a wrong sub-aggregation scope: `entity:node` under
/// `index_a` must be **30** (max of jf1=10, jf2=30) — neither the global max
/// (60, jf6) nor its bucket's first document's own value (10, jf1).
#[tokio::test]
async fn four_level_topology_scopes_sub_aggregations_correctly() {
    let (app, _dir) = jf_app().await;
    let v = json!({
        "maxPopularity": "max(popularity)",
        "siteHashes": {
            "limit": -1, "field": "hash", "type": "terms",
            "facet": {
                "indexes": {
                    "limit": -1, "field": "index_id", "type": "terms",
                    "facet": {
                        "dataSources": {
                            "limit": -1, "field": "ss_search_api_datasource", "type": "terms",
                            "facet": {"maxPopularityPerDataSource": "max(popularity)"}
                        }
                    }
                }
            }
        }
    });
    let (status, body) = get(&app, &jf_select(&v)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "4-level json.facet must be a 200: {body}"
    );

    let expected = fixture_facets("jf343_deep_max");
    assert_eq!(
        body.get("facets"),
        Some(&expected),
        "the full 4-level topology must match jf343_deep_max exactly: {body}"
    );

    // Explicit standalone assertion (not just "the whole tree matched") per
    // the task spec: this exact leaf is the one a wrong sub-aggregation
    // scope (accidentally reusing the global aggregation, or the bucket's
    // first row instead of a real per-bucket MAX) would get wrong while
    // leaving every *other* leaf accidentally correct.
    let entity_node_under_index_a = body
        .pointer("/facets/siteHashes/buckets/0/indexes/buckets/0/dataSources/buckets/0/maxPopularityPerDataSource")
        .and_then(Value::as_i64);
    assert_eq!(
        entity_node_under_index_a,
        Some(30),
        "entity:node under siteA/index_a must be 30 (max of jf1=10, jf2=30), \
         not the global max 60 (jf6) and not jf1's own value 10 (a wrong sub- \
         aggregation scope could produce either): {body}"
    );
    assert_ne!(entity_node_under_index_a, Some(60));
    assert_ne!(entity_node_under_index_a, Some(10));
}

// === 5. `max(_version_)` end to end (shape from fixtures, values from the real index) ===

/// `jf343_agg_max_version` pins **shape only** (an integer at `facets.maxVersion`,
/// alongside `count`) — its own numeric value is Solr's opaque update-log long
/// and differs on every capture (spec §4). The real expected value is this
/// index's actual maximum `_version_`, read straight out of Tantivy.
#[tokio::test]
async fn max_version_aggregation_is_integer_and_equals_real_max_version() {
    let (app, dir) = jf_app().await;
    let versions = indexed_versions(&dir);
    let real_max = *versions.iter().max().expect("corpus must be non-empty");

    let v = json!({"maxVersion": "max(_version_)"});
    let (status, body) = get(&app, &jf_select(&v)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "max(_version_) must be a 200: {body}"
    );

    let maxversion_value = body
        .pointer("/facets/maxVersion")
        .unwrap_or_else(|| panic!("facets.maxVersion must be present: {body}"));
    assert!(
        maxversion_value.is_i64(),
        "max(_version_) must render as a raw JSON integer, not the float the \
         stats component would emit for the same column (jf343_agg_max_version \
         shows `1872604773983715328`, no decimal point), got {maxversion_value}"
    );
    assert_eq!(
        maxversion_value.as_i64(),
        Some(real_max),
        "maxVersion must equal this index's real maximum _version_, not the \
         fixture's own (Solr-opaque, non-reproducible) number: {body}"
    );
    assert_eq!(
        body.pointer("/facets/count").and_then(Value::as_i64),
        Some(6)
    );

    // Shape sanity against the fixture: same key set at /facets (count,
    // maxVersion), independent of the actual numbers.
    let expected_facets = fixture_facets("jf343_agg_max_version");
    let expected_keys: Vec<&String> = expected_facets
        .as_object()
        .expect("fixture facets must be an object")
        .keys()
        .collect();
    let actual_keys: Vec<&String> = body["facets"]
        .as_object()
        .expect("facets must be an object")
        .keys()
        .collect();
    assert_eq!(
        actual_keys, expected_keys,
        "facets key set for max(_version_) must match the fixture's shape"
    );
}

/// `jf343_deep_version`'s companion to `four_level_topology_scopes_sub_aggregations_correctly`,
/// with `max(_version_)` as the leaf aggregation instead of `max(popularity)`.
/// Pins shape (same 4-level nesting, integer leaves) plus the same
/// sub-aggregation-scoping property against real per-document versions:
/// `jf_corpus()` is indexed as a single batch, and versions increase by
/// exactly 1 per document in insertion order (`tests/version_field.rs`), so
/// `indexed_versions()[i]` is jf<i+1>'s real `_version_`.
#[tokio::test]
async fn deep_version_topology_scopes_sub_aggregations_correctly() {
    let (app, dir) = jf_app().await;
    let versions = indexed_versions(&dir);
    // jf1..jf6 map onto versions[0..6] in insertion order (see doc comment).
    let v_jf1 = versions[0];
    let v_jf2 = versions[1];
    let v_jf3 = versions[2];
    let v_jf4 = versions[3];
    let v_jf5 = versions[4];
    let v_jf6 = versions[5];
    assert!(
        v_jf1 < v_jf2 && v_jf2 < v_jf3 && v_jf3 < v_jf4 && v_jf4 < v_jf5 && v_jf5 < v_jf6,
        "test setup: a single-batch post must assign strictly increasing \
         versions in insertion order, got {versions:?}"
    );

    let v = json!({
        "maxVersion": "max(_version_)",
        "siteHashes": {
            "limit": -1, "field": "hash", "type": "terms",
            "facet": {
                "indexes": {
                    "limit": -1, "field": "index_id", "type": "terms",
                    "facet": {
                        "dataSources": {
                            "limit": -1, "field": "ss_search_api_datasource", "type": "terms",
                            "facet": {"maxVersionPerDataSource": "max(_version_)"}
                        }
                    }
                }
            }
        }
    });
    let (status, body) = get(&app, &jf_select(&v)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "4-level max(_version_) json.facet must be a 200: {body}"
    );

    assert_eq!(
        body.pointer("/facets/maxVersion").and_then(Value::as_i64),
        Some(v_jf6),
        "the global maxVersion must be jf6's real version (the highest), got {body}"
    );

    let entity_node_under_index_a = body
        .pointer("/facets/siteHashes/buckets/0/indexes/buckets/0/dataSources/buckets/0/maxVersionPerDataSource")
        .and_then(Value::as_i64);
    assert_eq!(
        entity_node_under_index_a,
        Some(v_jf2),
        "entity:node under siteA/index_a must be jf2's version (max of jf1, jf2), \
         not the global max (jf6) and not jf1's own version: {body}"
    );
    assert_ne!(entity_node_under_index_a, Some(v_jf6));
    assert_ne!(entity_node_under_index_a, Some(v_jf1));

    let entity_user_under_index_a = body
        .pointer("/facets/siteHashes/buckets/0/indexes/buckets/0/dataSources/buckets/1/maxVersionPerDataSource")
        .and_then(Value::as_i64);
    assert_eq!(entity_user_under_index_a, Some(v_jf3));

    let entity_node_under_index_b = body
        .pointer("/facets/siteHashes/buckets/0/indexes/buckets/1/dataSources/buckets/0/maxVersionPerDataSource")
        .and_then(Value::as_i64);
    assert_eq!(entity_node_under_index_b, Some(v_jf4));

    let entity_node_under_index_c = body
        .pointer("/facets/siteHashes/buckets/1/indexes/buckets/0/dataSources/buckets/0/maxVersionPerDataSource")
        .and_then(Value::as_i64);
    assert_eq!(entity_node_under_index_c, Some(v_jf5));

    // Shape sanity: same key structure as the fixture (values differ, keys don't).
    let expected_facets = fixture_facets("jf343_deep_version");
    let expected_keys: Vec<&String> = expected_facets
        .as_object()
        .expect("fixture facets must be an object")
        .keys()
        .collect();
    let actual_keys: Vec<&String> = body["facets"]
        .as_object()
        .expect("facets must be an object")
        .keys()
        .collect();
    assert_eq!(actual_keys, expected_keys);
}

// === 6. coexistence with classic faceting and stats, incl. top-level shape ===

#[tokio::test]
async fn coexists_with_classic_faceting() {
    let (app, _dir) = jf_app().await;
    let v = json!({"maxPopularity": "max(popularity)"});
    let (status, body) = get(
        &app,
        &format!(
            "select?q=*:*&rows=0&facet=true&facet.field=hash&json.facet={}&wt=json",
            jf_param(&v)
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "facet=true + json.facet must be a 200: {body}"
    );

    let expected = fixture("jf343_with_classic");
    assert_eq!(
        body.get("facet_counts"),
        expected.get("facet_counts"),
        "classic facet_counts must be unaffected by json.facet coexisting: {body}"
    );
    assert_eq!(
        body.get("facets"),
        expected.get("facets"),
        "json.facet's facets block must be unaffected by facet=true coexisting: {body}"
    );
}

#[tokio::test]
async fn coexists_with_classic_faceting_and_stats() {
    let (app, _dir) = jf_app().await;
    let v = json!({"maxPopularity": "max(popularity)"});
    let (status, body) = get(
        &app,
        &format!(
            "select?q=*:*&rows=0&facet=true&facet.field=hash&stats=true&stats.field=popularity&json.facet={}&wt=json",
            jf_param(&v)
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "facet + stats + json.facet must be a 200: {body}"
    );

    let expected = fixture("jf343_with_classic_stats");
    assert_eq!(body.get("facet_counts"), expected.get("facet_counts"));
    assert_eq!(body.get("facets"), expected.get("facets"));
    assert_eq!(
        body.get("stats"),
        expected.get("stats"),
        "classic stats must render its usual float metrics unaffected by json.facet \
         (and, by construction of jf_corpus(), byte-identical to the captured numbers): {body}"
    );

    // Top-level *presence* of all three sibling blocks (order is pinned in
    // tests/json_key_order.rs, which reads raw text, not a parsed Value).
    for key in ["facet_counts", "facets", "stats"] {
        assert!(
            body.get(key).is_some(),
            "top level must carry `{key}` alongside the others: {body}"
        );
    }
}

// === 7. error envelope split =================================================

#[tokio::test]
async fn malformed_json_facet_is_parse_time_400_with_no_response_block() {
    let (app, _dir) = jf_app().await;
    // Exact truncated text from jf343_err_bad_json.json's params.json.facet.
    let raw = r#"{"siteHashes":{"field":"#;
    let (status, body) = get(
        &app,
        &format!(
            "select?q=*:*&rows=0&json.facet={}&wt=json",
            percent_encode(raw)
        ),
    )
    .await;
    assert_parse_time_400(status, &body);
}

#[tokio::test]
async fn unknown_facet_type_is_parse_time_400_with_no_response_block() {
    let (app, _dir) = jf_app().await;
    let v = json!({"siteHashes":{"type":"nosuchtype","field":"hash"}});
    let (status, body) = get(&app, &jf_select(&v)).await;
    assert_parse_time_400(status, &body);
    assert!(
        error_msg(&body).to_lowercase().contains("nosuchtype")
            || error_msg(&body).to_lowercase().contains("type"),
        "the 400 must name the unrecognised type, got: {}",
        error_msg(&body)
    );
}

#[tokio::test]
async fn unknown_field_is_a_field_resolution_400_with_response_block() {
    let (app, _dir) = jf_app().await;
    let v = json!({"x":{"type":"terms","field":"no_such_field"}});
    let (status, body) = get(&app, &jf_select(&v)).await;
    assert_field_resolution_400(status, &body);
    assert!(
        error_msg(&body).contains("no_such_field"),
        "the 400 must name the unresolvable field, got: {}",
        error_msg(&body)
    );
}

// === 8. deliberate divergences from captured Solr (spec §1c) ================

/// `jf343_err_no_docvalues.json`: real Solr answers a terms facet on a
/// non-docValues field (`body`, text_en, not `fast`) with **200** and
/// `{"buckets":[]}`. Wayfinder refuses with a 400 instead, consistent with
/// finding 105's existing classic-facet divergence — silently returning an
/// empty bucket list is a wrong answer a client cannot detect.
#[tokio::test]
async fn terms_on_non_docvalues_field_diverges_from_solr_as_a_400() {
    let (app, _dir) = jf_app().await;
    let v = json!({"x":{"type":"terms","field":"body"}});
    let (status, body) = get(&app, &jf_select(&v)).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "terms on a non-fast field must be a 400 (jf343_err_no_docvalues.json shows \
         Solr's own 200/{{buckets:[]}} here — a deliberate Wayfinder divergence): {body}"
    );
    assert!(
        error_msg(&body).contains("fast values (docValues)"),
        "the refusal must be Wayfinder's own check_facetable wording (finding 105), \
         not merely any 400 that happens to name the field, got: {}",
        error_msg(&body)
    );
}

/// `jf343_err_agg_text.json`: real Solr's `max(body)` over a text field
/// returns the lexicographic max (`"zeta"`). Wayfinder refuses with a 400:
/// silently returning a string where the client expects a number is worse
/// than failing loudly (spec §1c).
#[tokio::test]
async fn max_over_text_field_diverges_from_solr_as_a_400() {
    let (app, _dir) = jf_app().await;
    let v = json!({"x": "max(body)"});
    let (status, body) = get(&app, &jf_select(&v)).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "max(body) over a text field must be a 400 (jf343_err_agg_text.json shows \
         Solr's own 200/\"zeta\" here — a deliberate Wayfinder divergence): {body}"
    );
}

// === 9. out-of-scope inputs must 400, never be silently ignored (spec §2) ===

#[tokio::test]
async fn facet_type_query_is_out_of_scope_400() {
    let (app, _dir) = jf_app().await;
    let v = json!({"x":{"type":"query","q":"popularity:[0 TO 100]"}});
    let (status, body) = get(&app, &jf_select(&v)).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "type:query is out of scope: {body}"
    );
    assert!(
        error_msg(&body).to_lowercase().contains("query"),
        "the 400 must name the unsupported type, got: {}",
        error_msg(&body)
    );
}

#[tokio::test]
async fn facet_type_range_is_out_of_scope_400() {
    let (app, _dir) = jf_app().await;
    let v = json!({"x":{"type":"range","field":"popularity","start":0,"end":100,"gap":10}});
    let (status, body) = get(&app, &jf_select(&v)).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "type:range is out of scope: {body}"
    );
    assert!(
        error_msg(&body).to_lowercase().contains("range"),
        "the 400 must name the unsupported type, got: {}",
        error_msg(&body)
    );
}

#[tokio::test]
async fn object_form_aggregation_is_out_of_scope_400() {
    let (app, _dir) = jf_app().await;
    let v = json!({"x":{"type":"func","func":"max(popularity)"}});
    let (status, body) = get(&app, &jf_select(&v)).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "the object aggregation form {{\"type\":\"func\",\"func\":...}} is never \
         sent by the real client (§1a) and must not be silently accepted: {body}"
    );
}

#[tokio::test]
async fn unknown_aggregation_function_matches_fixture_400() {
    let (app, _dir) = jf_app().await;
    // Exact json.facet from jf343_err_bad_func.json.
    let v = json!({"x": "nosuchfunc(popularity)"});
    let (status, body) = get(&app, &jf_select(&v)).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an unknown aggregation function must 400: {body}"
    );
}

#[tokio::test]
async fn non_max_aggregation_function_sum_is_out_of_scope_400() {
    let (app, _dir) = jf_app().await;
    let v = json!({"x": "sum(popularity)"});
    let (status, body) = get(&app, &jf_select(&v)).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "sum() is a real Solr aggregation Wayfinder does not implement (spec §2) \
         and must 400, not silently be ignored into a wrong count: {body}"
    );
}

#[tokio::test]
async fn non_max_aggregation_function_avg_is_out_of_scope_400() {
    let (app, _dir) = jf_app().await;
    let v = json!({"x": "avg(popularity)"});
    let (status, body) = get(&app, &jf_select(&v)).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "avg() must 400 (spec §2): {body}"
    );
}

/// The unevidenced per-facet settings the real client (§1a) never sends:
/// `domain`, `offset`, `numBuckets`, `allBuckets`, `missing`, `prefix`,
/// `method`, `refine`, `overrequest`, `excludeTags`. Each must 400 rather
/// than be silently ignored — an ignored setting yields wrong counts that
/// look right, which the spec calls out as worse than a 400 divergence.
#[tokio::test]
async fn unevidenced_per_facet_settings_are_400() {
    let (app, _dir) = jf_app().await;
    let cases: &[(&str, Value)] = &[
        ("domain", json!({})),
        ("offset", json!(1)),
        ("numBuckets", json!(true)),
        ("allBuckets", json!(true)),
        ("missing", json!(true)),
        ("prefix", json!("site")),
        ("method", json!("enum")),
        ("refine", json!(true)),
        ("overrequest", json!(5)),
        ("excludeTags", json!(["tag1"])),
    ];
    for (setting, value) in cases {
        let mut facet = json!({"type": "terms", "field": "hash"});
        facet
            .as_object_mut()
            .expect("facet must be an object")
            .insert((*setting).to_string(), value.clone());
        let v = json!({"x": facet});
        let (status, body) = get(&app, &jf_select(&v)).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "unevidenced setting `{setting}` must 400 rather than be silently \
             ignored into a wrong count, got {status} / {body}"
        );
        assert!(
            error_msg(&body).contains(setting),
            "the 400 for `{setting}` must name it (a clear message, not a silent \
             ignore), got: {}",
            error_msg(&body)
        );
    }
}

// === 10. `strict_params = true` must not 400 `json.facet` ====================

#[tokio::test]
async fn strict_params_accepts_json_facet() {
    let (app, _dir) = jf_app_with_config("strict_params = true\n").await;
    let v = json!({"siteHashes":{"limit":-1,"field":"hash","type":"terms"}});
    let (status, body) = get(&app, &jf_select(&v)).await;
    let msg = body
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        !msg.contains("unknown request parameter"),
        "json.facet must be registered in SELECT_PARAMS, got: {msg}"
    );
    assert_eq!(
        status,
        StatusCode::OK,
        "strict_params=true must not 400 json.facet: {body}"
    );
}
