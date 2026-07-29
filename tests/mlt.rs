//! MoreLikeThis (`/mlt`, issue #6, PRD §5) — `GET /solr/<core>/mlt`.
//!
//! Every expected value here comes from a committed fixture in
//! `solr-ref/responses/mlt_*.json`, captured against a dedicated 20-doc corpus
//! (`solr-ref/capture.sh`'s MLT block, container `wayfinder-solr-6`, port
//! 8993) — the canonical 5-doc tracer-bullet corpus has too little shared
//! vocabulary for MLT term statistics to mean anything (`docs/solr-ref-findings.md`
//! finding 55). Nothing here is derived from what Wayfinder happens to
//! produce.
//!
//! Scope, per the issue: `q` (selects the source doc), `mlt.fl`, `mlt.mintf`,
//! `mlt.mindf`, `mlt.maxdf`, `mlt.minwl`, `mlt.maxwl`, `mlt.maxqt`,
//! `mlt.boost`, plus standard `fl`/`rows`/`start`. Out of scope (not tested
//! here): `mlt=true` as a `/select` search component, and content-stream MLT
//! (POSTing free text instead of selecting a doc via `q`).
//!
//! ## Envelope shape, per the captured fixtures (findings 51-58)
//!
//! `{responseHeader, match, response[, interestingTerms]}`. `match` is the
//! source document as its own nested search-result object
//! (`numFound`/`start`/`numFoundExact`/`docs`) — empty `docs`/`numFound: 0`
//! when `q` did not resolve to a document. `response` holds the similar-docs
//! result set in the same four-key shape, **except** when `q` matched no
//! source document at all, in which case `response` is the bare JSON value
//! `null` (finding 54) — not an empty object. `interestingTerms` only appears
//! at all when `mlt.interestingTerms` is set to a truthy value, as a bare
//! top-level array sibling to `match`/`response`.
//!
//! ## A known gap this file cannot close alone
//!
//! `solr-ref/manifest.tsv` now carries the ten `mlt_*` rows (plain
//! core-relative GETs, per `CLAUDE.md`'s compatibility-contract section), so
//! `tests/differential.rs::hermetic_whole_query_set_matches_committed_fixtures`
//! picks them up too — but that test runs every manifest row against
//! `common::indexed_app()`'s 5-doc tracer-bullet corpus, which has none of
//! `mlt1`..`mlt20`. Extending `indexed_app()`'s corpus to a superset is not a
//! safe fix: dozens of existing fixtures (`facet_*`, `select_all`, etc.) pin
//! exact `numFound`/facet counts against exactly 5 docs. The correct fix is
//! almost certainly teaching that hermetic loop to skip `mlt_*`-named entries
//! in favour of this file's own dedicated-corpus loop
//! (`hermetic_mlt_manifest_entries_match_committed_fixtures` below) — the same
//! pattern `tests/differential.rs` already uses for `manifest-errors.tsv` rows
//! that need a non-canonical corpus (`FACETS_SCHEMA_TOML`, lines 44-51 there).
//! That plumbing change lives in `tests/differential.rs`, which per the task
//! spec is left for the implementor rather than made here. Until it lands,
//! `hermetic_whole_query_set_matches_committed_fixtures` will (correctly) fail
//! on every `mlt_*` entry the moment `/mlt` starts returning `200`s that don't
//! match the 5-doc corpus.

mod common;

use axum::Router;
use axum::http::StatusCode;
use common::diff::{diff, diff_ranked_ids, load_manifest, normalize, ranked_docs};
use common::key_order::{assert_same_key_order, get_text};
use common::{CORE, fixture, get};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Same field shape as the canonical tracer-bullet schema
/// (`tests/common::SCHEMA_TOML`) — `id` (string, fast, stored, unique key),
/// `body` (text_en, stored), `category` (string, fast, multi_valued, stored)
/// — matching `solr-ref/capture.sh`'s MLT block schema exactly, so the
/// captured fixtures are ground truth here too. Core named `content` per the
/// established convention (`tests/faceting.rs`'s `RANGE_SCHEMA_TOML` comment):
/// Wayfinder's core name is independent of the Solr core the fixtures came
/// from.
const MLT_SCHEMA_TOML: &str = r#"
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

