//! Ping/health admin-UI page tests (issue #130, PRD §5 "v2.5 — Admin web
//! UI").
//!
//! Scope: a new page (or status element) showing this process's ping/health
//! status, reusing the existing `/solr/{core}/admin/ping` handler/logic
//! directly rather than a second health-check code path — the same "call the
//! real handler, don't reimplement it" pattern issue #127's query tester
//! established for `/select` (`query_ui` in `src/lib.rs` calls `select()`
//! itself).
//!
//! Routing decision: this suite targets a *dedicated* `GET /ui/ping` page,
//! not a status element bolted onto the existing `GET /ui` core page.
//! Reasoning:
//!   - Issue #130 itself offers the dedicated-route example first ("e.g.
//!     `GET /ui/ping`"), and every one of the three prior v2.5 flesh-out
//!     milestones (#127 query tester, #128 schema view, #129 index stats)
//!     shipped as its own route rather than growing `/ui`'s existing page —
//!     "match existing conventions closely rather than inventing new ones"
//!     (the task spec) favours continuing that one-concern-per-page pattern
//!     over a first-of-its-kind departure from it.
//!   - A dedicated route is also easier to reuse the real `ping` handler
//!     from cleanly: `query_ui` calls `select(State, AxPath(core),
//!     RawQuery)` and adapts its `Response` into the page; a `ping_ui`
//!     handler can do the exact same thing with `ping(...)`. Splicing that
//!     same call into the existing `core_ui` handler (which currently takes
//!     no path/query args and reads `CoreIndex` accessors directly, not a
//!     `Response` to unwrap) would be more invasive for equivalent value.
//!
//! If the implementor disagrees and instead adds a status element to the
//! existing `/ui` page, this file's route constant and the tests in it are
//! the ones to revisit or fold into `tests/admin_ui.rs` — flagging this
//! explicitly rather than deciding it silently, per the task spec.
//!
//! Route name is not pinned by issue #130 ("e.g. `GET /ui/ping`"). These
//! tests hit `GET /ui/ping` — if the implementor picks a different path,
//! update `UI_PING_ROUTE` below rather than every call site (same convention
//! `tests/admin_ui.rs`/`tests/admin_ui_index_stats.rs` established for
//! `UI_ROUTE`/`UI_STATS_ROUTE`).
//!
//! Premise check on "reuse, no second code path": reading `src/lib.rs`'s
//! `ping()` handler, its status is *unconditional* — `check_core` only ever
//! fails for a core-name mismatch (unreachable from any `/ui/*` route, which
//! never takes a core segment: this process serves exactly one core), and
//! past that check the handler always returns `{"status": "OK", ...}`
//! regardless of doc count, index state, or anything else. There is no real
//! "unhealthy core" scenario producible in this codebase today, so this
//! suite does not invent one; instead the reuse guarantee is tested the same
//! way #127's query tester is (`ping_page_reflects_the_real_admin_ping_status_value`
//! below): the UI page's rendered status must equal the value the real
//! `/solr/{core}/admin/ping` endpoint actually returns for the same request,
//! not an independently hardcoded string. If a future change makes `ping()`'s
//! status conditional, that test starts actually distinguishing reuse from a
//! hardcoded-healthy UI; today it mainly guards against silently drifting
//! wording (e.g. the UI rendering "healthy" while the wire endpoint says
//! "OK").

mod common;

use common::{CORE, SCHEMA_TOML, app_with_schema, get, get_text, indexed_app};

/// Not pinned by issue #130; adjust here if the implementor picks a
/// different path (see the module doc's routing-decision note above).
const UI_PING_ROUTE: &str = "/ui/ping";

/// Finds the first case-insensitive occurrence of `label` in `haystack`,
/// then returns the first contiguous run of alphanumeric characters within
/// the following 200 characters (or `None` if `label` isn't present, or no
/// such run follows it in that window). Mirrors
/// `tests/admin_ui_index_stats.rs`'s `number_after_label`, generalised to
/// alphabetic tokens (e.g. `"OK"`) since a health status is text, not a
/// count.
fn word_after_label(haystack: &str, label: &str) -> Option<String> {
    let lower = haystack.to_lowercase();
    let label_lower = label.to_lowercase();
    let label_pos = lower.find(&label_lower)?;
    let start_search = label_pos + label_lower.len();
    let window_end = (start_search + 200).min(haystack.len());
    let window = &haystack[start_search..window_end];

    let mut begin = None;
    for (i, c) in window.char_indices() {
        if c.is_alphanumeric() {
            begin = Some(i);
            break;
        }
    }
    let begin = begin?;
    let mut end = begin;
    for (i, c) in window[begin..].char_indices() {
        if c.is_alphanumeric() {
            end = begin + i + c.len_utf8();
        } else {
            break;
        }
    }
    Some(window[begin..end].to_string())
}

