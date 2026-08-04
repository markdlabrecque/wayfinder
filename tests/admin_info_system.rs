//! `/wayfinder/admin/info/system` (server-level) and `/wayfinder/{core}/admin/system`
//! (core-scoped fallback) — issue #59, PRD open question 2.
//!
//! `search_api_solr`'s `SolrConnector::getSolrVersion()` (finding 78) used to read
//! `lucene.solr-spec-version` off `<core>/admin/system`, falling back to
//! `/admin/info/system`, and regex-captures the leading `major.minor.patch`;
//! that interop is withdrawn by this issue and the key is now
//! `lucene.wayfinder-spec-version`. `responseHeader`,
//! `mode`, `wayfinder_home`/`core_root`, `lucene-spec-version`, and `core.schema`
//! are also pinned to exact literal values below — `tests/differential.rs`'s
//! `EXPECTED_DIVERGENCES`/`EXPECTED_DIVERGENCES_MANIFEST_ERRORS` reason
//! strings claim those are "compared exactly and do match", so something
//! here has to make that true. Only `jvm`/`system`/`security` (volatile host
//! stats with no Wayfinder equivalent) are checked for *shape* alone, per
//! the task spec ("matched if cheap").
//!
//! Ground truth for the envelope shape: `solr-ref/search-api/trace/00023.json`
//! (server-level) and `00026.json` (core-scoped, plus the extra `core{}` key).
//! Both fixtures are copied verbatim, volatile fields included, at
//! `solr-ref/responses/admin_info_system.json` / `admin_system.json`.

mod common;

use axum::Router;
use axum::http::StatusCode;
use tempfile::TempDir;

use common::{CORE, get, request_full};

/// Builds an app against the tracer-bullet schema with an optional server
/// config TOML — mirrors `tests/server_config.rs::build_app_with_config`,
/// duplicated here rather than shared (that helper lives in a different
/// integration-test binary; `tests/common/` cannot be shared across them,
/// same precedent `tests/differential.rs`'s per-file schema duplication
/// documents).
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

/// `Admin`'s config-only test: `ServerConfig::parse` must accept `[admin]`
/// and default `reported_server_version` to `"9.0.0"` (PRD open question 2,
/// decision recorded in this issue's spec: lowest version in the 9.x branch
/// the capture's generated `schema.xml` targets).
#[test]
fn server_config_admin_defaults_to_9_0_0() {
    let config = wayfinder::ServerConfig::parse("").expect("empty config is valid");
    assert_eq!(config.admin.reported_server_version, "9.0.0");
}

#[test]
fn server_config_admin_section_is_overridable() {
    let config = wayfinder::ServerConfig::parse("[admin]\nreported_server_version = \"8.5.0\"\n")
        .expect("a valid admin section must parse");
    assert_eq!(config.admin.reported_server_version, "8.5.0");
}

/// Issue #325's entire operator-facing back-compat promise: the key was
/// renamed `reported_solr_version` -> `reported_server_version`, and
/// `Admin`'s `#[serde(alias = "reported_solr_version")]` is what keeps a
/// `wayfinder.toml` written before the rename loading unchanged. `Admin` is
/// `deny_unknown_fields`, so without the alias this is not a silent no-op but
/// a hard startup failure on the old key -- which is exactly why it needs a
/// test rather than trust. Deleting the attribute must turn this red.
#[test]
fn server_config_admin_accepts_the_legacy_reported_solr_version_key() {
    let config = wayfinder::ServerConfig::parse("[admin]\nreported_solr_version = \"8.5.0\"\n")
        .expect("the pre-#325 key must still parse via the serde alias");
    assert_eq!(
        config.admin.reported_server_version, "8.5.0",
        "the legacy key must populate the renamed field, not fall back to the default"
    );
}

#[test]
fn server_config_admin_rejects_unknown_key_by_name() {
    let err = wayfinder::ServerConfig::parse("[admin]\nreported_server_versionn = \"9.0.0\"\n")
        .expect_err("a typo'd admin key must not silently no-op");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("reported_server_versionn"),
        "error must name the offending key, got: {msg}"
    );
}

