//! Issue #246: fusing the `facet.field` terms aggregation into the main
//! `/select` search pass must not change a single byte of the wire response
//! (the task's own hard constraint). These tests capture today's (unfused)
//! response as ground truth over a real multi-segment corpus with `fq`
//! present, so a fused implementation has something concrete to match --
//! not a "the fused path returns something plausible" check.
//!
//! This file is deliberately a *characterization* test: it is green against
//! today's implementation, and its job is to stay green across the #246
//! refactor. The tests that actually drive the new API red (`search_top_with_aggs`,
//! `plan_facet_fields`, `render_facet_fields`) live in `src/core_index.rs`
//! and `src/facet.rs`'s own `#[cfg(test)]` modules, where they can reach
//! into the aggregation internals directly.

mod common;

use axum::Router;
use axum::http::StatusCode;
use serde_json::json;
use tempfile::TempDir;

use common::{SCHEMA_TOML, app_with_schema, get, post_docs};

/// Two commits (two segments) over `tests/common`'s shared tracer-bullet
/// schema (`category` string/fast/multi_valued, `body` text_en), so any
/// fused aggregation has a real segment boundary to merge across, not just a
/// single-segment index that would pass even with a broken merge.
async fn multi_segment_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("create temp dir");
    let app = app_with_schema(dir.path(), SCHEMA_TOML).expect("app_with_schema must build");

    post_docs(
        &app,
        &json!([
            {"id":"doc1","body":"the quick brown fox jumps over the lazy dog","category":["animals","classic"]},
            {"id":"doc2","body":"a lazy afternoon in the garden","category":["garden"]},
            {"id":"doc3","body":"quick thinking saves the day","category":["misc","classic"]},
        ]),
    )
    .await;
    post_docs(
        &app,
        &json!([
            {"id":"doc4","body":"dogs and cats living together","category":["animals"]},
            {"id":"doc5","body":"nothing much here at all","category":["animals"]},
            {"id":"doc6","body":"quick foxes everywhere","category":["animals","garden"]},
        ]),
    )
    .await;

    (app, dir)
}

/// The exact benchmark query shape issue #246's own measurement used: `q` +
/// `defType=edismax` + `qf` + `fq` + `facet=true&facet.field` + `hl` +
/// bounded `rows`. `facet_fields.category` here is the whole point: `fq`
/// narrows to 4 docs (doc1/doc4/doc5/doc6, all `category:animals`), but the
/// facet is over `q` **AND** `fq` together, and only doc1/doc6 match `q` --
/// so `animals` must land at 2, not 4. A fused pass that silently dropped
/// the `fq` (aggregating over the full 4-doc `fq` set) or dropped `q`
/// (aggregating over the whole corpus) would both produce a *plausible*
/// non-zero count here without matching this one, which is exactly the
/// failure mode a looser assertion would miss.
#[tokio::test]
async fn benchmark_shaped_request_response_is_unchanged_by_fusion() {
    let (app, _dir) = multi_segment_app().await;

    let (status, body) = get(
        &app,
        "select?q=quick&defType=edismax&qf=body&fq=category:animals&facet=true&\
         facet.field=category&hl=true&hl.fl=body&rows=10&wt=json",
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["response"]["numFound"],
        json!(2),
        "precondition: exactly doc1 and doc6 must match `q=quick` AND `fq=category:animals`"
    );
    assert_eq!(
        body["facet_counts"]["facet_fields"]["category"],
        json!(["animals", 2, "classic", 1, "garden", 1, "misc", 0]),
        "facet_fields.category must be computed over q AND fq together (2 docs: doc1, doc6), \
         not the 4-doc fq set alone and not the whole corpus, and must keep the zero-count \
         `misc` bucket that only a whole-dictionary string-field walk produces"
    );
    let highlighting = body["highlighting"]
        .as_object()
        .expect("hl=true must produce a highlighting object");
    assert!(
        !highlighting.is_empty(),
        "hl=true over real hits must produce non-empty highlighting"
    );
}

/// `facet.mincount`, `facet.sort=index`, and `facet.missing` combined with
/// `fq` and multiple `facet.field` values, over the same multi-segment
/// corpus -- broader coverage of the same "the whole envelope must not move"
/// property than the single benchmark shape above.
#[tokio::test]
async fn multi_field_facet_with_mincount_sort_and_missing_is_unchanged_by_fusion() {
    let (app, _dir) = multi_segment_app().await;

    let (status, body) = get(
        &app,
        "select?q=quick&fq=category:animals&facet=true&facet.field=category&facet.field=id&\
         facet.mincount=1&facet.sort=index&facet.missing=true&rows=0&wt=json",
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["facet_counts"]["facet_fields"]["category"],
        json!(["animals", 2, "classic", 1, "garden", 1, null, 0]),
        "facet.mincount=1 must drop the zero-count `misc` bucket, facet.sort=index must order \
         lexically, and facet.missing must append the null bucket last"
    );
    assert_eq!(
        body["facet_counts"]["facet_fields"]["id"],
        json!(["doc1", 1, "doc6", 1, null, 0]),
        "a second facet.field sharing the request with `category` must be unaffected by it, \
         and must itself stay scoped to q AND fq (doc1, doc6), not the wider 4-doc fq set"
    );
}

