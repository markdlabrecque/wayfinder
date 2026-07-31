//! Index stats admin-UI page tests (issue #129, PRD §5 "v2.5 — Admin web UI").
//!
//! Scope: a new, read-only page showing doc count, segment count, on-disk
//! size, and process uptime for the single core this process serves, plus
//! the mmap/no-JVM-heap honesty line for "resident memory" that mirrors the
//! precedent PRD §6 already set for the absent heap-tuning knob.
//!
//! Route name is not pinned by issue #129 ("e.g. `GET /ui/stats`"). These
//! tests hit `GET /ui/stats` — if the implementor picks a different path,
//! update `UI_STATS_ROUTE` below rather than every call site (same
//! convention `tests/admin_ui.rs` established for `UI_ROUTE`).
//!
//! No new stats-collection subsystem is assumed: doc count and on-disk size
//! are independently re-derived here (a fresh directory walk / a fresh
//! `tantivy::Index::open_in_dir` read of the same committed data directory)
//! rather than trusting the same code path the handler presumably calls —
//! same reasoning `walk_size_oracle()` documents in `src/core_index.rs`'s own
//! test module: an oracle built from the same code as the implementation
//! cannot catch the implementation being wrong.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{CORE, get_text, indexed_app};

/// Not pinned by issue #129; adjust here if the implementor picks a
/// different path.
const UI_STATS_ROUTE: &str = "/ui/stats";

/// Manually scans for `needle` as a standalone number in `haystack` — not
/// preceded or followed by another ASCII digit — so e.g. asserting doc count
/// `5` doesn't spuriously match inside `15` or `2025`. Copied from
/// `tests/admin_ui.rs`'s helper of the same name: each integration test file
/// is its own crate, so helpers cannot be shared except via `tests/common`,
/// and this one is specific enough to this file's assertions that it isn't
/// worth promoting there.
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

/// Finds the first case-insensitive occurrence of `label` in `haystack`, then
/// returns the first contiguous run of ASCII digits within the following 200
/// characters (or `None` if `label` isn't present, or no digit run follows it
/// in that window).
///
/// This is a loose, format-agnostic way to pull "whatever number sits next to
/// this row's label" out of rendered HTML without pinning the surrounding
/// markup — the same trade-off `tests/admin_ui.rs`'s
/// `parse_rendered_size_bytes` makes for the `(<N> bytes)` convention, but
/// generalised since this page's exact markup for segment count / uptime
/// isn't dictated by any existing template.
fn number_after_label(haystack: &str, label: &str) -> Option<u64> {
    let lower = haystack.to_lowercase();
    let label_lower = label.to_lowercase();
    let label_pos = lower.find(&label_lower)?;
    let window_end = (label_pos + label_lower.len() + 200).min(haystack.len());
    let window = &haystack[label_pos + label_lower.len()..window_end];
    let bytes = window.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            return window[start..i].parse::<u64>().ok();
        }
        i += 1;
    }
    None
}

/// Extracts the exact byte figure the page renders as `(<N> bytes)`, mirroring
/// the convention `templates/core.html` uses (`{{ size_human }} ({{
/// size_bytes }} bytes)`) — issue #129 reuses `disk_size_bytes()`, so the
/// simplest, most consistent choice is the same rendering helper `/ui`
/// already uses. If the implementor renders the stats page's size
/// differently, this helper (and the test that calls it) is the one to
/// revisit.
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

/// Independent oracle for on-disk size: a fresh, differently-implemented
/// (iterative, explicit stack) walk of `dir`, summing file lengths. Mirrors
/// `walk_size_oracle()` in `src/core_index.rs`'s own test module, which this
/// black-box test cannot call directly (`CoreIndex`/`dir_size_bytes` are
/// private to the crate) — so it is re-derived here against the *directory*
/// `tests/common::indexed_app()` is documented to create
/// (`<tempdir>/data`), not against any Wayfinder API.
fn walk_size_oracle(dir: &Path) -> u64 {
    let mut stack = vec![dir.to_path_buf()];
    let mut total = 0u64;
    while let Some(path) = stack.pop() {
        for entry in std::fs::read_dir(&path).expect("oracle reads a readable dir") {
            let entry = entry.expect("oracle reads a readable entry");
            let meta = entry.metadata().expect("oracle stats a readable entry");
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total += meta.len();
            }
        }
    }
    total
}

