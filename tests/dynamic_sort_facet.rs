//! Issue #66 — `sort` and `facet.field` must resolve a field that only exists
//! via a `[[dynamic_fields]]` pattern match, not just a `[[fields]]` entry.
//!
//! Confirmed bug (read from source before writing these): `check_sort`
//! (`src/lib.rs`) resolves the sort field via
//! `state.index.wf_schema.fields.iter().find(|f| f.name == field_name)` —
//! `wf_schema.fields` is only ever populated from `[[fields]]`, `match_dynamic`
//! is never consulted. `check_facetable` (`src/facet.rs`) does the same thing
//! through `schema.field_config(field_name)`, which is a thin wrapper over the
//! same `fields` lookup. A field that exists only via a `[[dynamic_fields]]`
//! pattern — e.g. `pattern = "ss_*"`, `fast = true` — therefore 400s with
//! "undefined field" on both `sort=ss_lang asc` and `facet.field=ss_lang`,
//! even though the matching rule declares `fast = true` and the value is
//! genuinely indexed with docValues (`src/schema.rs`'s catch-all JSON
//! container is built with `.set_fast(...)` unconditionally) and already
//! queryable via `q=ss_lang:en` (`CoreIndex::rewrite_dynamic_fields` already
//! resolves dynamic fields for the query path — the fix here should mirror
//! that resolution, not invent a new one).
//!
//! No Solr fixture backs this: it is a Wayfinder-only validation gap, not a
//! wire-compatibility fact pinned by a capture. Expected values are computed
//! directly (alphabetic sort order, term counts) against a schema built for
//! this issue alone, following the same-shape pattern as
//! `tests/query_types.rs`'s `DYNAMIC_QUOTE_SCHEMA_TOML` /
//! `tests/faceting.rs`'s `DEBT_SCHEMA_TOML`.
//!
//! The negative case matters as much as the positive ones: the fix must key
//! off the matched `DynamicFieldConfig`'s own `fast` flag, not just "did a
//! dynamic rule match at all" — otherwise a rule the schema author declared
//! non-fast becomes silently sortable/facetable, which is its own
//! compatibility bug (mirrors the existing non-fast-*static*-field refusal
//! `tests/error_shapes.rs::sort_on_a_non_fast_field_matches_solr_error_shape`
//! and `tests/faceting.rs`'s `Refusal::NotFast`).

// The `dead_code` allow for the shared helpers is an inner attribute inside
// `tests/common/mod.rs`; do not add a second one here (clippy rejects it under
// `-D warnings`).
mod common;

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;

use common::{get, post_docs};

/// `id` (string, fast, required, unique key) + `body` (text_en, unused by
/// these tests but present so the schema resembles a real core) as
/// `[[fields]]`, plus two `[[dynamic_fields]]` rules:
///
/// - `ss_*` -> `string`, `fast = true`: the rule under test. A field matching
///   only this pattern (e.g. `ss_lang`) must become sortable/facetable once
///   the bug is fixed.
/// - `sx_*` -> `string`, no `fast`: the control. A field matching only this
///   pattern (e.g. `sx_note`) must still be refused, exactly as a non-fast
///   *static* field is refused today — the fix must not make every dynamic
///   match sortable/facetable regardless of its own `fast` flag.
const DYNAMIC_SORT_FACET_SCHEMA_TOML: &str = r#"
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

[[dynamic_fields]]
pattern = "ss_*"
type = "string"
stored = true
fast = true

[[dynamic_fields]]
pattern = "sx_*"
type = "string"
stored = true
"#;

/// Builds an app on `DYNAMIC_SORT_FACET_SCHEMA_TOML` and indexes `docs`.
async fn dynamic_app(docs: &Value) -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), DYNAMIC_SORT_FACET_SCHEMA_TOML)
        .expect("app must build");
    let (status, body) = post_docs(&app, docs).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the dynamic-sort/facet corpus must succeed, got {body}"
    );
    (app, dir)
}

/// The ordered `id` list in a response envelope.
fn ids(envelope: &Value) -> Vec<String> {
    envelope["response"]["docs"]
        .as_array()
        .expect("response.docs must be an array")
        .iter()
        .map(|d| {
            d["id"]
                .as_str()
                .expect("every doc must have a string id")
                .to_string()
        })
        .collect()
}

/// `facet_counts.facet_fields.<field>` as a flat alternating `[term, count,
/// term, count, ...]` array — the shape Solr uses and `tests/faceting.rs`
/// already relies on (`flat_facet`/`expect_flat`, not reused across test
/// binaries since each integration test file is its own crate).
fn flat_facet(body: &Value, field: &str) -> Vec<Value> {
    body.pointer(&format!("/facet_counts/facet_fields/{field}"))
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| {
            panic!("facet_counts.facet_fields.{field} must be a flat array, got: {body}")
        })
        .clone()
}

