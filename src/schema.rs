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

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tantivy::schema::{
    DateOptions, DateTimePrecision, Field, IndexRecordOption, JsonObjectOptions, NumericOptions,
    Schema, TextFieldIndexing, TextOptions,
};
use tantivy::tokenizer::{
    Language, LowerCaser, RawTokenizer, RemoveLongFilter, SimpleTokenizer, Stemmer, StopWordFilter,
    TextAnalyzer, Token, TokenFilter, TokenStream, Tokenizer, TokenizerManager,
    WhitespaceTokenizer,
};
use unicode_normalization::UnicodeNormalization;
use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};
use unicode_segmentation::UnicodeSegmentation;

use crate::synonyms::SynonymResource;

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

/// Search API Solr's shipped language field-type codes without a Tantivy
/// stemmer. Each is a real suggest dictionary with its own analyzer identity,
/// but shares the unstemmed suggest chain rather than claiming to be `und`.
/// Derived from the unique `field_type_language_code` values in
/// `coverage/search_api_solr_4.4.0_source/config/optional/`
/// `search_api_solr.solr_field_type.text_*.yml` after removing [`LANGUAGES`].
const UNSTEMMED_SUGGEST_LANGUAGES: &[&str] = &[
    "bg", "ca", "cs", "cy", "et", "fa", "ga", "hi", "hr", "id", "ja", "ko", "lv", "nb", "nn", "pl",
    "pt-br", "pt-pt", "sk", "sr", "th", "uk", "zh-hans", "zh-hant",
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

/// Versioned UAX #29 `text_general` identity. v6 and its predecessors remain
/// registered for legacy-index diagnosis; newly-built fields record v7.
const TEXT_GENERAL_TOKENIZER: &str = "wayfinder_text_general_v7";
const TEXT_GENERAL_TOKENIZER_V6: &str = "wayfinder_text_general_v6";
const TEXT_GENERAL_TOKENIZER_V5: &str = "wayfinder_text_general_v5";
const TEXT_GENERAL_TOKENIZER_V4: &str = "wayfinder_text_general_v4";
const TEXT_GENERAL_TOKENIZER_V3: &str = "wayfinder_text_general_v3";
const TEXT_GENERAL_TOKENIZER_LEGACY: &str = "default";
/// Wayfinder's versioned English analyzer uses UAX #29 word segmentation,
/// an inclusive character-length bound, lowercase, English stopword removal,
/// and stemming.
/// It intentionally does not override Tantivy's `en_stem`, which remains
/// available for custom analyzer chains and upstream defaults.
const TEXT_EN_TOKENIZER: &str = "wayfinder_text_en_v7";
/// The v6 predecessor remains registered only so a legacy Tantivy schema
/// can be diagnosed safely; no newly-built field records this identity.
const TEXT_EN_TOKENIZER_V6: &str = "wayfinder_text_en_v6";
/// The Phase-3 predecessor remains registered only so a legacy Tantivy schema
/// can be diagnosed safely; no newly-built field records this identity.
const TEXT_EN_TOKENIZER_V5: &str = "wayfinder_text_en_v5";
/// The Phase-2 predecessor remains registered only so a legacy Tantivy schema
/// can be diagnosed safely; no newly-built field records this identity.
const TEXT_EN_TOKENIZER_V4: &str = "wayfinder_text_en_v4";
/// The #388 predecessor remains registered for migration diagnosis.
const TEXT_EN_TOKENIZER_V3: &str = "wayfinder_text_en_v3";
const TEXT_EN_TOKENIZER_V2: &str = "wayfinder_text_en_v2";
/// The versioned dynamic-text catch-all retains Search API's Snowball behavior
/// while using UAX #29 segmentation. Earlier identities remain registered only
/// for legacy-index diagnosis.
const DYNAMIC_TEXT_TOKENIZER: &str = "wayfinder_dynamic_text_v6";
const DYNAMIC_TEXT_TOKENIZER_V5: &str = "wayfinder_dynamic_text_v5";
const DYNAMIC_TEXT_TOKENIZER_V4: &str = "wayfinder_dynamic_text_v4";
const DYNAMIC_TEXT_TOKENIZER_V3: &str = "wayfinder_dynamic_text_v3";
const DYNAMIC_TEXT_TOKENIZER_V1: &str = "wayfinder_text_en_v1";
const RETIRED_TEXT_TOKENIZERS: [&str; 5] = [
    DYNAMIC_TEXT_TOKENIZER_V1,
    TEXT_EN_TOKENIZER_V2,
    TEXT_EN_TOKENIZER_V3,
    TEXT_EN_TOKENIZER_V4,
    TEXT_EN_TOKENIZER_V5,
];

fn language_tokenizer(code: &str) -> String {
    format!("wayfinder_text_{code}_v6")
}

fn language_tokenizer_v4(code: &str) -> String {
    format!("wayfinder_text_{code}_v4")
}

fn language_tokenizer_v5(code: &str) -> String {
    format!("wayfinder_text_{code}_v5")
}

fn legacy_language_tokenizer(code: &str) -> String {
    format!("text_{code}")
}

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

/// Inclusive Unicode-scalar-value resource bound on the current static
/// `text_en` and `text_general` presets. The one-character lower bound retains
/// the `_default` fixture contract while preventing pathological token sizes.
const STATIC_TEXT_MAX_TOKEN_LEN: usize = 32_766;

/// Search-quality upper bound on `wayfinder_suggest_*` terms. It is measured
/// in characters and inclusive; the chains deliberately carry no byte-based
/// `RemoveLongFilter`, so a 45-byte ASCII token or a 14-character CJK token
/// survives. There is intentionally no lower bound: one-character and CJK
/// tokens are meaningful (#389 Phase 3).
const SUGGEST_MAX_TOKEN_LEN: usize = 100;
/// The `suggest.dictionary` value the shipped `solr.SuggestComponent` gives the
/// stemmer-free `text_und` analyzer, and the fallback for any code Wayfinder has
/// no stemmer for.
const SUGGEST_UNDEFINED_DICTIONARY: &str = "und";

/// The on-disk analyzer contract for built-in text and dynamic analyzers using
/// UAX #29 segmentation. This is separate from Tantivy's schema: it lets
/// startup identify old term formats before their tokenizer identity can be
/// adopted.
pub const ANALYZER_CONTRACT: &str = "text_presets_static_length_v7";
/// A safely adopted index whose unused `_dynamic_text` catch-all still has an
/// older tokenizer identity. It is not full v7 certification: a later rule
/// that starts writing analyzed dynamic values must reindex.
pub const ANALYZER_CONTRACT_LEGACY_DYNAMIC_TEXT: &str =
    "text_presets_static_length_v7_legacy_dynamic_text";
/// The Phase 4 word-delimiter contract, superseded by v7's static text length
/// bound. It is retained to distinguish the changed static presets from every
/// unaffected v6 path during migration.
pub const ANALYZER_CONTRACT_V6: &str = "text_presets_uax29_word_delimiter_v6";
pub const ANALYZER_CONTRACT_V6_LEGACY_DYNAMIC_TEXT: &str =
    "text_presets_uax29_word_delimiter_v6_legacy_dynamic_text";
/// Phase 3's UAX #29 contract, superseded by Phase 4 word-delimiter terms.
pub const ANALYZER_CONTRACT_V5: &str = "text_presets_uax29_v5";
pub const ANALYZER_CONTRACT_V5_LEGACY_DYNAMIC_TEXT: &str =
    "text_presets_uax29_v5_legacy_dynamic_text";
/// Phase 2's accent-folding contract, superseded by UAX #29 segmentation in
/// v5. It is recognized only to fail closed or safely adopt raw-only indexes.
pub const ANALYZER_CONTRACT_V4: &str = "text_presets_accent_folding_v4";
pub const ANALYZER_CONTRACT_V4_LEGACY_DYNAMIC_TEXT: &str =
    "text_presets_accent_folding_v4_legacy_dynamic_text";
/// The first folding contract changed static built-in text terms but left the
/// dynamic catch-all unchanged. Its marker is recognized so static-only and
/// raw-only indexes can upgrade safely while analyzed dynamic indexes reindex.
pub const ANALYZER_CONTRACT_V3: &str = "text_en_solr_length_case_v3";
/// Retained for indexes created by pre-release Phase 2 builds.
pub const ANALYZER_CONTRACT_V3_LEGACY_DYNAMIC_TEXT: &str =
    "text_en_porter_compatible_v3_legacy_dynamic_text";
/// The pre-folding Porter-compatible contract.
pub const ANALYZER_CONTRACT_V2: &str = "text_en_porter_compatible_v2";
pub const ANALYZER_CONTRACT_V2_LEGACY_DYNAMIC_TEXT: &str =
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
    /// Issue #389 Phase 1 seam: a distinct query-time tokenizer, registered
    /// alongside `tokenizer`'s index-time analyzer. `None` means this field
    /// type has no separate query analyzer, so the query path must fall back
    /// to the index one.
    #[serde(default)]
    pub query_tokenizer: Option<String>,
    /// The filter chain for `query_tokenizer`. Only consulted when
    /// `query_tokenizer` is `Some`.
    #[serde(default)]
    pub query_filters: Vec<FilterConfig>,
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
    /// The **query**-side twin of `tokenizers` (issue #389 Phase 1): a second,
    /// complete manager in which every chain is registered under the *same*
    /// identity it has in `tokenizers` — its **index** identity. A chain that
    /// declares a query analyzer registers that analyzer here; a chain that does
    /// not registers its index analyzer here as well. So every identity resolves
    /// in both managers, and the two differ only where a chain genuinely has two
    /// analyzers.
    ///
    /// Keying by the index identity is what makes the seam reachable from
    /// Tantivy's own `QueryParser`, which resolves a field's analyzer *inside*
    /// Tantivy from the manager it was handed, keyed by the tokenizer name in the
    /// schema — i.e. the index identity. `QueryParser::new(schema, fields,
    /// query_tokenizers)` is the whole of that wiring; a separate query-identity
    /// namespace would have been unreachable from there.
    ///
    /// Only the index identity is ever written into the Tantivy schema, so an
    /// entry here changes query-time analysis only and never a term on disk
    /// (which is why `ANALYZER_CONTRACT` does not move).
    pub query_tokenizers: TokenizerManager,
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

    /// Whether this schema can contain static `text_en` or `text_general`
    /// terms from the v6 chain whose 40-byte cutoff v7 replaces with an
    /// inclusive Unicode-scalar-value bound.
    pub fn uses_changed_static_text(&self) -> bool {
        self.fields
            .iter()
            .any(|field| matches!(field.type_.as_str(), "text_en" | "text_general"))
    }

    /// Whether the schema can contain static `text_en` terms from a pre-v2 chain.
    pub fn uses_static_text_en(&self) -> bool {
        self.fields.iter().any(|field| field.type_ == "text_en")
    }

    /// Whether a static built-in text preset has indexed terms subject to
    /// Phase 2 accent folding. Custom and dynamic chains are not included:
    /// neither receives this built-in filter.
    pub fn uses_static_accent_folded_text(&self) -> bool {
        self.fields.iter().any(|field| {
            field.type_ == "text_general"
                || field.type_ == "text_en"
                || LANGUAGES
                    .iter()
                    .any(|(code, _)| field.type_ == format!("text_{code}"))
        })
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

    /// The identity a **query**-time path looks up in [`Self::query_tokenizers`],
    /// given the index-time identity it found on the field (issue #389 Phase 1) —
    /// which is that same identity, unchanged. There is no query-side name
    /// namespace: the query manager is keyed by index identity, precisely so
    /// Tantivy's `QueryParser` can resolve a field's query analyzer itself from
    /// the tokenizer name in the schema. This function exists to state that
    /// invariant in one place and is what
    /// `suggest_dictionary_tokenizer_query_lookup_is_unaffected` pins.
    ///
    /// The reason the two sides need to be able to differ at all is search
    /// quality, not symmetry: synonym expansion belongs on the query side, where
    /// it costs nothing on disk and the table can change without a reindex, and
    /// aggressive splitting/recombining of delimited tokens is worth more on the
    /// index side, where it is paid for once. That difference lives in *which
    /// analyzer* the two managers hold under this one name, never in the name.
    pub fn query_tokenizer_name(&self, index_tokenizer_name: &str) -> String {
        index_tokenizer_name.to_string()
    }

    /// Query-time counterpart to [`Self::tokenize`]: analyzes `text` with
    /// `type_name`'s **query** analyzer — the same identity resolved against
    /// [`Self::query_tokenizers`] instead of `tokenizers` (issue #389 Phase 1).
    /// A type that declares no query chain has its index analyzer registered in
    /// both managers, so this equals [`Self::tokenize`] for it.
    pub fn tokenize_query(&self, type_name: &str, text: &str) -> Option<Vec<String>> {
        let tokenizer_name = match resolve_type(type_name, &self.field_types).ok()? {
            ResolvedType::Str => "raw".to_string(),
            ResolvedType::Text { tokenizer } => tokenizer,
            ResolvedType::BoostTermPayload => BOOST_TERM_PAYLOAD_TOKENIZER.to_string(),
            _ => return None,
        };
        let mut analyzer = self.query_tokenizers.get(&tokenizer_name)?;
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
                    tokenizer: language_tokenizer(code),
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

/// Folds accents after tokenization, keeping Tantivy's original byte offsets.
///
/// This deliberately is not a Solr-style char filter: a char filter would need
/// an offset-correction map for expansions such as `ß` -> `ss`, while changing
/// only `Token::text` leaves offsets into the original query intact for suggest
/// prefix matching and highlighting (#389 Phase 2). UAX#29 already treats the
/// affected characters as letters, so folding after tokenization cannot move a
/// token boundary.
#[derive(Clone)]
struct AccentFoldingFilter;

impl TokenFilter for AccentFoldingFilter {
    type Tokenizer<T: Tokenizer> = AccentFoldingTokenizer<T>;

    fn transform<T: Tokenizer>(self, tokenizer: T) -> Self::Tokenizer<T> {
        AccentFoldingTokenizer {
            inner: tokenizer,
            buffer: String::new(),
        }
    }
}

#[derive(Clone)]
struct AccentFoldingTokenizer<T> {
    inner: T,
    buffer: String,
}

impl<T: Tokenizer> Tokenizer for AccentFoldingTokenizer<T> {
    type TokenStream<'a> = AccentFoldingTokenStream<'a, T::TokenStream<'a>>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        AccentFoldingTokenStream {
            tail: self.inner.token_stream(text),
            buffer: &mut self.buffer,
        }
    }
}

struct AccentFoldingTokenStream<'a, T> {
    tail: T,
    buffer: &'a mut String,
}

