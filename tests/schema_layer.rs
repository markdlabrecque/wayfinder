//! Schema layer completion (issue #10, PRD §3): dynamic fields, copy fields,
//! analyzer presets + custom chains, numeric/date field types, and the startup
//! schema-compatibility check.
//!
//! Ground truth for doc-level unknown-field handling is
//! `solr-ref/responses/update_unknown_field_strict.json` — captured from a Solr
//! started with `-Dupdate.autoCreateFields=false`. The `_default` configset that
//! produced the other fixtures is *schemaless* and silently adds unknown fields
//! to the schema (`update_unknown_field_schemaless.json`, HTTP 200); that is
//! configset behaviour and explicitly out of scope for Wayfinder (PRD §3 — no
//! runtime schema mutation), so the strict capture is the one we match.

mod common;

use axum::http::StatusCode;
use serde_json::{Value, json};
use tantivy::Index;
use tempfile::TempDir;
use wayfinder::schema;

/// A schema exercising every issue-#10 feature at once.
const FULL_SCHEMA_TOML: &str = r#"
[core]
name = "content"
unique_key = "id"
default_field = "body"

[[fields]]
name = "id"
type = "string"
stored = true
required = true

[[fields]]
name = "title"
type = "text_en"
stored = true

[[fields]]
name = "body"
type = "text_en"
stored = true

[[fields]]
name = "views"
type = "int"
stored = true
fast = true

[[fields]]
name = "rating"
type = "double"
stored = true
fast = true

[[fields]]
name = "created"
type = "date"
stored = true
fast = true

[[dynamic_fields]]
pattern = "*_i"
type = "int"
stored = true
fast = true

[[dynamic_fields]]
pattern = "*_txt_i"
type = "text_en"
stored = true

[[copy_fields]]
source = "title"
dest = "body"
"#;

fn write_schema(toml: &str) -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("schema.toml");
    std::fs::write(&path, toml).expect("write schema");
    (dir, path)
}

/// Materializes an index produced before #51's analyzer-contract marker
/// existed. It has the ordinary persisted schema snapshot and Tantivy schema,
/// but intentionally no new marker file.
fn create_pre_analyzer_contract_index(dir: &std::path::Path, toml: &str) {
    let schema_path = dir.join("schema.toml");
    std::fs::write(&schema_path, toml).expect("write legacy schema");
    let wf_schema = schema::load(&schema_path).expect("legacy schema loads");
    let data_dir = dir.join("data");
    std::fs::create_dir_all(&data_dir).expect("create legacy data dir");
    let _legacy_index = Index::builder()
        .schema(wf_schema.tantivy_schema)
        .create_in_dir(&data_dir)
        .expect("create legacy Tantivy index");
    std::fs::write(schema::snapshot_path(&data_dir), toml).expect("write legacy schema snapshot");
}

// --- dynamic field matching -------------------------------------------------

#[test]
fn dynamic_field_matching_is_longest_pattern_wins() {
    let (_dir, path) = write_schema(FULL_SCHEMA_TOML);
    let wf = schema::load(&path).expect("schema loads");

    // `count_i` matches only `*_i`.
    assert_eq!(
        wf.match_dynamic("count_i").map(|d| d.pattern.as_str()),
        Some("*_i"),
        "`count_i` must match the `*_i` pattern"
    );

    // `notes_txt_i` matches BOTH `*_i` and `*_txt_i`; Solr takes the longest
    // pattern, so the text type must win over the int one.
    let matched = wf
        .match_dynamic("notes_txt_i")
        .expect("`notes_txt_i` must match a dynamic pattern");
    assert_eq!(
        matched.pattern, "*_txt_i",
        "longest pattern must win (Solr semantics), got `{}`",
        matched.pattern
    );
    assert_eq!(matched.type_, "text_en");
}

