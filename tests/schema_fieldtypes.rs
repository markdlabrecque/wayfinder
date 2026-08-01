//! `GET /solr/{core}/schema/fieldtypes` (issue #156, resolving #142 as In).
//!
//! Ground truth for the response shape is `solr-ref/search-api/trace/00020.json`
//! (a real `solr:9` `GET /solr/{core}/schema/fieldtypes?wt=json&json.nl=flat`):
//! a bare `{"responseHeader":{...},"fieldTypes":[{"name":...,"class":...,...}]}`
//! envelope, one object per type, `name`/`class` always present plus a handful
//! of attribute keys that vary per entry. These tests do not pin the exact
//! attribute set (Wayfinder's analysis is Tantivy's, not Lucene's, per the
//! issue's honesty constraint) -- only the envelope shape, the `name`/`class`
//! presence, and which *names* must appear.
//!
//! # The 16-language ticket premise vs. the verified source
//!
//! Issue #156's "Scope" section claims Wayfinder supports "the 15 non-English
//! `text_<code>` presets ... (`ar`, `da`, `nl`, `fi`, `fr`, `de`, `el`, `hu`,
//! `it`, `no`, `pt`, `ro`, `ru`, `es`, `sv`)" -- i.e. 16 languages total with
//! English. `src/schema.rs`'s `LANGUAGES` table and `resolve_type` (verified by
//! the test-writer before writing this file) show this is wrong: `LANGUAGES`
//! also carries `ta` (Tamil) and `tr` (Turkish), and `resolve_type`'s generic
//! `text_<code>` branch accepts any non-`en` code in that table with no
//! additional filtering -- so Wayfinder genuinely has a stemmer for 18
//! languages (English + 17), not 16. Padding the honesty-guard test down to
//! the ticket's smaller list would itself violate the ticket's own honesty
//! constraint by *omitting* two languages Wayfinder really supports. These
//! tests assert the verified 18, not the ticket's 16, and this doc comment is
//! the flagged correction the working agreement calls for.
//! (`docs/PRD.md` compatibility contract: "Don't paper over a wrong ticket
//! premise ... flag the correction, not silently build to the wrong spec.")

mod common;

use common::{CORE, SCHEMA_TOML, app_with_schema, get, indexed_app, request_full};

/// Every non-English code in `src/schema.rs`'s `LANGUAGES` table, verified by
/// reading the source directly (see module doc). `en` is asserted separately
/// as `text_en`, which is a distinct built-in preset (`resolve_type`'s
/// explicit branch), not routed through the generic `text_<code>` branch.
const NON_ENGLISH_LANGUAGE_CODES: &[&str] = &[
    "ar", "da", "nl", "fi", "fr", "de", "el", "hu", "it", "no", "pt", "ro", "ru", "es", "sv", "ta",
    "tr",
];

/// Built-in type names `resolve_type` accepts that are not language presets:
/// `string`/`keyword` (both resolve to `Str`), `text_general`, `int`/`long`,
/// `float`/`double`, `date`. `text_en` is listed separately below since it is
/// the one `text_*` preset with its own dedicated tokenizer identity
/// (`wayfinder_text_en_v1`), not a `LANGUAGES`-table lookup.
const NON_LANGUAGE_BUILTIN_TYPES: &[&str] = &[
    "string",
    "keyword",
    "text_general",
    "int",
    "long",
    "float",
    "double",
    "date",
];

/// Names that must never appear: real languages `resolve_type` does not
/// accept. If any of these ever needs to become supported, this test (not a
/// silent widening of the honesty guard) is where that change gets recorded.
const UNSUPPORTED_LANGUAGE_NAMES: &[&str] = &["text_ja", "text_zh", "text_ko"];

fn field_type_names(body: &serde_json::Value) -> Vec<String> {
    body["fieldTypes"]
        .as_array()
        .expect("fieldTypes must be a JSON array")
        .iter()
        .map(|entry| {
            entry["name"]
                .as_str()
                .expect("every fieldTypes entry must have a string `name`")
                .to_string()
        })
        .collect()
}

// --- envelope shape ----------------------------------------------------

#[tokio::test]
async fn schema_fieldtypes_returns_ok_with_field_types_array() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, "schema/fieldtypes?wt=json&json.nl=flat").await;

    assert_eq!(status, axum::http::StatusCode::OK, "body: {body}");
    assert_eq!(body["responseHeader"]["status"], 0, "body: {body}");
    assert!(
        body["fieldTypes"].is_array(),
        "response must carry a `fieldTypes` array in the trace's shape, got: {body}"
    );
}

