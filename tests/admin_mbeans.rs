//! `GET /solr/{core}/admin/mbeans?stats=true` (issue #158), reversing the
//! #57 descope for this endpoint.
//!
//! Ground truth is `solr-ref/search-api/trace/00025.json` (issue #55): 48 KB
//! of real `solr:9` response, almost all Java class names / per-handler
//! timers / JVM metrics with no consumer. `SolrConnectorPluginBase::
//! getStatsSummary()` (coverage/search_api_solr_4.4.0_source/src/
//! SolrConnector/SolrConnectorPluginBase.php, ~L775-820) reads exactly six
//! leaves off it, on the Solr >= 7.0 branch (the branch that applies here,
//! since `config.admin.reported_solr_version` defaults to "9.0.0"):
//!
//!   - `solr-mbeans.UPDATE.updateHandler.stats["UPDATE.updateHandler.docsPending"]`
//!   - `...["UPDATE.updateHandler.softAutoCommitMaxTime"]`
//!   - `...["UPDATE.updateHandler.deletesById"]`
//!   - `...["UPDATE.updateHandler.deletesByQuery"]`
//!   - `solr-mbeans.CORE.core.stats["CORE.coreName"]`
//!   - `solr-mbeans.CORE.core.stats["INDEX.size"]`
//!
//! No `solr-ref/manifest.tsv` row exists for this endpoint (deliberate, per
//! the issue: "48 KB of Java internals cannot be matched honestly") so the
//! differential harness does not enforce exact wire-format fidelity here.
//! Everything BUT the six leaves above ("bean list" shape, `CONTAINER`,
//! `ADMIN`, `QUERY`, `CACHE`, per-handler timers, Java class names) is a
//! static, commented placeholder per the `admin_info_jvm_system_security()`
//! precedent in `src/lib.rs` -- this file does not pin any of that fictional
//! content, only the skeleton needed for the six real leaves to resolve.
//!
//! **Malformed-request premise, verified from the trace (not assumed):**
//! `solr-ref/search-api/trace/00025.json`'s `request.path` is verbatim
//! `/solr/search_api_capture/admin/mbeans?stats=true?omitHeader=false&json.nl=map&json.nl=flat&wt=json`
//! -- the module concatenates a handler string (`admin/mbeans?stats=true`)
//! that already contains a query onto Solarium's own `json.nl=map` param,
//! and mitmproxy's `capture_addon.py` records `req.path` raw off the wire
//! (no re-encoding), so this is exactly what Solr received. The captured
//! RESPONSE settles both open questions:
//!
//!   - `solr-mbeans` is a JSON *object* (map shape), not the alternating
//!     flat-array shape `json.nl=flat` would produce -- so the FIRST
//!     `json.nl` (`map`) won over the second (`flat`), not the last.
//!   - `UPDATE.updateHandler.stats` is present with real, nonzero-history
//!     values (`softAutoCommits: 2`, `cumulativeAdds.count: 12`, etc.), so
//!     `stats` WAS honoured even though its value was the literal string
//!     `"true?omitHeader=false"`, not `"true"`.
//!
//! Wayfinder's own `Params::get`/`get_all` (src/params.rs) already return
//! the FIRST match for a repeated key and parse `&`-delimited segments with
//! `?` as an ordinary value character -- both match the trace's observed
//! behaviour already, with no new parsing logic needed. `mbeans_malformed_*`
//! below pins exactly this request against Wayfinder.
//!
//! `INDEX.size`'s spelling was checked against `src/admin_ui.rs::human_size`:
//! the trace's `21607` bytes / 1024 = 21.1... -> `"21.1 KB"`, which is
//! exactly what `human_size` already produces (1 decimal, binary steps,
//! `"{value:.1} {unit}"`) -- no unit-spelling mismatch to fix.
//!
//! **Ambiguity raised, then resolved against the fixture (not guessed):**
//! an earlier draft of this suite asserted a bare JSON integer for
//! `softAutoCommitMaxTime`, reasoning that the PHP consumer only ever does
//! `(int) $value` on it and no manifest/differential row enforces exact
//! wire fidelity for this endpoint. Both points are true but neither
//! licenses the divergence -- the compatibility contract says expected
//! values come from the fixtures, never from what is convenient to
//! produce, and "nothing external would catch it" is an argument for
//! pinning the value here, not for skipping it. Checked directly against
//! `solr-ref/search-api/trace/00025.json`:
//! `.response.body.solr-mbeans.UPDATE.updateHandler.stats["UPDATE.updateHandler.softAutoCommitMaxTime"]`
//! is the STRING `"5000ms"` (with `"UPDATE.updateHandler.autoCommitMaxTime"`
//! alongside it as `"15000ms"`, same shape, not asserted by this suite).
//! `mbeans_soft_auto_commit_max_time_reflects_configured_autocommit_max_time`
//! below now asserts that string form, built from the configured
//! millisecond value, so a hardcoded `"5000ms"` in the handler cannot pass.
//!
//! The paired "-1 when unset" assertion was also wrong, for a different
//! reason: `-1` is `SolrConnectorPluginBase::getStatsSummary()`'s own
//! default for a *missing* key, not a value Solr ever puts on the wire.
//! The relevant lines (coverage/search_api_solr_4.4.0_source/src/
//! SolrConnector/SolrConnectorPluginBase.php:787-793) are:
//!
//! ```php
//! $max_time = -1;
//! if (isset($update_handler_stats['UPDATE.updateHandler.softAutoCommitMaxTime'])) {
//!   $max_time = (int) $update_handler_stats['UPDATE.updateHandler.softAutoCommitMaxTime'];
//! }
//! ```
//!
//! The `isset` guard exists because Solr omits the key entirely when soft
//! autocommit is disabled -- so the faithful behaviour when
//! `config.commit.autocommit_max_time` is unset is the KEY BEING ABSENT
//! from `UPDATE.updateHandler.stats`, not present with a sentinel `-1`.
//! `mbeans_soft_auto_commit_max_time_is_absent_when_unset` below now
//! asserts absence of the key itself.
//!
//! Delete-counter increment granularity is also a judgment call, stated
//! here rather than left implicit: Solr's JSON update loader turns a
//! multi-id `{"delete": ["a","b"]}` body into one `DeleteUpdateCommand` per
//! id, so the real `deletesById` counter increments once per id, not once
//! per HTTP call. `mbeans_deletes_by_id_and_deletes_by_query_increment_independently`
//! below is written to that per-id interpretation (it sends a 1-id call then
//! a 2-id call and expects a cumulative total of 3, which a per-call-only
//! counter would fail at 2).

