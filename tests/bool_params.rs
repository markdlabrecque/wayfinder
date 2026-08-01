//! Boolean param parsing (issue #187): Wayfinder's boolean request-param
//! reads must match real Solr 9's `StrUtils.parseBool`, not the stricter
//! `== "true"` / `starts_with("true")` checks the codebase used before this
//! issue.
//!
//! **The issue's own premise is wrong and this file does not build to it.**
//! It claims Solr accepts `1`/`0`/`t`/`f`/`y` -- measured behaviour, captured
//! against real `solr:9` (port 8996, 2026-08-01) and recorded in
//! `docs/solr-ref-findings.md`, is:
//! - `true` if the lowercased value starts with `true`, `on`, or `yes`
//! - `false` if it starts with `false` or `off`, or equals `no` exactly
//! - anything else, including the empty string, is a 400 with
//!   `error.msg = "invalid boolean value: <raw>"`
//!
//! All nine cases below have `solr-ref/manifest.tsv` rows, so
//! `cargo test --test differential` is the primary gate; this file exists to
//! give the same nine cases readable names, a facet-bucket-shaped assertion,
//! and coverage of the two different error-envelope shapes (`facet=1` is
//! read before the base query runs, so the error is envelope-only; the
//! `facet.missing=nope` case is read inside faceting, after the base query
//! has already run, so the response block from that query rides alongside
//! `error` -- issue #35's shape).
//!
//! Expected values come from `solr-ref/responses/bool_*.json` throughout,
//! never from what the implementation happens to produce.

// The `dead_code` allow for partially-used shared helpers is an inner
// attribute inside `tests/common/mod.rs`; repeating it here is a clippy
// error under `-D warnings`.
mod common;

use axum::http::StatusCode;
use serde_json::Value;

use common::{assert_matches_fixture, fixture, get, indexed_app};

/// `facet_counts.facet_fields.<label>` as the flat alternating array Solr
/// uses, or `None` when the label is absent entirely. Mirrors
/// `tests/facet_field_missing_override.rs`'s helper of the same name --
/// duplicated rather than shared, per this repo's established convention
/// that `tests/common/mod.rs` cannot be shared across integration test
/// binaries (see that file's own comment on the point).
fn facet_bucket(body: &Value, label: &str) -> Option<Vec<Value>> {
    body.pointer(&format!("/facet_counts/facet_fields/{label}"))
        .map(|v| {
            v.as_array()
                .unwrap_or_else(|| {
                    panic!("facet_counts.facet_fields.{label} must be a flat array, got: {body}")
                })
                .clone()
        })
}

/// The flat counts array a fixture recorded under `label`.
fn fixture_bucket(fixture_name: &str, label: &str) -> Vec<Value> {
    facet_bucket(&fixture(fixture_name), label).unwrap_or_else(|| {
        panic!("fixture {fixture_name} has no facet_counts.facet_fields.{label}")
    })
}

// --- facet.missing: true-family values (start-with, case-insensitive) ------

/// `facet.missing=TRUE` (uppercase) must still add the null bucket -- the
/// parser is case-insensitive. Matches `bool_facet_missing_upper_true.json`.
#[tokio::test]
async fn facet_missing_upper_true_adds_the_null_bucket() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&facet.missing=TRUE&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "category").as_deref(),
        Some(fixture_bucket("bool_facet_missing_upper_true", "category").as_slice()),
        "facet.missing=TRUE must add the null bucket; got {body}"
    );
    assert_matches_fixture(body, "bool_facet_missing_upper_true");
}

/// `facet.missing=yes` must add the null bucket -- the `yes` family, not just
/// `true`. Matches `bool_facet_missing_yes.json`.
#[tokio::test]
async fn facet_missing_yes_adds_the_null_bucket() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&facet.missing=yes&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "category").as_deref(),
        Some(fixture_bucket("bool_facet_missing_yes", "category").as_slice()),
        "facet.missing=yes must add the null bucket; got {body}"
    );
    assert_matches_fixture(body, "bool_facet_missing_yes");
}

/// `facet.missing=on` must add the null bucket -- the `on` family. Matches
/// `bool_facet_missing_on.json`.
#[tokio::test]
async fn facet_missing_on_adds_the_null_bucket() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&facet.missing=on&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "category").as_deref(),
        Some(fixture_bucket("bool_facet_missing_on", "category").as_slice()),
        "facet.missing=on must add the null bucket; got {body}"
    );
    assert_matches_fixture(body, "bool_facet_missing_on");
}

