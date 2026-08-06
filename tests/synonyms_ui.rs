//! Phase 4 synonym administration contract (issue #389).
//!
//! Product decisions encoded here: each core owns a query-side-only synonym
//! table; `GET`/`POST /ui/synonyms` is the operator surface (not `/select`);
//! and a successful edit is persisted in the core data directory without a
//! restart or reindex.
//!
//! Minimal form/file contract chosen for this phase:
//! - the POST body is `application/x-www-form-urlencoded` with one `groups`
//!   field containing newline-separated, comma-separated groups;
//! - every member is one non-whitespace token; groups are normalized to
//!   lowercase and expand symmetrically;
//! - the durable file is `data/synonyms.txt`, UTF-8, one normalized group per
//!   line (`drupal,durpal\n`).
//!
//! These choices intentionally leave multi-token mappings and directional
//! mappings for a later phase.

mod common;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode};
use common::{app_with_schema, get, post_docs};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

const SYNONYMS_ROUTE: &str = "/ui/synonyms";
const SYNONYM_FILE: &str = "data/synonyms.txt";
const SYNONYM_SCHEMA: &str = r#"
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
name = "twm_suggest"
type = "text_en"
stored = true
multi_valued = true
"#;

async fn post_synonym_form(
    app: &axum::Router,
    form: &str,
) -> (StatusCode, axum::http::HeaderMap, String) {
    post_synonym_form_with_headers(app, form, HeaderMap::new()).await
}

async fn post_synonym_form_with_headers(
    app: &axum::Router,
    form: &str,
    headers: HeaderMap,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let mut request = Request::builder()
        .method("POST")
        .uri(SYNONYMS_ROUTE)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(form.to_owned()))
        .expect("build synonym form request");
    request.headers_mut().extend(headers);
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("synonym form request must not fail at transport level");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("synonym form response must be readable")
        .to_bytes();
    (
        status,
        headers,
        String::from_utf8(bytes.to_vec()).expect("synonym form response must be UTF-8"),
    )
}

async fn synonym_app() -> (axum::Router, TempDir) {
    let dir = TempDir::new().expect("create temp dir");
    let app = app_with_schema(dir.path(), SYNONYM_SCHEMA).expect("synonym schema must load");
    let (status, body) = post_docs(
        &app,
        &json!([{"id": "cms", "body": "Drupal guide", "twm_suggest": ["Drupal guide"]}]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "index synonym corpus: {body}");
    (app, dir)
}

fn num_found(body: &Value) -> u64 {
    body.pointer("/response/numFound")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("response.numFound must be numeric: {body}"))
}

async fn assert_query_count(app: &axum::Router, q: &str, expected: u64) {
    let (status, body) = get(app, &format!("select?q={q}&rows=0")).await;
    assert_eq!(status, StatusCode::OK, "/select?q={q}: {body}");
    assert_eq!(num_found(&body), expected, "/select?q={q}: {body}");
}

fn assert_save_succeeded(status: StatusCode, body: &str) {
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "valid synonym save must return 200 or redirect after saving; status={status}, body: {body}"
    );
}

#[tokio::test]
async fn synonym_page_has_client_filter_and_editable_group_rows() {
    let (app, _dir) = synonym_app().await;
    let (status, _headers, body) = post_synonym_form(&app, "groups=Drupal%2CDurpal").await;
    assert_save_succeeded(status, &body);

    let (status, headers, body) = common::get_text(&app, SYNONYMS_ROUTE).await;
    assert_eq!(status, StatusCode::OK, "GET {SYNONYMS_ROUTE}: {body}");
    assert!(
        headers
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("text/html")),
        "synonym admin page must be HTML: {headers:?}"
    );

    // The filter is explicitly client-side: a search control, the group list
    // it filters, and script wiring are all required. Server query parameters
    // alone would not satisfy this operator UX.
    assert!(
        body.contains("id=\"synonym-filter\""),
        "missing filter: {body}"
    );
    assert!(
        body.contains("id=\"synonym-groups\""),
        "missing group list: {body}"
    );
    assert!(
        body.contains("<script"),
        "filter must have client-side wiring: {body}"
    );

    // A persisted group must render as an editable row, with explicit add,
    // edit, and delete affordances rather than an opaque configuration blob.
    assert!(
        body.contains("value=\"drupal,durpal\""),
        "missing group row: {body}"
    );
    for action in ["add", "edit", "delete"] {
        assert!(
            body.contains(&format!("data-action=\"{action}\"")),
            "synonym page must expose a `{action}` control: {body}"
        );
    }
}