mod common;

use std::path::Path;

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;

use common::request_full;

/// A minimal, single-field-plus-body schema, parametrized on core name so
/// tests needing two distinctly-named cores (the `INDEX.size`/`CORE.coreName`
/// hardcoding guard) don't collide. Deliberately NOT `common::CORE`
/// (`"content"`) or `update9` (`tests/update_pipeline.rs`) -- this issue's
/// acceptance criteria explicitly want a non-default core name so a
/// hardcoded `CORE.coreName` fails.
fn mbeans_schema_toml(core_name: &str) -> String {
    format!(
        r#"
[core]
name = "{core_name}"
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
"#
    )
}

/// Builds an app against `mbeans_schema_toml(core_name)`, with an optional
/// server config TOML, seeding nothing.
fn build_mbeans_app(
    dir: &Path,
    core_name: &str,
    config_toml: Option<&str>,
) -> anyhow::Result<Router> {
    let schema_path = dir.join("schema.toml");
    std::fs::write(&schema_path, mbeans_schema_toml(core_name)).expect("write schema.toml");
    let data_dir = dir.join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    match config_toml {
        Some(toml) => {
            let config_path = dir.join("wayfinder.toml");
            std::fs::write(&config_path, toml).expect("write wayfinder.toml");
            wayfinder::app_with_config(&schema_path, &data_dir, &config_path)
        }
        None => wayfinder::app(&schema_path, &data_dir),
    }
}

