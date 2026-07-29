//! `defType=edismax` (issue #7, PRD §5 v1 exception) — `q`/`qf`/`pf`/`mm`/
//! `tie`/`boost`/`bq` over `GET /solr/<core>/select`.
//!
//! Every expected value here comes from a committed fixture in
//! `solr-ref/responses/edismax_*.json`, captured against a dedicated 10-doc
//! corpus (`solr-ref/capture.sh`'s edismax block, container
//! `wayfinder-solr-7`, port 8994) — the canonical 5-doc tracer-bullet corpus
//! has only one text field (`body`) and an unanalyzed `category`, so it
//! cannot exercise `qf`'s per-field weighting or `pf`'s phrase boost at all.
//! Same precedent as `tests/mlt.rs`'s dedicated MLT corpus. Nothing here is
//! derived from what Wayfinder happens to produce; `docs/solr-ref-findings.md`
//! findings 68-75 record what was learned capturing it.
//!
//! ## Scope, per the issue
//!
//! `q` (free text, quoted phrases, `+`/`-` operators), `qf` with per-field
//! boosts, `pf` (phrase boost over the same fields as `qf`), `mm` (its own
//! grammar, unit-tested independently in `tests/mm.rs`), `tie`, `boost`
//! (multiplicative), `bq` (additive). Out of scope: `bf`, `pf2`/`pf3`, `ps`,
//! stopwords, `lowercaseOperators`, full function-query syntax — an
//! unsupported edismax param must be ignored like any other unknown param
//! (finding 8), not rejected.
//!
//! ## Why score-bearing fixtures are compared structurally, not by exact float
//!
//! PRD ratified-divergence 4 (`tests/differential.rs`'s
//! `RANKED_SCORE_VALUE_RATIFIED`, `tests/mlt.rs`'s
//! `blank_bm25_score_magnitudes`): Tantivy's BM25 and Solr/Lucene's
//! BM25Similarity numerically disagree on the same corpus by a real,
//! permanently-accepted margin. A fixture that carries `score` is therefore
//! ground truth for *which* documents match, their *rank order*, and
//! *structural relationships between scores in the same engine* (a doubled
//! `boost` doubles every score; `tie` moves only a doc matching in more than
//! one `qf` field; `bq` leaves a non-matching doc's score untouched) — never
//! for the raw float value transplanted from Solr onto Wayfinder. Every
//! score-based assertion below checks one of those structural relationships
//! within Wayfinder's own two responses, using the fixture only to know
//! which documents and which relationship to expect (findings 69-73).
//!
//! `mm`'s own grammar-to-integer arithmetic has no HTTP-level test here at
//! all — see `tests/mm.rs`, which is a pure unit-test file with no fixture
//! dependency by design (finding 68). This file's `mm_*` tests exercise only
//! the wiring: does `mm` reach the query that actually runs.

mod common;

use axum::Router;
use axum::http::StatusCode;
use common::diff::score_tolerance;
use common::{app_with_schema, assert_matches_fixture, fixture, get};
use serde_json::Value;
use std::collections::HashMap;
use tempfile::TempDir;

/// Two text_en fields (`title`, `body`) over the same `id` shape as the
/// canonical tracer-bullet schema — matches `solr-ref/capture.sh`'s edismax
/// block schema exactly (`docs/solr-ref-findings.md`, "Findings from issue
/// #7"), so the captured fixtures are ground truth here too. Core named
/// `content` per the same convention `tests/mlt.rs`'s `MLT_SCHEMA_TOML`
/// documents: Wayfinder's own core name is independent of the Solr core the
/// fixtures were captured from.
const EDISMAX_SCHEMA_TOML: &str = r#"
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
name = "title"
type = "text_en"
stored = true

[[fields]]
name = "body"
type = "text_en"
stored = true
"#;