#[tokio::test]
async fn saved_synonyms_expand_immediately_on_queries_but_never_enter_index_terms() {
    let (app, dir) = synonym_app().await;
    let (status, _headers, body) = post_synonym_form(&app, "groups=Drupal%2CDurpal").await;
    assert_save_succeeded(status, &body);

    // Case-insensitive, symmetric expand-style lookup must take effect in the
    // running core immediately; no index write/reopen is permitted for it.
    assert_query_count(&app, "durpal", 1).await;
    assert_query_count(&app, "DRUPAL", 1).await;
    // `fq` and both edismax entry points call the same query analyzer; a
    // synonym that works only for the default parser is a hidden query-path
    // split, not query-side synonym support.
    assert_query_count(&app, "*:*&fq=durpal", 1).await;
    assert_query_count(&app, "%22durpal%22", 1).await;
    assert_query_count(&app, "%22durpal+guide%22", 1).await;
    assert_query_count(&app, "durpal&defType=edismax&qf=body", 1).await;
    assert_query_count(&app, "%22durpal%22&defType=edismax&qf=body", 1).await;
    assert_query_count(&app, "%22durpal+guide%22&defType=edismax&qf=body", 1).await;

    // Query-side-only is the reason synonym edits do not require reindexing:
    // the synonym spelling must not be materialized into the term dictionary.
    let (status, terms) = get(&app, "terms?terms=true&terms.fl=body&terms.limit=100").await;
    assert_eq!(status, StatusCode::OK, "/terms must succeed: {terms}");
    let listed: Vec<&str> = terms["terms"]["body"]
        .as_array()
        .unwrap_or_else(|| panic!("terms.body must be a flat term array: {terms}"))
        .iter()
        .step_by(2)
        .map(|value| value.as_str().expect("term must be a string"))
        .collect();
    assert!(
        listed.contains(&"drupal"),
        "indexed source term missing: {terms}"
    );
    assert!(
        !listed.contains(&"durpal"),
        "query-only synonym must not be written to index terms: {terms}"
    );

    assert_eq!(
        std::fs::read(dir.path().join(SYNONYM_FILE)).expect("saved synonym file"),
        b"drupal,durpal\n",
        "the durable table format is normalized one comma-group per line"
    );
}

#[tokio::test]
async fn synonym_table_survives_reopen_and_is_loaded_before_the_first_query() {
    let (app, dir) = synonym_app().await;
    let (status, _headers, body) = post_synonym_form(&app, "groups=Drupal%2CDurpal").await;
    assert_save_succeeded(status, &body);
    drop(app);

    let reopened = app_with_schema(dir.path(), SYNONYM_SCHEMA).expect("reopen core");
    assert_query_count(&reopened, "durpal", 1).await;
}

