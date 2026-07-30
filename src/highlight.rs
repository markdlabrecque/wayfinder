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
//! `solr-ref/responses/hl_*.json` (`docs/solr-ref-findings.md` findings
//! 51-54 and 81):
//!
//! - **finding 51**: `highlighting` is a top-level object keyed by the
//!   unique key. A doc that matched the base query through some field other
//!   than any `hl.fl` field still gets an entry -- an empty object, never
//!   absent and never `{"field": []}`. A specific field with no term overlap
//!   for a doc that does have an entry is simply absent from that doc's
//!   per-field map, not `[]`.
//! - **finding 52**: `hl.snippets` caps, never pads -- it never fabricates
//!   snippets beyond what actually exists in the field.
//! - **finding 53**: `hl.fl`'s default, when `hl=true` with no `hl.fl` at
//!   all, is `df` (the resolved default search field), not `*`/absent.
//! - **finding 54**: `hl.fragsize` truncation is only meaningfully visible in
//!   this fixture set under `hl.method=original`; Solr's default
//!   `hl.method=unified` barely truncates a short, punctuation-free field
//!   regardless of `hl.fragsize` (`hl_fragsize_small.json` is exactly that
//!   control: fragsize 18, still untruncated). So `hl.fragsize` is only
//!   applied here (via `SnippetGenerator::set_max_num_chars`) when
//!   `hl.method=original` is explicit; otherwise this leaves Tantivy's own
//!   150-char `SnippetGenerator` default alone. This is a documented
//!   judgment call standing in for a real `hl.method=unified`
//!   implementation, which the issue explicitly puts out of scope. Finding
//!   54's scope limit is about *nonzero* `hl.fragsize` only -- the zero case
//!   is finding 81 below and is decided before this split is consulted.
//! - **finding 81**: `hl.fragsize=0` means "return the whole field,
//!   unfragmented, as a single snippet" -- it is not "unset". It behaves that
//!   way for *both* `hl.method` values, confirmed by
//!   `solr-ref/responses/hl_fragsize_zero_whole_field.json` (default
//!   `hl.method`, i.e. unified) and
//!   `hl_fragsize_zero_whole_field_method_original.json`
//!   (`hl.method=original`), which are byte-identical in their
//!   `highlighting` block. So an explicit zero is special-cased here *ahead
//!   of* the finding-54 original-vs-default split above, mapping to
//!   `WHOLE_FIELD_MAX_CHARS` (a sentinel char budget no field can exceed) so
//!   `SnippetGenerator::set_max_num_chars` never fragments. Absent or
//!   unparseable `hl.fragsize` is unaffected and still falls back as before
//!   (`DEFAULT_FRAGSIZE` under `hl.method=original`, Tantivy's own 150-char
//!   default otherwise).

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
/// `select` in `src/lib.rs` can render it as Solr's own 400 via
/// `downcast_ref`, matching `check_sort`'s undefined-field error and
/// `facet::check_facetable`'s undefined/unfacetable field, instead of
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
/// whenever `hl.fragsize` is not applied (finding 54, module docs above).
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
    // finding 53: `hl.fl` defaults to `df`, not `*`/absent.
    let fl_raw = params.get("hl.fl").unwrap_or(default_field);
    let fields: Vec<&str> = fl_raw
        .split(|c: char| c == ',' || c.is_whitespace())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

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
    // vs. everything-else split (finding 54) -- not a real
    // `hl.method=unified` fragmenter. The ceiling is exactly the module docs'
    // finding-54 paragraph: `hl.fragsize` is applied verbatim under
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

    // Validated once per request, before touching Tantivy at all: an
    // undefined or non-text `hl.fl` field is a request-input problem (Solr's
    // own `check_sort` / `facet::check_facetable` precedent), not the kind of
    // internal failure a genuine `SnippetGenerator`/searcher error would be.
    for field_name in &fields {
        check_highlightable(&index.wf_schema, field_name)
            .map_err(|e| anyhow::Error::new(InvalidHlField(e)))?;
    }

    let mut out = Map::new();
    for &(_, addr) in page {
        let key = doc_key(index, addr, unique_key)?;
        let mut per_field = Map::new();
        for field_name in &fields {
            let snippets = index.highlight_field(
                query,
                addr,
                field_name,
                max_chars,
                pre,
                post,
                snippets_cap,
            )?;
            if snippets.is_empty() {
                // finding 51: absent from the per-field map, not `[]`.
                continue;
            }
            // `highlight_field` already stops extracting at `snippets_cap` --
            // it has to, since every extra snippet costs another pass over the
            // field text. This `take` is the belt to that braces: capping is
            // finding 52's wire contract, and it stays enforced here rather
            // than relying on the primitive having got it right.
            let capped: Vec<Value> = snippets
                .into_iter()
                .take(snippets_cap)
                .map(Value::from)
                .collect();
            per_field.insert((*field_name).to_string(), Value::Array(capped));
        }
        // finding 51: always an entry, `{}` when nothing matched, never
        // absent.
        out.insert(key, Value::Object(per_field));
    }
    Ok(Value::Object(out))
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
    match schema.field_config(field_name) {
        None => bail!("can not highlight undefined field: {field_name}"),
        Some(_) => match schema.value_kind(field_name) {
            Some(ValueKind::Text) => Ok(()),
            _ => bail!("can not highlight a non-text field: {field_name}"),
        },
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
