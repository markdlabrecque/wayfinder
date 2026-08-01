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
    // after review round 1 (finding 89). Real Solr does not validate `mm`
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
    // `numFound` against prose (finding 89 / this file's own comments), not a
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
    // Companion to the test above, same capture (finding 89): what makes an
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
    // 400s on it with the same `NumberFormatException` as `mm=` (finding 89),
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

// --- issue #147: the two facts that rest on inference, not capture ----------
//
// `build_field_disjunction` (#137) makes an unquoted multi-token clause a
// boolean OR rather than a `PhraseQuery`, and `local_params::bound_token_len`
// (#137) binds an inline `{!edismax}` to the next run only. Neither is backed
// by a fixture today: the first rests on Solr's *documented*
// `autoGeneratePhraseQueries` default (finding 92, explicitly flagged
// documentation-derived), the second on consistency with seven captured
// `numFound` values rather than on Solr's own parse tree (findings 90/91).
// CLAUDE.md's compatibility contract says fixtures are ground truth and
// expected values never come from what the implementation happens to produce,
// so both gaps are contract violations with an issue attached.
//
// The two tests below are the fixture-derived assertions. They are red until
// issue #147's captures land, and they read every expected value out of the
// fixture -- no number in this section was written by looking at Wayfinder.

/// Fixture answering "does an unquoted multi-token edismax clause build a
/// phrase or an OR?". `q=quick%2Brocket` is *one* clause (`+` is an ordinary
/// term character mid-token in Lucene's `_TERM_CHAR` set) whose `text_en`
/// analysis yields two tokens, so Solr's answer separates the two readings:
/// a phrase can only match a document with "quick rocket" adjacent, an OR
/// matches every document carrying either token.
const UNQUOTED_MULTITOKEN_FIXTURE: &str = "edismax_unquoted_multitoken";

/// The request the fixture above must be captured from. Deliberately
/// `sort=id+asc`: without it the response order is BM25 order, which diverges
/// between Tantivy and Solr by a permanently-ratified margin (PRD
/// ratified-divergence 4), and this fixture is about *which* documents match,
/// not their ranking. Also what keeps it safe as a `manifest.tsv` row for
/// `hermetic_edismax_manifest_entries_match_committed_fixtures`, which compares
/// document order exactly for a row carrying no `score`.
const UNQUOTED_MULTITOKEN_PATH: &str =
    "select?q=quick%2Brocket&defType=edismax&qf=title+body&fl=id&sort=id+asc&wt=json";

/// Fixture recording Solr's own parse tree for the Shape-B inline nested query
/// (`solr-ref/search-api/trace/00003.json`'s shape), captured with
/// `debugQuery=true`. `qf` names `title`/`body` while `df` names `id`, exactly
/// the split the captured `/select` handler defaults have
/// (`solr-ref/search-api/configset/solrconfig_extra.xml:110-118`: `df=id`), so
/// the parsed query says out loud which clause the `+` bound to: under
/// "bind the next run" only `"quick"` reaches the `qf` fan-out and `"rocket"`
/// is resolved by the outer lucene parser against `df`; under any
/// "bind the whole remainder" reading `"rocket"` fans out over `qf` too and
/// never touches `df`.
const SHAPE_B_DEBUG_FIXTURE: &str = "edismax_shape_b_debug_parsedquery";

/// The request `SHAPE_B_DEBUG_FIXTURE` must be captured from. Not a
/// `manifest.tsv` row: Wayfinder implements no `debug` section, so the
/// whole-body sweep would compare a `debug` key that cannot exist. Same
/// deliberate exclusion as `edismax_qf_partial_invalid` (issue #111) -- the
/// exact command belongs in `capture.sh` as a comment instead.
const SHAPE_B_DEBUG_PATH: &str = "select?q=(%7B!edismax+qf%3D%27title+body%27%7D%2B%22quick%22+%2B%22rocket%22)&df=id&debugQuery=true&fl=id&sort=id+asc&wt=json";

