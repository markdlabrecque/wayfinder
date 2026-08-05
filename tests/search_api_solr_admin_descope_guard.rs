//! Expiring guard for the three `search_api_solr_admin` endpoints issue #354
//! descoped — core reload (`/solr/admin/cores?action=RELOAD`), field analysis
//! (`/<core>/analysis/field`), and configset file read (`/<core>/admin/file`)
//! (PRD §5 "`search_api_solr_admin` — a Solr connector module, unreachable
//! against Wayfinder").
//!
//! Found by the 2026-08-04 full-source sweep of `search_api_solr` 4.4.0's
//! `modules/search_api_solr_admin/`. The three are real on the standard
//! (non-cloud) connector, so the SolrCloud non-goal does not by itself exclude
//! them (#354's own framing). A stronger exclusion does: **`search_api_solr_admin`
//! cannot see a Wayfinder server at all.** Every route and every command path in
//! the module hard-gates on `$backend instanceof SolrBackendInterface`, and
//! `WayfinderBackend` is a separate Search API backend (`extends
//! BackendPluginBase implements PluginFormInterface`), not a `search_api_solr`
//! connector — deliberately, because Wayfinder is not Solr. So against a
//! Wayfinder server the reload-core and field-analysis forms 403 at Drupal's own
//! route access check, the reload Drush command throws "Server is not a Solr
//! server" from `Utility::getSolrConnector`, and no HTTP request for any of the
//! three endpoints is ever emitted (finding 194).
//!
//! Because none of the three is reachable, none moves the coverage denominator:
//! they are in none of the 28 traces and none of the contract's 9 endpoints / 75
//! items, so the decision not to build them moves the coverage fraction by zero
//! — 75/75 is unchanged (#225).
//!
//! Per CLAUDE.md's rule for deliberate skips, this guard must fail the day the
//! evidence stops holding. When it goes red, the fix is **not** to weaken it —
//! it is to revisit PRD §5's #354 decision with the new evidence: if the
//! `instanceof SolrBackendInterface` gate leaves the source, if
//! `WayfinderBackend` starts implementing `SolrBackendInterface`, or if a trace
//! carries one of the three endpoints, decide whether to build it, then delete
//! the corresponding arm of this guard.
//!
//! Five channels, mirroring `tests/q_op_qt_descope_guard.rs` and
//! `tests/version_write_descope_guard.rs`:
//!
//!   1. The vendored `search_api_solr` 4.4.0 source still gates every admin
//!      route and command path on `instanceof SolrBackendInterface`, still
//!      throws from `Utility::getSolrConnector` for a non-Solr backend, and
//!      still carries the three connector methods that would be reached
//!      otherwise.
//!   2. `WayfinderBackend` (our connector module) still does NOT implement
//!      `SolrBackendInterface` — the other half of the unreachability
//!      invariant.
//!   3. No captured client request across the 28 committed traces in
//!      `solr-ref/search-api/trace/` hits any of the three endpoints.
//!   4. PRD §5 records the descope.
//!   5. The three endpoints still 404 against a built app — a later silent
//!      route addition is caught.

// The dead-code allow for partially-used shared helpers is an inner attribute
// inside `tests/common/mod.rs`; repeating it here is a clippy error under
// `-D warnings`.
mod common;

use std::path::{Path, PathBuf};

use axum::http::StatusCode;
use serde_json::Value;

use common::{get, indexed_app};

// The `search_api_solr` 4.4.0 source. These are committed under coverage/ and
// pinned by provenance; reading them directly (not via the structured evidence
// JSON) matches `tests/q_op_qt_descope_guard.rs`.
const SOLR_ADMIN_ACCESS_CHECK: &str = include_str!(
    "../coverage/search_api_solr_4.4.0_source/modules/search_api_solr_admin/src/Access/SolrAdminAccessCheck.php"
);
const LOCAL_ACTION_ACCESS_CHECK: &str =
    include_str!("../coverage/search_api_solr_4.4.0_source/src/Access/LocalActionAccessCheck.php");
