//! Solr highlighting (PRD §5 "Highlighting" row): `hl`/`hl.fl`/`hl.snippets`/
//! `hl.fragsize`/`hl.simple.pre`/`hl.simple.post` -> the `highlighting`
//! response block.
//!
//! Wire semantics live here, the same way `crate::facet` holds Solr's facet
//! semantics on top of `CoreIndex::term_facet`/`count`: which fields
//! highlight, how many snippets, and the exact envelope shape.
//! `CoreIndex::highlight_field` is the Tantivy-facing primitive: it extracts
//! distinct fragments for a doc/field up to the `hl.snippets` cap this module
//! resolves and hands it. Facts pinned by fixtures in
//! `solr-ref/responses/hl_*.json` (`solr-ref/FINDINGS.md` findings
//! 52-55, 81, and 110):
//!
//! - **finding 52**: `highlighting` is a top-level object keyed by the
//!   unique key. A doc that matched the base query through some field other
//!   than any `hl.fl` field still gets an entry -- an empty object, never
//!   absent and never `{"field": []}`. A specific field with no term overlap
//!   for a doc that does have an entry is simply absent from that doc's
//!   per-field map, not `[]`.
//! - **finding 53**: `hl.snippets` caps, never pads -- it never fabricates
//!   snippets beyond what actually exists in the field.
//! - **finding 54**: `hl.fl`'s default, when `hl=true` with no `hl.fl` at
//!   all, is `df` (the resolved default search field), not `*`/absent.
//! - **finding 55**: `hl.fragsize` truncation is only meaningfully visible in
//!   this fixture set under `hl.method=original`; Solr's default
//!   `hl.method=unified` barely truncates a short, punctuation-free field
//!   regardless of `hl.fragsize` (`hl_fragsize_small.json` is exactly that
//!   control: fragsize 18, still untruncated). So `hl.fragsize` is only
//!   applied here (via `SnippetGenerator::set_max_num_chars`) when
//!   `hl.method=original` is explicit; otherwise this leaves Tantivy's own
//!   150-char `SnippetGenerator` default alone. This is a documented
//!   judgment call standing in for a real `hl.method=unified`
//!   implementation, which the issue explicitly puts out of scope. Finding
//!   55's scope limit is about *nonzero* `hl.fragsize` only -- the zero case
//!   is finding 81 below and is decided before this split is consulted.
//! - **finding 81**: `hl.fragsize=0` means "return the whole field,
//!   unfragmented, as a single snippet" -- it is not "unset". It behaves that
//!   way for *both* `hl.method` values, confirmed by
//!   `solr-ref/responses/hl_fragsize_zero_whole_field.json` (default
//!   `hl.method`, i.e. unified) and
//!   `hl_fragsize_zero_whole_field_method_original.json`
//!   (`hl.method=original`), which are byte-identical in their
//!   `highlighting` block. So an explicit zero is special-cased here *ahead
//!   of* the finding-55 original-vs-default split above, mapping to
//!   `WHOLE_FIELD_MAX_CHARS` (a sentinel char budget no field can exceed) so
//!   `SnippetGenerator::set_max_num_chars` never fragments. Absent or
//!   unparseable `hl.fragsize` is unaffected and still falls back as before
//!   (`DEFAULT_FRAGSIZE` under `hl.method=original`, Tantivy's own 150-char
//!   default otherwise).
//! - **issue #139**: `hl.fl` also accepts Solr's `*` wildcard, which expands
//!   to the schema's highlightable fields and never errors on the non-text
//!   ones it sweeps up. See `resolve_hl_fl`/`highlightable_fields` below for
//!   the evidence and the asymmetry between wildcard and explicit fields.
//!
//! Issue #353 admitted five more `hl.*` params `SearchApiSolrBackend::
//! setHighlighting` emits. Two have implemented behaviour here:
//!
//! - **`hl.preserveMulti`**: under `hl.method=original`, one snippet PER VALUE
//!   of a multi-valued field, in indexed order, for every value (matching
//!   highlighted, non-matching plain) -- see `highlight_field_preserve_multi`
//!   in `crate::core_index` and `hl353_preserve_multi_on`/`_off`. A no-op on
//!   the default (`hl.method=unified`) path, which Wayfinder does not truly
//!   implement (finding 55), matching real Solr's own no-op there.
//! - **`hl.fragmenter`**: `gap` is Solr's default original-method fragmenter
//!   (`LuceneGapFragmenter`) and changes nothing here -- Tantivy already gaps.
//!   See `hl353_fragmenter_gap` (byte-identical to omitting it).
//!
//! The other three (#353) are admitted but currently inert; each ceiling is
//! named so an accepted param cannot change behaviour silently:
//!
//! - ponytail: **`hl.maxAnalyzedChars`** caps how many leading characters of a
//!   field Solr scans for highlight candidates (default 51200). Tantivy's
//!   `SnippetGenerator` analyses the whole field, so this is ignored today.
//!   Implementing it means truncating each field value to the char window
//!   before snippet generation and verifying boundary behaviour (a term that
//!   straddles the window), captured live first.
//! - ponytail: **`hl.usePhraseHighlighter`** correlates the terms of a phrase
//!   query so they highlight as one span. Wayfinder's snippet path highlights
//!   each query term independently and has no phrase-span notion; the param is
//!   accepted and ignored.
//! - ponytail: **`hl.highlightMultiTerm`** expands wildcard/prefix/fuzzy query
//!   terms so their expansions are highlighted. Wayfinder does not expand
//!   such terms for highlighting; the param is accepted and ignored.
//!
//! `hl.fragmenter=regex` is also not built: the regex fragmenter needs
//! `hl.regex.*`, which the client never reaches because of the inverted inner
//! guard at `SearchApiSolrBackend.php:4250` -- `tests/hl353_regex_descope_guard.rs`
//! fails the day that inversion is fixed upstream.

