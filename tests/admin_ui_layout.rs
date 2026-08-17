//! Shared admin-UI chrome: the section nav and skip link every `/ui*` page
//! inherits from `templates/_layout.html`.
//!
//! The per-page suites (`tests/admin_ui.rs`, `tests/admin_ui_schema_view.rs`,
//! ...) each assert their own page's content. Nothing covered the chrome the
//! layout adds around all of them, so a nav link pointing at a route that does
//! not exist, or every tab claiming to be the current one, would have been
//! invisible to the suite.
//!
//! Ordering disclosure: these tests were written after the layout they cover,
//! not red-first — the layout was a visual refactor of pages the suites above
//! already pinned. Each assertion here was mutation-checked instead (break the
//! link target / the `aria-current` condition, confirm this file goes red,
//! revert).

mod common;

use common::{SCHEMA_TOML, app_with_schema, get_text};

/// Every section in the nav, by route: each is both a page to render and a
/// link target that must appear in the nav on all the others. Kept in one
/// place so a new admin page is one line here.
const SECTIONS: &[&str] = &[
    "/ui",
    "/ui/schema",
    "/ui/synonyms",
    "/ui/query",
    "/ui/stats",
    "/ui/ping",
];

/// A core with the shared test schema; every `/ui*` route is reachable on it.
fn ui_app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().expect("create temp dir");
    let app = app_with_schema(dir.path(), SCHEMA_TOML).expect("wayfinder::app must build");
    (app, dir)
}

#[tokio::test]
async fn every_admin_page_links_to_every_other_admin_page() {
    let (app, _dir) = ui_app();

    for page in SECTIONS {
        let (status, _headers, body) = get_text(&app, page).await;
        assert_eq!(status, 200, "GET {page} must render; body: {body}");

        for target in SECTIONS {
            assert!(
                body.contains(&format!("href=\"{target}\"")),
                "the nav on {page} must link to {target} — a section that is \
                 not linked cannot be reached from the UI at all; body: {body}"
            );
        }
    }
}

/// Guards the accessible "you are here" signal: exactly one tab per page, and
/// it is that page's own tab. Marking every tab (or the wrong one) is worse
/// than marking none, because a screen reader announces it as fact.
#[tokio::test]
async fn each_admin_page_marks_exactly_its_own_nav_tab_as_current() {
    let (app, _dir) = ui_app();

    for page in SECTIONS {
        let (_status, _headers, body) = get_text(&app, page).await;

        // Counts the attribute as it appears on an element (`...="page">`),
        // not as it appears in the layout's own stylesheet, which selects on
        // `a[aria-current="page"]` and would otherwise be counted too.
        assert_eq!(
            body.matches("aria-current=\"page\">").count(),
            1,
            "{page} must mark exactly one nav tab as the current page; body: {body}"
        );
        assert!(
            body.contains(&format!("href=\"{page}\" aria-current=\"page\">")),
            "the tab marked current on {page} must be {page}'s own tab; body: {body}"
        );
    }
}

/// Keyboard users tab into the nav on every page; without a skip link they
/// walk all six tabs before reaching the content.
#[tokio::test]
async fn every_admin_page_offers_a_skip_link_to_its_main_content() {
    let (app, _dir) = ui_app();

    for page in SECTIONS {
        let (_status, _headers, body) = get_text(&app, page).await;

        assert!(
            body.contains("href=\"#main\""),
            "{page} must offer a skip link to its main content; body: {body}"
        );
        assert!(
            body.contains("id=\"main\""),
            "{page}'s skip link needs a target: the main content element must \
             carry `id=\"main\"`; body: {body}"
        );
    }
}
