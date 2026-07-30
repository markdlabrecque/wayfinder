//! TOML schema file -> Tantivy `Schema` (PRD §3).
//!
//! Covers the full v1 schema format: static fields (`string`, `keyword`,
//! `text_*` presets, `int`/`long`/`float`/`double`, `date`), `[[dynamic_fields]]`
//! glob patterns, `[[copy_fields]]`, `[[field_types]]` custom analyzer chains,
//! and the startup compatibility check against the schema an existing index was
//! built with.
//!
//! Out of scope (PRD §3): runtime schema mutation, `schema.xml`, per-field
//! similarity.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tantivy::schema::{
    DateOptions, DateTimePrecision, Field, IndexRecordOption, JsonObjectOptions, NumericOptions,
    Schema, TextFieldIndexing, TextOptions,
};
use tantivy::tokenizer::{
    Language, LowerCaser, RemoveLongFilter, SimpleTokenizer, Stemmer, StopWordFilter, TextAnalyzer,
    TokenizerManager,
};

/// Catch-all Tantivy field holding every document field that resolved through a
/// `[[dynamic_fields]]` pattern to a non-analyzed type (string/keyword/numeric/
/// date). Tantivy schemas are fixed at index creation, so dynamic fields cannot
/// each become their own field the way Solr's do; a JSON field gives per-path
/// typed indexing instead, and `_dynamic.<name>` is the query path.
pub const DYNAMIC_FIELD: &str = "_dynamic";
/// As `DYNAMIC_FIELD`, but for patterns resolving to an analyzed text type.
/// Split in two because a JSON field carries a single tokenizer for all of its
/// string values, and a `*_s` string pattern must not be stemmed like a
/// `*_txt` one.
pub const DYNAMIC_TEXT_FIELD: &str = "_dynamic_text";

/// The ISO-639-1 code -> Tantivy stemmer language table. PRD open question 5:
/// ship every language Tantivy's stemmer set gives cheaply, which is all of
/// them — each preset is a tokenizer + three filters.
const LANGUAGES: &[(&str, Language)] = &[
    ("ar", Language::Arabic),
    ("da", Language::Danish),
    ("nl", Language::Dutch),
    ("en", Language::English),
    ("fi", Language::Finnish),
    ("fr", Language::French),
    ("de", Language::German),
    ("el", Language::Greek),
    ("hu", Language::Hungarian),
    ("it", Language::Italian),
    ("no", Language::Norwegian),
    ("pt", Language::Portuguese),
    ("ro", Language::Romanian),
    ("ru", Language::Russian),
    ("es", Language::Spanish),
    ("sv", Language::Swedish),
    ("ta", Language::Tamil),
    ("tr", Language::Turkish),
];

/// Tantivy's own `default` analyzer: simple tokenizer, long tokens dropped,
/// lowercased, no stemming. Solr calls this shape `text_general`.
const TEXT_GENERAL_TOKENIZER: &str = "default";
/// Tantivy's own English analyzer — `text_general` plus an English stemmer.
/// `text_en` maps onto it rather than a hand-built equivalent so the tracer
/// bullet's captured relevance behaviour is unchanged.
const TEXT_EN_TOKENIZER: &str = "en_stem";