/// The shared folding primitive for search analyzers and persisted synonym
/// members. It intentionally preserves case; `LowerCaser` runs after this
/// filter in the production chain.
pub(crate) fn fold_accents(text: &str, output: &mut String) {
    output.clear();
    for character in text.nfkd() {
        if character.general_category() == GeneralCategory::NonspacingMark {
            continue;
        }
        match character {
            'ß' => output.push_str("ss"),
            'ẞ' => output.push_str("SS"),
            'æ' => output.push_str("ae"),
            'Æ' => output.push_str("AE"),
            'œ' => output.push_str("oe"),
            'Œ' => output.push_str("OE"),
            'þ' => output.push_str("th"),
            'Þ' => output.push_str("TH"),
            'ð' => output.push('d'),
            'Ð' => output.push('D'),
            'ø' => output.push('o'),
            'Ø' => output.push('O'),
            'đ' => output.push('d'),
            'Đ' => output.push('D'),
            'ł' => output.push('l'),
            'Ł' => output.push('L'),
            character => output.push(character),
        }
    }
}

impl<T: TokenStream> TokenStream for AccentFoldingTokenStream<'_, T> {
    fn advance(&mut self) -> bool {
        if !self.tail.advance() {
            return false;
        }

        fold_accents(&self.tail.token().text, self.buffer);
        let text = &mut self.tail.token_mut().text;
        text.clear();
        text.push_str(self.buffer);
        true
    }

    fn token(&self) -> &Token {
        self.tail.token()
    }

    fn token_mut(&mut self) -> &mut Token {
        self.tail.token_mut()
    }
}

/// UAX #29 word-boundary tokenizer for Wayfinder's built-in search analyzers.
///
/// `unicode-segmentation` returns every UAX #29 boundary segment, including
/// punctuation and whitespace. Whitespace-only and Unicode-punctuation-only
/// segments are discarded; every other segment, including emoji, symbols, and
/// combining-mark-only segments, becomes a term. The slice offsets come
/// directly from the original input, so `Token` offsets remain original byte
/// offsets for query prefix matching and highlighting.
#[derive(Clone, Default)]
struct Uax29Tokenizer;

impl Tokenizer for Uax29Tokenizer {
    type TokenStream<'a> = Uax29TokenStream<'a>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        Uax29TokenStream {
            segments: text
                .split_word_bound_indices()
                .collect::<Vec<_>>()
                .into_iter(),
            token: Token::default(),
            position: 0,
        }
    }
}

struct Uax29TokenStream<'a> {
    segments: std::vec::IntoIter<(usize, &'a str)>,
    token: Token,
    position: usize,
}