fn expect_flat(pairs: &[(&str, i64)]) -> Vec<Value> {
    let mut out = Vec::with_capacity(pairs.len() * 2);
    for (term, count) in pairs {
        out.push(Value::from(*term));
        out.push(Value::from(*count));
    }
    out
}

/// Asserts a 400 with the Solr error envelope, naming the offending field and
/// containing `fragment` — the same two-part check `tests/faceting.rs`'s
/// `assert_facet_400` uses, so a bare "some 400 happened" (e.g. a Tantivy
/// aggregation error rather than Wayfinder's own guard) cannot pass silently.
fn assert_bad_request(status: StatusCode, body: &Value, must_name: &str, fragment: &str) {
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "must be a 400, got {status} / {body}"
    );
    assert_eq!(
        body.pointer("/error/code").and_then(Value::as_i64),
        Some(400),
        "error.code must mirror the HTTP status, got {body}"
    );
    assert_eq!(
        body.pointer("/responseHeader/status")
            .and_then(Value::as_i64),
        Some(400),
        "responseHeader.status must mirror the HTTP status, got {body}"
    );
    let msg = body
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("error.msg must be present, got {body}"));
    assert!(
        msg.contains(must_name),
        "error.msg must name the offending field `{must_name}`, got: {msg}"
    );
    assert!(
        msg.contains(fragment),
        "error.msg must contain `{fragment}`, got: {msg}"
    );
}

// --- 1. sort on a field that only matches a `[[dynamic_fields]]` pattern ----

#[tokio::test]
async fn sort_on_a_fast_dynamic_only_field_orders_results() {
    let (app, _dir) = dynamic_app(&json!([
        {"id": "d1", "ss_lang": "fr"},
        {"id": "d2", "ss_lang": "de"},
        {"id": "d3", "ss_lang": "en"},
    ]))
    .await;

    let (status, body) = get(&app, "select?q=*:*&sort=ss_lang+asc&wt=json").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "sorting on a fast dynamic-only field must succeed, got {status} / {body}"
    );
    assert_eq!(
        ids(&body),
        vec!["d2", "d3", "d1"],
        "must be ordered by ss_lang ascending (de, en, fr), got {body}"
    );

    // And the reverse direction is the exact reverse order.
    let (status, body) = get(&app, "select?q=*:*&sort=ss_lang+desc&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        ids(&body),
        vec!["d1", "d3", "d2"],
        "descending must be the exact reverse (fr, en, de), got {body}"
    );
}

// --- 2. facet.field on a field that only matches a dynamic pattern ---------

#[tokio::test]
async fn facet_field_on_a_fast_dynamic_only_field_reports_counts() {
    let (app, _dir) = dynamic_app(&json!([
        {"id": "d1", "ss_lang": "en"},
        {"id": "d2", "ss_lang": "en"},
        {"id": "d3", "ss_lang": "de"},
        {"id": "d4", "ss_lang": "fr"},
    ]))
    .await;

    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=ss_lang&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "faceting on a fast dynamic-only field must succeed, got {status} / {body}"
    );
    assert_eq!(
        flat_facet(&body, "ss_lang"),
        // Default `facet.sort` is by count descending, ties broken
        // alphabetically ascending (mirrors `tests/faceting.rs`'s documented
        // default-order behaviour): en:2, then de/fr tied at 1.
        expect_flat(&[("en", 2), ("de", 1), ("fr", 1)]),
        "facet_counts.facet_fields.ss_lang must report the real term counts, got {body}"
    );
}

// --- 3. control: a dynamic match WITHOUT `fast = true` is still refused ----

#[tokio::test]
async fn sort_on_a_non_fast_dynamic_only_field_is_still_a_400() {
    let (app, _dir) = dynamic_app(&json!([
        {"id": "d1", "sx_note": "alpha"},
        {"id": "d2", "sx_note": "beta"},
    ]))
    .await;

    let (status, body) = get(&app, "select?q=*:*&sort=sx_note+asc&wt=json").await;
    assert_bad_request(status, &body, "sx_note", "fast values (docValues)");
    assert!(
        body.get("response").is_none(),
        "a rejected sort must not also return a result set, got {body}"
    );
}

#[tokio::test]
async fn facet_field_on_a_non_fast_dynamic_only_field_is_still_a_400() {
    let (app, _dir) = dynamic_app(&json!([
        {"id": "d1", "sx_note": "alpha"},
        {"id": "d2", "sx_note": "beta"},
    ]))
    .await;

    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=sx_note&wt=json",
    )
    .await;
    assert_bad_request(status, &body, "sx_note", "fast values (docValues)");
    assert!(
        body.get("facet_counts").is_none(),
        "a rejected facet must not also carry a facet_counts block, got {body}"
    );
}
