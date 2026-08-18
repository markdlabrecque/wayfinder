//! Admin UI tracer-bullet tests (issue #94, PRD §5 "v2.5 — Admin web UI").
//!
//! Scope: the tracer bullet only — one page, the core list, server-rendered
//! HTML, reading real doc count and on-disk size from a running core, via a
//! new route in the existing axum app.
//!
//! Route name is not pinned by the PRD or issue #94 ("naming TBD at
//! implementation time"). These tests hit `GET /ui` — if the implementor
//! picks a different path (e.g. `/admin`), update the `UI_ROUTE` constant
//! below rather than every call site.
//!
//! Assertions are on rendered HTML *content* (substrings), not exact markup,
//! per the task spec — exact byte-for-byte HTML is an implementation detail
//! this repo's compatibility contract (which governs the Solr wire format,
//! not this new UI surface) has no opinion on.
//!
//! Single-core-per-process only (see `src/lib.rs`'s module doc and
//! `AppState`: `build()` opens exactly one `CoreIndex`). Issue #94 and the
//! PRD's v2.5 section originally implied a multi-core *list*; both were
//! corrected to a single-core *view* after this file's tests surfaced the
//! conflict with the real architecture (no `CoreRegistry` exists). A
//! multi-core listing test (`tests/admin_ui_multi_core.rs`) was written
//! against that premise and has been removed rather than built toward, per
//! the resolution recorded in issue #94 and docs/COMPATIBILITY.md.

mod common;

use common::{CORE, SCHEMA_TOML, app_with_schema, get_text, indexed_app};

/// Not pinned by the PRD/issue #94; adjust here if the implementor picks a
/// different path.
const UI_ROUTE: &str = "/ui";

/// Manually scans for `needle` as a standalone number in `haystack` — not
/// preceded or followed by another ASCII digit — so e.g. asserting doc count
/// `5` doesn't spuriously match inside `15` or `2025`. Avoids pulling in a
/// regex dependency for one assertion (task spec: no new Cargo.toml
/// dependency from the test-writer stage).
fn contains_standalone_number(haystack: &str, n: usize) -> bool {
    let needle = n.to_string();
    let bytes = haystack.as_bytes();
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle.as_str()) {
        let abs = start + pos;
        let before_ok = abs == 0 || !bytes[abs - 1].is_ascii_digit();
        let after = abs + needle.len();
        let after_ok = after >= bytes.len() || !bytes[after].is_ascii_digit();
        if before_ok && after_ok {
            return true;
        }
        start = abs + 1;
    }
    false
}

/// Loose, format-agnostic check for "some plausible on-disk size
/// representation" per the task spec's own guidance ("use your judgement on
/// what's testable without over-specifying formatting, but don't skip it
/// entirely"). Looks for a size-label word plus a byte-unit word somewhere in
/// the page; does not pin an exact number, since a real on-disk footprint for
/// a handful of committed Tantivy segments is not a stable, predictable value
/// to assert byte-for-byte.
fn contains_plausible_size_indication(haystack: &str) -> bool {
    let lower = haystack.to_lowercase();
    let has_size_label = lower.contains("size");
    let has_byte_unit = ["byte", " b", "kb", "mb", "gb"]
        .iter()
        .any(|unit| lower.contains(unit));
    has_size_label && has_byte_unit
}

/// Extracts the exact byte figure the page renders as `(<N> bytes)` (see
/// `templates/core.html`: `{{ size_human }} ({{ size_bytes }} bytes)`).
///
/// Same hand-rolled-scan style as `contains_standalone_number` above, for the
/// same reason: no new dependency for one assertion. Finds the literal
/// ` bytes)` suffix, walks backwards over the ASCII digits in front of it, and
/// requires an opening `(` immediately before those digits, so a stray
/// "bytes)" elsewhere in the page can't be mistaken for the figure.
fn parse_rendered_size_bytes(haystack: &str) -> Option<u64> {
    const SUFFIX: &str = " bytes)";
    let bytes = haystack.as_bytes();
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(SUFFIX) {
        let end = start + pos;
        let mut digits_start = end;
        while digits_start > 0 && bytes[digits_start - 1].is_ascii_digit() {
            digits_start -= 1;
        }
        if digits_start < end
            && digits_start > 0
            && bytes[digits_start - 1] == b'('
            && let Ok(n) = haystack[digits_start..end].parse::<u64>()
        {
            return Some(n);
        }
        start = end + SUFFIX.len();
    }
    None
}

