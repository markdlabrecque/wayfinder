//! Internal `_version_` field (issue #99): it is not user-configured, but it
//! is a real i64 Tantivy fast field populated for every accepted document.

mod common;

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use tantivy::Index;
use tantivy::schema::FieldType;
use tempfile::TempDir;

/// Build an empty core whose user schema deliberately has no `_version_`
/// declaration. The implementation must add the internal field itself.
fn version_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), common::SCHEMA_TOML).expect("app must build");
    (app, dir)
}

/// Read the internal fast field directly from Tantivy rather than exposing it
/// through `fl`: the architecture contract limits `_version_` visibility to
/// schema resolution and stats, not select-result documents.
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

#[test]
fn version_field_is_internal_i64_fast_and_absent_from_schema_toml() {
    assert!(
        !common::SCHEMA_TOML.contains("_version_"),
        "_version_ must not be user-configured in schema.toml"
    );

    let (_app, dir) = version_app();
    let index = Index::open_in_dir(dir.path().join("data")).expect("open Tantivy index");
    let schema = index.schema();
    let field = schema
        .get_field("_version_")
        .expect("the internal _version_ field must be in every core schema");
    let entry = schema.get_field_entry(field);
    assert!(
        matches!(entry.field_type(), FieldType::I64(_)),
        "_version_ must be a signed i64 field, got {:?}",
        entry.field_type()
    );
    assert!(entry.is_fast(), "_version_ must be a fast field");
}

#[tokio::test]
async fn versions_are_gapless_for_successful_documents_and_stats_returns_their_maximum() {
    let (app, dir) = version_app();

    let (status, body) = common::post_docs(
        &app,
        &json!([
            {"id":"v1","body":"first"},
            {"id":"v2","body":"second"}
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "first add_documents call: {body}");

    let (status, body) = common::post_docs(&app, &json!([{"id":"v3","body":"third"}])).await;
    assert_eq!(status, StatusCode::OK, "second add_documents call: {body}");

    // A rejected document must not consume a version. `id` is required by the
    // user schema, so this is validation before writer insertion.
    let (status, body) = common::post_docs(&app, &json!([{"body":"invalid"}])).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "test setup: a document missing required id must be rejected: {body}"
    );

    let (status, body) = common::post_docs(&app, &json!([{"id":"v4","body":"fourth"}])).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "third successful add_documents call: {body}"
    );

    let versions = indexed_versions(&dir);
    assert_eq!(
        versions.len(),
        4,
        "each of the four successfully indexed documents must carry _version_"
    );
    for pair in versions.windows(2) {
        assert_eq!(
            pair[1],
            pair[0] + 1,
            "versions must increase once per successful insertion, including within a batch and across calls"
        );
    }

    let (status, body) = common::get(
        &app,
        "select?q=*:*&rows=0&stats=true&stats.field=_version_&function=max(_version_)&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the captured stats.field/_version_ function request must succeed: {body}"
    );

    // `_version_` values are intentionally seeded at process start, so this
    // fixture pins Solr's request/envelope/metric shape, while the exact local
    // maximum is derived from the real indexed fast-field values above.
    let expected = common::fixture("stats_version_max");
    assert_eq!(
        body.pointer("/responseHeader/params/function"),
        expected.pointer("/responseHeader/params/function"),
        "function=max(_version_) must be accepted and echoed like Solr"
    );
    assert_eq!(
        body.pointer("/responseHeader/params/stats.field"),
        expected.pointer("/responseHeader/params/stats.field"),
        "stats.field=_version_ must be accepted and echoed like Solr"
    );
    let expected_metrics = expected
        .pointer("/stats/stats_fields/_version_")
        .and_then(Value::as_object)
        .expect("fixture must contain Solr's _version_ stats metrics");
    let actual_metrics = body
        .pointer("/stats/stats_fields/_version_")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("stats.stats_fields._version_ must be present: {body}"));
    assert_eq!(
        actual_metrics.keys().collect::<Vec<_>>(),
        expected_metrics.keys().collect::<Vec<_>>(),
        "_version_ must use the existing Solr stats metric envelope"
    );
    assert_eq!(actual_metrics.get("count").and_then(Value::as_i64), Some(4));
    assert_eq!(
        actual_metrics.get("missing").and_then(Value::as_i64),
        Some(0)
    );
    assert_eq!(
        actual_metrics.get("max").and_then(Value::as_f64),
        versions.last().map(|version| *version as f64),
        "stats max(_version_) must equal the latest internally assigned version"
    );
}
