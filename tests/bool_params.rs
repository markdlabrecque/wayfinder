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
//! All nine cases below have committed fixtures. This file gives them readable
//! names, a facet-bucket-shaped assertion,
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

use common::{assert_matches_fixture, fixture, get, indexed_app, request};

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

/// Asserts an *error* response against its fixture.
///
/// The seven success cases in this file use `common::assert_matches_fixture`,
/// which compares the whole envelope. The two error cases cannot: Solr's
/// `error.metadata` carries Java class names
/// (`org.apache.solr.common.SolrException`), while Wayfinder deliberately
/// emits its own honest analogues (`wayfinder::InvalidBoolean`,
/// `wayfinder::FacetError`) -- `src/error.rs` documents those values as
/// outside the comparison contract, the retained fixture tests' normaliser drops
/// `error.metadata` outright, and `tests/error_shapes.rs` compares its *shape*
/// only. Matching the fixture wholesale would mean impersonating Solr's Java
/// class names, so this pins everything the fixture actually proves and
/// relaxes only the two metadata *values*.
///
/// Checked against the fixture: HTTP status, `responseHeader.status`, the
/// `responseHeader.params` echo, the presence *and* contents of the `response`
/// block (the error-timing split this file exists to cover), `error.msg`
/// verbatim, `error.code`, and `error.metadata`'s shape -- a four-element flat
/// array whose keys match and whose values are non-empty strings.
fn assert_error_matches_fixture(status: StatusCode, body: &Value, fixture_name: &str) {
    let expected = fixture(fixture_name);

    let want_code = expected["error"]["code"]
        .as_i64()
        .unwrap_or_else(|| panic!("fixture {fixture_name} has no error.code"));
    assert_eq!(
        status.as_u16() as i64,
        want_code,
        "HTTP status must equal the fixture's error.code ({fixture_name}); got {body}"
    );
    assert_eq!(
        body["error"]["code"].as_i64(),
        Some(want_code),
        "error.code must match the fixture ({fixture_name}); got {body}"
    );
    assert_eq!(
        body.pointer("/responseHeader/status")
            .and_then(Value::as_i64),
        expected
            .pointer("/responseHeader/status")
            .and_then(Value::as_i64),
        "responseHeader.status must match the fixture ({fixture_name}); got {body}"
    );

    // The params echo is compared in full -- `serde_json`'s `Map` equality is
    // order-independent, so Solr's own key ordering does not have to be
    // reproduced, but every key and raw string value does.
    assert_eq!(
        body.pointer("/responseHeader/params"),
        expected.pointer("/responseHeader/params"),
        "responseHeader.params must match the fixture ({fixture_name}); got {body}"
    );

    // Presence *and* contents of the `response` block: this is the error-timing
    // split (pre-query `facet` has none, post-query `facet.missing` carries the
    // base query's real hits). No `_version_`/`_root_` normalisation is needed
    // -- both fixtures were captured with `rows=0`, so `docs` is empty.
    assert_eq!(
        body.get("response"),
        expected.get("response"),
        "the `response` block must match the fixture exactly, presence included \
         ({fixture_name}); got {body}"
    );

    assert_eq!(
        body.pointer("/error/msg").and_then(Value::as_str),
        expected.pointer("/error/msg").and_then(Value::as_str),
        "error.msg must match the fixture verbatim ({fixture_name}); got {body}"
    );

    // metadata: flat alternating array, same length, same keys; the values are
    // Java class names in Solr and Wayfinder-honest strings here, so they are
    // asserted non-empty rather than compared (same contract as
    // `tests/error_shapes.rs`).
    let want_meta = expected["error"]["metadata"]
        .as_array()
        .unwrap_or_else(|| panic!("fixture {fixture_name} has no error.metadata array"));
    let got_meta = body["error"]["metadata"].as_array().unwrap_or_else(|| {
        panic!("{fixture_name}: error.metadata must be a flat array; got {body}")
    });
    assert_eq!(
        got_meta.len(),
        want_meta.len(),
        "error.metadata length must match the fixture ({fixture_name}); got {body}"
    );
    for (i, want) in want_meta.iter().enumerate().step_by(2) {
        assert_eq!(
            got_meta[i].as_str(),
            want.as_str(),
            "error.metadata key at index {i} must match the fixture ({fixture_name}); got {body}"
        );
        assert!(
            got_meta[i + 1].as_str().is_some_and(|v| !v.is_empty()),
            "error.metadata value at index {} must be a non-empty string ({fixture_name}); \
             got {body}",
            i + 1
        );
    }

    // Nothing beyond the keys the fixture itself has may appear.
    let want_keys: Vec<&str> = expected
        .as_object()
        .expect("fixture must be a JSON object")
        .keys()
        .map(String::as_str)
        .collect();
    let got_keys: Vec<&str> = body
        .as_object()
        .expect("response must be a JSON object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        got_keys, want_keys,
        "the envelope's top-level keys, in order, must match the fixture ({fixture_name})"
    );
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
    // Not `assert_matches_fixture`: the fixture's `error.metadata` holds Solr's
    // Java class names, which Wayfinder deliberately does not impersonate --
    // see `assert_error_matches_fixture`'s doc comment. Everything else,
    // including the `response` block this test is about, is still compared.
    assert_error_matches_fixture(status, &body, "bool_facet_missing_invalid");
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
    // Not `assert_matches_fixture`: the fixture's `error.metadata` holds Solr's
    // Java class names, which Wayfinder deliberately does not impersonate --
    // see `assert_error_matches_fixture`'s doc comment. The absent `response`
    // block this test is about is still compared against the fixture's.
    assert_error_matches_fixture(status, &body, "bool_facet_invalid");
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

// --- unfixtured guards: every remaining boolean read, and every handler -----
//
// Nothing below has a Solr fixture, and that is deliberate rather than a gap.
// Finding 115 pins the *rule*; these pin that each read site actually applies
// it. The code they guard is validation-only, which `CLAUDE.md` requires be
// mutation-tested: deleting the guard must break a test, not merely fail to be
// exercised. Each test's doc comment names the mutation it was verified to
// kill -- a claim worth distrusting until re-checked, since an earlier revision
// of this file asserted a mutation guard over code that turned out to be
// unreachable.
//
// The expected message is Solr's verbatim wording from the two captured error
// fixtures (`invalid boolean value: <raw>`); the status is the 400 those
// fixtures record. What is unfixtured is only *which param on which endpoint*,
// not the shape of the answer.

/// Asserts an invalid boolean anywhere answers Solr's 400 with the raw value
/// named. `error.metadata` is not inspected here -- the two fixtured cases
/// above already pin its shape, and these have no fixture to compare against.
fn assert_invalid_bool_400(status: StatusCode, body: &Value, raw: &str, what: &str) {
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "{what}: an invalid boolean must be a 400; got {body}"
    );
    assert_eq!(
        body.pointer("/error/msg").and_then(Value::as_str),
        Some(format!("invalid boolean value: {raw}").as_str()),
        "{what}: error.msg must name the invalid raw value verbatim; got {body}"
    );
    assert_eq!(
        body.pointer("/error/code").and_then(Value::as_i64),
        Some(400),
        "{what}: error.code must be 400; got {body}"
    );
}