/// Independent oracle for segment count: opens the same committed data
/// directory as a fresh `tantivy::Index` (not through any Wayfinder type) and
/// counts its searchable segments directly off `Index::searchable_segment_metas`.
/// A real oracle, not a hardcoded guess — the actual segment count after one
/// `commit=true` update depends on tantivy's own merge/flush behaviour, which
/// this test does not assume a specific value for.
fn segment_count_oracle(data_dir: &Path) -> usize {
    let index = tantivy::Index::open_in_dir(data_dir)
        .expect("independent oracle must open the committed index directory");
    index
        .searchable_segment_metas()
        .expect("independent oracle must list searchable segment metas")
        .len()
}

#[tokio::test]
async fn stats_page_returns_200_html_for_a_populated_core() {
    let (app, _dir) = indexed_app().await;

    let (status, headers, body) = get_text(&app, UI_STATS_ROUTE).await;

    assert_eq!(
        status, 200,
        "GET {UI_STATS_ROUTE} must return 200; body: {body}"
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
        "stats page must contain the core's name (`{CORE}`); body: {body}"
    );
}

#[tokio::test]
async fn stats_page_shows_the_real_doc_count() {
    let (app, _dir) = indexed_app().await;

    let (_status, _headers, body) = get_text(&app, UI_STATS_ROUTE).await;

    // `indexed_app()` indexes the 5-doc corpus (tests/common/mod.rs::corpus()).
    assert!(
        contains_standalone_number(&body, 5),
        "stats page must contain the real doc count (5), reusing \
         `CoreIndex::doc_count()`; body: {body}"
    );
}

#[tokio::test]
async fn stats_page_shows_the_real_segment_count() {
    let (app, dir) = indexed_app().await;
    let data_dir = dir.path().join("data");

    let (_status, _headers, body) = get_text(&app, UI_STATS_ROUTE).await;

    let expected = segment_count_oracle(&data_dir);
    assert!(
        expected > 0,
        "a committed core must have at least one searchable segment"
    );

    let rendered = number_after_label(&body, "segment").unwrap_or_else(|| {
        panic!(
            "stats page must render a segment count next to a \"segment\" \
             label; body: {body}"
        )
    });
    assert_eq!(
        rendered, expected as u64,
        "rendered segment count must equal the real segment count from an \
         independent oracle (an open of the same data dir via \
         tantivy::Index::open_in_dir), not a hardcoded or unrelated number"
    );
}

#[tokio::test]
async fn stats_page_shows_the_real_on_disk_size() {
    let (app, dir) = indexed_app().await;
    let data_dir = dir.path().join("data");

    let (_status, _headers, body) = get_text(&app, UI_STATS_ROUTE).await;

    let expected = walk_size_oracle(&data_dir);
    assert!(
        expected > 0,
        "a committed core must have written something to {}",
        data_dir.display()
    );

    let rendered = parse_rendered_size_bytes(&body).unwrap_or_else(|| {
        panic!("stats page must render an exact `(<N> bytes)` size figure; body: {body}")
    });
    assert_eq!(
        rendered, expected,
        "rendered on-disk size must equal the real total from an \
         independent directory walk of the core's data dir, not a hardcoded \
         or stale number"
    );
}

