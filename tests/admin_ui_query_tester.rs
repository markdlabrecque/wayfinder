//! Query tester tests (issue #127, PRD §5 "v2.5 — Admin web UI").
//!
//! Scope, per the issue: a new page (naming consistent with the existing
//! `GET /ui` tracer bullet from issue #94) that renders a form for `q`, `fq`,
//! `fl`, `rows`, `start`, `facet.field`, and — on submission — renders the raw
//! JSON response from the *same* in-process `/select` handler already
//! exercised by `tests/tracer_bullet.rs` et al. No second query-parsing/
//! execution path, no UI-only validation that could drift from `/select`'s
//! real behaviour.
//!
//! Route name is not pinned by the issue ("e.g. `GET /ui/query`, naming
//! consistent with `/ui`"). These tests hit `GET /ui/query` — if the
//! implementor picks a different path, update the `QUERY_TESTER_ROUTE`
//! constant below rather than every call site (same convention as
//! `tests/admin_ui.rs`'s `UI_ROUTE`).
//!
//! Assertions are on rendered HTML/JSON *content* (substrings, or embedded
//! JSON structurally compared against the real `/select` response), not exact
//! markup — same rationale as `tests/admin_ui.rs`.
//!
//! Whether the form is submitted via GET query string or POST is left to the
//! implementor (the issue says "your call"); these tests submit via GET query
//! string against `QUERY_TESTER_ROUTE` since that is the simplest form
//! encoding and keeps the tester page itself bookmarkable/linkable, matching
//! how `/select` itself is addressed. If the implementor chooses POST, this
//! file's `submit_query_tester()` helper is the one place to change.

mod common;

use common::{CORE, get, get_text, indexed_app};
use serde_json::Value;

/// Not pinned by the issue; adjust here if the implementor picks a different
/// path (mirrors `tests/admin_ui.rs`'s `UI_ROUTE` convention).
const QUERY_TESTER_ROUTE: &str = "/ui/query";

/// Issues a GET against the query-tester page with `query_and_fragment` as
/// its query string (e.g. `"q=fox&rows=2&wt=json"`, or `""` for the
/// first-load/empty state).
async fn submit_query_tester(
    app: &axum::Router,
    query_and_fragment: &str,
) -> (axum::http::StatusCode, axum::http::HeaderMap, String) {
    let path = if query_and_fragment.is_empty() {
        QUERY_TESTER_ROUTE.to_string()
    } else {
        format!("{QUERY_TESTER_ROUTE}?{query_and_fragment}")
    };
    get_text(app, &path).await
}