#[tokio::test]
async fn invalid_group_rejects_without_replacing_live_or_durable_synonyms() {
    let (app, dir) = synonym_app().await;
    // The 45-byte English word is below the analyzer's inclusive 32_766
    // Unicode-scalar-value member limit; byte length alone must not reject it.
    let (status, _headers, body) = post_synonym_form(
        &app,
        "groups=Drupal%2CDurpal%2Cpneumonoultramicroscopicsilicovolcanoconiosis",
    )
    .await;
    assert_save_succeeded(status, &body);
    let path = dir.path().join(SYNONYM_FILE);
    let before = std::fs::read(&path).expect("baseline synonym file");

    // Multi-token phrases are deliberately outside this phase's safe form
    // contract. A rejected write must leave both the currently hot table and
    // the complete on-disk file untouched (the observable atomicity contract).
    let (status, _headers, body) = post_synonym_form(&app, "groups=new+york%2Cnyc").await;
    assert!(
        status.is_client_error(),
        "invalid synonym group must fail with a 4xx response; status={status}, body: {body}"
    );
    assert_eq!(
        std::fs::read(&path).expect("synonym file after rejected save"),
        before,
        "a rejected edit must not partially replace the durable table"
    );
    assert_query_count(&app, "durpal", 1).await;
    assert_query_count(&app, "nyc", 0).await;

    // A delimiter graph member such as `sku42` spans `sku` and `42`, so it
    // cannot be a same-position synonym alternative. Its rejection has the
    // same atomicity guarantee as every other malformed edit.
    let (status, _headers, body) = post_synonym_form(&app, "groups=sku42%2Cbaz").await;
    assert!(
        status.is_client_error(),
        "split synonym member must fail: status={status}, body: {body}"
    );
    assert_eq!(
        std::fs::read(&path).expect("synonym file after split-member rejection"),
        before,
        "a split member must not replace the durable table"
    );
    assert_query_count(&app, "durpal", 1).await;

    // Core-wide synonyms must survive every built-in chain before the synonym
    // filter. English stopwords and members over the static chain's inclusive
    // 32_766-Unicode-scalar-value limit do not.
    for invalid in [
        "groups=the%2Cfoo".to_owned(),
        format!("groups={}%2Cfoo", "x".repeat(32_767)),
    ] {
        let (status, _headers, body) = post_synonym_form(&app, &invalid).await;
        assert!(
            status.is_client_error(),
            "analyzer-rejected synonym member must fail: status={status}, body: {body}"
        );
        assert_eq!(
            std::fs::read(&path).expect("synonym file after filtered-member rejection"),
            before,
            "a filtered member must not replace the durable table"
        );
        assert_query_count(&app, "durpal", 1).await;
    }

    // A member in two groups would make expansion order-dependent (`durpal`
    // could expand differently after a reorder), so it is malformed too.
    let (status, _headers, body) =
        post_synonym_form(&app, "groups=drupal%2Cdurpal%0Adurpal%2Cdrpl").await;
    assert!(
        status.is_client_error() && body.contains("multiple groups"),
        "an overlapping duplicate must be rejected: status={status}, body: {body}"
    );
    assert_eq!(
        std::fs::read(&path).expect("synonym file after duplicate rejection"),
        before,
        "a rejected duplicate must not replace the durable table"
    );
    assert_query_count(&app, "durpal", 1).await;
}