impl TokenStream for Uax29TokenStream<'_> {
    fn advance(&mut self) -> bool {
        for (offset_from, segment) in self.segments.by_ref() {
            if segment.chars().all(char::is_whitespace)
                || segment.chars().all(|character| {
                    matches!(
                        character.general_category(),
                        GeneralCategory::ConnectorPunctuation
                            | GeneralCategory::DashPunctuation
                            | GeneralCategory::ClosePunctuation
                            | GeneralCategory::FinalPunctuation
                            | GeneralCategory::InitialPunctuation
                            | GeneralCategory::OtherPunctuation
                            | GeneralCategory::OpenPunctuation
                    )
                })
            {
                continue;
            }
            self.token = Token {
                offset_from,
                offset_to: offset_from + segment.len(),
                position: self.position,
                position_length: 1,
                text: segment.to_owned(),
            };
            self.position += 1;
            return true;
        }
        false
    }

    fn token(&self) -> &Token {
        &self.token
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.token
    }
}

/// Symmetric search-quality word delimiter expansion (#389 Phase 4).
///
/// Every original UAX word is retained, alongside its case/number/delimiter
/// parts and their catenation. A punctuation-separated UAX run, such as
/// `SKU-42`, is one compound too, so it gains `sku42`; whitespace always ends
/// that run. All alternatives share a position; this is a graph in the
/// token-stream sense, but query and index chains intentionally emit the same
/// forms.
#[derive(Clone)]
struct WordDelimiterFilter {
    preserve_original: bool,
}

impl WordDelimiterFilter {
    fn search() -> Self {
        Self {
            preserve_original: true,
        }
    }

    fn suggest() -> Self {
        Self {
            preserve_original: false,
        }
    }
}

impl TokenFilter for WordDelimiterFilter {
    type Tokenizer<T: Tokenizer> = WordDelimiterTokenizer<T>;

    fn transform<T: Tokenizer>(self, tokenizer: T) -> Self::Tokenizer<T> {
        WordDelimiterTokenizer {
            inner: tokenizer,
            preserve_original: self.preserve_original,
        }
    }
}

#[derive(Clone)]
struct WordDelimiterTokenizer<T> {
    inner: T,
    preserve_original: bool,
}

impl<T: Tokenizer> Tokenizer for WordDelimiterTokenizer<T> {
    type TokenStream<'a> = WordDelimiterTokenStream<'a, T::TokenStream<'a>>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        WordDelimiterTokenStream {
            tail: self.inner.token_stream(text),
            input: text,
            pending: VecDeque::new(),
            lookahead: None,
            position_shift: 0,
            preserve_original: self.preserve_original,
        }
    }
}

#[derive(Clone)]
struct DelimiterPart {
    text: String,
    offset_from: usize,
    offset_to: usize,
}

fn delimiter_parts(text: &str, base_offset: usize, original_end: usize) -> Vec<DelimiterPart> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    // Accent folding can change byte width while retaining source offsets. In
    // that case individual split bounds are unknowable here, so keep the full
    // original token range rather than risking a non-UTF-8 suggest slice.
    let offsets_are_exact = text.len() == original_end - base_offset;
    let offsets = |from: usize, to: usize| {
        if offsets_are_exact {
            (base_offset + from, base_offset + to)
        } else {
            (base_offset, original_end)
        }
    };
    let mut parts = Vec::new();
    let mut start = None;
    for (index, (offset, current)) in chars.iter().copied().enumerate() {
        let previous = index.checked_sub(1).map(|previous| chars[previous].1);
        let next = chars.get(index + 1).map(|(_, character)| *character);
        let boundary = previous.is_some_and(|previous| {
            (previous.is_alphabetic() && current.is_numeric())
                || (previous.is_numeric() && current.is_alphabetic())
                || (previous.is_lowercase() && current.is_uppercase())
                || (previous.is_uppercase()
                    && current.is_uppercase()
                    && next.is_some_and(char::is_lowercase))
        });
        if !current.is_alphanumeric() {
            if let Some(start) = start.take() {
                let (offset_from, offset_to) = offsets(start, offset);
                parts.push(DelimiterPart {
                    text: text[start..offset].to_string(),
                    offset_from,
                    offset_to,
                });
            }
        } else if boundary {
            if let Some(start) = start.replace(offset) {
                let (offset_from, offset_to) = offsets(start, offset);
                parts.push(DelimiterPart {
                    text: text[start..offset].to_string(),
                    offset_from,
                    offset_to,
                });
            }
        } else if start.is_none() {
            start = Some(offset);
        }
    }
    if let Some(start) = start {
        let (offset_from, offset_to) = offsets(start, text.len());
        parts.push(DelimiterPart {
            text: text[start..].to_string(),
            offset_from,
            offset_to,
        });
    }
    // UAX deliberately retains non-punctuation symbols (for example emoji) as
    // meaningful terms. They have no alphanumeric run, but must remain a graph
    // part rather than disappearing or being catenated to a linked word.
    if parts.is_empty() && !text.is_empty() {
        let (offset_from, offset_to) = offsets(0, text.len());
        parts.push(DelimiterPart {
            text: text.to_string(),
            offset_from,
            offset_to,
        });
    }
    parts
}

/// Canonicalizes one configured synonym member through the same pre-synonym
/// query analysis steps. Synonym expansion can only add one same-position,
/// position-length-one alternative, so the member must be one actual UAX term
/// and its pre-lowercase delimiter graph must produce exactly that one term.
pub(crate) fn canonicalize_synonym_member(member: &str) -> Result<String> {
    let mut folded = String::new();
    fold_accents(member, &mut folded);

    // The analyzer, rather than a hand-maintained script or numeric blacklist,
    // decides whether a member is safe. Folding is included because it is part
    // of the query chain and compatibility ideographs can change under NFKD.
    let mut uax = Uax29Tokenizer;
    let mut uax_stream = uax.token_stream(&folded);
    if !uax_stream.advance() || uax_stream.advance() {
        bail!("synonym members must each be one nonempty UAX token");
    }
    let mut delimiter = TextAnalyzer::builder(Uax29Tokenizer)
        .filter_dynamic(WordDelimiterFilter::search())
        .build();
    let mut graph = delimiter.token_stream(&folded);
    if !graph.advance() {
        bail!("synonym members must each be one nonempty safe token");
    }
    let token = graph.token().clone();
    if token.position_length != 1 || graph.advance() {
        bail!("synonym members must not split across delimiter positions");
    }

    let mut canonical = String::new();
    for character in token.text.chars() {
        // Match the simple, one-scalar lowercase filter used by the current
        // static English/general and dynamic query chains.
        canonical.push(character.to_lowercase().next().unwrap_or(character));
    }

    // A synonym is core-wide, so it must survive the strictest pre-synonym
    // built-in chain. Static text drops members over its inclusive character
    // bound and English chains remove their stopwords before expansion;
    // accepting either would create a member that can never expand
    // symmetrically.
    let english_stopwords =
        StopWordFilter::new(Language::English).expect("Tantivy ships English stopwords");
    let mut acceptance = TextAnalyzer::builder(RawTokenizer::default())
        .filter_dynamic(LengthFilter {
            min: 1,
            max: STATIC_TEXT_MAX_TOKEN_LEN,
        })
        .filter(english_stopwords)
        .build();
    {
        let mut accepted = acceptance.token_stream(&canonical);
        if !accepted.advance() {
            bail!("synonym members must survive built-in length and stopword filters");
        }
    }
    Ok(canonical)
}

struct WordDelimiterTokenStream<'a, T> {
    tail: T,
    input: &'a str,
    pending: VecDeque<Token>,
    lookahead: Option<Token>,
    /// Delimiter expansion can widen or narrow a graph relative to the span
    /// of upstream positions it consumed. Later positions carry this signed
    /// cumulative shift instead of being compacted or underflowing.
    position_shift: isize,
    preserve_original: bool,
}

fn is_punctuation(character: char) -> bool {
    matches!(
        character.general_category(),
        GeneralCategory::ConnectorPunctuation
            | GeneralCategory::DashPunctuation
            | GeneralCategory::ClosePunctuation
            | GeneralCategory::FinalPunctuation
            | GeneralCategory::InitialPunctuation
            | GeneralCategory::OtherPunctuation
            | GeneralCategory::OpenPunctuation
    )
}

/// UAX can emit the sides of `SKU-42` as separate tokens. Buffering the next
/// UAX token lets this filter preserve one compound graph without ever joining
/// across whitespace.
fn is_compound_separator(gap: &str) -> bool {
    !gap.is_empty() && gap.chars().all(is_punctuation)
}