/// `facet.missing=truestuff` -- a `true`-*prefixed* value that is not the
/// exact word -- must still add the null bucket. This is the sharpest test
/// of "starts with", since `truestuff` would fail an exact `== "true"`
/// comparison. Matches `bool_facet_missing_prefix.json`.
#[tokio::test]
async fn facet_missing_true_prefixed_value_adds_the_null_bucket() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&facet.missing=truestuff&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "category").as_deref(),
        Some(fixture_bucket("bool_facet_missing_prefix", "category").as_slice()),
        "facet.missing=truestuff (true-prefixed, not exact) must add the null bucket; got {body}"
    );
    assert_matches_fixture(body, "bool_facet_missing_prefix");
}

// --- facet.missing: the false exception (`no` exactly) ---------------------

/// `facet.missing=no` must NOT add the null bucket -- `no` parses false
/// (exact match, not a `false`/`off` prefix). Matches
/// `bool_facet_missing_no.json`. Also confirms the false path is not
/// accidentally 400ing: this and the true-family tests above are the two
/// halves of "valid but not `true`/`false` literally".
#[tokio::test]
async fn facet_missing_no_does_not_add_the_null_bucket() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&facet.missing=no&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "category").as_deref(),
        Some(fixture_bucket("bool_facet_missing_no", "category").as_slice()),
        "facet.missing=no must not add a null bucket; got {body}"
    );
    assert_matches_fixture(body, "bool_facet_missing_no");
}

// --- facet.missing: invalid, and the response-block-carrying error shape ---

/// `facet.missing=nope` is invalid -- not `no` exactly, and not `false`/`off`
/// prefixed. `facet.missing` is read inside `facet::facet_counts`, *after*
/// the base query has already run, so the error envelope must carry the
/// query's own `response` block alongside `error` (issue #35's shape,
/// `WfError::with_response`) -- unlike the pre-query `facet=1` case below.
/// Matches `bool_facet_missing_invalid.json`.
#[tokio::test]
async fn facet_missing_nope_is_invalid_and_the_response_block_survives() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&facet.missing=nope&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert!(
        body.get("response").is_some(),
        "facet.missing is read after the base query has already run, so its error envelope \
         must still carry the response block (issue #35's shape); got {body}"
    );
    assert_eq!(
        body.pointer("/error/msg").and_then(Value::as_str),
        Some("invalid boolean value: nope"),
        "error.msg must name the invalid raw value verbatim; got {body}"
    );
    assert_matches_fixture(body, "bool_facet_missing_invalid");
}

// --- facet: the true-family value that is not the exact word `true` --------

/// `facet=on` must turn faceting on -- `facet_counts` must be present, using
/// the `on`-family value rather than the literal `true` every other test in
/// this repo happens to send. Matches `bool_facet_on.json`.
#[tokio::test]
async fn facet_on_enables_faceting() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=on&facet.field=category&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert!(
        body.get("facet_counts").is_some(),
        "facet=on must enable faceting; got {body}"
    );
    assert_eq!(
        facet_bucket(&body, "category").as_deref(),
        Some(fixture_bucket("bool_facet_on", "category").as_slice()),
        "got {body}"
    );
    assert_matches_fixture(body, "bool_facet_on");
}

// --- facet: invalid, and the pre-query error-only envelope shape -----------

/// `facet=1` is invalid (the issue's own wrong premise -- Solr does NOT
/// accept `1`). `facet` is read before the base query even runs, so the
/// error envelope must carry no `response` block at all -- the opposite
/// shape from `facet.missing=nope` above. Matches `bool_facet_invalid.json`.
#[tokio::test]
async fn facet_equals_1_is_invalid_and_the_envelope_has_no_response_block() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=1&facet.field=category&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert!(
        body.get("response").is_none(),
        "facet is read before the base query runs, so its error envelope must carry no \
         response block; got {body}"
    );
    assert_eq!(
        body.pointer("/error/msg").and_then(Value::as_str),
        Some("invalid boolean value: 1"),
        "error.msg must name the invalid raw value verbatim; got {body}"
    );
    assert_matches_fixture(body, "bool_facet_invalid");
}

// --- omitHeader: a true-family value that is not the literal `true` --------

/// `omitHeader=yes` must suppress `responseHeader` entirely, same as the
/// literal `omitHeader=true` `tests/omit_header.rs` already covers -- this
/// is `omit_header()`'s own documented divergence from every other boolean
/// read in the codebase (`src/params.rs`'s doc comment), now settled by this
/// fixture. Matches `bool_omit_header_yes.json`.
#[tokio::test]
async fn omit_header_yes_suppresses_the_response_header() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=*:*&rows=0&omitHeader=yes&wt=json").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert!(
        body.get("responseHeader").is_none(),
        "omitHeader=yes must suppress responseHeader entirely, got {body}"
    );
    assert_matches_fixture(body, "bool_omit_header_yes");
}