[[fields]]
name = "category"
type = "string"
stored = true
fast = true
multi_valued = true
"#;

/// The exact 20-doc corpus `solr-ref/capture.sh`'s MLT block indexes: four
/// topic clusters (cooking, gardening, astronomy, outdoors) with real shared
/// vocabulary within a cluster, plus two deliberately unrelated docs
/// (`mlt19`, `mlt20`).
fn mlt_corpus() -> Value {
    serde_json::json!([
        {"id":"mlt1", "body":"the chef prepared a delicious pasta dish with fresh tomatoes and basil","category":["cooking","italian"]},
        {"id":"mlt2", "body":"fresh basil and ripe tomatoes make a wonderful pasta sauce","category":["cooking","italian"]},
        {"id":"mlt3", "body":"grilling chicken with garlic and rosemary is a classic dinner","category":["cooking","grilling"]},
        {"id":"mlt4", "body":"roasted vegetables with olive oil and garlic taste amazing","category":["cooking","vegetarian"]},
        {"id":"mlt5", "body":"baking bread requires yeast flour water and patience","category":["cooking","baking"]},
        {"id":"mlt6", "body":"planting tomatoes and basil in the garden this spring","category":["gardening"]},
        {"id":"mlt7", "body":"the garden needs watering every morning during summer heat","category":["gardening"]},
        {"id":"mlt8", "body":"pruning rose bushes keeps the garden looking tidy","category":["gardening"]},
        {"id":"mlt9", "body":"composting kitchen scraps enriches garden soil naturally","category":["gardening"]},
        {"id":"mlt10","body":"growing herbs like basil and rosemary indoors year round","category":["gardening","cooking"]},
        {"id":"mlt11","body":"astronomers observed a bright comet streaking across the night sky","category":["astronomy"]},
        {"id":"mlt12","body":"the telescope revealed distant galaxies and bright stars","category":["astronomy"]},
        {"id":"mlt13","body":"a lunar eclipse darkened the night sky for hours","category":["astronomy"]},
        {"id":"mlt14","body":"scientists study the orbit of planets around distant stars","category":["astronomy"]},
        {"id":"mlt15","body":"the night sky was clear enough to see the milky way","category":["astronomy"]},
        {"id":"mlt16","body":"hiking through the mountains offers stunning views of the valley","category":["outdoors"]},
        {"id":"mlt17","body":"camping near the lake was peaceful and quiet at night","category":["outdoors"]},
        {"id":"mlt18","body":"the river flows quietly through the quiet forest valley","category":["outdoors"]},
        {"id":"mlt19","body":"a short trip to buy office supplies and paper clips","category":["misc"]},
        {"id":"mlt20","body":"nothing here relates to any other document in this corpus","category":["misc"]}
    ])
}

/// Builds an app on `MLT_SCHEMA_TOML` and indexes `mlt_corpus()`.
async fn mlt_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), MLT_SCHEMA_TOML).expect("app must build");
    let (status, body) = common::post_docs(&app, &mlt_corpus()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the mlt corpus must succeed, got {body}"
    );
    (app, dir)
}

/// `common::normalize_envelope`/`common::diff::normalize` only strip
/// `_version_`/`_root_` from `/response/docs` — the `/mlt` envelope carries
/// the *same* internal fields under `/match/docs` too, since `match` is its
/// own nested search-result object. This composes `common::diff::normalize`
/// (for `responseHeader.QTime` and `/response/docs`) with the same stripping
/// applied to `/match/docs`.
fn normalize_mlt(value: Value) -> Value {
    let normalized = normalize(value).value;
    let mut v = normalized;
    if let Some(docs) = v.pointer_mut("/match/docs").and_then(|d| d.as_array_mut()) {
        for doc in docs.iter_mut() {
            if let Some(obj) = doc.as_object_mut() {
                obj.remove("_version_");
                obj.remove("_root_");
            }
        }
    }
    v
}