#[tokio::test]
async fn stats_page_shows_an_uptime_that_does_not_decrease() {
    let (app, _dir) = indexed_app().await;

    let (_status1, _headers1, body1) = get_text(&app, UI_STATS_ROUTE).await;
    let first = number_after_label(&body1, "uptime").unwrap_or_else(|| {
        panic!(
            "stats page must render a numeric uptime figure next to an \
             \"uptime\" label; body: {body1}"
        )
    });

    // Long enough that a whole-seconds uptime counter must have ticked at
    // least once, short enough not to meaningfully slow the suite.
    tokio::time::sleep(Duration::from_millis(1200)).await;

    let (_status2, _headers2, body2) = get_text(&app, UI_STATS_ROUTE).await;
    let second = number_after_label(&body2, "uptime").unwrap_or_else(|| {
        panic!(
            "stats page must render a numeric uptime figure next to an \
             \"uptime\" label on a second request; body: {body2}"
        )
    });

    assert!(
        second >= first,
        "uptime must never go backwards between two requests over 1s apart \
         (first: {first}, second: {second})"
    );
    assert!(
        second > first,
        "uptime must reflect real elapsed process time: over 1.2s between \
         requests, a whole-seconds uptime counter must have advanced \
         (first: {first}, second: {second}); if the implementor's format is \
         coarser than seconds, this assertion (not the >= one above) is the \
         one to revisit"
    );
}

#[tokio::test]
async fn stats_page_states_mmap_honesty_for_resident_memory_with_no_fabricated_number() {
    let (app, _dir) = indexed_app().await;

    let (_status, _headers, body) = get_text(&app, UI_STATS_ROUTE).await;
    let lower = body.to_lowercase();

    assert!(
        lower.contains("mmap"),
        "stats page must explicitly say Wayfinder is mmap-based (PRD §6's \
         absent-heap-knob honesty precedent, restated for resident memory by \
         issue #129); body: {body}"
    );
    assert!(
        lower.contains("resident"),
        "stats page must have a resident-memory line at all, even if only \
         to explain there is no JVM-heap-shaped number for it; body: {body}"
    );

    // The honesty requirement is not just "mentions mmap somewhere on the
    // page" — it is that the *resident-memory line itself* carries no
    // fabricated number. Scan a window around each "resident" occurrence for
    // something that looks like a fabricated size figure (digits directly
    // followed by a byte unit) and fail if one appears there.
    let mut idx = 0;
    while let Some(pos) = lower[idx..].find("resident") {
        let abs = idx + pos;
        let window_start = abs.saturating_sub(40);
        let window_end = (abs + 120).min(lower.len());
        let window = &lower[window_start..window_end];
        let looks_fabricated = ["kb", "mb", "gb", " b)", " bytes"]
            .iter()
            .any(|unit| window.contains(unit));
        assert!(
            !looks_fabricated,
            "the resident-memory line must not carry a fabricated byte \
             figure — Wayfinder has no JVM-heap-shaped number to report, so \
             the honest line is prose, not a number; window: {window:?}"
        );
        idx = abs + "resident".len();
    }
}

#[tokio::test]
async fn stats_page_is_read_only_with_no_form_and_does_not_mutate_the_core() {
    let (app, _dir) = indexed_app().await;

    let (status_first, _headers_first, body_first) = get_text(&app, UI_STATS_ROUTE).await;
    let (status_second, _headers_second, body_second) = get_text(&app, UI_STATS_ROUTE).await;

    assert_eq!(status_first, 200);
    assert_eq!(status_second, 200);
    assert!(
        !body_first.to_lowercase().contains("<form"),
        "the stats page is read-only and must not contain a form or mutation \
         control; body: {body_first}"
    );

    // Uptime legitimately advances between the two renders, so this is not
    // a byte-for-byte equality check like `tests/admin_ui.rs`'s core-page
    // idempotency test — the invariant that matters here is that hitting the
    // page does not change the underlying document count.
    let _ = body_second;
    let (select_status, select_body) = common::get(&app, "select?q=*:*&rows=0&wt=json").await;
    assert_eq!(select_status, 200);
    assert_eq!(
        select_body["response"]["numFound"], 5,
        "rendering the stats page must not add, remove, or otherwise mutate \
         documents in the core"
    );
}