/// The decoded `q` of `SHAPE_B_DEBUG_PATH`, i.e. what `debug.rawquerystring`
/// must echo back. Checking it is what makes the fixture self-identifying:
/// a `parsedquery` is only evidence about the binding rule if it is the parse
/// of *this* query.
const SHAPE_B_DEBUG_Q: &str = "({!edismax qf='title body'}+\"quick\" +\"rocket\")";

/// Second `debugQuery=true` Shape-B fixture, for the *other* terminator in the
/// rule `local_params::bound_token_len` implements.
///
/// `SHAPE_B_DEBUG_FIXTURE` above only ever evidences the **whitespace**
/// terminator: in trace 00003's shape the bound run `+"quick"` ends at a space.
/// Finding 91 claims a second terminator — a `)` at run-local paren depth 0 —
/// and that half is what trace 00006's shape (`({!edismax ...}+"quick")`, the
/// whole `q` parenthesised with no whitespace after the run at all) exercises.
/// Without this capture the `)` half of findings 90/91 stays inferred from
/// `numFound` alone, which is the thing issue #147 exists to stop.
const SHAPE_B_DEBUG_PAREN_FIXTURE: &str = "edismax_shape_b_debug_parsedquery_paren_terminated";

/// The request `SHAPE_B_DEBUG_PAREN_FIXTURE` must be captured from. Excluded
/// from `manifest.tsv` for the same reason as `SHAPE_B_DEBUG_PATH`.
const SHAPE_B_DEBUG_PAREN_PATH: &str = "select?q=(%7B!edismax+qf%3D%27title+body%27%7D%2B%22quick%22)&df=id&debugQuery=true&fl=id&sort=id+asc&wt=json";

/// The decoded `q` of `SHAPE_B_DEBUG_PAREN_PATH`.
const SHAPE_B_DEBUG_PAREN_Q: &str = "({!edismax qf='title body'}+\"quick\")";

/// `UNQUOTED_MULTITOKEN_FIXTURE`'s request again, captured with
/// `debugQuery=true`.
///
/// The `numFound` capture settles phrase-vs-OR, but it takes the step *before*
/// that on trust: that `quick+rocket` is **one** clause whose analysis yields
/// two tokens (`+` being an ordinary term character mid-token in Lucene's
/// `_TERM_CHAR` set) rather than two clauses. That step is what generalises the
/// result to issue #137's actual `state-of-the-art` case, and reading the
/// grammar is exactly the kind of inference issue #147 exists to replace with a
/// capture. Solr's own parse tree discriminates the two directly -- see
/// `unquoted_multitoken_debug_parsedquery_shows_one_clause_over_both_tokens`.
const UNQUOTED_MULTITOKEN_DEBUG_FIXTURE: &str = "edismax_unquoted_multitoken_debug";

/// The request `UNQUOTED_MULTITOKEN_DEBUG_FIXTURE` must be captured from:
/// `UNQUOTED_MULTITOKEN_PATH` plus `debugQuery=true`. Not a `manifest.tsv` row,
/// for the same reason as `SHAPE_B_DEBUG_PATH` -- Wayfinder implements no
/// `debug` section.
const UNQUOTED_MULTITOKEN_DEBUG_PATH: &str = "select?q=quick%2Brocket&defType=edismax&qf=title+body&debugQuery=true&fl=id&sort=id+asc&wt=json";

/// Issue #197's direct evidence that `-`, like #147's captured `+`, is an
/// ordinary term character mid-token.
const MIDTOKEN_MINUS_DEBUG_FIXTURE: &str = "edismax_midtoken_minus_debug";
const MIDTOKEN_MINUS_DEBUG_PATH: &str = "select?q=state-of-the-art&defType=edismax&qf=title+body&debugQuery=true&fl=id&sort=id+asc&wt=json";