#[tokio::test]
async fn ping_page_returns_200_html_for_a_healthy_core() {
    let (app, _dir) = indexed_app().await;

    let (status, headers, body) = get_text(&app, UI_PING_ROUTE).await;

    assert_eq!(
        status, 200,
        "GET {UI_PING_ROUTE} must return 200 for a healthy core; body: {body}"
    );
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.contains("text/html"),
        "expected a text/html Content-Type, got `{content_type}`"
    );
    assert!(
        body.contains(CORE),
        "ping page must identify the core it is reporting on (`{CORE}`); body: {body}"
    );
    assert!(
        word_after_label(&body, "status").as_deref() == Some("OK"),
        "ping page must render a `status` label followed by the real \
         `/admin/ping` status value (`OK`), not omit it or say something \
         else; body: {body}"
    );
}

#[tokio::test]
async fn ping_page_shows_ok_for_an_empty_core_too() {
    // The real `/admin/ping` handler's status does not depend on doc count
    // (see `ping()` in src/lib.rs — it is unconditional past the
    // core-name check), so the UI page must not either.
    let dir = tempfile::TempDir::new().expect("create temp dir");
    let app = app_with_schema(dir.path(), SCHEMA_TOML).expect("wayfinder::app must build");

    let (status, _headers, body) = get_text(&app, UI_PING_ROUTE).await;

    assert_eq!(
        status, 200,
        "an empty core must still ping healthy, not error; body: {body}"
    );
    assert!(
        word_after_label(&body, "status").as_deref() == Some("OK"),
        "ping page must report status OK for an empty core, exactly as \
         `/admin/ping` itself does; body: {body}"
    );
}

/// The reuse guard named in the module doc: the UI page's rendered status
/// must equal what the real `/solr/{core}/admin/ping` endpoint actually
/// returns for this process, fetched independently via `common::get` (the
/// same wire path a real Solr client would hit) — not a value the UI
/// handler invents or hardcodes on its own.
#[tokio::test]
async fn ping_page_reflects_the_real_admin_ping_status_value() {
    let (app, _dir) = indexed_app().await;

    let (wire_status_code, wire_body) = get(&app, "admin/ping?wt=json").await;
    assert_eq!(
        wire_status_code, 200,
        "the real /admin/ping endpoint must itself return 200 for this \
         suite's premise to hold"
    );
    let real_status = wire_body["status"]
        .as_str()
        .expect("/admin/ping response must have a string `status` field")
        .to_string();

    let (_status, _headers, body) = get_text(&app, UI_PING_ROUTE).await;

    let rendered = word_after_label(&body, "status").unwrap_or_else(|| {
        panic!("ping page must render a `status` label with a value next to it; body: {body}")
    });
    assert_eq!(
        rendered, real_status,
        "the ping page's rendered status must be exactly the value the real \
         /admin/ping endpoint returns (`{real_status}`), reusing its handler \
         rather than reimplementing health logic separately; body: {body}"
    );
}

#[tokio::test]
async fn ping_page_is_read_only_and_idempotent() {
    let (app, _dir) = indexed_app().await;

    let (status_first, _headers_first, body_first) = get_text(&app, UI_PING_ROUTE).await;
    let (status_second, _headers_second, body_second) = get_text(&app, UI_PING_ROUTE).await;

    assert_eq!(status_first, 200);
    assert_eq!(status_second, 200);
    assert_eq!(
        body_first, body_second,
        "hitting the ping page twice in a row must not change its content \
         — this is a read-only status view with no write/commit side effect"
    );
    assert!(
        !body_first.to_lowercase().contains("<form"),
        "the ping page is read-only and must not contain a form or mutation \
         control; body: {body_first}"
    );

    let (select_status, select_body) = common::get(&app, "select?q=*:*&rows=0&wt=json").await;
    assert_eq!(select_status, 200);
    assert_eq!(
        select_body["response"]["numFound"], 5,
        "rendering the ping page must not add, remove, or otherwise mutate \
         documents in the core"
    );
}