// --- server-level: /wayfinder/admin/info/system ----------------------------------

#[tokio::test]
async fn admin_info_system_default_version_is_9_0_0() {
    let (app, _dir) = build_app_with_config(None).expect("app must build");
    let (status, body) =
        request_full(&app, "GET", "admin/info/system?wt=json&json.nl=flat", None).await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["responseHeader"]["status"], 0);
    assert_eq!(
        body["lucene"]["wayfinder-spec-version"], "9.0.0",
        "default reported version must be 9.0.0 (PRD open question 2), got: {body}"
    );
}

#[tokio::test]
async fn admin_info_system_reports_configured_version_override() {
    let (app, _dir) = build_app_with_config(Some("[admin]\nreported_server_version = \"8.5.0\"\n"))
        .expect("app must build");
    let (status, body) =
        request_full(&app, "GET", "admin/info/system?wt=json&json.nl=flat", None).await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["lucene"]["wayfinder-spec-version"], "8.5.0",
        "reported version must be read from config, not hardcoded, got: {body}"
    );
}

/// Mutation-testing gate (CLAUDE.md: "code whose whole value is failing
/// correctly gets mutation-tested"; issue spec point 7): an implausibly high
/// configured version must flow through completely unclamped. A silent cap
/// would mask a misconfiguration that could unlock `search_api_solr`
/// version-gated features (`payload_score` at Solr major >= 6, etc.)
/// Wayfinder cannot actually serve — see this issue's report for the full
/// reasoning.
#[tokio::test]
async fn admin_info_system_reports_an_implausibly_high_version_unclamped() {
    let (app, _dir) =
        build_app_with_config(Some("[admin]\nreported_server_version = \"99.0.0\"\n"))
            .expect("app must build");
    let (status, body) =
        request_full(&app, "GET", "admin/info/system?wt=json&json.nl=flat", None).await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["lucene"]["wayfinder-spec-version"], "99.0.0",
        "an implausible version must pass through exactly as configured, no silent clamp, \
         got: {body}"
    );
}

#[tokio::test]
async fn admin_info_system_top_level_key_shape_matches_the_captured_envelope() {
    let (app, _dir) = build_app_with_config(None).expect("app must build");
    let (status, body) =
        request_full(&app, "GET", "admin/info/system?wt=json&json.nl=flat", None).await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    for key in [
        "responseHeader",
        "mode",
        "wayfinder_home",
        "core_root",
        "lucene",
        "jvm",
        "security",
        "system",
    ] {
        assert!(
            body.get(key).is_some(),
            "server-level admin/info/system envelope must carry top-level key `{key}` \
             (solr-ref/search-api/trace/00023.json), got: {body}"
        );
    }
    for key in [
        "wayfinder-spec-version",
        "wayfinder-impl-version",
        "lucene-spec-version",
        "lucene-impl-version",
    ] {
        assert!(
            body["lucene"].get(key).is_some(),
            "lucene{{}} must carry `{key}`, got: {body}"
        );
    }
    assert!(
        body["security"].is_object(),
        "security must be an object, got: {body}"
    );
}

/// Pins the fields the `EXPECTED_DIVERGENCES_MANIFEST_ERRORS` reason string
/// in `tests/differential.rs` claims are "compared exactly and do match":
/// `responseHeader`, `mode`, `wayfinder_home`, `core_root`, and
/// `lucene-spec-version` (the one hardcoded lucene value, as opposed to
/// `wayfinder-spec-version` which is the deliberately-configured one under test
/// above). Round-2 review flagged that nothing previously pinned these
/// literal values — a regression here would stay green everywhere else.
#[tokio::test]
async fn admin_info_system_pins_the_fields_the_differential_reason_string_claims_match() {
    let (app, _dir) = build_app_with_config(None).expect("app must build");
    let (status, body) =
        request_full(&app, "GET", "admin/info/system?wt=json&json.nl=flat", None).await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["responseHeader"]["status"], 0);
    assert_eq!(body["responseHeader"]["QTime"], 0);
    assert_eq!(body["mode"], "std");
    assert_eq!(body["wayfinder_home"], "/var/wayfinder/data");
    assert_eq!(body["core_root"], "/var/wayfinder/data");
    assert_eq!(body["lucene"]["lucene-spec-version"], "9.12.3");
}

