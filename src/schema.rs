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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tantivy::schema::{
    DateOptions, DateTimePrecision, Field, IndexRecordOption, JsonObjectOptions, NumericOptions,
    Schema, TextFieldIndexing, TextOptions,
};
use tantivy::tokenizer::{
    Language, LowerCaser, RemoveLongFilter, SimpleTokenizer, Stemmer, StopWordFilter, TextAnalyzer,
    Token, TokenFilter, TokenStream, Tokenizer, TokenizerManager, WhitespaceTokenizer,
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
/// Internal version assigned by `CoreIndex` immediately before insertion. It
/// deliberately has no `[[fields]]` entry: user schema configuration never
/// controls this fast field.
pub const VERSION_FIELD: &str = "_version_";

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

/// The built-in type names `resolve_type` accepts that are not derived from
/// the `LANGUAGES` table. Kept adjacent to `resolve_type`'s match arms: the
/// two are the same list written twice, and `builtin_type_names` is what
/// `/schema/fieldtypes` reports, so a new arm must be added here too.
const NON_LANGUAGE_BUILTIN_TYPES: &[&str] = &[
    "string",
    "keyword",
    "text_general",
    "text_en",
    "int",
    "long",
    "float",
    "double",
    "date",
    // `location` and `location_rpt` are lat/lon points stored as two synthetic
    // f64 fast columns (`<field>__lat`/`__lon`, see `parse`). Listed here so a
    // custom [[field_types]] chain named either is rejected like every other
    // built-in (#170). `location` is Solr's LatLonPointSpatialField (geodist,
    // #331); `location_rpt` is SpatialRecursivePrefixTreeFieldType (heatmap
    // facets, #334). #331 owns the shared encoding; #334 resolves `location_rpt`
    // to the same `ValueKind::Location` rather than forking it.
    "location",
    "location_rpt",
    // `boost_term_payload` mirrors the Drupal module's own payload-bearing
    // field type (`solr-conf-templates/9.x/schema.xml:387-406`): whitespace
    // tokenizer, length min=2/max=100, lowercase, remove-duplicates, then a
    // delimited float payload split on the last `|`. One Tantivy text field
    // with *two* tokenizers -- the indexing side drops the `|<float>` suffix
    // so `{!payload_score v=...}` matches a real posting list, and the fast
    // (columnar) side keeps the token verbatim so the payload can be read back
    // at score time (#340).
    "boost_term_payload",
    // `date_range` is Solr's `DateRangeField` (#341): an interval-valued date
    // whose verbatim text is kept in the field itself and whose endpoints live
    // in two synthetic millisecond-precision date fast columns
    // (`<field>__start`/`__end`, see `parse`). Listed here so a custom
    // [[field_types]] chain named `date_range` is rejected like every other
    // built-in (#170).
    "date_range",
];

/// Every built-in field type `resolve_type` accepts: the non-language types
/// above plus one `text_<code>` per non-English entry in `LANGUAGES`. The
/// language half is derived from `LANGUAGES` itself rather than re-listed, so
/// adding a stemmer automatically reports it and no hand-maintained copy can
/// drift into claiming a language Wayfinder cannot actually stem (issue #156's
/// honesty constraint).
pub fn builtin_type_names() -> Vec<String> {
    let mut names: Vec<String> = NON_LANGUAGE_BUILTIN_TYPES
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    names.extend(
        LANGUAGES
            .iter()
            .filter(|(code, _)| *code != "en")
            .map(|(code, _)| format!("text_{code}")),
    );
    names
}

/// Tantivy's own `default` analyzer: simple tokenizer, long tokens dropped,
/// lowercased, no stemming. Solr calls this shape `text_general`.
const TEXT_GENERAL_TOKENIZER: &str = "default";
/// Wayfinder's versioned Solr-compatible English analyzer: simple tokenizer,
/// long-token removal, lowercase, English stopword removal, then stemming.
/// It intentionally does not override Tantivy's `en_stem`, which remains
/// available for custom analyzer chains and upstream defaults.
const TEXT_EN_TOKENIZER: &str = "wayfinder_text_en_v2";
/// The shared dynamic-text catch-all retains the pre-v2 Snowball analyzer.
/// Drupal Search API's captured configset uses Snowball and preserves singular
/// `day`; all analyzed dynamic rules share this one tokenizer in Tantivy.
const DYNAMIC_TEXT_TOKENIZER: &str = "wayfinder_text_en_v1";

/// `boost_term_payload`'s **indexing** analyzer: the module's front half
/// (whitespace, length min=2/max=100, lowercase, remove-duplicates) followed by
/// the delimited-payload split, keeping only the term. `dog|4.5` indexes as
/// `dog`, so `{!payload_score v="dog"}` resolves against a real posting list.
const BOOST_TERM_PAYLOAD_TOKENIZER: &str = "wayfinder_boost_term_payload_v1";
/// `boost_term_payload`'s **fast-field (columnar)** analyzer: the same front
/// half, but the surviving token is kept verbatim (`dog|4.5`). Tantivy resolves
/// the indexing tokenizer and the fast-field tokenizer through two independent
/// managers (`Index::tokenizers()` vs `Index::fast_field_tokenizer()`), both of
/// which `CoreIndex` seeds with this one `TokenizerManager`, so a single field
/// can legitimately carry a different analyzer on each side. That is what lets
/// the payload live on the same field as the term instead of in a synthetic
/// sibling (#340).
const BOOST_TERM_PAYLOAD_VERBATIM_TOKENIZER: &str = "wayfinder_boost_term_payload_raw_v1";
/// The delimiter Solr's `DelimitedPayloadTokenFilterFactory` defaults to, and
/// the one the module's `sprintf('%s|%.1F')` writes.
pub const BOOST_TERM_PAYLOAD_DELIMITER: char = '|';
/// The module's `LengthFilterFactory` bounds on `boost_term_payload`. Applied
/// *before* the payload split, exactly as the configset orders the chain, which
/// is why `v="a"` analyzes to nothing (and 400s `SpanQuery is null`) while
/// `a|1.0` -- seven characters -- would not.
const BOOST_TERM_PAYLOAD_MIN_LEN: usize = 2;
const BOOST_TERM_PAYLOAD_MAX_LEN: usize = 100;

/// The on-disk analyzer contract for indexes built with Wayfinder's
/// Porter-compatible English preset. This is separate from Tantivy's schema:
/// it lets startup identify pre-contract indexes before their old tokenizer
/// identity can be adopted.
pub const ANALYZER_CONTRACT: &str = "text_en_porter_compatible_v2";
/// A safely adopted pre-v2 index whose unused `_dynamic_text` catch-all still
/// has an older tokenizer identity. It is not full v2 certification: a later
/// rule that starts writing analyzed dynamic values must reindex.
pub const ANALYZER_CONTRACT_LEGACY_DYNAMIC_TEXT: &str =
    "text_en_porter_compatible_v2_legacy_dynamic_text";
/// The v1 marker is recognized explicitly during upgrade, rather than treated
/// as an unknown marker, so raw-only indexes can be adopted safely while
/// indexes whose terms may use the old English analyzer fail closed.
pub const ANALYZER_CONTRACT_V1: &str = "text_en_stopwords_v1";
pub const ANALYZER_CONTRACT_V1_LEGACY_DYNAMIC_TEXT: &str =
    "text_en_stopwords_v1_legacy_dynamic_text";

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
    /// A lat/lon point: indexed as two synthetic f64 fast columns
    /// (`<field>__lat`/`__lon`) rather than one Tantivy field, so its values do
    /// not flow through `add_values`' single-field path the way the other
    /// kinds do (#331 owns the encoding; #334 reuses it for `location_rpt`).
    Location,
    /// A `solr.DateRangeField` interval (#341): the verbatim value text is
    /// stored in the field itself, and its resolved endpoints go into two
    /// synthetic millisecond-precision date fast columns
    /// (`<field>__start`/`__end`) that `crate::date_range`'s interval
    /// predicates read back member by member.
    DateRange,
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
    /// Static `location`/`location_rpt` fields' two synthetic f64 columns,
    /// keyed by the user-facing field name: `(lat_field, lon_field)`. Kept out
    /// of `field_handles` (like `VERSION_FIELD`) so the synthetic `__lat`/`__lon`
    /// columns never leak into name-based resolver paths and are reachable
    /// only through [`Self::location_fields`] (#331; #334's heatmap reads them).
    location_fields: HashMap<String, (Field, Field)>,
    /// Static `date_range` fields' two synthetic date columns, keyed by the
    /// user-facing field name: `(start_field, end_field)`. Kept out of
    /// `field_handles` for the same reason `location_fields` is — the
    /// `<name>__start`/`__end` names must never reach a name-based resolver or
    /// leak into `fl` output (#341). Unlike `location`, the user-facing name
    /// *is* also a real Tantivy field here: it holds the verbatim value text
    /// finding 179 requires to round-trip.
    date_range_fields: HashMap<String, (Field, Field)>,
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

    /// The two synthetic f64 columns (`__lat`, `__lon`) backing a declared
    /// static `location`/`location_rpt` field, or `None` if `name` is not one.
    /// Dynamic `locs_*`/`rpts_*`-rule fields resolve to `Location` too but have
    /// no synthetic columns (the name is unknown at build time), so they are
    /// `None` here -- `geodist()` (#331) and `facet.heatmap` (#334) on a dynamic
    /// field are a documented ceiling, not a supported path.
    pub fn location_fields(&self, name: &str) -> Option<(Field, Field)> {
        self.location_fields.get(name).copied()
    }

    /// The two synthetic date columns (`__start`, `__end`) backing a declared
    /// static `date_range` field, or `None` if `name` is not one (#341). A
    /// dynamic `date_range` rule has no synthetic columns: its endpoints are
    /// JSON sub-paths inside the catch-all instead
    /// (`_dynamic.<name>.start`/`.end`, see
    /// [`Self::resolved_date_range_columns`]).
    pub fn date_range_fields(&self, name: &str) -> Option<(Field, Field)> {
        self.date_range_fields.get(name).copied()
    }

    /// The pair of fast-field *column names* holding `name`'s interval
    /// endpoints, resolved with the same static-before-dynamic precedence as
    /// [`Self::resolved_fast_column`]: `<name>__start`/`__end` for a declared
    /// `date_range` field, `_dynamic.<name>.start`/`.end` for one that only
    /// matches a `[[dynamic_fields]]` rule. `None` if `name` is not a
    /// `date_range` field at all (#341).
    pub fn resolved_date_range_columns(&self, name: &str) -> Option<(String, String)> {
        if self.resolved_value_kind(name) != Some(ValueKind::DateRange) {
            return None;
        }
        if self.is_static(name) {
            return Some((format!("{name}__start"), format!("{name}__end")));
        }
        let rule = self.match_dynamic(name)?;
        let container = self.dynamic_target(rule);
        Some((
            format!("{container}.{name}.start"),
            format!("{container}.{name}.end"),
        ))
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
        if name == VERSION_FIELD {
            return None;
        }

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

    /// True if `name` is a declared `boost_term_payload` field, i.e. one whose
    /// fast column carries verbatim `<term>|<boost>` tokens a
    /// `{!payload_score}` can read (#340). Every other field -- including a
    /// perfectly ordinary analyzed text field -- is not payload-bearing, and
    /// `{!payload_score f=<that>}` is a 400 rather than the uncaught Lucene NPE
    /// real Solr answers with (PRD divergence 11, fixture `pls_err_nonpayload`).
    pub fn is_boost_term_payload(&self, name: &str) -> bool {
        self.field_config(name).is_some_and(|f| {
            matches!(
                resolve_type(&f.type_, &self.field_types),
                Ok(ResolvedType::BoostTermPayload)
            )
        })
    }

    /// Whether this schema can contain static data whose analyzer changed from
    /// v1 Snowball stemming to v2's Porter-compatible terminal-`y` behavior.
    pub fn uses_static_text_en(&self) -> bool {
        self.fields.iter().any(|field| field.type_ == "text_en")
    }

    /// Whether any configured dynamic rule can write analyzed text through
    /// `_dynamic_text`. This path changed before analyzer contract v1, but it
    /// deliberately retains v1 Snowball semantics in v2 for the captured
    /// Search API configset.
    pub fn uses_analyzed_dynamic_path(&self) -> bool {
        self.dynamic_fields.iter().any(|field| {
            matches!(
                resolve_type(&field.type_, &self.field_types),
                Ok(ResolvedType::Text { .. })
            )
        })
    }

    /// Whether this schema has the `_dynamic_text` catch-all. Before analyzer
    /// contract v1, every dynamic schema created it with Tantivy's `en_stem`,
    /// even if all configured rules were raw.
    pub fn has_dynamic_fields(&self) -> bool {
        !self.dynamic_fields.is_empty()
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
            // The indexing-side analyzer: the terms a query actually resolves
            // against. The verbatim fast-field analyzer is deliberately not
            // reachable here -- it is storage, not the query-time analysis
            // chain (#340).
            ResolvedType::BoostTermPayload => BOOST_TERM_PAYLOAD_TOKENIZER.to_string(),
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
    Text {
        tokenizer: String,
    },
    I64,
    F64,
    Date,
    /// A lat/lon point backed by two synthetic f64 fast columns. The build path
    /// creates `<field>__lat`/`<field>__lon` for it (see `parse`); it is never a
    /// single Tantivy field, which is why `Location` has no arm in the
    /// `field = match resolved { ... }` block below.
    Location,
    /// The Drupal module's payload-bearing `boost_term` type (#340): one
    /// Tantivy text field carrying two analyzers -- payload-stripped terms on
    /// the indexing side, verbatim `<term>|<boost>` tokens in the fast column.
    /// It takes JSON strings like any other text type, so it is a
    /// [`ValueKind::Text`] and needs no new arm anywhere downstream.
    BoostTermPayload,
    /// A `solr.DateRangeField` interval (#341). The build path creates the
    /// user-facing field (verbatim value text) *plus* two synthetic date fast
    /// columns `<field>__start`/`<field>__end`, so like `Location` it is
    /// handled above the `field = match resolved { ... }` block.
    DateRange,
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
        // Solr's `LatLonPointSpatialField` (`location`, geodist #331) and
        // `SpatialRecursivePrefixTreeFieldType` (`location_rpt`, heatmap #334):
        // a point stored as two synthetic f64 fast columns. Both resolve to the
        // same encoding; #292 sizing deferred the `location_rpt` choice to #334,
        // which keeps the shared two-column storage (finding 158).
        "location" | "location_rpt" => ResolvedType::Location,
        // The module's own payload field type, reproduced (#340). See
        // `BOOST_TERM_PAYLOAD_TOKENIZER` for why this is not a plain
        // `Text { tokenizer }`: the field needs a *different* analyzer on its
        // indexing and fast-field sides.
        "boost_term_payload" => ResolvedType::BoostTermPayload,
        // Solr's `DateRangeField` (#341): an interval-valued date. Reported to
        // `/schema/fieldtypes` as `wayfinder.DateRangeField`.
        "date_range" => ResolvedType::DateRange,
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
        ResolvedType::Str | ResolvedType::Text { .. } | ResolvedType::BoostTermPayload => {
            ValueKind::Text
        }
        ResolvedType::I64 => ValueKind::I64,
        ResolvedType::F64 => ValueKind::F64,
        ResolvedType::Date => ValueKind::Date,
        ResolvedType::Location => ValueKind::Location,
        ResolvedType::DateRange => ValueKind::DateRange,
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

/// Implements classic Porter step 1c before Tantivy's English Snowball
/// stemmer: a terminal ASCII `y` becomes `i` only if the preceding stem has
/// an ASCII vowel.
///
/// ponytail: this is the captured Porter terminal-y compatibility rule layered
/// over Tantivy's English stemmer, not a full replacement tokenizer.
#[derive(Clone)]
struct PorterTerminalYFilter;

impl TokenFilter for PorterTerminalYFilter {
    type Tokenizer<T: Tokenizer> = PorterTerminalYTokenizer<T>;

    fn transform<T: Tokenizer>(self, tokenizer: T) -> Self::Tokenizer<T> {
        PorterTerminalYTokenizer { inner: tokenizer }
    }
}

#[derive(Clone)]
struct PorterTerminalYTokenizer<T> {
    inner: T,
}

impl<T: Tokenizer> Tokenizer for PorterTerminalYTokenizer<T> {
    type TokenStream<'a> = PorterTerminalYTokenStream<T::TokenStream<'a>>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        PorterTerminalYTokenStream {
            tail: self.inner.token_stream(text),
        }
    }
}

struct PorterTerminalYTokenStream<T> {
    tail: T,
}

impl<T: TokenStream> TokenStream for PorterTerminalYTokenStream<T> {
    fn advance(&mut self) -> bool {
        if !self.tail.advance() {
            return false;
        }
        let text = &mut self.tail.token_mut().text;
        if text.is_ascii()
            && text.ends_with('y')
            && text[..text.len() - 1]
                .bytes()
                .any(|byte| matches!(byte, b'a' | b'e' | b'i' | b'o' | b'u'))
        {
            text.pop();
            text.push('i');
        }
        true
    }

    fn token(&self) -> &Token {
        self.tail.token()
    }

    fn token_mut(&mut self) -> &mut Token {
        self.tail.token_mut()
    }
}

/// Solr's `LengthFilterFactory`: drops tokens shorter than `min` or longer than
/// `max` characters. Tantivy ships only `RemoveLongFilter` (an upper bound), and
/// `boost_term_payload` needs the lower bound too -- it is what makes `v="a"` a
/// 400 rather than a term query nothing carries (#340, `pls_err_short_v`).
#[derive(Clone)]
struct LengthFilter {
    min: usize,
    max: usize,
}

impl TokenFilter for LengthFilter {
    type Tokenizer<T: Tokenizer> = LengthFilterTokenizer<T>;

    fn transform<T: Tokenizer>(self, tokenizer: T) -> Self::Tokenizer<T> {
        LengthFilterTokenizer {
            inner: tokenizer,
            min: self.min,
            max: self.max,
        }
    }
}

#[derive(Clone)]
struct LengthFilterTokenizer<T> {
    inner: T,
    min: usize,
    max: usize,
}

impl<T: Tokenizer> Tokenizer for LengthFilterTokenizer<T> {
    type TokenStream<'a> = LengthFilterTokenStream<T::TokenStream<'a>>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        LengthFilterTokenStream {
            tail: self.inner.token_stream(text),
            min: self.min,
            max: self.max,
        }
    }
}

struct LengthFilterTokenStream<T> {
    tail: T,
    min: usize,
    max: usize,
}

impl<T: TokenStream> TokenStream for LengthFilterTokenStream<T> {
    fn advance(&mut self) -> bool {
        while self.tail.advance() {
            // Solr's LengthFilter measures the term attribute's length, i.e.
            // characters, not bytes.
            let len = self.tail.token().text.chars().count();
            if len >= self.min && len <= self.max {
                return true;
            }
        }
        false
    }

    fn token(&self) -> &Token {
        self.tail.token()
    }

    fn token_mut(&mut self) -> &mut Token {
        self.tail.token_mut()
    }
}

/// Solr's `RemoveDuplicatesTokenFilterFactory`: drops a token whose text
/// duplicates one already emitted *at the same position*. Tokens at different
/// positions are never duplicates, which is why d3's two `dog|...` values
/// (`solr-ref/capture.sh`'s pls corpus) both survive -- consecutive multiValued
/// values sit at consecutive positions.
#[derive(Clone)]
struct RemoveDuplicatesFilter;

impl TokenFilter for RemoveDuplicatesFilter {
    type Tokenizer<T: Tokenizer> = RemoveDuplicatesTokenizer<T>;

    fn transform<T: Tokenizer>(self, tokenizer: T) -> Self::Tokenizer<T> {
        RemoveDuplicatesTokenizer { inner: tokenizer }
    }
}

#[derive(Clone)]
struct RemoveDuplicatesTokenizer<T> {
    inner: T,
}

impl<T: Tokenizer> Tokenizer for RemoveDuplicatesTokenizer<T> {
    type TokenStream<'a> = RemoveDuplicatesTokenStream<T::TokenStream<'a>>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        RemoveDuplicatesTokenStream {
            tail: self.inner.token_stream(text),
            position: None,
            seen: Vec::new(),
        }
    }
}

