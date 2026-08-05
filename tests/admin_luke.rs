//! `GET /wayfinder/{core}/admin/luke` — issue #157, reversing the #57 descope for
//! this endpoint.
//!
//! Ground truth for the envelope shape: `solr-ref/search-api/trace/00024.json`
//! (the captured document lives at `.response.body`, a JSON string, not at
//! the trace's top level). The client (`SearchApiSolrBackend.php:993`, per
//! this issue's spec) reads exactly one field out of it:
//! `$data['index']['numDocs']` — confirmed by reading the vendored source:
//! `getLuke()` is called, then only `$data['index']['numDocs']` is read
//! (`isset()` check plus the value itself). Nothing else in the response is
//! consumed by `search_api_solr`.
//!
//! No the captured fixture request set row for this endpoint (ticket's explicit
//! scope note: Lucene-identity fields — directory class names, per-field
//! flag strings, heap accounting — cannot be reproduced honestly), so there
//! is no fixture to diff against. These tests instead independently
//! re-derive the expected `index{}` values from a fresh, separate
//! `tantivy::Index::open_in_dir` read of the same committed data
//! directory — same "don't trust the same code path" reasoning
//! `tests/admin_ui_index_stats.rs::segment_count_oracle` documents — plus
//! known indexed-doc counts and a live schema, never a hardcoded constant a
//! static blob could satisfy.
//!
//! Deliberately never pinned to a *value* (per the ticket's scope note): the
//! Lucene-identity placeholders in `index{}` — `version`, `current`,
//! `directory`, `segmentsFile`, `segmentsFileSizeInBytes`, `userData`. They
//! are static and nothing consumes them, so
//! `luke_index_lucene_identity_placeholder_keys_are_present` asserts their
//! presence and nothing more; asserting a value would freeze a fabricated
//! number into the suite as if it were ground truth.
//! `indexHeapUsageBytes` and `lastModified` are not placeholders at all — the
//! handler omits them, as real Solr does in the captured trace.
//!
//! Also deliberately absent, and not asserted in either direction: Lucene
//! per-field `schema`/`index` flag strings and `topTerms`/`histogram` in
//! `fields{}` (Wayfinder has no Lucene index internals to report honestly; a
//! plausible-looking fake string would be worse than omitting the key).

mod common;

use std::path::Path;

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;

use common::{CORE, app_with_schema, get, request_full};

/// A non-default schema: swaps out `category` (present in
/// `tests/common::SCHEMA_TOML`) for an `int` field, `rating`, so a `fields{}`
/// block that is a static blob of the default schema's field names fails
/// immediately.
const CUSTOM_SCHEMA_TOML: &str = r#"
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
name = "rating"
type = "int"
stored = true
"#;

/// Opens the same committed data directory as a fresh `tantivy::Index` (not
/// through any Wayfinder type) and sums each searchable segment's alive/
/// deleted/max doc counts directly off `SegmentMeta`. A real independent
/// oracle: the actual counts after a commit (and after a delete without a
/// merge) depend on tantivy's own segment bookkeeping, which this test does
/// not assume a value for ahead of time.
struct IndexOracle {
    num_docs: u64,
    num_deleted_docs: u64,
    max_doc: u64,
    segment_count: usize,
}

fn index_oracle(data_dir: &Path) -> IndexOracle {
    let index = tantivy::Index::open_in_dir(data_dir)
        .expect("independent oracle must open the committed index directory");
    let metas = index
        .searchable_segment_metas()
        .expect("independent oracle must list searchable segment metas");
    let mut num_docs = 0u64;
    let mut num_deleted_docs = 0u64;
    let mut max_doc = 0u64;
    for meta in &metas {
        num_docs += u64::from(meta.num_docs());
        num_deleted_docs += u64::from(meta.num_deleted_docs());
        max_doc += u64::from(meta.max_doc());
    }
    IndexOracle {
        num_docs,
        num_deleted_docs,
        max_doc,
        segment_count: metas.len(),
    }
}