#[tokio::test]
async fn admin_info_system_strict_params_accepts_wt_and_json_nl() {
    let (app, _dir) =
        build_app_with_config(Some("strict_params = true\n")).expect("app must build");
    let (status, body) =
        request_full(&app, "GET", "admin/info/system?wt=json&json.nl=flat", None).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "strict_params=true must not 400 on wt/json.nl, got: {body}"
    );
}

/// The `check_params` gate must actually run for this route: a 5-minute
/// mutation (deleting the `check_params` call in `admin_info_system`) would
/// leave every *other* test in this file green, since they only exercise
/// `wt`/`json.nl`, both allowed. This is the negative case.
#[tokio::test]
async fn admin_info_system_strict_params_rejects_unknown_param() {
    let (app, _dir) =
        build_app_with_config(Some("strict_params = true\n")).expect("app must build");
    let (status, body) = request_full(
        &app,
        "GET",
        "admin/info/system?wt=json&json.nl=flat&bogus=1",
        None,
    )
    .await;

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "strict_params=true must 400 on an unrecognized param, got: {body}"
    );
    let msg = body
        .pointer("/error/msg")
        .and_then(|m| m.as_str())
        .expect("error.msg must be present");
    assert!(
        msg.contains("bogus"),
        "error.msg must name the unknown param, got: {msg}"
    );
}

/// Method-agnostic, matching `ping`'s `any(...)` precedent (Solr's own
/// handlers are method-agnostic; issue spec point 1).
#[tokio::test]
async fn admin_info_system_is_method_agnostic() {
    let (app, _dir) = build_app_with_config(None).expect("app must build");
    let (status, _body) =
        request_full(&app, "POST", "admin/info/system?wt=json&json.nl=flat", None).await;
    assert_eq!(status, StatusCode::OK);
}

// --- core-scoped fallback: /wayfinder/{core}/admin/system ------------------------

#[tokio::test]
async fn core_admin_system_default_version_is_9_0_0() {
    let (app, _dir) = build_app_with_config(None).expect("app must build");
    let (status, body) = get(&app, "admin/system?wt=json&json.nl=flat").await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["responseHeader"]["status"], 0);
    assert_eq!(body["lucene"]["wayfinder-spec-version"], "9.0.0");
}

#[tokio::test]
async fn core_admin_system_reports_configured_version_override() {
    let (app, _dir) = build_app_with_config(Some("[admin]\nreported_server_version = \"8.5.0\"\n"))
        .expect("app must build");
    let (status, body) = get(&app, "admin/system?wt=json&json.nl=flat").await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["lucene"]["wayfinder-spec-version"], "8.5.0",
        "reported version must be read from config, not hardcoded, got: {body}"
    );
}

#[tokio::test]
async fn core_admin_system_top_level_key_shape_matches_the_captured_envelope() {
    let (app, _dir) = build_app_with_config(None).expect("app must build");
    let (status, body) = get(&app, "admin/system?wt=json&json.nl=flat").await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    for key in [
        "responseHeader",
        "core",
        "mode",
        "lucene",
        "jvm",
        "security",
        "system",
    ] {
        assert!(
            body.get(key).is_some(),
            "core-scoped admin/system envelope must carry top-level key `{key}` \
             (solr-ref/search-api/trace/00026.json), got: {body}"
        );
    }
    for key in ["schema", "host", "now", "start", "directory"] {
        assert!(
            body["core"].get(key).is_some(),
            "core{{}} must carry `{key}`, got: {body}"
        );
    }
    for key in ["cwd", "instance", "data", "dirimpl", "index"] {
        assert!(
            body["core"]["directory"].get(key).is_some(),
            "core.directory{{}} must carry `{key}`, got: {body}"
        );
    }
}

