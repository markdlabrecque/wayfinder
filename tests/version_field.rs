//! Internal `_version_` field (issue #99): it is not user-configured, but it
//! is a real i64 Tantivy fast field populated for every accepted document.

mod common;

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use tantivy::Index;
use tantivy::schema::FieldType;
use tempfile::TempDir;
use wayfinder::schema;

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

#[test]
fn user_schema_declaration_of_version_field_is_a_normal_load_error() {
    let toml = format!(
        "{}\n[[fields]]\nname = \"_version_\"\ntype = \"long\"\nfast = true\n",
        common::SCHEMA_TOML
    );
    let dir = TempDir::new().expect("temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, toml).expect("write schema.toml");

    let result = std::panic::catch_unwind(|| schema::load(&schema_path));
    let err = result
        .expect("a user _version_ declaration must return an error, not panic")
        .expect_err("_version_ must be reserved and cannot control the internal field");
    assert!(
        format!("{err:#}").contains("_version_"),
        "the schema-load error must name the reserved field: {err:#}"
    );
}

/// This rule deliberately collides with the reserved name. Dynamic schema
/// resolution must not let a user redefine or expose the internal field.
const VERSION_DYNAMIC_COLLISION_SCHEMA_TOML: &str = r#"
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

[[dynamic_fields]]
pattern = "_version_"
type = "long"
stored = true
fast = true
"#;

/// A catch-all dynamic rule must not offer a bypass that the exact rule above
/// does not: `_version_` remains reserved before dynamic resolution.
const VERSION_WILDCARD_DYNAMIC_SCHEMA_TOML: &str = r#"
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

[[dynamic_fields]]
pattern = "*"
type = "long"
stored = true
fast = true
"#;

#[tokio::test]
async fn dynamic_version_rule_cannot_override_internal_access_controls() {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), VERSION_DYNAMIC_COLLISION_SCHEMA_TOML)
        .expect("a dynamic rule must not prevent constructing the core");

    let (status, body) = common::post_docs(&app, &json!([{"id":"forged","_version_":1}])).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a dynamic rule must not allow user input for reserved _version_: {body}"
    );

    let (status, body) = common::post_docs(&app, &json!([{"id":"real"}])).await;
    assert_eq!(status, StatusCode::OK, "test setup must index: {body}");

    let (status, body) = common::get(
        &app,
        "select?q=*:*&rows=0&stats=true&stats.field=_version_&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "stats is the sole authorized _version_ access path: {body}"
    );

    let (status, body) = common::get(&app, "select?q=*:*&sort=_version_+asc&wt=json").await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a dynamic rule must not make _version_ sortable: {body}"
    );

    let (status, body) = common::get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=_version_&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a dynamic rule must not make _version_ facetable: {body}"
    );
}

#[tokio::test]
async fn wildcard_dynamic_rule_cannot_bypass_reserved_version_input_rejection() {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), VERSION_WILDCARD_DYNAMIC_SCHEMA_TOML)
        .expect("a wildcard dynamic rule must not prevent constructing the core");

    let (status, body) = common::post_docs(&app, &json!([{"id":"forged","_version_":1}])).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a wildcard dynamic rule must not allow user input for reserved _version_: {body}"
    );
}

#[tokio::test]
async fn version_field_is_stats_only_not_sortable_or_facetable() {
    let (app, _dir) = version_app();

    let (status, body) = common::get(
        &app,
        "select?q=*:*&rows=0&stats=true&stats.field=_version_&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "_version_ must remain available to the existing stats path: {body}"
    );

    let (status, body) = common::get(&app, "select?q=*:*&sort=_version_+asc&wt=json").await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "_version_ sorting is out of scope: {body}"
    );

    let (status, body) = common::get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=_version_&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "_version_ faceting is out of scope: {body}"
    );
}

/// Dynamic numeric fields are stored in JSON-path fast columns; stats is
/// static-only and must not validate one then aggregate a wrong bare column.
const DYNAMIC_STATS_SCHEMA_TOML: &str = r#"
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

[[dynamic_fields]]
pattern = "*_i"
type = "int"
stored = true
fast = true
"#;

#[tokio::test]
async fn stats_on_a_fast_dynamic_numeric_field_remains_rejected() {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), DYNAMIC_STATS_SCHEMA_TOML).expect("app builds");
    let (status, body) = common::post_docs(&app, &json!([{"id":"d1","count_i":1}])).await;
    assert_eq!(status, StatusCode::OK, "test setup must index: {body}");

    let (status, body) = common::get(
        &app,
        "select?q=*:*&rows=0&stats=true&stats.field=count_i&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "dynamic stats must not pass validation into a wrong bare column: {body}"
    );
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