/// Issue #197's nested-paren Shape-B request. If whitespace terminates only at
/// depth zero, the whole balanced expression reaches edismax and parses; Solr's
/// 400 proves the depth-one whitespace cut leaves unbalanced outer text.
const SHAPE_B_NESTED_PAREN_DEBUG_FIXTURE: &str = "edismax_shape_b_debug_nested_paren";
const SHAPE_B_NESTED_PAREN_DEBUG_PATH: &str = "select?q=(%7B!edismax+qf%3D%27title+body%27%7D(%2B%22quick%22+%2B%22fox%22))&df=id&debugQuery=true&fl=id&sort=id+asc&wt=json";
const SHAPE_B_NESTED_PAREN_DEBUG_Q: &str = "({!edismax qf='title body'}(+\"quick\" +\"fox\"))";

fn fixture_file(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("solr-ref/responses")
        .join(format!("{name}.json"))
}

/// Loads a fixture that issue #147 must capture, failing with the exact
/// command to run rather than a bare "No such file".
fn require_capture(name: &str, path_and_query: &str) -> Value {
    let file = fixture_file(name);
    let raw = std::fs::read_to_string(&file).unwrap_or_else(|e| {
        panic!(
            "issue #147's capture is missing: {file} ({e}).\n\
             Capture it against a real `solr:9` running `solr-ref/capture.sh`'s edismax block \
             schema and 10-doc corpus (container `wayfinder-solr-7`, port 8994, core `content`, \
             fields `title`/`body`), append the block at the END of capture.sh per CLAUDE.md, and \
             do NOT re-run capture.sh wholesale:\n\
             \n  \
             curl -sg 'http://localhost:8994/solr/content/{path_and_query}' \\\n    \
             -o solr-ref/responses/{name}.json\n",
            file = file.display(),
        )
    });
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse fixture {}: {e}", file.display()))
}

fn num_found(envelope: &Value) -> u64 {
    envelope
        .pointer("/response/numFound")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("response.numFound must be a number in {envelope}"))
}

/// `numFound` and the matching document ids for an unquoted multi-token
/// edismax clause come from Solr, not from Wayfinder.
///
/// Red until `solr-ref/responses/edismax_unquoted_multitoken.json` exists. If
/// Solr's answer turns out to be the *phrase* reading, this test fails with a
/// real divergence: per CLAUDE.md that is a bug in `build_field_disjunction`
/// to fix, not an assertion to relax or a fixture to normalise.
#[tokio::test]
async fn unquoted_multitoken_clause_matches_committed_capture() {
    // The capture only separates phrase from OR if the corpus it was taken
    // against has the two tokens present but never adjacent. Assert that on
    // the transcribed corpus first, so a future corpus edit cannot quietly
    // turn this fixture into one that both readings satisfy.
    let corpus = edismax_corpus();
    let docs = corpus.as_array().expect("corpus is an array");
    let text = |doc: &Value, field: &str| {
        doc.get(field)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase()
    };
    let mut with_quick = 0usize;
    let mut with_rocket = 0usize;
    for doc in docs {
        for field in ["title", "body"] {
            let value = text(doc, field);
            assert!(
                !value.contains("quick rocket"),
                "`{field}` of {doc} has \"quick\" and \"rocket\" adjacent, so a `PhraseQuery` and \
                 a boolean OR would both match it and the capture stops discriminating"
            );
            if value
                .split_whitespace()
                .any(|w| w.trim_matches('.') == "quick")
            {
                with_quick += 1;
            }
            if value
                .split_whitespace()
                .any(|w| w.trim_matches('.') == "rocket")
            {
                with_rocket += 1;
            }
        }
    }
    assert!(
        with_quick > 0 && with_rocket > 0,
        "the corpus must carry both tokens for the capture to discriminate; saw quick in \
         {with_quick} field(s), rocket in {with_rocket}"
    );

    // The request replayed against Wayfinder is the one `manifest.tsv` records
    // as captured, not a string retyped here -- if the two disagree the
    // fixture is not evidence about this request at all.
    let row = load_manifest(&manifest_path())
        .into_iter()
        .find(|e| e.name == UNQUOTED_MULTITOKEN_FIXTURE)
        .unwrap_or_else(|| {
            panic!(
                "solr-ref/manifest.tsv has no `{UNQUOTED_MULTITOKEN_FIXTURE}` row. Issue #147 owns \
                 capture.sh/manifest.tsv: append\n  \
                 cape {UNQUOTED_MULTITOKEN_FIXTURE} '{UNQUOTED_MULTITOKEN_PATH}'\n\
                 at the END of capture.sh's edismax section so the capture is reproducible and \
                 swept by hermetic_edismax_manifest_entries_match_committed_fixtures."
            )
        });
    assert_eq!(
        row.path, UNQUOTED_MULTITOKEN_PATH,
        "the captured request must be the unquoted-multi-token one this test reasons about \
         (a literal `+` mid-token, `%2B`, not `+`-as-space)"
    );
    assert_eq!(row.status, 200, "real Solr answers this request 200");

    let expected = require_capture(UNQUOTED_MULTITOKEN_FIXTURE, &row.path);
    let (app, _dir) = edismax_app().await;
    let (status, actual) = get(&app, &row.path).await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(
        num_found(&actual),
        num_found(&expected),
        "numFound for `{path}` must equal real Solr's. Solr matched {solr} document(s), Wayfinder \
         {wf}. If Solr matched none while Wayfinder matched several, Solr built a `PhraseQuery` \
         for the unquoted multi-token clause and `build_field_disjunction`'s boolean-OR reading \
         (finding 92, documentation-derived) is wrong -- fix the implementation and escalate, do \
         not relax this assertion.\nSolr: {expected}\nWayfinder: {actual}",
        path = row.path,
        solr = num_found(&expected),
        wf = num_found(&actual),
    );
    assert_eq!(
        ordered_ids(&actual),
        ordered_ids(&expected),
        "the matching document ids (in `sort=id asc` order, so BM25 divergence cannot enter) must \
         be exactly Solr's"
    );
}

