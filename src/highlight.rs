//! Solr highlighting (PRD §5 "Highlighting" row): `hl`/`hl.fl`/`hl.snippets`/
//! `hl.fragsize`/`hl.simple.pre`/`hl.simple.post` -> the `highlighting`
//! response block.
//!
//! Wire semantics live here, the same way `crate::facet` holds Solr's facet
//! semantics on top of `CoreIndex::term_facet`/`count`: which fields
//! highlight, how many snippets, and the exact envelope shape.
//! `CoreIndex::highlight_field` is the Tantivy-facing primitive (one
//! `SnippetGenerator` call per doc/field). Facts pinned by fixtures in
//! `solr-ref/responses/hl_*.json` (`docs/solr-ref-findings.md` findings
//! 51-54):
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
//!   implementation, which the issue explicitly puts out of scope.

use anyhow::{Result, anyhow};
use serde_json::{Map, Value};
use tantivy::query::Query;
use tantivy::{DocAddress, Score};

use crate::core_index::CoreIndex;
use crate::params::Params;

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

    // finding 54: only apply `hl.fragsize` truncation under the explicit
    // `hl.method=original` opt-in -- see module docs.
    let max_chars = if params.get("hl.method") == Some("original") {
        params
            .get("hl.fragsize")
            .and_then(|v| v.parse().ok())
            .filter(|&n: &usize| n > 0)
            .unwrap_or(DEFAULT_FRAGSIZE)
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
            let snippets = index.highlight_field(query, addr, field_name, max_chars, pre, post)?;
            if snippets.is_empty() {
                // finding 51: absent from the per-field map, not `[]`.
                continue;
            }
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