/// `n` docs with ids `d0`..`d{n-1}` against `tests/common::SCHEMA_TOML`
/// (which has `body`/`category`, both optional besides required `id`).
fn n_docs(n: usize) -> Value {
    let docs: Vec<Value> = (0..n)
        .map(|i| json!({"id": format!("d{i}"), "body": "placeholder text"}))
        .collect();
    Value::Array(docs)
}

#[tokio::test]
async fn luke_numdocs_matches_the_real_live_document_count() {
    let dir = TempDir::new().expect("create temp dir");
    let app = app_with_schema(dir.path(), common::SCHEMA_TOML).expect("app must build");

    // 7, not the 5-doc `common::corpus()` or the trace's incidental 6 — a
    // distinct count so a hardcoded constant does not silently pass.
    let (status, body) = common::post_docs(&app, &n_docs(7)).await;
    assert_eq!(status, StatusCode::OK, "index 7 docs: {body}");

    let (status, body) = get(&app, "admin/luke?wt=json&json.nl=flat").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "GET admin/luke must succeed: {body}"
    );
    assert_eq!(
        body["index"]["numDocs"], 7,
        "index.numDocs must equal the real live document count (7 docs \
         indexed), not a hardcoded constant; got: {body}"
    );
}

#[tokio::test]
async fn luke_index_block_is_internally_consistent_and_matches_live_index_before_any_delete() {
    let dir = TempDir::new().expect("create temp dir");
    let app = app_with_schema(dir.path(), common::SCHEMA_TOML).expect("app must build");
    let data_dir = dir.path().join("data");

    let (status, body) = common::post_docs(&app, &n_docs(6)).await;
    assert_eq!(status, StatusCode::OK, "index 6 docs: {body}");

    let (status, body) = get(&app, "admin/luke?wt=json&json.nl=flat").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "GET admin/luke must succeed: {body}"
    );

    let oracle = index_oracle(&data_dir);
    assert_eq!(
        body["index"]["numDocs"], oracle.num_docs,
        "index.numDocs must match a fresh independent tantivy oracle, got: {body}"
    );
    assert_eq!(
        body["index"]["deletedDocs"], 0,
        "no deletes have happened yet, got: {body}"
    );
    assert_eq!(
        body["index"]["hasDeletions"], false,
        "hasDeletions must be false with zero deletedDocs, got: {body}"
    );
    assert_eq!(
        body["index"]["maxDoc"], oracle.max_doc,
        "index.maxDoc must match a fresh independent tantivy oracle, got: {body}"
    );
    assert_eq!(
        body["index"]["segmentCount"], oracle.segment_count as u64,
        "index.segmentCount must match a fresh independent tantivy oracle, got: {body}"
    );
}

#[tokio::test]
async fn luke_index_block_stays_consistent_after_a_delete_without_a_merge() {
    let dir = TempDir::new().expect("create temp dir");
    let app = app_with_schema(dir.path(), common::SCHEMA_TOML).expect("app must build");
    let data_dir = dir.path().join("data");

    let (status, body) = common::post_docs(&app, &n_docs(6)).await;
    assert_eq!(status, StatusCode::OK, "index 6 docs: {body}");

    // Delete 2 of the 6 by id, with `commit=true` (baked into `post_docs`'s
    // URI) so the reader reloads — but a plain tantivy commit does not force
    // a merge, so the deleted docs stay live-but-tombstoned in their
    // original segment(s) rather than vanishing from `maxDoc`.
    let (status, body) = common::post_docs(&app, &json!({"delete": ["d0", "d1"]})).await;
    assert_eq!(status, StatusCode::OK, "delete 2 docs: {body}");

    let (status, body) = get(&app, "admin/luke?wt=json&json.nl=flat").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "GET admin/luke must succeed: {body}"
    );

    let oracle = index_oracle(&data_dir);
    assert!(
        oracle.num_deleted_docs > 0,
        "sanity: the independent oracle must itself observe a tombstoned \
         doc after a delete-without-merge, or this test proves nothing"
    );

    assert_eq!(
        body["index"]["numDocs"], oracle.num_docs,
        "index.numDocs after delete must match a fresh independent tantivy \
         oracle (live/alive doc count), got: {body}"
    );
    assert_eq!(
        body["index"]["deletedDocs"], oracle.num_deleted_docs,
        "index.deletedDocs must match a fresh independent tantivy oracle, \
         got: {body}"
    );
    assert_eq!(
        body["index"]["maxDoc"], oracle.max_doc,
        "index.maxDoc must still count deleted-but-not-merged docs \
         (numDocs + deletedDocs), got: {body}"
    );
    assert_eq!(
        body["index"]["hasDeletions"], true,
        "hasDeletions must be true once deletedDocs > 0, got: {body}"
    );

    // Mutual consistency between the three fields themselves, independent of
    // the oracle: maxDoc must be exactly numDocs + deletedDocs, and
    // hasDeletions must exactly track deletedDocs > 0.
    let num_docs = body["index"]["numDocs"].as_u64().expect("numDocs is a u64");
    let deleted_docs = body["index"]["deletedDocs"]
        .as_u64()
        .expect("deletedDocs is a u64");
    let max_doc = body["index"]["maxDoc"].as_u64().expect("maxDoc is a u64");
    assert_eq!(
        max_doc,
        num_docs + deleted_docs,
        "maxDoc must equal numDocs + deletedDocs, got: {body}"
    );
    assert_eq!(
        body["index"]["segmentCount"], oracle.segment_count as u64,
        "index.segmentCount must match a fresh independent tantivy oracle \
         after the delete, got: {body}"
    );
}

