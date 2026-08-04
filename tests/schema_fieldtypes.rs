//! `GET /wayfinder/{core}/schema/fieldtypes` (issue #156, resolving #142 as In).
//!
//! Ground truth for the response shape is `solr-ref/search-api/trace/00020.json`
//! (a real `solr:9` `GET /solr/{core}/schema/fieldtypes?wt=json&json.nl=flat`):
//! a bare `{"responseHeader":{...},"fieldTypes":[{"name":...,"class":...,...}]}`
//! envelope, one object per type, `name`/`class` always present plus a handful
//! of attribute keys that vary per entry. These tests do not pin Solr's exact
//! per-entry attribute *set* (Wayfinder's analysis is Tantivy's, not Lucene's,
//! per the issue's honesty constraint, and Solr's sparseness is a
//! managed-schema serialisation artifact) -- they pin the envelope shape, the
//! `name` -> `class` mapping for every built-in, the presence of the four
//! attribute keys Wayfinder does emit, and the exact, complete set of *names*.
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
/// `float`/`double`, `date`, and `location` (#331). `text_en` is listed
/// separately below since it is the one `text_*` preset with its own
/// dedicated tokenizer identity (`wayfinder_text_en_v2`), not a `LANGUAGES`-
/// table lookup.
const NON_LANGUAGE_BUILTIN_TYPES: &[&str] = &[
    "string",
    "keyword",
    "text_general",
    "int",
    "long",
    "float",
    "double",
    "date",
    "location",
];

/// Names that must never appear: real languages `resolve_type` does not
/// accept. If any of these ever needs to become supported, this test (not a
/// silent widening of the honesty guard) is where that change gets recorded.
const UNSUPPORTED_LANGUAGE_NAMES: &[&str] = &["text_ja", "text_zh", "text_ko"];

/// `name` -> `class`, every pair derived from
/// `solr-ref/search-api/trace/00020.json` (`.response.body.fieldTypes`), never
/// from what Wayfinder happens to emit. Where Wayfinder's built-in name
/// differs from Solr's name for the same type, the trace entry it is taken
/// from is named in the comment: Wayfinder calls the point-numeric types
/// `int`/`long`/`float`/`double`/`date`, Solr 9 calls them
/// `pint`/`plong`/`pfloat`/`pdouble`/`pdate`, but the *class* is the thing a
/// client would read and it must be Solr's.
const EXPECTED_CLASSES: &[(&str, &str)] = &[
    // trace: `string`
    ("string", "wayfinder.StrField"),
    // Wayfinder alias for `string`, so the same class the trace pins for it.
    ("keyword", "wayfinder.StrField"),
    // trace: `pint`
    ("int", "wayfinder.IntPointField"),
    // trace: `plong`
    ("long", "wayfinder.LongPointField"),
    // trace: `pfloat`
    ("float", "wayfinder.FloatPointField"),
    // trace: `pdouble`
    ("double", "wayfinder.DoublePointField"),
    // trace: `pdate`
    ("date", "wayfinder.DatePointField"),
    // Solr's `location` is `LatLonPointSpatialField`; the class is Solr's
    // vocabulary even though Wayfinder stores a point as two f64 columns (#331).
    ("location", "wayfinder.LatLonPointSpatialField"),
    // trace: `text_en`
    ("text_en", "wayfinder.TextField"),
    // trace: every `text_*` entry (`text_und`, `text_ws`, ...) is a TextField.
    ("text_general", "wayfinder.TextField"),
];

/// The keys `field_type_entry` must put on every entry beyond `name`/`class`.
/// Real Solr emits these sparsely (`indexed` on 4 of the trace's 41 entries,
/// `docValues` on 12); Wayfinder emits all four on every entry as a recorded
/// deliberate addition (see `field_type_entry`'s doc comment and PRD 5), so
/// this asserts presence, which is what stops the four from being silently
/// dropped.
const EXPECTED_ATTRIBUTE_KEYS: &[&str] = &["indexed", "stored", "multiValued", "docValues"];

/// The exact, complete set of `fieldTypes` names Wayfinder must report for a
/// schema declaring the custom chains in `extra_custom`: the ten non-language
/// built-ins (`NON_LANGUAGE_BUILTIN_TYPES` plus `text_en`), one
/// `text_<code>` per non-English `LANGUAGES` entry, and each custom chain --
/// nothing else. Sorted, so `assert_eq!` against a sorted actual list is a
/// set comparison.
fn expected_exact_names(extra_custom: &[&str]) -> Vec<String> {
    let mut expected: Vec<String> = NON_LANGUAGE_BUILTIN_TYPES
        .iter()
        .chain(std::iter::once(&"text_en"))
        .chain(extra_custom.iter())
        .map(|name| (*name).to_string())
        .collect();
    expected.extend(
        NON_ENGLISH_LANGUAGE_CODES
            .iter()
            .map(|code| format!("text_{code}")),
    );
    expected.sort();
    expected
}

