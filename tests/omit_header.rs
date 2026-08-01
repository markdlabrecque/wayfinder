//! `omitHeader`/`TZ` (issue #143) — `search_api_solr` sends
//! `omitHeader=true&TZ=UTC` on essentially every `/select` and `/mlt`
//! request (`solr-ref/search-api/trace/00002` through `00022`), plus
//! `omitHeader=false` on `/update` (`solr-ref/search-api/trace/00001`).
//! Neither param is registered in `SELECT_PARAMS`/`MLT_PARAMS`/
//! `UPDATE_PARAMS` today, so `strict_params = true` 400s the exact request
//! shape the module sends, and — worse — `omitHeader=true` is silently
//! ignored on `/select`/`/mlt`, so Wayfinder always answers with a
//! `responseHeader` the client explicitly asked to suppress. Every trace
//! that sends `omitHeader=true` has a captured response with **no**
//! `responseHeader` key at all (confirmed by inspection of the 20 traces
//! that send it: `00002`-`00019`, `00021`, `00022`).
//!
//! `GET /solr/{core}/terms` (issue #155) already implements this correctly
//! (`src/lib.rs::terms`, guarded by `tests/terms.rs`'s
//! `terms_omit_header_true_suppresses_response_header` and friends) — not
//! duplicated here.
//!
//! ## Premises verified before writing these tests
//!
//! 1. **Core claim confirmed against the fixtures, not the ticket**: every
//!    trace sending `omitHeader=true` (20 of them: `00002`-`00019`, `00021`
//!    on `/select`; `00022` on `/mlt`) has a response body with no
//!    top-level `responseHeader` key. `00001` (`/update`) sends
//!    `omitHeader=false` and its response *does* carry `responseHeader`.
//! 2. **Issue #179 settles error responses from real Solr 9.10.1.**
//!    `omit_header_error_true.json` and `omit_header_update_error_true.json`
//!    show `omitHeader=true` suppressing `responseHeader` on both `/select`'s
//!    params-echoing error and `/update`'s no-params error. The corresponding
//!    rows in `manifest-errors.tsv` keep both paths in the differential gate.
//! 3. **No `EXPECTED_DIVERGENCES` entry masks the implemented behavior.** The
//!    JSON error fixtures are ordinary differential rows. The one deliberate
//!    exception is invalid boolean syntax: Solr fails before its JSON writer
//!    and returns Jetty HTML, while Wayfinder keeps its JSON-only contract;
//!    PRD ratified divergence 8 records that choice and the dedicated test
//!    below guards the status, JSON content type, and headerless shape.
//!    `src/coverage.rs`'s
//!    `"request.omitHeader"`/`"request.timezone.utc"` runtime probes
//!    (exercised under `strict_params = true`) already exist and already
//!    check exactly this, and were carried on `tests/search_api_coverage.rs`'s
//!    self-expiring uncovered list — removed there as part of this change
//!    (see that file's diff), which is the sharper, executable form of "no
//!    `EXPECTED_DIVERGENCES` entry to delete because none exists".
//! 4. **Which endpoints get `omitHeader`/`TZ` from the module, read off all
//!    28 traces' request paths**: `/select` and `/mlt` get both
//!    `omitHeader=true` and `TZ=UTC`. `/update` gets `omitHeader=false`
//!    only (no `TZ`). `/terms` gets `omitHeader=true` only (no `TZ`,
//!    already registered). None of `/admin/luke`, `/admin/mbeans`,
//!    `/admin/info/system`, `/<core>/admin/system`, `/schema/fieldtypes`
//!    ever receives `omitHeader` or `TZ` from the module at all — so this
//!    ticket's `SELECT_PARAMS`/`MLT_PARAMS`/`UPDATE_PARAMS` registrations
//!    are the whole allowlist surface; the admin allowlists are
//!    deliberately untouched. `admin_luke_omit_header_is_not_a_registered_param_and_leaves_the_envelope_unconditional`
//!    below pins that scope limit with a real assertion, per this ticket's
//!    own acceptance criterion ("if scoped ... that limit is stated and
//!    guarded by a test"). Trace `00025` (mbeans)'s glued
//!    `stats=true?omitHeader=false` malformed-param shape is untouched by
//!    this file — `tests/admin_mbeans.rs` already covers it.
//! 5. **Open question 3 (`TZ`): no date-math or date-facet path exists
//!    that a timezone could change.** Wayfinder has no `date` field type,
//!    no `NOW`/date-math query syntax, and no `facet.range` on a temporal
//!    type in `src/schema.rs`/`src/query.rs`/`src/facet.rs` (`facet.range`
//!    only appears wired to `its_field_rating`-style numeric ranges in the
//!    captured configset and in `tests/faceting.rs`). `TZ` is therefore
//!    accept-and-ignore, pinned by the `_and_tz_...` tests below (they send
//!    `TZ=UTC` and assert only that it does not 400 or change results, never
//!    that it changes anything).
//!
//! ## Interpretation this file has to make: `/update`
//!
//! No trace ever sends `omitHeader=true` to `/update` (only `00001`'s
//! `omitHeader=false`), so there is no captured fixture proving `/update`
//! suppresses `responseHeader` on `omitHeader=true`. This file still pins
//! that generalization (`update_omit_header_true_suppresses_response_header_entirely`
//! below) rather than leaving `/update` unconditional, for the same reason
//! `tests/terms.rs` picked its pre-query-error precedent for an unfixtured
//! case: it is the reading consistent with every other envelope-returning
//! success path in this codebase (`/select`, `/mlt`, `/terms` all gate on
//! the same params-level `omitHeader=true` check), and the two readings
//! cannot both be right. If a future capture disagrees, this is the test to
//! change.

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{CORE, corpus, fixture, get, indexed_app};
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

