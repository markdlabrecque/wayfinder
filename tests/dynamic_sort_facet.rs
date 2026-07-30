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
/// - `its_*` -> `int`, `fast = true` and `ds_*` -> `date`, `fast = true`:
///   the real motivating shapes for this issue. `search_api_solr` (Drupal's
///   Solr backend) names its dynamic fields by a type-prefix convention —
///   `ss_`/`its_`/`ds_` for string/integer/date — and none of those are
///   `[[fields]]` entries in a generated schema, only `[[dynamic_fields]]`
///   patterns. `ss_*` alone would leave the numeric/date shapes unpinned.
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

[[dynamic_fields]]
pattern = "its_*"
type = "int"
stored = true
fast = true

[[dynamic_fields]]
pattern = "ds_*"
type = "date"
stored = true
fast = true
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

// --- 4. sort/facet on a fast dynamic-only INT field -------------------------
//
// The actual motivating case: Drupal's `search_api_solr` names integer
// dynamic fields `its_*` and never declares them as `[[fields]]`.

#[tokio::test]
async fn sort_on_a_fast_dynamic_int_field_orders_results() {
    let (app, _dir) = dynamic_app(&json!([
        {"id": "d1", "its_count": 30},
        {"id": "d2", "its_count": 10},
        {"id": "d3", "its_count": 20},
    ]))
    .await;

    let (status, body) = get(&app, "select?q=*:*&sort=its_count+asc&wt=json").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "sorting on a fast dynamic-only int field must succeed, got {status} / {body}"
    );
    assert_eq!(
        ids(&body),
        vec!["d2", "d3", "d1"],
        "must be ordered by its_count ascending (10, 20, 30), got {body}"
    );

    let (status, body) = get(&app, "select?q=*:*&sort=its_count+desc&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        ids(&body),
        vec!["d1", "d3", "d2"],
        "descending must be the exact reverse (30, 20, 10), got {body}"
    );
}

#[tokio::test]
async fn facet_field_on_a_fast_dynamic_int_field_reports_counts() {
    let (app, _dir) = dynamic_app(&json!([
        {"id": "d1", "its_count": 10},
        {"id": "d2", "its_count": 10},
        {"id": "d3", "its_count": 20},
    ]))
    .await;

    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=its_count&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "faceting on a fast dynamic-only int field must succeed, got {status} / {body}"
    );
    assert_eq!(
        flat_facet(&body, "its_count"),
        // Default facet.sort is count desc, ties broken by index order:
        // 10:2, then 20:1.
        expect_flat(&[("10", 2), ("20", 1)]),
        "facet_counts.facet_fields.its_count must report the real term counts, got {body}"
    );
}

// --- 5. sort/facet on a fast dynamic-only DATE field ------------------------
//
// Drupal's `search_api_solr` names date dynamic fields `ds_*`, same story.

#[tokio::test]
async fn sort_on_a_fast_dynamic_date_field_orders_results() {
    let (app, _dir) = dynamic_app(&json!([
        {"id": "d1", "ds_created": "2020-01-03T00:00:00Z"},
        {"id": "d2", "ds_created": "2020-01-01T00:00:00Z"},
        {"id": "d3", "ds_created": "2020-01-02T00:00:00Z"},
    ]))
    .await;

    let (status, body) = get(&app, "select?q=*:*&sort=ds_created+asc&wt=json").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "sorting on a fast dynamic-only date field must succeed, got {status} / {body}"
    );
    assert_eq!(
        ids(&body),
        vec!["d2", "d3", "d1"],
        "must be ordered by ds_created ascending, got {body}"
    );

    let (status, body) = get(&app, "select?q=*:*&sort=ds_created+desc&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        ids(&body),
        vec!["d1", "d3", "d2"],
        "descending must be the exact reverse, got {body}"
    );
}

#[tokio::test]
async fn facet_field_on_a_fast_dynamic_date_field_reports_counts() {
    let (app, _dir) = dynamic_app(&json!([
        {"id": "d1", "ds_created": "2020-01-01T00:00:00Z"},
        {"id": "d2", "ds_created": "2020-01-01T00:00:00Z"},
        {"id": "d3", "ds_created": "2020-01-02T00:00:00Z"},
    ]))
    .await;

    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=ds_created&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "faceting on a fast dynamic-only date field must succeed, got {status} / {body}"
    );
    assert_eq!(
        flat_facet(&body, "ds_created"),
        expect_flat(&[("2020-01-01T00:00:00Z", 2), ("2020-01-02T00:00:00Z", 1),]),
        "facet_counts.facet_fields.ds_created must report the real term counts, got {body}"
    );
}

// --- 6. multi-segment: a segment with NO column for the dynamic field ------
//
// A dynamic-only column is a JSON path, only materialised in segments where
// some doc actually carried that key (`src/collector.rs`'s `SegmentSortColumn::
// Absent` doc comment). Index a first batch WITH the dynamic keys, commit
// (segment 1), then a second batch WITHOUT them, commit again (segment 2, no
// column at all for `ss_lang`/`its_count`) — this is the only way to make the
// `Absent` branch reachable for real, rather than resting on a comment.