/// Presence only, deliberately: the module header claims the Lucene-identity
/// keys are served as static placeholders, and until this test existed nothing
/// in the suite backed that claim — the handler could have dropped every one of
/// them and stayed green. Values are *not* asserted: they are plausible
/// fictions (`admin_luke_index_placeholders` in `src/lib.rs` names each one and
/// why), so pinning one would freeze a fabricated number into the suite as
/// though it were ground truth.
#[tokio::test]
async fn luke_index_lucene_identity_placeholder_keys_are_present() {
    let (app, _dir) = common::indexed_app().await;

    let (status, body) = get(&app, "admin/luke?wt=json&json.nl=flat").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "GET admin/luke must succeed: {body}"
    );

    let index = body["index"]
        .as_object()
        .unwrap_or_else(|| panic!("index{{}} must be an object, got: {body}"));
    for key in [
        "version",
        "current",
        "directory",
        "segmentsFile",
        "segmentsFileSizeInBytes",
        "userData",
    ] {
        assert!(
            index.contains_key(key),
            "index{{}} must carry the Lucene-identity placeholder `{key}` \
             (presence only -- its value is deliberately unasserted), got: {body}"
        );
    }
}

#[tokio::test]
async fn luke_fields_block_reflects_the_live_schema_not_a_static_blob() {
    let dir = TempDir::new().expect("create temp dir");
    let app = app_with_schema(dir.path(), CUSTOM_SCHEMA_TOML).expect("app must build");

    let (status, body) = common::post_docs(
        &app,
        &json!([{"id": "x1", "body": "hello world", "rating": 4}]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "index 1 doc: {body}");

    let (status, body) = get(&app, "admin/luke?wt=json&json.nl=flat").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "GET admin/luke must succeed: {body}"
    );

    let fields = body["fields"]
        .as_object()
        .unwrap_or_else(|| panic!("fields{{}} must be an object, got: {body}"));
    let mut names: Vec<&str> = fields.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec!["body", "id", "rating"],
        "fields{{}} must list exactly this schema's field names (`rating`, \
         not `category` from the *default* tracer-bullet schema) — a static \
         blob copied from the default schema fails this, got: {body}"
    );

    assert!(
        fields["rating"].get("type").is_some(),
        "each field entry must carry a `type` attribute, got: {body}"
    );
    assert!(
        fields["id"].get("type").is_some(),
        "each field entry must carry a `type` attribute, got: {body}"
    );

    // Deliberately not asserted: exact Lucene-style `schema`/`index` flag
    // strings (`ITS-----OF-----`) — the ticket's scope note says Wayfinder
    // has no Lucene index internals to report honestly, and a plausible
    // fake string would be worse than omitting the key. Likewise
    // `topTerms`/`histogram` are out of scope.
}