#[derive(Debug, Deserialize)]
struct SchemaFile {
    core: CoreConfig,
    #[serde(rename = "fields", default)]
    fields: Vec<FieldConfig>,
    #[serde(default)]
    dynamic_fields: Vec<DynamicFieldConfig>,
    #[serde(default)]
    copy_fields: Vec<CopyFieldConfig>,
    #[serde(default)]
    field_types: Vec<FieldTypeConfig>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CoreConfig {
    pub name: String,
    pub unique_key: String,
    pub default_field: String,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct FieldConfig {
    pub name: String,
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(default)]
    pub stored: bool,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub fast: bool,
    #[serde(default)]
    pub multi_valued: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct DynamicFieldConfig {
    pub pattern: String,
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(default)]
    pub stored: bool,
    #[serde(default)]
    pub fast: bool,
    #[serde(default)]
    pub multi_valued: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CopyFieldConfig {
    pub source: String,
    pub dest: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct FieldTypeConfig {
    pub name: String,
    pub tokenizer: String,
    #[serde(default)]
    pub filters: Vec<FilterConfig>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct FilterConfig {
    pub kind: String,
    #[serde(default)]
    pub language: Option<String>,
}

/// How a field's JSON values are coerced on the way in and rendered on the way
/// out. Text and string types share `Text` — both take JSON strings; they differ
/// only in analysis, which the Tantivy schema options already carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    Text,
    I64,
    F64,
    Date,
}

/// A parsed schema: the Tantivy `Schema`, the core config, the dynamic/copy
/// rules, the tokenizer manager the index must be opened with, and a lookup from
/// field name to both the Tantivy `Field` handle and the original config.
pub struct WayfinderSchema {
    pub tantivy_schema: Schema,
    pub core: CoreConfig,
    pub fields: Vec<FieldConfig>,
    pub dynamic_fields: Vec<DynamicFieldConfig>,
    pub copy_fields: Vec<CopyFieldConfig>,
    pub field_types: Vec<FieldTypeConfig>,
    pub tokenizers: TokenizerManager,
    field_handles: HashMap<String, Field>,
}

// `TokenizerManager` is not `Debug`, so derive is out; tests still need it for
// `Result::expect_err`.
impl std::fmt::Debug for WayfinderSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WayfinderSchema")
            .field("core", &self.core)
            .field("fields", &self.fields)
            .field("dynamic_fields", &self.dynamic_fields)
            .field("copy_fields", &self.copy_fields)
            .finish_non_exhaustive()
    }
}

impl WayfinderSchema {
    pub fn field(&self, name: &str) -> Option<Field> {
        self.field_handles.get(name).copied()
    }

    pub fn field_config(&self, name: &str) -> Option<&FieldConfig> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// True if `name` is a field declared in `[[fields]]`. A declared field
    /// always wins over a `[[dynamic_fields]]` pattern that would also match it,
    /// as in Solr.
    pub fn is_static(&self, name: &str) -> bool {
        self.field_config(name).is_some()
    }

    /// The `[[dynamic_fields]]` rule matching `name`, longest pattern first
    /// (Solr's rule). Pattern-only: callers must check `is_static` first.
    pub fn match_dynamic(&self, name: &str) -> Option<&DynamicFieldConfig> {
        self.dynamic_fields
            .iter()
            .filter(|d| glob_matches(&d.pattern, name))
            .max_by_key(|d| d.pattern.len())
    }

    /// Every `[[copy_fields]]` destination for `source`.
    pub fn copy_dests(&self, source: &str) -> impl Iterator<Item = &str> {
        self.copy_fields
            .iter()
            .filter(move |c| c.source == source)
            .map(|c| c.dest.as_str())
    }

    /// The value kind of a declared field.
    pub fn value_kind(&self, name: &str) -> Option<ValueKind> {
        self.field_config(name)
            .and_then(|f| value_kind_of(&f.type_, &self.field_types).ok())
    }

    /// True if `name` is an unanalyzed `string`/`keyword` field, as opposed to
    /// a tokenized `text_*`/custom-chain one — both resolve to `ValueKind::
    /// Text`, so this is the distinction issue #8's fuzzy/wildcard multi-term
    /// analysis needs (lowercase-but-do-not-stem an analyzed field's search
    /// term; leave a raw string's alone, matching Solr's `StrField` not
    /// analyzing at all). A field that is not declared (unknown, or a
    /// catch-all dynamic container) is not a raw string by this definition.
    pub fn is_raw_string(&self, name: &str) -> bool {
        self.field_config(name)
            .is_some_and(|f| matches!(f.type_.as_str(), "string" | "keyword"))
    }

    /// Which catch-all JSON field a dynamic rule's values live in.
    pub fn dynamic_target(&self, rule: &DynamicFieldConfig) -> &'static str {
        match resolve_type(&rule.type_, &self.field_types) {
            Ok(ResolvedType::Text { .. }) => DYNAMIC_TEXT_FIELD,
            _ => DYNAMIC_FIELD,
        }
    }

