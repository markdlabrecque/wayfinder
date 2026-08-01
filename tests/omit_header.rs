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
//! 2. **Open question 1 (error responses) is NOT settled by the fixtures.**
//!    Every one of the 28 captured `search_api_solr` traces is a 200; none
//!    combines `omitHeader=true` with an error. `solr-ref/manifest.tsv` and
//!    `solr-ref/manifest-errors.tsv` likewise have no row using `omitHeader`
//!    at all. This file therefore does not assert anything about
//!    `omitHeader` on error responses in either direction — see the
//!    module-level note in `src/error.rs` (left for the implementor to add)
//!    for the scope limit this implies: `WfError`'s envelope construction is
//!    out of scope for this ticket, and success-path suppression must not
//!    be threaded into it without new fixture evidence.
//! 3. **Open question 2: no `EXPECTED_DIVERGENCES`/`ACCEPTED_DIVERGENCES`
//!    entry exists for this anywhere in `tests/differential.rs`.** Grepping
//!    `solr-ref/manifest.tsv` and `solr-ref/manifest-errors.tsv` for
//!    `omitHeader` returns nothing — the differential harness is green on
//!    this today because it never exercises `omitHeader` at all, not
//!    because a normaliser masks the divergence. That said, this *is* a real
//!    coverage gap in the compatibility evidence: `src/coverage.rs`'s
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