#[test]
fn dynamic_field_with_no_matching_pattern_is_none() {
    let (_dir, path) = write_schema(FULL_SCHEMA_TOML);
    let wf = schema::load(&path).expect("schema loads");
    assert!(
        wf.match_dynamic("nosuchfield").is_none(),
        "a field matching no pattern must not resolve"
    );
    assert!(
        wf.match_dynamic("i_leading").is_none(),
        "`*_i` is a suffix pattern and must not match `i_leading`"
    );
}

#[test]
fn static_field_takes_precedence_over_dynamic_pattern() {
    // A static field named `views_i` and a `*_i` pattern: the static one wins.
    let toml = FULL_SCHEMA_TOML.replace(r#"name = "views""#, r#"name = "views_i""#);
    let (_dir, path) = write_schema(&toml);
    let wf = schema::load(&path).expect("schema loads");
    assert!(
        wf.field("views_i").is_some(),
        "the static field must exist in its own right"
    );
    assert!(
        wf.is_static("views_i"),
        "a declared field must be treated as static, not dynamic"
    );
}

// --- analyzer presets + custom chains ---------------------------------------

#[test]
fn text_presets_tokenize_as_expected() {
    let (_dir, path) = write_schema(FULL_SCHEMA_TOML);
    let wf = schema::load(&path).expect("schema loads");

    // Solr-compatible text_en: lowercase, remove English stopwords, then stem.
    assert_eq!(
        wf.tokenize("text_en", "The Quick Runners")
            .expect("text_en preset"),
        vec!["quick", "runner"],
        "text_en must drop English stopwords before stemming remaining tokens"
    );

    // text_general: lowercased, NOT stemmed.
    assert_eq!(
        wf.tokenize("text_general", "The Quick Runners")
            .expect("text_general preset"),
        vec!["the", "quick", "runners"],
        "text_general must lowercase without stemming"
    );

    // string / keyword: one untokenized, unlowercased term.
    assert_eq!(
        wf.tokenize("string", "The Quick Runners")
            .expect("string preset"),
        vec!["The Quick Runners"],
        "string must be a single raw term"
    );
    assert_eq!(
        wf.tokenize("keyword", "The Quick Runners")
            .expect("keyword preset"),
        vec!["The Quick Runners"],
        "keyword must behave like string"
    );
}

#[test]
fn pre_analyzer_contract_text_en_index_refuses_startup_requiring_reindex() {
    let dir = TempDir::new().expect("temp dir");
    create_pre_analyzer_contract_index(dir.path(), FULL_SCHEMA_TOML);

    let err = common::app_with_schema(dir.path(), FULL_SCHEMA_TOML)
        .expect_err("a pre-marker index with text_en data must require reindexing");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("text_en") && msg.to_lowercase().contains("reindex"),
        "legacy text_en refusal must identify the analyzer and require reindexing, got: {msg}"
    );
}

#[test]
fn pre_analyzer_contract_dynamic_text_index_refuses_startup_requiring_reindex() {
    let dynamic_text = FULL_SCHEMA_TOML
        .replace(
            r#"name = "title"
type = "text_en""#,
            r#"name = "title"
type = "text_general""#,
        )
        .replace(
            r#"name = "body"
type = "text_en""#,
            r#"name = "body"
type = "text_general""#,
        );
    let dir = TempDir::new().expect("temp dir");
    create_pre_analyzer_contract_index(dir.path(), &dynamic_text);

    let err = common::app_with_schema(dir.path(), &dynamic_text)
        .expect_err("a pre-marker dynamic text catch-all must require reindexing");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("text_en") && msg.to_lowercase().contains("reindex"),
        "legacy dynamic-text refusal must identify text_en and require reindexing, got: {msg}"
    );
}