#[tokio::test]
async fn core_admin_system_strict_params_accepts_wt_and_json_nl() {
    let (app, _dir) =
        build_app_with_config(Some("strict_params = true\n")).expect("app must build");
    let (status, body) = get(&app, "admin/system?wt=json&json.nl=flat").await;

    assert_eq!(
        status,
        StatusCode::OK,
        "strict_params=true must not 400 on wt/json.nl, got: {body}"
    );
}

/// Same 5-minute mutation gap as `admin_info_system`'s check_params test
/// above, but for the core-scoped route's own `check_params` call.
#[tokio::test]
async fn core_admin_system_strict_params_rejects_unknown_param() {
    let (app, _dir) =
        build_app_with_config(Some("strict_params = true\n")).expect("app must build");
    let (status, body) = get(&app, "admin/system?wt=json&json.nl=flat&bogus=1").await;

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "strict_params=true must 400 on an unrecognized param, got: {body}"
    );
    let msg = body
        .pointer("/error/msg")
        .and_then(|m| m.as_str())
        .expect("error.msg must be present");
    assert!(
        msg.contains("bogus"),
        "error.msg must name the unknown param, got: {msg}"
    );
}

/// `core.schema`'s shape matters for real: `search_api_solr`'s
/// `SolrConnectorPluginBase.php` `explode('-', $schema)`s this value and
/// indexes into `$parts[1]` (module version), `$parts[3]` (targeted Solr
/// branch), and `$parts[4]` — all three must be present and non-empty, or
/// those calls hit an undefined array index (finding 78,
/// docs/solr-ref-findings.md; ground truth:
/// `solr-ref/responses/admin_system.json`'s `core.schema`).
#[tokio::test]
async fn core_admin_system_schema_has_the_dash_part_shape_search_api_solr_indexes_into() {
    let (app, _dir) = build_app_with_config(None).expect("app must build");
    let (status, body) = get(&app, "admin/system?wt=json&json.nl=flat").await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    let schema = body["core"]["schema"]
        .as_str()
        .expect("core.schema must be a string");

    // Round-2 review: the shape checks below (>=5 parts, parts non-empty)
    // pass for *any* correctly-shaped string, e.g. "a-b-c-d-e" — which would
    // keep this test green while breaking `getSchemaTargetedSolrBranch()` in
    // the real client. Pin the exact literal too, verbatim from
    // `solr-ref/responses/admin_system.json`'s `core.schema`.
    assert_eq!(
        schema, "drupal-4.4.0-wayfinder-9.x-0",
        "core.schema must match the captured fixture exactly, got `{schema}`"
    );

    let parts: Vec<&str> = schema.split('-').collect();
    assert!(
        parts.len() >= 5,
        "core.schema must have at least 5 dash-separated parts \
         (search_api_solr indexes up to parts[4]), got `{schema}`"
    );
    assert!(
        !parts[1].is_empty(),
        "parts[1] (module version) must be non-empty, got `{schema}`"
    );
    assert!(
        !parts[3].is_empty(),
        "parts[3] (targeted Solr branch) must be non-empty, got `{schema}`"
    );
    assert!(
        !parts[4].is_empty(),
        "parts[4] must be non-empty, got `{schema}`"
    );
}

/// `check_core` gate, like `ping`'s (finding 49's divergence family: Wayfinder
/// answers its own JSON 404 rather than Solr's HTML easter egg, but the
/// status code — and this route's presence in that gate at all — is what's
/// under test here, not the body shape).
#[tokio::test]
async fn core_admin_system_unknown_core_is_not_found() {
    let (app, _dir) = build_app_with_config(None).expect("app must build");
    let (status, body) = request_full(
        &app,
        "GET",
        "nosuchcore/admin/system?wt=json&json.nl=flat",
        None,
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
    assert_eq!(body["error"]["code"], 404);
}

/// Sanity check that the core-scoped route really is scoped under the one
/// core this app serves, i.e. it is reachable at `{CORE}/admin/system` and
/// not, say, silently server-level too.
#[tokio::test]
async fn core_admin_system_is_reachable_under_the_configured_core_name() {
    let (app, _dir) = build_app_with_config(None).expect("app must build");
    let (status, _body) = request_full(
        &app,
        "GET",
        &format!("{CORE}/admin/system?wt=json&json.nl=flat"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}