const CORE_UTILITY: &str =
    include_str!("../coverage/search_api_solr_4.4.0_source/src/Utility/Utility.php");
const STANDARD_CONNECTOR: &str = include_str!(
    "../coverage/search_api_solr_4.4.0_source/src/Plugin/SolrConnector/StandardSolrConnector.php"
);
const CONNECTOR_BASE: &str = include_str!(
    "../coverage/search_api_solr_4.4.0_source/src/SolrConnector/SolrConnectorPluginBase.php"
);

// Our connector module: the other half of the unreachability invariant. If this
// ever starts `implements SolrBackendInterface`, every `search_api_solr_admin`
// route stops 403ing and the descope premise is gone.
const WAYFINDER_BACKEND: &str = include_str!(
    "../drupal/search_api_wayfinder/src/Plugin/search_api/backend/WayfinderBackend.php"
);

const TRACE_DIR: &str = "solr-ref/search-api/trace";
const PRD: &str = include_str!("../docs/PRD.md");

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

// --- source channel: the instanceof SolrBackendInterface gate -------------

/// The reload-core route's access check still requires a `SolrBackendInterface`
/// backend (and a non-cloud connector). This is the gate the whole descope
/// rests on: if `search_api_solr_admin` stops requiring it here, the route is
/// no longer access-forbidden against a Wayfinder server and the unreachability
/// premise moved.
#[test]
fn source_admin_access_check_still_gates_on_solr_backend_interface() {
    assert!(
        SOLR_ADMIN_ACCESS_CHECK.contains("instanceof SolrBackendInterface"),
        "`SolrAdminAccessCheck` no longer gates on `instanceof SolrBackendInterface`. PRD §5's \
         #354 descope rests on every `search_api_solr_admin` route access-gating on the Solr \
         backend interface; if that gate moved, the route may be reachable against a Wayfinder \
         server and the descope must be revisited."
    );
}

/// The field-analysis route's access check (`LocalActionAccessCheck`, shipped in
/// the `search_api_solr` core module) still requires a `SolrBackendInterface`
/// backend. A separate access check guards that one route, so it gets its own
/// assertion rather than being implied by the reload-core one.
#[test]
fn source_local_action_access_check_still_gates_on_solr_backend_interface() {
    assert!(
        LOCAL_ACTION_ACCESS_CHECK.contains("instanceof SolrBackendInterface"),
        "`LocalActionAccessCheck` (the field-analysis route's access check) no longer gates on \
         `instanceof SolrBackendInterface`. PRD §5's #354 descope rests on that route \
         access-gating on the Solr backend interface; revisit the descope if it moved."
    );
}

/// The command path is the strongest statement of intent. `Utility::getSolrConnector`
/// — which the reload Drush command and every command-helper entry point calls —
/// still throws "Server is not a Solr server" for a non-`SolrBackendInterface`
/// backend, before it can reach `reloadCore()`/`getAnalysisQueryField()`/`getFile()`.
/// If this throw leaves the source, the command path may stop failing fast against
/// a Wayfinder server and the descope premise moved.
#[test]
fn source_command_path_still_throws_for_non_solr_backend() {
    assert!(
        CORE_UTILITY.contains("is not a Solr server"),
        "`Utility::getSolrConnector` no longer throws for a non-Solr backend. PRD §5's #354 \
         descope rests on the command path failing fast with 'Server is not a Solr server' before \
         reaching any connector method; if that guard moved, revisit the descope."
    );
}