async fn post_m(app: &Router, core: &str, query: &str, body: &str) -> (StatusCode, Value) {
    request_full(app, "POST", &format!("{core}/{query}"), Some(body)).await
}

async fn get_m(app: &Router, core: &str, query: &str) -> (StatusCode, Value) {
    request_full(app, "GET", &format!("{core}/{query}"), None).await
}

/// Config enabling soft autocommit, which is the state the captured Solr was in
/// (trace `00025.json` reports `softAutoCommitMaxTime: "5000ms"`). Tests that
/// assert all SIX leaves resolve must use it: Solr omits
/// `softAutoCommitMaxTime` entirely when soft autocommit is off, so a core with
/// it unset legitimately exposes only five.
const SOFT_AUTOCOMMIT_ON: &str = "[commit]\nautocommit_max_time = 5000\n";

/// The six exact key paths the module reads, per
/// `SolrConnectorPluginBase::getStatsSummary()`'s Solr >= 7.0 branch.
fn assert_six_leaves_present(body: &Value) {
    for pointer in [
        "/solr-mbeans/UPDATE/updateHandler/stats/UPDATE.updateHandler.docsPending",
        "/solr-mbeans/UPDATE/updateHandler/stats/UPDATE.updateHandler.softAutoCommitMaxTime",
        "/solr-mbeans/UPDATE/updateHandler/stats/UPDATE.updateHandler.deletesById",
        "/solr-mbeans/UPDATE/updateHandler/stats/UPDATE.updateHandler.deletesByQuery",
        "/solr-mbeans/CORE/core/stats/CORE.coreName",
        "/solr-mbeans/CORE/core/stats/INDEX.size",
    ] {
        assert!(
            body.pointer(pointer).is_some(),
            "exact key path `{pointer}` must resolve (SolrConnectorPluginBase::getStatsSummary, \
             Solr >= 7.0 branch), got: {body}"
        );
    }
}

// --- six leaves, exact key strings ------------------------------------------

#[tokio::test]
async fn mbeans_six_leaves_resolve_by_exact_key_strings() {
    let dir = TempDir::new().expect("temp dir");
    let core = "mbeans_leaves";
    let app = build_mbeans_app(dir.path(), core, Some(SOFT_AUTOCOMMIT_ON)).expect("app must build");
    let (status, body) = post_m(
        &app,
        core,
        "update?commit=true",
        &json!([{"id": "d1", "body": "hello"}]).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seed doc: {body}");

    let (status, body) = get_m(&app, core, "admin/mbeans?stats=true&json.nl=map&wt=json").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_six_leaves_present(&body);
}

/// A plausible-but-differently-spelled key (leaf missing its
/// `UPDATE.updateHandler.` value-key prefix, or nested one level shallower
/// without the `stats` wrapper) must NOT resolve -- guards against an
/// implementation that puts the right VALUE at the wrong PATH, which the
/// presence-only assertion above cannot catch by itself.
#[tokio::test]
async fn mbeans_leaves_do_not_resolve_at_plausible_but_wrong_paths() {
    let dir = TempDir::new().expect("temp dir");
    let core = "mbeans_wrong_paths";
    let app = build_mbeans_app(dir.path(), core, Some(SOFT_AUTOCOMMIT_ON)).expect("app must build");
    let (status, body) = get_m(&app, core, "admin/mbeans?stats=true&json.nl=map&wt=json").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_six_leaves_present(&body);

    assert!(
        body.pointer("/solr-mbeans/UPDATE/updateHandler/softAutoCommitMaxTime")
            .is_none(),
        "the leaf must live under the `stats` object, not directly on `updateHandler`, got: {body}"
    );
    assert!(
        body.pointer("/solr-mbeans/CORE/core/stats/coreName")
            .is_none(),
        "the key must be the full `CORE.coreName` string, not bare `coreName`, got: {body}"
    );
    assert!(
        body.pointer("/solr-mbeans/CORE/core/stats/size").is_none(),
        "the key must be the full `INDEX.size` string, not bare `size`, got: {body}"
    );
}

// --- stats=true gate ---------------------------------------------------------

#[tokio::test]
async fn mbeans_without_stats_param_is_bean_list_only() {
    let dir = TempDir::new().expect("temp dir");
    let core = "mbeans_no_stats";
    let app = build_mbeans_app(dir.path(), core, None).expect("app must build");
    let (status, body) = get_m(&app, core, "admin/mbeans?wt=json&json.nl=map").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    assert!(
        body.pointer("/solr-mbeans/UPDATE").is_some(),
        "the bean list itself must still be present without stats=true, got: {body}"
    );
    assert!(
        body.pointer("/solr-mbeans/UPDATE/updateHandler/stats")
            .is_none(),
        "without stats=true, no `stats` sub-object may appear, got: {body}"
    );
    assert!(
        body.pointer("/solr-mbeans/CORE/core/stats").is_none(),
        "without stats=true, no `stats` sub-object may appear on CORE either, got: {body}"
    );
}

#[tokio::test]
async fn mbeans_stats_false_is_also_bean_list_only() {
    let dir = TempDir::new().expect("temp dir");
    let core = "mbeans_stats_false";
    let app = build_mbeans_app(dir.path(), core, None).expect("app must build");
    let (status, body) = get_m(&app, core, "admin/mbeans?stats=false&wt=json&json.nl=map").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        body.pointer("/solr-mbeans/UPDATE/updateHandler/stats")
            .is_none(),
        "stats=false must not include stats blocks, got: {body}"
    );
}