    /// Whether `name` can hold fast-field (docValues) values, resolved with
    /// the same static-before-dynamic precedence `is_static`/`match_dynamic`
    /// already establish for indexing (issue #66): a declared `[[fields]]`
    /// entry's own `fast` flag wins, otherwise the matching
    /// `[[dynamic_fields]]` rule's `fast` flag, otherwise `None` if `name`
    /// resolves to neither.
    pub fn resolved_fast(&self, name: &str) -> Option<bool> {
        if let Some(f) = self.field_config(name) {
            return Some(f.fast);
        }
        self.match_dynamic(name).map(|rule| rule.fast)
    }

    /// The actual Tantivy fast-field column backing `name`: itself for a
    /// static field, or the catch-all JSON path (`_dynamic[_text].<name>`,
    /// the same prefix `CoreIndex::rewrite_dynamic_fields` inserts for the
    /// query path) for a field that only matches a `[[dynamic_fields]]`
    /// pattern. `None` if `name` resolves to neither.
    pub fn resolved_fast_column(&self, name: &str) -> Option<String> {
        if self.is_static(name) {
            return Some(name.to_string());
        }
        self.match_dynamic(name)
            .map(|rule| format!("{}.{}", self.dynamic_target(rule), name))
    }

    /// The value kind backing `name`, resolved with the same precedence as
    /// `resolved_fast`: a declared field's own kind, or the kind of the
    /// `[[dynamic_fields]]` rule matching it.
    pub fn resolved_value_kind(&self, name: &str) -> Option<ValueKind> {
        if let Some(kind) = self.value_kind(name) {
            return Some(kind);
        }
        self.match_dynamic(name)
            .and_then(|rule| dynamic_value_kind(rule, &self.field_types).ok())
    }

    /// Runs `text` through the analyzer registered for field type `type_name`,
    /// returning the resulting terms. Used by the schema tests to prove each
    /// preset and custom chain actually does what it claims.
    pub fn tokenize(&self, type_name: &str, text: &str) -> Option<Vec<String>> {
        let tokenizer_name = match resolve_type(type_name, &self.field_types).ok()? {
            ResolvedType::Str => "raw".to_string(),
            ResolvedType::Text { tokenizer } => tokenizer,
            _ => return None,
        };
        let mut analyzer = self.tokenizers.get(&tokenizer_name)?;
        let mut stream = analyzer.token_stream(text);
        let mut out = Vec::new();
        while stream.advance() {
            out.push(stream.token().text.clone());
        }
        Some(out)
    }
}

/// What a schema `type = "..."` resolves to.
enum ResolvedType {
    Str,
    Text { tokenizer: String },
    I64,
    F64,
    Date,
}

fn resolve_type(type_: &str, custom: &[FieldTypeConfig]) -> Result<ResolvedType> {
    if let Some(ft) = custom.iter().find(|ft| ft.name == type_) {
        return Ok(ResolvedType::Text {
            tokenizer: ft.name.clone(),
        });
    }
    Ok(match type_ {
        "string" | "keyword" => ResolvedType::Str,
        "text_general" => ResolvedType::Text {
            tokenizer: TEXT_GENERAL_TOKENIZER.to_string(),
        },
        "text_en" => ResolvedType::Text {
            tokenizer: TEXT_EN_TOKENIZER.to_string(),
        },
        "int" | "long" => ResolvedType::I64,
        "float" | "double" => ResolvedType::F64,
        "date" => ResolvedType::Date,
        other => {
            let code = other.strip_prefix("text_").filter(|code| {
                LANGUAGES
                    .iter()
                    .any(|(lang_code, _)| lang_code == code && *code != "en")
            });
            match code {
                Some(code) => ResolvedType::Text {
                    tokenizer: format!("text_{code}"),
                },
                None => bail!("unsupported field type `{other}`"),
            }
        }
    })
}

fn value_kind_of(type_: &str, custom: &[FieldTypeConfig]) -> Result<ValueKind> {
    Ok(match resolve_type(type_, custom)? {
        ResolvedType::Str | ResolvedType::Text { .. } => ValueKind::Text,
        ResolvedType::I64 => ValueKind::I64,
        ResolvedType::F64 => ValueKind::F64,
        ResolvedType::Date => ValueKind::Date,
    })
}