#[tokio::test]
async fn luke_strict_params_accepts_the_documented_solr_params() {
    let dir = TempDir::new().expect("create temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, common::SCHEMA_TOML).expect("write schema.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let config_path = dir.path().join("wayfinder.toml");
    std::fs::write(&config_path, "strict_params = true\n").expect("write wayfinder.toml");
    let app: Router =
        wayfinder::app_with_config(&schema_path, &data_dir, &config_path).expect("app must build");

    for query in [
        "admin/luke?wt=json",
        "admin/luke?wt=json&json.nl=flat",
        "admin/luke?wt=json&numTerms=10",
        "admin/luke?wt=json&show=schema",
        "admin/luke?wt=json&fl=id",
    ] {
        let (status, body) = get(&app, query).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "strict_params=true must not 400 on a real Solr `admin/luke` \
             param (`{query}`), got: {body}"
        );
    }
}

#[tokio::test]
async fn luke_strict_params_rejects_an_unknown_param() {
    let dir = TempDir::new().expect("create temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, common::SCHEMA_TOML).expect("write schema.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let config_path = dir.path().join("wayfinder.toml");
    std::fs::write(&config_path, "strict_params = true\n").expect("write wayfinder.toml");
    let app: Router =
        wayfinder::app_with_config(&schema_path, &data_dir, &config_path).expect("app must build");

    let (status, body) = get(&app, "admin/luke?wt=json&bogus=1").await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an unknown param must be rejected under strict_params, got: {body}"
    );
    assert_eq!(body["error"]["code"].as_u64(), Some(400));
}

/// Mutation guard for the `check_core` call in the handler, added by the
/// implementor (the spec named this guard explicitly: sibling #156 shipped
/// without it and the reviewer caught it). Without `check_core`,
/// `GET /wayfinder/nosuchcore/admin/luke` would report the real core's doc count
/// under any core name at all. Verified by deletion: removing the
/// `check_core` line makes this test fail with 200, and nothing else in the
/// suite notices.
#[tokio::test]
async fn luke_unknown_core_is_a_json_404() {
    let (app, _dir) = common::indexed_app().await;

    let (status, body) = request_full(
        &app,
        "GET",
        "nosuchcore/admin/luke?wt=json&json.nl=flat",
        None,
    )
    .await;

    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an unknown core must 404, got: {body}"
    );
    let header = body
        .get("responseHeader")
        .unwrap_or_else(|| panic!("the WithParams envelope carries responseHeader, got: {body}"));
    assert_eq!(header["status"].as_u64(), Some(404), "body: {body}");
    assert!(
        header.get("params").is_some(),
        "this route uses the WithParams envelope, so params are echoed, got: {body}"
    );
    assert_eq!(body["error"]["code"].as_i64(), Some(404), "body: {body}");
    assert!(
        body["error"]["msg"]
            .as_str()
            .is_some_and(|m| m.contains("nosuchcore")),
        "error.msg must name the unknown core, got: {body}"
    );
    assert!(
        body.get("index").is_none(),
        "an unknown core must not leak the real core's index stats, got: {body}"
    );
}

/// Routing assertion: `GET /wayfinder/{core}/admin/luke` is reachable under the
/// core path at all. Deliberately weaker than the tests above — it asserts
/// only "not 404", so it stays true regardless of what the handler answers,
/// and it fails loudly if the route is ever dropped from `search_api_routes!`
/// or moved off the core-relative path. The status/body shape for the routed
/// case is pinned by the `index{}`/`fields{}` tests above; the unknown-core
/// 404 is pinned by `luke_unknown_core_is_a_json_404`.
#[tokio::test]
async fn luke_route_exists_under_the_core_path() {
    let (app, _dir) = common::indexed_app().await;

    let (status, body) = request_full(&app, "GET", &format!("{CORE}/admin/luke"), None).await;
    assert_ne!(
        status,
        StatusCode::NOT_FOUND,
        "GET /wayfinder/{{core}}/admin/luke must be routed, got: {body}"
    );
}