// --- the malformed captured request (trace 00025.json) ----------------------

/// Reproduces `solr-ref/search-api/trace/00025.json`'s exact captured
/// request path verbatim -- the second `?`, and `json.nl` twice with
/// conflicting values -- and pins the two things the real response settled:
/// `stats` was honoured despite its value being
/// `"true?omitHeader=false"`, and the FIRST `json.nl` (`map`) won, not the
/// second (`flat`): `solr-mbeans` must be a JSON object, not the alternating
/// array `json.nl=flat` would produce.
#[tokio::test]
async fn mbeans_malformed_captured_request_honours_stats_and_first_json_nl() {
    let dir = TempDir::new().expect("temp dir");
    let core = "mbeans_malformed";
    let app = build_mbeans_app(dir.path(), core, None).expect("app must build");
    let (status, body) = get_m(
        &app,
        core,
        "admin/mbeans?stats=true?omitHeader=false&json.nl=map&json.nl=flat&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    assert!(
        body.get("solr-mbeans").is_some_and(|v| v.is_object()),
        "`solr-mbeans` must be a JSON object -- the FIRST `json.nl` (map) must win over the \
         second (flat), matching the captured response's shape, got: {body}"
    );
    assert!(
        body.pointer("/solr-mbeans/UPDATE/updateHandler/stats/UPDATE.updateHandler.docsPending")
            .is_some(),
        "stats must be honoured even though its raw value is `true?omitHeader=false`, matching \
         the captured trace (UPDATE.updateHandler.stats is present with real values there), \
         got: {body}"
    );
}

// --- docsPending: real uncommitted-doc count --------------------------------

#[tokio::test]
async fn mbeans_docs_pending_reflects_real_uncommitted_docs_then_zero_after_commit() {
    let dir = TempDir::new().expect("temp dir");
    let core = "mbeans_pending";
    let app = build_mbeans_app(dir.path(), core, None).expect("app must build");

    let (status, body) = post_m(
        &app,
        core,
        "update?wt=json",
        &json!([
            {"id": "p1", "body": "one"},
            {"id": "p2", "body": "two"},
            {"id": "p3", "body": "three"},
            {"id": "p4", "body": "four"}
        ])
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "add 4 uncommitted docs: {body}");

    let (status, body) = get_m(&app, core, "admin/mbeans?stats=true&wt=json").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body.pointer("/solr-mbeans/UPDATE/updateHandler/stats/UPDATE.updateHandler.docsPending"),
        Some(&json!(4)),
        "docsPending must equal the real count of uncommitted docs, got: {body}"
    );

    let (status, body) = post_m(&app, core, "update?commit=true", "[]").await;
    assert_eq!(status, StatusCode::OK, "commit: {body}");

    let (status, body) = get_m(&app, core, "admin/mbeans?stats=true&wt=json").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body.pointer("/solr-mbeans/UPDATE/updateHandler/stats/UPDATE.updateHandler.docsPending"),
        Some(&json!(0)),
        "docsPending must drop to 0 once every pending doc has been committed, got: {body}"
    );
}