/// The value kind of a dynamic rule's declared type, for coercing incoming
/// values before they go into the catch-all JSON field.
pub fn dynamic_value_kind(
    rule: &DynamicFieldConfig,
    custom: &[FieldTypeConfig],
) -> Result<ValueKind> {
    value_kind_of(&rule.type_, custom)
}

/// Solr-style glob: `*suffix`, `prefix*`, or bare `*`. Anything else is matched
/// literally. Patterns are validated by `validate_pattern` at load time, so
/// nothing else can reach here.
///
/// ponytail: prefix/suffix match rather than a glob crate — Solr's dynamic-field
/// patterns are only ever `*_suffix`, `prefix_*`, or `*`.
fn glob_matches(pattern: &str, name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    match (pattern.strip_prefix('*'), pattern.strip_suffix('*')) {
        (Some(suffix), None) => name.len() > suffix.len() && name.ends_with(suffix),
        (None, Some(prefix)) => name.len() > prefix.len() && name.starts_with(prefix),
        _ => pattern == name,
    }
}

/// The catch-all JSON fields that `rules` causes to exist in the Tantivy schema.
/// The single source of truth for that decision, shared by `parse` (which adds
/// them) and `check_compatible` (which refuses a change to the set).
fn catch_all_fields(rules: &[DynamicFieldConfig]) -> &'static [&'static str] {
    if rules.is_empty() {
        &[]
    } else {
        &[DYNAMIC_FIELD, DYNAMIC_TEXT_FIELD]
    }
}

/// Solr allows a `*` at exactly one end of a dynamic-field pattern (or a bare
/// `*`). Reject anything else at load time rather than inventing semantics Solr
/// does not have.
fn validate_pattern(pattern: &str) -> Result<()> {
    let ok = match pattern.matches('*').count() {
        0 => true,
        1 => pattern.starts_with('*') || pattern.ends_with('*'),
        _ => false,
    };
    if !ok {
        bail!(
            "dynamic field pattern `{pattern}` is not supported: use `*suffix`, `prefix*`, or `*`"
        );
    }
    Ok(())
}

/// Builds the `TextAnalyzer` for a `[[field_types]]` chain.
fn build_analyzer(ft: &FieldTypeConfig) -> Result<TextAnalyzer> {
    let mut builder = match ft.tokenizer.as_str() {
        "simple" => TextAnalyzer::builder(SimpleTokenizer::default()).dynamic(),
        other => bail!(
            "unsupported tokenizer `{other}` on field type `{}` (supported: `simple`)",
            ft.name
        ),
    };
    builder = builder.filter_dynamic(RemoveLongFilter::limit(40));

    for filter in &ft.filters {
        let language = |kind: &str| -> Result<Language> {
            let name = filter.language.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "filter `{kind}` on field type `{}` requires a `language`",
                    ft.name
                )
            })?;
            language_by_name(name).ok_or_else(|| {
                anyhow::anyhow!(
                    "unsupported language `{name}` on filter `{kind}` of field type `{}`",
                    ft.name
                )
            })
        };
        builder = match filter.kind.as_str() {
            "lowercase" => builder.filter_dynamic(LowerCaser),
            "stopwords" => {
                let lang = language("stopwords")?;
                let stop = StopWordFilter::new(lang).ok_or_else(|| {
                    anyhow::anyhow!(
                        "Tantivy ships no stopword list for `{:?}` (field type `{}`)",
                        lang,
                        ft.name
                    )
                })?;
                builder.filter_dynamic(stop)
            }
            "stemmer" => builder.filter_dynamic(Stemmer::new(language("stemmer")?)),
            other => bail!(
                "unsupported filter kind `{other}` on field type `{}`",
                ft.name
            ),
        };
    }
    Ok(builder.build())
}

/// Accepts both an English language name (`english`) and an ISO-639-1 code
/// (`en`), because the PRD's example chain writes `language = "english"`.
fn language_by_name(name: &str) -> Option<Language> {
    let lower = name.to_lowercase();
    LANGUAGES
        .iter()
        .find(|(code, lang)| *code == lower || format!("{lang:?}").to_lowercase() == lower)
        .map(|(_, lang)| *lang)
}