struct RemoveDuplicatesTokenStream<T> {
    tail: T,
    /// The position the `seen` texts belong to; `None` before the first token.
    position: Option<usize>,
    seen: Vec<String>,
}

impl<T: TokenStream> TokenStream for RemoveDuplicatesTokenStream<T> {
    fn advance(&mut self) -> bool {
        while self.tail.advance() {
            let token = self.tail.token();
            if self.position != Some(token.position) {
                self.position = Some(token.position);
                self.seen.clear();
            }
            if self.seen.iter().any(|text| text == &token.text) {
                continue;
            }
            self.seen.push(token.text.clone());
            return true;
        }
        false
    }

    fn token(&self) -> &Token {
        self.tail.token()
    }

    fn token_mut(&mut self) -> &mut Token {
        self.tail.token_mut()
    }
}

/// The term half of Solr's `DelimitedPayloadTokenFilterFactory` with the
/// `float` encoder: split the token at its **last** delimiter and keep the
/// prefix, so `dog|4.5` indexes as `dog`. A token with no delimiter, or whose
/// suffix is not a float, is left exactly as it is -- Lucene's filter only
/// separates a payload when it actually finds one to decode.
///
/// ponytail: the payload *value* is not carried on the token (tantivy 0.26 has
/// no postings payload at all). It is read back from the field's fast column,
/// which `BOOST_TERM_PAYLOAD_VERBATIM_TOKENIZER` populates with the undivided
/// token -- see `PayloadScoreQuery`.
#[derive(Clone)]
struct DelimitedPayloadStripFilter;