/// Solr's own parse tree must show `quick+rocket` as **one** clause spanning
/// both analysed tokens, not two clauses -- the `_TERM_CHAR` step that
/// `unquoted_multitoken_clause_matches_committed_capture`'s `numFound` cannot
/// see, and the step that generalises the phrase-vs-OR result to issue #137's
/// actual `state-of-the-art` case.
///
/// The discriminator is structural and sharp: edismax fans **each clause** out
/// over `qf` as its own `DisjunctionMaxQuery`. So one clause carrying two tokens
/// gives exactly one `DisjunctionMaxQuery` with both tokens inside it, while two
/// clauses give two, one per token. Counting them separates the readings without
/// depending on how Lucene prints a disjunction.
///
/// Fixture-only by design: Wayfinder implements no `debug` section, so there is
/// nothing on its side to compare a parse tree against. The behavioural half of
/// this capture is `unquoted_multitoken_clause_matches_committed_capture`, which
/// replays the same request (minus `debugQuery`) against Wayfinder; the
/// `numFound` cross-check below is what pins the two captures to the same
/// request and corpus.
#[tokio::test]
async fn unquoted_multitoken_debug_parsedquery_shows_one_clause_over_both_tokens() {
    let expected = require_capture(
        UNQUOTED_MULTITOKEN_DEBUG_FIXTURE,
        UNQUOTED_MULTITOKEN_DEBUG_PATH,
    );

    let raw = expected
        .pointer("/debug/rawquerystring")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "fixture `{UNQUOTED_MULTITOKEN_DEBUG_FIXTURE}` must be captured with \
                 `debugQuery=true` -- there is no `debug.rawquerystring` in it: {expected}"
            )
        });
    assert_eq!(
        raw, "quick+rocket",
        "the capture must be the parse of the unquoted multi-token query this test reasons about, \
         with a literal `+` mid-token (`%2B`, not `+`-as-space)"
    );

    // Same request, same corpus as the `numFound` capture: if these disagree,
    // one of the two was taken against something else and neither is evidence
    // about the other's claim.
    let plain = require_capture(UNQUOTED_MULTITOKEN_FIXTURE, UNQUOTED_MULTITOKEN_PATH);
    assert_eq!(
        num_found(&expected),
        num_found(&plain),
        "the `debugQuery=true` capture and the `numFound` capture must be the same request against \
         the same corpus.\ndebug: {expected}\nplain: {plain}"
    );

    let parsed = expected
        .pointer("/debug/parsedquery")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "fixture `{UNQUOTED_MULTITOKEN_DEBUG_FIXTURE}` has no `debug.parsedquery`: \
                 {expected}"
            )
        });

    assert_eq!(
        parsed.matches("DisjunctionMaxQuery").count(),
        1,
        "Solr's parsedquery has {n} `DisjunctionMaxQuery` nodes, not 1. edismax fans each clause \
         out over `qf` as its own disjunction, so two of them means Solr read `quick+rocket` as \
         **two** clauses and the `_TERM_CHAR` reading (finding 92: `+` is an ordinary term \
         character mid-token, so this is one clause analysing to two tokens) is wrong. That would \
         make `build_field_disjunction`'s whole clause-splitting account wrong too -- escalate with \
         this fixture rather than relaxing the assertion: {parsed}",
        n = parsed.matches("DisjunctionMaxQuery").count(),
    );

    // ...and that single disjunction carries *both* tokens on each `qf` field,
    // which is what "one clause, two tokens" means concretely.
    for field in ["title", "body"] {
        for token in ["quick", "rocket"] {
            assert!(
                parsed.contains(&format!("{field}:{token}")),
                "`{field}:{token}` is absent from Solr's single `DisjunctionMaxQuery`, so that \
                 disjunction does not span both analysed tokens over both `qf` fields: {parsed}"
            );
        }
    }

    // The OR reading again, this time read off the structure rather than off a
    // count: with `autoGeneratePhraseQueries` on, each field's side of the
    // disjunction would be a `PhraseQuery`, printed `title:"quick rocket"`.
    assert!(
        !parsed.contains("\"quick rocket\""),
        "Solr's parsedquery builds a phrase for the unquoted multi-token clause, so \
         `build_field_disjunction`'s boolean-OR reading is wrong -- fix the implementation and \
         escalate, do not relax this assertion: {parsed}"
    );
}