// --- deletesById / deletesByQuery: independent lifetime counters -----------

#[tokio::test]
async fn mbeans_deletes_by_id_and_deletes_by_query_increment_independently() {
    let dir = TempDir::new().expect("temp dir");
    let core = "mbeans_deletes";
    let app = build_mbeans_app(dir.path(), core, None).expect("app must build");

    let (status, body) = post_m(
        &app,
        core,
        "update?commit=true",
        &json!([
            {"id": "e1", "body": "alpha term"},
            {"id": "e2", "body": "bravo term"},
            {"id": "e3", "body": "charlie term"},
            {"id": "e4", "body": "delta needle"}
        ])
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seed: {body}");

    let (status, body) = get_m(&app, core, "admin/mbeans?stats=true&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.pointer("/solr-mbeans/UPDATE/updateHandler/stats/UPDATE.updateHandler.deletesById"),
        Some(&json!(0)),
        "deletesById must start at 0, got: {body}"
    );
    assert_eq!(
        body.pointer("/solr-mbeans/UPDATE/updateHandler/stats/UPDATE.updateHandler.deletesByQuery"),
        Some(&json!(0)),
        "deletesByQuery must start at 0, got: {body}"
    );

    // Single-id delete: +1.
    let (status, body) = post_m(&app, core, "update?commit=true", r#"{"delete":["e1"]}"#).await;
    assert_eq!(status, StatusCode::OK, "delete e1: {body}");
    let (status, body) = get_m(&app, core, "admin/mbeans?stats=true&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.pointer("/solr-mbeans/UPDATE/updateHandler/stats/UPDATE.updateHandler.deletesById"),
        Some(&json!(1)),
        "deletesById must be 1 after deleting a single id, got: {body}"
    );
    assert_eq!(
        body.pointer("/solr-mbeans/UPDATE/updateHandler/stats/UPDATE.updateHandler.deletesByQuery"),
        Some(&json!(0)),
        "deletesByQuery must be untouched by a delete-by-id call, got: {body}"
    );

    // Two-id list delete in one call: cumulative total 3, not 2 -- a
    // per-call (rather than per-id) counter would fail this specific
    // assertion (see this file's header comment on the interpretation).
    let (status, body) = post_m(
        &app,
        core,
        "update?commit=true",
        r#"{"delete":["e2","e3"]}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "delete e2,e3: {body}");
    let (status, body) = get_m(&app, core, "admin/mbeans?stats=true&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.pointer("/solr-mbeans/UPDATE/updateHandler/stats/UPDATE.updateHandler.deletesById"),
        Some(&json!(3)),
        "deletesById must accumulate per id deleted (1 + 2 = 3), got: {body}"
    );
    assert_eq!(
        body.pointer("/solr-mbeans/UPDATE/updateHandler/stats/UPDATE.updateHandler.deletesByQuery"),
        Some(&json!(0)),
        "deletesByQuery must still be untouched, got: {body}"
    );

    // Delete-by-query: +1, deletesById unaffected.
    let (status, body) = post_m(
        &app,
        core,
        "update?commit=true",
        r#"{"delete":{"query":"body:needle"}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "delete by query: {body}");
    let (status, body) = get_m(&app, core, "admin/mbeans?stats=true&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.pointer("/solr-mbeans/UPDATE/updateHandler/stats/UPDATE.updateHandler.deletesByQuery"),
        Some(&json!(1)),
        "deletesByQuery must be 1 after a single delete-by-query call, got: {body}"
    );
    assert_eq!(
        body.pointer("/solr-mbeans/UPDATE/updateHandler/stats/UPDATE.updateHandler.deletesById"),
        Some(&json!(3)),
        "deletesById must remain 3, independent of the delete-by-query call, got: {body}"
    );
}

/// Mutation guard for `CoreIndex::delete_by_query`'s ordering
/// (`src/core_index.rs`): the `fetch_add` sits AFTER
/// `let parsed = self.parse_query(...)?;` precisely so a query that never
/// parsed never became a delete and must not count. That guard was asserted
/// only in a comment -- hoisting the `fetch_add` above the parse left the
/// whole suite green. Project rule: code whose whole value is failing
/// correctly gets mutation-tested. Verified by hoisting: with the
/// `fetch_add` moved above the parse, this test fails with
/// `deletesByQuery == 1`.
#[tokio::test]
async fn mbeans_deletes_by_query_does_not_count_a_query_that_failed_to_parse() {
    let dir = TempDir::new().expect("temp dir");
    let core = "mbeans_bad_delete_query";
    let app = build_mbeans_app(dir.path(), core, None).expect("app must build");

    let (status, body) = post_m(
        &app,
        core,
        "update?commit=true",
        &json!([{"id": "b1", "body": "alpha"}]).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seed: {body}");

    // An unclosed `/regex` is an ordinary parse failure (see
    // `tests/query_types.rs::regex_unclosed_is_a_400_syntax_error`), so
    // `delete_by_query` bails at `parse_query` before touching the writer.
    let (status, body) = post_m(
        &app,
        core,
        "update?commit=true",
        &json!({"delete": {"query": "body:/unclosed"}}).to_string(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a delete-by-query whose query cannot be parsed must be an error, got: {body}"
    );

    let (status, body) = get_m(&app, core, "admin/mbeans?stats=true&wt=json").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body.pointer("/solr-mbeans/UPDATE/updateHandler/stats/UPDATE.updateHandler.deletesByQuery"),
        Some(&json!(0)),
        "a query that failed to parse never became a delete, so deletesByQuery must still be 0, \
         got: {body}"
    );
}

// --- INDEX.size / CORE.coreName: real state, non-default core name ---------

/// Reverses `human_size`'s `"{value:.1} {unit}"` (`src/admin_ui.rs`) rendering
/// back to an approximate byte count, so this test can assert a real growth
/// relationship rather than a brittle exact figure Tantivy's segment layout
/// does not promise.
fn parse_human_size(s: &str) -> f64 {
    let (value, unit) = s
        .rsplit_once(' ')
        .unwrap_or_else(|| panic!("INDEX.size must be `\"<value> <unit>\"`, got `{s}`"));
    let value: f64 = value
        .parse()
        .unwrap_or_else(|_| panic!("INDEX.size value must be numeric, got `{s}`"));
    let multiplier = match unit {
        "B" => 1.0,
        "KB" => 1024.0,
        "MB" => 1024.0 * 1024.0,
        "GB" => 1024.0 * 1024.0 * 1024.0,
        "TB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        other => panic!("unrecognized INDEX.size unit `{other}` in `{s}`"),
    };
    value * multiplier
}

#[tokio::test]
async fn mbeans_core_name_is_the_real_configured_core_name_not_hardcoded() {
    let dir_a = TempDir::new().expect("temp dir");
    let core_a = "mbeans_alpha_core";
    let app_a = build_mbeans_app(dir_a.path(), core_a, None).expect("app must build");
    let (status, body_a) = get_m(&app_a, core_a, "admin/mbeans?stats=true&wt=json").await;
    assert_eq!(status, StatusCode::OK, "body: {body_a}");

    let dir_b = TempDir::new().expect("temp dir");
    let core_b = "mbeans_bravo_core";
    let app_b = build_mbeans_app(dir_b.path(), core_b, None).expect("app must build");
    let (status, body_b) = get_m(&app_b, core_b, "admin/mbeans?stats=true&wt=json").await;
    assert_eq!(status, StatusCode::OK, "body: {body_b}");

    assert_eq!(
        body_a.pointer("/solr-mbeans/CORE/core/stats/CORE.coreName"),
        Some(&json!(core_a)),
        "CORE.coreName must be this process's real (non-default) core name, got: {body_a}"
    );
    assert_eq!(
        body_b.pointer("/solr-mbeans/CORE/core/stats/CORE.coreName"),
        Some(&json!(core_b)),
        "CORE.coreName must be this process's real (non-default) core name, got: {body_b}"
    );
    assert_ne!(
        body_a.pointer("/solr-mbeans/CORE/core/stats/CORE.coreName"),
        body_b.pointer("/solr-mbeans/CORE/core/stats/CORE.coreName"),
        "two differently-named cores must report differently -- a hardcoded value would make \
         these equal"
    );
}

#[tokio::test]
async fn mbeans_index_size_tracks_real_on_disk_size_growth() {
    let dir_small = TempDir::new().expect("temp dir");
    let core_small = "mbeans_size_small";
    let app_small = build_mbeans_app(dir_small.path(), core_small, None).expect("app must build");
    let (status, body) = post_m(
        &app_small,
        core_small,
        "update?commit=true",
        &json!([{"id": "s1", "body": "x"}]).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seed small: {body}");
    let (status, body_small) =
        get_m(&app_small, core_small, "admin/mbeans?stats=true&wt=json").await;
    assert_eq!(status, StatusCode::OK, "body: {body_small}");

    let dir_large = TempDir::new().expect("temp dir");
    let core_large = "mbeans_size_large";
    let app_large = build_mbeans_app(dir_large.path(), core_large, None).expect("app must build");
    let long_body = "quick brown fox jumps over the lazy dog ".repeat(500);
    let docs: Vec<Value> = (0..200)
        .map(|i| json!({"id": format!("l{i}"), "body": long_body.clone()}))
        .collect();
    let (status, body) = post_m(
        &app_large,
        core_large,
        "update?commit=true",
        &json!(docs).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seed large: {body}");
    let (status, body_large) =
        get_m(&app_large, core_large, "admin/mbeans?stats=true&wt=json").await;
    assert_eq!(status, StatusCode::OK, "body: {body_large}");

    let size_small = body_small
        .pointer("/solr-mbeans/CORE/core/stats/INDEX.size")
        .and_then(Value::as_str)
        .expect("INDEX.size must be a string");
    let size_large = body_large
        .pointer("/solr-mbeans/CORE/core/stats/INDEX.size")
        .and_then(Value::as_str)
        .expect("INDEX.size must be a string");

    assert!(
        parse_human_size(size_large) > parse_human_size(size_small),
        "INDEX.size must track real on-disk size: a ~200-doc, long-body core (`{size_large}`) \
         must report larger than a 1-tiny-doc core (`{size_small}`) -- a hardcoded/static value \
         would make these equal"
    );
}

// --- softAutoCommitMaxTime: config-driven "<N>ms" string, absent when unset -

#[tokio::test]
async fn mbeans_soft_auto_commit_max_time_reflects_configured_autocommit_max_time() {
    let dir = TempDir::new().expect("temp dir");
    let core = "mbeans_softcommit_configured";
    let configured_ms = 5000;
    let app = build_mbeans_app(
        dir.path(),
        core,
        Some(&format!(
            "[commit]\nautocommit_max_time = {configured_ms}\n"
        )),
    )
    .expect("app must build");
    let (status, body) = get_m(&app, core, "admin/mbeans?stats=true&wt=json").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body.pointer(
            "/solr-mbeans/UPDATE/updateHandler/stats/UPDATE.updateHandler.softAutoCommitMaxTime"
        ),
        Some(&json!(format!("{configured_ms}ms"))),
        "softAutoCommitMaxTime must reflect config.commit.autocommit_max_time as Solr's own \
         \"<N>ms\" string (per solr-ref/search-api/trace/00025.json, not a bare integer), \
         got: {body}"
    );
}

#[tokio::test]
async fn mbeans_soft_auto_commit_max_time_is_absent_when_unset() {
    let dir = TempDir::new().expect("temp dir");
    let core = "mbeans_softcommit_unset";
    let app = build_mbeans_app(dir.path(), core, None).expect("app must build");
    let (status, body) = get_m(&app, core, "admin/mbeans?stats=true&wt=json").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        body.pointer(
            "/solr-mbeans/UPDATE/updateHandler/stats/UPDATE.updateHandler.softAutoCommitMaxTime"
        )
        .is_none(),
        "the key must be ABSENT when autocommit_max_time is unset -- Solr omits it entirely \
         when soft autocommit is disabled, and SolrConnectorPluginBase's `isset(...)` guard \
         (SolrConnectorPluginBase.php:789-792) only falls back to its own `-1` default when the \
         key is missing, so `-1` is never a value Solr puts on the wire, got: {body}"
    );
}

// --- unknown core -------------------------------------------------------------

/// Mutation guard for the `check_core` call in the handler (`src/lib.rs`),
/// mirroring `tests/admin_luke.rs::luke_unknown_core_is_a_json_404`. Sibling
/// #156 shipped without this guard and the reviewer caught it; here, deleting
/// the `check_core` line leaves the whole rest of the suite green while
/// `GET /solr/nosuchcore/admin/mbeans` happily reports the real core's stats
/// (and its `CORE.coreName`) under any core name at all. Verified by
/// deletion: without `check_core` this test fails with 200.
#[tokio::test]
async fn mbeans_unknown_core_is_a_json_404() {
    let dir = TempDir::new().expect("temp dir");
    let core = "mbeans_real_core";
    let app = build_mbeans_app(dir.path(), core, Some(SOFT_AUTOCOMMIT_ON)).expect("app must build");

    let (status, body) = get_m(
        &app,
        "nosuchcore",
        "admin/mbeans?stats=true&wt=json&json.nl=map",
    )
    .await;

    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an unknown core must 404, got: {body}"
    );
    let header = body
        .get("responseHeader")
        .unwrap_or_else(|| panic!("the WithParams envelope carries responseHeader, got: {body}"));
    assert_eq!(header["status"].as_u64(), Some(404), "body: {body}");
    assert!(
        header.get("params").is_some(),
        "this route uses the WithParams envelope, so params are echoed, got: {body}"
    );
    assert_eq!(body["error"]["code"].as_i64(), Some(404), "body: {body}");
    assert!(
        body["error"]["msg"]
            .as_str()
            .is_some_and(|m| m.contains("nosuchcore")),
        "error.msg must name the unknown core, got: {body}"
    );
    assert!(
        body.get("solr-mbeans").is_none(),
        "an unknown core must not leak the real core's mbeans stats, got: {body}"
    );
}

// --- params allowlist --------------------------------------------------------

#[tokio::test]
async fn mbeans_strict_params_accepts_the_documented_allowlist() {
    let dir = TempDir::new().expect("temp dir");
    let core = "mbeans_strict_ok";
    let app =
        build_mbeans_app(dir.path(), core, Some("strict_params = true\n")).expect("app must build");
    let (status, body) = get_m(
        &app,
        core,
        "admin/mbeans?stats=true&wt=json&json.nl=map&cat=UPDATE&key=updateHandler",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "strict_params=true must accept stats/wt/json.nl/cat/key, got: {body}"
    );
}

#[tokio::test]
async fn mbeans_strict_params_rejects_unknown_param() {
    let dir = TempDir::new().expect("temp dir");
    let core = "mbeans_strict_bad";
    let app =
        build_mbeans_app(dir.path(), core, Some("strict_params = true\n")).expect("app must build");
    let (status, body) = get_m(&app, core, "admin/mbeans?stats=true&wt=json&bogus=1").await;
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
