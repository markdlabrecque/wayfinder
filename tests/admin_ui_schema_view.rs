//! Schema view tests (issue #128, PRD §5 "v2.5 — Admin web UI").
//!
//! Scope, per the issue: a new page (naming consistent with the existing
//! `GET /ui` tracer bullet from issue #94 and `GET /ui/query` from issue
//! #127) that renders the current core's persisted schema, read-only:
//!
//!   - field name, type, and `stored`/`fast`/`multi_valued`/`required` flags
//!     for each `[[fields]]` entry;
//!   - `[[dynamic_fields]]` patterns and their types;
//!   - `[[copy_fields]]` source/dest pairs.
//!
//! Data must come from the `WayfinderSchema` already loaded in-process by
//! `CoreIndex::open` (the same struct `check_compatible` uses at startup) —
//! no new parsing path, no new on-disk read. `schema_page_does_not_reread_the_schema_file_from_disk`
//! below is the regression test for that: it deletes the on-disk schema file
//! *after* the app is built and asserts the page still renders correctly, which
//! would only be possible if the handler serves the in-process struct rather
//! than re-parsing the TOML per request.
//!
//! Route name is not pinned by the issue ("e.g. `GET /ui/schema`"). These
//! tests hit `GET /ui/schema` — if the implementor picks a different path,
//! update the `SCHEMA_VIEW_ROUTE` constant below rather than every call site
//! (same convention as `tests/admin_ui.rs`'s `UI_ROUTE` and
//! `tests/admin_ui_query_tester.rs`'s `QUERY_TESTER_ROUTE`).
//!
//! Assertions are on rendered HTML *content* (substrings, plus lightweight
//! "row" extraction to check that per-field flags actually vary the
//! rendering, not on exact markup) — same rationale as the other admin-UI
//! test files: the issue does not pin markup, only content.
//!
//! Interpretation note: the issue does not pin how boolean flags are
//! rendered (`true`/`false`, `yes`/`no`, a checkmark, a CSS class, ...). Since
//! guessing a literal token risks testing this test-writer's own imagination
//! rather than the spec, the flag assertions below hold four *pairs* of
//! fields that are identical except for exactly one flag (isolating stored,
//! fast, required, multi_valued in turn) and assert the rendered "row" for
//! each pair member differs — i.e. that the page actually reflects each
//! field's real per-flag configuration, not a fixed/ignored rendering. This
//! is weaker than pinning exact wording but stronger than "the flag never
//! appears anywhere", and does not invent formatting requirements the issue
//! never stated.

mod common;

use common::get_text;

/// Not pinned by the issue; adjust here if the implementor picks a different
/// path (mirrors `tests/admin_ui.rs`'s `UI_ROUTE` convention).
const SCHEMA_VIEW_ROUTE: &str = "/ui/schema";

/// A schema exercising all three sections the issue names: `[[fields]]` with
/// varied flags (including isolated single-flag-difference pairs for
/// `stored`/`fast`/`required`/`multi_valued`), at least one `[[dynamic_fields]]`
/// entry, and at least one `[[copy_fields]]` entry.
///
/// `id` is the required, unanalyzed, non-multi-valued unique key (`schema::load`
/// enforces this); `body` is the declared `default_field`. `body_copy` exists
/// purely to be a valid `[[copy_fields]]` destination (`load` requires both
/// source and dest to be declared fields).
const SCHEMA_TOML_WITH_DYNAMIC_AND_COPY: &str = r#"
[core]
name = "content"
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

[[fields]]
name = "body_copy"
type = "text_en"
stored = true

[[fields]]
name = "plain_field"
type = "string"
stored = false
fast = false
required = false
multi_valued = false

[[fields]]
name = "stored_field"
type = "string"
stored = true
fast = false
required = false
multi_valued = false

[[fields]]
name = "fast_field"
type = "string"
stored = false
fast = true
required = false
multi_valued = false

[[fields]]
name = "required_field"
type = "string"
stored = false
fast = false
required = true
multi_valued = false

[[fields]]
name = "multi_field"
type = "string"
stored = false
fast = false
required = false
multi_valued = true

[[dynamic_fields]]
pattern = "*_s"
type = "string"
stored = true
fast = true

[[dynamic_fields]]
pattern = "*_i"
type = "long"
stored = true

[[copy_fields]]
source = "body"
dest = "body_copy"
"#;