#[test]
fn pre_analyzer_contract_raw_static_and_dynamic_schema_is_adopted() {
    let raw_only = r#"
[core]
name = "raw-content"
unique_key = "id"
default_field = "body"

[[fields]]
name = "id"
type = "string"
stored = true
required = true

[[fields]]
name = "body"
type = "string"
stored = true

[[fields]]
name = "category"
type = "keyword"
stored = true

[[dynamic_fields]]
pattern = "*_s"
type = "string"
stored = true

[[dynamic_fields]]
pattern = "*_k"
type = "keyword"
stored = true
"#;
    let dir = TempDir::new().expect("temp dir");
    create_pre_analyzer_contract_index(dir.path(), raw_only);

    let _ = common::app_with_schema(dir.path(), raw_only)
        .expect("a pre-marker schema with only raw static and dynamic fields must be adopted");
}

#[test]
fn pre_analyzer_contract_analyzed_dynamic_rules_always_refuse_startup() {
    // Dynamic values all pass through `_dynamic_text`, whose tokenizer changed
    // for #51, regardless of the user-facing dynamic rule's declared type.
    let without_static_text_en = FULL_SCHEMA_TOML.replace("text_en", "text_general");
    let dynamic_rule = r#"pattern = "*_txt_i"
type = "text_general""#;
    let schema_with_dynamic_type = |type_name: &str| {
        without_static_text_en.replace(
            dynamic_rule,
            &format!(
                r#"pattern = "*_txt_i"
type = "{type_name}""#
            ),
        )
    };
    let custom = format!(
        r#"{}
[[field_types]]
name = "custom_lowercase"
tokenizer = "simple"
[[field_types.filters]]
kind = "lowercase"
"#,
        schema_with_dynamic_type("custom_lowercase")
    );

    for (type_name, toml) in [
        ("text_general", schema_with_dynamic_type("text_general")),
        ("text_de", schema_with_dynamic_type("text_de")),
        ("custom_lowercase", custom),
    ] {
        let dir = TempDir::new().expect("temp dir");
        create_pre_analyzer_contract_index(dir.path(), &toml);

        let err = common::app_with_schema(dir.path(), &toml).expect_err(&format!(
            "a pre-marker index with a `{type_name}` dynamic rule must require reindexing"
        ));
        let msg = format!("{err:#}");
        assert!(
            msg.contains("text_en") && msg.to_lowercase().contains("reindex"),
            "legacy `{type_name}` dynamic-rule refusal must identify text_en and require reindexing, got: {msg}"
        );
    }
}

#[test]
fn a_language_preset_ships_for_every_tantivy_stemmer_language() {
    let (_dir, path) = write_schema(FULL_SCHEMA_TOML);
    let wf = schema::load(&path).expect("schema loads");

    // PRD open question 5: ship every language Tantivy's stemmer set gives
    // cheaply. `tantivy::tokenizer::Language` has 18 variants.
    for code in [
        "ar", "da", "nl", "en", "fi", "fr", "de", "el", "hu", "it", "no", "pt", "ro", "ru", "es",
        "sv", "ta", "tr",
    ] {
        let type_name = format!("text_{code}");
        assert!(
            wf.tokenize(&type_name, "Hello").is_some(),
            "preset `{type_name}` must be registered"
        );
    }

    // German stemming is observable: "Bücher" -> "bucher"/"buch" family, and
    // definitely not the untouched input.
    let de = wf
        .tokenize("text_de", "Häuser Bücher")
        .expect("text_de preset");
    assert_eq!(de.len(), 2, "text_de must tokenize into two terms: {de:?}");
    assert!(
        de.iter().all(|t| t.chars().all(|c| !c.is_uppercase())),
        "text_de must lowercase: {de:?}"
    );
    assert_ne!(
        de,
        vec!["häuser", "bücher"],
        "text_de must stem, not merely lowercase"
    );
}

#[test]
fn custom_field_type_applies_filters_in_declared_order() {
    let toml = format!(
        r#"{FULL_SCHEMA_TOML}
[[field_types]]
name = "text_en_custom"
tokenizer = "simple"
[[field_types.filters]]
kind = "lowercase"
[[field_types.filters]]
kind = "stopwords"
language = "english"
[[field_types.filters]]
kind = "stemmer"
language = "english"
"#
    );
    let (_dir, path) = write_schema(&toml);
    let wf = schema::load(&path).expect("schema loads");

    // "The" is an English stopword and must be dropped; "Runners" stemmed.
    assert_eq!(
        wf.tokenize("text_en_custom", "The Quick Runners")
            .expect("custom field type must be registered"),
        vec!["quick", "runner"],
        "custom chain must lowercase, drop stopwords, then stem"
    );
}

