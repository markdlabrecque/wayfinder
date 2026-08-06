//! Post-review Phase 4 regressions (issue #389).
//!
//! This file owns the retained coverage for the four review must-fixes while
//! earlier test files remain locked by retained sessions.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{app_with_schema, get, post_docs};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tantivy::Index;
use tempfile::TempDir;
use tower::ServiceExt;
use wayfinder::schema;

const RAW_DYNAMIC_SCHEMA: &str = r#"
[core]
name = "content"
unique_key = "id"
default_field = "id"

[[fields]]
name = "id"
type = "string"
stored = true
required = true

[[dynamic_fields]]
pattern = "tm_*"
type = "string"
stored = true
multi_valued = true
"#;

const ANALYZED_DYNAMIC_SCHEMA: &str = r#"
[core]
name = "content"
unique_key = "id"
default_field = "id"

[[fields]]
name = "id"
type = "string"
stored = true
required = true

[[dynamic_fields]]
pattern = "tm_*"
type = "text_en"
stored = true
multi_valued = true
"#;

const TEXT_SCHEMA: &str = r#"
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
"#;

/// Models an old raw-only dynamic index. Its unused catch-all has an old
/// physical tokenizer identity even though it carries no postings yet.
fn legacy_raw_dynamic_index() -> TempDir {
    let dir = TempDir::new().expect("create temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, RAW_DYNAMIC_SCHEMA).expect("write schema");
    let current = schema::load(&schema_path).expect("load current raw schema");
    let mut persisted =
        serde_json::to_value(&current.tantivy_schema).expect("serialize Tantivy schema");
    replace_tokenizer_for_field(
        &mut persisted,
        schema::DYNAMIC_TEXT_FIELD,
        "wayfinder_dynamic_text_v3",
    )
    .expect("dynamic catch-all exists");

    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    Index::builder()
        .schema(serde_json::from_value(persisted).expect("parse legacy schema"))
        .create_in_dir(&data_dir)
        .expect("create legacy index");
    std::fs::write(schema::snapshot_path(&data_dir), RAW_DYNAMIC_SCHEMA)
        .expect("write schema snapshot");
    std::fs::write(
        schema::analyzer_contract_path(&data_dir),
        schema::ANALYZER_CONTRACT_V3,
    )
    .expect("write legacy analyzer contract");
    dir
}

#[test]
fn old_empty_dynamic_indexes_retain_a_legacy_marker_until_reindexed() {
    let dir = legacy_raw_dynamic_index();

    let app = app_with_schema(dir.path(), RAW_DYNAMIC_SCHEMA)
        .expect("empty raw-only dynamic index can be adopted");
    drop(app);
    assert_eq!(
        std::fs::read_to_string(schema::analyzer_contract_path(&dir.path().join("data")))
            .expect("read adopted contract"),
        schema::ANALYZER_CONTRACT_LEGACY_DYNAMIC_TEXT,
        "an old physical catch-all identity must not be certified as v6"
    );

    let error = app_with_schema(dir.path(), ANALYZED_DYNAMIC_SCHEMA)
        .expect_err("analyzed dynamic use after adoption requires a reindex");
    assert!(
        format!("{error:#}").to_lowercase().contains("reindex"),
        "the fail-closed transition must name reindexing: {error:#}"
    );
}

#[test]
fn only_reserved_current_builtin_names_select_the_phase4_graph_path() {
    assert!(schema::is_current_builtin_graph_tokenizer(
        "wayfinder_text_en_v6"
    ));
    assert!(!schema::is_current_builtin_graph_tokenizer(
        "custom_no_graph_v6"
    ));
}

#[tokio::test]
async fn phrase_graphs_preserve_stopword_gaps_and_bound_deep_input() {
    let dir = TempDir::new().expect("create temp dir");
    let app = app_with_schema(dir.path(), TEXT_SCHEMA).expect("load text schema");
    let (status, body) = post_docs(&app, &json!([{"id": "gap", "body": "quick the fox"}])).await;
    assert_eq!(status, StatusCode::OK, "index phrase corpus: {body}");

    for (query, expected) in [("%22quick+the+fox%22", 1), ("%22quick+fox%22", 0)] {
        let (status, body) = get(&app, &format!("select?q=body%3A{query}&rows=0")).await;
        assert_eq!(status, StatusCode::OK, "/select must succeed: {body}");
        assert_eq!(
            body.pointer("/response/numFound").and_then(Value::as_u64),
            Some(expected),
            "phrase {query:?} must retain analyzer positions: {body}"
        );
    }

    let deep = std::iter::repeat_n("word", 257)
        .collect::<Vec<_>>()
        .join("+");
    let (status, body) = get(&app, &format!("select?q=body%3A%22{deep}%22&rows=0")).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "over-deep phrase graphs must fail without recursive traversal: {body}"
    );
    assert!(
        body.pointer("/error/msg")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains("phrase graph exceeds")),
        "the bounded graph error must explain the rejection: {body}"
    );
}