/// Asserts `actual` equals the named `/mlt` fixture, modulo `normalize_mlt`.
fn assert_matches_mlt_fixture(actual: Value, fixture_name: &str) {
    let expected = normalize_mlt(fixture(fixture_name));
    let actual = normalize_mlt(actual);
    assert_eq!(
        actual, expected,
        "response for fixture `{fixture_name}` did not match (modulo QTime / _version_ / _root_)"
    );
}

/// Recursively blanks every `score`/`maxScore` value to `null`. PRD
/// ratified-divergence 4 (also `tests/differential.rs`'s
/// `RANKED_SCORE_VALUE_RATIFIED`, applied there to `select_term_scored`/
/// `select_quick_scored`): Tantivy's BM25 magnitude is a real,
/// permanently-accepted scoring-formula divergence from Solr/Lucene's
/// BM25Similarity (observed here as a non-constant ~2.0x-2.2x ratio across
/// `mlt_fl_rows_start`'s four scored docs) — ranking order and every other
/// field still must match exactly, but the raw float is out of scope for
/// exact-equality fixture comparison. `diff()`'s own per-key `score`
/// tolerance (`score_tolerance() == 1e-3`) is for float/rounding noise, not a
/// magnitude difference this large, so it does not help here.
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

/// Same as `assert_matches_mlt_fixture`, but for fixtures that request
/// `fl=score`: doc set, order, and every non-score field must still match
/// the fixture exactly; `score`/`maxScore` values are blanked first per
/// `blank_bm25_score_magnitudes`.
fn assert_matches_mlt_fixture_ignoring_score_magnitude(actual: Value, fixture_name: &str) {
    let expected = blank_bm25_score_magnitudes(normalize_mlt(fixture(fixture_name)));
    let actual = blank_bm25_score_magnitudes(normalize_mlt(actual));
    assert_eq!(
        actual, expected,
        "response for fixture `{fixture_name}` did not match modulo QTime / _version_ / _root_ / \
         BM25 score magnitude (PRD ratified-divergence 4)"
    );
}

/// Real (un-blanked) `maxScore` semantics check, specific to the
/// `mlt_fl_rows_start` fixture's own data: `blank_bm25_score_magnitudes`
/// blanks *both* sides of the comparison above, so a regression that changes
/// `response.maxScore` from "the corpus-wide max over the full unpaginated
/// MLT hit set" (finding 58) to "the max over just the returned page" would
/// still pass every blanked-equality assert in this file, including the
/// manifest loop below. This asserts the real, un-blanked relationship
/// instead: `response.maxScore` must exceed the returned page's own max
/// score (this fixture's page, `start=1&rows=2`, never contains the
/// corpus-wide top hit), and `match.maxScore` — the *one*-doc `match`
/// block's summary — must equal its own single doc's score exactly.
fn assert_mlt_fl_rows_start_maxscore_semantics(actual: &Value) {
    let response = actual
        .get("response")
        .expect("mlt_fl_rows_start's response must be present");
    let page_max_score = response["docs"]
        .as_array()
        .expect("response.docs must be an array")
        .iter()
        .map(|doc| doc["score"].as_f64().expect("each doc must carry score"))
        .fold(f64::MIN, f64::max);
    let response_max_score = response["maxScore"]
        .as_f64()
        .expect("response.maxScore must be present when fl=score");
    assert!(
        response_max_score > page_max_score,
        "response.maxScore ({response_max_score}) must be the corpus-wide max over the full \
         unpaginated MLT hit set, strictly greater than this fixture's own returned page's max \
         ({page_max_score}) — equal would mean maxScore was (wrongly) recomputed over just the \
         page"
    );

    let match_block = actual
        .get("match")
        .expect("mlt_fl_rows_start's match must be present");
    let match_doc_score = match_block["docs"][0]["score"]
        .as_f64()
        .expect("match.docs[0].score must be present");
    let match_max_score = match_block["maxScore"]
        .as_f64()
        .expect("match.maxScore must be present when fl=score");
    assert_eq!(
        match_max_score, match_doc_score,
        "match.maxScore must equal match.docs[0].score — match always resolves to exactly one \
         source document"
    );
}

