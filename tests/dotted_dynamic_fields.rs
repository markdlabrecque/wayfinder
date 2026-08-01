//! Issue #164 — a dynamic field name containing a `.` (e.g.
//! `tm_X3b_en_a.b`, matched by the shipped `tm_X3b_en_*` glob in
//! `presets/search-api.toml`) resolves as a valid field on both `/select` and
//! `/terms`, but never matches anything indexed under it: `select?q=
//! tm_X3b_en_a.b:gamma` returns `numFound: 0` and `terms?terms.fl=
//! tm_X3b_en_a.b` returns `{"terms":{"tm_X3b_en_a.b":[]}}` even right after
//! indexing a document with that exact key.
//!
//! ## Root cause (confirmed by reading source, not inferred from the ticket)
//!
//! The read and write paths for a dynamic field's JSON path disagree on how
//! many segments a dotted name is:
//!
//! - **Read**: `CoreIndex::field_target` passes the *whole* requested name
//!   (dots and all) as `path` into `FieldTarget::Dynamic` (`src/core_index.rs`
//!   around line 1425). `CoreIndex::term_for_target` (line ~1591) then builds
//!   the term with `Term::from_field_json_path(container, path, expand_dots)`.
//!   Tantivy's `Term::from_field_json_path` (`tantivy-0.26.1/src/schema/
//!   term.rs:78`) unconditionally calls `split_json_path`, which splits on
//!   every *unescaped* `.` regardless of `expand_dots` — so `"tm_X3b_en_a.b"`
//!   always becomes the two segments `["tm_X3b_en_a", "b"]` on the read side,
//!   `expand_dots` or not.
//! - **Write**: real indexing walks the JSON object key-by-key
//!   (`tantivy-0.26.1/src/core/json_utils.rs::index_json_object`) and calls
//!   `JsonPathWriter::push(json_path_segment)` once per *already-distinct*
//!   JSON key — for a flat catch-all container, the whole dynamic field name
//!   is one key, pushed in one call. `JsonPathWriter::push`
//!   (`tantivy-common-0.11.0/src/json_path_writer.rs:53`) only replaces `.`
//!   with the `0x01` segment separator *inside that single push* when
//!   `expand_dots` is enabled; if it is disabled, the literal `.` byte is
//!   kept, producing a *one*-segment path containing a literal dot.
//!
//! `src/schema.rs` (currently line 705, `JsonObjectOptions::default()...` for
//! the dynamic catch-all containers) never calls `.set_expand_dots_enabled()`,
//! so today the write path is one segment (`"tm_X3b_en_a.b"` literal) and the
//! read path is two (`"tm_X3b_en_a"` \x01 `"b"`) — different byte sequences,
//! so the term the query builds never matches the term indexing wrote.
//!
//! **Maintainer decision: the fix goes in the write path** — enable
//! `expand_dots` on the catch-all `JsonObjectOptions` in `src/schema.rs`, so
//! indexing splits a dotted key the same way the read path already does.
//! Tests below must NOT assume the read path stops splitting; that
//! alternative was considered and rejected. This also means **existing
//! indexes with dotted dynamic field data need a reindex** once the fix
//! lands — the on-disk encoding of those documents changes.
//!
//! ## `expand_dots`'s byte-for-byte behaviour on the edge cases that a naive
//! fix gets wrong (verified against `tantivy-common-0.11.0`'s
//! `JsonPathWriter::push`, not inferred from the name)
//!
//! `push` does an in-place `.` -> `\x01` byte replacement of whatever segment
//! it is given. A leading dot (`.leading`), a trailing dot (`trailing.`), or
//! consecutive dots (`a..b`) all produce the *same* separator-for-dot swap a
//! human reading "expand_dots" might not expect: an empty path segment
//! wherever a dot has no field-name character on one side.  E.g. pushing the
//! single string `"a..b"` with `expand_dots` enabled yields the exact byte
//! string `"a\x01\x01b"`. That is bit-for-bit identical to what the read
//! path already produces for the query text `a..b` today (three
//! `path.push(segment)` calls for `["a", "", "b"]`, each call inserting its
//! own separator) — `split_json_path("a..b")` already yields `["a", "",
//! "b"]`, and `JsonPathWriter` inserts a separator *between* pushes
//! regardless of what either segment's own text is. So the fix does not need
//! special-casing for these edge cases: once both sides expand dots the same
//! way, leading/trailing/consecutive dots round-trip through an *empty* named
//! segment, rather than erroring or silently dropping data. This file pins
//! that round-trip explicitly, since it is the surprising case a
//! naive/partial fix would get wrong (e.g. one that tries to reject or
//! collapse empty segments instead of just carrying them through).
//!
//! ## Premises checked before writing these tests (per the task brief)
//!
//! 1. Confirmed above by reading `tantivy-0.26.1/src/schema/term.rs`,
//!    `tantivy-common-0.11.0/src/json_path_writer.rs`, and
//!    `tantivy-0.26.1/src/core/json_utils.rs` directly — not inferred from
//!    naming. `src/schema.rs:705` is indeed the `JsonObjectOptions::default()`
//!    construction site for the catch-all containers, with no
//!    `.set_expand_dots_enabled()` call anywhere in the file (`grep -n
//!    expand_dots src/schema.rs` — 0 hits besides the field name itself).
//! 2. Reproduced empirically (not assumed) with a throwaway harness before
//!    writing any assertion here: `POST` a doc with `{"id": "d1",
//!    "tm_X3b_en_a.b": ["gamma"]}` against a schema with a `tm_X3b_en_*`
//!    dynamic rule indexes 200 OK; `GET select?q=tm_X3b_en_a.b:gamma`
//!    afterwards returns `{"response":{"numFound":0,...}}`; `GET
//!    terms?terms.fl=tm_X3b_en_a.b` returns
//!    `{"terms":{"tm_X3b_en_a.b":[]}}`. Both confirmed against a live
//!    in-process app, matching the ticket's repro exactly.
//! 3. A dotted name matching *no* `[[dynamic_fields]]` pattern (and no
//!    `[[fields]]` entry) is rejected before it ever reaches
//!    `term_for_target`/indexing at all: `CoreIndex::field_target` returns
//!    `None` (neither `wf_schema.field` nor `wf_schema.match_dynamic`
//!    matches), which surfaces as a plain "unknown field"/"Field does not
//!    exist" 400 on both the index path and the query path — confirmed
//!    empirically (`zz_not_dynamic.b` 400s both ways). This fix cannot touch
//!    that path; `dotted_name_matching_no_dynamic_rule_is_still_rejected`
//!    below pins it stays a 400.
//! 4. Grepped `tests/`, `solr-ref/`, and `presets/` for any dotted dynamic
//!    field name already in use as an indexed value (as opposed to
//!    incidental dots in file paths, URLs, or prose) — none found. No
//!    existing test or fixture depends on today's one-segment write
//!    behaviour for a dotted name, so there is nothing this fix is expected
//!    to break.
//! 5. Confirmed empirically that a *non-dotted* dynamic name is completely
//!    unaffected today (`tm_X3b_en_title` indexes and round-trips through
//!    both `/select` and `/terms` with `numFound: 1` / the indexed term).
//!    `non_dotted_dynamic_field_is_unaffected` below pins that this stays
//!    true after the fix — it is green today and must stay green, since it
//!    covers the vast majority of real dynamic fields.