#[test]
fn custom_field_type_cannot_use_the_internal_text_en_tokenizer_identity() {
    let toml = format!(
        r#"{FULL_SCHEMA_TOML}
[[field_types]]
name = "wayfinder_text_en_v1"
tokenizer = "simple"
"#
    );
    let (_dir, path) = write_schema(&toml);
    let err = schema::load(&path).expect_err(
        "a custom field type must not overwrite Wayfinder's internal text_en tokenizer",
    );
    assert!(
        format!("{err:#}").contains("wayfinder_text_en_v1"),
        "reserved tokenizer-name error must identify the rejected custom field type: {err:#}"
    );
}

#[test]
fn unknown_filter_kind_errors_naming_the_field_type() {
    let toml = format!(
        r#"{FULL_SCHEMA_TOML}
[[field_types]]
name = "text_broken"
tokenizer = "simple"
[[field_types.filters]]
kind = "nosuchfilter"
"#
    );
    let (_dir, path) = write_schema(&toml);
    let err = schema::load(&path).expect_err("an unknown filter kind must be rejected");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("nosuchfilter") && msg.contains("text_broken"),
        "error must name the filter and the field type, got: {msg}"
    );
}

#[test]
fn unsupported_field_type_errors_naming_the_field() {
    let toml = FULL_SCHEMA_TOML.replace(r#"type = "double""#, r#"type = "nosuchtype""#);
    let (_dir, path) = write_schema(&toml);
    let err = schema::load(&path).expect_err("unsupported type must be rejected");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("nosuchtype") && msg.contains("rating"),
        "error must name the type and the field, got: {msg}"
    );
}

// --- unique_key must be string-typed (issue #9 review round 1, five-minute
// item) ---------------------------------------------------------------------

/// The update pipeline resolves `core.unique_key` as a Tantivy text term
/// (`Term::from_field_text` in `add_documents`/`delete_by_ids`) — a
/// non-string-typed uniqueKey would let that silently match nothing
/// (`overwrite=true` would duplicate instead of replace; delete-by-id would
/// 200 while deleting nothing). Schema load time rejects it loudly instead.
#[test]
fn non_string_unique_key_is_rejected_at_load_time() {
    let toml = FULL_SCHEMA_TOML.replace(
        r#"name = "id"
type = "string""#,
        r#"name = "id"
type = "int""#,
    );
    let (_dir, path) = write_schema(&toml);
    let err = schema::load(&path).expect_err("an int-typed uniqueKey must be rejected");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("unique_key") && msg.contains("id"),
        "error must name core.unique_key and the field, got: {msg}"
    );
}

// --- unique_key contract: single-valued, non-analyzed (issue #40) ----------

/// The update pipeline resolves `core.unique_key` as a single Tantivy text
/// term. A multi-valued uniqueKey field has no single term to resolve against
/// (`Term::from_field_text` takes one value), so overwrite/delete-by-id
/// semantics would be undefined. Schema load time must reject it loudly
/// instead of silently picking one value (or panicking) at request time.
#[test]
fn multi_valued_unique_key_is_rejected_at_load_time() {
    let toml = FULL_SCHEMA_TOML.replace(
        r#"name = "id"
type = "string"
stored = true
required = true"#,
        r#"name = "id"
type = "string"
stored = true
required = true
multi_valued = true"#,
    );
    let (_dir, path) = write_schema(&toml);
    let err = schema::load(&path).expect_err("a multi-valued uniqueKey must be rejected");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("unique_key") && msg.contains("id"),
        "error must name core.unique_key and the field, got: {msg}"
    );
}