/// Builds an app against `SCHEMA_TOML_WITH_DYNAMIC_AND_COPY` in a fresh temp
/// directory, indexing nothing (the schema view needs no documents). Returns
/// the router plus the schema file's path, so callers can delete/move it to
/// prove the handler does not re-read it per request.
fn app_with_schema_path() -> (axum::Router, std::path::PathBuf, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().expect("create temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, SCHEMA_TOML_WITH_DYNAMIC_AND_COPY).expect("write schema.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let app = wayfinder::app(&schema_path, &data_dir).expect("wayfinder::app must build");
    (app, schema_path, dir)
}

/// Finds every position in `haystack` where `token` appears as a standalone
/// identifier — not immediately preceded or followed by another identifier
/// character (`[A-Za-z0-9_]`). Guards against `body` spuriously matching
/// inside `body_copy`, or `*_s` matching inside `*_state`-shaped noise.
fn standalone_positions(haystack: &str, token: &str) -> Vec<usize> {
    let bytes = haystack.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    while let Some(pos) = haystack[start..].find(token) {
        let abs = start + pos;
        let before_ok = abs == 0 || !is_ident(bytes[abs - 1]);
        let after = abs + token.len();
        let after_ok = after >= bytes.len() || !is_ident(bytes[after]);
        if before_ok && after_ok {
            out.push(abs);
        }
        start = abs + 1;
    }
    out
}

/// True if `token` appears anywhere in `haystack` as a standalone identifier.
fn contains_standalone(haystack: &str, token: &str) -> bool {
    !standalone_positions(haystack, token).is_empty()
}

/// The text "row" for `name`'s first standalone occurrence in `haystack`:
/// from that occurrence up to (but not including) the next standalone
/// occurrence of any name in `boundary_names` (typically every other field
/// name declared in the test schema), or the end of the string if none
/// follows. Used to compare what the page renders for one field against
/// another without pinning exact table/markup structure.
fn field_row<'a>(haystack: &'a str, name: &str, boundary_names: &[&str]) -> &'a str {
    let starts = standalone_positions(haystack, name);
    let start = *starts
        .first()
        .unwrap_or_else(|| panic!("field `{name}` must appear in the rendered page"));

    let mut next_boundary = haystack.len();
    for boundary in boundary_names {
        if *boundary == name {
            continue;
        }
        for pos in standalone_positions(haystack, boundary) {
            if pos > start && pos < next_boundary {
                next_boundary = pos;
            }
        }
    }
    &haystack[start..next_boundary]
}

/// All field names declared in `SCHEMA_TOML_WITH_DYNAMIC_AND_COPY`'s
/// `[[fields]]`, in declaration order — used as row boundaries.
const ALL_FIELD_NAMES: &[&str] = &[
    "id",
    "body",
    "body_copy",
    "plain_field",
    "stored_field",
    "fast_field",
    "required_field",
    "multi_field",
];

/// Column indices of the per-field flag cells within a `field_row(...)`
/// extraction, per `templates/schema.html`'s `[[fields]]` row:
/// `<td>{name}</td><td>{type_}</td><td>{stored}</td><td>{fast}</td>
/// <td>{multi_valued}</td><td>{required}</td>`. Because `field_row` returns
/// the row starting *after* the name's opening `<td>` (at the name text
/// itself), splitting that string on `"<td>"` yields the name cell at index
/// 0, so the flag columns land one lower than their position in the `<tr>`.
const FIELD_STORED_COL: usize = 2;
const FIELD_FAST_COL: usize = 3;
const FIELD_MULTI_VALUED_COL: usize = 4;
const FIELD_REQUIRED_COL: usize = 5;

/// Extracts the text content of the cell at `index` (0-based) from a row
/// string produced by `field_row`, where the row has been split on the
/// literal `"<td>"` cell-opening tag. Panics with the row's content if the
/// requested cell is missing or unclosed, so a shape mismatch surfaces as a
/// clear test failure rather than a silent empty comparison.
fn nth_cell(row: &str, index: usize) -> &str {
    let part = row
        .split("<td>")
        .nth(index)
        .unwrap_or_else(|| panic!("row has no cell at index {index}: {row:?}"));
    let end = part
        .find("</td>")
        .unwrap_or_else(|| panic!("cell at index {index} does not close with `</td>`: {part:?}"));
    &part[..end]
}

/// The text of the `Copy fields` section: from the `Copy fields` heading up
/// to (but not including) the next `<h3>` heading, or the end of the body if
/// none follows. Used so copy-field assertions can only be satisfied by
/// content that is actually inside this section.
fn copy_fields_section(body: &str) -> &str {
    let start = body
        .find("Copy fields")
        .expect("schema page must render a `Copy fields` heading");
    let after = &body[start..];
    let end = after["Copy fields".len()..]
        .find("<h3>")
        .map(|p| p + "Copy fields".len())
        .unwrap_or(after.len());
    &after[..end]
}