#[tokio::test]
async fn core_list_page_renders_name_doc_count_and_size_for_a_populated_core() {
    let (app, _dir) = indexed_app().await;

    let (status, headers, body) = get_text(&app, UI_ROUTE).await;

    assert_eq!(status, 200, "GET {UI_ROUTE} must return 200; body: {body}");

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
        "core list page must contain the core's name (`{CORE}`); body: {body}"
    );

    // `indexed_app()` indexes the 5-doc corpus (tests/common/mod.rs::corpus()).
    assert!(
        contains_standalone_number(&body, 5),
        "core list page must contain the real doc count (5); body: {body}"
    );

    assert!(
        contains_plausible_size_indication(&body),
        "core list page must contain some on-disk size indication (a size \
         label plus a byte unit); body: {body}"
    );

    // The label-plus-unit check above is satisfiable by a hardcoded zero, which
    // would silently disconnect the page from the real directory — exactly the
    // wire this tracer bullet exists to prove. Pin the actual number instead.
    //
    // Not flaky: a committed Tantivy index always leaves at least a meta.json
    // plus segment files in the core's data dir, so the real on-disk size of an
    // indexed core is never legitimately 0.
    let rendered_bytes = parse_rendered_size_bytes(&body).unwrap_or_else(|| {
        panic!("core page must render an exact `(<N> bytes)` size figure; body: {body}")
    });
    assert!(
        rendered_bytes > 0,
        "core page must render the *real* on-disk size of the core's data \
         directory, which is never 0 for an indexed core; got {rendered_bytes}; \
         body: {body}"
    );
}

#[tokio::test]
async fn core_list_page_renders_zero_doc_count_for_an_empty_core() {
    let dir = tempfile::TempDir::new().expect("create temp dir");
    let app = app_with_schema(dir.path(), SCHEMA_TOML).expect("wayfinder::app must build");

    let (status, _headers, body) = get_text(&app, UI_ROUTE).await;

    assert_eq!(
        status, 200,
        "an empty core must still render the page, not error; body: {body}"
    );
    assert!(
        body.contains(CORE),
        "core list page must contain the core's name (`{CORE}`) even with no docs indexed; body: {body}"
    );
    assert!(
        contains_standalone_number(&body, 0),
        "core list page must show a doc count of 0 for an empty core, not \
         omit the row or error; body: {body}"
    );
}

#[tokio::test]
async fn core_list_page_is_read_only_and_idempotent() {
    let (app, _dir) = indexed_app().await;

    let (status_first, _headers_first, body_first) = get_text(&app, UI_ROUTE).await;
    let (status_second, _headers_second, body_second) = get_text(&app, UI_ROUTE).await;

    assert_eq!(status_first, 200);
    assert_eq!(status_second, 200);
    assert_eq!(
        body_first, body_second,
        "hitting the core list page twice in a row must not change its \
         content — this is a read-only view with no write/commit side effect"
    );

    // Belt-and-braces: the underlying doc count (as reported by the real
    // `/select` endpoint, not the UI) must be unaffected by having rendered
    // the admin page in between.
    let (select_status, select_body) = common::get(&app, "select?q=*:*&rows=0&wt=json").await;
    assert_eq!(select_status, 200);
    assert_eq!(
        select_body["response"]["numFound"], 5,
        "rendering the admin UI page must not add, remove, or otherwise \
         mutate documents in the core"
    );
}