#[tokio::test]
async fn schema_fieldtypes_every_entry_has_name_and_class() {
    let (app, _dir) = indexed_app().await;
    let (_status, body) = get(&app, "schema/fieldtypes?wt=json&json.nl=flat").await;

    let entries = body["fieldTypes"]
        .as_array()
        .expect("fieldTypes must be a JSON array");
    assert!(!entries.is_empty(), "fieldTypes must not be empty");
    for entry in entries {
        assert!(
            entry["name"].is_string(),
            "every entry needs a string `name`, got: {entry}"
        );
        assert!(
            entry["class"].is_string(),
            "every entry needs a plausible `class` (trace shape), got: {entry}"
        );
    }
}

// --- built-in types resolve_type accepts --------------------------------

#[tokio::test]
async fn schema_fieldtypes_includes_every_non_language_builtin_type() {
    let (app, _dir) = indexed_app().await;
    let (_status, body) = get(&app, "schema/fieldtypes?wt=json&json.nl=flat").await;
    let names = field_type_names(&body);

    for builtin in NON_LANGUAGE_BUILTIN_TYPES {
        assert!(
            names.iter().any(|n| n == builtin),
            "fieldTypes must include built-in type `{builtin}` that resolve_type accepts; got names: {names:?}"
        );
    }
}

#[tokio::test]
async fn schema_fieldtypes_includes_text_en() {
    let (app, _dir) = indexed_app().await;
    let (_status, body) = get(&app, "schema/fieldtypes?wt=json&json.nl=flat").await;
    let names = field_type_names(&body);

    assert!(
        names.iter().any(|n| n == "text_en"),
        "fieldTypes must include `text_en`, got names: {names:?}"
    );
}

// --- the honesty guard ---------------------------------------------------
//
// This is the test that matters most (per the task spec): it must fail if
// the implementor pads the language list to look more Solr-like, and it must
// fail if the implementor under-reports a language Wayfinder genuinely
// stems. See the module doc for why the asserted set is 18 languages, not
// the ticket's stated 16.

#[tokio::test]
async fn schema_fieldtypes_honesty_guard_every_supported_language_present() {
    let (app, _dir) = indexed_app().await;
    let (_status, body) = get(&app, "schema/fieldtypes?wt=json&json.nl=flat").await;
    let names = field_type_names(&body);

    for code in NON_ENGLISH_LANGUAGE_CODES {
        let expected = format!("text_{code}");
        assert!(
            names.iter().any(|n| n == &expected),
            "fieldTypes must list `{expected}` -- Wayfinder's LANGUAGES table \
             and resolve_type genuinely accept it (verified against \
             src/schema.rs), got names: {names:?}"
        );
    }
}

#[tokio::test]
async fn schema_fieldtypes_honesty_guard_unsupported_languages_absent() {
    let (app, _dir) = indexed_app().await;
    let (_status, body) = get(&app, "schema/fieldtypes?wt=json&json.nl=flat").await;
    let names = field_type_names(&body);

    for unsupported in UNSUPPORTED_LANGUAGE_NAMES {
        assert!(
            !names.iter().any(|n| n == unsupported),
            "fieldTypes must NOT list `{unsupported}` -- resolve_type has no \
             stemmer for it, and listing it would misreport a language \
             upward, which the issue calls out as worse than today's \
             misreport-downward (nobody investigates green); got names: {names:?}"
        );
    }
}

/// Padding-resistant version of the two guards above: the *exact* set of
/// `text_<lang>` names (English included) must equal the verified 18, no
/// more, no fewer. A implementation that pads the list with a few
/// Solr-looking extras (`text_ja`, `text_zh`, ...) alongside the real ones
/// would pass the two tests above individually but fail this one.
#[tokio::test]
async fn schema_fieldtypes_honesty_guard_language_set_is_exact() {
    let (app, _dir) = indexed_app().await;
    let (_status, body) = get(&app, "schema/fieldtypes?wt=json&json.nl=flat").await;
    let names = field_type_names(&body);

    let mut actual_text_lang_names: Vec<String> = names
        .iter()
        .filter(|n| n.starts_with("text_"))
        .filter(|n| *n != "text_general")
        .cloned()
        .collect();
    actual_text_lang_names.sort();
    actual_text_lang_names.dedup();

    let mut expected: Vec<String> = NON_ENGLISH_LANGUAGE_CODES
        .iter()
        .map(|code| format!("text_{code}"))
        .collect();
    expected.push("text_en".to_string());
    expected.sort();

    assert_eq!(
        actual_text_lang_names, expected,
        "the exact set of stemmed text_<lang> types must be the 18 \
         languages Wayfinder's LANGUAGES table really has a stemmer for -- \
         no more (padding), no fewer (under-reporting)"
    );
}

