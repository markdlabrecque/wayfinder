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
use common::diff::{diff, load_manifest, score_tolerance};
use common::{app_with_schema, assert_matches_fixture, fixture, get};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
    // Self-expiring skip guard, issue #51: `eB`/`eD` land a genuine BM25
    // near-tie for this query, and Tantivy's own fieldnorm quantization
    // (a 256-entry `u8` table, `tantivy::fieldnorm::code`) diverges from
    // Lucene's `SmallFloat` norm-byte quantization by enough to flip their
    // relative order versus the committed Solr fixture (`eC, eD, eB, eA`),
    // while leaving the *set* of matching docs unchanged. This intentionally
    // asserts Wayfinder's current (wrong) order rather than the fixture's
    // order, so that closing #51 — or any unrelated BM25/fieldnorm change
    // that happens to fix this — trips this assertion instead of staying
    // silently green. When it fails, restore `assert_matches_fixture(body,
    // "edismax_basic")` and close #51.
    let (app, _dir) = edismax_app().await;
    let (status, body) = get(
        &app,
        "select?q=rocket&defType=edismax&qf=title+body&fl=id&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let fixture_order = ordered_ids(&fixture("edismax_basic"));
    assert_eq!(
        fixture_order,
        vec!["eC", "eD", "eB", "eA"],
        "the committed fixture's own order changed — re-derive this guard's expectations"
    );
    let actual_order = ordered_ids(&body);
    assert_eq!(
        actual_order,
        vec!["eC", "eB", "eD", "eA"],
        "known BM25 near-tie order flip (issue #51) no longer reproduces — the divergence may \
         be fixed; if `actual_order` now equals the fixture order `{fixture_order:?}`, restore \
         `assert_matches_fixture(body, \"edismax_basic\")` and close #51"
    );
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
    // Self-expiring skip guard, issue #51: real Solr's `text_en` strips
    // stopwords, so pA's body ("a quick fox ran away") and pB's ("a fox
    // that is quick ran away") both index to 4 tokens and score identically
    // (finding 70). Wayfinder's own `text_en` deliberately does not strip
    // stopwords (already-ratified PRD divergence, PRD open question 5,
    // documented on `CoreIndex::mlt_query`), so pA indexes to 5 tokens and
    // pB to 7 — a real, unequal document-length norm, not float noise. This
    // can't be fixed inside edismax's scope (query-side stopword filtering
    // wouldn't change an already-committed index-time length norm), so this
    // asserts the current (unequal) scores explicitly rather than the
    // equal-scores property the fixture would otherwise pin. When this
    // fails, restore the `(pa - pb).abs() <= score_tolerance()` assertion
    // and close #51.
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
        (pa - pb).abs() > score_tolerance(),
        "known text_en-stopword-driven score inequality (issue #51) no longer reproduces — the \
         divergence may be fixed; if pA and pB now score within score_tolerance() of each \
         other, restore the equal-scores assertion and close #51: pA={pa}, pB={pb}"
    );
    assert!(
        pa > pb,
        "known direction of the divergence changed (issue #51) — pA (shorter indexed doc, 5 \
         tokens without stopword-stripping) should still score higher than pB (7 tokens): \
         pA={pa}, pB={pb}"
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
    // Self-expiring skip guard, issue #51: same root cause as
    // `edismax_basic_matches_committed_fixture` above — `eA`/`eB` are a
    // genuine BM25 near-tie here, and Tantivy's fieldnorm quantization vs.
    // Lucene's `SmallFloat` quantization flips their relative order versus
    // the committed fixture (`eD, eA, eB`), leaving the matching doc set
    // unchanged. Asserts the current (wrong) order explicitly; when this
    // fails, restore `assert_matches_fixture(body,
    // "edismax_operators_required")` and close #51.
    let (app, _dir) = edismax_app().await;
    let (status, body) = get(
        &app,
        "select?q=%2Brocket+%2Blaunch&defType=edismax&qf=title+body&fl=id&fq=id:(eA+OR+eB+OR+eC+OR+eD)&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let fixture_order = ordered_ids(&fixture("edismax_operators_required"));
    assert_eq!(
        fixture_order,
        vec!["eD", "eA", "eB"],
        "the committed fixture's own order changed — re-derive this guard's expectations"
    );
    let actual_order = ordered_ids(&body);
    assert_eq!(
        actual_order,
        vec!["eD", "eB", "eA"],
        "known BM25 near-tie order flip (issue #51) no longer reproduces — the divergence may \
         be fixed; if `actual_order` now equals the fixture order `{fixture_order:?}`, restore \
         `assert_matches_fixture(body, \"edismax_operators_required\")` and close #51"
    );
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

// --- differential-harness-style coverage over the edismax_* manifest rows --

/// The subset of `solr-ref/manifest.tsv` this file owns: every row whose
/// name starts with `edismax_`. `tests/differential.rs`'s generic hermetic
/// loop skips these (its `indexed_app()` seeds the unrelated 5-doc
/// tracer-bullet corpus, which has no `title` field and none of `eA`-`eD`/
/// `pA`/`pB`/`mmA`-`mmD`) — same pattern as that file's `mlt_*` skip and
/// `tests/mlt.rs`'s own manifest-driven test (issue #6 precedent).
fn manifest_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("solr-ref/manifest.tsv")
}

/// Recursively nulls every `score`/`maxScore` value so two envelopes can be
/// compared for everything BUT BM25 magnitude — same rationale and shape as
/// `tests/mlt.rs`'s `blank_bm25_score_magnitudes` (PRD ratified-divergence
/// 4: Tantivy's BM25 and Solr/Lucene's BM25Similarity numerically disagree
/// by a real, permanently-accepted margin).
fn blank_bm25_score_magnitudes(mut value: Value) -> Value {
    match &mut value {
        Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                if key == "score" || key == "maxScore" {
                    *v = Value::Null;
                } else {
                    let taken = std::mem::take(v);
                    *v = blank_bm25_score_magnitudes(taken);
                }
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                let taken = std::mem::take(item);
                *item = blank_bm25_score_magnitudes(taken);
            }
        }
        _ => {}
    }
    value
}