impl<T: TokenStream> WordDelimiterTokenStream<'_, T> {
    fn next_source(&mut self) -> Option<Token> {
        self.lookahead
            .take()
            .or_else(|| self.tail.advance().then(|| self.tail.token().clone()))
    }

    fn fill_compound(&mut self) -> bool {
        let Some(first) = self.next_source() else {
            return false;
        };
        let mut sources = vec![first];
        while let Some(next) = self.next_source() {
            let previous = sources.last().expect("compound has its first token");
            let gap = &self.input[previous.offset_to..next.offset_from];
            if is_compound_separator(gap) {
                sources.push(next);
            } else {
                self.lookahead = Some(next);
                break;
            }
        }

        let compound_start = sources[0].offset_from;
        let compound_end = sources.last().expect("compound is nonempty").offset_to;
        let parts: Vec<DelimiterPart> = sources
            .iter()
            .flat_map(|source| delimiter_parts(&source.text, source.offset_from, source.offset_to))
            .collect();
        let upstream_start = sources[0].position;
        let Some(upstream_end) = sources
            .last()
            .and_then(|source| source.position.checked_add(source.position_length))
        else {
            return false;
        };
        let Some(upstream_span) = upstream_end.checked_sub(upstream_start) else {
            return false;
        };
        let width = parts.len();
        if parts.is_empty() {
            // Accent folding removes combining-only input. It still consumes
            // an upstream span, so carry its negative graph delta forward.
            return self.apply_position_delta(width, upstream_span);
        }

        // Map every graph start from its upstream position. A split such as
        // `sku42` consumes one UAX position but emits two, so it shifts every
        // later upstream position by one; an already-present stopword gap must
        // remain a gap after that expansion. checked_add_signed also prevents
        // a narrowing delta from producing a negative output position.
        let Some(start) = upstream_start.checked_add_signed(self.position_shift) else {
            return false;
        };
        let mut seen = HashSet::new();
        let mut push =
            |text: String, position: usize, position_length: usize, from: usize, to: usize| {
                if !text.is_empty() && seen.insert((text.clone(), position, position_length)) {
                    self.pending.push_back(Token {
                        offset_from: from,
                        offset_to: to,
                        position,
                        position_length,
                        text,
                    });
                }
            };
        for (index, part) in parts.iter().enumerate() {
            push(
                part.text.clone(),
                start + index,
                1,
                part.offset_from,
                part.offset_to,
            );
        }
        let source_has_symbol = sources.iter().any(|source| {
            source
                .text
                .chars()
                .any(|character| !character.is_alphanumeric() && !is_punctuation(character))
        }) || self.input[compound_start..compound_end].contains('@');
        let compound_has_dash = sources
            .windows(2)
            .any(|pair| self.input[pair[0].offset_to..pair[1].offset_from].contains('-'));
        let compound_has_number = sources
            .iter()
            .any(|source| source.text.chars().any(char::is_numeric));
        if self.preserve_original
            && !source_has_symbol
            && (!compound_has_dash || compound_has_number)
        {
            // Use the upstream token text, not the raw input slice: filters may
            // already have lowercased/folded it before this wrapper runs. Raw
            // text here would create a second differently-cased alternative.
            let mut preserved = sources[0].text.clone();
            for pair in sources.windows(2) {
                preserved.push_str(&self.input[pair[0].offset_to..pair[1].offset_from]);
                preserved.push_str(&pair[1].text);
            }
            push(preserved, start, width, compound_start, compound_end);
        }
        // UAX may deliver an address as one punctuation-linked compound. Keep
        // the right-hand `b.com`/`bcom` forms as well as its all-parts spelling.
        if let Some(symbol_offset) = self.input[compound_start..compound_end].find('@') {
            let suffix_offset = compound_start + symbol_offset + '@'.len_utf8();
            let suffix = &self.input[suffix_offset..compound_end];
            let suffix_parts = delimiter_parts(suffix, suffix_offset, compound_end);
            if let Some(last) = suffix_parts.last() {
                let suffix_start = start + parts.len() - suffix_parts.len();
                push(
                    suffix.to_string(),
                    suffix_start,
                    suffix_parts.len(),
                    suffix_parts[0].offset_from,
                    last.offset_to,
                );
                push(
                    suffix_parts.iter().map(|part| part.text.as_str()).collect(),
                    suffix_start,
                    suffix_parts.len(),
                    suffix_parts[0].offset_from,
                    last.offset_to,
                );
            }
        }
        if !source_has_symbol {
            let all = parts
                .iter()
                .map(|part| part.text.as_str())
                .collect::<String>();
            push(all, start, width, compound_start, compound_end);
        }

        let mut run_start = 0;
        while run_start < parts.len() {
            if !parts[run_start].text.chars().all(char::is_alphabetic) {
                run_start += 1;
                continue;
            }
            let mut run_end = run_start + 1;
            while run_end < parts.len() && parts[run_end].text.chars().all(char::is_alphabetic) {
                run_end += 1;
            }
            if run_end - run_start > 1 {
                push(
                    parts[run_start..run_end]
                        .iter()
                        .map(|part| part.text.as_str())
                        .collect(),
                    start + run_start,
                    run_end - run_start,
                    parts[run_start].offset_from,
                    parts[run_end - 1].offset_to,
                );
            }
            run_start = run_end;
        }
        // Token streams must be monotonic by position. The catenated forms are
        // created after the sequential parts above, so restore graph order
        // before downstream filters/indexing observe the batch.
        self.pending
            .make_contiguous()
            .sort_by_key(|token| token.position);
        self.apply_position_delta(width, upstream_span)
    }

    /// Apply the graph-width delta against the positions actually covered by
    /// the upstream graph. Source count is not that span: punctuation-linked
    /// emoji and folded-away combining marks make the distinction observable.
    fn apply_position_delta(&mut self, width: usize, upstream_span: usize) -> bool {
        let Some(width) = isize::try_from(width).ok() else {
            return false;
        };
        let Some(upstream_span) = isize::try_from(upstream_span).ok() else {
            return false;
        };
        let Some(delta) = width.checked_sub(upstream_span) else {
            return false;
        };
        let Some(shift) = self.position_shift.checked_add(delta) else {
            return false;
        };
        self.position_shift = shift;
        true
    }
}

impl<T: TokenStream> TokenStream for WordDelimiterTokenStream<'_, T> {
    fn advance(&mut self) -> bool {
        loop {
            if let Some(token) = self.pending.pop_front() {
                *self.tail.token_mut() = token;
                return true;
            }
            if !self.fill_compound() {
                return false;
            }
        }
    }

    fn token(&self) -> &Token {
        self.tail.token()
    }

    fn token_mut(&mut self) -> &mut Token {
        self.tail.token_mut()
    }
}

/// Query-side expand-style synonym filter. The resource is an Arc<RwLock>, so
/// every analyzer clone sees a hot replacement without rebuilding a Tantivy
/// index. Shipped tables are single-token groups, therefore this emits stacked
/// position-length-one alternatives; word delimiter expansion is the only
/// graph source for that data.
#[derive(Clone)]
struct SynonymFilter {
    resource: SynonymResource,
}

impl TokenFilter for SynonymFilter {
    type Tokenizer<T: Tokenizer> = SynonymTokenizer<T>;

    fn transform<T: Tokenizer>(self, tokenizer: T) -> Self::Tokenizer<T> {
        SynonymTokenizer {
            inner: tokenizer,
            resource: self.resource,
        }
    }
}

#[derive(Clone)]
struct SynonymTokenizer<T> {
    inner: T,
    resource: SynonymResource,
}

impl<T: Tokenizer> Tokenizer for SynonymTokenizer<T> {
    type TokenStream<'a> = SynonymTokenStream<T::TokenStream<'a>>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        SynonymTokenStream {
            tail: self.inner.token_stream(text),
            resource: self.resource.clone(),
            pending: VecDeque::new(),
        }
    }
}

struct SynonymTokenStream<T> {
    tail: T,
    resource: SynonymResource,
    pending: VecDeque<Token>,
}