// --- derived from the live schema, not a static blob --------------------

/// A custom `[[field_types]]` chain in the schema must show up in the
/// response. A hardcoded-blob implementation (one that emits a fixed JSON
/// document regardless of `AppState.index.wf_schema`) has no way to know
/// this name exists, so it fails this test.
#[tokio::test]
async fn schema_fieldtypes_reflects_a_live_custom_field_type() {
    let dir = tempfile::TempDir::new().expect("create temp dir");
    let toml = format!(
        r#"{SCHEMA_TOML}
[[fields]]
name = "shout"
type = "custom_shout_9f3a"
stored = true

[[field_types]]
name = "custom_shout_9f3a"
tokenizer = "simple"
[[field_types.filters]]
kind = "lowercase"
"#
    );
    let app = app_with_schema(dir.path(), &toml).expect("app with a custom field type must build");
    let (status, body) = get(&app, "schema/fieldtypes?wt=json&json.nl=flat").await;

    assert_eq!(status, axum::http::StatusCode::OK, "body: {body}");
    let names = field_type_names(&body);
    assert!(
        names.iter().any(|n| n == "custom_shout_9f3a"),
        "fieldTypes must include the schema's own custom [[field_types]] \
         entry `custom_shout_9f3a` -- this response must be derived from the \
         live WayfinderSchema, not a static blob; got names: {names:?}"
    );
}

/// The mirror negative case: a schema that never declared `custom_shout_9f3a`
/// must not report it. This is what actually rules out a static blob wide
/// enough to contain every name any test happens to ask for -- the previous
/// test alone cannot distinguish "derived from the live schema" from "a
/// blob padded with every name this suite uses".
#[tokio::test]
async fn schema_fieldtypes_does_not_leak_a_custom_type_from_another_schema() {
    let (app, _dir) = indexed_app().await;
    let (_status, body) = get(&app, "schema/fieldtypes?wt=json&json.nl=flat").await;
    let names = field_type_names(&body);

    assert!(
        !names.iter().any(|n| n == "custom_shout_9f3a"),
        "a schema that never declared `custom_shout_9f3a` must not report \
         it; got names: {names:?}"
    );
}

// --- params allowlist -----------------------------------------------------

#[tokio::test]
async fn schema_fieldtypes_accepts_wt_and_json_nl_under_strict_params() {
    let dir = tempfile::TempDir::new().expect("create temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, SCHEMA_TOML).expect("write schema.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let config_path = dir.path().join("wayfinder.toml");
    std::fs::write(&config_path, "strict_params = true\n").expect("write wayfinder.toml");
    let app =
        wayfinder::app_with_config(&schema_path, &data_dir, &config_path).expect("app must build");

    let (status, body) = request_full(
        &app,
        "GET",
        &format!("{CORE}/schema/fieldtypes?wt=json&json.nl=flat"),
        None,
    )
    .await;

    assert_eq!(
        status,
        axum::http::StatusCode::OK,
        "strict_params=true must not 400 on wt/json.nl, got: {body}"
    );
}

/// The negative case, same reasoning as `admin_info_system_strict_params_rejects_unknown_param`:
/// a mutation that deletes the `check_params` call in this route's handler
/// would leave the positive test above green (it only exercises allowed
/// params).
#[tokio::test]
async fn schema_fieldtypes_rejects_unknown_param_under_strict_params() {
    let dir = tempfile::TempDir::new().expect("create temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, SCHEMA_TOML).expect("write schema.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let config_path = dir.path().join("wayfinder.toml");
    std::fs::write(&config_path, "strict_params = true\n").expect("write wayfinder.toml");
    let app =
        wayfinder::app_with_config(&schema_path, &data_dir, &config_path).expect("app must build");

    let (status, body) = request_full(
        &app,
        "GET",
        &format!("{CORE}/schema/fieldtypes?wt=json&json.nl=flat&bogus=1"),
        None,
    )
    .await;

    assert_eq!(
        status,
        axum::http::StatusCode::BAD_REQUEST,
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