use std::fmt;

use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value};
use tantivy::query::Query;
use tantivy::{DocAddress, Score};

use crate::core_index::{CoreIndex, WHOLE_FIELD_MAX_CHARS};
use crate::params::Params;
use crate::schema::{ValueKind, WayfinderSchema};

/// Marks a `highlighting` error as a request-input problem -- an undefined or
/// non-text `hl.fl` field -- rather than a genuine internal failure, so
/// the select workflow (`src/select_workflow.rs`) can render it as Solr's own
/// 400 via `downcast_ref`, matching `sort::parse_spec`'s undefined-field error
/// and `facet::check_facetable`'s undefined/unfacetable field, instead of
/// treating it as a 500. Wraps the original error rather than replacing it --
/// `Display` forwards to it verbatim.
#[derive(Debug)]
pub struct InvalidHlField(anyhow::Error);

impl fmt::Display for InvalidHlField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for InvalidHlField {}

/// Solr's `hl.snippets` default.
const DEFAULT_SNIPPETS: usize = 1;
/// Solr's `hl.fragsize` default.
const DEFAULT_FRAGSIZE: usize = 100;
/// Tantivy's own `SnippetGenerator` default char budget
/// (`DEFAULT_MAX_NUM_CHARS`, `tantivy-0.26.1/src/snippet/mod.rs`) -- used
/// whenever `hl.fragsize` is not applied (finding 55, module docs above).
const TANTIVY_DEFAULT_MAX_CHARS: usize = 150;
/// Solr's own default highlight markers.
const DEFAULT_PRE: &str = "<em>";
const DEFAULT_POST: &str = "</em>";