/// `value_kind_of` maps both `ResolvedType::Str` (`string`/`keyword`, raw and
/// unanalyzed) and `ResolvedType::Text { .. }` (analyzed presets like
/// `text_en`, and custom `[[field_types]]` chains) to the same `ValueKind::Text`.
/// Only the former is safe as a uniqueKey: an analyzed field tokenizes a value
/// like `"Hello World"` into `["hello", "world"]`, so the document would no
/// longer match itself as a single exact term via `Term::from_field_text`.
/// Load time must reject an analyzed uniqueKey, not just a non-text one.
#[test]
fn analyzed_unique_key_is_rejected_at_load_time() {
    let toml = FULL_SCHEMA_TOML.replace(
        r#"name = "id"
type = "string""#,
        r#"name = "id"
type = "text_en""#,
    );
    let (_dir, path) = write_schema(&toml);
    let err = schema::load(&path).expect_err("an analyzed (text_en) uniqueKey must be rejected");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("unique_key") && msg.contains("id"),
        "error must name core.unique_key and the field, got: {msg}"
    );
}

/// A document missing its uniqueKey has no term to overwrite/delete by, so
/// the field must be guaranteed present on every document. Schema load time
/// must reject a uniqueKey that is not `required = true`.
#[test]
fn non_required_unique_key_is_rejected_at_load_time() {
    let toml = FULL_SCHEMA_TOML.replace(
        r#"name = "id"
type = "string"
stored = true
required = true"#,
        r#"name = "id"
type = "string"
stored = true
required = false"#,
    );
    let (_dir, path) = write_schema(&toml);
    let err = schema::load(&path).expect_err("a non-required uniqueKey must be rejected");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("unique_key") && msg.contains("id") && msg.contains("required"),
        "error must name core.unique_key, the field, and the `required` requirement, got: {msg}"
    );
}

/// Boundary control: a plain, single-valued `string` uniqueKey is exactly the
/// shape the two tests above are carving out as forbidden, so it must still
/// load cleanly.
#[test]
fn single_valued_string_unique_key_still_loads() {
    let (_dir, path) = write_schema(FULL_SCHEMA_TOML);
    schema::load(&path).expect("a single-valued, non-analyzed string uniqueKey must load");
}

// --- copy fields ------------------------------------------------------------

#[test]
fn copy_field_with_unknown_source_or_dest_errors_naming_it() {
    for (bad, needle) in [
        (r#"source = "title""#, "nosuchsource"),
        (r#"dest = "body""#, "nosuchdest"),
    ] {
        let replacement = bad.replace("title", needle).replace("body", needle);
        let toml = FULL_SCHEMA_TOML.replace(bad, &replacement);
        let (_dir, path) = write_schema(&toml);
        let err = schema::load(&path).expect_err("copy field must validate its endpoints");
        let msg = format!("{err:#}");
        assert!(
            msg.contains(needle),
            "error must name the unknown copy-field endpoint `{needle}`, got: {msg}"
        );
    }
}

#[tokio::test]
async fn copy_field_makes_source_text_searchable_on_dest() {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), FULL_SCHEMA_TOML).expect("app builds");

    // `zarquon` appears only in `title`, which copy-fields into `body`. No
    // direct `body` value here (issue #9): `body` is single-valued
    // (`multi_valued` unset), and the copy-field destination gets the same
    // single-valued enforcement as any other field across its combined own
    // + copied values (finding 48e) — a direct `body` value here would
    // collide with the copied `title` value and 400, which is not what this
    // test is about.
    let (status, _) = common::post_docs(&app, &json!([{"id":"c1","title":"zarquon rising"}])).await;
    assert_eq!(status, StatusCode::OK, "indexing must succeed");

    // `body` is the default field, so a bare term hits the copy-field target.
    let (status, resp) = common::get(&app, "select?q=zarquon&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "copy_fields must make `title` text searchable via `body`: {resp}"
    );
}