// The `dead_code` allow for the shared helpers is an inner attribute inside
// `tests/common/mod.rs`; do not add a second one here (clippy rejects it
// under `-D warnings`).
mod common;

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;

use common::{app_with_schema, get, post_docs};

/// `id` (string, fast, stored, unique key) as the only static field, plus the
/// exact `tm_X3b_en_*` dynamic rule `presets/search-api.toml:113-117` ships
/// (multi-valued English text, stored) — the same pattern the ticket's own
/// repro (`tm_X3b_en_a.b`) matches. Every dynamic name in this file resolves
/// purely through `[[dynamic_fields]]`, never a declared `[[fields]]` entry.
const DOTTED_DYNAMIC_SCHEMA_TOML: &str = r#"
[core]
name = "content"
unique_key = "id"
default_field = "id"

[[fields]]
name = "id"
type = "string"
stored = true
required = true
fast = true

[[dynamic_fields]]
pattern = "tm_X3b_en_*"
type = "text_en"
multi_valued = true
stored = true
"#;

async fn dotted_app(corpus: &Value) -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = app_with_schema(dir.path(), DOTTED_DYNAMIC_SCHEMA_TOML).expect("app must build");
    let (status, body) = post_docs(&app, corpus).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing a document with a dotted dynamic field name must succeed \
         (indexing itself is not in question, only whether the indexed value \
         is later findable), got {body}"
    );
    (app, dir)
}

