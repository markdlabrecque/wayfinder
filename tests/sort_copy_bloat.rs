//! Issue #362 measurement: on-disk index cost of the N+1 language-specific
//! sort copies `DocumentBuilder` writes per text/string field, vs. a single
//! language-agnostic sort copy.
//!
//! `#[ignore]`'d because it builds several full indexes and is a measurement,
//! not an assertion. Run explicitly:
//!
//! ```text
//! cargo test --test sort_copy_bloat -- --nocapture --ignored
//! ```
//!
//! Methodology
//! -----------
//! Each document models a real captured `search_api_solr` doc
//! (`solr-ref/search-api/trace/00001.json`): the same sortable source fields
//! (text `title`/`body`, string `field_sku`/`field_keywords`/`type`) plus
//! the same non-sortable companions (`ds_created`, `its_*`, `bs_*`). Sort-copy
//! VALUES are varied per document (real titles/skus differ), so the
//! dictionary-encoded fast columns are not artificially flat.
//!
//! For a given enabled-language count L we index K documents two ways into two
//! fresh cores and measure each data dir's byte size after commit:
//!
//! - **multi(L)** -- current behaviour: every sortable field carries an
//!   `sort_X3b_<lang>_<id>` copy for each of the L languages plus `und`
//!   (L+1 copies per field), exactly what `DocumentBuilder::sortLanguages()`
//!   emits.
//! - **single** -- proposed divergence: every sortable field carries one
//!   language-agnostic `sort_<id>` copy.
//!
//! The base (non-sort) fields are identical between the two, so the size delta
//! is the pure sort-copy cost. The bloat factor for multi(L) over single is
//! what the divergence would reclaim.

mod common;

use std::path::Path;

use axum::Router;
use serde_json::{Value, json};
use tempfile::TempDir;

/// Sortable source fields in the captured trace doc (text + string families).
/// Each gets a sort copy in `DocumentBuilder` because its mapped name begins
/// with 't' or 's' (FieldMapper::usesLanguageSpecificSortCopy()).
const SORTABLE_FIELDS: &[(&str, &str)] = &[
    // (field id, base mapped name at index time)
    ("title", "tm_X3b_en_title"),
    ("body", "tm_X3b_en_body"),
    ("field_sku", "ss_field_sku"),
    ("field_keywords", "sm_field_keywords"),
    ("type", "ss_type"),
];

/// A realistic Drupal langcode pool, taken in order to stand in for "the first
/// L enabled site languages". Order is arbitrary for the size measurement; it
/// only needs to be a fixed, distinct set.
const LANG_POOL: &[&str] = &["en", "fr", "de", "es", "it", "pt", "nl", "da"];

const K: usize = 1200;

/// The first value of a sortable field for document `i` (varied so fast-column
/// dictionary encoding does not flatten the cost).
fn first_value(field_id: &str, i: usize) -> String {
    match field_id {
        "title" => format!("Document {i}: the quick brown fox and related indexing notes"),
        "body" => format!(
            "A classic pangram used to test relevance. Notes for document {i} \
             cover ranking, facets, and the lazy dog."
        ),
        "field_sku" => format!("ART-{i:05}"),
        "field_keywords" => format!("tag-{}", i % 97),
        "type" => "article".to_string(),
        _ => "x".to_string(),
    }
}

/// Builds one document with the given sort-copy strategy.
fn build_doc(i: usize, langs: &[&str], single: bool) -> Value {
    let mut doc = json!({
        "id": format!("doc-{i}"),
        "index_id": "capture_index",
        "ss_search_api_language": "en",
        // non-sortable companions (constant across variants; not part of the delta)
        "ds_created": "2026-07-29T23:03:57Z",
        "its_field_rating": (i % 10),
        "its_nid": i,
        "bs_field_featured": (i.is_multiple_of(2)).to_string(),
        "bs_sticky": "false",
    });

    for &(field_id, base_name) in SORTABLE_FIELDS {
        let value = first_value(field_id, i);
        // base source field (identical across variants)
        doc[base_name] = if field_id == "field_keywords" {
            json!([value.clone(), format!("extra-{i}")])
        } else {
            Value::String(value.clone())
        };
        // sort copies
        if single {
            doc[format!("sort_{field_id}")] = Value::String(value);
        } else {
            for &lang in langs {
                doc[format!("sort_X3b_{lang}_{field_id}")] = Value::String(value.clone());
            }
            doc[format!("sort_X3b_und_{field_id}")] = Value::String(value);
        }
    }
    doc
}

/// Indexes `corpus` into a fresh core built from the shipped search-api preset,
/// committing once (a single segment -- the steady-state shape of a merged
/// index, so the measurement reflects column costs rather than per-segment
/// overhead) and returns the total byte size of the resulting data dir.
async fn index_and_measure(corpus: &[Value]) -> u64 {
    let dir = TempDir::new().expect("temp dir");
    let preset_toml = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("presets/search-api.toml"),
    )
    .expect("preset");
    let app: Router = common::app_with_schema(dir.path(), &preset_toml).expect("app builds");
    let (status, body) = common::post_docs(&app, &Value::Array(corpus.to_vec())).await;
    assert_eq!(
        status,
        axum::http::StatusCode::OK,
        "indexing must succeed: {body}"
    );
    dir_size(&dir.path().join("data"))
}

/// Recursive byte size of a directory tree.
fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

#[tokio::test]
#[ignore = "issue #362 measurement; run with --ignored --nocapture"]
async fn measures_sort_copy_bloat_across_language_counts() {
    let single_corpus: Vec<Value> = (0..K)
        .map(|i| build_doc(i, &LANG_POOL[..1], true))
        .collect();
    let single = index_and_measure(&single_corpus).await;

    println!();
    println!(
        "#362 sort-copy bloat measurement (K = {K} docs, {} sortable fields)",
        SORTABLE_FIELDS.len()
    );
    println!();
    println!("  strategy           L    index bytes    KiB     vs single");
    println!("  ─────────────────────────────────────────────────────────");
    println!(
        "  single (1 copy)    -    {:>11}  {:>7.1}        1.00x",
        single,
        single as f64 / 1024.0
    );

    for &l in &[1usize, 2, 4, 8] {
        let langs = &LANG_POOL[..l];
        let multi_corpus: Vec<Value> = (0..K).map(|i| build_doc(i, langs, false)).collect();
        let multi = index_and_measure(&multi_corpus).await;
        println!(
            "  multi (L+1 copies) {}    {:>11}  {:>7.1}      {:>5.2}x",
            l,
            multi,
            multi as f64 / 1024.0,
            multi as f64 / single as f64,
        );
    }
    println!();
    println!(
        "  Each sortable field carries (L+1) identical-value copies in `multi`;\
         \n  one copy in `single`. The delta is pure redundancy: Wayfinder maps\
         \n  every `sort_*` to plain `string` (no per-language collation), so the\
         \n  language-specific copies are byte-identical with no ordering benefit."
    );
}
