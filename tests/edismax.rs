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

/// Reviewer round-1 must-fix item 2: `parse_edismax_query` used to call
/// `tantivy::query_grammar::parse_query` directly, skipping the same
/// `*:*` → `AllQuery` short-circuit the plain parser applies before ever
/// reaching the grammar. Tantivy 0.26 parses `*:*` as
/// `UserInputLeaf::Exists { field: "*" }`, which fell into this function's
/// per-leaf fallback arm and 400'd as an undefined field (`*` is not a real
/// field) — real Solr returns every doc for `q=*:*` under edismax, same as
/// the lucene parser.
#[tokio::test]
async fn star_colon_star_matches_everything_under_edismax() {
    let (app, _dir) = edismax_app().await;
    let (status, body) = get(&app, "select?q=*:*&defType=edismax&qf=title+body&wt=json").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "q=*:* must not 400 under defType=edismax: {body}"
    );
    assert_eq!(
        body["response"]["numFound"],
        Value::from(10),
        "q=*:* must match every doc in the 10-doc edismax corpus: {body}"
    );
}

/// Reviewer round-1 must-fix item 2 (continued): `parse_edismax_query` also
/// used to skip `rewrite_dynamic_fields`/`rewrite_wildcard_subclause`, the
/// same prologue `parse_query` runs before handing the query string to the
/// grammar — so a `[[dynamic_fields]]` catch-all name in an edismax `q`
/// reached the grammar unrewritten and hit the same "undefined field" 400
/// as `*:*` did above, for the same missing-prologue reason. No Solr fixture
/// pins this (Wayfinder-only regression, same rationale as
/// `tests/query_types.rs`'s dynamic-field regression tests), so this pins
/// only that it must not 400 and must match, not any particular fixture.
const DYNAMIC_EDISMAX_SCHEMA_TOML: &str = r#"
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
type = "text_en"
stored = true

[[dynamic_fields]]
pattern = "*_i"
type = "int"
stored = true
fast = true
"#;