#[tokio::test]
async fn hot_synonyms_are_same_position_suggest_alternatives() {
    let (app, _dir) = synonym_app().await;
    let (status, _headers, body) =
        post_synonym_form(&app, "groups=Drupal%2CDurpal%0Aguide%2Cday").await;
    assert_save_succeeded(status, &body);

    let (status, body) = get(
        &app,
        "suggest?suggest.dictionary=en&suggest.q=durpal&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "/suggest must succeed: {body}");
    assert_eq!(
        body.pointer("/suggest/en/durpal/numFound")
            .and_then(Value::as_u64),
        Some(1),
        "a same-position synonym is an alternative, not a second required suggestion word: {body}"
    );

    let (status, body) = get(&app, "suggest?suggest.dictionary=en&suggest.q=day&wt=json").await;
    assert_eq!(status, StatusCode::OK, "/suggest must succeed: {body}");
    assert_eq!(
        body.pointer("/suggest/en/day/numFound")
            .and_then(Value::as_u64),
        Some(1),
        "synonyms must expand before English terminal-y normalization: {body}"
    );
}

#[tokio::test]
async fn configured_members_are_folded_and_reject_unsafe_analyzer_inputs() {
    let (app, dir) = synonym_app().await;
    let (status, _headers, body) = post_synonym_form(
        &app,
        "groups=Sm%C3%B8rrebr%C3%B8d%2CStra%C3%9Fe%2CEncyclop%C3%A6dia",
    )
    .await;
    assert_save_succeeded(status, &body);
    assert_eq!(
        std::fs::read(dir.path().join(SYNONYM_FILE)).expect("canonical synonym file"),
        b"smorrebrod,strasse,encyclopaedia\n"
    );

    // Validation follows the analyzer graph rather than a hand-maintained
    // script or numeric blacklist. U+2F800 folds to U+4E3D before the UAX and
    // delimiter checks, while a numeric one-position token is equally safe.
    let (status, _headers, body) = post_synonym_form(&app, "groups=%F0%AF%A0%80%2Cbaz").await;
    assert_save_succeeded(status, &body);
    assert_eq!(
        std::fs::read(dir.path().join(SYNONYM_FILE)).expect("compatibility ideograph synonym file"),
        "丽,baz\n".as_bytes(),
        "U+2F800 must persist its analyzer-folded form"
    );
    let (status, _headers, body) = post_synonym_form(&app, "groups=42%2Cbaz").await;
    assert_save_succeeded(status, &body);

    // Canonicalization must still reject every member which the real query
    // analyzer turns into multiple UAX terms or delimiter graph positions.
    for invalid in [
        "foo-bar%2Cbaz",
        "sku42%2Cbaz",
        "camelCase%2Cbaz",
        "%E6%9D%B1%E4%BA%AC%E9%83%BD%2Cbaz",
        "%CC%81%2Cbaz",
    ] {
        let (status, _headers, body) = post_synonym_form(&app, &format!("groups={invalid}")).await;
        assert!(
            status.is_client_error(),
            "{invalid:?} must be rejected: {status} {body}"
        );
    }

    // Tantivy lowercases Unicode one character at a time; it deliberately
    // avoids Rust's contextual final-sigma rule, so ΟΣ canonicalizes to οσ.
    let (status, _headers, body) = post_synonym_form(&app, "groups=%CE%9F%CE%A3%2Comega").await;
    assert_save_succeeded(status, &body);
    assert_eq!(
        std::fs::read(dir.path().join(SYNONYM_FILE)).expect("Tantivy-cased synonym file"),
        "οσ,omega\n".as_bytes()
    );
}

#[tokio::test]
async fn phrase_expansion_is_bounded_before_cartesian_allocation() {
    let (app, _dir) = synonym_app().await;
    let (status, _headers, body) = post_synonym_form(&app, "groups=Drupal%2CDurpal").await;
    assert_save_succeeded(status, &body);

    let query = std::iter::repeat_n("durpal", 11)
        .collect::<Vec<_>>()
        .join("+");
    let (status, body) = get(&app, &format!("select?q=%22{query}%22&rows=0")).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "phrase graph must be rejected: {body}"
    );
    assert!(
        body.to_string().contains("phrase expansion exceeds"),
        "error must identify the bounded phrase expansion: {body}"
    );
}

#[tokio::test]
async fn cross_origin_synonym_post_is_rejected() {
    let (app, _dir) = synonym_app().await;
    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("wayfinder.test"));
    headers.insert(
        "origin",
        HeaderValue::from_static("https://attacker.invalid"),
    );
    let (status, _headers, body) =
        post_synonym_form_with_headers(&app, "groups=Drupal%2CDurpal", headers).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "cross-origin form POST must fail: {body}"
    );
}

#[tokio::test]
async fn malformed_durable_synonyms_fail_startup_before_the_first_query() {
    let dir = TempDir::new().expect("create temp dir");
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).expect("create data directory");
    std::fs::write(data.join("synonyms.txt"), "drupal,durpal\ndurpal,drpl\n")
        .expect("write malformed durable table");

    let error = app_with_schema(dir.path(), SYNONYM_SCHEMA)
        .expect_err("an ambiguous durable synonym table must fail startup");
    let message = format!("{error:#}");
    assert!(
        message.contains("synonym table") && message.contains("multiple groups"),
        "startup error must identify the malformed durable synonym table: {message}"
    );
}