/// Builds the `highlighting` response block for the docs actually returned
/// on this page (`page`, already paginated/sorted the same as
/// `response.docs`), keyed by each doc's `unique_key` stored value.
pub fn highlighting(
    index: &CoreIndex,
    params: &Params,
    default_field: &str,
    query: &dyn Query,
    page: &[(Score, DocAddress)],
    unique_key: &str,
) -> Result<Value> {
    // finding 54: `hl.fl` defaults to `df`, not `*`/absent.
    let fl_raw = params.get("hl.fl").unwrap_or(default_field);
    let fields = resolve_hl_fl(&index.wf_schema, fl_raw)?;

    let snippets_cap: usize = params
        .get("hl.snippets")
        .and_then(|v| v.parse().ok())
        .filter(|&n: &usize| n > 0)
        .unwrap_or(DEFAULT_SNIPPETS);

    // Parsed once, because an explicit `0` and an absent/unparseable value
    // are *different* answers here (finding 81) -- `Some(0)` means whole
    // field, `None` means fall back.
    let fragsize: Option<usize> = params.get("hl.fragsize").and_then(|v| v.parse().ok());

    // ponytail: for a *nonzero* `hl.fragsize`, only the `hl.method=original`
    // vs. everything-else split (finding 55) -- not a real
    // `hl.method=unified` fragmenter. The ceiling is exactly the module docs'
    // finding-55 paragraph: `hl.fragsize` is applied verbatim under
    // `hl.method=original`, and ignored entirely (falling back to Tantivy's
    // own 150-char default) for every other `hl.method` value, including the
    // unset default. A real `unified` implementation would need its own
    // fragmenter, which the issue puts out of scope.
    let max_chars = if fragsize == Some(0) {
        // finding 81: an explicit zero is whole-field for *both* `hl.method`
        // values, so it is decided before the split below is consulted.
        WHOLE_FIELD_MAX_CHARS
    } else if params.get("hl.method") == Some("original") {
        // `Some(0)` was already taken by the branch above, so this is
        // "absent or unparseable -> Solr's default", not a zero filter.
        fragsize.unwrap_or(DEFAULT_FRAGSIZE)
    } else {
        TANTIVY_DEFAULT_MAX_CHARS
    };

    let pre = params.get("hl.simple.pre").unwrap_or(DEFAULT_PRE);
    let post = params.get("hl.simple.post").unwrap_or(DEFAULT_POST);

    let mut out = Map::new();
    for &(_, addr) in page {
        let key = doc_key(index, addr, unique_key)?;
        let mut per_field = Map::new();
        for field_name in &fields {
            // Solr defaults `hl.requireFieldMatch` to false: absent or
            // explicit false uses query terms from every field. Only true
            // restricts terms to the field being highlighted (finding 113).
            let cross_field_query_terms = params.get("hl.requireFieldMatch") != Some("true");
            let original_fragments = params.get("hl.method") == Some("original");
            let merge_contiguous =
                original_fragments && params.get("hl.mergeContiguous") == Some("true");
            // Issue #353: `hl.preserveMulti` only takes effect on the original
            // path (the default `hl.method=unified` is a captured no-op for
            // it), and only on a multi-valued field -- on a single-valued
            // field "one snippet per value" is just the ordinary path.
            let is_multi_valued = if index.wf_schema.is_static(field_name) {
                index
                    .wf_schema
                    .field_config(field_name)
                    .is_some_and(|c| c.multi_valued)
            } else {
                index
                    .wf_schema
                    .match_dynamic(field_name)
                    .is_some_and(|rule| rule.multi_valued)
            };
            let preserve_multi = original_fragments
                && params.get("hl.preserveMulti") == Some("true")
                && is_multi_valued;
            let mut snippets = if preserve_multi && index.wf_schema.is_static(field_name) {
                index.highlight_field_preserve_multi(
                    query,
                    addr,
                    field_name,
                    max_chars,
                    pre,
                    post,
                    snippets_cap,
                    cross_field_query_terms,
                    merge_contiguous,
                )?
            } else if preserve_multi {
                index.highlight_dynamic_field_preserve_multi(
                    query,
                    addr,
                    field_name,
                    max_chars,
                    pre,
                    post,
                    snippets_cap,
                    cross_field_query_terms,
                    merge_contiguous,
                )?
            } else if index.wf_schema.is_static(field_name) {
                index.highlight_field_with_options(
                    query,
                    addr,
                    field_name,
                    max_chars,
                    pre,
                    post,
                    snippets_cap,
                    cross_field_query_terms,
                    original_fragments,
                    merge_contiguous,
                )?
            } else {
                index.highlight_dynamic_field_with_options(
                    query,
                    addr,
                    field_name,
                    max_chars,
                    pre,
                    post,
                    snippets_cap,
                    cross_field_query_terms,
                    original_fragments,
                    merge_contiguous,
                )?
            };
            if !preserve_multi && snippets.is_empty() && index.wf_schema.is_raw_string(field_name) {
                snippets = raw_string_snippets(
                    index,
                    query,
                    addr,
                    field_name,
                    pre,
                    post,
                    cross_field_query_terms,
                )?;
            }
            if snippets.is_empty() {
                // finding 52: absent from the per-field map, not `[]`.
                continue;
            }
            // `highlight_field` already stops extracting at `snippets_cap` --
            // it has to, since every extra snippet costs another pass over the
            // field text. This `take` is the belt to that braces: capping is
            // finding 53's wire contract, and it stays enforced here rather
            // than relying on the primitive having got it right. NOT applied
            // under `hl.preserveMulti`: that path returns one snippet per
            // value by contract (issue #353), so capping it to `hl.snippets`
            // would drop values Solr returns -- captured `hl.snippets` 1, 2,
            // and 5 all yield every value.
            let capped: Vec<Value> = if preserve_multi {
                snippets.into_iter().map(Value::from).collect()
            } else {
                snippets
                    .into_iter()
                    .take(snippets_cap)
                    .map(Value::from)
                    .collect()
            };
            per_field.insert((*field_name).to_string(), Value::Array(capped));
        }
        // finding 52: always an entry, `{}` when nothing matched, never
        // absent.
        out.insert(key, Value::Object(per_field));
    }
    Ok(Value::Object(out))
}