/// Builds an app with the given `wayfinder.toml` contents (or defaults when
/// `None`), indexes the tracer-bullet corpus, and returns the router plus
/// the `TempDir` guard. Mirrors `tests/server_config.rs::indexed_app_with_config`
/// and `tests/mlt.rs`'s inline strict_params setup — duplicated rather than
/// shared, since `tests/common/mod.rs` cannot be shared across integration
/// test binaries (established precedent, see `tests/mlt.rs`'s own comment on
/// the same point).
async fn indexed_app_with_config(config: Option<&str>) -> (Router, TempDir) {
    let dir = TempDir::new().expect("create temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, common::SCHEMA_TOML).expect("write schema.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");

    let app = match config {
        Some(toml) => {
            let config_path = dir.path().join("wayfinder.toml");
            std::fs::write(&config_path, toml).expect("write wayfinder.toml");
            wayfinder::app_with_config(&schema_path, &data_dir, &config_path)
                .expect("app must build")
        }
        None => wayfinder::app(&schema_path, &data_dir).expect("app must build"),
    };

    let req = Request::builder()
        .method("POST")
        .uri(format!("/solr/{CORE}/update?commit=true"))
        .header("content-type", "application/json")
        .body(Body::from(corpus().to_string()))
        .unwrap();
    let resp = app
        .clone()
        .oneshot(req)
        .await
        .expect("update request must not fail at the transport level");
    assert_eq!(resp.status(), StatusCode::OK, "indexing must succeed");
    (app, dir)
}

// --- /select ----------------------------------------------------------------

/// Ground truth: `solr-ref/search-api/trace/00003.json` sends
/// `?omitHeader=true&TZ=UTC&wt=json&...` and its response has no
/// `responseHeader` key at all. This exercises the same shape (minus the
/// edismax specifics, which are covered elsewhere) against the tracer-bullet
/// corpus.
#[tokio::test]
async fn select_omit_header_true_suppresses_response_header() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=*:*&omitHeader=true&TZ=UTC&wt=json").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert!(
        body.get("responseHeader").is_none(),
        "omitHeader=true must suppress responseHeader entirely, got {body}"
    );
    assert!(
        body.get("response").is_some(),
        "the response block itself must still be present, got {body}"
    );
}