/// [`assert_invalid_bool_400`] plus issue #214's suppression policy: an
/// invalid `omitHeader` answers with **no `responseHeader` at all**.
///
/// That is what distinguishes this path from every other invalid boolean.
/// `check_params`'s error calls `.suppress_response_header()`; the ordinary
/// `Params::bool_or` error does not. Asserting the absence here therefore pins
/// *which* validation answered -- if the shared boolean parser ever stopped
/// feeding `validate_omit_header` and some other read site started 400ing
/// first, the message would still match but the header would come back.
fn assert_invalid_omit_header_400(status: StatusCode, body: &Value, raw: &str, what: &str) {
    assert_invalid_bool_400(status, body, raw, what);
    assert!(
        body.get("responseHeader").is_none(),
        "{what}: an invalid omitHeader must suppress responseHeader entirely \
         (issue #214's policy, reached through the #187 shared parser); got {body}"
    );
}

/// **Regression test for a short-circuit bug.** `/update` reads `commit` and
/// `softCommit` and treats either as "commit now". Written as
/// `bool_or("commit")? || bool_or("softCommit")?` the `||` short-circuits, so
/// with `commit=true` the second value is never parsed at all and
/// `softCommit=nope` sails through with a 200 -- silently accepting an invalid
/// boolean, the exact behaviour issue #187 exists to remove. Both must be
/// validated regardless of the other's value, so both orderings are checked.
#[tokio::test]
async fn update_validates_both_commit_booleans_even_when_the_first_is_true() {
    let (app, _dir) = indexed_app().await;
    let body_json = r#"[{"id":"bool1","body":"hello"}]"#;

    let (status, body) = request(
        &app,
        "POST",
        "update?commit=true&softCommit=nope&wt=json",
        Some(body_json),
    )
    .await;
    assert_invalid_bool_400(status, &body, "nope", "update?commit=true&softCommit=nope");

    // The mirror image: a valid `softCommit` must not excuse an invalid
    // `commit` either.
    let (status, body) = request(
        &app,
        "POST",
        "update?commit=nope&softCommit=true&wt=json",
        Some(body_json),
    )
    .await;
    assert_invalid_bool_400(status, &body, "nope", "update?commit=nope&softCommit=true");
}