/// Highlights exact stored `string`/`keyword` values when Tantivy's snippet
/// path misses a matching member of a multi-valued field. Tantivy joins the
/// stored values with spaces before applying the raw tokenizer, turning
/// `["animals", "classic"]` into one non-matching token; finding 110 proves
/// Solr highlights the matching value separately.
fn raw_string_snippets(
    index: &CoreIndex,
    query: &dyn Query,
    addr: DocAddress,
    field_name: &str,
    pre: &str,
    post: &str,
    cross_field_query_terms: bool,
) -> Result<Vec<String>> {
    let field = index
        .wf_schema
        .field(field_name)
        .ok_or_else(|| anyhow!("can not highlight undefined field: {field_name}"))?;
    let mut terms = Vec::new();
    query.query_terms(&mut |term, _| {
        if (cross_field_query_terms || term.field() == field)
            && let Some(value) = term.value().as_str()
        {
            terms.push(value.to_owned());
        }
    });
    if terms.is_empty() {
        return Ok(Vec::new());
    }

    let rendered = index.render_doc(addr, Some(&[field_name.to_owned()]), None)?;
    let values: Vec<&str> = match rendered.get(field_name) {
        Some(Value::String(value)) => vec![value],
        Some(Value::Array(values)) => values.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    Ok(values
        .into_iter()
        .filter(|value| terms.iter().any(|term| term == value))
        .map(|value| {
            let escaped = value
                .replace('&', "&amp;")
                .replace('"', "&quot;")
                .replace('\'', "&#x27;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            format!("{pre}{escaped}{post}")
        })
        .collect())
}

/// Solr's `hl.fl` wildcard: "every highlightable field in the schema".
const HL_FL_WILDCARD: &str = "*";

/// Splits `hl.fl` into the concrete field list to highlight, expanding the
/// `*` wildcard (issue #139) and validating every *explicitly named* field.
///
/// The two halves are validated differently on purpose, and that asymmetry
/// is the whole point of this function:
///
/// - An **explicitly named** field that is undefined or non-text is a
///   request-input problem and 400s, exactly as before (`InvalidHlField`,
///   `check_highlightable`) -- the user asked for something impossible.
/// - A field the **wildcard** produced is never an error. Real Solr's
///   `DefaultSolrHighlighter::getHighlightFields` expands `*` against the
///   *schema's* field names (via `SolrPluginUtils.expandWildcardsInField`),
///   not against the query's `qf`/`df` set, and a field that comes back from
///   that expansion but cannot be analyzed simply never produces a snippet
///   -- indistinguishable on the wire from "no term overlap" (finding 52's
///   `{}` covers both). Running wildcard-expanded names through
///   `check_highlightable` would instead 400 any schema that merely
///   *contains* a non-text field, which is not a shape Solr can produce.
///   So the wildcard filters down to highlightable fields up front, and the
///   non-highlightable ones are silently skipped.
///
/// Evidence that `*` is not a `df` fallback, from the captured Search API
/// traffic: the traced core's `/select` handler sets `df` to `id`
/// (`solr-ref/search-api/configset/solrconfig_extra.xml:113`), so a real
/// fallback candidate is in force, yet every wildcard trace with snippets
/// keys them on `tm_X3b_en_body`/`tm_X3b_en_title` and never on `id`
/// (`solr-ref/search-api/trace/` `00002`, `00005`, `00006`, `00007`,
/// `00009`).
///
/// Finding 110 supersedes finding 94's unresolved raw-string inference:
/// stored `string` fields are included and can produce snippets. Numeric and
/// date fields remain filtered here because Tantivy has no tokenizer with
/// which to highlight them; importantly, wildcard expansion silently omits
/// those fields rather than applying the explicit-field 400 path.
fn resolve_hl_fl<'a>(schema: &'a WayfinderSchema, fl_raw: &'a str) -> Result<Vec<&'a str>> {
    let mut fields: Vec<&'a str> = Vec::new();
    for token in fl_raw
        .split(|c: char| c == ',' || c.is_whitespace())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if token == HL_FL_WILDCARD {
            for name in highlightable_fields(schema) {
                if !fields.contains(&name) {
                    fields.push(name);
                }
            }
        } else {
            // Validated before touching Tantivy at all: Solr's own
            // `sort::parse_spec` / `facet::check_facetable` precedent for telling a
            // request-input problem from an internal failure.
            check_highlightable(schema, token)
                .map_err(|e| anyhow::Error::new(InvalidHlField(e)))?;
            if !fields.contains(&token) {
                fields.push(token);
            }
        }
    }
    Ok(fields)
}

/// Every statically declared field `*` expands to: stored (highlighting
/// reads the stored value, so an unstored field could never produce a
/// snippet) and text-valued, including raw `string`/`keyword` fields. Schema
/// declaration order matches Solr's own insertion-ordered expansion set.
/// Finding 110's `hl_wildcard_stored_string.json` proves that
/// `q=category:animals&hl.fl=*` highlights the stored raw `category` value,
/// identically to explicit `hl.fl=category`; numeric and date fields remain
/// excluded because they are not `ValueKind::Text`.
///
/// ponytail: static `[[fields]]` only, and only the bare `*` token --
/// no `[[dynamic_fields]]` instances and no partial globs (`tm_*`). Real
/// Solr expands against the field names actually present in the index, so it
/// would also return dynamic-field instances; Wayfinder stores every dynamic
/// value in the shared `_dynamic_text`/`_dynamic` catch-alls rather than in
/// per-name Tantivy fields, so there are no per-instance names for `*` to
/// resolve to and `CoreIndex::highlight_field` has no handle to address one
/// with. Lifting that ceiling means per-instance highlight extraction out of
/// the catch-all, which no captured fixture pins.
fn highlightable_fields(schema: &WayfinderSchema) -> Vec<&str> {
    schema
        .fields
        .iter()
        .filter(|f| f.stored)
        .filter(|f| schema.value_kind(&f.name) == Some(ValueKind::Text))
        .map(|f| f.name.as_str())
        .collect()
}

/// Refuses an `hl.fl` field Tantivy cannot highlight, rather than surfacing
/// whatever internal error `SnippetGenerator::create`/`tokenizer_for_field`
/// would raise for it (an undefined field would panic-free but wrongly
/// resolve to "no term overlap"; a Points-based field -- int/long/float/
/// double/date -- has no tokenizer at all and `tokenizer_for_field` errors).
/// Both are request-input problems, not internal ones -- mirrors
/// `facet::check_facetable`'s precedent for the same distinction on
/// `facet.field`.
fn check_highlightable(schema: &WayfinderSchema, field_name: &str) -> Result<()> {
    if schema.is_static(field_name) {
        return match schema.value_kind(field_name) {
            Some(ValueKind::Text) => Ok(()),
            _ => bail!("can not highlight a non-text field: {field_name}"),
        };
    }
    match schema.resolved_value_kind(field_name) {
        Some(ValueKind::Text) => Ok(()),
        Some(_) => bail!("can not highlight a non-text field: {field_name}"),
        None => bail!("can not highlight undefined field: {field_name}"),
    }
}

/// The unique key's stored value for `addr`, rendered the same way
/// `CoreIndex::render_doc` already renders it for `response.docs`.
fn doc_key(index: &CoreIndex, addr: DocAddress, unique_key: &str) -> Result<String> {
    let rendered = index.render_doc(addr, Some(&[unique_key.to_string()]), None)?;
    rendered
        .get(unique_key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("doc at {addr:?} is missing its unique key `{unique_key}`"))
}