/// `omitHeader=true` must suppress the header only — the `response` block
/// underneath it must be the same content real Solr returns for the identical
/// query with the header present (`select_all.json`), modulo the standard
/// `normalize_envelope` allowances (`_version_`/`_root_`, which Wayfinder has
/// no equivalent of by an explicit default-`fl` decision — findings fact 9,
/// PRD section 7).
#[tokio::test]
async fn select_omit_header_true_leaves_response_block_unaffected() {
    let (app, _dir) = indexed_app().await;
    let (_status, body) = get(&app, "select?q=*:*&rows=10&omitHeader=true&wt=json").await;
    let body = common::normalize_envelope(body);
    let expected = common::normalize_envelope(fixture("select_all"));
    assert_eq!(
        body.get("response"),
        expected.get("response"),
        "suppressing responseHeader must not change the response block, got {body}"
    );
}

#[tokio::test]
async fn select_response_header_present_when_omit_header_absent() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=*:*&rows=10&wt=json").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert!(
        body.get("responseHeader").is_some(),
        "responseHeader must be present when omitHeader is absent, got {body}"
    );
}

#[tokio::test]
async fn select_response_header_present_when_omit_header_false() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=*:*&rows=10&omitHeader=false&wt=json").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert!(
        body.get("responseHeader").is_some(),
        "responseHeader must be present when omitHeader=false, got {body}"
    );
}

/// `strict_params = true` must not 400 the exact `omitHeader`/`TZ` shape
/// `search_api_solr` sends on essentially every `/select` request — the
/// bug this ticket exists to fix, per the task spec's framing.
#[tokio::test]
async fn select_omit_header_and_tz_are_registered_params_under_strict_params() {
    let (app, _dir) = indexed_app_with_config(Some("strict_params = true\n")).await;
    let (status, body) = get(&app, "select?q=*:*&omitHeader=true&TZ=UTC&wt=json").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "omitHeader/TZ must be registered SELECT_PARAMS so strict_params does not 400 the \
         request shape search_api_solr actually sends, got {body}"
    );
}

// --- /mlt --------------------------------------------------------------------

/// Ground truth: `solr-ref/search-api/trace/00022.json` sends
/// `?omitHeader=true&TZ=UTC&wt=json&...` to `/mlt` and its response has no
/// `responseHeader` key.
#[tokio::test]
async fn mlt_omit_header_true_suppresses_response_header() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "mlt?q=id:doc1&mlt.fl=body&omitHeader=true&TZ=UTC&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert!(
        body.get("responseHeader").is_none(),
        "omitHeader=true must suppress responseHeader entirely on /mlt, got {body}"
    );
    assert!(
        body.get("match").is_some(),
        "the match block itself must still be present, got {body}"
    );
}

#[tokio::test]
async fn mlt_response_header_present_when_omit_header_absent() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "mlt?q=id:doc1&mlt.fl=body&wt=json").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert!(
        body.get("responseHeader").is_some(),
        "responseHeader must be present when omitHeader is absent, got {body}"
    );
}

#[tokio::test]
async fn mlt_response_header_present_when_omit_header_false() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "mlt?q=id:doc1&mlt.fl=body&omitHeader=false&wt=json").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert!(
        body.get("responseHeader").is_some(),
        "responseHeader must be present when omitHeader=false, got {body}"
    );
}

/// `strict_params = true` must not 400 the exact `omitHeader`/`TZ` shape
/// `search_api_solr` sends on essentially every `/mlt` request.
#[tokio::test]
async fn mlt_omit_header_and_tz_are_registered_params_under_strict_params() {
    let (app, _dir) = indexed_app_with_config(Some("strict_params = true\n")).await;
    let (status, body) = get(
        &app,
        "mlt?q=id:doc1&mlt.fl=body&omitHeader=true&TZ=UTC&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "omitHeader/TZ must be registered MLT_PARAMS so strict_params does not 400 the request \
         shape search_api_solr actually sends, got {body}"
    );
}

