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
use tantivy::{
    Index, Term,
    collector::Count,
    query::TermQuery,
    schema::IndexRecordOption,
    tokenizer::{Language, LowerCaser, SimpleTokenizer, Stemmer, TextAnalyzer},
};
use tantivy::tokenizer::TokenStream;
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

/// Materializes an index with an older `text_en` tokenizer identity. It has
/// the ordinary persisted schema snapshot and Tantivy schema, but no analyzer
/// marker unless the caller writes one.
fn create_older_analyzer_index(dir: &std::path::Path, toml: &str, tokenizer: &str) {
    let schema_path = dir.join("schema.toml");
    std::fs::write(&schema_path, toml).expect("write legacy schema");
    let wf_schema = schema::load(&schema_path).expect("legacy schema loads");
    // Targets the CURRENT static-text_en identity (`wayfinder_text_en_v3` as
    // of issue #388, previously `_v2`), so a caller-given older identity
    // (`wayfinder_text_en_v1`, bare `en_stem`, ...) actually lands in the
    // serialized schema in its place.
    let schema_json = serde_json::to_string(&wf_schema.tantivy_schema)
        .expect("serialize current Tantivy schema")
        .replace("wayfinder_text_en_v3", tokenizer);
    // As above, but for `_dynamic_text`'s CURRENT identity
    // (`wayfinder_dynamic_text_v3` as of #388; it no longer coincides with
    // static `text_en`'s v1 identity the way it did pre-#388). Swapped
    // whenever the caller wants a truly historical shared identity on both
    // paths at once: the fully pre-#51 bare `en_stem` state, or the v1/v2-era
    // state in which `_dynamic_text` had not yet diverged from static
    // `text_en`'s v1 identity (`wayfinder_text_en_v1` was the catch-all's
    // identity through the v2 contract bump too -- only static `text_en`
    // moved to `_v2`; see the `DYNAMIC_TEXT_TOKENIZER` doc comment in
    // `src/schema.rs`). A caller building an all-raw-dynamic-rule schema (no
    // static analyzed field at all, so the first replace above is a no-op)
    // relies on this branch to land the retired identity on `_dynamic_text`.
    let schema_json = if tokenizer == "en_stem" || tokenizer == "wayfinder_text_en_v1" {
        schema_json.replace("wayfinder_dynamic_text_v3", tokenizer)
    } else {
        schema_json
    };
    let data_dir = dir.join("data");
    std::fs::create_dir_all(&data_dir).expect("create legacy data dir");
    let _legacy_index = Index::builder()
        .schema(serde_json::from_str(&schema_json).expect("legacy Tantivy schema parses"))
        .create_in_dir(&data_dir)
        .expect("create legacy Tantivy index");
    std::fs::write(schema::snapshot_path(&data_dir), toml).expect("write legacy schema snapshot");
}