impl TokenFilter for DelimitedPayloadStripFilter {
    type Tokenizer<T: Tokenizer> = DelimitedPayloadStripTokenizer<T>;

    fn transform<T: Tokenizer>(self, tokenizer: T) -> Self::Tokenizer<T> {
        DelimitedPayloadStripTokenizer { inner: tokenizer }
    }
}

#[derive(Clone)]
struct DelimitedPayloadStripTokenizer<T> {
    inner: T,
}

impl<T: Tokenizer> Tokenizer for DelimitedPayloadStripTokenizer<T> {
    type TokenStream<'a> = DelimitedPayloadStripTokenStream<T::TokenStream<'a>>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        DelimitedPayloadStripTokenStream {
            tail: self.inner.token_stream(text),
        }
    }
}

struct DelimitedPayloadStripTokenStream<T> {
    tail: T,
}

impl<T: TokenStream> TokenStream for DelimitedPayloadStripTokenStream<T> {
    fn advance(&mut self) -> bool {
        if !self.tail.advance() {
            return false;
        }
        let text = &mut self.tail.token_mut().text;
        if let Some(term_len) = split_delimited_payload(text).map(|(term, _)| term.len()) {
            text.truncate(term_len);
        }
        true
    }

    fn token(&self) -> &Token {
        self.tail.token()
    }