// --- /update -----------------------------------------------------------------

/// Ground truth: `solr-ref/search-api/trace/00001.json` sends
/// `omitHeader=false` to `/update` and its response carries `responseHeader`
/// (the bare `{"responseHeader":{"status":0,"QTime":...}}` envelope, no
/// other keys — finding 46).
#[tokio::test]
async fn update_response_header_present_when_omit_header_false() {
    let (app, _dir) = indexed_app().await;
    let req = Request::builder()
        .method("POST")
        .uri(format!(
            "/solr/{CORE}/update?commit=true&omitHeader=false&wt=json"
        ))
        .header("content-type", "application/json")
        .body(Body::from("[]"))
        .unwrap();
    let resp = app.clone().oneshot(req).await.expect("must not fail");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body must be readable");
    let body: Value = serde_json::from_slice(&bytes).expect("body must be valid JSON");
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert!(
        body.get("responseHeader").is_some(),
        "responseHeader must be present when omitHeader=false, got {body}"
    );
}

#[tokio::test]
async fn update_response_header_present_when_omit_header_absent() {
    let (app, _dir) = indexed_app().await;
    let req = Request::builder()
        .method("POST")
        .uri(format!("/solr/{CORE}/update?commit=true"))
        .header("content-type", "application/json")
        .body(Body::from("[]"))
        .unwrap();
    let resp = app.clone().oneshot(req).await.expect("must not fail");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body must be readable");
    let body: Value = serde_json::from_slice(&bytes).expect("body must be valid JSON");
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert!(
        body.get("responseHeader").is_some(),
        "responseHeader must be present when omitHeader is absent, got {body}"
    );
}

/// No captured trace exercises `omitHeader=true` on `/update` — this is the
/// interpretation documented in the module doc comment above: generalizing
/// the same gate every other success envelope uses, on the theory that the
/// two readings ("suppress" vs. "never suppress on /update") cannot both be
/// right and this is the only one consistent with `/select`/`/mlt`/`/terms`.
#[tokio::test]
async fn update_omit_header_true_suppresses_response_header_entirely() {
    let (app, _dir) = indexed_app().await;
    let req = Request::builder()
        .method("POST")
        .uri(format!(
            "/solr/{CORE}/update?commit=true&omitHeader=true&wt=json"
        ))
        .header("content-type", "application/json")
        .body(Body::from("[]"))
        .unwrap();
    let resp = app.clone().oneshot(req).await.expect("must not fail");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body must be readable");
    let body: Value = serde_json::from_slice(&bytes).expect("body must be valid JSON");
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        body,
        serde_json::json!({}),
        "omitHeader=true must suppress /update's bare envelope entirely, leaving an empty \
         object (no other keys exist in the bare success shape to survive), got {body}"
    );
}

/// `strict_params = true` must not 400 `omitHeader` on `/update` — the
/// module sends `omitHeader=false` on every `/update` request
/// (`00001.json`).
#[tokio::test]
async fn update_omit_header_is_a_registered_param_under_strict_params() {
    let (app, _dir) = indexed_app_with_config(Some("strict_params = true\n")).await;
    let req = Request::builder()
        .method("POST")
        .uri(format!(
            "/solr/{CORE}/update?commit=true&omitHeader=false&wt=json"
        ))
        .header("content-type", "application/json")
        .body(Body::from("[]"))
        .unwrap();
    let resp = app.clone().oneshot(req).await.expect("must not fail");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "omitHeader must be a registered UPDATE_PARAMS entry so strict_params does not 400 the \
         request shape search_api_solr actually sends"
    );
}

// --- scope limit: admin endpoints are deliberately untouched ---------------