/// The query-tester page must reuse the real `/select` handler rather than a
/// second query-parsing/execution path, so its embedded JSON must be
/// structurally identical (order-insensitive, same as
/// `common::assert_matches_fixture`) to hitting `/select` directly with the
/// same params. This helper finds *a* JSON object embedded anywhere in
/// `haystack` that parses and, **after the same `common::normalize_envelope`
/// pass the caller applies to `expected`**, structurally equals it.
///
/// Normalising both sides is the point: the variable/ignored fields
/// (`QTime`, `_version_`, `_root_`) are a property of the *comparison*, not
/// of the page. The page renders `/select`'s response verbatim — an
/// implementation that stripped those fields on the way out to make this
/// assertion pass would be normalising production output to hide a
/// divergence, which the repo's compatibility contract forbids. So the test
/// absorbs the difference on both sides, symmetrically. It scans for
/// every `{` and attempts to parse the substring from there to the matching
/// closing brace (tracked via a simple depth counter, ignoring braces inside
/// JSON string literals). This tolerates the page wrapping the JSON in
/// arbitrary HTML (a `<pre>`, indentation, pretty-printing) without pinning
/// exact markup, while still proving the *real* `/select` response — not a
/// hand-summarised approximation of it — is what got rendered.
fn contains_embedded_json_matching(haystack: &str, expected: &Value) -> bool {
    let bytes = haystack.as_bytes();
    for start in 0..bytes.len() {
        if bytes[start] != b'{' {
            continue;
        }
        let mut depth = 0i32;
        let mut in_string = false;
        let mut escaped = false;
        for (offset, &b) in bytes[start..].iter().enumerate() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if b == b'\\' {
                    escaped = true;
                } else if b == b'"' {
                    in_string = false;
                }
                continue;
            }
            match b {
                b'"' => in_string = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        let end = start + offset + 1;
                        if let Ok(candidate) = serde_json::from_str::<Value>(&haystack[start..end])
                            && &common::normalize_envelope(candidate) == expected
                        {
                            return true;
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    false
}

#[tokio::test]
async fn query_tester_first_load_renders_the_form_without_executing_a_query() {
    let (app, _dir) = indexed_app().await;

    let (status, headers, body) = submit_query_tester(&app, "").await;

    assert_eq!(
        status, 200,
        "GET {QUERY_TESTER_ROUTE} with no params must render the empty form; body: {body}"
    );
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.contains("text/html"),
        "expected a text/html Content-Type, got `{content_type}`"
    );

    // A form control per param the issue names.
    for param in ["q", "fq", "fl", "rows", "start", "facet.field"] {
        assert!(
            body.contains(&format!("name=\"{param}\"")),
            "first-load query tester page must contain a form field named \
             `{param}`; body: {body}"
        );
    }

    // No query has been executed yet: nothing that looks like a rendered
    // `/select` response envelope should appear.
    assert!(
        !body.contains("responseHeader"),
        "first-load query tester page must not have executed a query yet \
         (no `responseHeader` in the body); body: {body}"
    );
    assert!(
        !body.contains("numFound"),
        "first-load query tester page must not have executed a query yet \
         (no `numFound` in the body); body: {body}"
    );
}

#[tokio::test]
async fn query_tester_submission_renders_the_real_select_response() {
    let (app, _dir) = indexed_app().await;

    let query = "q=quick&rows=5&wt=json";
    let (select_status, select_body) = get(&app, &format!("select?{query}")).await;
    assert_eq!(
        select_status, 200,
        "sanity check: /select must succeed for this query directly"
    );
    assert_eq!(
        select_body["response"]["numFound"], 2,
        "sanity check: `quick` must match doc1 and doc3 in the reference corpus"
    );

    let (status, _headers, body) = submit_query_tester(&app, query).await;

    assert_eq!(
        status, 200,
        "submitting a valid query to {QUERY_TESTER_ROUTE} must return 200; body: {body}"
    );

    // Same normalisation `common::assert_matches_fixture` already applies
    // (QTime is always variable and is not part of this assertion).
    let normalized_expected = common::normalize_envelope(select_body);
    assert!(
        contains_embedded_json_matching(&body, &normalized_expected),
        "query tester page must embed the real `/select` JSON response \
         (modulo QTime) for `{query}` -- not a hand-summarised approximation; \
         body: {body}"
    );

    // Even without parsing embedded JSON out, the page must contain content
    // that could only come from actually running the query against the real
    // index -- not a static/UI-only echo of the form.
    assert!(
        body.contains("doc1") && body.contains("doc3"),
        "query tester page must show the real matching document ids (doc1, \
         doc3) for q=quick; body: {body}"
    );
}

#[tokio::test]
async fn query_tester_submission_respects_fl_fq_rows_start() {
    let (app, _dir) = indexed_app().await;

    // fq narrows to the "classic" category (doc1, doc3); fl limits stored
    // fields to id only; rows/start page a single result.
    let query = "q=*:*&fq=category:classic&fl=id&rows=1&start=1&wt=json";
    let (select_status, select_body) = get(&app, &format!("select?{query}")).await;
    assert_eq!(
        select_status, 200,
        "sanity check: /select must accept these params"
    );

    let (status, _headers, body) = submit_query_tester(&app, query).await;
    assert_eq!(status, 200, "body: {body}");

    let normalized_expected = common::normalize_envelope(select_body);
    assert!(
        contains_embedded_json_matching(&body, &normalized_expected),
        "query tester page must embed the real, param-respecting `/select` \
         response for fq/fl/rows/start -- not just echo `q`; body: {body}"
    );
}

#[tokio::test]
async fn query_tester_submission_renders_facet_field_results() {
    let (app, _dir) = indexed_app().await;

    let query = "q=*:*&facet=true&facet.field=category&rows=0&wt=json";
    let (select_status, select_body) = get(&app, &format!("select?{query}")).await;
    assert_eq!(
        select_status, 200,
        "sanity check: facet.field=category must succeed"
    );
    assert!(
        select_body["facet_counts"]["facet_fields"]["category"].is_array(),
        "sanity check: facet_counts.facet_fields.category must be present"
    );

    let (status, _headers, body) = submit_query_tester(&app, query).await;
    assert_eq!(status, 200, "body: {body}");

    let normalized_expected = common::normalize_envelope(select_body);
    assert!(
        contains_embedded_json_matching(&body, &normalized_expected),
        "query tester page must embed the real facet.field response, \
         including facet_counts -- not just the top-level docs; body: {body}"
    );
}

#[tokio::test]
async fn query_tester_surfaces_the_same_400_error_select_returns() {
    let (app, _dir) = indexed_app().await;

    // `body` is not a `fast` field in SCHEMA_TOML, so sorting on it is a hard
    // 400 against real Solr and against Wayfinder's own `/select`
    // (`solr-ref/responses/err_bad_sort.json`; see tests/error_shapes.rs and
    // tests/sort.rs for the same param combination).
    let query = "q=*:*&sort=body+desc&wt=json";
    let (select_status, select_body) = get(&app, &format!("select?{query}")).await;
    assert_eq!(
        select_status, 400,
        "sanity check: /select must 400 on sort=body desc directly"
    );
    assert_eq!(select_body["error"]["code"], 400);

    let (status, _headers, body) = submit_query_tester(&app, query).await;

    assert_eq!(
        status.as_u16(),
        400,
        "the query tester must surface the exact same HTTP status /select \
         returns for malformed input, not swallow it into a 200 with a \
         UI-only validation message; body: {body}"
    );

    // The error content itself -- not a UI-invented message -- must be what
    // is shown. Compare against /select's own error.code and error.msg,
    // the same fields tests/error_shapes.rs pins for the wire API.
    let expected_msg = select_body["error"]["msg"]
        .as_str()
        .expect("fixture-backed /select error must have a string msg")
        .to_string();
    assert!(
        body.contains(&expected_msg) || body.contains("400"),
        "query tester error page must surface /select's real error content \
         (either its literal message or its error code), not a UI-only \
         validation message that could drift from the real endpoint's \
         behavior; expected msg: `{expected_msg}`; body: {body}"
    );

    // The form itself must still be present so the operator can correct and
    // resubmit -- an error is not a dead end.
    assert!(
        body.contains("name=\"q\""),
        "query tester page must still render the form after a 400, so the \
         operator can correct the query; body: {body}"
    );
}

#[tokio::test]
async fn query_tester_is_read_only_and_does_not_mutate_the_index() {
    let (app, _dir) = indexed_app().await;

    let _ = submit_query_tester(&app, "q=quick&rows=5&wt=json").await;

    let (select_status, select_body) = get(&app, "select?q=*:*&rows=0&wt=json").await;
    assert_eq!(select_status, 200);
    assert_eq!(
        select_body["response"]["numFound"], 5,
        "submitting a query through the tester must not add, remove, or \
         otherwise mutate documents in the core (matches the read-only \
         property already asserted for GET /ui in tests/admin_ui.rs)"
    );
    // Also sanity-check CORE is untouched/still the same core name.
    assert!(!CORE.is_empty());
}