    fn token_mut(&mut self) -> &mut Token {
        self.tail.token_mut()
    }
}

/// Splits one `boost_term_payload` token into `(term, payload)` at its **last**
/// delimiter, or `None` when there is no delimiter or the suffix is not a
/// float. The single definition both the indexing filter above and
/// `PayloadScoreQuery`'s column reader use, so the two halves of the field type
/// cannot drift apart on what counts as a payload.
pub fn split_delimited_payload(token: &str) -> Option<(&str, f32)> {
    let (term, payload) = token.rsplit_once(BOOST_TERM_PAYLOAD_DELIMITER)?;
    // Rust's `f32::from_str` accepts `inf`/`NaN`, which Lucene's FloatEncoder
    // (a plain `Float.parseFloat`) also does. Keep them out anyway: a NaN
    // payload would poison min/max comparisons in the scorer.
    let value: f32 = payload.parse().ok()?;
    value.is_finite().then_some((term, value))
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

/// Registers Wayfinder's English preset, the other language presets, and
/// every custom chain into a tokenizer manager seeded with Tantivy's defaults
/// (`raw`, `default`, `en_stem`).
fn build_tokenizers(field_types: &[FieldTypeConfig]) -> Result<TokenizerManager> {
    let manager = TokenizerManager::default();
    let english_stopwords =
        || StopWordFilter::new(Language::English).expect("Tantivy ships an English stopword list");
    manager.register(
        DYNAMIC_TEXT_TOKENIZER,
        TextAnalyzer::builder(SimpleTokenizer::default())
            .filter(RemoveLongFilter::limit(40))
            .filter(LowerCaser)
            .filter(english_stopwords())
            .filter(Stemmer::new(Language::English))
            .build(),
    );
    manager.register(
        TEXT_EN_TOKENIZER,
        TextAnalyzer::builder(SimpleTokenizer::default())
            .filter(RemoveLongFilter::limit(40))
            .filter(LowerCaser)
            .filter(english_stopwords())
            .filter(PorterTerminalYFilter)
            .filter(Stemmer::new(Language::English))
            .build(),
    );
    for (code, lang) in LANGUAGES {
        if *code == "en" {
            continue; // registered above under Wayfinder's versioned identity.
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
    // `boost_term_payload`'s two halves (#340). Both share the module's front
    // half; they differ only in whether the `|<float>` suffix survives, and the
    // *same* manager backs both of tantivy's tokenizer lookups (indexing and
    // fast field), so registering both names here is all the wiring one field
    // with two analyzers needs.
    let boost_term_payload_front = || {
        TextAnalyzer::builder(WhitespaceTokenizer::default())
            .filter_dynamic(LengthFilter {
                min: BOOST_TERM_PAYLOAD_MIN_LEN,
                max: BOOST_TERM_PAYLOAD_MAX_LEN,
            })
            .filter_dynamic(LowerCaser)
            .filter_dynamic(RemoveDuplicatesFilter)
    };
    manager.register(
        BOOST_TERM_PAYLOAD_TOKENIZER,
        boost_term_payload_front()
            .filter_dynamic(DelimitedPayloadStripFilter)
            .build(),
    );
    manager.register(
        BOOST_TERM_PAYLOAD_VERBATIM_TOKENIZER,
        boost_term_payload_front().build(),
    );
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
    if parsed
        .fields
        .iter()
        .any(|field| field.name == VERSION_FIELD)
    {
        bail!("field `{VERSION_FIELD}` is reserved for Wayfinder's internal version field");
    }
    // Any dynamic rule makes the builder allocate these catch-all fields
    // implicitly. Reserving their names against explicit `[[fields]]`
    // declarations in that case keeps an operator typo from reaching Tantivy's
    // duplicate-field panic (issue #194). Without dynamic rules no implicit
    // field exists, so these names remain valid static fields.
    if !parsed.dynamic_fields.is_empty()
        && let Some(field) = parsed
            .fields
            .iter()
            .find(|field| matches!(field.name.as_str(), DYNAMIC_FIELD | DYNAMIC_TEXT_FIELD))
    {
        bail!(
            "field `{}` is reserved for Wayfinder's dynamic-field storage",
            field.name
        );
    }
    // Every name `resolve_type` resolves to a built-in is reserved (issue
    // #170). `resolve_type` checks `[[field_types]]` *before* its built-in
    // match arms, so a custom chain named `double` silently retypes every
    // `type = "double"` field from a numeric field to analyzed text, breaking
    // range queries and sorting with no error anywhere. Rejected outright
    // rather than allowed as an override, extending the `text_en` reservation
    // (#51) to the whole set. The list comes from `builtin_type_names()`, whose
    // language half is derived from `LANGUAGES`, so adding a stemmer language
    // cannot leave a built-in shadowable. Its other half,
    // `NON_LANGUAGE_BUILTIN_TYPES`, *is* a second hand-written copy of
    // `resolve_type`'s match arms, and nothing in the type system pins the two
    // in sync -- adding an arm without extending the list would reintroduce this
    // bug. What catches that is the expiring guard
    // `type_names_absent_from_the_reservation_list_are_still_unresolvable` in
    // `tests/schema_layer.rs`: it asserts the plausible additions (`boolean`,
    // `bool`, `binary`, `location`) are still unresolvable, so the moment one
    // gains an arm the suite fails and names this list.
    // `TEXT_EN_TOKENIZER` is not a schema type name but is
    // reserved alongside them: it is the tokenizer identity `text_en` registers
    // under, and a chain registering over it would redefine the built-in preset.
    let reserved_type_names: Vec<String> = builtin_type_names()
        .into_iter()
        .chain([
            TEXT_EN_TOKENIZER.to_string(),
            DYNAMIC_TEXT_TOKENIZER.to_string(),
            BOOST_TERM_PAYLOAD_TOKENIZER.to_string(),
            BOOST_TERM_PAYLOAD_VERBATIM_TOKENIZER.to_string(),
        ])
        .collect();
    if let Some(field_type) = parsed
        .field_types
        .iter()
        .find(|field_type| reserved_type_names.contains(&field_type.name))
    {
        bail!(
            "field type `{}` is reserved: it names one of Wayfinder's built-in field types, and \
             `[[field_types]]` may not shadow a built-in — rename the custom chain",
            field_type.name
        );
    }
    // `resolve_type` picks the *first* `[[field_types]]` entry whose name
    // matches, so a second entry sharing that name is silently dead code --
    // and `build_tokenizers` would register it over the first. Case-sensitive,
    // exactly like the `==` that `resolve_type` resolves with.
    let mut seen_field_types = HashSet::new();
    for field_type in &parsed.field_types {
        if !seen_field_types.insert(field_type.name.as_str()) {
            bail!(
                "duplicate field type `{}`: two [[field_types]] entries share that name",
                field_type.name
            );
        }
    }
    // Two `[[fields]]` entries sharing a `name` make `SchemaBuilder::add_field`
    // *panic* ("Field already exists in schema <name>", tantivy-0.26.1
    // `src/schema/schema.rs:202`), so an operator typo crashes the process from
    // inside a dependency instead of producing the ordinary schema-load error
    // every other mistake in this file produces (issue #173). This must stay
    // ahead of the `add_*_field` calls below, which is what makes that panic
    // unreachable. Keyed on the name alone: a second declaration is a duplicate
    // name however it is configured, and differing configuration is the more
    // dangerous case, since the two declarations disagree about what the field
    // is. Case-sensitive, like Tantivy's own exact-name check and #160.
    let mut seen_fields = HashSet::new();
    for field in &parsed.fields {
        if !seen_fields.insert(field.name.as_str()) {
            bail!(
                "duplicate field `{}`: two [[fields]] entries share that name",
                field.name
            );
        }
    }
    // Two `[[dynamic_fields]]` rules sharing a `pattern` fail silently rather
    // than loudly: `match_dynamic`'s `max_by_key(|d| d.pattern.len())` returns
    // the *last* rule among equal-length patterns, so the earlier declaration is
    // dead code, and with differing types the two rules also disagree about
    // which catch-all field (`_dynamic` vs `_dynamic_text`) the values land in
    // (issue #173). Exact duplicates only: overlapping-but-distinct globs
    // (`tm_*` alongside `tm_X3b_*` and `*`) are ordinary Solr configuration,
    // and longest-pattern-wins exists precisely to resolve that overlap.
    // Precedence, deliberate: this runs before `validate_pattern`, so two
    // identical *invalid* patterns report the duplicate rather than the
    // pre-existing "is not supported". Accepted -- both diagnoses are true, the
    // choice is deterministic, and either one sends the operator to the same
    // line of schema.toml.
    let mut seen_patterns = HashSet::new();
    for rule in &parsed.dynamic_fields {
        if !seen_patterns.insert(rule.pattern.as_str()) {
            bail!(
                "duplicate dynamic field pattern `{}`: two [[dynamic_fields]] entries share it",
                rule.pattern
            );
        }
    }
    let tokenizers = build_tokenizers(&parsed.field_types)?;

    let mut builder = Schema::builder();
    let mut field_handles = HashMap::new();
    let mut location_fields: HashMap<String, (Field, Field)> = HashMap::new();
    let mut date_range_fields: HashMap<String, (Field, Field)> = HashMap::new();

    // `_version_` is a Wayfinder-owned field, never a schema.toml field. It
    // is not stored, so default select responses cannot expose it. Keep it
    // out of `field_handles`: all normal resolver paths remain user-schema
    // only; `stats::check_statable` is its deliberate narrow exception.
    builder.add_i64_field(VERSION_FIELD, numeric_options(false, true));

    for fc in &parsed.fields {
        let resolved = resolve_type(&fc.type_, &parsed.field_types)
            .with_context(|| format!("on field `{}`", fc.name))?;
        // A `location`/`location_rpt` field is not one Tantivy field but two
        // synthetic f64 fast columns (`<name>__lat`/`<name>__lon`), so it
        // bypasses the single-field match and `field_handles` entirely: the
        // columns are reachable only via `location_fields`, and the user-facing
        // name never becomes a Tantivy field (#331; #334's heatmap reads them).
        // Dynamic `rpts_*`/`locs_*`-rule fields resolve to `Location` too but
        // have no synthetic columns (name unknown at build time), so the heatmap
        // (and `geodist()`) require a declared static field -- a documented
        // ceiling, not a bug.
        if matches!(resolved, ResolvedType::Location) {
            let lat_name = format!("{}__lat", fc.name);
            let lon_name = format!("{}__lon", fc.name);
            let lat = builder.add_f64_field(&lat_name, numeric_options(false, true));
            let lon = builder.add_f64_field(&lon_name, numeric_options(false, true));
            if location_fields
                .insert(fc.name.clone(), (lat, lon))
                .is_some()
            {
                // Two [[fields]] entries can only share a non-location name;
                // the duplicate-name guard above keys on `name` alone, so this
                // is belt-and-braces against a future `__lat`/`__lon` collision
                // with a user-declared field of the same synthetic name.
                bail!(
                    "location field `{}` would create synthetic columns that collide with an \
                     existing field; rename the location field",
                    fc.name
                );
            }
            continue;
        }
        // A `date_range` field (#341) is three physical fields: the
        // user-facing one, holding the value's verbatim text so `fl` can
        // round-trip it exactly (finding 179), plus two synthetic
        // millisecond-precision date fast columns
        // (`<name>__start`/`<name>__end`) holding the resolved endpoints of
        // each interval member, appended in the same order so ordinal `i` of
        // one pairs with ordinal `i` of the other -- which is what makes the
        // hole-sensitive `Intersects`/`Contains` predicates possible on a
        // multiValued field (finding 182). The two synthetic columns stay out
        // of `field_handles`, so they can never be named by `fl`, `sort`,
        // `facet.field` or a query.
        if matches!(resolved, ResolvedType::DateRange) {
            let raw_indexing = TextFieldIndexing::default()
                .set_tokenizer("raw")
                .set_index_option(IndexRecordOption::Basic)
                .set_fieldnorms(false);
            let mut raw_opts = TextOptions::default().set_indexing_options(raw_indexing);
            if fc.stored {
                raw_opts = raw_opts.set_stored();
            }
            let raw = builder.add_text_field(&fc.name, raw_opts);
            let endpoint_opts = DateOptions::default()
                .set_indexed()
                .set_fast()
                .set_precision(DateTimePrecision::Milliseconds);
            let start =
                builder.add_date_field(&format!("{}__start", fc.name), endpoint_opts.clone());
            let end = builder.add_date_field(&format!("{}__end", fc.name), endpoint_opts);
            if date_range_fields
                .insert(fc.name.clone(), (start, end))
                .is_some()
            {
                // Same belt-and-braces guard `location` carries: the
                // duplicate-name check above keys on `name` alone.
                bail!(
                    "date_range field `{}` would create synthetic columns that collide with an \
                     existing field; rename the date_range field",
                    fc.name
                );
            }
            field_handles.insert(fc.name.clone(), raw);
            continue;
        }
        let field = match resolved {
            ResolvedType::Str => {
                // Not Tantivy's own `STRING` const: that leaves fieldnorms
                // on, which BM25-length-norms a multivalued string field by
                // its value count — Solr's `StrField` sets `omitNorms=true`
                // and never does that (finding 59's `select_q_field_term`:
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
            // The module's payload field type (#340): one text field, two
            // tokenizers. The fast column is set unconditionally rather than
            // from `fc.fast` -- it is not optional docValues here but the only
            // place the payload is kept, so a `fast = false` declaration would
            // silently leave every `{!payload_score}` scoring 0.
            ResolvedType::BoostTermPayload => {
                let indexing = TextFieldIndexing::default()
                    .set_tokenizer(BOOST_TERM_PAYLOAD_TOKENIZER)
                    .set_index_option(IndexRecordOption::WithFreqsAndPositions);
                let mut opts = TextOptions::default()
                    .set_indexing_options(indexing)
                    .set_fast(Some(BOOST_TERM_PAYLOAD_VERBATIM_TOKENIZER));
                if fc.stored {
                    opts = opts.set_stored();
                }
                builder.add_text_field(&fc.name, opts)
            }
            // `Location` is handled above the match (two synthetic columns),
            // so this arm is unreachable; present only to satisfy the match's
            // exhaustiveness over `ResolvedType`.
            ResolvedType::Location => unreachable!(),
            // Same reason as `Location`: handled above the match (one stored
            // raw field plus two synthetic date columns).
            ResolvedType::DateRange => unreachable!(),
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
        // ponytail: Tantivy gives this shared JSON catch-all one analyzer for
        // every analyzed dynamic rule. Keep the captured Search API configset's
        // Snowball behavior (`day` stays `day`) here; static built-in `text_en`
        // uses the v2 Porter-compatible analyzer independently.
        let tokenizer = if *name == DYNAMIC_TEXT_FIELD {
            DYNAMIC_TEXT_TOKENIZER
        } else {
            "raw"
        };
        // Dot expansion has to be on: the read path splits a dynamic field
        // name on `.` unconditionally. `Term::from_field_json_path`
        // (tantivy-0.26.1 `schema/term.rs:78`) always runs `split_json_path`,
        // which splits on every unescaped `.` regardless of the `expand_dots`
        // argument, so a query for `tm_X3b_en_a.b` always addresses the two
        // segments `tm_X3b_en_a` \x01 `b`. Indexing pushes the whole dynamic
        // name as one JSON key, so without this the write side stores a
        // single segment containing a literal `.` and the two never meet
        // (issue #164: `numFound: 0`, empty `/terms`). With it,
        // `JsonPathWriter::push` does a byte-for-byte `.` -> \x01 swap inside
        // that one push, producing exactly the read path's bytes.
        //
        // ponytail: this changes the on-disk encoding of dotted dynamic field
        // names, so an existing index holding documents with a dotted dynamic
        // name must be reindexed to benefit. On such an index the fix is
        // simply *inert*, not harmful: `CoreIndex::open`
        // (`src/core_index.rs`, the `create_in_dir(..).or_else(open_in_dir)`
        // pair) reopens an existing directory with the schema persisted in its
        // own `meta.json`, where `expand_dots` is still false, and both sides
        // then read that same opened schema -- `term_for_target` takes the
        // flag off `self.index.schema()`, and the writer built from that index
        // encodes with it too. So the pre-fix (broken) one-segment behaviour
        // is preserved end-to-end until a reindex into a fresh data directory;
        // there is no mixed-encoding or partially-corrupt state. Ceiling: no
        // migration and no detection that an opened index predates the fix.
        // Non-dotted names (the overwhelming majority) are unaffected either
        // way -- `push` on a dot-free segment is a no-op.
        let opts = JsonObjectOptions::default()
            .set_expand_dots_enabled()
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
            if !parsed
                .fields
                .iter()
                .any(|field| field.name.as_str() == name)
            {
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
    if !parsed
        .fields
        .iter()
        .any(|field| field.name == parsed.core.default_field)
    {
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
        location_fields,
        date_range_fields,
    })
}

/// Where the schema an index was built with is kept, next to the index itself.
pub fn snapshot_path(data_dir: &Path) -> PathBuf {
    data_dir.join("wayfinder-schema.toml")
}

/// Where the internal analyzer contract for an index is kept. It is separate
/// from the operator-owned schema snapshot because analyzer semantics can
/// change while a TOML field type name remains `text_en`.
pub fn analyzer_contract_path(data_dir: &Path) -> PathBuf {
    data_dir.join("wayfinder-analyzer-contract")
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