// --- numeric / date field types ---------------------------------------------

#[tokio::test]
async fn numeric_and_date_values_round_trip() {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), FULL_SCHEMA_TOML).expect("app builds");

    let (status, resp) = common::post_docs(
        &app,
        &json!([{
            "id":"n1", "body":"numbers",
            "views": 42, "rating": 4.5, "created": "2026-07-28T12:00:00Z"
        }]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "typed values must index: {resp}");

    let (status, resp) = common::get(&app, "select?q=id:n1&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    let doc = resp
        .pointer("/response/docs/0")
        .expect("the doc must come back");
    assert_eq!(
        doc.get("views"),
        Some(&json!(42)),
        "int must round-trip as a JSON number, not a string: {doc}"
    );
    assert_eq!(
        doc.get("rating"),
        Some(&json!(4.5)),
        "double must round-trip as a JSON number: {doc}"
    );
    assert_eq!(
        doc.get("created").and_then(Value::as_str),
        Some("2026-07-28T12:00:00Z"),
        "date must round-trip in Solr's RFC3339 form: {doc}"
    );
}

#[tokio::test]
async fn wrong_json_type_for_a_typed_field_is_rejected() {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), FULL_SCHEMA_TOML).expect("app builds");
    let (status, resp) = common::post_docs(
        &app,
        &json!([{"id":"n2","body":"x","views":"not-a-number"}]),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a non-numeric value for an int field must 400: {resp}"
    );
}

// --- doc-level unknown field handling ---------------------------------------

#[tokio::test]
async fn doc_with_unknown_field_is_rejected_like_strict_solr() {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), FULL_SCHEMA_TOML).expect("app builds");

    let (status, resp) =
        common::post_docs(&app, &json!([{"id":"u1","body":"x","nosuchfield":"y"}])).await;

    // Ground truth: solr-ref/responses/update_unknown_field_strict.json
    let expected = common::fixture("update_unknown_field_strict");
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "unknown doc field must 400 like strict Solr: {resp}"
    );
    assert_eq!(
        resp.pointer("/error/code"),
        expected.pointer("/error/code"),
        "error.code must match the captured Solr error"
    );
    assert_eq!(
        resp.pointer("/responseHeader/status"),
        expected.pointer("/responseHeader/status"),
        "responseHeader.status must mirror error.code"
    );
    let msg = resp
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        msg.contains("nosuchfield"),
        "error message must name the offending field, got: {msg}"
    );
}

#[tokio::test]
async fn doc_field_matching_a_dynamic_pattern_is_indexed_and_returned() {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), FULL_SCHEMA_TOML).expect("app builds");

    let (status, resp) = common::post_docs(
        &app,
        &json!([{"id":"d1","body":"dyn","count_i":7,"notes_txt_i":"quick runners"}]),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a field matching a dynamic pattern must be accepted: {resp}"
    );

    // Queryable by its own name...
    let (status, resp) = common::get(&app, "select?q=count_i:7&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "a dynamic field must be queryable under its document name: {resp}"
    );

    // ...and returned as its declared type.
    let doc = resp
        .pointer("/response/docs/0")
        .expect("doc must come back");
    assert_eq!(
        doc.get("count_i"),
        Some(&json!(7)),
        "a stored dynamic field must be returned with its declared type: {doc}"
    );

    // The longest-match pattern gave `notes_txt_i` a text_en type, so it stems.
    let (status, resp) = common::get(&app, "select?q=notes_txt_i:runner&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "`*_txt_i` must win over `*_i` and analyze as text_en: {resp}"
    );
}

// --- startup schema compatibility check -------------------------------------

#[test]
fn schema_compatibility_check_accepts_an_identical_schema() {
    schema::check_compatible(FULL_SCHEMA_TOML, FULL_SCHEMA_TOML)
        .expect("an unchanged schema must be compatible");
}