/// True if, within `section`, a `<td>{first}</td>` cell is immediately
/// followed (modulo whitespace) by a `<td>{second}</td>` cell -- i.e. `first`
/// and `second` are rendered as adjacent table cells in the same row, per
/// `templates/schema.html`'s copy-fields row:
/// `<td>{{ copy.source }}</td><td>{{ copy.dest }}</td>`.
fn has_adjacent_td_pair(section: &str, first: &str, second: &str) -> bool {
    let first_marker = format!("<td>{first}</td>");
    let second_marker = format!("<td>{second}</td>");
    let mut search_from = 0;
    while let Some(pos) = section[search_from..].find(&first_marker) {
        let abs = search_from + pos;
        let after = &section[abs + first_marker.len()..];
        if after.trim_start().starts_with(&second_marker) {
            return true;
        }
        search_from = abs + 1;
    }
    false
}

/// Extracts, in order, the text content of every `<th scope="col">...</th>`
/// cell within `section`.
fn extract_th_labels(section: &str) -> Vec<String> {
    const OPEN: &str = "<th scope=\"col\">";
    let mut out = Vec::new();
    let mut rest = section;
    while let Some(start) = rest.find(OPEN) {
        let after_open = &rest[start + OPEN.len()..];
        let end = after_open
            .find("</th>")
            .expect("`<th scope=\"col\">` must be closed with `</th>`");
        out.push(after_open[..end].to_string());
        rest = &after_open[end + "</th>".len()..];
    }
    out
}

#[tokio::test]
async fn schema_page_returns_200_html_for_the_running_core() {
    let (app, _schema_path, _dir) = app_with_schema_path();

    let (status, headers, body) = get_text(&app, SCHEMA_VIEW_ROUTE).await;

    assert_eq!(
        status, 200,
        "GET {SCHEMA_VIEW_ROUTE} must return 200; body: {body}"
    );
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.contains("text/html"),
        "expected a text/html Content-Type, got `{content_type}`"
    );
}

#[tokio::test]
async fn schema_page_lists_every_declared_field_name_and_type() {
    let (app, _schema_path, _dir) = app_with_schema_path();

    let (_status, _headers, body) = get_text(&app, SCHEMA_VIEW_ROUTE).await;

    for name in ALL_FIELD_NAMES {
        assert!(
            contains_standalone(&body, name),
            "schema page must list the declared field `{name}`; body: {body}"
        );
    }

    // Types: `string` (id, body_copy's sibling fields are text_en; several
    // plain fields are `string`) and `text_en` (body, body_copy) must both
    // appear as the type of at least one field.
    assert!(
        contains_standalone(&body, "string"),
        "schema page must render the `string` field type; body: {body}"
    );
    assert!(
        contains_standalone(&body, "text_en"),
        "schema page must render the `text_en` field type; body: {body}"
    );
}

#[tokio::test]
async fn schema_page_reflects_the_stored_flag_per_field() {
    let (app, _schema_path, _dir) = app_with_schema_path();
    let (_status, _headers, body) = get_text(&app, SCHEMA_VIEW_ROUTE).await;

    let plain_row = field_row(&body, "plain_field", ALL_FIELD_NAMES);
    let stored_row = field_row(&body, "stored_field", ALL_FIELD_NAMES);
    let plain_cell = nth_cell(plain_row, FIELD_STORED_COL);
    let stored_cell = nth_cell(stored_row, FIELD_STORED_COL);
    assert_eq!(
        plain_cell, "no",
        "`plain_field` (stored=false) must render `no` in its `stored` \
         column; plain_field row: {plain_row:?}"
    );
    assert_eq!(
        stored_cell, "yes",
        "`stored_field` (stored=true) must render `yes` in its `stored` \
         column; stored_field row: {stored_row:?}"
    );
}

#[tokio::test]
async fn schema_page_reflects_the_fast_flag_per_field() {
    let (app, _schema_path, _dir) = app_with_schema_path();
    let (_status, _headers, body) = get_text(&app, SCHEMA_VIEW_ROUTE).await;

    let plain_row = field_row(&body, "plain_field", ALL_FIELD_NAMES);
    let fast_row = field_row(&body, "fast_field", ALL_FIELD_NAMES);
    let plain_cell = nth_cell(plain_row, FIELD_FAST_COL);
    let fast_cell = nth_cell(fast_row, FIELD_FAST_COL);
    assert_eq!(
        plain_cell, "no",
        "`plain_field` (fast=false) must render `no` in its `fast` column; \
         plain_field row: {plain_row:?}"
    );
    assert_eq!(
        fast_cell, "yes",
        "`fast_field` (fast=true) must render `yes` in its `fast` column; \
         fast_field row: {fast_row:?}"
    );
}