/// None of the 28 captured traces ever sends `omitHeader` to
/// `/admin/luke` — the module never asks that endpoint to suppress its
/// header. This ticket therefore does not register `omitHeader` in
/// `ADMIN_LUKE_PARAMS`, and `/admin/luke`'s envelope stays unconditional:
/// sending `omitHeader=true` there has no effect (it is simply an
/// unrecognized param under the endpoint's own allowlist, silently ignored
/// under the default `strict_params = false`, same as any other param the
/// endpoint does not implement). If a future capture ever shows
/// `search_api_solr` sending `omitHeader` to an admin endpoint, this is the
/// test to change.
#[tokio::test]
async fn admin_luke_omit_header_is_not_a_registered_param_and_leaves_the_envelope_unconditional() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "admin/luke?omitHeader=true&wt=json").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert!(
        body.get("responseHeader").is_some(),
        "admin/luke must not suppress responseHeader -- omitHeader is out of this ticket's \
         scope for admin endpoints (no trace ever sends it there), got {body}"
    );
}

/// An unsupported `omitHeader` must not gain effect merely because strict mode
/// turns it into an error on an admin endpoint.
#[tokio::test]
async fn admin_luke_strict_unknown_omit_header_error_retains_header() {
    let (app, _dir) = indexed_app_with_config(Some("strict_params = true\n")).await;
    let (status, body) = get(&app, "admin/luke?omitHeader=true&wt=json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert!(body.get("responseHeader").is_some(), "got {body}");
}

/// Core validation runs before endpoint parameter validation. Even there, an
/// admin endpoint's unsupported omitHeader parameter must remain inert.
#[tokio::test]
async fn admin_luke_unknown_core_omit_header_error_retains_header() {
    let (app, _dir) = indexed_app().await;
    let req = Request::builder()
        .uri("/solr/nosuch/admin/luke?omitHeader=true&wt=json")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.expect("must not fail");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body must be readable");
    let body: Value = serde_json::from_slice(&bytes).expect("body must be valid JSON");

    assert_eq!(status, StatusCode::NOT_FOUND, "got {body}");
    assert!(body.get("responseHeader").is_some(), "got {body}");
}

// --- error envelopes and boolean vocabulary (issue #179) -------------------

/// Ground truth: `omit_header_error_true.json` is Solr 9.10.1's 400 response
/// to this request. `omitHeader` removes only the envelope header: the error
/// block and HTTP status remain intact.
#[tokio::test]
async fn select_error_omit_header_true_suppresses_header_and_retains_error() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "select?q=nosuchfield:x&omitHeader=true&wt=json").await;

    let expected = fixture("omit_header_error_true");
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert!(
        body.get("responseHeader").is_none(),
        "omitHeader=true must suppress responseHeader, got {body}"
    );
    assert_eq!(
        body.pointer("/error/code"),
        expected.pointer("/error/code"),
        "omitHeader=true must retain the fixture error code, got {body}"
    );
}

/// Ground truth: `omit_header_error_yes.json` captures the accepted `yes`
/// alias. Finding 109 further establishes case-insensitive `true` and `on`.
#[tokio::test]
async fn select_error_omit_header_accepts_yes_true_and_on_case_insensitively() {
    let (app, _dir) = indexed_app().await;
    let expected = fixture("omit_header_error_yes");

    for value in ["yes", "TRUE", "oN"] {
        let (status, body) = get(
            &app,
            &format!("select?q=nosuchfield:x&omitHeader={value}&wt=json"),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "omitHeader={value}: {body}"
        );
        assert!(
            body.get("responseHeader").is_none(),
            "omitHeader={value} must suppress responseHeader, got {body}"
        );
        assert_eq!(
            body.pointer("/error/code"),
            expected.pointer("/error/code"),
            "omitHeader={value} must retain the fixture error code, got {body}"
        );
    }
}