/// Solr's own `parsedquery` for a Shape-B inline nested query must show the
/// `+` binding only the next run -- the rule `local_params::bound_token_len`
/// implements and findings 90/91 record from `numFound` consistency alone.
///
/// Asserted structurally, not by string equality against a guessed
/// `parsedquery`: which field each token was resolved against is the property
/// that separates "bind the next run" from "bind the whole remainder", and it
/// survives any rendering difference in how Lucene prints a disjunction.
#[tokio::test]
async fn shape_b_debug_parsedquery_shows_the_plus_binding_only_the_next_run() {
    let expected = require_capture(SHAPE_B_DEBUG_FIXTURE, SHAPE_B_DEBUG_PATH);

    let raw = expected
        .pointer("/debug/rawquerystring")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "fixture `{SHAPE_B_DEBUG_FIXTURE}` must be captured with `debugQuery=true` -- \
                 there is no `debug.rawquerystring` in it: {expected}"
            )
        });
    assert_eq!(
        raw, SHAPE_B_DEBUG_Q,
        "the capture must be the parse of the Shape-B query this test reasons about \
         (trace 00003's shape, with `qf` naming fields `df` does not)"
    );

    let parsed = expected
        .pointer("/debug/parsedquery")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("fixture `{SHAPE_B_DEBUG_FIXTURE}` has no `debug.parsedquery`: {expected}")
        });

    // The bound run did reach the edismax nested query: "quick" fanned out
    // over both `qf` fields.
    for field in ["title", "body"] {
        assert!(
            parsed.contains(&format!("{field}:quick")),
            "`{field}:quick` is absent from Solr's parsedquery, so the `+\"quick\"` run never \
             reached the `{{!edismax qf='title body'}}` nested query at all: {parsed}"
        );
    }
    // ...and nothing after that run did: "rocket" never fanned out over `qf`.
    for field in ["title", "body"] {
        assert!(
            !parsed.contains(&format!("{field}:rocket")),
            "`{field}:rocket` is present in Solr's parsedquery, so the inline nested query bound \
             more than the next run -- `local_params::bound_token_len` and findings 90/91 are \
             wrong about the binding rule, and that is an implementation bug to fix (escalate; \
             do not relax this assertion): {parsed}"
        );
    }
    // It was resolved by the outer lucene parser against `df=id` instead,
    // which is what makes real Solr's Shape-B recall as low as it is.
    assert!(
        parsed.contains("id:rocket"),
        "Solr's parsedquery does not resolve `+\"rocket\"` against `df=id`, so finding 90's \
         \"everything after the bound run is parsed by the outer lucene parser against `df`\" is \
         not what Solr did: {parsed}"
    );

    // Behavioural half: the same request against Wayfinder. Weaker than the
    // parsedquery assertions above by construction -- with `df=id` the
    // mandatory `id:"rocket"` clause makes Solr's own answer 0, so a wrong
    // binding rule could coincide here -- but it is what pins that Wayfinder
    // and Solr agree on the *outcome* of the tree above.
    let (app, _dir) = edismax_app().await;
    let (status, actual) = get(&app, SHAPE_B_DEBUG_PATH).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        num_found(&actual),
        num_found(&expected),
        "numFound for the Shape-B query must equal real Solr's.\nSolr: {expected}\nWayfinder: \
         {actual}"
    );
}