// --- basic envelope / status ------------------------------------------------

#[tokio::test]
async fn mlt_route_exists_and_returns_200_for_a_known_doc() {
    let (app, _dir) = mlt_app().await;
    let (status, _body) = get(&app, "mlt?q=id:mlt1&mlt.fl=body,category&wt=json").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "GET /solr/<core>/mlt for a known source doc must be 200 (no route registered yet is \
         the expected red state)"
    );
}

#[tokio::test]
async fn mlt_baseline_matches_fixture_including_degenerate_empty_result() {
    // Real Solr's default mintf=2/mindf=5 thresholds are too strict for a
    // 20-doc corpus (finding 55): even mlt1/mlt2's near-duplicate vocabulary
    // does not clear mindf=5, so `response` is legitimately an empty result
    // object here, not a bug in the fixture.
    let (app, _dir) = mlt_app().await;
    let (status, body) = get(&app, "mlt?q=id:mlt1&mlt.fl=body,category&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_mlt_fixture(body, "mlt_baseline");
}

#[tokio::test]
async fn mlt_fl_restricted_to_one_field_matches_fixture() {
    let (app, _dir) = mlt_app().await;
    let (status, body) = get(&app, "mlt?q=id:mlt1&mlt.fl=body&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_mlt_fixture(body, "mlt_fl_restricted");
}

// --- mintf/mindf/maxdf tuning, via ranked-id comparison ---------------------

#[tokio::test]
async fn mlt_mintf_mindf_maxdf_tuning_matches_ranked_fixture() {
    // Loosening mintf/mindf from the too-strict defaults surfaces 4 real
    // matches from the astronomy cluster (finding 55/56) — compared by
    // ranked-id list per PRD §8 ("compare ranked ID lists, not just result
    // sets"), reusing tests/differential.rs's own machinery
    // (`common::diff::diff_ranked_ids`) rather than inventing a second one.
    let (app, _dir) = mlt_app().await;
    let (status, body) = get(
        &app,
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.maxdf=10&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let expected = normalize_mlt(fixture("mlt_mintf_mindf_maxdf"));
    let actual = normalize_mlt(body);
    let report = diff_ranked_ids(&ranked_docs(&expected), &ranked_docs(&actual));
    assert!(
        report.diffs.is_empty(),
        "mlt.mintf/mlt.mindf/mlt.maxdf ranked result diverges from mlt_mintf_mindf_maxdf.json: \
         {:?}",
        report.diffs
    );
    // The ranked-id check only looks at response.docs[]; match/numFound
    // matter too and are pinned by the exact-match assertion.
    assert_matches_mlt_fixture(actual.clone(), "mlt_mintf_mindf_maxdf");
    let _ = actual;
}

// --- minwl/maxwl word-length gate --------------------------------------------

#[tokio::test]
async fn mlt_minwl_maxwl_narrows_the_match_set() {
    // Stacked on the same mintf=1/mindf=1 loosening as the tuning fixture
    // above (finding 56): minwl=6/maxwl=10 excludes short shared words
    // ("night", "sky") and narrows the astronomy cluster's 4 matches down to
    // 1 (`mlt12`).
    let (app, _dir) = mlt_app().await;
    let (status, body) = get(
        &app,
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.minwl=6&mlt.maxwl=10&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_mlt_fixture(body, "mlt_minwl_maxwl");
}

// --- maxqt caps interesting terms -------------------------------------------

#[tokio::test]
async fn mlt_maxqt_caps_interesting_terms_to_zero_matches() {
    // Capping to the top 2 interesting terms by weight (finding 56) narrows
    // the same loosened astronomy query all the way to 0 matches — a real,
    // captured Solr behaviour on this corpus, not a bug to soften.
    let (app, _dir) = mlt_app().await;
    let (status, body) = get(
        &app,
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.maxqt=2&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_mlt_fixture(body, "mlt_maxqt");
}

// --- mlt.boost changes match set and order ----------------------------------

#[tokio::test]
async fn mlt_boost_changes_ranked_match_order() {
    // finding 57: mlt.boost=true (loosened mintf/mindf) surfaces 3 matches in
    // a specific order (mlt2, mlt6, mlt10) that a plain term-overlap count
    // would not necessarily produce. Ranked-id comparison, same rationale as
    // the mintf/mindf/maxdf test above.
    let (app, _dir) = mlt_app().await;
    let (status, body) = get(
        &app,
        "mlt?q=id:mlt1&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.boost=true&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let expected = normalize_mlt(fixture("mlt_boost"));
    let actual = normalize_mlt(body);
    let report = diff_ranked_ids(&ranked_docs(&expected), &ranked_docs(&actual));
    assert!(
        report.diffs.is_empty(),
        "mlt.boost ranked result diverges from mlt_boost.json: {:?}",
        report.diffs
    );
}

// --- standard fl/rows/start on the MLT result set ---------------------------

#[tokio::test]
async fn mlt_fl_rows_start_paginates_the_similar_docs_result_set() {
    // finding 58: fl=id,score / rows=2 / start=1 against a 4-match query
    // pages to (mlt15, mlt12), and both `match` and `response` carry
    // per-doc `score` plus a top-level `maxScore` once `fl` includes `score`
    // — `response.maxScore` is the corpus-wide max, not recomputed over the
    // returned page (PRD ratified-divergence 4's `/select` semantics extend
    // here). The raw score magnitude itself is exempt from exact-equality
    // comparison for the same reason `select_term_scored`/`select_quick_scored`
    // are in `tests/differential.rs`'s `RANKED_SCORE_VALUE_RATIFIED`
    // (Tantivy's BM25 vs. Solr/Lucene's BM25Similarity is a real, permanent,
    // ratified scoring-formula divergence, not a wiring bug) — doc set,
    // order, and every other field still must match exactly.
    let (app, _dir) = mlt_app().await;
    let (status, body) = get(
        &app,
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&fl=id,score&rows=2&start=1&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_mlt_fl_rows_start_maxscore_semantics(&body);
    assert_matches_mlt_fixture_ignoring_score_magnitude(body, "mlt_fl_rows_start");
}

// --- key order (finding 52: responseHeader, match, response[, interestingTerms]) --

/// The `/mlt` row's own manifest query (rather than hand-copying it), so
/// editing `manifest.tsv` can't silently desynchronise this test from the
/// request the fixture was actually captured against — same rationale as
/// `tests/json_key_order.rs::keyorder_query_from_manifest`.
fn mlt_manifest_query(name: &str) -> String {
    let entries = load_manifest(&manifest_path());
    entries
        .iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("manifest.tsv has no row named `{name}`"))
        .path
        .clone()
}

#[tokio::test]
async fn mlt_baseline_key_order_matches_solr() {
    // finding 52: top-level `responseHeader, match, response` (no
    // `interestingTerms` here, since `mlt.interestingTerms` was not
    // requested), plus `match`/`response`'s own inner
    // `numFound, start, numFoundExact, docs` order.
    let (app, _dir) = mlt_app().await;
    let (status, text) = get_text(&app, CORE, &mlt_manifest_query("mlt_baseline")).await;
    assert_eq!(status, StatusCode::OK, "mlt_baseline must be a 200: {text}");
    assert_same_key_order(&text, "mlt_baseline");
}

#[tokio::test]
async fn mlt_interesting_terms_details_key_order_matches_solr() {
    // The only fixture exercising `interestingTerms`'s trailing position as a
    // top-level sibling of `match`/`response`.
    let (app, _dir) = mlt_app().await;
    let (status, text) = get_text(
        &app,
        CORE,
        &mlt_manifest_query("mlt_interesting_terms_details"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "mlt_interesting_terms_details must be a 200: {text}"
    );
    assert_same_key_order(&text, "mlt_interesting_terms_details");
}

// --- interestingTerms gating -------------------------------------------------

#[tokio::test]
async fn mlt_interesting_terms_key_absent_by_default() {
    let (app, _dir) = mlt_app().await;
    let (status, body) = get(&app, "mlt?q=id:mlt1&mlt.fl=body&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.get("interestingTerms").is_none(),
        "interestingTerms must be absent when mlt.interestingTerms was not requested, got: {body}"
    );
}

#[tokio::test]
async fn mlt_interesting_terms_details_matches_fixture() {
    // finding 53: `mlt.interestingTerms=details` adds a bare top-level
    // `interestingTerms` array (empty here, since this particular query's
    // result set is also empty per finding 55) sibling to `match`/`response`.
    let (app, _dir) = mlt_app().await;
    let (status, body) = get(
        &app,
        "mlt?q=id:mlt1&mlt.fl=body&mlt.interestingTerms=details&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.get("interestingTerms").is_some(),
        "interestingTerms must be present when mlt.interestingTerms=details was requested, got: \
         {body}"
    );
    assert_matches_mlt_fixture(body, "mlt_interesting_terms_details");
}

// --- degenerate cases: no interesting terms / no source doc -----------------

#[tokio::test]
async fn mlt_doc_with_no_interesting_terms_still_returns_empty_response_object() {
    // finding 54 (contrast case): a source doc that *does* exist but has no
    // meaningfully-shared vocabulary still gets an empty `response` *object*
    // (numFound: 0, docs: []) — not `null`. `null` is reserved for the case
    // below, where the source doc itself does not exist.
    let (app, _dir) = mlt_app().await;
    let (status, body) = get(&app, "mlt?q=id:mlt20&mlt.fl=body&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_mlt_fixture(body.clone(), "mlt_no_interesting_terms");
    assert!(
        body.pointer("/response").is_some_and(Value::is_object),
        "response must be an empty object (not null) when the source doc exists but has no \
         interesting terms, got: {body}"
    );
}

#[tokio::test]
async fn mlt_nonexistent_source_doc_returns_200_with_null_response() {
    // finding 54: `q` resolving to zero source documents is a 200 with
    // `match.numFound: 0` and the literal JSON value `null` for `response` —
    // not an error, and not the empty-object shape used when the source doc
    // exists but has no interesting terms.
    let (app, _dir) = mlt_app().await;
    let (status, body) = get(&app, "mlt?q=id:nosuchdoc&mlt.fl=body&wt=json").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_mlt_fixture(body.clone(), "mlt_nonexistent_doc");
    assert!(
        body.pointer("/response").is_some_and(Value::is_null),
        "response must be JSON null when q matched no source document, got: {body}"
    );
    assert_eq!(
        body.pointer("/match/numFound").and_then(Value::as_i64),
        Some(0)
    );
}

// --- SELECT_PARAMS-style strict_params guard (mlt.* must not 400) ----------

#[tokio::test]
async fn mlt_specific_params_are_not_rejected_as_unknown() {
    // Every mlt.* param exercised above must be registered wherever
    // `/select`'s SELECT_PARAMS-style allowlist lives for `/mlt`, or
    // strict_params = true would 400 requests real Solr serves (per
    // CLAUDE.md's compatibility-contract note on SELECT_PARAMS/UPDATE_PARAMS).
    // Exercised here as a single request naming every in-scope mlt.* param at
    // once, under an app with strict_params on.
    let dir = TempDir::new().expect("temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, MLT_SCHEMA_TOML).expect("write schema.toml");
    let config_path = dir.path().join("wayfinder.toml");
    std::fs::write(&config_path, "strict_params = true\n").expect("write wayfinder.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let app =
        wayfinder::app_with_config(&schema_path, &data_dir, &config_path).expect("app must build");
    let (status, body) = common::post_docs(&app, &mlt_corpus()).await;
    assert_eq!(status, StatusCode::OK, "indexing must succeed, got {body}");

    let (status, body) = get(
        &app,
        "mlt?q=id:mlt1&df=body&mlt.fl=body,category&mlt.mintf=1&mlt.mindf=1&mlt.maxdf=10&\
         mlt.minwl=2&mlt.maxwl=20&mlt.maxqt=10&mlt.boost=true&mlt.interestingTerms=details&\
         fl=id,score&rows=5&start=0&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "every mlt.* param in PRD §5's scope must be a registered param, not rejected under \
         strict_params: {body}"
    );
}

// --- overwritten docs must not desync doc_freq vs. alive-doc count ---------

#[tokio::test]
async fn mlt_after_reindexing_overwrites_does_not_panic_or_garbage_score() {
    // `mlt_idf`'s `doc_count` (`segment_reader.num_docs()`, alive docs only)
    // can be less than a term's raw `doc_freq` (`Searcher::doc_freq`, which
    // still counts docs an overwrite deleted from the term dictionary but
    // has not yet merged away) once any doc has been overwritten —
    // `add_documents` deletes-then-reinserts on every unique-key collision.
    // Re-POSTing the same corpus (every doc overwrites itself by `id`)
    // reproduces that gap without needing segment merges: this must not
    // panic (subtract-with-overflow) or 500, and the response must still be
    // a sane, well-formed `/mlt` result.
    let (app, _dir) = mlt_app().await;
    // A single overwrite is not enough to force `doc_freq > doc_count` for
    // any term in this corpus (few enough docs share any one word that one
    // extra generation of stale postings still leaves `doc_freq` well under
    // the 20 alive docs) — repost several times so stale postings from
    // multiple deleted-but-unmerged segments accumulate past `doc_count`.
    for _ in 0..25 {
        let (status, body) = common::post_docs(&app, &mlt_corpus()).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "re-indexing (overwriting) the mlt corpus must succeed, got {body}"
        );
    }

    let (status, body) = get(
        &app,
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.maxdf=10&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "/mlt after overwriting every doc must not panic or 500, got {body}"
    );
    let response = body
        .get("response")
        .expect("response must be present for a resolved source doc");
    assert!(
        response["numFound"].as_u64().unwrap_or(0) > 0,
        "the astronomy cluster's loosened mintf/mindf query must still find real matches after \
         overwriting, got: {body}"
    );
}

// --- differential-harness-style coverage over the mlt_* manifest rows -------

/// The subset of `solr-ref/manifest.tsv` this file owns: every row whose name
/// starts with `mlt_`. See the module doc comment for why these cannot run
/// through `tests/differential.rs`'s generic hermetic loop unmodified (that
/// loop's `common::indexed_app()` seeds a different, 5-doc corpus).
fn manifest_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("solr-ref/manifest.tsv")
}

#[tokio::test]
async fn hermetic_mlt_manifest_entries_match_committed_fixtures() {
    let (app, _dir) = mlt_app().await;
    let entries: Vec<_> = load_manifest(&manifest_path())
        .into_iter()
        .filter(|e| e.name.starts_with("mlt_"))
        .collect();
    assert!(
        !entries.is_empty(),
        "expected at least the ten mlt_* rows capture.sh's MLT block appends to manifest.tsv"
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

        // `mlt_fl_rows_start` is the one manifest row requesting `fl=score`;
        // its BM25 magnitude is exempt from exact comparison for the same
        // ratified reason `assert_matches_mlt_fixture_ignoring_score_magnitude`
        // documents above (PRD ratified-divergence 4). The real (un-blanked)
        // `maxScore` semantics are still checked directly, so this loop
        // can't be fooled by a regression that blanking alone would hide.
        let (expected, actual) = if entry.name == "mlt_fl_rows_start" {
            let normalized_actual = normalize_mlt(actual);
            assert_mlt_fl_rows_start_maxscore_semantics(&normalized_actual);
            (
                blank_bm25_score_magnitudes(normalize_mlt(fixture(&entry.name))),
                blank_bm25_score_magnitudes(normalized_actual),
            )
        } else {
            (normalize_mlt(fixture(&entry.name)), normalize_mlt(actual))
        };
        let report = diff(&expected, &actual);
        if !report.diffs.is_empty() {
            failures.push(format!("{}: {:?}", entry.name, report.diffs));
        }
    }

    assert!(
        failures.is_empty(),
        "hermetic mlt differential failures against solr-ref fixtures:\n{}",
        failures.join("\n")
    );
}