/// How many docs the bucket-budget corpus below indexes. Each gets a distinct
/// `id`, and `id` is a string fast field, so `min_doc_count: 0` makes one
/// `facet.field=id` aggregation produce exactly this many buckets regardless
/// of how few docs `q` matches. Two such aggregations therefore produce
/// `2 * 33_000 = 66_000`, which straddles the numbers that matter: over
/// Tantivy's default 65_000 per-*request* bucket limit, and under the
/// `2 * MAX_FACET_TERMS = 130_000` the fused request is now sized to.
const BUCKET_BUDGET_DOCS: usize = 33_000;

/// Review round 1's must-fix, end to end (issue #246).
///
/// Tantivy's bucket limit is a per-request budget summed over every top-level
/// aggregation (`intermediate_agg_result.rs:158-167` +
/// `agg_result.rs:24-30`). The unfused path ran one `AggregationCollector`
/// per `facet.field` and so gave each its own fresh guard; fusing N fields
/// into one request under the default guard would have failed any request
/// whose fields' dictionaries sum past 65_000 — a request real Solr, and
/// Wayfinder one commit earlier, both answer 200.
///
/// Faceting the *same* field under two `{!key=}` labels is the cheapest
/// hermetic way to reach that sum: 33_000 docs, one dictionary, two
/// aggregations over it.
///
/// What this pins, measured by reverting each half of the fix and re-running:
/// reverting **both** `fused_bucket_limit` and `is_aggregation_error` (i.e.
/// the state the fusion landed in before review round 1) fails here with a
/// 500 `wayfinder::SearchError`, "bucket limit was exceeded. Limit: 65000,
/// Current: 66000". Reverting either half *alone* still passes, and that is
/// the design working rather than a gap in the assertion:
///
/// - without `fused_bucket_limit`, Tantivy refuses the fused request and
///   `is_aggregation_error` answers it on the unfused path, whose wire output
///   is byte-identical by construction — a correctness-preserving fallback is
///   not observable from the wire, only from the lost performance;
/// - without `is_aggregation_error`, the sized guard means no aggregation
///   error is raised in the first place. Each terms agg is itself capped at
///   `size = MAX_FACET_TERMS` buckets, so N aggs can never sum past
///   `N * MAX_FACET_TERMS`, and the bucket limit becomes unreachable by
///   construction. The fallback's remaining job is the shared memory counter
///   (500 MB, not hermetically reachable) and defence in depth; the error
///   classification itself is unit-tested in `core_index`'s
///   `aggregation_errors_are_distinguishable_from_other_search_errors`.
#[tokio::test]
async fn two_facet_fields_over_a_dictionary_larger_than_one_fields_budget() {
    let dir = TempDir::new().expect("create temp dir");
    let app = app_with_schema(dir.path(), SCHEMA_TOML).expect("app_with_schema must build");

    // Only the first two docs match `q`, so the hit set stays tiny while the
    // term dictionary — which is what the bucket budget is spent on — does
    // not. Batched to keep the update requests a sane size; the segment count
    // that falls out is incidental.
    for batch_start in (0..BUCKET_BUDGET_DOCS).step_by(5_000) {
        let batch_end = (batch_start + 5_000).min(BUCKET_BUDGET_DOCS);
        let docs: Vec<serde_json::Value> = (batch_start..batch_end)
            .map(|i| {
                let body = if i < 2 {
                    "quick brown fox"
                } else {
                    "filler text"
                };
                json!({"id": format!("doc{i:05}"), "body": body})
            })
            .collect();
        let (status, body) = post_docs(&app, &json!(docs)).await;
        assert_eq!(status, StatusCode::OK, "indexing batch failed: {body}");
    }

    let (status, body) = get(
        &app,
        // `facet.field={!key=a}id` and `{!key=b}id`, percent-encoded.
        "select?q=quick&rows=0&facet=true&facet.limit=3\
         &facet.field=%7B%21key%3Da%7Did&facet.field=%7B%21key%3Db%7Did&wt=json",
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "two facet fields whose bucket counts sum past one field's budget must still be a \
         200 -- a 500 here means the fused request was refused by Tantivy's per-request \
         bucket limit and that refusal was not caught and answered on the unfused path: {body}"
    );
    assert_eq!(
        body["response"]["numFound"],
        json!(2),
        "precondition: exactly the two `quick` docs match"
    );
    // Real counts, not an empty or degraded block: the two zero-count buckets
    // after the two hits are what proves the whole-dictionary walk happened.
    let expected = json!(["doc00000", 1, "doc00001", 1, "doc00002", 0]);
    assert_eq!(
        body["facet_counts"]["facet_fields"]["a"], expected,
        "the first label's counts must survive the fused request intact"
    );
    assert_eq!(
        body["facet_counts"]["facet_fields"]["b"], expected,
        "and the second label's, over the same column, must be identical to the first's"
    );
}