impl<T: TokenStream> TokenStream for SynonymTokenStream<T> {
    fn advance(&mut self) -> bool {
        if let Some(token) = self.pending.pop_front() {
            *self.tail.token_mut() = token;
            return true;
        }
        if !self.tail.advance() {
            return false;
        }
        let source = self.tail.token().clone();
        for text in self.resource.expansions(&source.text) {
            self.pending.push_back(Token {
                text,
                ..source.clone()
            });
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

/// Lucene's `LowerCaseFilterFactory`, which is `Character.toLowerCase` --
/// Unicode's **simple** (1:1) case mapping. Tantivy's `LowerCaser` uses Rust's
/// `str::to_lowercase`, which applies the **full** mapping, and the two disagree
/// on the characters whose full lowercasing expands: `İ` (U+0130) folds to a
/// bare `i` under Java's mapping but to `i` + U+0307 COMBINING DOT ABOVE under
/// Rust's. That difference is observable on the suggest read path --
/// `suggest_q_multibyte_grow_en` pins `suggest.q=istanbul` matching
/// `İstanbul airport`, which it cannot do if the phrase token carries a trailing
/// combining dot the query token does not.
///
/// `char::to_lowercase().next()` IS Java's simple mapping wherever the two
/// differ: Rust's iterator yields the full expansion, whose first character is
/// the simple one. (U+212A KELVIN SIGN, the other multi-byte character in the
/// #384 corpus, already agrees between the two -- it folds to `k` either way.)
#[derive(Clone)]
struct SimpleLowerCaseFilter;

impl TokenFilter for SimpleLowerCaseFilter {
    type Tokenizer<T: Tokenizer> = SimpleLowerCaseTokenizer<T>;

    fn transform<T: Tokenizer>(self, tokenizer: T) -> Self::Tokenizer<T> {
        SimpleLowerCaseTokenizer { inner: tokenizer }
    }
}

#[derive(Clone)]
struct SimpleLowerCaseTokenizer<T> {
    inner: T,
}

impl<T: Tokenizer> Tokenizer for SimpleLowerCaseTokenizer<T> {
    type TokenStream<'a> = SimpleLowerCaseTokenStream<T::TokenStream<'a>>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        SimpleLowerCaseTokenStream {
            tail: self.inner.token_stream(text),
            buffer: String::new(),
        }
    }
}

struct SimpleLowerCaseTokenStream<T> {
    tail: T,
    /// Scratch space, reused across tokens so the common already-lowercase case
    /// costs one pass and no allocation.
    buffer: String,
}

impl<T: TokenStream> TokenStream for SimpleLowerCaseTokenStream<T> {
    fn advance(&mut self) -> bool {
        if !self.tail.advance() {
            return false;
        }
        let text = &mut self.tail.token_mut().text;
        if text.chars().any(|c| c.to_lowercase().next() != Some(c)) {
            self.buffer.clear();
            for c in text.chars() {
                // `to_lowercase()` never yields an empty iterator, so the
                // `unwrap_or(c)` is unreachable defensive cover, not a case.
                self.buffer.push(c.to_lowercase().next().unwrap_or(c));
            }
            text.clear();
            text.push_str(&self.buffer);
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

/// Solr's `RemoveDuplicatesTokenFilterFactory`: drops a token whose text
/// duplicates one already emitted *at the same position*. Tokens at different
/// positions are never duplicates, which is why d3's two `dog|...` values
/// (the dedicated payload-score corpus) both survive -- consecutive multiValued
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

/// The tokenizer identity of the suggest-path analyzer for one dictionary
/// (language) code. Namespaced away from the `text_*` presets on purpose: the
/// shipped `suggestAnalyzerFieldType` chain is NOT the same chain as Wayfinder's
/// global `text_en` / `text_general` presets, and conflating them is what made
/// the read path diverge (see [`build_tokenizers`]).
fn suggest_tokenizer(code: &str) -> String {
    format!("wayfinder_suggest_{code}_v4")
}

/// The analyzer (Tantivy tokenizer name) a `suggest.dictionary` value
/// selects, mirroring the shipped `solr.SuggestComponent`'s
/// `suggestAnalyzerFieldType` per-dictionary analyzer: `text_en` for `en`,
/// and the unstemmed chain for `und` plus installed Search API field-type codes
/// lacking Tantivy stemmers. The dictionary name is the language code the
/// Suggester plugin passes (`Suggester.php` derives it from the langcode tag),
/// and every configured code retains that identity. Explicit unknown
/// dictionaries are rejected by [`is_configured_suggester`] before this lookup;
/// the fallback keeps the absent-dictionary `und` default. Used by the
/// `/suggest?suggest.q=` read path (issue #384).
///
/// These are the dedicated `wayfinder_suggest_*` chains, not the `text_*`
/// presets, because the shipped suggest field types carry filters the presets do
/// not -- see [`build_tokenizers`] for the chain and its remaining ceiling.
pub fn dictionary_tokenizer(dictionary: &str) -> String {
    if LANGUAGES.iter().any(|(code, _)| *code == dictionary)
        || UNSTEMMED_SUGGEST_LANGUAGES.contains(&dictionary)
    {
        suggest_tokenizer(dictionary)
    } else {
        suggest_tokenizer(SUGGEST_UNDEFINED_DICTIONARY)
    }
}

/// Whether a requested dictionary has a registered per-language suggester.
pub fn is_configured_suggester(dictionary: &str) -> bool {
    dictionary == SUGGEST_UNDEFINED_DICTIONARY
        || LANGUAGES.iter().any(|(code, _)| *code == dictionary)
        || UNSTEMMED_SUGGEST_LANGUAGES.contains(&dictionary)
}

/// Whether an on-disk tokenizer identity uses issue #389's built-in token graph.
/// v6 static identities remain graph analyzers for legacy-index diagnosis.
pub fn is_current_builtin_graph_tokenizer(tokenizer: &str) -> bool {
    matches!(
        tokenizer,
        TEXT_GENERAL_TOKENIZER
            | TEXT_GENERAL_TOKENIZER_V6
            | TEXT_EN_TOKENIZER
            | TEXT_EN_TOKENIZER_V6
            | DYNAMIC_TEXT_TOKENIZER
    ) || LANGUAGES
        .iter()
        .filter(|(code, _)| *code != "en")
        .any(|(code, _)| tokenizer == language_tokenizer(code))
}

/// Which of a `[[field_types]]` entry's two chains [`build_analyzer`] is
/// building (issue #389 Phase 1). Also names the config key an error message has
/// to point an operator at: `tokenizer` and `query_tokenizer` accept the same
/// values, so an error that names neither leaves the operator guessing which of
/// the two lines to fix.
#[derive(Copy, Clone)]
enum AnalyzerSide {
    Index,
    Query,
}

impl AnalyzerSide {
    /// The `[[field_types]]` key holding this side's tokenizer.
    fn tokenizer_key(self) -> &'static str {
        match self {
            Self::Index => "tokenizer",
            Self::Query => "query_tokenizer",
        }
    }
}

/// Builds the `TextAnalyzer` for one side of a `[[field_types]]` chain: the
/// index-side one (`tokenizer` + `filters`) or the query-side one
/// (`query_tokenizer` + `query_filters`). The two share every tokenizer and
/// filter kind; only which pair of config fields they read from differs
/// (issue #389 Phase 1).
fn build_analyzer(ft: &FieldTypeConfig, side: AnalyzerSide) -> Result<TextAnalyzer> {
    let (tokenizer, filters) = match side {
        AnalyzerSide::Query => {
            let tokenizer = ft.query_tokenizer.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "field type `{}` has no `query_tokenizer` to build a query analyzer from",
                    ft.name
                )
            })?;
            (tokenizer, &ft.query_filters)
        }
        AnalyzerSide::Index => (ft.tokenizer.as_str(), &ft.filters),
    };
    let mut builder = match tokenizer {
        "simple" => TextAnalyzer::builder(SimpleTokenizer::default()).dynamic(),
        other => bail!(
            "unsupported tokenizer `{other}` on `{}` of field type `{}` (supported: `simple`)",
            side.tokenizer_key(),
            ft.name
        ),
    };
    builder = builder.filter_dynamic(RemoveLongFilter::limit(40));

    for filter in filters {
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
///
/// Returns **two** complete managers, index-side and query-side (issue #389
/// Phase 1). Every chain is registered in both, under the same identity — the
/// index one — so any name resolves in either manager and the two differ only
/// where a chain genuinely has two analyzers. A `[[field_types]]` entry
/// declaring `query_tokenizer` puts its query analyzer into the query manager;
/// every other chain puts its index analyzer there, which is what makes the
/// "no query chain declared" case *a registration* rather than a lookup-time
/// fallback.
///
/// Keying the query manager by the index identity is not cosmetic: it is what
/// lets `QueryParser::new(schema, fields, query_manager)` reach the query
/// analyzers at all. Tantivy resolves a field's analyzer internally from the
/// manager it was handed, keyed by the tokenizer name recorded in the schema,
/// which is always the index identity (`QueryParser::for_index` is exactly
/// `QueryParser::new(index.schema(), fields, index.tokenizers().clone())`).
fn build_tokenizers(
    field_types: &[FieldTypeConfig],
    synonyms: &SynonymResource,
) -> Result<(TokenizerManager, TokenizerManager)> {
    let manager = TokenizerManager::default();
    let query_manager = TokenizerManager::default();
    // Registers one analyzer as *both* sides of a chain. Built-in, dynamic,
    // and suggest chains use the same UAX #29 tokenizer at index and query time
    // so punctuation boundaries and meaningful singletons remain symmetric.
    let register_both = |name: &str, analyzer: TextAnalyzer| {
        manager.register(name, analyzer.clone());
        query_manager.register(name, analyzer);
    };
    let register_split =
        |name: &str, index_analyzer: TextAnalyzer, query_analyzer: TextAnalyzer| {
            manager.register(name, index_analyzer);
            query_manager.register(name, query_analyzer);
        };
    let english_stopwords =
        || StopWordFilter::new(Language::English).expect("Tantivy ships an English stopword list");
    // Keep every pre-folding identity registered with its original chain. An
    // old schema can therefore be opened only long enough for migration
    // diagnosis; no new field is allowed to record one of these names.
    register_both(
        TEXT_GENERAL_TOKENIZER_LEGACY,
        TextAnalyzer::builder(SimpleTokenizer::default())
            .filter(RemoveLongFilter::limit(40))
            .filter(LowerCaser)
            .build(),
    );
    register_both(
        TEXT_GENERAL_TOKENIZER_V3,
        TextAnalyzer::builder(SimpleTokenizer::default())
            .filter(RemoveLongFilter::limit(40))
            .filter_dynamic(SimpleLowerCaseFilter)
            .build(),
    );
    register_both(
        TEXT_GENERAL_TOKENIZER_V4,
        TextAnalyzer::builder(SimpleTokenizer::default())
            .filter_dynamic(AccentFoldingFilter)
            .filter(RemoveLongFilter::limit(40))
            .filter_dynamic(SimpleLowerCaseFilter)
            .build(),
    );
    register_both(
        TEXT_GENERAL_TOKENIZER_V5,
        TextAnalyzer::builder(Uax29Tokenizer)
            .filter_dynamic(AccentFoldingFilter)
            .filter(RemoveLongFilter::limit(40))
            .filter_dynamic(SimpleLowerCaseFilter)
            .build(),
    );
    register_split(
        TEXT_GENERAL_TOKENIZER_V6,
        TextAnalyzer::builder(Uax29Tokenizer)
            .filter_dynamic(AccentFoldingFilter)
            .filter_dynamic(WordDelimiterFilter::search())
            .filter(RemoveLongFilter::limit(40))
            .filter_dynamic(SimpleLowerCaseFilter)
            .build(),
        TextAnalyzer::builder(Uax29Tokenizer)
            .filter_dynamic(AccentFoldingFilter)
            .filter_dynamic(WordDelimiterFilter::search())
            .filter(RemoveLongFilter::limit(40))
            .filter_dynamic(SimpleLowerCaseFilter)
            .filter_dynamic(SynonymFilter {
                resource: synonyms.clone(),
            })
            .build(),
    );
    register_split(
        TEXT_GENERAL_TOKENIZER,
        TextAnalyzer::builder(Uax29Tokenizer)
            .filter_dynamic(AccentFoldingFilter)
            .filter_dynamic(WordDelimiterFilter::search())
            .filter_dynamic(LengthFilter {
                min: 1,
                max: STATIC_TEXT_MAX_TOKEN_LEN,
            })
            .filter_dynamic(SimpleLowerCaseFilter)
            .build(),
        TextAnalyzer::builder(Uax29Tokenizer)
            .filter_dynamic(AccentFoldingFilter)
            .filter_dynamic(WordDelimiterFilter::search())
            .filter_dynamic(LengthFilter {
                min: 1,
                max: STATIC_TEXT_MAX_TOKEN_LEN,
            })
            .filter_dynamic(SimpleLowerCaseFilter)
            .filter_dynamic(SynonymFilter {
                resource: synonyms.clone(),
            })
            .build(),
    );
    register_both(
        DYNAMIC_TEXT_TOKENIZER_V1,
        TextAnalyzer::builder(SimpleTokenizer::default())
            .filter(RemoveLongFilter::limit(40))
            .filter(LowerCaser)
            .filter(english_stopwords())
            .filter(Stemmer::new(Language::English))
            .build(),
    );
    register_both(
        DYNAMIC_TEXT_TOKENIZER_V3,
        TextAnalyzer::builder(SimpleTokenizer::default())
            .filter_dynamic(LengthFilter { min: 2, max: 100 })
            .filter_dynamic(SimpleLowerCaseFilter)
            .filter(english_stopwords())
            .filter(Stemmer::new(Language::English))
            .build(),
    );
    register_both(
        DYNAMIC_TEXT_TOKENIZER_V4,
        TextAnalyzer::builder(SimpleTokenizer::default())
            .filter_dynamic(AccentFoldingFilter)
            .filter_dynamic(LengthFilter { min: 2, max: 100 })
            .filter_dynamic(SimpleLowerCaseFilter)
            .filter(english_stopwords())
            .filter(Stemmer::new(Language::English))
            .build(),
    );
    register_both(
        DYNAMIC_TEXT_TOKENIZER_V5,
        TextAnalyzer::builder(Uax29Tokenizer)
            .filter_dynamic(AccentFoldingFilter)
            .filter_dynamic(LengthFilter { min: 1, max: 100 })
            .filter_dynamic(SimpleLowerCaseFilter)
            .filter(english_stopwords())
            .filter(Stemmer::new(Language::English))
            .build(),
    );
    register_split(
        DYNAMIC_TEXT_TOKENIZER,
        TextAnalyzer::builder(Uax29Tokenizer)
            .filter_dynamic(AccentFoldingFilter)
            .filter_dynamic(LengthFilter { min: 1, max: 100 })
            .filter_dynamic(SimpleLowerCaseFilter)
            .filter(english_stopwords())
            .filter_dynamic(WordDelimiterFilter::search())
            .filter(Stemmer::new(Language::English))
            .build(),
        TextAnalyzer::builder(Uax29Tokenizer)
            .filter_dynamic(AccentFoldingFilter)
            .filter_dynamic(LengthFilter { min: 1, max: 100 })
            .filter_dynamic(SimpleLowerCaseFilter)
            .filter(english_stopwords())
            .filter_dynamic(WordDelimiterFilter::search())
            .filter_dynamic(SynonymFilter {
                resource: synonyms.clone(),
            })
            .filter(Stemmer::new(Language::English))
            .build(),
    );
    // Kept for opening and diagnosing legacy schemas; new text_en fields use
    // the v7 identity below.
    register_both(
        TEXT_EN_TOKENIZER_V2,
        TextAnalyzer::builder(SimpleTokenizer::default())
            .filter(RemoveLongFilter::limit(40))
            .filter(LowerCaser)
            .filter(english_stopwords())
            .filter(PorterTerminalYFilter)
            .filter(Stemmer::new(Language::English))
            .build(),
    );
    register_both(
        TEXT_EN_TOKENIZER_V3,
        TextAnalyzer::builder(SimpleTokenizer::default())
            .filter(RemoveLongFilter::limit(40))
            .filter_dynamic(SimpleLowerCaseFilter)
            .filter(english_stopwords())
            .filter(PorterTerminalYFilter)
            .filter(Stemmer::new(Language::English))
            .build(),
    );
    register_both(
        TEXT_EN_TOKENIZER_V4,
        TextAnalyzer::builder(SimpleTokenizer::default())
            .filter_dynamic(AccentFoldingFilter)
            .filter(RemoveLongFilter::limit(40))
            .filter_dynamic(SimpleLowerCaseFilter)
            .filter(english_stopwords())
            .filter(PorterTerminalYFilter)
            .filter(Stemmer::new(Language::English))
            .build(),
    );
    register_both(
        TEXT_EN_TOKENIZER_V5,
        TextAnalyzer::builder(Uax29Tokenizer)
            .filter_dynamic(AccentFoldingFilter)
            .filter(RemoveLongFilter::limit(40))
            .filter_dynamic(SimpleLowerCaseFilter)
            .filter(english_stopwords())
            .filter(PorterTerminalYFilter)
            .filter(Stemmer::new(Language::English))
            .build(),
    );
    register_split(
        TEXT_EN_TOKENIZER_V6,
        TextAnalyzer::builder(Uax29Tokenizer)
            .filter_dynamic(AccentFoldingFilter)
            .filter(RemoveLongFilter::limit(40))
            .filter_dynamic(SimpleLowerCaseFilter)
            .filter(english_stopwords())
            .filter_dynamic(WordDelimiterFilter::search())
            .filter(PorterTerminalYFilter)
            .filter(Stemmer::new(Language::English))
            .build(),
        TextAnalyzer::builder(Uax29Tokenizer)
            .filter_dynamic(AccentFoldingFilter)
            .filter(RemoveLongFilter::limit(40))
            .filter_dynamic(SimpleLowerCaseFilter)
            .filter(english_stopwords())
            .filter_dynamic(WordDelimiterFilter::search())
            .filter_dynamic(SynonymFilter {
                resource: synonyms.clone(),
            })
            .filter(PorterTerminalYFilter)
            .filter(Stemmer::new(Language::English))
            .build(),
    );
    register_split(
        TEXT_EN_TOKENIZER,
        TextAnalyzer::builder(Uax29Tokenizer)
            .filter_dynamic(AccentFoldingFilter)
            .filter_dynamic(LengthFilter {
                min: 1,
                max: STATIC_TEXT_MAX_TOKEN_LEN,
            })
            .filter_dynamic(SimpleLowerCaseFilter)
            .filter(english_stopwords())
            .filter_dynamic(WordDelimiterFilter::search())
            .filter(PorterTerminalYFilter)
            .filter(Stemmer::new(Language::English))
            .build(),
        TextAnalyzer::builder(Uax29Tokenizer)
            .filter_dynamic(AccentFoldingFilter)
            .filter_dynamic(LengthFilter {
                min: 1,
                max: STATIC_TEXT_MAX_TOKEN_LEN,
            })
            .filter_dynamic(SimpleLowerCaseFilter)
            .filter(english_stopwords())
            .filter_dynamic(WordDelimiterFilter::search())
            .filter_dynamic(SynonymFilter {
                resource: synonyms.clone(),
            })
            .filter(PorterTerminalYFilter)
            .filter(Stemmer::new(Language::English))
            .build(),
    );
    for (code, lang) in LANGUAGES {
        if *code == "en" {
            continue; // registered above under Wayfinder's versioned identity.
        }
        register_both(
            &legacy_language_tokenizer(code),
            TextAnalyzer::builder(SimpleTokenizer::default())
                .filter(RemoveLongFilter::limit(40))
                .filter(LowerCaser)
                .filter(Stemmer::new(*lang))
                .build(),
        );
        register_both(
            &language_tokenizer_v4(code),
            TextAnalyzer::builder(SimpleTokenizer::default())
                .filter_dynamic(AccentFoldingFilter)
                .filter(RemoveLongFilter::limit(40))
                .filter(LowerCaser)
                .filter(Stemmer::new(*lang))
                .build(),
        );
        register_both(
            &language_tokenizer_v5(code),
            TextAnalyzer::builder(Uax29Tokenizer)
                .filter_dynamic(AccentFoldingFilter)
                .filter(RemoveLongFilter::limit(40))
                .filter(LowerCaser)
                .filter(Stemmer::new(*lang))
                .build(),
        );
        register_split(
            &language_tokenizer(code),
            TextAnalyzer::builder(Uax29Tokenizer)
                .filter_dynamic(AccentFoldingFilter)
                .filter_dynamic(WordDelimiterFilter::search())
                .filter(RemoveLongFilter::limit(40))
                .filter(LowerCaser)
                .filter(Stemmer::new(*lang))
                .build(),
            TextAnalyzer::builder(Uax29Tokenizer)
                .filter_dynamic(AccentFoldingFilter)
                .filter_dynamic(WordDelimiterFilter::search())
                .filter(RemoveLongFilter::limit(40))
                .filter(LowerCaser)
                .filter_dynamic(SynonymFilter {
                    resource: synonyms.clone(),
                })
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
    register_both(
        BOOST_TERM_PAYLOAD_TOKENIZER,
        boost_term_payload_front()
            .filter_dynamic(DelimitedPayloadStripFilter)
            .build(),
    );
    register_both(
        BOOST_TERM_PAYLOAD_VERBATIM_TOKENIZER,
        boost_term_payload_front().build(),
    );
    // The `/suggest` read path's per-dictionary analyzers (#384). The shipped
    // `solrconfig_extra.xml` points each dictionary's `suggestAnalyzerFieldType`
    // at `text_en` / `text_und`, and *those* field types in
    // `schema_extra_types.xml` carry filters Wayfinder's global `text_en` /
    // `text_general` presets do not:
    //
    //   Stop(ignoreCase) -> WordDelimiterGraph -> Length(min=2,max=100)
    //     -> LowerCase -> [SnowballPorter English, text_en only]
    //     -> RemoveDuplicates
    //
    // The Solr-derived lower length bound is deliberately not reproduced: a
    // one-character or CJK word is meaningful for search quality. Simple case
    // folding is retained (`suggest_q_multibyte_grow_en`; see
    // `SimpleLowerCaseFilter`). `Stop` runs after `Length`, which cannot change
    // the surviving set because both filters only remove tokens.
    //
    // Unlike Wayfinder's other chains, this one has *no* `RemoveLongFilter`:
    // that filter is byte-based (`token.text.len() < limit`), and the shipped
    // field types bound length only with `LengthFilterFactory min="2"
    // max="100"`, which Lucene measures in characters. `LengthFilter` above is
    // the character-counting equivalent and is the whole bound here. Do not
    // re-add `RemoveLongFilter`: a 40-byte cut would drop tokens Solr keeps
    // (`pneumonoultramicroscopicsilicovolcanoconiosis` is 45 bytes; 14 CJK
    // characters are 42) and would make `SUGGEST_MAX_TOKEN_LEN` unreachable.
    // The other chains in this file keep it because it matches *their* Solr
    // field types, not this one.
    //
    // ponytail: suggest stays a distinct analyzer from the static presets. The
    // latter use simple case folding and the broader 32_766-character resource
    // bound; this path deliberately retains its independent 100-character
    // search-quality bound rather than becoming an alias for `/select`.
    let suggest_front = || {
        TextAnalyzer::builder(Uax29Tokenizer)
            .filter_dynamic(AccentFoldingFilter)
            // Search-quality UAX #29 words may be meaningful one-character
            // terms (including CJK), so retain the upper bound but do not
            // carry forward Solr's blind min=2 cutoff.
            .filter_dynamic(LengthFilter {
                min: 1,
                max: SUGGEST_MAX_TOKEN_LEN,
            })
            .filter_dynamic(SimpleLowerCaseFilter)
    };
    // `stopwords_und.txt` is empty in the shipped configset, so the unstemmed
    // `und` and non-stemming-language chains have no stopword filter -- unlike
    // `text_en`'s 33-word list. They remain separately registered because the
    // requested dictionary name is the response key and analyzer identity.
    for code in std::iter::once(SUGGEST_UNDEFINED_DICTIONARY)
        .chain(UNSTEMMED_SUGGEST_LANGUAGES.iter().copied())
    {
        register_split(
            &suggest_tokenizer(code),
            suggest_front()
                .filter_dynamic(WordDelimiterFilter::suggest())
                .filter_dynamic(RemoveDuplicatesFilter)
                .build(),
            suggest_front()
                .filter_dynamic(WordDelimiterFilter::suggest())
                .filter_dynamic(SynonymFilter {
                    resource: synonyms.clone(),
                })
                .filter_dynamic(RemoveDuplicatesFilter)
                .build(),
        );
    }
    for (code, lang) in LANGUAGES {
        let index_chain = if *code == "en" {
            // Only `text_en` ships a stopword list; the same Porter terminal-y
            // compatibility rule `TEXT_EN_TOKENIZER` needs applies here too.
            suggest_front()
                .filter_dynamic(english_stopwords())
                .filter_dynamic(WordDelimiterFilter::suggest())
                .filter_dynamic(PorterTerminalYFilter)
        } else {
            suggest_front().filter_dynamic(WordDelimiterFilter::suggest())
        };
        let query_chain = if *code == "en" {
            suggest_front()
                .filter_dynamic(english_stopwords())
                .filter_dynamic(WordDelimiterFilter::suggest())
                .filter_dynamic(SynonymFilter {
                    resource: synonyms.clone(),
                })
                .filter_dynamic(PorterTerminalYFilter)
        } else {
            suggest_front()
                .filter_dynamic(WordDelimiterFilter::suggest())
                .filter_dynamic(SynonymFilter {
                    resource: synonyms.clone(),
                })
        };
        register_split(
            &suggest_tokenizer(code),
            index_chain
                .filter_dynamic(Stemmer::new(*lang))
                .filter_dynamic(RemoveDuplicatesFilter)
                .build(),
            query_chain
                .filter_dynamic(Stemmer::new(*lang))
                .filter_dynamic(RemoveDuplicatesFilter)
                .build(),
        );
    }
    for ft in field_types {
        // The custom chains. Both managers get an entry under the field type's
        // own name -- that name IS the index identity, and the query manager is
        // keyed by it -- so the only question is which analyzer goes into the
        // query manager.
        manager.register(&ft.name, build_analyzer(ft, AnalyzerSide::Index)?);
        let query_analyzer = match ft.query_tokenizer {
            Some(_) => build_analyzer(ft, AnalyzerSide::Query)?,
            None => {
                if !ft.query_filters.is_empty() {
                    bail!(
                        "field type `{}` sets `query_filters` without a `query_tokenizer`: the \
                         query filters would never run, because a type declaring no query \
                         tokenizer gets its *index* analyzer registered as its query analyzer too",
                        ft.name
                    );
                }
                build_analyzer(ft, AnalyzerSide::Index)?
            }
        };
        query_manager.register(&ft.name, query_analyzer);
    }
    Ok((manager, query_manager))
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

/// As [`load`], but with a core's live synonym resource installed into query
/// analyzers. `CoreIndex::open` is the production caller; plain `load`/`parse`
/// retain an empty resource for schema-only validation tests.
pub fn load_with_synonyms(path: &Path, synonyms: SynonymResource) -> Result<WayfinderSchema> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading schema file {}", path.display()))?;
    parse_with_synonyms(&raw, synonyms)
        .with_context(|| format!("parsing schema file {}", path.display()))
}

/// As `load`, but from the TOML text directly.
pub fn parse(raw: &str) -> Result<WayfinderSchema> {
    let temporary = tempfile::tempdir().context("creating empty schema synonym resource")?;
    parse_with_synonyms(raw, SynonymResource::open(temporary.path())?)
}

fn parse_with_synonyms(raw: &str, synonyms: SynonymResource) -> Result<WayfinderSchema> {
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
    // The built-in tokenizer identities are reserved alongside the field type
    // names: a custom chain registering over one would redefine that preset.
    // Non-English current and legacy identities are derived from `LANGUAGES`,
    // so adding a language cannot leave its internal analyzer shadowable. The
    // `wayfinder_suggest_*` identities (#384) are reserved for the same reason
    // -- a custom chain under one of those names would redefine the analyzer
    // the `/suggest` read path resolves through `dictionary_tokenizer`.
    let reserved_type_names: Vec<String> = builtin_type_names()
        .into_iter()
        .chain(RETIRED_TEXT_TOKENIZERS.iter().map(|name| name.to_string()))
        .chain([
            TEXT_GENERAL_TOKENIZER_LEGACY.to_string(),
            TEXT_GENERAL_TOKENIZER.to_string(),
            TEXT_GENERAL_TOKENIZER_V6.to_string(),
            TEXT_GENERAL_TOKENIZER_V5.to_string(),
            TEXT_GENERAL_TOKENIZER_V4.to_string(),
            TEXT_GENERAL_TOKENIZER_V3.to_string(),
            TEXT_EN_TOKENIZER.to_string(),
            TEXT_EN_TOKENIZER_V6.to_string(),
            TEXT_EN_TOKENIZER_V5.to_string(),
            TEXT_EN_TOKENIZER_V4.to_string(),
            TEXT_EN_TOKENIZER_V3.to_string(),
            TEXT_EN_TOKENIZER_V2.to_string(),
            DYNAMIC_TEXT_TOKENIZER.to_string(),
            DYNAMIC_TEXT_TOKENIZER_V5.to_string(),
            DYNAMIC_TEXT_TOKENIZER_V4.to_string(),
            DYNAMIC_TEXT_TOKENIZER_V3.to_string(),
            DYNAMIC_TEXT_TOKENIZER_V1.to_string(),
            BOOST_TERM_PAYLOAD_TOKENIZER.to_string(),
            BOOST_TERM_PAYLOAD_VERBATIM_TOKENIZER.to_string(),
            suggest_tokenizer(SUGGEST_UNDEFINED_DICTIONARY),
        ])
        .chain(
            LANGUAGES
                .iter()
                .filter(|(code, _)| *code != "en")
                .flat_map(|(code, _)| {
                    [
                        legacy_language_tokenizer(code),
                        format!("wayfinder_text_{code}_v3"),
                        language_tokenizer_v4(code),
                        language_tokenizer_v5(code),
                        language_tokenizer(code),
                    ]
                }),
        )
        .chain(LANGUAGES.iter().map(|(code, _)| suggest_tokenizer(code)))
        .chain(
            UNSTEMMED_SUGGEST_LANGUAGES
                .iter()
                .map(|code| suggest_tokenizer(code)),
        )
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
    let (tokenizers, query_tokenizers) = build_tokenizers(&parsed.field_types, &synonyms)?;

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
        // Snowball behavior (`day` stays `day`) here while folding accents;
        // static built-in `text_en` uses its Porter-compatible analyzer independently.
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
        query_tokenizers,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn uax29_tokens(text: &str) -> Vec<Token> {
        let mut tokenizer = Uax29Tokenizer;
        let mut stream = tokenizer.token_stream(text);
        let mut tokens = Vec::new();
        while stream.advance() {
            tokens.push(stream.token().clone());
        }
        tokens
    }

    #[test]
    fn word_delimiter_compound_is_a_monotonic_position_graph() {
        let mut tokenizer = WordDelimiterFilter::search().transform(Uax29Tokenizer);
        let mut stream = tokenizer.token_stream("SKU-42 next");
        let mut tokens = Vec::new();
        while stream.advance() {
            tokens.push(stream.token().clone());
        }
        assert!(
            tokens
                .windows(2)
                .all(|pair| pair[0].position <= pair[1].position)
        );
        assert!(tokens.iter().any(|token| {
            (token.text == "SKU-42" || token.text == "SKU42")
                && token.position == 0
                && token.position_length == 2
        }));
        assert!(
            tokens
                .iter()
                .any(|token| token.text == "SKU" && token.position == 0)
        );
        assert!(
            tokens
                .iter()
                .any(|token| token.text == "42" && token.position == 1)
        );
        assert!(
            tokens
                .iter()
                .any(|token| token.text == "next" && token.position == 2)
        );
    }

    #[test]
    fn suggest_language_classification_matches_shipped_search_api_field_types() {
        use std::collections::BTreeSet;

        let optional_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("coverage/search_api_solr_4.4.0_source/config/optional");
        let mut shipped_codes = BTreeSet::new();
        for entry in std::fs::read_dir(&optional_dir).expect("read vendored Search API field types")
        {
            let path = entry.expect("read field-type entry").path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !name.starts_with("search_api_solr.solr_field_type.text_")
                || path.extension().and_then(|ext| ext.to_str()) != Some("yml")
            {
                continue;
            }
            let contents = std::fs::read_to_string(&path).expect("read vendored field type");
            let code = contents
                .lines()
                .find_map(|line| line.strip_prefix("field_type_language_code:"))
                .map(|code| code.trim().trim_matches(['\'', '"']).to_string())
                .unwrap_or_else(|| panic!("{} has no field_type_language_code", path.display()));
            shipped_codes.insert(code);
        }

        let stemmed: BTreeSet<String> = LANGUAGES
            .iter()
            .map(|(code, _)| (*code).to_string())
            .collect();
        let unstemmed: BTreeSet<String> = UNSTEMMED_SUGGEST_LANGUAGES
            .iter()
            .map(|code| (*code).to_string())
            .collect();
        assert_eq!(
            stemmed.len(),
            LANGUAGES.len(),
            "LANGUAGES must not contain duplicate codes"
        );
        assert_eq!(
            unstemmed.len(),
            UNSTEMMED_SUGGEST_LANGUAGES.len(),
            "UNSTEMMED_SUGGEST_LANGUAGES must not contain duplicate codes"
        );
        assert!(
            stemmed.is_disjoint(&unstemmed),
            "a suggest language must not be both stemmed and unstemmed"
        );
        assert!(
            shipped_codes
                .iter()
                .all(|code| stemmed.contains(code) ^ unstemmed.contains(code)),
            "every shipped Search API language field type must have exactly one suggest chain"
        );
        let expected_unstemmed: BTreeSet<String> =
            shipped_codes.difference(&stemmed).cloned().collect();
        assert_eq!(
            unstemmed, expected_unstemmed,
            "unstemmed suggest languages must be exactly the shipped codes without Tantivy stemmers"
        );
    }

    #[test]
    fn graph_semantics_are_reserved_for_exact_builtin_tokenizer_identities() {
        assert!(is_current_builtin_graph_tokenizer(TEXT_EN_TOKENIZER));
        assert!(is_current_builtin_graph_tokenizer(DYNAMIC_TEXT_TOKENIZER));
        assert!(!is_current_builtin_graph_tokenizer("operator_custom_v6"));
        assert!(!is_current_builtin_graph_tokenizer("wayfinder_text_en_v5"));
    }

    #[test]
    fn uax29_tokenizer_retains_emoji_symbols_and_combining_only_segments() {
        let input = "😀 !!! ★";
        let tokens = uax29_tokens(input);
        assert_eq!(
            tokens
                .iter()
                .map(|token| token.text.as_str())
                .collect::<Vec<_>>(),
            vec!["😀", "★"],
            "only whitespace-only and punctuation-only UAX #29 segments may be discarded"
        );
        for token in tokens {
            assert_eq!(
                &input[token.offset_from..token.offset_to],
                token.text,
                "UAX #29 terms must retain offsets into the original input"
            );
        }

        let combining_only = "\u{301}";
        let token = uax29_tokens(combining_only)
            .pop()
            .expect("a combining-only UAX #29 segment must be retained");
        assert_eq!(token.text, combining_only);
        assert_eq!(
            (token.offset_from, token.offset_to),
            (0, combining_only.len()),
            "combining-only terms must retain original offsets"
        );
    }
}