#[tokio::test]
async fn schema_page_reflects_the_required_flag_per_field() {
    let (app, _schema_path, _dir) = app_with_schema_path();
    let (_status, _headers, body) = get_text(&app, SCHEMA_VIEW_ROUTE).await;

    let plain_row = field_row(&body, "plain_field", ALL_FIELD_NAMES);
    let required_row = field_row(&body, "required_field", ALL_FIELD_NAMES);
    let plain_cell = nth_cell(plain_row, FIELD_REQUIRED_COL);
    let required_cell = nth_cell(required_row, FIELD_REQUIRED_COL);
    assert_eq!(
        plain_cell, "no",
        "`plain_field` (required=false) must render `no` in its `required` \
         column; plain_field row: {plain_row:?}"
    );
    assert_eq!(
        required_cell, "yes",
        "`required_field` (required=true) must render `yes` in its \
         `required` column; required_field row: {required_row:?}"
    );
}

#[tokio::test]
async fn schema_page_reflects_the_multi_valued_flag_per_field() {
    let (app, _schema_path, _dir) = app_with_schema_path();
    let (_status, _headers, body) = get_text(&app, SCHEMA_VIEW_ROUTE).await;

    let plain_row = field_row(&body, "plain_field", ALL_FIELD_NAMES);
    let multi_row = field_row(&body, "multi_field", ALL_FIELD_NAMES);
    let plain_cell = nth_cell(plain_row, FIELD_MULTI_VALUED_COL);
    let multi_cell = nth_cell(multi_row, FIELD_MULTI_VALUED_COL);
    assert_eq!(
        plain_cell, "no",
        "`plain_field` (multi_valued=false) must render `no` in its \
         `multi_valued` column; plain_field row: {plain_row:?}"
    );
    assert_eq!(
        multi_cell, "yes",
        "`multi_field` (multi_valued=true) must render `yes` in its \
         `multi_valued` column; multi_field row: {multi_row:?}"
    );
}

/// The fields table's header cells must be bound to the correct columns --
/// each flag test above only pins the *cell values* for two fields; it does
/// not prove `Stored` labels the stored column rather than, say, the
/// `Required` column. Swapping the `<th>Stored</th>` and `<th>Required</th>`
/// labels (leaving cell data unchanged) would pass every other test in this
/// file, so pin the header order directly.
#[tokio::test]
async fn schema_page_fields_table_header_labels_match_their_columns() {
    let (app, _schema_path, _dir) = app_with_schema_path();
    let (_status, _headers, body) = get_text(&app, SCHEMA_VIEW_ROUTE).await;

    let thead_start = body
        .find("<thead>")
        .expect("the fields table must render a `<thead>`");
    let thead_end = body[thead_start..]
        .find("</thead>")
        .expect("the fields table `<thead>` must close");
    let thead = &body[thead_start..thead_start + thead_end];

    let labels = extract_th_labels(thead);
    assert_eq!(
        labels,
        vec![
            "Field",
            "Type",
            "Stored",
            "Fast",
            "Multi-valued",
            "Required"
        ],
        "the fields table header cells must be bound to their actual \
         columns, in this order; got: {labels:?}"
    );
}

#[tokio::test]
async fn schema_page_renders_dynamic_field_patterns_and_types() {
    let (app, _schema_path, _dir) = app_with_schema_path();
    let (_status, _headers, body) = get_text(&app, SCHEMA_VIEW_ROUTE).await;

    assert!(
        body.contains("*_s"),
        "schema page must render the `[[dynamic_fields]]` pattern `*_s`; body: {body}"
    );
    assert!(
        body.contains("*_i"),
        "schema page must render the `[[dynamic_fields]]` pattern `*_i`; body: {body}"
    );

    // Each pattern's type must appear near its own pattern, not merely
    // somewhere on the page (which `string`/`text_en` already do, for the
    // static fields) -- `*_s` maps to `string`, `*_i` maps to `long`, a type
    // no static field in this schema uses, so its presence anywhere on the
    // page already proves it came from the dynamic-field rule.
    assert!(
        contains_standalone(&body, "long"),
        "schema page must render the `long` type for the `*_i` dynamic \
         field rule (no static field in this schema uses `long`, so its \
         presence proves the dynamic rule's type was rendered); body: {body}"
    );

    let pattern_positions = standalone_positions(&body, "*_s");
    assert!(
        !pattern_positions.is_empty(),
        "`*_s` must appear as its own token; body: {body}"
    );
    let long_positions = standalone_positions(&body, "long");
    let star_i_positions = standalone_positions(&body, "*_i");
    assert!(
        !star_i_positions.is_empty() && !long_positions.is_empty(),
        "both `*_i` and `long` must be present; body: {body}"
    );
    // `long` must appear within a reasonably small window of `*_i` (same
    // row/line), not merely somewhere in the page.
    let close = star_i_positions
        .iter()
        .any(|&i| long_positions.iter().any(|&l| l.abs_diff(i) < 200));
    assert!(
        close,
        "the `long` type must render near the `*_i` pattern it belongs to \
         (within the same row), not just anywhere on the page; body: {body}"
    );
}