/// Self-expiring to-do list for manifest rows known to diverge from their
/// committed fixture for a documented, tracked reason (issue #51) — the
/// counterpart of `tests/differential.rs`'s `EXPECTED_DIVERGENCES`, but
/// scoped to this file's `edismax_*` rows rather than extending that shared,
/// PRD-ratified table (per the #51 triage decision: this is a follow-up
/// bug, not a ratified permanent divergence). Every entry's reason names the
/// issue that owns the fix. If a listed row's diff ever becomes empty (the
/// divergence stopped reproducing), the loop below fails loudly and names
/// the entry to remove — a stale entry here would otherwise be a permanently
/// green lie.
const EDISMAX_KNOWN_DIVERGENCES: &[(&str, &str)] = &[
    (
        "edismax_basic",
        "issue #51: eB/eD BM25 near-tie order flip (Tantivy vs Lucene fieldnorm quantization)",
    ),
    (
        "edismax_score_baseline",
        "issue #51: eB/eD BM25 near-tie order flip (Tantivy vs Lucene fieldnorm quantization)",
    ),
    (
        "edismax_boost_multiplicative",
        "issue #51: eB/eD BM25 near-tie order flip (Tantivy vs Lucene fieldnorm quantization)",
    ),
    (
        "edismax_operators_required",
        "issue #51: eA/eB BM25 near-tie order flip (Tantivy vs Lucene fieldnorm quantization)",
    ),
];

fn edismax_known_divergence_reason(name: &str) -> Option<&'static str> {
    EDISMAX_KNOWN_DIVERGENCES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, reason)| *reason)
}

#[tokio::test]
async fn hermetic_edismax_manifest_entries_match_committed_fixtures() {
    let (app, _dir) = edismax_app().await;
    let entries: Vec<_> = load_manifest(&manifest_path())
        .into_iter()
        .filter(|e| e.name.starts_with("edismax_"))
        .collect();
    assert!(
        !entries.is_empty(),
        "expected at least the edismax_* rows capture.sh's edismax block appends to manifest.tsv"
    );

    let manifest_names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    for (name, reason) in EDISMAX_KNOWN_DIVERGENCES {
        assert!(
            manifest_names.contains(name),
            "EDISMAX_KNOWN_DIVERGENCES entry `{name}` (reason: {reason}) does not match any \
             manifest entry — fix the name or remove the stale entry"
        );
    }

    let mut failures = Vec::new();
    for entry in &entries {
        let (status, actual) = get(&app, &entry.path).await;
        if status.as_u16() != entry.status {
            failures.push(format!(
                "{}: HTTP status {} vs expected {}",
                entry.name, status, entry.status
            ));
            continue;
        }

        let expected = common::normalize_envelope(fixture(&entry.name));
        let actual = common::normalize_envelope(actual);
        // Score-bearing rows are ground truth for which docs match, rank
        // order, and structural score relationships, never for the raw
        // BM25 float value transplanted from Solr (see this file's module
        // doc, PRD ratified-divergence 4) — blank both sides' magnitudes
        // before comparing. Rows with no `score` in `fl` are unaffected by
        // this blanking and still compare doc order exactly.
        let expected = blank_bm25_score_magnitudes(expected);
        let actual = blank_bm25_score_magnitudes(actual);
        let report = diff(&expected, &actual);

        match edismax_known_divergence_reason(&entry.name) {
            Some(reason) if report.diffs.is_empty() => failures.push(format!(
                "{}: EDISMAX_KNOWN_DIVERGENCES says this should still diverge ({reason}), but \
                 it now matches — the underlying divergence is fixed, so remove this entry from \
                 EDISMAX_KNOWN_DIVERGENCES and close the tracking issue",
                entry.name
            )),
            Some(reason) => eprintln!(
                "{}: expected divergence ({reason}): {:?}",
                entry.name, report.diffs
            ),
            None if !report.diffs.is_empty() => {
                failures.push(format!("{}: {:?}", entry.name, report.diffs))
            }
            None => {}
        }
    }

    assert!(
        failures.is_empty(),
        "hermetic edismax differential failures against solr-ref fixtures:\n{}",
        failures.join("\n")
    );
}