#[test]
fn schema_compatibility_check_refuses_a_removed_field_naming_it() {
    let without_rating = FULL_SCHEMA_TOML.replace(
        r#"
[[fields]]
name = "rating"
type = "double"
stored = true
fast = true
"#,
        "\n",
    );
    let err = schema::check_compatible(FULL_SCHEMA_TOML, &without_rating)
        .expect_err("removing a field must be refused");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("rating"),
        "refusal must name the removed field, got: {msg}"
    );
}

#[test]
fn schema_compatibility_check_allows_toggling_required() {
    // `required` is an input-validation rule, not part of the Tantivy schema, so
    // it must not force a reindex.
    let now_required = FULL_SCHEMA_TOML.replace(
        r#"name = "title"
type = "text_en"
stored = true"#,
        r#"name = "title"
type = "text_en"
stored = true
required = true"#,
    );
    assert_ne!(
        now_required, FULL_SCHEMA_TOML,
        "test setup: the `required` toggle must actually change the schema"
    );
    schema::check_compatible(FULL_SCHEMA_TOML, &now_required)
        .expect("toggling `required` must not require a reindex");
}

#[test]
fn schema_compatibility_check_refuses_a_changed_field_option_naming_it() {
    let now_fast = FULL_SCHEMA_TOML.replace(
        r#"name = "title"
type = "text_en"
stored = true"#,
        r#"name = "title"
type = "text_en"
stored = true
fast = true"#,
    );
    let err = schema::check_compatible(FULL_SCHEMA_TOML, &now_fast)
        .expect_err("making a field fast changes the Tantivy schema");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("title"),
        "refusal must name the changed field, got: {msg}"
    );
}

#[test]
fn schema_compatibility_check_refuses_a_retyped_field_naming_it() {
    let retyped = FULL_SCHEMA_TOML.replace(
        r#"name = "views"
type = "int""#,
        r#"name = "views"
type = "double""#,
    );
    let err = schema::check_compatible(FULL_SCHEMA_TOML, &retyped)
        .expect_err("retyping a field must be refused");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("views"),
        "refusal must name the retyped field, got: {msg}"
    );
}

#[test]
fn schema_compatibility_check_reports_an_added_field_as_needing_a_reindex() {
    let with_extra = format!(
        r#"{FULL_SCHEMA_TOML}
[[fields]]
name = "summary"
type = "text_en"
stored = true
"#
    );
    // PRD open question 4 calls an added field "compatible", but Tantivy cannot
    // extend the schema of an existing index in place, so v1 still requires a
    // reindex — the refusal must say so, and name the field, rather than
    // failing with Tantivy's opaque "schema does not match".
    let err = schema::check_compatible(FULL_SCHEMA_TOML, &with_extra)
        .expect_err("adding a field still needs a reindex under Tantivy");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("summary") && msg.to_lowercase().contains("reindex"),
        "refusal must name the added field and say a reindex is needed, got: {msg}"
    );
}

/// `FULL_SCHEMA_TOML` with the whole `[[dynamic_fields]]` block removed.
fn schema_without_dynamic_fields() -> String {
    let start = FULL_SCHEMA_TOML
        .find("[[dynamic_fields]]")
        .expect("test setup: the fixture schema must declare dynamic fields");
    let end = FULL_SCHEMA_TOML
        .find("[[copy_fields]]")
        .expect("test setup: the fixture schema must declare copy fields");
    let stripped = format!("{}{}", &FULL_SCHEMA_TOML[..start], &FULL_SCHEMA_TOML[end..]);
    assert!(
        !stripped.contains("[[dynamic_fields]]") && stripped.contains("[[copy_fields]]"),
        "test setup: only the dynamic_fields block must be removed"
    );
    stripped
}