/// Registers the language presets and every custom chain into a tokenizer
/// manager seeded with Tantivy's defaults (`raw`, `default`, `en_stem`).
fn build_tokenizers(field_types: &[FieldTypeConfig]) -> Result<TokenizerManager> {
    let manager = TokenizerManager::default();
    for (code, lang) in LANGUAGES {
        if *code == "en" {
            continue; // `text_en` uses Tantivy's own `en_stem`.
        }
        manager.register(
            &format!("text_{code}"),
            TextAnalyzer::builder(SimpleTokenizer::default())
                .filter(RemoveLongFilter::limit(40))
                .filter(LowerCaser)
                .filter(Stemmer::new(*lang))
                .build(),
        );
    }
    for ft in field_types {
        manager.register(&ft.name, build_analyzer(ft)?);
    }
    Ok(manager)
}

fn text_options(tokenizer: &str, stored: bool, fast: bool) -> TextOptions {
    let indexing = TextFieldIndexing::default()
        .set_tokenizer(tokenizer)
        .set_index_option(IndexRecordOption::WithFreqsAndPositions);
    let mut opts = TextOptions::default().set_indexing_options(indexing);
    if stored {
        opts = opts.set_stored();
    }
    if fast {
        opts = opts.set_fast(Some(tokenizer));
    }
    opts
}

fn numeric_options(stored: bool, fast: bool) -> NumericOptions {
    let mut opts = NumericOptions::default().set_indexed();
    if stored {
        opts = opts.set_stored();
    }
    if fast {
        opts = opts.set_fast();
    }
    opts
}

/// Loads and builds a Tantivy schema from a TOML schema file at `path`.
pub fn load(path: &Path) -> Result<WayfinderSchema> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading schema file {}", path.display()))?;
    parse(&raw).with_context(|| format!("parsing schema file {}", path.display()))
}