/// Sorted names, with duplicates left in place on purpose: a duplicate `name`
/// is itself a bug (it is exactly what corrupts `isPartOfSchema`'s
/// `in_array` name list), so it must be able to fail an exact-set assertion
/// rather than being deduplicated away before comparison.
fn sorted_field_type_names(body: &serde_json::Value) -> Vec<String> {
    let mut names = field_type_names(body);
    names.sort();
    names
}

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
        for key in EXPECTED_ATTRIBUTE_KEYS {
            assert!(
                entry.get(*key).is_some(),
                "every entry must carry the `{key}` attribute key, got: {entry}"
            );
        }
    }
}

/// `is_string()` on `class` is not enough: `int -> wayfinder.StrField`, or a
/// fabricated `wayfinder.Foo`, would pass it. This pins the mapping to the
/// classes `solr-ref/search-api/trace/00020.json` actually shows (see
/// `EXPECTED_CLASSES`), which is the compatibility contract's rule that
/// expected values come from the fixtures, not from the implementation.
#[tokio::test]
async fn schema_fieldtypes_class_matches_the_trace_for_each_builtin() {
    let (app, _dir) = indexed_app().await;
    let (_status, body) = get(&app, "schema/fieldtypes?wt=json&json.nl=flat").await;
    let entries = body["fieldTypes"]
        .as_array()
        .expect("fieldTypes must be a JSON array");

    for (name, want_class) in EXPECTED_CLASSES {
        let entry = entries
            .iter()
            .find(|e| e["name"].as_str() == Some(name))
            .unwrap_or_else(|| panic!("fieldTypes must include `{name}`, got: {body}"));
        assert_eq!(
            entry["class"].as_str(),
            Some(*want_class),
            "`{name}` must report the class real Solr reports for this type \
             (trace 00020.json), got: {entry}"
        );
    }

    // Every stemmed language preset is analyzed text, which the trace shows as
    // `wayfinder.TextField` for all of its own `text_*` entries.
    for code in NON_ENGLISH_LANGUAGE_CODES {
        let name = format!("text_{code}");
        let entry = entries
            .iter()
            .find(|e| e["name"].as_str() == Some(name.as_str()))
            .unwrap_or_else(|| panic!("fieldTypes must include `{name}`, got: {body}"));
        assert_eq!(
            entry["class"].as_str(),
            Some("wayfinder.TextField"),
            "`{name}` is analyzed text, which the trace reports as \
             wayfinder.TextField, got: {entry}"
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

/// Padding-resistant version of the two guards above, and the strongest test
/// in this file: the *exact, complete* `fieldTypes` name list must equal the
/// verified set -- the ten non-language built-ins, `text_en`, and the 17
/// non-English stemmed presets -- with no duplicates and nothing else at all.
///
/// Scoping this to the `text_*` subset (an earlier version of this test did)
/// left a hole: padding the list with non-language Solr-looking types
/// (`boolean`, `pdate`, `location`, `binary` -- all real names from the trace;
/// `location` joined the built-ins with #331, the others remain unsupported)
/// went undetected, which is precisely the "make it look more Solr-like"
/// failure mode this endpoint exists to prevent. The expected set is
/// hardcoded test-side, so widening `schema.rs`'s tables cannot widen the
/// assertion with it.
#[tokio::test]
async fn schema_fieldtypes_honesty_guard_full_name_set_is_exact() {
    let (app, _dir) = indexed_app().await;
    let (_status, body) = get(&app, "schema/fieldtypes?wt=json&json.nl=flat").await;
    let actual = sorted_field_type_names(&body);

    assert_eq!(
        actual,
        expected_exact_names(&[]),
        "the exact set of reported fieldTypes must be the built-ins \
         resolve_type really accepts plus the 18 languages Wayfinder's \
         LANGUAGES table really has a stemmer for -- no more (padding, of \
         either a language or a Solr-looking non-language type), no fewer \
         (under-reporting), and no duplicates"
    );
}

/// The same exactness assertion with a custom `[[field_types]]` chain
/// declared: the response is the built-ins plus that chain, once.
#[tokio::test]
async fn schema_fieldtypes_full_name_set_is_exact_with_a_custom_chain() {
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
    let (_status, body) = get(&app, "schema/fieldtypes?wt=json&json.nl=flat").await;

    assert_eq!(
        sorted_field_type_names(&body),
        expected_exact_names(&["custom_shout_9f3a"]),
        "a declared custom chain adds exactly one name to the built-in set"
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

/// Was `schema_fieldtypes_custom_chain_shadowing_a_builtin_is_reported_once`,
/// which pinned the pre-#170 behaviour at the endpoint level: a custom
/// `[[field_types]]` chain named after a built-in shadowed it (because
/// `schema::resolve_type` checks the schema's own chains before its built-in
/// match arms), the core started, and this endpoint had to de-duplicate the
/// name so `isPartOfSchema`'s `in_array` list stayed clean.
///
/// Issue #170 removes that situation at the source: shadowing a built-in is a
/// schema-load error, so a core carrying such a schema never starts and there
/// is no shadowed name for this endpoint to report at all. The test is kept,
/// inverted, rather than deleted, so the reason the de-duplication case
/// disappeared stays recorded next to the endpoint it used to constrain.
#[tokio::test]
async fn schema_fieldtypes_custom_chain_shadowing_a_builtin_is_rejected_at_load_time() {
    let dir = tempfile::TempDir::new().expect("create temp dir");
    let toml = format!(
        r#"{SCHEMA_TOML}
[[fields]]
name = "shout"
type = "text_de"
stored = true

[[field_types]]
name = "text_de"
tokenizer = "simple"
[[field_types.filters]]
kind = "lowercase"
"#
    );
    let err = match app_with_schema(dir.path(), &toml) {
        Ok(_) => panic!("a schema shadowing the built-in `text_de` preset must not build an app"),
        Err(err) => format!("{err:#}"),
    };
    assert!(
        err.contains("text_de") && err.contains("reserved"),
        "startup must fail with the reserved-field-type error naming `text_de`, got: {err}"
    );

    // A core whose custom chains shadow nothing still reports the exact,
    // complete built-in name set -- the de-duplication this test used to
    // exercise is unreachable, not merely untested.
    let clean_dir = tempfile::TempDir::new().expect("create temp dir");
    let clean_toml = format!(
        r#"{SCHEMA_TOML}
[[fields]]
name = "shout"
type = "text_de_custom"
stored = true

[[field_types]]
name = "text_de_custom"
tokenizer = "simple"
[[field_types.filters]]
kind = "lowercase"
"#
    );
    let app = app_with_schema(clean_dir.path(), &clean_toml)
        .expect("a custom chain that shadows no built-in must still build");
    let (status, body) = get(&app, "schema/fieldtypes?wt=json&json.nl=flat").await;
    assert_eq!(status, axum::http::StatusCode::OK, "body: {body}");
    assert_eq!(
        sorted_field_type_names(&body),
        expected_exact_names(&["text_de_custom"]),
        "a non-shadowing custom chain is reported alongside every built-in"
    );
    // The only assertion in the suite that a *custom* `[[field_types]]` chain
    // reports Solr's `TextField` class: `EXPECTED_CLASSES` covers built-ins
    // only, `schema_fieldtypes_reflects_a_live_custom_field_type` asserts the
    // name alone, and `..._every_entry_has_name_and_class` only checks
    // `class.is_string()`. Every custom chain resolves to `ResolvedType::Text`
    // (`schema::resolve_type` returns that for any matching `[[field_types]]`
    // entry), so `wayfinder.TextField` is the class a client must read.
    let custom = body["fieldTypes"]
        .as_array()
        .expect("fieldTypes must be a JSON array")
        .iter()
        .find(|entry| entry["name"] == "text_de_custom")
        .unwrap_or_else(|| panic!("the custom chain must be reported, got: {body}"));
    assert_eq!(
        custom["class"].as_str(),
        Some("wayfinder.TextField"),
        "a custom [[field_types]] chain is analyzed text, so it must report Solr's TextField \
         class, got: {custom}"
    );
}

// --- unknown core ---------------------------------------------------------

/// Same endpoint-agnostic JSON-404 divergence every other route carries
/// (finding 49, `tests/update_pipeline.rs`'s `unknown_core_*` block): without
/// this, `GET /wayfinder/nosuchcore/schema/fieldtypes` would serve the real core's
/// field types under any core name at all.
#[tokio::test]
async fn schema_fieldtypes_unknown_core_is_a_json_404() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = request_full(
        &app,
        "GET",
        "nosuchcore/schema/fieldtypes?wt=json&json.nl=flat",
        None,
    )
    .await;

    assert_eq!(
        status,
        axum::http::StatusCode::NOT_FOUND,
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
        body.get("fieldTypes").is_none(),
        "a 404 must not leak the real core's field types, got: {body}"
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
