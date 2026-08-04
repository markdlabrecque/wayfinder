//! `/suggest?suggest.buildAll=true` — the search_api_solr cron path (issue #352).
//!
//! `search_api_solr`'s `SearchApiSolrHooks::cron` fires
//! `GET /<core>/suggest?suggest.buildAll=true` via `fireAndForget`
//! (`nowaitforresponserequest`) whenever the server is Drupal-only-writeable,
//! the index saw updates since the last build, and the last build was more
//! than 1800s ago. Verified against the vendored 4.4.0 source at
//! `coverage/search_api_solr_4.4.0_source/src/Hook/SearchApiSolrHooks.php:143-164`
//! (gate) and `:159-161` (`getSuggesterQuery` + `addParam('suggest.buildAll',
//! TRUE)` + `fireAndForget`).
//!
//! ## Ground truth
//!
//! `solr-ref/responses/suggest_build_all.json`, captured against a real
//! `solr:9` with the canonical Drupal configset (which carries the `/suggest`
//! requestHandler and its `suggest` SuggestComponent in
//! `solr-ref/search-api/configset/solrconfig_extra.xml`). Solr's
//! SuggestComponent short-circuits a build command and emits
//! `{"responseHeader":{status,QTime},"command":"buildAll"}` — no `suggest`
//! block (that appears only for a `suggest.q` lookup) and crucially **no
//! `params` under `responseHeader`**: the component does not echo them, unlike
//! `/select`. Tantivy's term dictionary is already an FST, so Wayfinder has no
//! separate dictionary to build — `buildAll` is accepted and inert, returning
//! this envelope unchanged.

mod common;

use axum::Router;
use axum::http::StatusCode;
use common::{CORE, get, request_full};
use serde_json::Value;
use tempfile::TempDir;