#[tokio::test]
async fn sort_across_segments_with_and_without_the_dynamic_column_handles_missing_correctly() {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), DYNAMIC_SORT_FACET_SCHEMA_TOML)
        .expect("app must build");

    // Segment 1: every doc carries both dynamic fields.
    let (status, body) = post_docs(
        &app,
        &json!([
            {"id": "d1", "ss_lang": "fr", "its_count": 30},
            {"id": "d2", "ss_lang": "de", "its_count": 10},
            {"id": "d3", "ss_lang": "en", "its_count": 20},
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "batch 1 must index, got {body}");

    // Segment 2: neither doc carries either dynamic field, so this segment's
    // fast-field reader has no column at all for `ss_lang`/`its_count`.
    let (status, body) = post_docs(
        &app,
        &json!([
            {"id": "d4"},
            {"id": "d5"},
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "batch 2 must index, got {body}");

    // Numeric: missing-as-zero. Ascending puts the missing docs (value 0)
    // before every real value (10, 20, 30); descending puts them last.
    let (status, body) = get(&app, "select?q=*:*&sort=its_count+asc&wt=json").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let got = ids(&body);
    assert_eq!(
        got[2..],
        vec!["d2", "d3", "d1"],
        "docs with a real its_count must still be ordered 10, 20, 30 after the missing pair, got {body}"
    );
    assert_eq!(
        std::collections::BTreeSet::from_iter(got[..2].iter().cloned()),
        std::collections::BTreeSet::from(["d4".to_string(), "d5".to_string()]),
        "the segment-2 docs (no its_count column at all) must sort as 0, i.e. first ascending, got {body}"
    );

    let (status, body) = get(&app, "select?q=*:*&sort=its_count+desc&wt=json").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let got = ids(&body);
    assert_eq!(
        got[..3],
        vec!["d1", "d3", "d2"],
        "descending must still rank the real values 30, 20, 10 first, got {body}"
    );
    assert_eq!(
        std::collections::BTreeSet::from_iter(got[3..].iter().cloned()),
        std::collections::BTreeSet::from(["d4".to_string(), "d5".to_string()]),
        "the segment-2 docs must sort last (as 0) descending too, got {body}"
    );

    // String: missing-last in *both* directions.
    let (status, body) = get(&app, "select?q=*:*&sort=ss_lang+asc&wt=json").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let got = ids(&body);
    assert_eq!(
        got[..3],
        vec!["d2", "d3", "d1"],
        "docs with a real ss_lang must still sort de, en, fr ahead of the missing pair, got {body}"
    );
    assert_eq!(
        std::collections::BTreeSet::from_iter(got[3..].iter().cloned()),
        std::collections::BTreeSet::from(["d4".to_string(), "d5".to_string()]),
        "the segment-2 docs (no ss_lang column at all) must sort last ascending, got {body}"
    );

    let (status, body) = get(&app, "select?q=*:*&sort=ss_lang+desc&wt=json").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let got = ids(&body);
    assert_eq!(
        got[..3],
        vec!["d1", "d3", "d2"],
        "descending must still rank the real values fr, en, de first, got {body}"
    );
    assert_eq!(
        std::collections::BTreeSet::from_iter(got[3..].iter().cloned()),
        std::collections::BTreeSet::from(["d4".to_string(), "d5".to_string()]),
        "the segment-2 docs must sort last descending too (missing is never reordered by direction), got {body}"
    );
}

// --- 7. facet.range on a dynamic-only field is a deliberate, named refusal -
//
// `facet.range` needs the field's physical `Field` handle for
// `Term::from_field_i64` (see `check_facetable`'s doc comment in
// `src/facet.rs`), which a dynamic-only match has none of. This is an
// intentional scope limit for #66, not a bug — but it must fail with an
// honest message (the field DOES match, `sort`/`facet.field` both accept it),
// not "undefined field". If `facet.range` ever gains dynamic-field support,
// this test starts failing and someone has to consciously update/remove it.

#[tokio::test]
async fn facet_range_on_a_dynamic_only_field_is_refused_not_mislabeled_undefined() {
    let (app, _dir) = dynamic_app(&json!([
        {"id": "d1", "its_count": 10},
        {"id": "d2", "its_count": 20},
    ]))
    .await;

    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.range=its_count\
         &facet.range.start=0&facet.range.end=30&facet.range.gap=10&wt=json",
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "got {status} / {body}");
    let msg = body
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("error.msg must be present, got {body}"));
    assert!(
        msg.contains("its_count"),
        "error.msg must name the offending field, got: {msg}"
    );
    assert!(
        !msg.contains("undefined field"),
        "its_count matches a real dynamic rule (sort/facet.field both accept it) — \
         calling it undefined is misleading, got: {msg}"
    );
    assert!(
        msg.contains("dynamic"),
        "error.msg should name the actual scope limit (facet.range + dynamic fields), got: {msg}"
    );
}