#[tokio::test]
async fn dynamic_field_in_q_is_rewritten_under_edismax() {
    let dir = TempDir::new().expect("temp dir");
    let app = app_with_schema(dir.path(), DYNAMIC_EDISMAX_SCHEMA_TOML).expect("app must build");
    let (status, body) = common::post_docs(
        &app,
        &serde_json::json!([{"id": "d1", "body": "hello world", "count_i": 7}]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "indexing must succeed, got {body}");

    let (status, body) = get(
        &app,
        "select?q=count_i:7&defType=edismax&qf=body&fl=id&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a dynamic-field name in `q` must not 400 under defType=edismax: {body}"
    );
    assert_eq!(
        body["response"]["numFound"],
        Value::from(1),
        "the dynamic-field rewrite must still apply and match doc d1: {body}"
    );
}

// --- qf/pf: dynamic-field names, not just static ones (issue #84) ----------
//
// `resolve_field_weights` (the shared machinery behind both `qf` and `pf`)
// resolves every name with the same static-before-dynamic precedence
// indexing uses: a declared `[[fields]]` entry wins, and a name that only
// matches a `[[dynamic_fields]]` pattern falls back to `match_dynamic` and
// the catch-all container's JSON sub-path (`_dynamic[_text].<name>`) — the
// list-shaped equivalent of what `rewrite_dynamic_fields` does for the `q`
// text path (pinned above by
// `dynamic_field_in_q_is_rewritten_under_edismax`). The names in question
// are exactly the `presets/search-api.toml` `ts_*`/`tm_*` convention
// search_api_solr clients rely on.
//
// Before issue #84 that fallback did not exist: the lookup was a literal
// `wf_schema.field(&name)`, so a dynamic-only name was dropped from the
// disjunction outright. The two failure modes that dropping produced are
// what the tests below pin against regression — a `qf` naming *only*
// dynamic fields resolved to an empty list and hard-errored as "edismax
// `qf` names no field this core has", while `pf` (which never hard-errors —
// an empty resolution just skips the phrase-boost clause per
// `build_pf_query`'s doc comment) failed silently as a missing boost
// instead.
const DYNAMIC_QF_EDISMAX_SCHEMA_TOML: &str = r#"
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
type = "text_en"
stored = true

[[dynamic_fields]]
pattern = "ts_*"
type = "text_general"
stored = true
"#;

/// A `qf` naming only a dynamic field (`ts_title`, matching the
/// `presets/search-api.toml` `ts_*` pattern) must resolve through the same
/// dynamic-field machinery `q` already gets, not drop to an empty field
/// list. Two docs share unrelated `body` text so only `ts_title` can
/// distinguish a match, so a `qf=ts_title` that resolved to zero fields
/// would 400 before either doc was ever considered — which is exactly what
/// happened before issue #84.
#[tokio::test]
async fn qf_naming_only_a_dynamic_field_matches_instead_of_dropping_it() {
    let dir = TempDir::new().expect("temp dir");
    let app = app_with_schema(dir.path(), DYNAMIC_QF_EDISMAX_SCHEMA_TOML).expect("app must build");
    let (status, body) = common::post_docs(
        &app,
        &serde_json::json!([
            {"id": "d1", "body": "filler unrelated text", "ts_title": "rocket launch success"},
            {"id": "d2", "body": "filler unrelated text", "ts_title": "completely different words"}
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "indexing must succeed, got {body}");

    let (status, body) = get(
        &app,
        "select?q=rocket&defType=edismax&qf=ts_title&fl=id&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a `qf` naming only a dynamic field must not 400 with \"names no field this core \
         has\": {body}"
    );
    assert_eq!(
        body["response"]["numFound"],
        Value::from(1),
        "qf=ts_title must be honored in the disjunction and match only d1: {body}"
    );
    assert_eq!(
        body.pointer("/response/docs/0/id").and_then(Value::as_str),
        Some("d1"),
        "the matching doc must be d1 (whose ts_title carries \"rocket\"): {body}"
    );
}

/// `pf` shares `resolve_field_weights` with `qf` but never hard-errors on an
/// empty resolution (`build_pf_query` just skips the phrase-boost clause) —
/// so a `pf` naming only a dynamic field would fail *silently* if it ever
/// stopped resolving: the request would still succeed, only the phrase
/// boost would go missing. That is why this asserts on scores rather than
/// on status, and why a 200 alone proves nothing here. `qf=body` resolves
/// fine (a real static field) so the main query and the request itself
/// succeed either way; `body` is identical bag-of-words text for both docs,
/// so only `pf`'s adjacency-sensitive boost over the *dynamic* `ts_phrase`
/// field can tell them apart.
#[tokio::test]
async fn pf_naming_only_a_dynamic_field_still_boosts_the_adjacent_match() {
    let dir = TempDir::new().expect("temp dir");
    let app = app_with_schema(dir.path(), DYNAMIC_QF_EDISMAX_SCHEMA_TOML).expect("app must build");
    let (status, body) = common::post_docs(
        &app,
        &serde_json::json!([
            {"id": "adjA", "body": "quick fox", "ts_phrase": "quick fox"},
            {"id": "adjB", "body": "quick fox", "ts_phrase": "fox is quick"}
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "indexing must succeed, got {body}");

    let (status_off, body_off) = get(
        &app,
        "select?q=quick+fox&defType=edismax&qf=body&fl=id,score&wt=json",
    )
    .await;
    assert_eq!(status_off, StatusCode::OK, "pf-off request: {body_off}");
    let scores_off = scores_by_id(&body_off);
    assert_eq!(
        scores_off.len(),
        2,
        "both adjA and adjB must match on bag-of-words `body` terms alone, got {scores_off:?}"
    );
    assert!(
        (scores_off["adjA"] - scores_off["adjB"]).abs() <= score_tolerance(),
        "with no `pf`, adjA and adjB must score equally (identical `body` text): {scores_off:?}"
    );

    let (status_on, body_on) = get(
        &app,
        "select?q=quick+fox&defType=edismax&qf=body&pf=ts_phrase&fl=id,score&wt=json",
    )
    .await;
    assert_eq!(status_on, StatusCode::OK, "pf-on request: {body_on}");
    let scores_on = scores_by_id(&body_on);
    let adj_a = scores_on["adjA"];
    let adj_b = scores_on["adjB"];
    assert!(
        adj_a > adj_b + score_tolerance(),
        "pf=ts_phrase (a dynamic-only field name) must still boost adjA (adjacent phrase \
         \"quick fox\" in ts_phrase) above adjB (non-adjacent \"fox is quick\"), got \
         adjA={adj_a}, adjB={adj_b}"
    );
}

// --- pf phrase building over a negated clause (issue #114) -----------------
//
// Issue #114 assumed (its own wording: "presumably") that `literal_texts`
// (the text `pf`'s phrase is built from) should exclude a negated (`-term`)
// clause in `q`, since a term the user explicitly excluded "shouldn't also
// be phrase-boosted". A real-Solr capture (one-off container, same
// title/body schema as this file's `EDISMAX_SCHEMA_TOML`, docs `nA`="rocket
// launch success"/`nB`="launch rocket success" — same adjacent-vs-not-
// adjacent trick as `pA`/`pB` above) disproves that assumption: adding a
// negated clause for a term absent from every doc (`-zzznonexistent`) does
// not leave `pf`'s boost intact over the remaining positive terms — it
// makes the boost vanish completely. `edismax_pf_negation_isolated.json`
// (no negation) scores nA=2.03014/nB=1.01507 (pf boosts nA, the adjacent
// match); `edismax_pf_negation_with_absent_negated_term.json` (same query
// plus `-zzznonexistent`) scores nA=1.01507/nB=1.01507 — identical to the
// unboosted score nB already carries in the isolated capture, i.e. the
// boost vanished entirely. Consistent with real Solr's own `pf` also
// folding the negated term's text into the phrase it builds, and a phrase
// containing a term that can never appear in any matching doc can never
// match, silently dropping the boost. This is exactly what Wayfinder's `literal_texts`
// (not filtering by `Occur`) already does today — so there is **no
// divergence and no bug here**; issue #114 is closed as a corrected premise,
// not a fix, per this test locking in the confirmed-matching behavior.
#[tokio::test]
async fn pf_phrase_over_a_negated_absent_term_loses_its_boost_matching_solr() {
    let (app, _dir) = edismax_app().await;
    let (status, body) = common::post_docs(
        &app,
        &serde_json::json!([
            {"id": "nA", "title": "filler", "body": "rocket launch success"},
            {"id": "nB", "title": "filler", "body": "launch rocket success"}
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "indexing must succeed, got {body}");

    // Fixtures carry raw Solr BM25 magnitudes and are ground truth only for
    // which docs match / structural score relationships, never the exact
    // float (module doc, PRD ratified-divergence 4) -- so this compares
    // Wayfinder's own two live responses structurally, using the fixtures
    // only in the doc comment above to state what real Solr showed.
    let (status_no_neg, body_no_neg) = get(
        &app,
        "select?q=rocket+launch&defType=edismax&qf=body&pf=body&fl=id,score&fq=id:(nA+OR+nB)&wt=json",
    )
    .await;
    assert_eq!(
        status_no_neg,
        StatusCode::OK,
        "no-negation request: {body_no_neg}"
    );
    let scores_no_neg = scores_by_id(&body_no_neg);
    let na_no_neg = scores_no_neg["nA"];
    let nb_no_neg = scores_no_neg["nB"];
    assert!(
        na_no_neg > nb_no_neg + score_tolerance(),
        "without negation, pf=body must boost nA (adjacent \"rocket launch\") above nB \
         (non-adjacent \"launch rocket\"): {scores_no_neg:?}"
    );

    let (status_neg, body_neg) = get(
        &app,
        "select?q=rocket+launch+-zzznonexistent&defType=edismax&qf=body&pf=body&fl=id,score&fq=id:(nA+OR+nB)&wt=json",
    )
    .await;
    assert_eq!(
        status_neg,
        StatusCode::OK,
        "with-negation request: {body_neg}"
    );
    let scores_neg = scores_by_id(&body_neg);
    assert!(
        (scores_neg["nA"] - scores_neg["nB"]).abs() <= score_tolerance(),
        "once a negated clause is present, pf's boost must vanish entirely (nA and nB score \
         equally, matching real Solr's own behavior), got {scores_neg:?}"
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
        "Solr-compatible text_en stopword removal must leave pA and pB with equal scores: \
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

// --- in-query term boost: `q=rocket^5` (issue #109) ------------------------

#[tokio::test]
async fn in_query_term_boost_multiplies_that_terms_score_contribution() {
    let (app, _dir) = edismax_app().await;
    let (_, base) = get(
        &app,
        "select?q=rocket&defType=edismax&qf=title+body&fl=id,score&fq=id:(eA+OR+eB+OR+eC+OR+eD)&wt=json",
    )
    .await;
    let (_, boosted) = get(
        &app,
        "select?q=rocket^5&defType=edismax&qf=title+body&fl=id,score&fq=id:(eA+OR+eB+OR+eC+OR+eD)&wt=json",
    )
    .await;
    let base_scores = scores_by_id(&base);
    let boosted_scores = scores_by_id(&boosted);
    for (id, base_score) in &base_scores {
        let boosted_score = boosted_scores
            .get(id)
            .unwrap_or_else(|| panic!("q=rocket^5 response missing doc {id}: {boosted_scores:?}"));
        assert!(
            (boosted_score - 5.0 * base_score).abs() <= score_tolerance(),
            "q=rocket^5 must exactly multiply this term's own score contribution by 5, same as \
             real Solr (captured fixture `edismax_term_boost`): doc {id}, base={base_score}, \
             boosted={boosted_score}, expected ~{}",
            5.0 * base_score
        );
    }
}

#[tokio::test]
async fn in_query_term_boost_scopes_to_its_own_leaf_not_the_whole_query() {
    let (app, _dir) = edismax_app().await;
    let (_, base) = get(
        &app,
        "select?q=rocket+mission&defType=edismax&qf=title+body&fl=id,score&fq=id:(eA+OR+eB+OR+eC+OR+eD)&wt=json",
    )
    .await;
    let (_, boosted) = get(
        &app,
        "select?q=rocket^5+mission&defType=edismax&qf=title+body&fl=id,score&fq=id:(eA+OR+eB+OR+eC+OR+eD)&wt=json",
    )
    .await;
    let base_scores = scores_by_id(&base);
    let boosted_scores = scores_by_id(&boosted);
    assert!(
        (boosted_scores["eB"] - 5.0 * base_scores["eB"]).abs() <= score_tolerance(),
        "eB matches only \"rocket\" (never \"mission\"), so `rocket^5 mission` must scale its \
         score by exactly 5, same as the single-term case: base={}, boosted={}, expected ~{}",
        base_scores["eB"],
        boosted_scores["eB"],
        5.0 * base_scores["eB"]
    );
    assert!(
        boosted_scores["eC"] < 5.0 * base_scores["eC"],
        "eC matches both \"rocket\" and \"mission\"; boosting only the \"rocket\" leaf must \
         leave the \"mission\" leaf's contribution unscaled, so the total must land strictly \
         below a naive whole-query 5x (which would prove the boost leaked past its own leaf): \
         base={}, boosted={}, naive 5x would be {}",
        base_scores["eC"],
        boosted_scores["eC"],
        5.0 * base_scores["eC"]
    );
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

#[tokio::test]
async fn mm_present_but_empty_400s_like_a_malformed_spec() {
    // Issue #113's own stated premise is WRONG: it claims real Solr ignores
    // an empty `mm` and falls back to its normal OR default, same as `mm`
    // being absent entirely. Confirmed against real Solr (one-off
    // container, same schema/corpus as this file's other `mm_*` tests --
    // `docs/solr-ref-findings.md`) that `mm=` (present, but empty) 400s with
    // a `NumberFormatException`, same as any other malformed `mm` spec --
    // it does NOT silently fall back to anything. `mm` entirely *absent* is
    // a different case (see `mm_absent_still_uses_normal_or_default` below)
    // and must NOT change.
    //
    // Wayfinder's current (pre-fix) behavior: `edismax::min_should_match`
    // treats an empty spec as "require every clause" (`clause_count`), which
    // is silently wrong in the opposite direction from what the issue
    // assumed -- real Solr doesn't pick either interpretation, it rejects
    // the request outright.
    //
    // Per `tests/error_shapes.rs`'s documented narrow contract (same pattern
    // as issue #111's `qf_naming_one_undefined_field_among_valid_ones_400s`
    // above), only the 400 status and standard error envelope shape are
    // asserted -- `error.msg` is free text (Solr's is a Java exception
    // string; Wayfinder's is not) and is never compared verbatim.
    let (app, _dir) = edismax_app().await;
    let (status, body) = get(
        &app,
        "select?q=alpha+beta+gamma&defType=edismax&qf=body&mm=&fl=id&wt=json",
    )
    .await;
    let expected = fixture("edismax_mm_empty_string");
    let want_code = expected["error"]["code"]
        .as_i64()
        .expect("fixture has error.code");

    assert_eq!(
        status.as_u16() as i64,
        want_code,
        "mm= (present but empty) must 400 like any other malformed mm spec, not silently \
         fall back to an interpretation: {body}"
    );
    assert_eq!(body["error"]["code"].as_i64(), Some(want_code));
    assert!(
        body["error"]["msg"].as_str().is_some_and(|s| !s.is_empty()),
        "error.msg must be present and non-empty (never compared verbatim): {body}"
    );
    let metadata = body["error"]["metadata"]
        .as_array()
        .expect("error.metadata must be a flat array");
    assert!(
        metadata.iter().any(|v| v == "error-class")
            && metadata.iter().any(|v| v == "root-error-class"),
        "error.metadata must carry the same key shape as Solr's (values not compared): {body}"
    );
}

#[tokio::test]
async fn mm_absent_still_uses_normal_or_default() {
    // Characterization test, not a regression this fix touches: `mm`
    // entirely absent (no `mm=` param at all, as opposed to
    // `mm_present_but_empty_400s_like_a_malformed_spec`'s `mm=`) must keep
    // falling back to the normal OR default. Real Solr confirms this is
    // already correct (`edismax_mm_absent` fixture) -- mmA/mmB/mmC each
    // match at least one of "alpha beta gamma"'s three optional clauses,
    // mmD matches zero and stays excluded.
    let (app, _dir) = edismax_app().await;
    let (status, body) = get(
        &app,
        "select?q=alpha+beta+gamma&defType=edismax&qf=body&fl=id&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_fixture(body, "edismax_mm_absent");
}

#[tokio::test]
async fn empty_mm_alongside_a_single_clause_q_does_not_400() {
    // The other half of issue #113's correction, added by the implementor
    // after review round 1 (finding 85). Real Solr does not validate `mm`
    // eagerly as a request param -- it only *parses* the spec when it has a
    // multi-clause boolean query to apply it to, so the identical `mm=` that
    // 400s above (`q=alpha beta gamma`, three clauses) 200s when `q` yields
    // fewer than two clauses. Captured one-off against real Solr on the same
    // schema/corpus as this file's other `mm_*` tests (not re-run through
    // `capture.sh`; no fixture committed, same as finding 82's confirming
    // capture): `q=*:*` -> 200/numFound 10, `q=alpha` -> 200/numFound 3,
    // `q=-mission` -> 200/numFound 8, `q="alpha beta"` and `q=title:rocket`
    // -> 200. Multi-clause 400s regardless of occur kind: `q=alpha beta`,
    // `q=+alpha +beta`, `q=alpha -mission`.
    //
    // Only status and `numFound` are asserted: those are the captured facts
    // this guards (was the request rejected, and did the empty `mm` change
    // which docs matched), and the corpus here is the captured corpus.
    // `q=-mission` is the case a naive placement gets wrong -- Wayfinder
    // appends its own `AllQuery` `Should` clause to an all-`MustNot` query,
    // so a clause-count check made after that augmentation would 400 a
    // request real Solr answers 200.
    let (app, _dir) = edismax_app().await;
    for (query, want_num_found) in [
        ("select?q=*:*&defType=edismax&qf=body&mm=&fl=id&wt=json", 10),
        (
            "select?q=alpha&defType=edismax&qf=body&mm=&fl=id&wt=json",
            3,
        ),
        (
            "select?q=-mission&defType=edismax&qf=title&mm=&fl=id&wt=json",
            8,
        ),
    ] {
        let (status, body) = get(&app, query).await;
        assert_eq!(status, StatusCode::OK, "{query} must not 400: {body}");
        assert_eq!(
            body["response"]["numFound"].as_i64(),
            Some(want_num_found),
            "{query} must match the captured Solr result count: {body}"
        );
    }
}

#[tokio::test]
async fn empty_mm_alongside_star_all_matches_committed_fixture() {
    // Reviewer round-2 follow-up (issue #113): the round above only asserted
    // `numFound` against prose (finding 85 / this file's own comments), not a
    // committed fixture -- the same class of gap that let round 1's bad
    // placement (guard before the `*:*` short-circuit) hide from a green
    // suite. `edismax_mm_empty_star` is a genuine `manifest.tsv` row (see
    // `hermetic_edismax_manifest_entries_match_committed_fixtures` below,
    // which sweeps it too), so this is redundant with that sweep by design --
    // it exists as an explicit, named assertion for this specific boundary
    // point rather than relying solely on the generic manifest loop.
    //
    // Caveat, stated here rather than left implicit: this fixture's
    // `numFound` (10) is corroborated by the real one-off Solr capture this
    // test's sibling above cites; the doc order/id list was reconstructed
    // from Wayfinder's own hermetic output (see `solr-ref/capture.sh`'s
    // comment on this `cape` line) because no live Solr container was
    // available to re-capture it independently. It is not yet fully
    // real-Solr-verified evidence -- re-running `capture.sh`'s edismax block
    // against a live container would close that gap.
    let (app, _dir) = edismax_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&defType=edismax&qf=body&mm=&fl=id&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_matches_fixture(body, "edismax_mm_empty_star");
}

#[tokio::test]
async fn empty_mm_400s_for_every_multi_clause_shape_regardless_of_occur() {
    // Companion to the test above, same capture (finding 85): what makes an
    // empty `mm` reachable is the clause *count*, not whether the clauses are
    // optional. All three of these 400 in real Solr.
    let (app, _dir) = edismax_app().await;
    for query in [
        "select?q=alpha+beta&defType=edismax&qf=body&fl=id&mm=&wt=json",
        "select?q=%2Balpha+%2Bbeta&defType=edismax&qf=body&fl=id&mm=&wt=json",
        "select?q=alpha+-mission&defType=edismax&qf=body&fl=id&mm=&wt=json",
    ] {
        let (status, body) = get(&app, query).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{query} must 400 like real Solr: {body}"
        );
    }
}

#[tokio::test]
async fn whitespace_only_mm_400s_like_an_empty_one() {
    // `mm=%20` is the adjacent shape review round 1 asked about: real Solr
    // 400s on it with the same `NumberFormatException` as `mm=` (finding 85),
    // which is why the guard trims before testing for emptiness rather than
    // checking `is_empty()` on the raw value.
    let (app, _dir) = edismax_app().await;
    let (status, body) = get(
        &app,
        "select?q=alpha+beta+gamma&defType=edismax&qf=body&mm=%20&fl=id&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
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

#[tokio::test]
async fn non_numeric_boost_is_ignored_like_any_unsupported_function_query_not_rejected() {
    // Real Solr's `boost` is always a function query (`boost=2` just happens
    // to be its simplest constant form); Wayfinder has no function-query
    // evaluator (PRD v1 scope, same exclusion as `bf` -- issue #108/finding
    // 75), so a non-numeric `boost` value must fail to parse and be ignored,
    // not 400 (issue #110): same "unsupported param is silently a no-op"
    // contract as `bf` above, applied to `boost` instead of `bf`.
    let (app, _dir) = edismax_app().await;
    let (status_without, without) = get(
        &app,
        "select?q=rocket&defType=edismax&qf=title+body&fl=id,score&fq=id:(eA+OR+eB+OR+eC+OR+eD)&wt=json",
    )
    .await;
    let (status_with, with) = get(
        &app,
        "select?q=rocket&defType=edismax&qf=title+body&boost=recip(rord(id),1,2,3)&fl=id,score&fq=id:(eA+OR+eB+OR+eC+OR+eD)&wt=json",
    )
    .await;
    assert_eq!(status_without, StatusCode::OK);
    assert_eq!(
        status_with,
        StatusCode::OK,
        "a non-numeric `boost` must not 400, same as any other unsupported param (finding 8)"
    );
    let scores_without = scores_by_id(&without);
    let scores_with = scores_by_id(&with);
    assert_eq!(
        scores_without.len(),
        scores_with.len(),
        "an ignored function-query `boost` must not change which docs match"
    );
    for (id, score_without) in &scores_without {
        let score_with = scores_with.get(id).unwrap_or_else(|| {
            panic!("doc {id} missing once function-query `boost` is added: {scores_with:?}")
        });
        assert!(
            (score_with - score_without).abs() <= score_tolerance(),
            "an ignored function-query `boost` must not change doc {id}'s score: \
             without={score_without}, with={score_with}"
        );
    }
}

// --- qf: one undefined field among otherwise-valid ones still 400s (#111) --

#[tokio::test]
async fn qf_naming_one_undefined_field_among_valid_ones_400s() {
    // Captured Solr fact (`edismax_qf_partial_invalid` fixture): a `qf`
    // naming a mix of valid and undefined fields 400s on the undefined name
    // alone, even though `title` in the same `qf` is perfectly valid --
    // unlike a `qf` that names *only* undefined fields (already covered by
    // the pre-existing "names no field this core has" empty-resolution
    // path). Before issue #111, `resolve_field_weights`'s drop-unknown
    // filtering silently dropped `nosuchfield` and 200d using `title` alone,
    // which is the wrong-answer bug this test pins against regression.
    //
    // Per `tests/error_shapes.rs`'s documented narrow contract, only the 400
    // status and standard error envelope shape are asserted -- `error.msg`
    // is free text (Solr's is a Java exception string; Wayfinder's is not)
    // and is never compared verbatim.
    let (app, _dir) = edismax_app().await;
    let (status, body) = get(
        &app,
        "select?q=rocket&defType=edismax&qf=title+nosuchfield&fl=id&wt=json",
    )
    .await;
    let expected = fixture("edismax_qf_partial_invalid");
    let want_code = expected["error"]["code"]
        .as_i64()
        .expect("fixture has error.code");

    assert_eq!(
        status.as_u16() as i64,
        want_code,
        "qf naming one undefined field among valid ones must 400: {body}"
    );
    assert_eq!(body["error"]["code"].as_i64(), Some(want_code));
    assert!(
        body["error"]["msg"].as_str().is_some_and(|s| !s.is_empty()),
        "error.msg must be present and non-empty (never compared verbatim): {body}"
    );
    let metadata = body["error"]["metadata"]
        .as_array()
        .expect("error.metadata must be a flat array");
    assert!(
        metadata.iter().any(|v| v == "error-class")
            && metadata.iter().any(|v| v == "root-error-class"),
        "error.metadata must carry the same key shape as Solr's (values not compared): {body}"
    );
}

#[tokio::test]
async fn star_query_with_undefined_qf_field_still_400s() {
    // Captured Solr fact (`edismax_qf_star_unknown` fixture): real Solr
    // validates `qf` before ever looking at whether `q` is `*:*` -- an
    // undefined `qf` field 400s regardless of the query shape. Issue #112:
    // Wayfinder's `q.trim() == "*:*"` short-circuit in
    // `parse_edismax_query` returns `AllQuery` before the `qf`
    // field-validation loop (added for issue #111) ever runs, so today this
    // incorrectly 200s with all docs instead of 400ing.
    //
    // Per `tests/error_shapes.rs`'s documented narrow contract, only the 400
    // status and standard error envelope shape are asserted -- `error.msg`
    // is free text (Solr's is a Java exception string; Wayfinder's is not)
    // and is never compared verbatim.
    let (app, _dir) = edismax_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&defType=edismax&qf=nosuchfield&fl=id&wt=json",
    )
    .await;
    let expected = fixture("edismax_qf_star_unknown");
    let want_code = expected["error"]["code"]
        .as_i64()
        .expect("fixture has error.code");

    assert_eq!(
        status.as_u16() as i64,
        want_code,
        "q=*:* with an undefined qf field must 400, not short-circuit to AllQuery: {body}"
    );
    assert_eq!(body["error"]["code"].as_i64(), Some(want_code));
    assert!(
        body["error"]["msg"].as_str().is_some_and(|s| !s.is_empty()),
        "error.msg must be present and non-empty (never compared verbatim): {body}"
    );
    let metadata = body["error"]["metadata"]
        .as_array()
        .expect("error.metadata must be a flat array");
    assert!(
        metadata.iter().any(|v| v == "error-class")
            && metadata.iter().any(|v| v == "root-error-class"),
        "error.metadata must carry the same key shape as Solr's (values not compared): {body}"
    );
}

#[tokio::test]
async fn star_query_with_partially_invalid_qf_still_400s() {
    // Captured Solr fact (`edismax_qf_star_partial_invalid` fixture): same
    // `q=*:*` short-circuit bug (issue #112) as the test above, but for the
    // partially-valid `qf` shape issue #111 fixed for non-`*:*` queries --
    // `qf=title+nosuchfield` still 400s on the undefined name alone, even
    // with `q=*:*` and even though `title` in the same `qf` is valid.
    //
    // Per `tests/error_shapes.rs`'s documented narrow contract, only the 400
    // status and standard error envelope shape are asserted -- `error.msg`
    // is free text (Solr's is a Java exception string; Wayfinder's is not)
    // and is never compared verbatim.
    let (app, _dir) = edismax_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&defType=edismax&qf=title+nosuchfield&fl=id&wt=json",
    )
    .await;
    let expected = fixture("edismax_qf_star_partial_invalid");
    let want_code = expected["error"]["code"]
        .as_i64()
        .expect("fixture has error.code");

    assert_eq!(
        status.as_u16() as i64,
        want_code,
        "q=*:* with a partially-invalid qf must still 400 on the undefined name: {body}"
    );
    assert_eq!(body["error"]["code"].as_i64(), Some(want_code));
    assert!(
        body["error"]["msg"].as_str().is_some_and(|s| !s.is_empty()),
        "error.msg must be present and non-empty (never compared verbatim): {body}"
    );
    let metadata = body["error"]["metadata"]
        .as_array()
        .expect("error.metadata must be a flat array");
    assert!(
        metadata.iter().any(|v| v == "error-class")
            && metadata.iter().any(|v| v == "root-error-class"),
        "error.metadata must carry the same key shape as Solr's (values not compared): {body}"
    );
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
        "edismax_mm_empty_string",
        "edismax_mm_absent",
        "edismax_mm_empty_star",
        "edismax_qf_partial_invalid",
        "edismax_pf_negation_isolated",
        "edismax_pf_negation_with_absent_negated_term",
        "edismax_qf_star_unknown",
        "edismax_qf_star_partial_invalid",
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

        if !report.diffs.is_empty() {
            failures.push(format!("{}: {:?}", entry.name, report.diffs));
        }
    }

    assert!(
        failures.is_empty(),
        "hermetic edismax differential failures against solr-ref fixtures:\n{}",
        failures.join("\n")
    );
}