/// The exact 10-doc corpus `solr-ref/capture.sh`'s edismax block indexes,
/// purpose-built per knob (see the block's own comments and findings 68-75):
/// `eA`/`eB` swap which field carries the query terms (for `qf` boost
/// reordering), `eC`/`eD` isolate `tie`'s effect to a doc matching in both
/// fields, `pA`/`pB` share the same two words with different adjacency (for
/// `pf`), and `mmA`-`mmD` contain exactly 3/2/1/0 of a 3-word query (for
/// `mm`).
fn edismax_corpus() -> Value {
    serde_json::json!([
        {"id":"eA",  "title":"rocket launch success",               "body":"filler unrelated text about weather"},
        {"id":"eB",  "title":"filler unrelated text about weather",  "body":"rocket launch success"},
        {"id":"eC",  "title":"rocket mission",                       "body":"the rocket soared past the rocket pad toward the rocket"},
        {"id":"eD",  "title":"rocket rocket rocket mission control", "body":"launch complete"},
        {"id":"pA",  "title":"phrase doc a",                         "body":"a quick fox ran away"},
        {"id":"pB",  "title":"phrase doc b",                         "body":"a fox that is quick ran away"},
        {"id":"mmA", "title":"mm doc a",                             "body":"alpha beta gamma"},
        {"id":"mmB", "title":"mm doc b",                             "body":"alpha beta"},
        {"id":"mmC", "title":"mm doc c",                             "body":"alpha"},
        {"id":"mmD", "title":"mm doc d",                             "body":"nothing relevant here at all"}
    ])
}

/// Builds an app on `EDISMAX_SCHEMA_TOML` and indexes `edismax_corpus()`.
async fn edismax_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = app_with_schema(dir.path(), EDISMAX_SCHEMA_TOML).expect("app must build");
    let (status, body) = common::post_docs(&app, &edismax_corpus()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the edismax corpus must succeed, got {body}"
    );
    (app, dir)
}

/// Extracts `{id: score}` from `response.docs`, panicking if any doc is
/// missing an `id` or a `score` — every query in this file that needs this
/// helper requests `fl=id,score` explicitly.
fn scores_by_id(envelope: &Value) -> HashMap<String, f64> {
    envelope
        .pointer("/response/docs")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("response.docs must be an array in {envelope}"))
        .iter()
        .map(|doc| {
            let id = doc
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("doc missing string `id`: {doc}"))
                .to_string();
            let score = doc
                .get("score")
                .and_then(Value::as_f64)
                .unwrap_or_else(|| panic!("doc {id} missing numeric `score`: {doc}"));
            (id, score)
        })
        .collect()
}

/// Ordered `response.docs[].id` list, for order-sensitive assertions on
/// fixtures that carry no `score` (so there is no BM25-magnitude-divergence
/// concern — see this file's module doc).
fn ordered_ids(envelope: &Value) -> Vec<String> {
    envelope
        .pointer("/response/docs")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("response.docs must be an array in {envelope}"))
        .iter()
        .map(|doc| {
            doc.get("id")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("doc missing string `id`: {doc}"))
                .to_string()
        })
        .collect()
}

// --- basic route / envelope shape -------------------------------------------

#[tokio::test]
async fn edismax_route_returns_200() {
    let (app, _dir) = edismax_app().await;
    let (status, body) = get(
        &app,
        "select?q=rocket&defType=edismax&qf=title+body&fl=id&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "GET /solr/<core>/select?defType=edismax must be 200, got {body}"
    );
}