#[tokio::test]
async fn schema_page_renders_copy_field_source_dest_pairs() {
    let (app, _schema_path, _dir) = app_with_schema_path();
    let (_status, _headers, body) = get_text(&app, SCHEMA_VIEW_ROUTE).await;

    // Scope the assertion to the `Copy fields` section itself -- `body` and
    // `body_copy` both also appear independently as `[[fields]]` rows
    // earlier on the page, so checking their proximity anywhere in `body`
    // (the full response) is satisfiable by those unrelated rows alone.
    // Deleting the entire copy-fields section must make this test fail.
    let section = copy_fields_section(&body);
    assert!(
        has_adjacent_td_pair(section, "body", "body_copy"),
        "the copy-fields section must render a row pairing source `body` \
         with dest `body_copy` as adjacent table cells (`<td>body</td>` \
         immediately followed by `<td>body_copy</td>`); copy-fields \
         section: {section:?}"
    );
}

#[tokio::test]
async fn schema_page_has_no_form_or_mutation_affordance() {
    let (app, _schema_path, _dir) = app_with_schema_path();
    let (status, _headers, body) = get_text(&app, SCHEMA_VIEW_ROUTE).await;
    assert_eq!(
        status, 200,
        "sanity check: GET {SCHEMA_VIEW_ROUTE} must succeed before checking \
         for the absence of a form; body: {body}"
    );

    // Read-only per the issue: no form, no mutation route. Unlike the query
    // tester (`tests/admin_ui_query_tester.rs`), this page takes no input and
    // submits nothing.
    assert!(
        !body.to_lowercase().contains("<form"),
        "schema view must not render a form -- it is read-only; body: {body}"
    );
    assert!(
        !body.contains("method=\"post\""),
        "schema view must not render a POST affordance -- it is read-only; body: {body}"
    );
}

#[tokio::test]
async fn schema_page_is_idempotent_and_does_not_mutate_the_index() {
    let (app, _schema_path, _dir) = app_with_schema_path();

    let (status_first, _headers_first, body_first) = get_text(&app, SCHEMA_VIEW_ROUTE).await;
    let (status_second, _headers_second, body_second) = get_text(&app, SCHEMA_VIEW_ROUTE).await;

    assert_eq!(status_first, 200);
    assert_eq!(status_second, 200);
    assert_eq!(
        body_first, body_second,
        "hitting the schema page twice in a row must render identically -- \
         it is a read-only view with no side effect"
    );
}

/// The schema view must be sourced from the `WayfinderSchema` already loaded
/// in-process by `CoreIndex::open` at startup -- the same struct
/// `schema::check_compatible` uses -- not a new parsing path that reads the
/// TOML file again per request. Proof: delete the on-disk schema file after
/// the app is built, then hit the page and confirm it still renders the full,
/// correct schema. A handler that re-parsed the file per request would fail
/// (or 500) once the file is gone.
#[tokio::test]
async fn schema_page_does_not_reread_the_schema_file_from_disk() {
    let (app, schema_path, _dir) = app_with_schema_path();

    std::fs::remove_file(&schema_path).expect("remove the on-disk schema file");
    assert!(
        !schema_path.exists(),
        "sanity check: the schema file must actually be gone"
    );

    let (status, _headers, body) = get_text(&app, SCHEMA_VIEW_ROUTE).await;

    assert_eq!(
        status, 200,
        "the schema page must still render after the on-disk schema file is \
         removed, proving it serves the in-process struct rather than \
         re-reading the file per request; body: {body}"
    );
    for name in ALL_FIELD_NAMES {
        assert!(
            contains_standalone(&body, name),
            "schema page must still list `{name}` after the on-disk schema \
             file is removed; body: {body}"
        );
    }
    assert!(
        body.contains("*_s") && body.contains("*_i"),
        "schema page must still list the dynamic field patterns after the \
         on-disk schema file is removed; body: {body}"
    );
}