/// `getSolrConnector` still returns `SolrConnectorInterface`, which is what makes
/// the `instanceof` check inside it meaningful (a non-Solr backend cannot satisfy
/// the declared return type's source). ponytail: asserting the positive
/// `: SolrConnectorInterface` signature rather than the negative "no Wayfinder
/// path"; the executable 404 half below is the backstop for a route added out of
/// band.
#[test]
fn source_get_solr_connector_still_returns_solr_connector_interface() {
    assert!(
        CORE_UTILITY.contains("getSolrConnector(ServerInterface $server): SolrConnectorInterface"),
        "`Utility::getSolrConnector`'s signature changed. PRD §5's #354 descope records it as \
         `: SolrConnectorInterface`; if the return type broadened, re-derive whether a non-Solr \
         backend can now flow through it before trusting this guard."
    );
}

// --- source channel: the three connector methods are still real -----------
//
// These assert the *positive* — that the three endpoints #354 names are still
// implemented on the standard/base connector exactly as the descope describes —
// so the guard stays meaningful the day the source is upgraded. They are not
// reachability claims; the gates above and the WayfinderBackend invariant below
// are.

/// Core reload: `StandardSolrConnector::reloadCore()` builds a CoreAdmin
/// `createReload()`. The descope describes this method; if it moves or is
/// renamed, re-derive the reload path before trusting the rest of this guard.
#[test]
fn source_standard_connector_still_implements_reload_core() {
    assert!(
        STANDARD_CONNECTOR.contains("createReload()"),
        "`StandardSolrConnector::reloadCore()` no longer builds a `createReload()` action. PRD \
         §5's #354 descope describes core reload through that Solarium call; if it moved, \
         re-derive the reload path before extending this guard."
    );
}

/// Field analysis: `SolrConnectorPluginBase::getAnalysisQueryField()` returns
/// Solarium's `createAnalysisField()`.
#[test]
fn source_connector_base_still_exposes_analysis_field_query() {
    assert!(
        CONNECTOR_BASE.contains("createAnalysisField()"),
        "`SolrConnectorPluginBase::getAnalysisQueryField()` no longer returns Solarium's \
         `createAnalysisField()`. PRD §5's #354 descope describes field analysis through that \
         call; if it moved, re-derive the analysis/field path before extending this guard."
    );
}

/// Configset file read: `SolrConnectorPluginBase::getFile()` targets the
/// `<core>/admin/file` handler.
#[test]
fn source_connector_base_still_targets_admin_file_handler() {
    assert!(
        CONNECTOR_BASE.contains("/admin/file'"),
        "`SolrConnectorPluginBase::getFile()` no longer targets the `<core>/admin/file` handler. \
         PRD §5's #354 descope describes configset file read through that handler; if it moved, \
         re-derive the admin/file path before extending this guard."
    );
}

// --- WayfinderBackend invariant: still not a SolrBackendInterface ---------

/// `WayfinderBackend` still does NOT implement `SolrBackendInterface`. This is
/// the other half of the unreachability invariant: the access gates above 403 a
/// Wayfinder server precisely because our backend is a peer of
/// `SearchApiSolrBackend`, not a Solr connector. The day this changes — our
/// connector module starts `implements SolrBackendInterface` — every
/// `search_api_solr_admin` route stops 403ing and the descope premise is gone.
#[test]
fn wayfinder_backend_still_does_not_implement_solr_backend_interface() {
    assert!(
        !WAYFINDER_BACKEND.contains("SolrBackendInterface"),
        "`WayfinderBackend` now references `SolrBackendInterface`. PRD §5's #354 descope rests on \
         our connector module being a separate Search API backend, not a `search_api_solr` \
         connector; if it started implementing `SolrBackendInterface`, `search_api_solr_admin`'s \
         routes would stop 403ing against a Wayfinder server and the descope must be revisited."
    );
}

/// Positive control: `WayfinderBackend` still extends the Search API
/// `BackendPluginBase` and implements `PluginFormInterface` — the declaration
/// that makes it a backend peer rather than a connector. Without this, the
/// negative assertion above could stay green while the class was restructured
/// into something unrecognisable.
#[test]
fn wayfinder_backend_is_still_a_backend_plugin_base() {
    assert!(
        WAYFINDER_BACKEND.contains("class WayfinderBackend extends BackendPluginBase"),
        "`WayfinderBackend` no longer extends `BackendPluginBase`. The #354 guard's claim that it \
         is a Search API backend peer (not a Solr connector) rests on that declaration; if the \
         class was restructured, re-derive the invariant."
    );
}