#[tokio::test]
async fn synonyms_reject_terms_removed_by_any_builtin_query_front() {
    let dir = TempDir::new().expect("create temp dir");
    let app = app_with_schema(dir.path(), TEXT_SCHEMA).expect("load text schema");
    let (status, body) = post_docs(&app, &json!([{"id": "cms", "body": "Drupal guide"}])).await;
    assert_eq!(status, StatusCode::OK, "index synonym corpus: {body}");

    let (status, body) = post_synonym_form(&app, "groups=Drupal%2CDurpal").await;
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "valid synonym save must succeed: {status} {body}"
    );
    assert_query_count(&app, "durpal", 1).await;
    let synonym_file = dir.path().join("data/synonyms.txt");
    let before = std::fs::read(&synonym_file).expect("read synonym file");

    for invalid in [
        "groups=the%2Cfoo".to_owned(),
        format!("groups={}%2Cfoo", "x".repeat(40)),
    ] {
        let (status, body) = post_synonym_form(&app, &invalid).await;
        assert!(
            status.is_client_error(),
            "{invalid:?} must fail before the synonym filter: {status} {body}"
        );
        assert_eq!(
            std::fs::read(&synonym_file).expect("read synonym file after rejection"),
            before,
            "rejected pre-filter terms must not replace durable synonyms"
        );
        assert_query_count(&app, "durpal", 1).await;
    }
}

async fn post_synonym_form(app: &axum::Router, form: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .method("POST")
        .uri("/ui/synonyms")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(form.to_owned()))
        .expect("build synonym request");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("synonym request transport");
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("read synonym response")
        .to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

async fn assert_query_count(app: &axum::Router, query: &str, expected: u64) {
    let (status, body) = get(app, &format!("select?q={query}&rows=0")).await;
    assert_eq!(status, StatusCode::OK, "/select must succeed: {body}");
    assert_eq!(
        body.pointer("/response/numFound").and_then(Value::as_u64),
        Some(expected),
        "query {query:?} returned unexpected count: {body}"
    );
}

fn replace_tokenizer_for_field(
    value: &mut serde_json::Value,
    field_name: &str,
    legacy_tokenizer: &str,
) -> Option<()> {
    match value {
        serde_json::Value::Object(map)
            if map.get("name").and_then(serde_json::Value::as_str) == Some(field_name) =>
        {
            replace_tokenizer(value, legacy_tokenizer)
        }
        serde_json::Value::Object(map) => map
            .values_mut()
            .find_map(|child| replace_tokenizer_for_field(child, field_name, legacy_tokenizer)),
        serde_json::Value::Array(values) => values
            .iter_mut()
            .find_map(|child| replace_tokenizer_for_field(child, field_name, legacy_tokenizer)),
        _ => None,
    }
}

fn replace_tokenizer(value: &mut serde_json::Value, legacy_tokenizer: &str) -> Option<()> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(tokenizer) = map.get_mut("tokenizer") {
                *tokenizer = serde_json::Value::String(legacy_tokenizer.to_owned());
                return Some(());
            }
            map.values_mut()
                .find_map(|child| replace_tokenizer(child, legacy_tokenizer))
        }
        serde_json::Value::Array(values) => values
            .iter_mut()
            .find_map(|child| replace_tokenizer(child, legacy_tokenizer)),
        _ => None,
    }
}