/// Materializes an index produced before #51's analyzer-contract marker.
fn create_pre_analyzer_contract_index(dir: &std::path::Path, toml: &str) {
    create_older_analyzer_index(dir, toml, "en_stem");
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
    // Solr 9.10.1 `_default` text_en: its English Porter stemmer maps `day`
    // to `dai`; Tantivy's English stemmer leaves it unchanged.
    assert_eq!(
        wf.tokenize("text_en", "day boy happy sky")
            .expect("text_en preset"),
        vec!["dai", "boi", "happi", "sky"],
        "text_en must apply Porter's terminal-y rule only when the stem contains a vowel"
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

// --- issue #388: text_en / text_general / _dynamic_text length bounds and
// simple case folding --------------------------------------------------------
//
// Wayfinder's global analyzed text chains diverge from the shipped
// `search_api_solr` configset in three ways (see
// `tests/select_analyzer_bounds.rs`'s module doc for the full writeup):
// no `LengthFilterFactory min="2" max="100"` lower/upper bound, and Rust's
// full-Unicode `LowerCaser` instead of Lucene's simple (1:1) folding on
// U+0130. All three of Wayfinder's chains carry the bug: static `text_en`,
// static `text_general` (`text_general` is not currently a Wayfinder-owned
// chain at all -- it resolves to Tantivy's own `default` tokenizer), and the
// shared `_dynamic_text` catch-all every analyzed dynamic rule flows through
// regardless of its declared type.

/// A schema with an analyzed dynamic rule, so `_dynamic_text`'s tokenizer
/// is actually registered in the returned schema's `TokenizerManager`.
/// `WayfinderSchema::tokenize` only resolves preset *type names*
/// (`text_en`/`text_general`), not internal tokenizer identities, so the
/// dynamic catch-all's chain has to be read directly off `wf.tokenizers`
/// (see `tokenize_identity` below) rather than through `tokenize()`.
const DYNAMIC_ANALYZED_SCHEMA_TOML: &str = r#"
[core]
name = "content"
unique_key = "id"
default_field = "id"

[[fields]]
name = "id"
type = "string"
stored = true
required = true

[[dynamic_fields]]
pattern = "*_txt"
type = "text_general"
stored = true
"#;

/// Runs `text` through the tokenizer registered under `tokenizer_name` in
/// `wf`'s `TokenizerManager` and returns the resulting terms. Panics if the
/// identity is not registered at all, which is the expected failure mode for
/// the not-yet-implemented v3 identities this file asserts against.
fn tokenize_identity(
    wf: &schema::WayfinderSchema,
    tokenizer_name: &str,
    text: &str,
) -> Vec<String> {
    let mut analyzer = wf
        .tokenizers
        .get(tokenizer_name)
        .unwrap_or_else(|| panic!("tokenizer `{tokenizer_name}` must be registered"));
    let mut stream = analyzer.token_stream(text);
    let mut out = Vec::new();
    while stream.advance() {
        out.push(stream.token().text.clone());
    }
    out
}

#[test]
fn text_en_preset_enforces_the_configsets_length_bounds_and_simple_case_folding() {
    let (_dir, path) = write_schema(FULL_SCHEMA_TOML);
    let wf = schema::load(&path).expect("schema loads");

    // AMENDMENT 1 (issue #388): static text_en/text_general stand in for
    // Solr's `_default` configset (capture.sh:1686 boots the edismax
    // reference core with `solr-precreate content`, no configset argument),
    // whose text_en has NO LengthFilterFactory at all. Five edismax fixtures
    // pin this: the one-character `b`/`c`/`d` tokens in the pA/pB/mmA-mmD
    // docs move BM25's average field length and flip a 1% score gap, so a
    // one-character token surviving here is ground truth, not an unfixed bug.
    // The min=2 lower bound belongs to the dynamic-catch-all test only
    // (`_dynamic_text` stands in for the search_api_solr configset instead).
    assert_eq!(
        wf.tokenize("text_en", "i").expect("text_en preset"),
        vec!["i"],
        "issue #388 AMENDMENT 1: static text_en stands in for Solr's `_default` configset, \
         which has no LengthFilterFactory at all -- a one-character token must survive, \
         pinned by the edismax fixtures' BM25 average-field-length behaviour"
    );
    assert_eq!(
        wf.tokenize("text_en", "hi").expect("text_en preset"),
        vec!["hi"],
        "a two-character token must survive -- unaffected either way"
    );

    // ponytail: RemoveLongFilter::limit(40) counting bytes (not
    // LengthFilterFactory's characters, which `_default` doesn't have at
    // all) is a pre-existing divergence from `_default`, which drops
    // nothing at any length. Unpinned by any fixture; out of scope for
    // #388 (AMENDMENT 1 corrected the spec to leave this filter alone) and
    // filed as a follow-up.
    let a45 = "a".repeat(45);
    assert_eq!(
        wf.tokenize("text_en", &a45).expect("text_en preset"),
        Vec::<String>::new(),
        "issue #388 AMENDMENT 1: static text_en keeps RemoveLongFilter::limit(40) (bytes) \
         unchanged -- a 45-character/45-byte token must still be dropped"
    );
    let b100 = "b".repeat(100);
    assert_eq!(
        wf.tokenize("text_en", &b100).expect("text_en preset"),
        Vec::<String>::new(),
        "issue #388 AMENDMENT 1: 100 bytes is still well over RemoveLongFilter::limit(40) -- \
         must be dropped, same as before this issue"
    );
    let c101 = "c".repeat(101);
    assert_eq!(
        wf.tokenize("text_en", &c101).expect("text_en preset"),
        Vec::<String>::new(),
        "issue #388 AMENDMENT 1: 101 bytes is still well over RemoveLongFilter::limit(40) -- \
         must be dropped, same as before this issue"
    );

    assert_eq!(
        wf.tokenize("text_en", "İstanbul").expect("text_en preset"),
        vec!["istanbul"],
        "issue #388: Lucene's LowerCaseFilterFactory is Character.toLowerCase, Unicode's \
         SIMPLE (1:1) mapping -- U+0130 LATIN CAPITAL LETTER I WITH DOT ABOVE must fold to a \
         bare `i`. Tantivy's LowerCaser is Rust's full-Unicode str::to_lowercase, which \
         appends a trailing U+0307 COMBINING DOT ABOVE; assert the exact token, not `contains`"
    );

    // The existing expectations text_presets_tokenize_as_expected pins
    // (stemming, stopwords) must still hold -- none of its tokens are
    // shorter than 2 characters or contain U+0130, so the length/case fix
    // must not move them.
    assert_eq!(
        wf.tokenize("text_en", "The Quick Runners")
            .expect("text_en preset"),
        vec!["quick", "runner"],
        "text_en must still drop English stopwords before stemming remaining tokens"
    );
}

#[test]
fn text_general_preset_enforces_the_configsets_length_bounds_and_simple_case_folding() {
    let (_dir, path) = write_schema(FULL_SCHEMA_TOML);
    let wf = schema::load(&path).expect("schema loads");

    // AMENDMENT 1 (issue #388): static text_general stands in for Solr's
    // `_default` configset the same way text_en does (see the comment on
    // that test above) -- ground truth is the edismax/select/terms fixtures,
    // not the search_api_solr configset's `text_und`. A one-character token
    // must survive; the min=2 lower bound belongs to the dynamic-catch-all
    // test only.
    assert_eq!(
        wf.tokenize("text_general", "i")
            .expect("text_general preset"),
        vec!["i"],
        "issue #388 AMENDMENT 1: static text_general stands in for Solr's `_default` \
         configset, which has no LengthFilterFactory at all -- a one-character token must \
         survive"
    );
    assert_eq!(
        wf.tokenize("text_general", "hi")
            .expect("text_general preset"),
        vec!["hi"],
        "a two-character token must survive -- unaffected either way"
    );

    // ponytail: text_general currently resolves to Tantivy's own built-in
    // `default` tokenizer (SimpleTokenizer -> RemoveLongFilter::limit(40) ->
    // LowerCaser), so it already carries this same byte-based 40-byte cut
    // `_default` (no length filter at all) does not have. Pre-existing,
    // unpinned by any fixture, and out of scope for #388 (AMENDMENT 1
    // corrected the spec to leave this filter alone on both static
    // presets) -- filed as a follow-up.
    let a45 = "a".repeat(45);
    assert_eq!(
        wf.tokenize("text_general", &a45)
            .expect("text_general preset"),
        Vec::<String>::new(),
        "issue #388 AMENDMENT 1: static text_general keeps RemoveLongFilter::limit(40) \
         (bytes) unchanged -- a 45-character/45-byte token must still be dropped"
    );
    let b100 = "b".repeat(100);
    assert_eq!(
        wf.tokenize("text_general", &b100)
            .expect("text_general preset"),
        Vec::<String>::new(),
        "issue #388 AMENDMENT 1: 100 bytes is still well over RemoveLongFilter::limit(40) \
         -- must be dropped, same as before this issue"
    );
    let c101 = "c".repeat(101);
    assert_eq!(
        wf.tokenize("text_general", &c101)
            .expect("text_general preset"),
        Vec::<String>::new(),
        "issue #388 AMENDMENT 1: 101 bytes is still well over RemoveLongFilter::limit(40) \
         -- must be dropped, same as before this issue"
    );

    assert_eq!(
        wf.tokenize("text_general", "İstanbul")
            .expect("text_general preset"),
        vec!["istanbul"],
        "issue #388: text_general must fold U+0130 the same simple (1:1) way text_en does -- \
         a bare `i`, not Rust's full mapping with a trailing U+0307 COMBINING DOT ABOVE"
    );
}

#[test]
fn dynamic_text_catch_all_enforces_the_configsets_length_bounds_and_simple_case_folding() {
    let (_dir, path) = write_schema(DYNAMIC_ANALYZED_SCHEMA_TOML);
    let wf = schema::load(&path).expect("schema loads");
    let tokenizer = "wayfinder_dynamic_text_v3";

    assert_eq!(
        tokenize_identity(&wf, tokenizer, "i"),
        Vec::<String>::new(),
        "issue #388: `_dynamic_text` -- the shared catch-all every analyzed dynamic rule flows \
         through regardless of its declared type -- must drop a one-character token just like \
         the static presets"
    );
    assert_eq!(
        tokenize_identity(&wf, tokenizer, "hi"),
        vec!["hi"],
        "a two-character token must survive -- the lower bound is inclusive"
    );

    let a45 = "a".repeat(45);
    assert_eq!(
        tokenize_identity(&wf, tokenizer, &a45),
        vec![a45.clone()],
        "issue #388: a 45-character token is inside max=\"100\" and must survive. Regression \
         guard against RemoveLongFilter::limit(40) (bytes) being re-added instead of \
         LengthFilter (characters)"
    );
    let b100 = "b".repeat(100);
    assert_eq!(
        tokenize_identity(&wf, tokenizer, &b100),
        vec![b100.clone()],
        "issue #388: the upper bound is inclusive -- exactly 100 characters must survive"
    );
    let c101 = "c".repeat(101);
    assert_eq!(
        tokenize_identity(&wf, tokenizer, &c101),
        Vec::<String>::new(),
        "101 characters must be dropped -- one over the inclusive max=\"100\" bound"
    );

    assert_eq!(
        tokenize_identity(&wf, tokenizer, "İstanbul"),
        vec!["istanbul"],
        "issue #388: `_dynamic_text` must fold U+0130 the same simple way the static presets \
         do -- a bare `i`, not Rust's full mapping with a trailing U+0307 COMBINING DOT ABOVE"
    );

    // `_dynamic_text` deliberately keeps its v1 Snowball semantics (stopwords
    // + Stemmer, no PorterTerminalYFilter) -- only the length bound and case
    // folding change. `day` staying `day` (not `dai`, as text_en's Porter
    // terminal-y rule would produce) is the discriminator.
    assert_eq!(
        tokenize_identity(&wf, tokenizer, "day"),
        vec!["day"],
        "_dynamic_text must keep Snowball English stemming (day stays day), unlike text_en's \
         Porter terminal-y variant"
    );
}

// --- issue #388: analyzer contract v3 (text_en/text_general length+case
// bounds and simple case folding, and _dynamic_text's length+case fix) ------
//
// Changing the three chains above changes the terms already written to disk,
// so `src/core_index.rs`'s existing startup machinery (the same machinery
// that gated v1 -> v2 above) must refuse an incompatible existing index
// rather than silently return wrong results. `ANALYZER_CONTRACT` becomes the
// v3 marker; the v2 value moves to a new `ANALYZER_CONTRACT_V2` const (not
// referenced by name here -- these tests write its literal value directly, so
// they fail on an observed VALUE today rather than a missing symbol).

/// A marker already at the CURRENT contract must open normally and stay
/// untouched. Read through the live `schema::ANALYZER_CONTRACT` symbol
/// (never a literal) so this test tracks whatever the current contract value
/// is, both before and after #388 lands -- it is an invariant of every
/// contract version, not a value newly introduced by this issue.
#[test]
fn current_analyzer_contract_marker_opens_normally() {
    let dir = TempDir::new().expect("temp dir");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let marker = data_dir.join("wayfinder-analyzer-contract");
    std::fs::write(&marker, schema::ANALYZER_CONTRACT)
        .expect("write current analyzer contract marker");

    let _app = common::app_with_schema(dir.path(), FULL_SCHEMA_TOML).expect(
        "an index already certified under the current analyzer contract must open normally",
    );
    assert_eq!(
        std::fs::read_to_string(&marker).expect("read analyzer contract marker"),
        schema::ANALYZER_CONTRACT,
        "a marker already at the current contract must be left untouched"
    );
}

/// A v2 marker (today's `ANALYZER_CONTRACT` value, to become
/// `ANALYZER_CONTRACT_V2`) on a schema with a static `text_en` field -- which
/// v3 changes -- must require reindexing. Predicted RED: today's code treats
/// this literal as the CURRENT contract (nothing has bumped to v3 yet), so it
/// opens fine and `.expect_err` panics.
#[test]
fn v2_marker_on_static_text_en_field_refuses_startup_requiring_reindex() {
    let dir = TempDir::new().expect("temp dir");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    std::fs::write(
        data_dir.join("wayfinder-analyzer-contract"),
        "text_en_porter_compatible_v2",
    )
    .expect("write v2 analyzer contract marker");

    let err = common::app_with_schema(dir.path(), FULL_SCHEMA_TOML).expect_err(
        "a v2 marker with a static text_en field must require reindexing for the v3 \
         length+case fix (issue #388)",
    );
    let msg = format!("{err:#}");
    assert!(
        msg.to_lowercase().contains("reindex"),
        "v2-with-affected-data refusal must require reindexing, got: {msg}"
    );
}

/// As above, but the affected field is static `text_general` -- new ground
/// for #388, since `text_general` did not change between v1 and v2 at all.
/// Predicted RED for the same reason as the text_en case.
#[test]
fn v2_marker_on_static_text_general_field_refuses_startup_requiring_reindex() {
    let text_general_only = r#"
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
name = "body"
type = "text_general"
stored = true
"#;
    let dir = TempDir::new().expect("temp dir");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    std::fs::write(
        data_dir.join("wayfinder-analyzer-contract"),
        "text_en_porter_compatible_v2",
    )
    .expect("write v2 analyzer contract marker");

    let err = common::app_with_schema(dir.path(), text_general_only).expect_err(
        "a v2 marker with a static text_general field must require reindexing for the v3 \
         length+case fix (issue #388) -- text_general did not change v1 -> v2 at all, so this \
         is new ground",
    );
    let msg = format!("{err:#}");
    assert!(
        msg.to_lowercase().contains("reindex"),
        "v2-with-affected-data refusal must require reindexing, got: {msg}"
    );
}

/// As above, but the affected path is an analyzed `[[dynamic_fields]]` rule
/// (which flows through `_dynamic_text`) rather than a static field.
/// Predicted RED for the same reason.
#[test]
fn v2_marker_on_analyzed_dynamic_rule_refuses_startup_requiring_reindex() {
    let dir = TempDir::new().expect("temp dir");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    std::fs::write(
        data_dir.join("wayfinder-analyzer-contract"),
        "text_en_porter_compatible_v2",
    )
    .expect("write v2 analyzer contract marker");

    let err = common::app_with_schema(dir.path(), DYNAMIC_ANALYZED_SCHEMA_TOML).expect_err(
        "a v2 marker with an analyzed dynamic rule must require reindexing for the v3 \
         _dynamic_text length+case fix (issue #388)",
    );
    let msg = format!("{err:#}");
    assert!(
        msg.to_lowercase().contains("reindex"),
        "v2-with-affected-data refusal must require reindexing, got: {msg}"
    );
}

/// A v2 marker on a schema that can hold NO affected data (raw/string fields
/// only, no analyzed dynamic rule) must be silently upgraded to the current
/// (v3) contract rather than bailing -- mirroring `v1_raw_only_analyzer_
/// contract_is_upgraded_without_reindex` above, one version later. This is an
/// invariant that already holds today (a marker equal to the live
/// `ANALYZER_CONTRACT` symbol always opens fine, regardless of schema
/// content), so it is not expected to be red on its own -- it guards against
/// the v2-affected-data tests above being satisfied by a change that makes
/// EVERY v2 marker bail unconditionally, which would break raw-only adoption.
#[tokio::test]
async fn v2_marker_on_raw_only_schema_is_upgraded_without_reindex() {
    let raw_only = r#"
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
name = "body"
type = "string"
stored = true

[[dynamic_fields]]
pattern = "*_s"
type = "string"
stored = true
"#;
    let dir = TempDir::new().expect("temp dir");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let marker = data_dir.join("wayfinder-analyzer-contract");
    std::fs::write(&marker, "text_en_porter_compatible_v2")
        .expect("write v2 analyzer contract marker");

    let app = common::app_with_schema(dir.path(), raw_only)
        .expect("a v2 index with no changed analyzer path must remain adoptable");
    assert_eq!(
        std::fs::read_to_string(&marker).expect("read upgraded analyzer contract"),
        schema::ANALYZER_CONTRACT,
        "safe v2 adoption must persist the current (v3) analyzer contract"
    );

    // A raw-only schema still has `[[dynamic_fields]]`, so `catch_all_fields`
    // (`src/schema.rs`) gives it a `_dynamic_text` catch-all regardless of the
    // rule's own (non-analyzed) type -- opening the index is not enough to
    // prove that catch-all's tokenizer identity actually resolves; only a
    // write through it does (`tests/schema_layer.rs` module background,
    // issue #388 follow-up: `open().is_ok()` alone stayed green through a
    // real `Error getting tokenizer for field: _dynamic_text` regression).
    let (status, body) = common::post_docs(&app, &json!([{"id": "d1", "extra_s": "hello"}])).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a doc through the raw-only `_dynamic_text` catch-all must write after safe v2 \
         adoption, got: {body:?}"
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
fn v1_text_en_analyzer_contract_refuses_startup_requiring_reindex() {
    let dir = TempDir::new().expect("temp dir");
    create_older_analyzer_index(dir.path(), FULL_SCHEMA_TOML, "wayfinder_text_en_v1");
    std::fs::write(
        dir.path().join("data/wayfinder-analyzer-contract"),
        "text_en_stopwords_v1",
    )
    .expect("write current v1 analyzer contract");

    let err = match common::app_with_schema(dir.path(), FULL_SCHEMA_TOML) {
        Ok(_) => panic!("a v1 text_en index must require reindexing under the v2 contract"),
        Err(err) => err,
    };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("text_en") && msg.to_lowercase().contains("reindex"),
        "v1 text_en refusal must identify the analyzer and require reindexing, got: {msg}"
    );
}

#[test]
fn v1_marker_with_old_dynamic_postings_hidden_by_raw_snapshot_refuses_reindex() {
    let old_dynamic = DYNAMIC_ANALYZED_SCHEMA_TOML;
    let raw_dynamic = old_dynamic.replace("type = \"text_general\"", "type = \"string\"");
    assert_ne!(old_dynamic, raw_dynamic, "test setup: evolution must remove the analyzed rule");

    let dir = TempDir::new().expect("temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, old_dynamic).expect("write old analyzed schema");
    let old = schema::load(&schema_path).expect("old analyzed schema loads");
    let legacy_schema_json = serde_json::to_string(&old.tantivy_schema)
        .expect("serialize old Tantivy schema")
        .replace("wayfinder_dynamic_text_v3", "wayfinder_text_en_v1");
    assert!(
        legacy_schema_json.contains("wayfinder_text_en_v1"),
        "test setup: old catch-all must persist its v1 tokenizer identity"
    );

    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let legacy_tokenizers = old.tokenizers.clone();
    legacy_tokenizers.register(
        "wayfinder_text_en_v1",
        TextAnalyzer::builder(SimpleTokenizer::default())
            .filter(LowerCaser)
            .filter(Stemmer::new(Language::English))
            .build(),
    );
    {
        let legacy_index = Index::builder()
            .schema(serde_json::from_str(&legacy_schema_json).expect("legacy schema parses"))
            .tokenizers(legacy_tokenizers.clone())
            .fast_field_tokenizers(legacy_tokenizers)
            .create_in_dir(&data_dir)
            .expect("create old index");
        let dynamic_text = legacy_index
            .schema()
            .get_field("_dynamic_text")
            .expect("old schema has the dynamic catch-all");
        let mut writer = legacy_index.writer(15_000_000).expect("open old writer");
        writer
            .add_document(tantivy::doc!(dynamic_text => json!({"legacy_txt": "X"})))
            .expect("write through old analyzed catch-all");
        writer.commit().expect("commit old analyzed posting");

        // v1 indexed a one-character token; v3's catch-all drops it at the
        // new min=2 bound. A direct term query proves an actual old analyzed
        // posting exists, rather than merely an analyzed rule in a snapshot.
        let mut old_posting = Term::from_field_json_path(dynamic_text, "legacy_txt", false);
        old_posting.append_type_and_str("x");
        assert_eq!(
            legacy_index
                .reader()
                .expect("open old reader")
                .searcher()
                .search(
                    &TermQuery::new(old_posting, IndexRecordOption::Basic),
                    &Count,
                )
                .expect("search old posting"),
            1,
            "test setup: `_dynamic_text` must hold an actual old-chain analyzed posting"
        );
    }

    // This evolution was historically allowed: its latest persisted snapshot
    // says every dynamic rule is raw, even though the physical index above
    // already contains an old analyzed `_dynamic_text` posting.
    std::fs::write(schema::snapshot_path(&data_dir), &raw_dynamic)
        .expect("overwrite snapshot with evolved raw schema");
    std::fs::write(
        data_dir.join("wayfinder-analyzer-contract"),
        "text_en_stopwords_v1",
    )
    .expect("write v1 analyzer contract marker");

    let err = common::app_with_schema(dir.path(), &raw_dynamic).expect_err(
        "a raw latest snapshot cannot prove the old catch-all was unused; startup must require reindexing",
    );
    assert!(
        format!("{err:#}").to_lowercase().contains("reindex"),
        "unsafe legacy adoption must require reindexing, got: {err:#}"
    );
}

#[tokio::test]
async fn v1_raw_only_analyzer_contract_is_upgraded_without_reindex() {
    let raw_only = r#"
[core]
name = "raw-v1"
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

[[dynamic_fields]]
pattern = "*_s"
type = "string"
stored = true
"#;
    let dir = TempDir::new().expect("temp dir");
    create_older_analyzer_index(dir.path(), raw_only, "wayfinder_text_en_v1");
    let marker = dir.path().join("data/wayfinder-analyzer-contract");
    std::fs::write(&marker, "text_en_stopwords_v1").expect("write v1 analyzer contract");

    let app = common::app_with_schema(dir.path(), raw_only)
        .expect("a v1 index with no changed analyzer path must remain adoptable");
    assert_eq!(
        std::fs::read_to_string(marker).expect("read upgraded analyzer contract"),
        schema::ANALYZER_CONTRACT,
        "safe v1 adoption must persist the current analyzer contract"
    );

    // Issue #388 follow-up: this schema's raw (`string`) `[[dynamic_fields]]`
    // rule still gets a `_dynamic_text` catch-all (`catch_all_fields` does not
    // look at the rule's own type), and `create_older_analyzer_index` with
    // `wayfinder_text_en_v1` puts that CURRENTLY-retired identity on the
    // persisted `_dynamic_text` field -- the exact shape a real pre-#388
    // all-raw-dynamic index has. Opening successfully does not prove the
    // identity actually resolves; only a write through it does.
    let (status, body) = common::request_full(
        &app,
        "POST",
        "raw-v1/update?commit=true",
        Some(&json!([{"id": "d1", "extra_s": "hello"}]).to_string()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a doc through the raw-only `_dynamic_text` catch-all must write after safe v1 \
         adoption, got: {body:?}"
    );
}

// AMENDMENT 1 (issue #388): this test used to be
// `v1_analyzed_dynamic_contract_is_safely_upgraded` and asserted a silent
// upgrade, because pre-#388 the dynamic catch-all's analyzer hadn't changed
// since v1. v3 changes `_dynamic_text`'s chain (the length bound and simple
// case folding), so a v1-marker index with an analyzed dynamic rule now
// carries stale terms on disk *and* a persisted Tantivy schema naming the
// retired `wayfinder_text_en_v1` identity nothing registers any more --
// upgrading it silently would produce silently wrong results. This test
// encodes an invariant the change deliberately invalidates, so it is amended
// to expect the reindex bail instead of a safe upgrade.
#[test]
fn v1_analyzed_dynamic_contract_now_requires_reindexing() {
    let dynamic_text = r#"
[core]
name = "dynamic-v1"
unique_key = "id"
default_field = "id"

[[fields]]
name = "id"
type = "string"
stored = true
required = true

[[dynamic_fields]]
pattern = "*_txt"
type = "text_general"
stored = true
"#;
    let dir = TempDir::new().expect("temp dir");
    create_older_analyzer_index(dir.path(), dynamic_text, "wayfinder_text_en_v1");
    let marker = dir.path().join("data/wayfinder-analyzer-contract");
    std::fs::write(&marker, "text_en_stopwords_v1").expect("write v1 analyzer contract");

    let err = common::app_with_schema(dir.path(), dynamic_text).expect_err(
        "issue #388 AMENDMENT 1: a v1-marker index with an analyzed dynamic rule must now \
         require reindexing -- v3 changes _dynamic_text's chain, so silently upgrading would \
         leave stale terms on disk and a persisted schema naming the retired \
         wayfinder_text_en_v1 identity",
    );
    let msg = format!("{err:#}");
    assert!(
        msg.to_lowercase().contains("reindex"),
        "v1-analyzed-dynamic refusal must require reindexing, got: {msg}"
    );
    assert_eq!(
        std::fs::read_to_string(&marker).expect("read analyzer contract marker"),
        "text_en_stopwords_v1",
        "a refused startup must not touch the on-disk marker"
    );
}

#[tokio::test]
async fn v1_legacy_dynamic_contract_remains_fail_closed() {
    let raw_dynamic = r#"
[core]
name = "legacy-dynamic-v1"
unique_key = "id"
default_field = "id"

[[fields]]
name = "id"
type = "string"
stored = true
required = true

[[dynamic_fields]]
pattern = "*_value"
type = "string"
stored = true
"#;
    let dir = TempDir::new().expect("temp dir");
    create_older_analyzer_index(dir.path(), raw_dynamic, "en_stem");
    let marker = dir.path().join("data/wayfinder-analyzer-contract");
    std::fs::write(&marker, "text_en_stopwords_v1_legacy_dynamic_text")
        .expect("write v1 legacy-dynamic marker");

    {
        let app = common::app_with_schema(dir.path(), raw_dynamic)
            .expect("an unused legacy dynamic catch-all remains safe to open");
        assert_eq!(
            std::fs::read_to_string(&marker).expect("read upgraded legacy marker"),
            schema::ANALYZER_CONTRACT_LEGACY_DYNAMIC_TEXT,
            "v1 legacy-dynamic state must remain distinguished after upgrade"
        );

        // Issue #388 follow-up / item E: the legacy `en_stem` dynamic path
        // (unlike `wayfinder_text_en_v1`/`_v2`, `en_stem` is Tantivy's own
        // built-in tokenizer, always registered regardless of the #388
        // retired-identity aliases) must still actually write through
        // `_dynamic_text`, not merely open. Pins the "legacy-dynamic state is
        // unaffected" finding so it is not re-litigated by a future change
        // that only checks `.is_ok()`.
        let (status, body) = common::request_full(
            &app,
            "POST",
            "legacy-dynamic-v1/update?commit=true",
            Some(&json!([{"id": "d1", "extra_value": "hello"}]).to_string()),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "a doc through the legacy en_stem `_dynamic_text` catch-all must write, got: {body:?}"
        );
    }

    let now_analyzed = raw_dynamic.replace(
        "pattern = \"*_value\"\ntype = \"string\"",
        "pattern = \"*_value\"\ntype = \"text_general\"",
    );
    let err = common::app_with_schema(dir.path(), &now_analyzed)
        .expect_err("activating an analyzed rule on the old en_stem catch-all must reindex");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("text_en") && msg.to_lowercase().contains("reindex"),
        "legacy dynamic refusal must identify text_en and reindexing, got: {msg}"
    );
}

// --- issue #388 follow-up: an all-raw-dynamic-rule schema's `_dynamic_text`
// catch-all must actually resolve after adoption, not just open -----------
//
// `catch_all_fields` (`src/schema.rs`) gives `_dynamic_text` to any schema
// with a `[[dynamic_fields]]` rule at all, regardless of the rule's own
// declared type -- including a schema whose rules are ALL non-analyzed
// (`string`/`int`). Such a schema's persisted Tantivy identity for
// `_dynamic_text` is the pre-#388 shared identity `wayfinder_text_en_v1`
// (see the `DYNAMIC_TEXT_TOKENIZER` doc comment in `src/schema.rs`: it
// coincided with static `text_en`'s v1 identity through the v2 contract
// bump), while `uses_changed_analyzed_path` is false for it -- so a v1 *or*
// v2 marker is correctly adopted without a reindex and the marker rewritten
// to v3. If the retired identity is not kept resolvable (aliased) as of the
// rewrite, every subsequent write then fails with HTTP 500 `Error getting
// tokenizer for field: _dynamic_text` -- a failure `open().is_ok()` alone
// cannot see, since it only surfaces on the next write. v1 and v2 take
// different arms in `src/core_index.rs`'s marker-upgrade match, so both are
// covered separately.

/// An all-raw dynamic-rule schema: every `[[dynamic_fields]]` rule is
/// `string` or `int`, no analyzed field anywhere (static or dynamic).
const ALL_RAW_DYNAMIC_SCHEMA_TOML: &str = r#"
[core]
name = "content"
unique_key = "id"
default_field = "id"

[[fields]]
name = "id"
type = "string"
stored = true
required = true

[[dynamic_fields]]
pattern = "*_s"
type = "string"
stored = true

[[dynamic_fields]]
pattern = "*_i"
type = "int"
stored = true
"#;

#[tokio::test]
async fn v1_marker_on_all_raw_dynamic_schema_adopts_rewrites_and_writes() {
    let dir = TempDir::new().expect("temp dir");
    create_older_analyzer_index(
        dir.path(),
        ALL_RAW_DYNAMIC_SCHEMA_TOML,
        "wayfinder_text_en_v1",
    );
    let marker = dir.path().join("data/wayfinder-analyzer-contract");
    std::fs::write(&marker, "text_en_stopwords_v1").expect("write v1 analyzer contract");

    // 1. the index adopts (open succeeds).
    let app = common::app_with_schema(dir.path(), ALL_RAW_DYNAMIC_SCHEMA_TOML).expect(
        "an all-raw-dynamic-rule index has no changed analyzer path and must remain adoptable \
         under a v1 marker",
    );

    // 2. the marker is rewritten to the current (v3) contract.
    assert_eq!(
        std::fs::read_to_string(&marker).expect("read upgraded analyzer contract"),
        schema::ANALYZER_CONTRACT,
        "safe v1 adoption of an all-raw-dynamic-rule index must persist the current analyzer \
         contract"
    );

    // 3. a subsequently POSTed and committed document returns 200 -- the
    // load-bearing assertion. (1) and (2) alone pass even when the retired
    // `wayfinder_text_en_v1` identity is not kept resolvable; only a write
    // through `_dynamic_text` proves the tokenizer manager can still resolve
    // it.
    let (status, body) = common::post_docs(
        &app,
        &json!([{"id": "d1", "extra_s": "hello", "count_i": 3}]),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a doc through the all-raw-dynamic `_dynamic_text` catch-all must write after safe v1 \
         adoption, got: {body:?}"
    );
}

#[tokio::test]
async fn v2_marker_on_all_raw_dynamic_schema_adopts_rewrites_and_writes() {
    let dir = TempDir::new().expect("temp dir");
    create_older_analyzer_index(
        dir.path(),
        ALL_RAW_DYNAMIC_SCHEMA_TOML,
        "wayfinder_text_en_v1",
    );
    let marker = dir.path().join("data/wayfinder-analyzer-contract");
    // `_dynamic_text` did not change between v1 and v2 (only static `text_en`
    // moved), so a genuinely pre-#388 all-raw-dynamic index carries the same
    // on-disk `wayfinder_text_en_v1` identity under either marker -- only the
    // marker file's own contents differ, exercising the v2 arm in
    // `src/core_index.rs` instead of the v1 one.
    std::fs::write(&marker, "text_en_porter_compatible_v2").expect("write v2 analyzer contract");

    // 1. the index adopts (open succeeds).
    let app = common::app_with_schema(dir.path(), ALL_RAW_DYNAMIC_SCHEMA_TOML).expect(
        "an all-raw-dynamic-rule index has no changed analyzer path and must remain adoptable \
         under a v2 marker",
    );

    // 2. the marker is rewritten to the current (v3) contract.
    assert_eq!(
        std::fs::read_to_string(&marker).expect("read upgraded analyzer contract"),
        schema::ANALYZER_CONTRACT,
        "safe v2 adoption of an all-raw-dynamic-rule index must persist the current analyzer \
         contract"
    );

    // 3. the load-bearing write assertion -- see the v1 sibling test above.
    let (status, body) = common::post_docs(
        &app,
        &json!([{"id": "d1", "extra_s": "hello", "count_i": 3}]),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a doc through the all-raw-dynamic `_dynamic_text` catch-all must write after safe v2 \
         adoption, got: {body:?}"
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
fn legacy_dynamic_text_identity_cannot_be_adopted_then_reused_for_analyzed_rules() {
    // A pre-#51 raw-only dynamic schema is safe to adopt, even though its
    // unused `_dynamic_text` catch-all still names Tantivy's old `en_stem`
    // tokenizer. That adoption must not bless the old identity with a v1
    // marker: a later compatible rule edit that begins using the catch-all
    // must require a reindex.
    let raw_only = r#"
[core]
name = "legacy-raw-dynamic"
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

[[dynamic_fields]]
pattern = "*_s"
type = "string"
stored = true
"#;
    let dir = TempDir::new().expect("temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, raw_only).expect("write legacy schema");
    let current = schema::load(&schema_path).expect("raw-only dynamic schema loads");
    // Issue #388: `_dynamic_text`'s current identity moves to
    // `wayfinder_dynamic_text_v3` (previously it coincided with static
    // `text_en`'s v1 identity, `wayfinder_text_en_v1`). This test simulates a
    // truly pre-#51 index, whose `_dynamic_text` catch-all used Tantivy's bare
    // `en_stem` before either identity existed, so the replace target must
    // track whatever the CURRENT identity is.
    let legacy_schema_json = serde_json::to_string(&current.tantivy_schema)
        .expect("serialize current Tantivy schema")
        .replace("wayfinder_dynamic_text_v3", "en_stem");
    assert!(
        legacy_schema_json.contains("_dynamic_text")
            && legacy_schema_json.contains("en_stem")
            && !legacy_schema_json.contains("wayfinder_dynamic_text_v3"),
        "test setup: materialize the old `_dynamic_text` tokenizer identity"
    );
    let materialize_legacy = |root: &std::path::Path| {
        std::fs::write(root.join("schema.toml"), raw_only).expect("write legacy schema");
        let data_dir = root.join("data");
        std::fs::create_dir_all(&data_dir).expect("create legacy data dir");
        let legacy_index = Index::builder()
            .schema(
                serde_json::from_str(&legacy_schema_json).expect("legacy Tantivy schema parses"),
            )
            .create_in_dir(&data_dir)
            .expect("create legacy Tantivy index");
        std::fs::write(schema::snapshot_path(&data_dir), raw_only).expect("write legacy snapshot");
        drop(legacy_index);
    };
    materialize_legacy(dir.path());
    let marker = dir.path().join("data/wayfinder-analyzer-contract");
    {
        let _app = common::app_with_schema(dir.path(), raw_only)
            .expect("an unused legacy dynamic-text catch-all is safe to adopt");
    }
    assert!(
        marker.is_file(),
        "test setup: safe adoption must write its analyzer-contract marker"
    );

    let future_dir = TempDir::new().expect("future temp dir");
    materialize_legacy(future_dir.path());
    std::fs::copy(
        marker,
        future_dir.path().join("data/wayfinder-analyzer-contract"),
    )
    .expect("copy legacy-dynamic analyzer marker");

    let now_analyzed = raw_only.replace(
        r#"pattern = "*_s"
type = "string""#,
        r#"pattern = "*_s"
type = "text_general""#,
    );
    assert_ne!(
        now_analyzed, raw_only,
        "test setup: the compatible dynamic rule edit must begin using `_dynamic_text`"
    );
    let err = common::app_with_schema(future_dir.path(), &now_analyzed).expect_err(
        "a marker written during raw-only adoption must not bless legacy _dynamic_text data",
    );
    assert!(
        format!("{err:#}").to_lowercase().contains("reindex"),
        "legacy _dynamic_text identity must refuse startup with a reindex error, got: {err:#}"
    );
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
fn custom_field_type_cannot_shadow_the_builtin_text_en_preset() {
    let toml = format!(
        r#"{FULL_SCHEMA_TOML}
[[field_types]]
name = "text_en"
tokenizer = "simple"
"#
    );
    let (_dir, path) = write_schema(&toml);
    let err = schema::load(&path)
        .expect_err("a custom field type must not shadow the built-in text_en preset");
    assert!(
        format!("{err:#}").contains("text_en"),
        "built-in-preset shadowing error must identify text_en: {err:#}"
    );
}

#[test]
fn custom_field_type_cannot_use_an_internal_text_en_tokenizer_identity() {
    // Issue #388: static `text_en`/`text_general` and the `_dynamic_text`
    // catch-all all move to new v3 tokenizer identities
    // (`wayfinder_text_en_v3`, `wayfinder_text_general_v3`,
    // `wayfinder_dynamic_text_v3`), because a tokenizer name is the on-disk
    // analyzer identity. Old and new identities must ALL stay reserved so a
    // custom `[[field_types]]` entry can never shadow one -- including
    // `wayfinder_text_general_v3`, which is new territory: pre-#388,
    // `text_general` resolved to Tantivy's own `default` tokenizer, not a
    // Wayfinder-owned (and therefore reservable) identity at all.
    for name in [
        "wayfinder_text_en_v1",
        "wayfinder_text_en_v2",
        "wayfinder_text_en_v3",
        "wayfinder_text_general_v3",
        "wayfinder_dynamic_text_v3",
    ] {
        let toml = format!(
            r#"{FULL_SCHEMA_TOML}
[[field_types]]
name = "{name}"
tokenizer = "simple"
"#
        );
        let (_dir, path) = write_schema(&toml);
        let err = schema::load(&path)
            .expect_err("a custom field type must not overwrite an internal text_en tokenizer");
        assert!(
            format!("{err:#}").contains(name),
            "reserved tokenizer-name error must identify `{name}`: {err:#}"
        );
    }
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

// --- duplicate [[field_types]] names (issue #160, found by the #156 round-2
// reviewer) -------------------------------------------------------------------

/// `resolve_type` picks a `[[field_types]]` match with `.find(|ft| ft.name ==
/// type_)`, which returns the *first* match in declaration order — so two
/// entries sharing a `name` silently make the second one dead code. That is a
/// config error worth failing on: the schema author who wrote the second
/// block clearly intended it to take effect. This is the root cause behind
/// `GET /wayfinder/{core}/schema/fieldtypes` emitting a duplicated name twice in
/// its `fieldTypes` array (issue #156, round 2).
#[test]
fn duplicate_field_type_names_are_rejected_at_load_time() {
    let toml = format!(
        r#"{FULL_SCHEMA_TOML}
[[field_types]]
name = "text_custom_dup"
tokenizer = "simple"
[[field_types.filters]]
kind = "lowercase"

[[field_types]]
name = "text_custom_dup"
tokenizer = "simple"
[[field_types.filters]]
kind = "stopwords"
language = "english"
"#
    );
    let (_dir, path) = write_schema(&toml);
    let err = schema::load(&path)
        .expect_err("two [[field_types]] entries sharing a name must be rejected");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("text_custom_dup"),
        "duplicate-field-type error must name the duplicated type, got: {msg}"
    );
}

/// Mutation guard for the duplicate check itself. The test above pins only
/// the easiest shape -- two identically-configured entries, adjacent -- which
/// two wrong implementations survive: an adjacent-only `windows(2)` scan, and
/// a set keyed on `(name, tokenizer)` rather than `name` alone. This case
/// separates the two duplicates with an unrelated entry *and* gives them
/// different tokenizers, so it fails under both. `resolve_type` still picks
/// the first match by name regardless of how the later entry is configured,
/// so both of these are real duplicates.
#[test]
fn duplicate_field_type_names_are_rejected_when_separated_and_differently_configured() {
    let toml = format!(
        r#"{FULL_SCHEMA_TOML}
[[field_types]]
name = "text_custom_dup"
tokenizer = "simple"

[[field_types]]
name = "text_custom_other"
tokenizer = "simple"

[[field_types]]
name = "text_custom_dup"
tokenizer = "whitespace"
"#
    );
    let (_dir, path) = write_schema(&toml);
    let err = schema::load(&path).expect_err(
        "two non-adjacent [[field_types]] entries sharing a name must be rejected even when \
         they are configured differently",
    );
    let msg = format!("{err:#}");
    // `duplicate` is load-bearing here, not decoration: `whitespace` is not a
    // supported tokenizer, so an implementation that keyed the duplicate check
    // on `(name, tokenizer)` would let this schema through the guard and then
    // fail later in `build_analyzer` with a message that also happens to name
    // `text_custom_dup`. Asserting the *duplicate* error is what distinguishes
    // the two.
    assert!(
        msg.contains("text_custom_dup") && msg.contains("duplicate"),
        "load must fail with the duplicate-field-type error naming the duplicated type, got: {msg}"
    );
}

/// The guard must not over-reject: distinct `[[field_types]]` names are
/// unrelated to each other and the schema must still load.
#[test]
fn distinct_field_type_names_still_load() {
    let toml = format!(
        r#"{FULL_SCHEMA_TOML}
[[field_types]]
name = "text_custom_a"
tokenizer = "simple"

[[field_types]]
name = "text_custom_b"
tokenizer = "simple"
"#
    );
    let (_dir, path) = write_schema(&toml);
    schema::load(&path).expect("schema with distinct field type names must load");
}

/// `resolve_type` matches `[[field_types]]` names with plain `==` (no
/// lowercasing), so duplicate detection must be exactly as case-sensitive as
/// resolution itself: two names differing only in case are not duplicates and
/// must not be rejected by the new guard. If this ever starts failing because
/// duplicate detection normalises case, that is a deliberate widening beyond
/// what `resolve_type` actually does and needs its own decision, not a silent
/// side effect of this guard.
#[test]
fn field_type_names_differing_only_in_case_are_not_duplicates() {
    let toml = format!(
        r#"{FULL_SCHEMA_TOML}
[[field_types]]
name = "text_Custom_dup"
tokenizer = "simple"

[[field_types]]
name = "text_custom_dup"
tokenizer = "simple"
"#
    );
    let (_dir, path) = write_schema(&toml);
    schema::load(&path)
        .expect("field type names differing only in case must not be treated as duplicates");
}

// --- every built-in field type name is reserved (issue #170) ----------------

/// The names that must be rejected as `[[field_types]]` names no matter what
/// `builtin_type_names()` happens to return. Spelled out here so the
/// exhaustive test below cannot go vacuously green if that function ever
/// returns an empty (or shrunken) list -- which is exactly the mutation the
/// guard for this issue is most likely to be broken by. Kept small and
/// deliberately spanning all four resolution shapes `resolve_type` has:
/// `Str`, `Text` (a fixed preset, a `LANGUAGES`-derived preset, and the
/// dedicated `text_en` one), `I64`, `F64`, and `Date`.
const MUST_BE_RESERVED_TYPE_NAMES: &[&str] = &[
    "string",
    "keyword",
    "text_general",
    "text_en",
    "int",
    "long",
    "float",
    "double",
    "date",
    // #331 added `location`; #334 added `location_rpt`. Both resolve to the
    // shared two-column encoding, and both must be reserved like the rest.
    "location",
    "location_rpt",
    "text_de",
    "text_fr",
    "text_ta",
    // #331 added `location` as a built-in `resolve_type` arm, so it joins the
    // reserved set like every other built-in (#170).
    "location",
];

/// Was `custom_field_type_can_silently_shadow_a_non_text_en_builtin`: a
/// characterization test that pinned the *pre*-#170 behaviour, in which
/// `resolve_type`'s "custom chains before built-in match arms" order let a
/// `[[field_types]]` entry named `double` silently retype every
/// `type = "double"` field from an F64 numeric field into an analyzed text
/// field (verified empirically: the schema loaded clean and
/// `wf.tokenize("double", "Hello World")` returned the custom `simple`
/// chain's terms), breaking range queries and sorting with no error anywhere.
///
/// Issue #170 resolves that by rejecting the shadowing outright, consistent
/// with the codebase's one precedent (`text_en`, reserved since #51) rather
/// than blessing it as an override mechanism. The old assertion is kept here
/// inverted rather than deleted so the history of the decision stays in the
/// suite.
#[test]
fn formerly_silent_shadowing_of_a_non_text_en_builtin_is_now_rejected() {
    let toml = format!(
        r#"{FULL_SCHEMA_TOML}
[[field_types]]
name = "double"
tokenizer = "simple"
"#
    );
    let (_dir, path) = write_schema(&toml);
    let err = schema::load(&path)
        .expect_err("a custom field type must not shadow the built-in `double` numeric type");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("double") && msg.contains("reserved"),
        "shadowing a built-in must fail with the reserved-field-type error naming the built-in, \
         got: {msg}"
    );
}

/// The `text_en` reservation was a one-name special case; every other built-in
/// `resolve_type` accepts is shadowable in exactly the same way and must be
/// reserved in exactly the same way. Driving this off `builtin_type_names()`
/// keeps the guard and `GET /wayfinder/{core}/schema/fieldtypes` from drifting
/// apart when a stemmer language is added -- that list's *contents* are pinned
/// independently against a real Solr trace in `tests/schema_fieldtypes.rs`,
/// so this test only has to assert the reservation covers all of it, and
/// `MUST_BE_RESERVED_TYPE_NAMES` keeps the loop from being vacuous.
#[test]
fn every_builtin_field_type_name_is_reserved() {
    let builtins = schema::builtin_type_names();
    for name in MUST_BE_RESERVED_TYPE_NAMES {
        assert!(
            builtins.contains(&(*name).to_string()),
            "`{name}` must still be a built-in type name; `builtin_type_names()` returned \
             {builtins:?}"
        );
    }

    for name in &builtins {
        let toml = format!(
            r#"{FULL_SCHEMA_TOML}
[[field_types]]
name = "{name}"
tokenizer = "simple"
"#
        );
        let (_dir, path) = write_schema(&toml);
        match schema::load(&path) {
            Ok(_) => {
                panic!("a custom field type named after the built-in `{name}` must be rejected")
            }
            Err(err) => {
                let msg = format!("{err:#}");
                assert!(
                    msg.contains(name.as_str()) && msg.contains("reserved"),
                    "shadowing built-in `{name}` must fail with the reserved-field-type error \
                     naming it, got: {msg}"
                );
            }
        }
    }
}

/// Expiring guard on the *other* direction of the reservation, the one
/// `every_builtin_field_type_name_is_reserved` cannot see. That test runs
/// `builtin_type_names()` -> reserved; nothing runs `resolve_type` ->
/// `builtin_type_names()`. `NON_LANGUAGE_BUILTIN_TYPES` is a hand-maintained
/// copy of `resolve_type`'s non-language match arms, so adding an arm without
/// extending the list reintroduces #170's exact bug -- silently, with the whole
/// suite green. Adding a `boolean` arm is actively contemplated
/// (`tests/search_api_preset.rs`: "Wayfinder has no boolean type").
///
/// So this asserts these names are *still unresolvable*: no built-in arm, no
/// `LANGUAGES` entry, hence nothing to reserve. The moment one of them becomes
/// resolvable this test fails and names the list to extend, and then it should
/// be deleted (or the name moved into `MUST_BE_RESERVED_TYPE_NAMES` above)
/// rather than relaxed -- it exists to expire.
///
/// This is a heuristic net, not a proof: a test cannot enumerate `resolve_type`'s
/// `match` arms, so no fixed list of names can close the hole -- an arm for a name
/// nobody thought of here still slips through. The list is therefore stocked with
/// the names most likely to be added next: `boolean`/`bool` (actively contemplated)
/// and the Solr point-type aliases `pint`/`plong`/`pfloat`/`pdouble`/`pdate`, which
/// are modern Solr's names for types Wayfinder already has and so are the obvious
/// wire-compat aliases to add (`pdate` already shows up as a real trace name in
/// `tests/schema_fieldtypes.rs`). A passing guard means these names are still
/// unresolved, not that the reservation list is complete.
#[test]
fn type_names_absent_from_the_reservation_list_are_still_unresolvable() {
    for name in [
        "boolean", "bool", "binary", "pint", "plong", "pfloat", "pdouble", "pdate",
    ] {
        assert!(
            !schema::builtin_type_names().contains(&name.to_string()),
            "`{name}` is not expected in `builtin_type_names()`; if it was added there, this \
             guard has expired -- move it into MUST_BE_RESERVED_TYPE_NAMES instead"
        );
        let toml = format!(
            r#"{FULL_SCHEMA_TOML}
[[fields]]
name = "unresolvable_probe"
type = "{name}"
stored = true
"#
        );
        let (_dir, path) = write_schema(&toml);
        let msg = match schema::load(&path) {
            Ok(_) => panic!(
                "`{name}` now resolves to a built-in field type, so `resolve_type` gained an arm \
                 for it. `NON_LANGUAGE_BUILTIN_TYPES` in src/schema.rs is a separate hand-written \
                 copy of those arms and does not list `{name}`, so a custom [[field_types]] chain \
                 named `{name}` can now silently shadow it -- issue #170's bug. Add `{name}` to \
                 NON_LANGUAGE_BUILTIN_TYPES and to MUST_BE_RESERVED_TYPE_NAMES, then drop it from \
                 this guard."
            ),
            Err(err) => format!("{err:#}"),
        };
        assert!(
            msg.contains(name) && msg.contains("unsupported field type"),
            "`{name}` must still be rejected as an unsupported field type, got: {msg}"
        );
    }
}

/// #331: a `location` field resolves (#292 sizing report) and is encoded as
/// two synthetic f64 fast columns `<name>__lat`/`<name>__lon`, not one Tantivy
/// field. The user-facing name is reachable only through `location_fields`,
/// and never becomes a `field()`-resolvable Tantivy field, so it cannot leak
/// into the name-based query/fl paths.
#[test]
fn location_field_is_two_synthetic_f64_columns() {
    let toml = format!(
        r#"{FULL_SCHEMA_TOML}
[[fields]]
name = "loc"
type = "location"
stored = true
"#
    );
    let (_dir, path) = write_schema(&toml);
    let wf = schema::load(&path).expect("a `location` field must resolve");

    // The two synthetic columns exist in the Tantivy schema.
    assert_eq!(
        wf.tantivy_schema
            .get_field("loc__lat")
            .map(|f| f.field_id())
            .ok(),
        wf.location_fields("loc").map(|(lat, _)| lat.field_id()),
        "loc__lat must be the first column `location_fields` returns"
    );
    assert_eq!(
        wf.tantivy_schema
            .get_field("loc__lon")
            .map(|f| f.field_id())
            .ok(),
        wf.location_fields("loc").map(|(_, lon)| lon.field_id()),
        "loc__lon must be the second column `location_fields` returns"
    );

    // The user-facing name is *not* itself a Tantivy field.
    assert!(
        wf.field("loc").is_none(),
        "a `location` field must not be a single name-resolvable Tantivy field"
    );
    assert_eq!(
        wf.value_kind("loc"),
        Some(schema::ValueKind::Location),
        "a declared `location` field resolves to ValueKind::Location"
    );
}

/// The guard must not over-reject. `resolve_type` matches built-in names with
/// plain `==` in a `match`, so a name that merely *resembles* a built-in is
/// not one and stays a legitimate custom chain name: a case variant, a
/// prefix/suffix extension, and `text_<code>` for a language Tantivy has no
/// stemmer for (so `resolve_type`'s generic branch rejects it, and it is not
/// in `builtin_type_names()` either -- `tests/schema_fieldtypes.rs` pins
/// `text_ja` as unsupported). If this starts failing, the reservation was
/// widened past what `resolve_type` actually resolves.
#[test]
fn custom_field_type_names_that_only_resemble_a_builtin_still_load() {
    for name in [
        "Double",
        "double_custom",
        "custom_double",
        "text_ja",
        "text_zz",
    ] {
        let toml = format!(
            r#"{FULL_SCHEMA_TOML}
[[fields]]
name = "resembling"
type = "{name}"
stored = true

[[field_types]]
name = "{name}"
tokenizer = "simple"
[[field_types.filters]]
kind = "lowercase"
"#
        );
        let (_dir, path) = write_schema(&toml);
        let wf = schema::load(&path).unwrap_or_else(|err| {
            panic!("`{name}` is not a built-in type name and must remain usable: {err:#}")
        });
        assert_eq!(
            wf.tokenize(name, "Hello World"),
            Some(vec!["hello".to_string(), "world".to_string()]),
            "custom chain `{name}` must still resolve to its own analyzer"
        );
    }
}

// --- dynamic catch-all field names are reserved (issue #194) ----------------

/// `_dynamic` and `_dynamic_text` are implicitly created whenever the schema
/// declares a `[[dynamic_fields]]` rule. Declaring either name as a static
/// field must return an ordinary load error instead of reaching Tantivy's
/// duplicate-field panic. Iterating both names is the mutation guard: removing
/// either reservation makes this test panic or unexpectedly load that schema.
#[test]
fn dynamic_catch_all_field_names_are_reserved_when_dynamic_rules_exist() {
    for name in [schema::DYNAMIC_FIELD, schema::DYNAMIC_TEXT_FIELD] {
        let toml = format!(
            r#"{FULL_SCHEMA_TOML}
[[fields]]
name = "{name}"
type = "string"
stored = true
"#
        );
        let (_dir, path) = write_schema(&toml);
        let err = schema::load(&path)
            .expect_err("a static field may not use a dynamic catch-all field name");
        let msg = format!("{err:#}");
        assert!(
            msg.contains(name) && msg.contains("reserved"),
            "load must fail with a reserved-name error naming `{name}`, got: {msg}"
        );
    }
}

/// Without dynamic rules Wayfinder creates no catch-all fields, so these names
/// cannot collide and retain their pre-existing meaning as ordinary static
/// fields. This keeps the panic fix scoped to schemas that can trigger it.
#[test]
fn dynamic_catch_all_names_still_load_as_static_fields_without_dynamic_rules() {
    for name in [schema::DYNAMIC_FIELD, schema::DYNAMIC_TEXT_FIELD] {
        let toml = format!(
            r#"{}
[[fields]]
name = "{name}"
type = "string"
stored = true
"#,
            schema_without_dynamic_fields()
        );
        let (_dir, path) = write_schema(&toml);
        let wf = schema::load(&path)
            .expect("a catch-all name without dynamic rules must remain a valid static field");
        assert!(wf.field(name).is_some(), "`{name}` must resolve normally");
    }
}

// --- duplicate [[fields]] names (issue #173) ---------------------------------
//
// Corrected premise. Issue #173 states that two `[[fields]]` entries sharing a
// `name` "each call `builder.add_*_field`, so two Tantivy fields are created
// under one name", with `field_handles` last-wins leaving the first one
// orphaned. Verified against tantivy-0.26.1: `SchemaBuilder::add_field`
// (`src/schema/schema.rs:202`) *panics* -- "Field already exists in schema
// dup" -- so no orphan is ever created and `field_handles` never gets a second
// insert. The real defect is therefore worse in a different way: an operator
// typo in schema.toml crashes the process from inside a dependency instead of
// producing the ordinary `anyhow` schema-load error every other schema mistake
// in this file produces. These tests pin the clean error; a panic fails them.

/// The base case: two adjacent, identically-configured entries.
#[test]
fn duplicate_field_names_are_rejected_at_load_time() {
    let toml = format!(
        r#"{FULL_SCHEMA_TOML}
[[fields]]
name = "dup_field"
type = "int"
stored = true

[[fields]]
name = "dup_field"
type = "int"
stored = true
"#
    );
    let (_dir, path) = write_schema(&toml);
    let err =
        schema::load(&path).expect_err("two [[fields]] entries sharing a name must be rejected");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("dup_field") && msg.contains("duplicate"),
        "load must fail with the duplicate-field error naming the duplicated field, got: {msg}"
    );
}

/// Mutation guard, per the shape #160's round-1 review caught: the base case
/// above is survived by an adjacent-only `windows(2)` scan over the declared
/// fields. Here the two duplicates are separated by an unrelated entry, and
/// both are still identically configured so nothing but adjacency differs.
#[test]
fn duplicate_field_names_are_rejected_when_separated_by_other_fields() {
    let toml = format!(
        r#"{FULL_SCHEMA_TOML}
[[fields]]
name = "dup_field"
type = "int"
stored = true

[[fields]]
name = "spacer_field"
type = "int"
stored = true

[[fields]]
name = "dup_field"
type = "int"
stored = true
"#
    );
    let (_dir, path) = write_schema(&toml);
    let err = schema::load(&path)
        .expect_err("two non-adjacent [[fields]] entries sharing a name must be rejected");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("dup_field") && msg.contains("duplicate"),
        "load must fail with the duplicate-field error naming the duplicated field, got: {msg}"
    );
}

/// Mutation guard for the other implementation #160's review found plausible:
/// `FieldConfig` derives `PartialEq`, so a dedup keyed on the whole struct (or
/// on `(name, type_)`) looks correct and survives the two tests above. A
/// second declaration of `dup_field` is a duplicate *name* regardless of how
/// it is configured -- and it is the more dangerous case, since the two
/// declarations disagree about what the field is. Both types here are valid on
/// their own, so the only error this schema can produce is the duplicate one:
/// there is no later failure that could stand in for the guard.
#[test]
fn duplicate_field_names_are_rejected_when_differently_configured() {
    let toml = format!(
        r#"{FULL_SCHEMA_TOML}
[[fields]]
name = "dup_field"
type = "int"
stored = true
fast = true

[[fields]]
name = "dup_field"
type = "text_en"
stored = false
multi_valued = true
"#
    );
    let (_dir, path) = write_schema(&toml);
    let err = schema::load(&path).expect_err(
        "two [[fields]] entries sharing a name must be rejected even when configured differently",
    );
    let msg = format!("{err:#}");
    assert!(
        msg.contains("dup_field") && msg.contains("duplicate"),
        "load must fail with the duplicate-field error naming the duplicated field, got: {msg}"
    );
}

/// The guard must not over-reject: distinct field names are unrelated and the
/// schema must still load, with every one of them resolvable.
#[test]
fn distinct_field_names_still_load() {
    let toml = format!(
        r#"{FULL_SCHEMA_TOML}
[[fields]]
name = "extra_a"
type = "int"
stored = true

[[fields]]
name = "extra_b"
type = "int"
stored = true
"#
    );
    let (_dir, path) = write_schema(&toml);
    let wf = schema::load(&path).expect("schema with distinct field names must load");
    for name in [
        "id", "title", "body", "views", "rating", "created", "extra_a", "extra_b",
    ] {
        assert!(
            wf.field(name).is_some(),
            "`{name}` must resolve to a Tantivy field"
        );
    }
}

/// Tantivy field names are case-sensitive (`add_field` rejects only an exact
/// repeat, as the panic above shows), so `Title` and `title` are two distinct
/// fields and must both load -- the same case-sensitivity decision #160 made
/// for `[[field_types]]` names. If this starts failing because duplicate
/// detection normalises case, that is a deliberate widening needing its own
/// decision, not a side effect of this guard.
#[test]
fn field_names_differing_only_in_case_are_not_duplicates() {
    let toml = format!(
        r#"{FULL_SCHEMA_TOML}
[[fields]]
name = "Title"
type = "text_en"
stored = true
"#
    );
    let (_dir, path) = write_schema(&toml);
    let wf = schema::load(&path)
        .expect("field names differing only in case must not be treated as duplicates");
    assert!(
        wf.field("Title").is_some() && wf.field("title").is_some(),
        "both case variants must resolve to their own Tantivy field"
    );
}

// --- duplicate [[dynamic_fields]] patterns (issue #173) ---------------------
//
// Unlike the static case, this one really is silent: dynamic rules share the
// two catch-all JSON fields, so nothing panics, and `match_dynamic`'s
// `max_by_key(|d| d.pattern.len())` returns the *last* rule among equal-length
// patterns -- verified empirically, two `*_i` rules typed `int` then `text_en`
// resolve `count_i` to `text_en`. The first declaration is silently dead, and
// with differing types the two rules even disagree about which catch-all field
// (`_dynamic` vs `_dynamic_text`) the values belong in.

/// The base case: two adjacent, identically-configured rules.
#[test]
fn duplicate_dynamic_field_patterns_are_rejected_at_load_time() {
    let toml = format!(
        r#"{FULL_SCHEMA_TOML}
[[dynamic_fields]]
pattern = "*_dup"
type = "int"
stored = true

[[dynamic_fields]]
pattern = "*_dup"
type = "int"
stored = true
"#
    );
    let (_dir, path) = write_schema(&toml);
    let err = schema::load(&path)
        .expect_err("two [[dynamic_fields]] entries sharing a pattern must be rejected");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("*_dup") && msg.contains("duplicate"),
        "load must fail with the duplicate-pattern error naming the duplicated pattern, got: {msg}"
    );
}

/// Mutation guard: an adjacent-only scan survives the base case. Separated by
/// an unrelated rule, identically configured otherwise.
#[test]
fn duplicate_dynamic_field_patterns_are_rejected_when_separated_by_other_rules() {
    let toml = format!(
        r#"{FULL_SCHEMA_TOML}
[[dynamic_fields]]
pattern = "*_dup"
type = "int"
stored = true

[[dynamic_fields]]
pattern = "*_spacer"
type = "int"
stored = true

[[dynamic_fields]]
pattern = "*_dup"
type = "int"
stored = true
"#
    );
    let (_dir, path) = write_schema(&toml);
    let err = schema::load(&path)
        .expect_err("two non-adjacent [[dynamic_fields]] rules sharing a pattern must be rejected");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("*_dup") && msg.contains("duplicate"),
        "load must fail with the duplicate-pattern error naming the duplicated pattern, got: {msg}"
    );
}

/// Mutation guard: a dedup keyed on more than the pattern survives the two
/// tests above. Two rules with the same pattern and different types are the
/// worst case, not an exempt one -- `match_dynamic` still picks exactly one of
/// them, and here they disagree about the catch-all field
/// (`int` -> `_dynamic`, `text_en` -> `_dynamic_text`). Both types are valid,
/// so the duplicate error is the only error this schema can produce.
#[test]
fn duplicate_dynamic_field_patterns_are_rejected_when_differently_configured() {
    let toml = format!(
        r#"{FULL_SCHEMA_TOML}
[[dynamic_fields]]
pattern = "*_dup"
type = "int"
stored = true
fast = true

[[dynamic_fields]]
pattern = "*_dup"
type = "text_en"
stored = false
multi_valued = true
"#
    );
    let (_dir, path) = write_schema(&toml);
    let err = schema::load(&path).expect_err(
        "two [[dynamic_fields]] rules sharing a pattern must be rejected even when configured \
         differently",
    );
    let msg = format!("{err:#}");
    assert!(
        msg.contains("*_dup") && msg.contains("duplicate"),
        "load must fail with the duplicate-pattern error naming the duplicated pattern, got: {msg}"
    );
}

/// The guard must reject *exact* duplicate patterns only. Overlapping-but-
/// distinct globs are ordinary Solr configuration -- `tm_*` and `tm_X3b_*`
/// both match `tm_X3b_en_body`, and Drupal's Search API generates exactly that
/// shape -- and `match_dynamic`'s longest-pattern-wins rule exists precisely
/// to resolve the overlap. A guard that rejected overlap, or that keyed on
/// something coarser than the pattern string, breaks a legitimate schema.
#[test]
fn overlapping_dynamic_field_patterns_still_load() {
    let toml = format!(
        r#"{FULL_SCHEMA_TOML}
[[dynamic_fields]]
pattern = "tm_*"
type = "text_en"
stored = true

[[dynamic_fields]]
pattern = "tm_X3b_*"
type = "text_en"
stored = true

[[dynamic_fields]]
pattern = "*"
type = "string"
stored = true
"#
    );
    let (_dir, path) = write_schema(&toml);
    let wf =
        schema::load(&path).expect("overlapping but distinct dynamic patterns must still load");

    // All three rules are live, resolved by longest-pattern-wins.
    assert_eq!(
        wf.match_dynamic("tm_X3b_en_body")
            .map(|d| d.pattern.as_str()),
        Some("tm_X3b_*"),
        "the longer overlapping pattern must win"
    );
    assert_eq!(
        wf.match_dynamic("tm_title").map(|d| d.pattern.as_str()),
        Some("tm_*"),
        "the shorter overlapping pattern must still match what the longer one does not"
    );
    assert_eq!(
        wf.match_dynamic("anything").map(|d| d.pattern.as_str()),
        Some("*"),
        "the catch-all pattern must still match"
    );
    // And the pre-existing overlapping pair in the base schema is untouched.
    assert_eq!(
        wf.match_dynamic("blurb_txt_i").map(|d| d.pattern.as_str()),
        Some("*_txt_i"),
        "`*_i` and `*_txt_i` overlap and must both remain live"
    );
}

/// `glob_matches` compares patterns and names byte-for-byte, so `*_i` and
/// `*_I` match different field names and are not duplicates. Same
/// case-sensitivity decision as everywhere else in this file; a failure here
/// means the guard normalises case, which is a separate decision.
#[test]
fn dynamic_field_patterns_differing_only_in_case_are_not_duplicates() {
    let toml = format!(
        r#"{FULL_SCHEMA_TOML}
[[dynamic_fields]]
pattern = "*_I"
type = "text_en"
stored = true
"#
    );
    let (_dir, path) = write_schema(&toml);
    let wf = schema::load(&path)
        .expect("dynamic patterns differing only in case must not be treated as duplicates");
    assert_eq!(
        wf.match_dynamic("count_I").map(|d| d.type_.as_str()),
        Some("text_en"),
        "the upper-case pattern must be live"
    );
    assert_eq!(
        wf.match_dynamic("count_i").map(|d| d.type_.as_str()),
        Some("int"),
        "the lower-case pattern must be unaffected"
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

/// #331/#334: a `location` and a `location_rpt` field both resolve
/// (`ValueKind::Location`) and encode as two synthetic f64 fast columns
/// (`<field>__lat`/`<field>__lon`), never as one name-resolvable Tantivy field.
/// `geodist()` (#331) and `facet.heatmap` (#334) read the columns back through
/// `location_fields`; this test pins that contract so a refactor that collapses
/// the pair back into one field (or drops a column) is caught.
#[test]
fn location_and_location_rpt_fields_are_two_synthetic_f64_columns() {
    let toml = r#"
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
name = "body"
type = "text_en"

[[fields]]
name = "loc"
type = "location"

[[fields]]
name = "rpt"
type = "location_rpt"
"#;
    let (_dir, path) = write_schema(toml);
    let wf = schema::load(&path).expect("`location` and `location_rpt` fields must resolve");

    for name in ["loc", "rpt"] {
        // Two synthetic columns, not one field.
        let (lat, lon) = wf
            .location_fields(name)
            .unwrap_or_else(|| panic!("`{name}` must have a `(__lat, __lon)` column pair"));
        assert_ne!(
            lat, lon,
            "`{name}__lat` and `{name}__lon` must be distinct columns"
        );
        // The synthetic columns and the user-facing name are never reachable
        // through the single-field resolver, so they cannot leak into `fl`,
        // `qf`, sort, or any name-based path.
        assert_eq!(
            wf.field(name),
            None,
            "`{name}` must not be a single name-resolvable Tantivy field"
        );
        assert_eq!(
            wf.field(&format!("{name}__lat")),
            None,
            "the synthetic `{name}__lat` column must not leak into the resolver"
        );
        assert_eq!(
            wf.value_kind(name),
            Some(schema::ValueKind::Location),
            "`{name}` must resolve to ValueKind::Location"
        );
    }

    // A name with no such field is `None`, not a panic.
    assert!(wf.location_fields("nope").is_none());
}