// --- trace channel: none of the three endpoints is captured ---------------

fn trace_files() -> Vec<PathBuf> {
    let dir = root().join(TRACE_DIR);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    files.sort();
    files
}

fn load(path: &Path) -> Value {
    let raw =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

#[test]
fn trace_corpus_is_the_28_traces_the_decision_was_checked_against() {
    let count = trace_files().len();
    assert_eq!(
        count, 28,
        "the #354 descope was checked against exactly 28 committed traces in \
         solr-ref/search-api/trace/; the corpus now has {count}. If new traces were added, re-check \
         every endpoint needle against their request sides before treating this guard as still \
         valid — see issue #354."
    );
}

/// The three endpoint paths the descope covers. `admin/cores` is server-level
/// (note the path shape); the other two are core-relative. Scanned as substrings
/// of the serialized request, which covers the path (query string), headers, and
/// a JSON/form body uniformly.
const ENDPOINT_NEEDLES: &[&str] = &["analysis/field", "admin/file", "admin/cores"];

/// No captured client request hits any of the three endpoints. The capture site
/// used `search_api_solr` against a real Solr; even so, none of the 28 traces
/// carries `search_api_solr_admin` traffic, because that module has no path to a
/// non-Solr backend (finding 194). This guard trips the day a trace does.
#[test]
fn no_trace_request_hits_any_of_the_three_endpoints() {
    for file in trace_files() {
        let capture = load(&file);
        let request = serde_json::to_string(&capture["request"])
            .unwrap_or_else(|e| panic!("{}: serialize request: {e}", file.display()));
        for needle in ENDPOINT_NEEDLES {
            assert!(
                !request.contains(needle),
                "{}: a captured request now hits `{needle}`. PRD §5's #354 descope rests on none \
                 of these three endpoints being reachable against a Wayfinder server; a trace \
                 carrying one means the premise no longer holds and the descope must be revisited.",
                file.display()
            );
        }
    }
}

/// Positive control for the request-side scan: the corpus DOES carry real
/// `/select` traffic, so the "no admin endpoint" claim is a real asymmetry
/// rather than a corpus that simply has no requests at all.
#[test]
fn traces_do_carry_select_traffic_so_the_absence_is_real() {
    let mut seen = false;
    for file in trace_files() {
        let capture = load(&file);
        let request = serde_json::to_string(&capture["request"]).unwrap_or_default();
        if request.contains("/select") || request.contains("q=") {
            seen = true;
            break;
        }
    }
    assert!(
        seen,
        "no trace carries any `/select` traffic. The endpoint-absence claim is now blind — pick a \
         new positive control or revisit the guard."
    );
}

// --- PRD channel: the descope is recorded --------------------------------

/// The whole §5 `search_api_solr_admin` subsection, from its heading up to (but
/// not including) the parity-roadmap heading that follows it.
fn admin_module_section() -> &'static str {
    let start = PRD
        .find(
            "### `search_api_solr_admin` — a Solr connector module, unreachable against Wayfinder",
        )
        .expect("PRD must still contain the `search_api_solr_admin` descope subsection");
    let rest = &PRD[start..];
    let end = rest.find("### Solr 9.x parity roadmap").expect(
        "the parity-roadmap heading must still follow the `search_api_solr_admin` subsection",
    );
    &rest[..end]
}

#[test]
fn prd_records_the_descope_and_its_decision_issue() {
    let section = admin_module_section();
    assert!(
        section.contains("#354"),
        "PRD §5's `search_api_solr_admin` subsection should reference issue #354 so a future reader \
         can find the decision and its evidence (finding 194)."
    );
    assert!(
        section.contains("finding 194"),
        "PRD §5's `search_api_solr_admin` subsection should reference finding 194, the source \
         evidence the descope rests on."
    );
    assert!(
        section.contains("SolrBackendInterface"),
        "PRD §5's `search_api_solr_admin` subsection should name the `SolrBackendInterface` gate \
         that makes the module unreachable against Wayfinder."
    );
}