/// Builds an app against the tracer-bullet schema with an optional server
/// config TOML — mirrors `tests/admin_info_system.rs::build_app_with_config`.
fn build_app_with_config(config: Option<&str>) -> anyhow::Result<(Router, TempDir)> {
    let dir = TempDir::new().expect("create temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, common::SCHEMA_TOML).expect("write schema.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let app = match config {
        Some(toml) => {
            let config_path = dir.path().join("wayfinder.toml");
            std::fs::write(&config_path, toml).expect("write wayfinder.toml");
            wayfinder::app_with_config(&schema_path, &data_dir, &config_path)?
        }
        None => wayfinder::app(&schema_path, &data_dir)?,
    };
    Ok((app, dir))
}

/// The cron path: `suggest.buildAll=true` returns the captured build envelope
/// verbatim (modulo the volatile `QTime`), with `command:"buildAll"` and NO
/// `suggest` block.
#[tokio::test]
async fn suggest_build_all_returns_captured_envelope() {
    let (app, _dir) = build_app_with_config(None).expect("app must build");
    let (status, body) = get(&app, "suggest?suggest.buildAll=true&wt=json").await;

    assert_eq!(
        status,
        StatusCode::OK,
        "buildAll must answer 200, got {status}: {body}"
    );
    assert_eq!(
        body["command"], "buildAll",
        "buildAll must echo command:\"buildAll\": {body}"
    );
    assert_eq!(
        body["responseHeader"]["status"], 0,
        "responseHeader.status must be 0: {body}"
    );
    // The decisive wire detail: Solr's SuggestComponent does NOT echo request
    // params under responseHeader for /suggest (unlike /select). Asserting the
    // absence here is what stops a future edit from adding `params.echo()` and
    // silently diverging from the fixture.
    assert!(
        body["responseHeader"].get("params").is_none(),
        "responseHeader must NOT carry params (Solr's /suggest never echoes them): {body}"
    );
    assert!(
        body.get("suggest").is_none(),
        "a build command carries no suggest block (that is a suggest.q lookup): {body}"
    );
}

/// The build envelope matches the committed fixture byte-for-byte outside the
/// volatile `QTime` — the wire-contract claim, asserted here in addition to the
/// differential harness's manifest-errors row.
#[tokio::test]
async fn suggest_build_all_matches_committed_fixture() {
    let (app, _dir) = build_app_with_config(None).expect("app must build");
    let (_status, body) = get(&app, "suggest?suggest.buildAll=true&wt=json").await;
    let expected = common::fixture("suggest_build_all");
    // QTime is the only volatile leaf; equalise it, then the rest must match.
    let mut actual = body.clone();
    if let Some(qt) = actual.pointer_mut("/responseHeader/QTime") {
        *qt = Value::Null;
    }
    let mut expected = expected;
    if let Some(qt) = expected.pointer_mut("/responseHeader/QTime") {
        *qt = Value::Null;
    }
    assert_eq!(actual, expected, "build envelope must match the fixture");
}

/// `suggest.build` (a single dictionary) and `suggest.reload` echo their own
/// `command` field — the same short-circuit, faithfully inert.
#[tokio::test]
async fn suggest_build_and_reload_echo_their_commands() {
    let (app, _dir) = build_app_with_config(None).expect("app must build");
    let (_, body) = get(&app, "suggest?suggest.build=true&wt=json").await;
    assert_eq!(
        body["command"], "build",
        "suggest.build -> command:\"build\""
    );
    let (_, body) = get(&app, "suggest?suggest.reload=true&wt=json").await;
    assert_eq!(
        body["command"], "reload",
        "suggest.reload -> command:\"reload\""
    );
}

/// `buildAll` wins when both `buildAll` and `build` are present (Solr processes
/// the build-all path first).
#[tokio::test]
async fn suggest_build_all_takes_precedence_over_build() {
    let (app, _dir) = build_app_with_config(None).expect("app must build");
    let (_, body) = get(
        &app,
        "suggest?suggest.buildAll=true&suggest.build=true&wt=json",
    )
    .await;
    assert_eq!(
        body["command"], "buildAll",
        "buildAll must win over build when both are sent: {body}"
    );
}

/// A bare `/suggest` (no build/reload command) returns just `responseHeader` —
/// no `command` key at all, matching Solr.
#[tokio::test]
async fn suggest_bare_returns_header_only() {
    let (app, _dir) = build_app_with_config(None).expect("app must build");
    let (status, body) = get(&app, "suggest?wt=json").await;
    assert_eq!(status, StatusCode::OK, "bare /suggest is a 200: {body}");
    assert!(
        body.get("command").is_none(),
        "bare /suggest carries no command: {body}"
    );
    assert!(
        body["responseHeader"]["status"] == 0,
        "bare /suggest still has a status-0 header: {body}"
    );
}

/// `omitHeader=true` drops `responseHeader` entirely, leaving only `command`.
#[tokio::test]
async fn suggest_omit_header_drops_response_header() {
    let (app, _dir) = build_app_with_config(None).expect("app must build");
    let (_, body) = get(
        &app,
        "suggest?suggest.buildAll=true&omitHeader=true&wt=json",
    )
    .await;
    assert_eq!(body["command"], "buildAll");
    assert!(
        body.get("responseHeader").is_none(),
        "omitHeader=true must drop responseHeader: {body}"
    );
}

/// `strict_params = true` must NOT 400 on any param the shipped `/suggest`
/// handler config makes routine: the component gate `suggest`, the defaults
/// `suggest.dictionary`/`suggest.count`, and the build commands. (The cron
/// request itself sends only `suggest.buildAll`; the rest are admitted for
/// parity so a handler-default param never 400s.)
#[tokio::test]
async fn suggest_strict_params_accepts_handler_routine_params() {
    let (app, _dir) =
        build_app_with_config(Some("strict_params = true\n")).expect("app must build");
    let (status, body) = request_full(
        &app,
        "GET",
        &format!(
            "{CORE}/suggest?suggest=true&suggest.buildAll=true&suggest.build=true&\
             suggest.reload=true&suggest.dictionary=und&suggest.count=10&wt=json"
        ),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "strict_params must accept the handler's routine params, got {status}: {body}"
    );
}

/// The negative case: under `strict_params`, an unrecognised `suggest.*` param
/// still 400s — admitting the routine params is not a blanket `suggest.*` pass.
/// Mutation-tested: deleting the param from `SUGGEST_PARAMS` must turn this red.
#[tokio::test]
async fn suggest_strict_params_rejects_unknown_suggest_param() {
    let (app, _dir) =
        build_app_with_config(Some("strict_params = true\n")).expect("app must build");
    let (status, body) = request_full(
        &app,
        "GET",
        &format!("{CORE}/suggest?suggest.buildAll=true&suggest.bogus=1&wt=json"),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "strict_params must 400 on an unknown suggest.* param, got {status}: {body}"
    );
    let msg = body
        .pointer("/error/msg")
        .and_then(|m| m.as_str())
        .unwrap_or("");
    assert!(
        msg.contains("suggest.bogus"),
        "the 400 must name the offending param: {body}"
    );
}

/// The `fireAndForget` cron caller closes the connection without reading the
/// response. The acceptance bar from the issue is: does not error, does not
/// hang, does not leak a task or connection per cron run. A synchronous
/// immediate return clears all three by construction — there is no background
/// work to outlive the request — and this asserts that property directly: the
/// handler answers promptly and stays inert across repeated cron runs (no
/// per-call state accumulates).
#[tokio::test]
async fn suggest_build_all_is_inert_and_does_not_leak_across_runs() {
    let (app, _dir) = build_app_with_config(None).expect("app must build");
    for _ in 0..5 {
        let (status, body) = get(&app, "suggest?suggest.buildAll=true&wt=json").await;
        assert_eq!(status, StatusCode::OK, "every cron run must answer 200");
        assert_eq!(body["command"], "buildAll");
        // No suggest block ever appears: the build is inert, so nothing about
        // the response grows or changes across repeated calls.
        assert!(body.get("suggest").is_none(), "no suggest block: {body}");
    }
}