/// The other half of the binding rule: a `)` at run-local paren depth 0 ends
/// the bound run too, so the run does not leak past it.
///
/// What a "terminates on whitespace only" implementation would do differently
/// is the whole discriminating power here, and it is sharp: with no whitespace
/// anywhere after `}`, that implementation binds `+"quick")` — the entire
/// remainder, closing paren included. So it hands the nested edismax parser an
/// unbalanced `)` (a syntax error, not a 200), and leaves the outer lucene
/// parser with nothing at all to close the `(` it already opened. Three
/// observables separate the two, all of them read out of the fixture:
///
/// 1. Solr answered **200** with a parse tree at all. Weak on its own, and
///    deliberately not leaned on: edismax has an escape-and-retry fallback on
///    parse failure, so a 200 does not by itself prove nothing went wrong. It is
///    context for 2a/2b/3, which carry the argument regardless.
/// 2. `parsedquery` is exactly the `qf` fan-out over `quick` — the `)` was
///    consumed as the outer paren's closer, so no `df` clause and no stray
///    term came from it.
/// 3. `numFound`/document ids are non-degenerate (unlike
///    `SHAPE_B_DEBUG_FIXTURE`, whose mandatory `id:"rocket"` makes both engines
///    return zero and so cannot tell a right binding rule from a wrong one).
#[tokio::test]
async fn shape_b_debug_parsedquery_shows_a_closing_paren_terminating_the_bound_run() {
    let expected = require_capture(SHAPE_B_DEBUG_PAREN_FIXTURE, SHAPE_B_DEBUG_PAREN_PATH);

    let raw = expected
        .pointer("/debug/rawquerystring")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "fixture `{SHAPE_B_DEBUG_PAREN_FIXTURE}` must be captured with `debugQuery=true` \
                 -- there is no `debug.rawquerystring` in it: {expected}"
            )
        });
    assert_eq!(
        raw, SHAPE_B_DEBUG_PAREN_Q,
        "the capture must be the parse of trace 00006's shape: the whole `q` parenthesised, with \
         the bound run running straight into the closing `)` and no whitespace after `}}` anywhere. \
         A query with whitespace after the run evidences the other terminator, not this one"
    );

    let parsed = expected
        .pointer("/debug/parsedquery")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("fixture `{SHAPE_B_DEBUG_PAREN_FIXTURE}` has no `debug.parsedquery`: {expected}")
        });

    // Observable 2a: the run did reach the nested edismax query and fanned out
    // over both `qf` fields, so the `)` did not corrupt it.
    for field in ["title", "body"] {
        assert!(
            parsed.contains(&format!("{field}:quick")),
            "`{field}:quick` is absent from Solr's parsedquery, so the `+\"quick\"` run did not \
             reach the `{{!edismax qf='title body'}}` nested query cleanly -- which is what a \
             whitespace-only terminator produces here, by binding `+\"quick\")` and handing the \
             nested parser an unbalanced paren: {parsed}"
        );
    }

    // Observable 2b: nothing came out of the `)`. Under the whitespace-only
    // reading there is no text left for the outer parser, and under a reading
    // that terminated the run but then re-parsed the `)` as text there would be
    // a stray `df` clause. Neither leaves a `df=id` clause behind, and real
    // Solr must not either: the `)` is the outer paren's closer, nothing more.
    assert!(
        !parsed.contains("id:"),
        "Solr's parsedquery carries a `df=id` clause, so something after the bound run was parsed \
         as query text rather than as the closing paren of the `(` the query opens with. Findings \
         90/91's account of the `)` terminator would then be wrong -- escalate rather than relaxing \
         this: {parsed}"
    );

    // Observable 1 + 3: status and result set. `sort=id asc` makes the id list
    // order-stable across the BM25 divergence, so it is asserted exactly.
    let (app, _dir) = edismax_app().await;
    let (status, actual) = get(&app, SHAPE_B_DEBUG_PAREN_PATH).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a `)`-terminated bound run is a valid query to real Solr (the fixture is a 200), so \
         Wayfinder must not 400 it"
    );
    assert_eq!(
        num_found(&actual),
        num_found(&expected),
        "numFound for the `)`-terminated Shape-B query must equal real Solr's.\nSolr: {expected}\n\
         Wayfinder: {actual}"
    );
    assert_eq!(
        ordered_ids(&actual),
        ordered_ids(&expected),
        "the matching document ids must be exactly Solr's -- this is the Shape-B capture whose \
         result set is non-degenerate, so it is the one that can actually catch a binding rule \
         that terminates the run in the wrong place"
    );
}