/// As `load`, but from the TOML text directly.
pub fn parse(raw: &str) -> Result<WayfinderSchema> {
    let parsed: SchemaFile = toml::from_str(raw)?;
    let tokenizers = build_tokenizers(&parsed.field_types)?;

    let mut builder = Schema::builder();
    let mut field_handles = HashMap::new();

    for fc in &parsed.fields {
        let resolved = resolve_type(&fc.type_, &parsed.field_types)
            .with_context(|| format!("on field `{}`", fc.name))?;
        let field = match resolved {
            ResolvedType::Str => {
                // Not Tantivy's own `STRING` const: that leaves fieldnorms
                // on, which BM25-length-norms a multivalued string field by
                // its value count — Solr's `StrField` sets `omitNorms=true`
                // and never does that (finding 45's `select_q_field_term`:
                // `doc1`, with 2 `category` values, must rank no worse than
                // `doc4`'s 1, tied on an equal score and insertion-order
                // tie-broken, not favouring the fewer-valued doc).
                let indexing = TextFieldIndexing::default()
                    .set_tokenizer("raw")
                    .set_index_option(IndexRecordOption::Basic)
                    .set_fieldnorms(false);
                let mut opts = TextOptions::default().set_indexing_options(indexing);
                if fc.stored {
                    opts = opts.set_stored();
                }
                if fc.fast {
                    opts = opts.set_fast(Some("raw"));
                }
                builder.add_text_field(&fc.name, opts)
            }
            ResolvedType::Text { tokenizer } => {
                builder.add_text_field(&fc.name, text_options(&tokenizer, fc.stored, fc.fast))
            }
            ResolvedType::I64 => {
                builder.add_i64_field(&fc.name, numeric_options(fc.stored, fc.fast))
            }
            ResolvedType::F64 => {
                builder.add_f64_field(&fc.name, numeric_options(fc.stored, fc.fast))
            }
            ResolvedType::Date => {
                // Solr's `pdate` is millisecond precision; `DateOptions`'
                // own default is seconds (issue #33 / finding 40), which
                // collapses two values inside the same second into one fast
                // column value — and so one facet bucket, undercounting a
                // real Solr divergence (`facet_field_date_ms_all.json`).
                let mut opts = DateOptions::default()
                    .set_indexed()
                    .set_precision(DateTimePrecision::Milliseconds);
                if fc.stored {
                    opts = opts.set_stored();
                }
                if fc.fast {
                    opts = opts.set_fast();
                }
                builder.add_date_field(&fc.name, opts)
            }
        };
        field_handles.insert(fc.name.clone(), field);
    }

    // Validate the dynamic rules' types up front, so a typo is a startup error
    // rather than a surprise on the first matching document.
    for rule in &parsed.dynamic_fields {
        validate_pattern(&rule.pattern)?;
        resolve_type(&rule.type_, &parsed.field_types)
            .with_context(|| format!("on dynamic field pattern `{}`", rule.pattern))?;
    }

    // The catch-all JSON fields backing `[[dynamic_fields]]`. Present only when
    // there is at least one rule, which is why toggling a schema between "no
    // dynamic rules" and "some dynamic rules" changes the Tantivy schema and so
    // needs a reindex — see `check_compatible`.
    for name in catch_all_fields(&parsed.dynamic_fields) {
        let tokenizer = if *name == DYNAMIC_TEXT_FIELD {
            TEXT_EN_TOKENIZER
        } else {
            "raw"
        };
        let opts = JsonObjectOptions::default()
            .set_stored()
            .set_fast(Some(tokenizer))
            .set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer(tokenizer)
                    .set_index_option(IndexRecordOption::WithFreqsAndPositions),
            );
        field_handles.insert((*name).to_string(), builder.add_json_field(name, opts));
    }

    for copy in &parsed.copy_fields {
        for (role, name) in [("source", &copy.source), ("dest", &copy.dest)] {
            if !field_handles.contains_key(name) {
                bail!("copy_fields {role} `{name}` is not a declared field");
            }
        }
    }

    let tantivy_schema = builder.build();

    if !field_handles.contains_key(&parsed.core.unique_key) {
        bail!(
            "core.unique_key `{}` is not a declared field",
            parsed.core.unique_key
        );
    }
    // The update pipeline (issue #9) resolves the uniqueKey term with
    // `Term::from_field_text`, which only makes sense for a string-typed
    // field — an i64/date uniqueKey would let `delete_term`/`TermQuery`
    // silently match nothing (overwrite=true would duplicate instead of
    // replace, delete-by-id would 200 while deleting nothing). Reject that
    // loudly at load time rather than let it fail silently at request time
    // (review round 1, five-minute item).
    let unique_key_field_config = parsed
        .fields
        .iter()
        .find(|f| f.name == parsed.core.unique_key)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "core.unique_key `{}` must be a plain declared field, not a dynamic/catch-all one",
                parsed.core.unique_key
            )
        })?;
    let unique_key_resolved_type =
        resolve_type(&unique_key_field_config.type_, &parsed.field_types)
            .with_context(|| format!("on core.unique_key `{}`", parsed.core.unique_key))?;
    if !matches!(unique_key_resolved_type, ResolvedType::Str) {
        bail!(
            "core.unique_key `{}` must be an unanalyzed string-typed field (`string`/`keyword`), \
             got `{}` — the update pipeline resolves the uniqueKey as a single exact text term \
             via `Term::from_field_text`, and an analyzed type (e.g. `text_en`, `text_general`, \
             or a custom analyzed [[field_types]] chain) would tokenize the value so a document \
             no longer matches itself",
            parsed.core.unique_key,
            unique_key_field_config.type_,
        );
    }
    // A multiValued uniqueKey has no single term to resolve against
    // (`Term::from_field_text` takes one value): overwrite/delete-by-id would
    // be undefined for a doc with e.g. `id: ["a", "b"]`. Solr refuses this
    // outright; reject it loudly at load time too (issue #40).
    if unique_key_field_config.multi_valued {
        bail!(
            "core.unique_key `{}` must not be multi-valued — the update pipeline resolves the \
             uniqueKey as a single term, and a multi-valued field has no single value to \
             resolve against",
            parsed.core.unique_key,
        );
    }
    // A document missing its uniqueKey has no term to overwrite/delete by;
    // Solr requires the uniqueKey field to be present on every document.
    // Require `required = true` on it at load time so that gap is caught
    // once, up front, rather than per-document at request time (issue #40).
    if !unique_key_field_config.required {
        bail!(
            "core.unique_key `{}` must be declared with `required = true` — every document \
             needs a value for the field the update pipeline overwrites/deletes by",
            parsed.core.unique_key,
        );
    }
    if !field_handles.contains_key(&parsed.core.default_field) {
        bail!(
            "core.default_field `{}` is not a declared field",
            parsed.core.default_field
        );
    }

    Ok(WayfinderSchema {
        tantivy_schema,
        core: parsed.core,
        fields: parsed.fields,
        dynamic_fields: parsed.dynamic_fields,
        copy_fields: parsed.copy_fields,
        field_types: parsed.field_types,
        tokenizers,
        field_handles,
    })
}