/// The coverage-denominator claim: none of the three moves 75/75. The descope
/// is a no-op on the fraction precisely because the endpoints are unreachable,
/// and pinning that wording here stops a later edit from claiming the descope
/// widened the denominator (or, conversely, that building one would shrink it).
///
/// Checked as two tokens rather than the literal `"75/75 is unchanged"` because
/// the phrase is line-wrapped in the PRD and a substring match would break on a
/// reflow; `75/75` plus `unchanged` in the same section is distinctive enough.
#[test]
fn prd_records_that_the_descope_does_not_move_the_coverage_denominator() {
    let section = admin_module_section();
    assert!(
        section.contains("75/75"),
        "PRD §5's `search_api_solr_admin` subsection should state the coverage fraction (75/75) \
         so the descope's effect on the denominator is explicit."
    );
    assert!(
        section.contains("unchanged"),
        "PRD §5's `search_api_solr_admin` subsection should state that the descope leaves the \
         coverage fraction unchanged — none of the three endpoints is in the denominator because \
         none is reachable against a Wayfinder server."
    );
}

// --- executable: the three endpoints still 404 ---------------------------

/// Each of the three unrouted endpoints, addressed against the one core
/// `indexed_app` serves (`content`). `analysis/field` and `admin/file` are
/// core-relative; `admin/cores` is server-level (no `{core}` segment).
const ANALYSIS_FIELD_PATH: &str =
    "/wayfinder/content/analysis/field?analysis.fieldtype=text_en&analysis.fieldvalue=hello";
const ADMIN_FILE_PATH: &str = "/wayfinder/content/admin/file?file=schema.xml";
const ADMIN_CORES_PATH: &str = "/wayfinder/admin/cores?action=RELOAD&core=content";

/// A later silent route addition — someone wires `/wayfinder/{core}/analysis/field`
/// without revisiting this descope — must turn a loud 404 into a 200 and trip
/// this test. 404 is axum's default for an unregistered route (verified
/// empirically when this guard was written); a JSON error envelope from a
/// registered handler would also fail this assertion, which is the point.
#[tokio::test]
async fn analysis_field_endpoint_is_not_routed() {
    let (app, _dir) = indexed_app().await;
    let (status, _body) = get(&app, ANALYSIS_FIELD_PATH).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "`/wayfinder/{{core}}/analysis/field` is descoped (#354: unreachable against a Wayfinder \
         server) and must 404, not be silently routed. If it was intentionally added, revisit PRD \
         §5's `search_api_solr_admin` descope and delete this assertion rather than relaxing it."
    );
}

#[tokio::test]
async fn admin_file_endpoint_is_not_routed() {
    let (app, _dir) = indexed_app().await;
    let (status, _body) = get(&app, ADMIN_FILE_PATH).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "`/wayfinder/{{core}}/admin/file` is descoped (#354: unreachable against a Wayfinder \
         server, and Wayfinder has no configset to serve) and must 404, not be silently routed. \
         If it was intentionally added, revisit PRD §5's `search_api_solr_admin` descope and \
         delete this assertion rather than relaxing it."
    );
}

#[tokio::test]
async fn admin_cores_reload_endpoint_is_not_routed() {
    let (app, _dir) = indexed_app().await;
    let (status, _body) = get(&app, ADMIN_CORES_PATH).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "`/wayfinder/admin/cores?action=RELOAD` is descoped (#354: unreachable against a Wayfinder \
         server, and Wayfinder has no reload concept to answer it with) and must 404, not be \
         silently routed. If it was intentionally added, revisit PRD §5's `search_api_solr_admin` \
         descope and delete this assertion rather than relaxing it."
    );
}