#[tokio::test]
async fn edismax_basic_matches_committed_fixture() {
    let (app, _dir) = edismax_app().await;
    let (status, body) = get(
        &app,
        "select?q=rocket&defType=edismax&qf=title+body&fl=id&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "edismax_basic");
}

// --- qf: per-field boost changes relative order (finding 69) ---------------

#[tokio::test]
async fn qf_unboosted_matches_committed_fixture_order() {
    let (app, _dir) = edismax_app().await;
    let (status, body) = get(
        &app,
        "select?q=rocket+launch+success&defType=edismax&qf=title+body&fl=id&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "edismax_qf_equal");
}

#[tokio::test]
async fn qf_boosted_toward_title_matches_committed_fixture_order() {
    let (app, _dir) = edismax_app().await;
    let (status, body) = get(
        &app,
        "select?q=rocket+launch+success&defType=edismax&qf=title^10+body&fl=id&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "edismax_qf_boost_title");
}

#[tokio::test]
async fn qf_boosted_toward_body_matches_committed_fixture_order() {
    let (app, _dir) = edismax_app().await;
    let (status, body) = get(
        &app,
        "select?q=rocket+launch+success&defType=edismax&qf=title+body^10&fl=id&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "edismax_qf_boost_body");
}

#[tokio::test]
async fn qf_boost_direction_actually_reorders_ea_and_eb() {
    // The two fixtures above pin exact order against Solr; this test pins
    // the property those fixtures exist to demonstrate (finding 69) as an
    // explicit, load-bearing assertion so a future refactor that keeps both
    // fixtures passing by accident (e.g. by ignoring `qf` boosts entirely
    // and always returning the same order) cannot silently regress this.
    let (app, _dir) = edismax_app().await;
    let (_, title_boosted) = get(
        &app,
        "select?q=rocket+launch+success&defType=edismax&qf=title^10+body&fl=id&wt=json",
    )
    .await;
    let (_, body_boosted) = get(
        &app,
        "select?q=rocket+launch+success&defType=edismax&qf=title+body^10&fl=id&wt=json",
    )
    .await;
    let title_ids = ordered_ids(&title_boosted);
    let body_ids = ordered_ids(&body_boosted);
    let ea_rank_title_boosted = title_ids.iter().position(|id| id == "eA");
    let eb_rank_title_boosted = title_ids.iter().position(|id| id == "eB");
    let ea_rank_body_boosted = body_ids.iter().position(|id| id == "eA");
    let eb_rank_body_boosted = body_ids.iter().position(|id| id == "eB");
    assert!(
        ea_rank_title_boosted < eb_rank_title_boosted,
        "boosting `title` must rank the title-only match (eA) above the body-only match (eB), \
         got order {title_ids:?}"
    );
    assert!(
        eb_rank_body_boosted < ea_rank_body_boosted,
        "boosting `body` must rank the body-only match (eB) above the title-only match (eA), \
         got order {body_ids:?}"
    );
}

// --- pf: phrase boost is additive, not a replacement (finding 70) ----------

#[tokio::test]
async fn pf_absent_gives_identical_scores_for_adjacent_and_non_adjacent_matches() {
    let (app, _dir) = edismax_app().await;
    let (status, body) = get(
        &app,
        "select?q=quick+fox&defType=edismax&qf=body&fl=id,score&fq=id:(pA+OR+pB)&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let scores = scores_by_id(&body);
    assert_eq!(
        scores.len(),
        2,
        "both pA and pB must match on bag-of-words terms alone, got {scores:?}"
    );
    let pa = scores["pA"];
    let pb = scores["pB"];
    assert!(
        (pa - pb).abs() <= score_tolerance(),
        "without `pf`, pA (adjacent phrase) and pB (same words, not adjacent) must score \
         identically: pA={pa}, pB={pb}"
    );
}

#[tokio::test]
async fn pf_present_boosts_only_the_adjacent_phrase_match() {
    let (app, _dir) = edismax_app().await;
    let (status_off, body_off) = get(
        &app,
        "select?q=quick+fox&defType=edismax&qf=body&fl=id,score&fq=id:(pA+OR+pB)&wt=json",
    )
    .await;
    let (status_on, body_on) = get(
        &app,
        "select?q=quick+fox&defType=edismax&qf=body&pf=body&fl=id,score&fq=id:(pA+OR+pB)&wt=json",
    )
    .await;
    assert_eq!(status_off, StatusCode::OK);
    assert_eq!(status_on, StatusCode::OK);
    let off = scores_by_id(&body_off);
    let on = scores_by_id(&body_on);
    assert!(
        on["pA"] > off["pA"],
        "adding `pf=body` must raise pA's score (it has the literal adjacent phrase): \
         off={}, on={}",
        off["pA"],
        on["pA"]
    );
    assert!(
        (on["pB"] - off["pB"]).abs() <= score_tolerance(),
        "adding `pf=body` must leave pB's score unchanged (its two words are not adjacent, so \
         the phrase clause never matches it): off={}, on={}",
        off["pB"],
        on["pB"]
    );
}

// --- tie: only moves a doc matching in more than one qf field (finding 71) -

#[tokio::test]
async fn tie_raises_the_score_of_a_doc_matching_in_two_fields() {
    let (app, _dir) = edismax_app().await;
    let (status0, body0) = get(
        &app,
        "select?q=rocket&defType=edismax&qf=title+body&tie=0&fl=id,score&fq=id:(eC+OR+eD)&wt=json",
    )
    .await;
    let (status1, body1) = get(
        &app,
        "select?q=rocket&defType=edismax&qf=title+body&tie=1&fl=id,score&fq=id:(eC+OR+eD)&wt=json",
    )
    .await;
    assert_eq!(status0, StatusCode::OK);
    assert_eq!(status1, StatusCode::OK);
    let tie0 = scores_by_id(&body0);
    let tie1 = scores_by_id(&body1);
    assert!(
        tie1["eC"] > tie0["eC"],
        "eC matches `rocket` in both title and body, so raising `tie` from 0 to 1 must raise \
         its score: tie=0 -> {}, tie=1 -> {}",
        tie0["eC"],
        tie1["eC"]
    );
}

#[tokio::test]
async fn tie_does_not_move_a_doc_matching_in_only_one_field() {
    let (app, _dir) = edismax_app().await;
    let (_, body0) = get(
        &app,
        "select?q=rocket&defType=edismax&qf=title+body&tie=0&fl=id,score&fq=id:(eC+OR+eD)&wt=json",
    )
    .await;
    let (_, body1) = get(
        &app,
        "select?q=rocket&defType=edismax&qf=title+body&tie=1&fl=id,score&fq=id:(eC+OR+eD)&wt=json",
    )
    .await;
    let tie0 = scores_by_id(&body0);
    let tie1 = scores_by_id(&body1);
    assert!(
        (tie1["eD"] - tie0["eD"]).abs() <= score_tolerance(),
        "eD matches `rocket` only in its title, so `tie` has no second field's score to blend \
         in and must not move its score at all: tie=0 -> {}, tie=1 -> {}",
        tie0["eD"],
        tie1["eD"]
    );
}

// --- boost: pure multiplicative wrapper (finding 72) ------------------------

#[tokio::test]
async fn boost_multiplies_every_docs_score_by_the_same_factor() {
    let (app, _dir) = edismax_app().await;
    let (status_base, base) = get(
        &app,
        "select?q=rocket&defType=edismax&qf=title+body&fl=id,score&fq=id:(eA+OR+eB+OR+eC+OR+eD)&wt=json",
    )
    .await;
    let (status_boosted, boosted) = get(
        &app,
        "select?q=rocket&defType=edismax&qf=title+body&boost=2&fl=id,score&fq=id:(eA+OR+eB+OR+eC+OR+eD)&wt=json",
    )
    .await;
    assert_eq!(status_base, StatusCode::OK);
    assert_eq!(status_boosted, StatusCode::OK);
    let base_scores = scores_by_id(&base);
    let boosted_scores = scores_by_id(&boosted);
    assert_eq!(
        base_scores.len(),
        4,
        "baseline must return all four eA-eD docs, got {base_scores:?}"
    );
    for (id, base_score) in &base_scores {
        let boosted_score = boosted_scores
            .get(id)
            .unwrap_or_else(|| panic!("boost=2 response missing doc {id}: {boosted_scores:?}"));
        assert!(
            (boosted_score - 2.0 * base_score).abs() <= score_tolerance(),
            "boost=2 must exactly double every doc's score: doc {id}, base={base_score}, \
             boosted={boosted_score}, expected ~{}",
            2.0 * base_score
        );
    }
}

// --- bq: additive, and only for docs matching the bq clause (finding 73) ---

#[tokio::test]
async fn bq_leaves_non_matching_docs_score_unchanged() {
    let (app, _dir) = edismax_app().await;
    let (_, base) = get(
        &app,
        "select?q=rocket&defType=edismax&qf=title+body&fl=id,score&fq=id:(eA+OR+eB+OR+eC+OR+eD)&wt=json",
    )
    .await;
    let (_, with_bq) = get(
        &app,
        "select?q=rocket&defType=edismax&qf=title+body&bq=title:mission^5&fl=id,score&fq=id:(eA+OR+eB+OR+eC+OR+eD)&wt=json",
    )
    .await;
    let base_scores = scores_by_id(&base);
    let bq_scores = scores_by_id(&with_bq);
    for id in ["eA", "eB"] {
        assert!(
            (bq_scores[id] - base_scores[id]).abs() <= score_tolerance(),
            "eA/eB have no \"mission\" in their title, so `bq=title:mission^5` must leave their \
             score unchanged: doc {id}, base={}, with_bq={}",
            base_scores[id],
            bq_scores[id]
        );
    }
}

#[tokio::test]
async fn bq_strictly_raises_the_score_of_matching_docs() {
    let (app, _dir) = edismax_app().await;
    let (_, base) = get(
        &app,
        "select?q=rocket&defType=edismax&qf=title+body&fl=id,score&fq=id:(eA+OR+eB+OR+eC+OR+eD)&wt=json",
    )
    .await;
    let (_, with_bq) = get(
        &app,
        "select?q=rocket&defType=edismax&qf=title+body&bq=title:mission^5&fl=id,score&fq=id:(eA+OR+eB+OR+eC+OR+eD)&wt=json",
    )
    .await;
    let base_scores = scores_by_id(&base);
    let bq_scores = scores_by_id(&with_bq);
    for id in ["eC", "eD"] {
        assert!(
            bq_scores[id] > base_scores[id],
            "eC/eD both have \"mission\" in their title, so `bq=title:mission^5` must strictly \
             raise their score: doc {id}, base={}, with_bq={}",
            base_scores[id],
            bq_scores[id]
        );
    }
}

// --- q grammar: quoted phrases, `+`/`-` operators (finding 74) -------------

#[tokio::test]
async fn quoted_phrase_in_q_matches_the_committed_fixture() {
    let (app, _dir) = edismax_app().await;
    let (status, body) = get(
        &app,
        "select?q=%22quick+fox%22&defType=edismax&qf=body&fl=id&fq=id:(pA+OR+pB)&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "edismax_quoted_phrase");
}

#[tokio::test]
async fn minus_operator_excludes_matches_the_committed_fixture() {
    let (app, _dir) = edismax_app().await;
    let (status, body) = get(
        &app,
        "select?q=rocket+-mission&defType=edismax&qf=title+body&fl=id&fq=id:(eA+OR+eB+OR+eC+OR+eD)&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "edismax_operators_exclude");
}

#[tokio::test]
async fn plus_operator_requires_matches_the_committed_fixture() {
    let (app, _dir) = edismax_app().await;
    let (status, body) = get(
        &app,
        "select?q=%2Brocket+%2Blaunch&defType=edismax&qf=title+body&fl=id&fq=id:(eA+OR+eB+OR+eC+OR+eD)&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "edismax_operators_required");
}

// --- mm wiring: the grammar itself is tests/mm.rs's job (finding 68) -------

#[tokio::test]
async fn mm_1_matches_the_committed_fixture() {
    let (app, _dir) = edismax_app().await;
    let (status, body) = get(
        &app,
        "select?q=alpha+beta+gamma&defType=edismax&qf=body&mm=1&fl=id&fq=id:(mmA+OR+mmB+OR+mmC+OR+mmD)&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "edismax_mm_1");
}

#[tokio::test]
async fn mm_2_matches_the_committed_fixture() {
    let (app, _dir) = edismax_app().await;
    let (status, body) = get(
        &app,
        "select?q=alpha+beta+gamma&defType=edismax&qf=body&mm=2&fl=id&fq=id:(mmA+OR+mmB+OR+mmC+OR+mmD)&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "edismax_mm_2");
}

#[tokio::test]
async fn mm_3_matches_the_committed_fixture() {
    let (app, _dir) = edismax_app().await;
    let (status, body) = get(
        &app,
        "select?q=alpha+beta+gamma&defType=edismax&qf=body&mm=3&fl=id&fq=id:(mmA+OR+mmB+OR+mmC+OR+mmD)&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "edismax_mm_3");
}

#[tokio::test]
async fn mm_conditional_grammar_matches_the_committed_fixture() {
    let (app, _dir) = edismax_app().await;
    let (status, body) = get(
        &app,
        "select?q=alpha+beta+gamma&defType=edismax&qf=body&mm=2%3C-1+3%3C80%25&fl=id&fq=id:(mmA+OR+mmB+OR+mmC+OR+mmD)&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "edismax_mm_conditional");
}

// --- unsupported edismax params are ignored, not rejected (finding 75) -----

#[tokio::test]
async fn unsupported_bf_param_is_ignored_like_any_unknown_param() {
    // `bf` (function-query boost) is explicitly out of scope (PRD §5); real
    // Solr does NOT ignore it (finding 75), so this is a Wayfinder-internal
    // consistency check, not a fixture comparison: passing `bf` must not
    // change the response at all versus the identical query without it.
    let (app, _dir) = edismax_app().await;
    let (status_without, without) = get(
        &app,
        "select?q=rocket&defType=edismax&qf=title+body&fl=id,score&fq=id:(eA+OR+eB+OR+eC+OR+eD)&wt=json",
    )
    .await;
    let (status_with, with) = get(
        &app,
        "select?q=rocket&defType=edismax&qf=title+body&bf=recip(rord(id),1,2,3)&fl=id,score&fq=id:(eA+OR+eB+OR+eC+OR+eD)&wt=json",
    )
    .await;
    assert_eq!(status_without, StatusCode::OK);
    assert_eq!(
        status_with,
        StatusCode::OK,
        "an unsupported edismax param must not 400, same as any other unknown param (finding 8)"
    );
    let scores_without = scores_by_id(&without);
    let scores_with = scores_by_id(&with);
    assert_eq!(
        scores_without.len(),
        scores_with.len(),
        "an ignored `bf` must not change which docs match"
    );
    for (id, score_without) in &scores_without {
        let score_with = scores_with
            .get(id)
            .unwrap_or_else(|| panic!("doc {id} missing once `bf` is added: {scores_with:?}"));
        assert!(
            (score_with - score_without).abs() <= score_tolerance(),
            "an ignored `bf` must not change doc {id}'s score: without={score_without}, \
             with={score_with}"
        );
    }
}

// --- a captured Solr fact this file assumes, made explicit -----------------

#[tokio::test]
async fn fixture_names_referenced_by_this_file_all_exist_in_the_manifest() {
    // Guards against a rename/typo silently turning a real assertion above
    // into a fixture-not-found panic that reads like a missing-feature
    // failure instead of a test-authoring bug.
    for name in [
        "edismax_basic",
        "edismax_qf_equal",
        "edismax_qf_boost_title",
        "edismax_qf_boost_body",
        "edismax_quoted_phrase",
        "edismax_operators_exclude",
        "edismax_operators_required",
        "edismax_mm_1",
        "edismax_mm_2",
        "edismax_mm_3",
        "edismax_mm_conditional",
    ] {
        let _ = fixture(name);
    }
}