/// Where the schema an index was built with is kept, next to the index itself.
pub fn snapshot_path(data_dir: &Path) -> PathBuf {
    data_dir.join("wayfinder-schema.toml")
}

/// Compares the schema an existing index was built with against the configured
/// one, and refuses any change that would make the index and the schema
/// disagree (PRD open question 4 — refuse and require a reindex).
///
/// Two things alter the Tantivy schema and are therefore checked: `[[fields]]`,
/// and whether `[[dynamic_fields]]` is empty — the catch-all JSON fields exist
/// only when there is at least one rule, so adding the first rule or removing
/// the last one changes the schema even though editing rules in between does
/// not.
///
/// `[[copy_fields]]` and `[[field_types]]` never alter the Tantivy schema (they
/// govern index-time content and analysis), so changing them affects only
/// documents indexed from then on.
pub fn check_compatible(previous: &str, current: &str) -> Result<()> {
    let prev: SchemaFile = toml::from_str(previous).context("parsing the index's stored schema")?;
    let cur: SchemaFile = toml::from_str(current).context("parsing the configured schema")?;

    for old in &prev.fields {
        match cur.fields.iter().find(|f| f.name == old.name) {
            None => bail!(
                "field `{}` was removed from the schema; the existing index still contains it — \
                 reindex into a fresh data directory",
                old.name
            ),
            Some(new) if new.type_ != old.type_ => bail!(
                "field `{}` changed type from `{}` to `{}`; the existing index was built with the \
                 old type — reindex into a fresh data directory",
                old.name,
                old.type_,
                new.type_
            ),
            // `required` is a validation rule, not part of the Tantivy schema,
            // so toggling it does not invalidate the index.
            Some(new)
                if (new.stored, new.fast, new.multi_valued)
                    != (old.stored, old.fast, old.multi_valued) =>
            {
                bail!(
                    "field `{}` changed options (stored/fast/multi_valued); the existing index \
                     was built with the old options — reindex into a fresh data directory",
                    old.name
                )
            }
            Some(_) => {}
        }
    }

    // PRD open question 4 calls an added field "compatible". Tantivy disagrees:
    // a schema is fixed when the index is created and cannot be extended in
    // place, so a new field still needs a reindex. Say that plainly instead of
    // letting Tantivy fail later with "schema does not match".
    for new in &cur.fields {
        if !prev.fields.iter().any(|f| f.name == new.name) {
            bail!(
                "field `{}` was added to the schema; Tantivy cannot add a field to an existing \
                 index — reindex into a fresh data directory",
                new.name
            );
        }
    }

    // The catch-all JSON fields exist only when at least one dynamic rule does,
    // so crossing that boundary changes the Tantivy schema. Without this check
    // the index would be reopened with its old schema — missing `_dynamic`, or
    // carrying a stray one — which is exactly the silent-stale-schema failure
    // this whole check exists to prevent.
    let (before, after) = (
        catch_all_fields(&prev.dynamic_fields),
        catch_all_fields(&cur.dynamic_fields),
    );
    if before != after {
        let detail = if before.is_empty() {
            "the existing index has no catch-all field to hold their values"
        } else {
            "the existing index still carries the catch-all fields they created"
        };
        bail!(
            "[[dynamic_fields]] went from {} rule(s) to {}; {detail} — reindex into a fresh data \
             directory",
            prev.dynamic_fields.len(),
            cur.dynamic_fields.len()
        );
    }

    Ok(())
}