#[test]
fn schema_compatibility_check_refuses_adding_the_first_or_removing_the_last_dynamic_rule() {
    // The catch-all JSON fields backing dynamic fields exist only when at least
    // one rule does, so crossing that boundary changes the Tantivy schema even
    // though editing rules in between does not.
    let none = schema_without_dynamic_fields();

    let err = schema::check_compatible(&none, FULL_SCHEMA_TOML)
        .expect_err("adding the first dynamic rule changes the Tantivy schema");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("dynamic_fields") && msg.to_lowercase().contains("reindex"),
        "refusal must name dynamic_fields and say a reindex is needed, got: {msg}"
    );

    let err = schema::check_compatible(FULL_SCHEMA_TOML, &none)
        .expect_err("removing the last dynamic rule changes the Tantivy schema");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("dynamic_fields") && msg.to_lowercase().contains("reindex"),
        "refusal must name dynamic_fields and say a reindex is needed, got: {msg}"
    );
}

#[test]
fn schema_compatibility_check_allows_editing_dynamic_rules_without_emptying_them() {
    // Rule set stays non-empty: no catch-all field appears or disappears, so no
    // reindex is needed.
    let extra_rule = format!(
        r#"{FULL_SCHEMA_TOML}
[[dynamic_fields]]
pattern = "*_s"
type = "string"
stored = true
"#
    );
    schema::check_compatible(FULL_SCHEMA_TOML, &extra_rule)
        .expect("adding a further dynamic rule must not require a reindex");
}

#[tokio::test]
async fn reopening_a_data_dir_after_toggling_dynamic_fields_refuses_both_ways() {
    // empty -> one rule
    let none = schema_without_dynamic_fields();
    let dir = TempDir::new().expect("temp dir");
    {
        let app = common::app_with_schema(dir.path(), &none).expect("app builds");
        let (status, _) = common::post_docs(&app, &json!([{"id":"t1","body":"x"}])).await;
        assert_eq!(status, StatusCode::OK);
    }
    let err = common::app_with_schema(dir.path(), FULL_SCHEMA_TOML)
        .expect_err("adding the first dynamic rule must refuse on reopen");
    assert!(
        format!("{err:#}").contains("dynamic_fields"),
        "refusal must name dynamic_fields, got: {err:#}"
    );

    // one rule -> empty
    let dir = TempDir::new().expect("temp dir");
    {
        let app = common::app_with_schema(dir.path(), FULL_SCHEMA_TOML).expect("app builds");
        let (status, _) = common::post_docs(&app, &json!([{"id":"t2","body":"x"}])).await;
        assert_eq!(status, StatusCode::OK);
    }
    let err = common::app_with_schema(dir.path(), &none)
        .expect_err("removing the last dynamic rule must refuse on reopen");
    assert!(
        format!("{err:#}").contains("dynamic_fields"),
        "refusal must name dynamic_fields, got: {err:#}"
    );
}

#[test]
fn a_pattern_with_stars_at_both_ends_is_rejected_at_load_time() {
    // Solr has no substring dynamic-field form; refuse rather than invent one.
    let toml = FULL_SCHEMA_TOML.replace(r#"pattern = "*_i""#, r#"pattern = "*_i*""#);
    let err = {
        let (_dir, path) = write_schema(&toml);
        schema::load(&path).expect_err("a two-star pattern must be rejected")
    };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("*_i*"),
        "error must name the offending pattern, got: {msg}"
    );
}

#[tokio::test]
async fn reopening_a_data_dir_with_a_changed_schema_refuses_with_a_clear_error() {
    let dir = TempDir::new().expect("temp dir");
    {
        let app = common::app_with_schema(dir.path(), FULL_SCHEMA_TOML).expect("app builds");
        let (status, _) = common::post_docs(&app, &json!([{"id":"s1","body":"x"}])).await;
        assert_eq!(status, StatusCode::OK);
    }

    // Same data dir, `views` retyped int -> double.
    let retyped = FULL_SCHEMA_TOML.replace(
        r#"name = "views"
type = "int""#,
        r#"name = "views"
type = "double""#,
    );
    let err = common::app_with_schema(dir.path(), &retyped)
        .expect_err("reopening with an incompatible schema must refuse");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("views"),
        "startup refusal must name the offending field, got: {msg}"
    );
}
