//! The index/query analyzer split, from the `/select` read path (issue #389
//! Phase 1).
//!
//! `tests/schema_layer.rs` pins the split at the `WayfinderSchema` level
//! (`tokenize` vs `tokenize_query`). That is not enough: the main `q` path never
//! calls either. It hands the query string to Tantivy's own `QueryParser`, which
//! resolves each field's analyzer *itself*, inside Tantivy, from the
//! `TokenizerManager` it was constructed with and keyed by the tokenizer name
//! recorded in the schema — the field's **index** identity. So an analyzer that
//! is only reachable through a Wayfinder-side name lookup is not reachable from
//! `q` at all, and `QueryParser::for_index` is exactly
//! `QueryParser::new(index.schema(), fields, index.tokenizers().clone())`
//! (tantivy-0.26.1), i.e. the indexing manager.
//!
//! This path is where the split has to pay off: query-side synonym expansion —
//! the reason to have a query-side chain at all, since it costs nothing on disk
//! and lets the synonym table change without a reindex — is a `/select` `q`
//! feature. A seam that `q` cannot reach is not a seam. Hence these tests, which
//! pin the *reach* of the split rather than its existence.
//!
//! The field type below is deliberately asymmetric in a way no shipped chain is:
//! the index side stems (`runners` → `runner`), the query side does not. So
//! `q=runners` matches **only** if the query text was analyzed with the index
//! analyzer, and `q=runner` matches either way. That makes the negative
//! assertion (0 hits) meaningful: it is paired with a positive control on the
//! same corpus, field and request shape.

mod common;

use axum::http::StatusCode;
use common::{app_with_schema, get, post_docs};
use serde_json::{Value, json};
use tempfile::TempDir;

/// Index chain: lowercase + English stemmer. Query chain: lowercase only.
const SPLIT_SCHEMA_TOML: &str = r#"
[core]
name = "content"
unique_key = "id"
default_field = "body"

[[fields]]
name = "id"
type = "string"
stored = true
required = true
fast = true

[[fields]]
name = "body"
type = "an389_asym"
stored = true

[[field_types]]
name = "an389_asym"
tokenizer = "simple"
query_tokenizer = "simple"
[[field_types.filters]]
kind = "lowercase"
[[field_types.filters]]
kind = "stemmer"
language = "english"
[[field_types.query_filters]]
kind = "lowercase"
"#;

async fn split_app() -> (axum::Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = app_with_schema(dir.path(), SPLIT_SCHEMA_TOML).expect("split schema must load");
    let (status, body) = post_docs(&app, &json!([{"id": "d1", "body": "quick runners"}])).await;
    assert_eq!(status, StatusCode::OK, "indexing must succeed: {body}");
    (app, dir)
}

fn num_found(body: &Value) -> u64 {
    body["response"]["numFound"]
        .as_u64()
        .unwrap_or_else(|| panic!("response.numFound must be a number, got {body}"))
}

/// The headline reach test: a field-less `q` literal goes through Tantivy's
/// `QueryParser`, so the parser must be built on the query-side manager.
#[tokio::test]
async fn select_q_literal_is_analyzed_by_the_query_analyzer() {
    let (app, _dir) = split_app().await;

    // Positive control: the query chain lowercases, so `RUNNER` reaches the
    // indexed stem `runner` and the document is findable at all.
    let (status, body) = get(&app, "select?q=RUNNER").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        num_found(&body),
        1,
        "`q=RUNNER` must match: the query chain lowercases and `runner` is the \
         indexed stem -- {body}"
    );

    // The discriminator: only the *index* chain stems, so `runners` reduces to
    // `runner` (and matches) if and only if the query text was analyzed with
    // the index analyzer. It must not be.
    let (status, body) = get(&app, "select?q=runners").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        num_found(&body),
        0,
        "`q=runners` must NOT match: the query analyzer carries no stemmer, so \
         the term stays `runners` while the index holds `runner`. A match here \
         means the `q` path is still analyzing query text with the *indexing* \
         tokenizer manager (issue #389 Phase 1) -- {body}"
    );
}

/// Same seam, reached through an explicit field name rather than `df`: a
/// fielded literal is delegated to the same parser.
#[tokio::test]
async fn select_fielded_q_literal_is_analyzed_by_the_query_analyzer() {
    let (app, _dir) = split_app().await;

    let (status, body) = get(&app, "select?q=body%3Arunner").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(num_found(&body), 1, "`q=body:runner` must match -- {body}");

    let (status, body) = get(&app, "select?q=body%3Arunners").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        num_found(&body),
        0,
        "`q=body:runners` must not match: the query chain does not stem -- {body}"
    );
}

/// A quoted phrase takes the same route and must resolve the same analyzer:
/// `\"quick runners\"` is a phrase of query-side tokens.
#[tokio::test]
async fn select_phrase_q_is_analyzed_by_the_query_analyzer() {
    let (app, _dir) = split_app().await;

    let (status, body) = get(&app, "select?q=%22quick+runner%22").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        num_found(&body),
        1,
        "the indexed stems are `quick runner`, so that phrase must match -- {body}"
    );

    let (status, body) = get(&app, "select?q=%22quick+runners%22").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        num_found(&body),
        0,
        "`\"quick runners\"` must not match: the query chain does not stem -- {body}"
    );
}

/// edismax builds its field disjunction from `tokenize_for_target`, a
/// Wayfinder-side analysis site rather than Tantivy's parser. It must resolve
/// the query side too, or `qf` and plain `q` would disagree about the same text.
#[tokio::test]
async fn edismax_qf_literal_is_analyzed_by_the_query_analyzer() {
    let (app, _dir) = split_app().await;

    let (status, body) = get(&app, "select?defType=edismax&qf=body&q=runner").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        num_found(&body),
        1,
        "edismax `q=runner` must match -- {body}"
    );

    let (status, body) = get(&app, "select?defType=edismax&qf=body&q=runners").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        num_found(&body),
        0,
        "edismax `q=runners` must not match: `tokenize_for_target` analyzes a \
         `qf` clause's own literal text, which is query text -- {body}"
    );
}