#[test]
fn midtoken_minus_debug_parsedquery_shows_one_clause_over_all_tokens() {
    let expected = require_capture(MIDTOKEN_MINUS_DEBUG_FIXTURE, MIDTOKEN_MINUS_DEBUG_PATH);
    assert_eq!(
        expected
            .pointer("/debug/rawquerystring")
            .and_then(Value::as_str),
        Some("state-of-the-art"),
        "the fixture must identify the literal mid-token `-` query"
    );
    let parsed = expected
        .pointer("/debug/parsedquery")
        .and_then(Value::as_str)
        .expect("mid-token-minus fixture has debug.parsedquery");
    assert_eq!(
        parsed.matches("DisjunctionMaxQuery").count(),
        1,
        "multiple dismax nodes would mean Solr split the hyphenated input into clauses: {parsed}"
    );
    for field in ["title", "body"] {
        for token in ["state", "art"] {
            assert!(
                parsed.contains(&format!("{field}:{token}")),
                "Solr's one clause must analyse to `{field}:{token}`: {parsed}"
            );
        }
    }
}

#[tokio::test]
async fn shape_b_whitespace_terminates_inside_nested_parens() {
    let expected = require_capture(
        SHAPE_B_NESTED_PAREN_DEBUG_FIXTURE,
        SHAPE_B_NESTED_PAREN_DEBUG_PATH,
    );
    assert_eq!(
        expected
            .pointer("/responseHeader/params/q")
            .and_then(Value::as_str),
        Some(SHAPE_B_NESTED_PAREN_DEBUG_Q),
        "the 400 fixture must identify the nested-paren Shape-B request"
    );
    assert_eq!(
        expected.pointer("/error/code").and_then(Value::as_u64),
        Some(400),
        "real Solr must reject the unbalanced remainder produced by the depth-one whitespace cut"
    );
    assert!(
        expected.get("debug").is_none(),
        "Solr cannot emit a debug parse tree when parsing fails"
    );

    let (app, _dir) = edismax_app().await;
    let (status, actual) = get(&app, SHAPE_B_NESTED_PAREN_DEBUG_PATH).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        actual.pointer("/error/code").and_then(Value::as_u64),
        Some(400)
    );
}