/// Finding 109's false vocabulary leaves the normal error envelope intact.
#[tokio::test]
async fn select_error_omit_header_false_no_and_off_retain_header() {
    let (app, _dir) = indexed_app().await;
    let expected_error_code = fixture("omit_header_error_true")["error"]["code"].clone();

    for value in ["false", "NO", "oFf"] {
        let (status, body) = get(
            &app,
            &format!("select?q=nosuchfield:x&omitHeader={value}&wt=json"),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "omitHeader={value}: {body}"
        );
        assert_eq!(
            body.pointer("/error/code"),
            Some(&expected_error_code),
            "omitHeader={value} must retain the fixture error code, got {body}"
        );
        assert!(
            body.get("responseHeader").is_some(),
            "omitHeader={value} must retain responseHeader, got {body}"
        );
    }
}

/// Ground truth: `omit_header_update_error_true.json` proves suppression also
/// applies to `/update`'s normally header-bearing, no-params error envelope.
#[tokio::test]
async fn update_error_omit_header_true_suppresses_header_and_retains_error() {
    let (app, _dir) = indexed_app().await;
    let req = Request::builder()
        .method("POST")
        .uri(format!("/solr/{CORE}/update?omitHeader=true&wt=json"))
        .header("content-type", "application/json")
        .body(Body::from("{not json"))
        .unwrap();
    let resp = app.oneshot(req).await.expect("must not fail");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body must be readable");
    let body: Value = serde_json::from_slice(&bytes).expect("body must be valid JSON");
    let expected = fixture("omit_header_update_error_true");

    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert!(
        body.get("responseHeader").is_none(),
        "omitHeader=true must suppress /update error responseHeader, got {body}"
    );
    assert_eq!(body.pointer("/error/code"), expected.pointer("/error/code"));
}

/// Validation follows core routing. Before `check_params` runs, an invalid
/// omitHeader value must not act like true and suppress an unknown-core error.
#[tokio::test]
async fn invalid_omit_header_is_inert_before_select_and_update_validation() {
    let (app, _dir) = indexed_app().await;

    let select = Request::builder()
        .uri("/solr/nosuch/select?q=*:*&omitHeader=1&wt=json")
        .body(Body::empty())
        .unwrap();
    let update = Request::builder()
        .method("POST")
        .uri("/solr/nosuch/update?omitHeader=1&wt=json")
        .header("content-type", "application/json")
        .body(Body::from("[]"))
        .unwrap();

    for req in [select, update] {
        let resp = app.clone().oneshot(req).await.expect("must not fail");
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body must be readable");
        let body: Value = serde_json::from_slice(&bytes).expect("body must be valid JSON");
        assert_eq!(status, StatusCode::NOT_FOUND, "got {body}");
        assert!(body.get("responseHeader").is_some(), "got {body}");
    }
}

/// Ground truth: `omit_header_invalid_one.html` is Solr 9.10.1's HTTP 400
/// for `omitHeader=1`; `t` is invalid too. PRD divergence 8 deliberately keeps
/// Wayfinder's error JSON rather than reproducing Jetty HTML.
#[tokio::test]
async fn select_invalid_omit_header_values_return_headerless_json_400() {
    let (app, _dir) = indexed_app().await;

    for value in ["1", "t"] {
        let req = Request::builder()
            .uri(format!(
                "/solr/{CORE}/select?q=*:*&omitHeader={value}&wt=json"
            ))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.expect("must not fail");
        let status = resp.status();
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body must be readable");
        let body: Value = serde_json::from_slice(&bytes).expect("body must be valid JSON");

        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "omitHeader={value}: {body}"
        );
        assert!(
            content_type.starts_with("application/json"),
            "Wayfinder's JSON-only divergence must return JSON, got {content_type}"
        );
        assert!(body.get("responseHeader").is_none(), "got {body}");
        assert_eq!(body.pointer("/error/code"), Some(&serde_json::json!(400)));
    }
}
