//! Select warnings for parameters that would otherwise silently change results.
//!
//! Issue #232 added an accept-and-warn treatment for `bf` and a function-form
//! `boost` because Wayfinder had no function-query evaluator. Issue #289 built
//! one, so those params are now **applied**, not warned: the two warnings came
//! out of `select`, and these assertions guard against a regression that
//! re-introduces them. The remaining select warning — a Points-based
//! `facet.field` raised to `mincount` 1 — still fires and is checked last.
//!
//! Exact score behaviour for function queries is fixture-backed in
//! `tests/differential.rs`'s `fnq` rows; this file covers only the
//! warning-envelope contract.

// The `dead_code` allow for partially-used shared helpers is an inner attribute
// inside `tests/common/mod.rs`; repeating it here is a clippy error under
// `-D warnings`.
mod common;

use axum::http::StatusCode;
use serde_json::{Value, json};

use common::{get, indexed_app};

#[tokio::test]
async fn numeric_boost_and_bq_do_not_warn() {
    let (app, _dir) = indexed_app().await;

    for (name, request) in [
        (
            "numeric boost",
            "select?q=lazy&defType=edismax&qf=body&boost=2&wt=json",
        ),
        (
            "bq",
            "select?q=lazy&defType=edismax&qf=body&bq=body:dog^2&wt=json",
        ),
    ] {
        let (status, body) = get(&app, request).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{name} request must succeed: {body}"
        );
        assert!(
            body.pointer("/responseHeader/warnings").is_none(),
            "{name} is implemented and must not carry a warnings entry: {body}"
        );
    }
}

#[tokio::test]
async fn function_form_bf_and_boost_are_applied_not_warned() {
    // The #232 warnings are gone (issue #289): `bf` and a function-form
    // `boost` are evaluated per document rather than ignored. `views` is a
    // declared fast `int` field (absent from the corpus, so it resolves to 0
    // -- the function still parses and applies, it just adds/multiplies by a
    // constant here). What matters is that no warning is emitted and the
    // request succeeds.
    let (app, _dir) = indexed_app().await;

    for (name, request) in [
        (
            "bf",
            "select?q=lazy&defType=edismax&qf=body&bf=sum(views,1)&wt=json",
        ),
        (
            "function-form boost",
            "select?q=lazy&defType=edismax&qf=body&boost=sum(views,2)&wt=json",
        ),
        (
            "{!func}",
            "select?q={!func}sum(views,1)&fl=id,score&wt=json",
        ),
    ] {
        let (status, body) = get(&app, request).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{name} request must succeed now that function queries are implemented: {body}"
        );
        assert!(
            body.pointer("/responseHeader/warnings").is_none(),
            "{name} must not carry the obsolete ignored-function warning: {body}"
        );
    }
}

#[tokio::test]
async fn facet_mincount_warning_still_fires_without_a_function_query_warning() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=views&wt=json",
    )
    .await;

    assert_eq!(status, StatusCode::OK, "request must succeed: {body}");
    assert_eq!(
        body.pointer("/responseHeader/warnings"),
        Some(&json!([
            "Raising facet.mincount from 0 to 1, because field views is Points-based."
        ])),
        "the facet warning is the only select warning now that bf/boost no longer warn: {body}"
    );
}

#[allow(dead_code)]
fn _unused_warning_shape() -> Value {
    // Kept so a future change to the warning string is a visible diff site,
    // mirroring the old `ignored_function_query_warning` helper's role.
    json!([])
}