/// `/update`'s own booleans, each rejected on its own. `overwrite` is the one
/// whose Solr default is `true`, so a defaulting bug there would be invisible
/// without an explicit invalid-value case.
#[tokio::test]
async fn update_rejects_each_invalid_boolean() {
    let (app, _dir) = indexed_app().await;
    let body_json = r#"[{"id":"bool2","body":"hello"}]"#;
    for (param, raw) in [
        ("commit", "nope"),
        ("softCommit", "maybe"),
        ("overwrite", "1"),
    ] {
        let (status, body) = request(
            &app,
            "POST",
            &format!("update?{param}={raw}&wt=json"),
            Some(body_json),
        )
        .await;
        assert_invalid_bool_400(status, &body, raw, &format!("update?{param}={raw}"));
    }
}

/// `/mlt`'s two booleans. `mlt.match.include` is the other default-`true`
/// param, same reasoning as `overwrite` above.
#[tokio::test]
async fn mlt_rejects_each_invalid_boolean() {
    let (app, _dir) = indexed_app().await;
    for (param, raw) in [("mlt.boost", "1"), ("mlt.match.include", "nope")] {
        let (status, body) = get(
            &app,
            &format!("mlt?q=id:doc1&mlt.fl=body&{param}={raw}&wt=json"),
        )
        .await;
        assert_invalid_bool_400(status, &body, raw, &format!("mlt?{param}={raw}"));
    }
}

/// `/terms`'s own gate param.
#[tokio::test]
async fn terms_rejects_an_invalid_boolean() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "terms?terms=nope&terms.fl=body&wt=json").await;
    assert_invalid_bool_400(status, &body, "nope", "terms?terms=nope");
}

/// `/select`'s three gate params, each rejected on its own. `facet=1` is the
/// fixtured case above; `stats` and `hl` are the same read site pattern with
/// no fixture of their own.
#[tokio::test]
async fn select_rejects_each_invalid_boolean() {
    let (app, _dir) = indexed_app().await;
    for (param, raw) in [("facet", "1"), ("stats", "nope"), ("hl", "y")] {
        let (status, body) = get(&app, &format!("select?q=*:*&rows=0&{param}={raw}&wt=json")).await;
        assert_invalid_bool_400(status, &body, raw, &format!("select?{param}={raw}"));
    }
}

/// **Guards the single `check_params` `omitHeader` validation, reached through
/// four different allowlists.**
///
/// There is exactly one validation site, not four. `check_params` (`src/lib.rs`)
/// calls `Params::validate_omit_header` whenever the endpoint's allowlist
/// contains the name, and `SELECT_PARAMS`/`UPDATE_PARAMS`/`MLT_PARAMS`/
/// `TERMS_PARAMS` all do -- so what these four requests prove is that each
/// endpoint reaches that one check, not that each has a check of its own.
///
/// An earlier revision of this file claimed to guard four per-handler
/// `bool_or("omitHeader", …)` calls and said "delete any one of the four and
/// this test fails for that endpoint alone". That was false: `check_params`
/// runs first in all four handlers, so those calls were unreachable and
/// deleting all four changed nothing. They have since been removed.
///
/// Mutation actually killed, verified by doing it: deleting the
/// `if allowed.contains(&"omitHeader") { … validate_omit_header … }` block in
/// `check_params` fails this test on all four endpoints at once. Narrowing the
/// guard to one allowlist fails it for the other three. Dropping the
/// `.suppress_response_header()` on that error fails the header assertion in
/// [`assert_invalid_omit_header_400`] while leaving status and message intact.
///
/// The response *body* is Wayfinder's own JSON envelope, not a captured Solr
/// one, and deliberately so (finding 115): real Solr answers an invalid
/// `omitHeader` with a Jetty HTML error page, because header suppression is
/// decided before the JSON response writer exists. Only the status is shared,
/// which is why there is no fixture and no the captured fixture request set row. Solr's *message*
/// is still matched, from the captured
/// `solr-ref/responses/omit_header_invalid_one.html`.
#[tokio::test]
async fn every_handler_rejects_an_invalid_omit_header() {
    let (app, _dir) = indexed_app().await;

    let (status, body) = get(&app, "select?q=*:*&rows=0&omitHeader=1&wt=json").await;
    assert_invalid_omit_header_400(status, &body, "1", "select?omitHeader=1");

    let (status, body) = get(&app, "mlt?q=id:doc1&mlt.fl=body&omitHeader=1&wt=json").await;
    assert_invalid_omit_header_400(status, &body, "1", "mlt?omitHeader=1");

    let (status, body) = get(&app, "terms?terms=true&terms.fl=body&omitHeader=1&wt=json").await;
    assert_invalid_omit_header_400(status, &body, "1", "terms?omitHeader=1");

    let (status, body) = request(
        &app,
        "POST",
        "update?omitHeader=1&wt=json",
        Some(r#"[{"id":"bool3","body":"hello"}]"#),
    )
    .await;
    assert_invalid_omit_header_400(status, &body, "1", "update?omitHeader=1");
}