/// The ticket's exact repro: indexing `tm_X3b_en_a.b` (a dotted name matching
/// the `tm_X3b_en_*` glob) must make it findable by `/select` afterwards.
/// Today this returns `numFound: 0` because the write path (one JSON-path
/// segment containing a literal dot) and the read path (two segments, split
/// on the dot) encode different terms for the same name.
#[tokio::test]
async fn dotted_dynamic_field_round_trips_through_select() {
    let corpus = json!([
        {"id": "d1", "tm_X3b_en_a.b": ["gamma"]},
    ]);
    let (app, _dir) = dotted_app(&corpus).await;

    let (status, body) = get(&app, "select?q=tm_X3b_en_a.b:gamma").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        body.pointer("/response/numFound").and_then(Value::as_u64),
        Some(1),
        "a document indexed with the dotted dynamic field name \
         `tm_X3b_en_a.b` must be found by a query naming that exact field, \
         got {body}"
    );
    let ids: Vec<&str> = body
        .pointer("/response/docs")
        .and_then(Value::as_array)
        .map(|docs| {
            docs.iter()
                .filter_map(|d| d.pointer("/id").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(ids, vec!["d1"], "got {body}");
}

/// The same defect via `/terms`: the analyzed term dictionary for the dotted
/// name must contain the term the document was indexed with, not an empty
/// list. `terms.fl=tm_X3b_en_a.b` resolving the field at all (200, not 400)
/// is not in question here — only whether the term dictionary it reads is
/// the one the document actually wrote into.
#[tokio::test]
async fn dotted_dynamic_field_round_trips_through_terms() {
    let corpus = json!([
        {"id": "d1", "tm_X3b_en_a.b": ["gamma"]},
    ]);
    let (app, _dir) = dotted_app(&corpus).await;

    let (status, body) = get(&app, "terms?terms=true&terms.fl=tm_X3b_en_a.b").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        body.pointer("/terms/tm_X3b_en_a.b"),
        Some(&json!(["gamma", 1])),
        "the term dictionary for the dotted dynamic field name must contain \
         the analyzed term(s) the document was indexed with, got {body}"
    );
}

/// The regression guard: a non-dotted dynamic name (the vast majority of
/// real dynamic fields, and the shape every other dynamic-field test in this
/// suite already exercises) must be completely unaffected by whatever the
/// fix does to dotted names. This is green today and must stay green.
#[tokio::test]
async fn non_dotted_dynamic_field_is_unaffected() {
    let corpus = json!([
        {"id": "d1", "tm_X3b_en_title": ["gamma"]},
    ]);
    let (app, _dir) = dotted_app(&corpus).await;

    let (status, body) = get(&app, "select?q=tm_X3b_en_title:gamma").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        body.pointer("/response/numFound").and_then(Value::as_u64),
        Some(1),
        "a non-dotted dynamic field name must round-trip through /select \
         exactly as it does today, got {body}"
    );

    let (status, body) = get(&app, "terms?terms=true&terms.fl=tm_X3b_en_title").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        body.pointer("/terms/tm_X3b_en_title"),
        Some(&json!(["gamma", 1])),
        "a non-dotted dynamic field name must round-trip through /terms \
         exactly as it does today, got {body}"
    );
}

/// A dotted name matching no `[[dynamic_fields]]` pattern (and no
/// `[[fields]]` entry) is rejected long before `term_for_target` — this
/// bounds the fix: it must not turn an actually-undefined field into a
/// silently-accepted one just because it contains a dot.
#[tokio::test]
async fn dotted_name_matching_no_dynamic_rule_is_still_rejected() {
    let dir = TempDir::new().expect("temp dir");
    let app = app_with_schema(dir.path(), DOTTED_DYNAMIC_SCHEMA_TOML).expect("app must build");

    let corpus = json!([
        {"id": "d1", "zz_not_dynamic.b": ["gamma"]},
    ]);
    let (status, body) = post_docs(&app, &corpus).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a dotted name matching no dynamic rule and no declared field must \
         still be rejected as unknown at index time, got {body}"
    );

    // Index a document under the real dynamic field so the /select request
    // below is a well-formed query against an existing core, isolating "is
    // the *field name* rejected" from any other reason a query could 400.
    let corpus = json!([
        {"id": "d2", "tm_X3b_en_title": ["gamma"]},
    ]);
    let (status, body) = post_docs(&app, &corpus).await;
    assert_eq!(status, StatusCode::OK, "got {body}");

    let (status, body) = get(&app, "select?q=zz_not_dynamic.b:gamma").await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a dotted name matching no dynamic rule and no declared field must \
         still be rejected as unknown at query time too, got {body}"
    );
    assert_eq!(
        body.pointer("/error/code").and_then(Value::as_u64),
        Some(400),
        "got {body}"
    );
}

/// The edge cases point 1 in the task brief named: a leading dot, a trailing
/// dot, and consecutive dots. Verified against `tantivy-common-0.11.0`'s
/// `JsonPathWriter::push` (see module doc comment): once the write path
/// expands dots the same way the read path already splits on them, all three
/// round-trip through an *empty* path segment rather than erroring — the
/// same behaviour a query against the literal text produces today via
/// `split_json_path`. This pins the real (if slightly surprising) behaviour,
/// not an idealised one.
#[tokio::test]
async fn dotted_dynamic_field_edge_cases_round_trip() {
    for name in [
        "tm_X3b_en_.leading",
        "tm_X3b_en_trailing.",
        "tm_X3b_en_a..b",
    ] {
        let corpus = json!([
            {"id": "d1", name: ["gamma"]},
        ]);
        let (app, _dir) = dotted_app(&corpus).await;

        let (status, body) = get(&app, &format!("select?q={name}:gamma")).await;
        assert_eq!(status, StatusCode::OK, "field {name:?}, got {body}");
        assert_eq!(
            body.pointer("/response/numFound").and_then(Value::as_u64),
            Some(1),
            "field {name:?} (leading/trailing/consecutive dot) must round-trip \
             through /select exactly like any other dotted dynamic name, got {body}"
        );
    }
}
