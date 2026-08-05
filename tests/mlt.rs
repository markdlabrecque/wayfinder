//! MoreLikeThis (`/mlt`, issue #6, PRD §5) — `GET /wayfinder/<core>/mlt`.
//!
//! Every fixture-backed expected value here comes from the dedicated 20-doc
//! corpus in the dedicated Solr capture's MLT block. The original fixtures used
//! `wayfinder-solr-6` on port 8993; `mlt_maxntp_low.json` was captured later
//! against the identical schema/corpus in `wayfinder-solr-189` (issue #189's
//! follow-up to the findings 97-101 MLT refinement block). The canonical 5-doc
//! tracer-bullet corpus has too little shared
//! vocabulary for MLT term statistics to mean anything
//! (`docs/solr-ref-findings.md` finding 64). Nothing is derived from what
//! Wayfinder happens to produce.
//!
//! Scope, per the issue: `q` (selects the source doc), `mlt.fl`, `mlt.mintf`,
//! `mlt.mindf`, `mlt.maxdf`, `mlt.minwl`, `mlt.maxwl`, `mlt.maxqt`,
//! `mlt.maxntp`, `mlt.boost`, plus standard `fl`/`rows`/`start`. Out of scope
//! (not tested here): `mlt=true` as a `/select` search component, and content-stream MLT
//! (POSTing free text instead of selecting a doc via `q`).
//!
//! ## Envelope shape, per the captured fixtures (findings 60-67)
//!
//! `{responseHeader, match, response[, interestingTerms]}`. `match` is the
//! source document as its own nested search-result object
//! (`numFound`/`start`/`numFoundExact`/`docs`) — empty `docs`/`numFound: 0`
//! when `q` did not resolve to a document. `response` holds the similar-docs
//! result set in the same four-key shape, **except** when `q` matched no
//! source document at all, in which case `response` is the bare JSON value
//! `null` (finding 63) — not an empty object. `interestingTerms` only appears
//! at all when `mlt.interestingTerms` is set to a truthy value, as a bare
//! top-level array sibling to `match`/`response`.
//!
//! ## Dedicated request coverage
//!
//! The retained fixture requests are declared in this file because each one
//! requires the dedicated 20-doc corpus. Extending the shared 5-doc corpus is
//! not safe: existing fixtures pin its exact counts and facet buckets.

mod common;

use axum::Router;
use axum::http::StatusCode;
use common::diff::{diff, diff_ranked_ids, normalize, ranked_docs};
use common::key_order::{assert_same_key_order, get_text};
use common::{CORE, fixture, get};
use serde_json::Value;
use tempfile::TempDir;

/// Same field shape as the canonical tracer-bullet schema
/// (`tests/common::SCHEMA_TOML`) — `id` (string, fast, stored, unique key),
/// `body` (text_en, stored), `category` (string, fast, multi_valued, stored)
/// — matching the dedicated Solr capture's MLT block schema exactly, so the
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

/// Retains stop words so `mlt.maxntp`'s count-before-noise ordering is
/// observable, and permits multiple stored values so its per-value reset is
/// observable in the same request.
const MLT_MAXNTP_SCHEMA_TOML: &str = r#"
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
type = "text_no_stop"
stored = true
multi_valued = true

[[field_types]]
name = "text_no_stop"
tokenizer = "simple"
[[field_types.filters]]
kind = "lowercase"
"#;

/// The exact 20-doc corpus the dedicated Solr capture's MLT block indexes: four
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
/// ratified-divergence 4 (also the retained fixture tests'
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
/// MLT hit set" (finding 67) to "the max over just the returned page" would
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
        "GET /wayfinder/<core>/mlt for a known source doc must be 200 (no route registered yet is \
         the expected red state)"
    );
}

#[tokio::test]
async fn mlt_baseline_matches_fixture_including_degenerate_empty_result() {
    // Real Solr's default mintf=2/mindf=5 thresholds are too strict for a
    // 20-doc corpus (finding 64): even mlt1/mlt2's near-duplicate vocabulary
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
    // matches from the astronomy cluster (finding 64/65) — compared by
    // ranked-id list per PRD §8 ("compare ranked ID lists, not just result
    // sets"), reusing feature tests' own machinery
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
    // above (finding 65): minwl=6/maxwl=10 excludes short shared words
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
    // Capping to the top 2 interesting terms by weight (finding 65) narrows
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
    // finding 66: mlt.boost=true (loosened mintf/mindf) surfaces 3 matches in
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
    // finding 67: fl=id,score / rows=2 / start=1 against a 4-match query
    // pages to (mlt15, mlt12), and both `match` and `response` carry
    // per-doc `score` plus a top-level `maxScore` once `fl` includes `score`
    // — `response.maxScore` is the corpus-wide max, not recomputed over the
    // returned page (PRD ratified-divergence 4's `/select` semantics extend
    // here). The raw score magnitude itself is exempt from exact-equality
    // comparison for the same reason `select_term_scored`/`select_quick_scored`
    // are in the retained fixture tests' `RANKED_SCORE_VALUE_RATIFIED`
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

// --- key order (finding 61: responseHeader, match, response[, interestingTerms]) --

const MLT_BASELINE_PATH: &str = "mlt?q=id:mlt1&mlt.fl=body,category&wt=json";
const MLT_INTERESTING_TERMS_DETAILS_PATH: &str =
    "mlt?q=id:mlt1&mlt.fl=body&mlt.interestingTerms=details&wt=json";

#[tokio::test]
async fn mlt_baseline_key_order_matches_solr() {
    // finding 61: top-level `responseHeader, match, response` (no
    // `interestingTerms` here, since `mlt.interestingTerms` was not
    // requested), plus `match`/`response`'s own inner
    // `numFound, start, numFoundExact, docs` order.
    let (app, _dir) = mlt_app().await;
    let (status, text) = get_text(&app, CORE, MLT_BASELINE_PATH).await;
    assert_eq!(status, StatusCode::OK, "mlt_baseline must be a 200: {text}");
    assert_same_key_order(&text, "mlt_baseline");
}

#[tokio::test]
async fn mlt_interesting_terms_details_key_order_matches_solr() {
    // The only fixture exercising `interestingTerms`'s trailing position as a
    // top-level sibling of `match`/`response`.
    let (app, _dir) = mlt_app().await;
    let (status, text) = get_text(&app, CORE, MLT_INTERESTING_TERMS_DETAILS_PATH).await;
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
    // finding 62: `mlt.interestingTerms=details` adds a bare top-level
    // `interestingTerms` array (empty here, since this particular query's
    // result set is also empty per finding 64) sibling to `match`/`response`.
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
    // finding 63 (contrast case): a source doc that *does* exist but has no
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
    // finding 63: `q` resolving to zero source documents is a 200 with
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

#[tokio::test]
async fn mlt_issue_141_params_are_not_rejected_as_unknown() {
    // Issue #141's four *implemented* params (`fq`, `mlt.match.include`,
    // `mlt.match.offset`, `json.nl`) must be registered in `MLT_PARAMS` —
    // same guard as `mlt_specific_params_are_not_rejected_as_unknown` above,
    // kept separate so a reviewer can see exactly which params this issue
    // adds.
    //
    // `mlt.maxntp` is covered by its compatibility test below: under strict
    // params it must be accepted, and a low value must narrow the result set
    // rather than being silently ignored.
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
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&fq=category:astronomy&\
         mlt.match.include=false&mlt.match.offset=0&json.nl=flat&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "fq/mlt.match.include/mlt.match.offset/json.nl must be registered params on /mlt, not \
         rejected under strict_params (issue #141): {body}"
    );
}

// --- issue #141: fq scopes the similar-docs result set, not seed resolution -

#[tokio::test]
async fn mlt_fq_narrows_the_similar_docs_result_set() {
    // finding 98: fq=category:astronomy narrows the unfiltered astronomy
    // cluster (mlt13, mlt15, mlt12, mlt17 — see mlt_mintf_mindf_maxdf) down
    // to 3, dropping mlt17 (the one non-astronomy doc in that set).
    let (app, _dir) = mlt_app().await;
    let (status, body) = get(
        &app,
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&fq=category:astronomy&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_mlt_fixture(body, "mlt_fq_scope");
}

#[tokio::test]
async fn mlt_fq_does_not_restrict_which_document_q_resolves_as_seed() {
    // finding 98: fq=category:cooking excludes mlt11's own category
    // (astronomy) — real Solr still resolves `match.docs[0]` to mlt11. `fq`
    // only ever filters `response` (the similar-docs set), never the seed
    // resolution driven by `q` alone.
    let (app, _dir) = mlt_app().await;
    let (status, body) = get(
        &app,
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&fq=category:cooking&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.pointer("/match/docs/0/id").and_then(Value::as_str),
        Some("mlt11"),
        "fq must not exclude the seed doc from `match`, even when the seed doc itself fails the \
         filter, got: {body}"
    );
    assert_matches_mlt_fixture(body, "mlt_fq_seed_not_filtered");
}

#[tokio::test]
async fn mlt_multiple_fq_params_and_together() {
    // finding 98: two fq params that no single doc satisfies both of
    // (astronomy AND outdoors) must empty `response` to 0 — same AND
    // semantics as /select's fq (CLAUDE.md's `parse_query`/`filter_queries`
    // loop in `select`).
    let (app, _dir) = mlt_app().await;
    let (status, body) = get(
        &app,
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&fq=category:astronomy&\
         fq=category:outdoors&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_mlt_fixture(body, "mlt_fq_multiple_and");
}

#[tokio::test]
async fn mlt_malformed_fq_is_a_400_not_a_silently_dropped_filter() {
    // Same rejection `/select` gives the same malformed filter
    // (`tests/json_key_order.rs::error_envelope_key_order_matches_solr` uses
    // this exact `fq` against `err_bad_syntax`). Worth its own test because
    // the failure mode of dropping the parse error is invisible otherwise: an
    // unparseable filter would silently widen the result set back to the
    // unfiltered one.
    let (app, _dir) = mlt_app().await;
    let (status, body) = get(
        &app,
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&fq=category:[unclosed&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a malformed fq on /mlt must 400, not be dropped: {body}"
    );
    assert_eq!(
        body.pointer("/error/code").and_then(Value::as_u64),
        Some(400),
        "the error envelope must carry the 400 code, got: {body}"
    );

    // The same malformed filter must still 400 when `q` resolves *no* seed
    // doc — the `fq` parse deliberately sits outside the `q` arm in
    // `src/lib.rs::mlt`, and nothing else in this suite exercises that.
    // Mutation-tested: wrapping the parse loop in `if !hits.is_empty()`
    // survived the entire suite until this case existed, silently downgrading
    // a malformed filter to a 200-with-null-response whenever `q` missed.
    let (status, body) = get(
        &app,
        "mlt?q=id:nosuchdoc&mlt.fl=body&fq=category:[unclosed&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a malformed fq must 400 even when `q` resolves no seed doc, got: {body}"
    );
    assert_eq!(
        body.pointer("/error/code").and_then(Value::as_u64),
        Some(400),
        "the no-seed error envelope must carry the 400 code, got: {body}"
    );

    // Strongest form of the same claim: no `q` param at all. Mutation-tested:
    // gating the parse loop on `params.get("q").is_some()` survived the whole
    // suite until this case existed, because every other case sends a `q`.
    let (status, body) = get(&app, "mlt?mlt.fl=body&fq=category:[unclosed&wt=json").await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a malformed fq must 400 even with no `q` param at all, got: {body}"
    );

    // Every `fq` is validated, not just the first. Mutation-tested: swallowing
    // the parse error for all but the first `fq` survived the suite until this
    // case existed, since the multi-fq test above sends two *valid* filters.
    let (status, body) = get(
        &app,
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&fq=category:animals&fq=category:[unclosed&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a malformed *second* fq must 400 just like a malformed first one, got: {body}"
    );
}

// --- issue #141: mlt.match.include=false drops the match key entirely -----

#[tokio::test]
async fn mlt_match_include_false_omits_the_match_key_entirely() {
    // finding 100: mlt.match.include=false removes `match` from the envelope
    // outright — not an empty-and-present object. `response` is unaffected.
    let (app, _dir) = mlt_app().await;
    let (status, body) = get(
        &app,
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.match.include=false&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.get("match").is_none(),
        "mlt.match.include=false must omit `match` entirely, got: {body}"
    );
    assert_matches_mlt_fixture(body, "mlt_match_include_false");
}

#[tokio::test]
async fn mlt_match_include_true_is_the_default_and_keeps_the_match_key() {
    // Only the literal `false` turns `match` off — `true` is Solr's documented
    // default, so sending it explicitly must be indistinguishable from not
    // sending it at all. No fixture captures the explicit-`true` request (it
    // asks for the default), so the ground truth here is the *same request
    // without the param*, compared whole rather than key-by-key.
    //
    // This exists because a gate keyed on the param's mere presence, rather
    // than on its value, passes every other test in this file: nothing else
    // ever sends `mlt.match.include=true` (mutation-tested — replacing the
    // value check with `params.get("mlt.match.include").is_none()` survived
    // the whole suite until this test).
    let (app, _dir) = mlt_app().await;
    let (status, explicit) = get(
        &app,
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.match.include=true&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        explicit
            .pointer("/match/docs/0/id")
            .and_then(Value::as_str)
            .is_some(),
        "mlt.match.include=true must keep the `match` block, got: {explicit}"
    );

    let (status, default) = get(
        &app,
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        normalize_mlt(explicit),
        normalize_mlt(default),
        "mlt.match.include=true must be indistinguishable from the param being absent"
    );
}

// --- issue #141: mlt.match.offset picks a different seed document ---------

#[tokio::test]
async fn mlt_match_offset_selects_the_nth_match_as_seed() {
    // finding 99: q=category:astronomy resolves 5 docs; mlt.match.offset=1
    // picks the *second* one (mlt12, doc order) as the seed, not the first
    // (mlt11, offset 0/absent) — and match.start reflects the offset (1),
    // not the usual 0.
    let (app, _dir) = mlt_app().await;
    let (status, body) = get(
        &app,
        "mlt?q=category:astronomy&mlt.fl=body&mlt.match.offset=1&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.pointer("/match/docs/0/id").and_then(Value::as_str),
        Some("mlt12"),
        "mlt.match.offset=1 must pick the second q-match (mlt12) as the seed doc, got: {body}"
    );
    assert_eq!(
        body.pointer("/match/start").and_then(Value::as_u64),
        Some(1),
        "match.start must reflect mlt.match.offset, got: {body}"
    );
    assert_matches_mlt_fixture(body, "mlt_match_offset");
}

#[tokio::test]
async fn mlt_match_offset_past_the_last_hit_resolves_no_seed() {
    // Pins *Wayfinder's chosen* behaviour, not captured Solr: no fixture
    // covers an out-of-range `mlt.match.offset`, and the `ponytail:` on
    // `src/lib.rs::mlt` names that as the ceiling. Without this test the
    // ceiling is unenforceable — both "fall back to the top hit"
    // (`.or(hits.first())`) and "clamp to the last hit" survive the whole
    // suite, and each would answer a different question silently.
    //
    // The chosen behaviour is no seed at all, which is finding 63's existing
    // "q matched nothing" shape: `match.numFound` still reports the real hit
    // count for `q`, `match.docs` is empty, and `response` is the bare JSON
    // `null`. If a capture ever contradicts this, the fixture wins and this
    // test is what will have to change.
    let (app, _dir) = mlt_app().await;
    let (status, body) = get(
        &app,
        "mlt?q=category:astronomy&mlt.fl=body&mlt.match.offset=99&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.pointer("/match/numFound").and_then(Value::as_u64),
        Some(5),
        "match.numFound must still be q's real hit count, got: {body}"
    );
    assert_eq!(
        body.pointer("/match/docs").and_then(Value::as_array),
        Some(&Vec::new()),
        "an out-of-range mlt.match.offset must resolve no seed doc — not the first hit, not the \
         last — got: {body}"
    );
    assert_eq!(
        body.get("response"),
        Some(&Value::Null),
        "with no seed resolved, `response` is the bare null of finding 63, got: {body}"
    );
}

// --- issue #141: json.nl reshapes interestingTerms's container ------------

#[tokio::test]
async fn mlt_json_nl_map_renders_interesting_terms_as_object_when_empty() {
    // finding 101: real Solr's default rendering of an empty interestingTerms
    // set is `[ ]` (json.nl=flat, the default — see
    // mlt_interesting_terms_details.json); json.nl=map renders the same
    // empty set as `{ }` instead. Wayfinder's interestingTerms is always `[]`
    // today regardless of json.nl, so this must diverge until json.nl is
    // threaded into that rendering.
    let (app, _dir) = mlt_app().await;
    let (status, body) = get(
        &app,
        "mlt?q=id:mlt1&mlt.fl=body&mlt.interestingTerms=details&json.nl=map&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.get("interestingTerms").is_some_and(Value::is_object),
        "json.nl=map must render an empty interestingTerms set as `{{}}`, not `[]`, got: {body}"
    );
    assert_matches_mlt_fixture(body, "mlt_json_nl_map_empty_terms");
}

#[tokio::test]
async fn mlt_json_nl_flat_renders_interesting_terms_as_the_default_array() {
    // The other half of finding 101: `flat` is Solr's `json.nl` default, so
    // sending it explicitly must produce the array shape
    // `mlt_interesting_terms_details.json` captures for the same query with
    // no `json.nl` at all — only `map` changes the container.
    //
    // Mutation-tested: keying the container on `json.nl` being *present*
    // rather than on it being `map` (`params.get("json.nl").is_some()`)
    // survived the whole suite until this test, because
    // `mlt_json_nl_map_empty_terms` is the only fixture that sends the param.
    let (app, _dir) = mlt_app().await;
    let (status, body) = get(
        &app,
        "mlt?q=id:mlt1&mlt.fl=body&mlt.interestingTerms=details&json.nl=flat&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.get("interestingTerms"),
        Some(&Value::Array(Vec::new())),
        "json.nl=flat is the default named-list shape and must render `[]`, got: {body}"
    );
    assert_matches_mlt_fixture(body, "mlt_interesting_terms_details");
}

// --- issue #188 (descoped from #141): `fl=*` is a wildcard ------------------

/// `fl=*,score` on real Solr returns every stored/docValues field *and*
/// `score` (`solr-ref/responses/mlt_fl_wildcard_score.json`, the ground truth
/// here; `solr-ref/search-api/trace/00010.json` is the `/select`-side witness
/// for the same shape) — `*` is a wildcard, not a literal field name.
///
/// `CoreIndex::render_doc` treated `fl` as a literal allowlist with no `*`
/// handling at all, so `fl=*,score` returned *only* `score` and dropped every
/// real field. That is a `render_doc` gap shared with `/select`, which is why
/// it was descoped from #141 into **issue #188** and fixed in `render_doc`
/// rather than in `/mlt`'s handler: the `/select` half is
/// `tests/select_fl_wildcard.rs`.
///
/// `assert_matches_mlt_fixture_ignoring_score_magnitude`, not
/// `assert_matches_mlt_fixture`: this request asks for `score`, and Tantivy's
/// BM25 magnitude is PRD ratified-divergence 4, which #188 does not change.
/// Doc set, ranking order and every non-score field still have to match the
/// fixture exactly.
///
/// `mlt_fl_wildcard_score` stays in the fixture loop's
/// `SCORE_MAGNITUDE_EXEMPT` set for the ratified score-magnitude divergence,
/// which is unrelated to wildcard field expansion.
#[tokio::test]
async fn mlt_fl_wildcard_plus_score_returns_every_stored_field_plus_score() {
    let (app, _dir) = mlt_app().await;
    let (status, body) = get(
        &app,
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&fl=*,score&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // Named up front so a failure says "the wildcard is still being treated as
    // a literal field name" rather than dumping a whole-envelope diff.
    let seed = body
        .pointer("/match/docs/0")
        .expect("the seed doc must still be resolved and rendered")
        .clone();
    for field in ["id", "body", "category"] {
        assert!(
            seed.get(field).is_some(),
            "issue #188: `fl=*,score` must expand `*` to every stored field, so `{field}` must \
             be present on the seed doc (`mlt_fl_wildcard_score.json`), got: {body}"
        );
    }
    assert!(
        seed.get("score").is_some(),
        "`fl=*,score` must still carry `score` alongside the expanded fields, got: {body}"
    );
    assert_matches_mlt_fixture_ignoring_score_magnitude(body, "mlt_fl_wildcard_score");
}

/// The key *order* half, which `assert_matches_mlt_fixture_ignoring_score_magnitude`
/// structurally cannot see: `serde_json` is built with `preserve_order`, but its
/// `Map` still compares as a map regardless of key order (see
/// `tests/common/mod.rs::normalize_envelope`'s doc comment). So the order has
/// to be read off the raw response text.
///
/// `mlt_fl_wildcard_score.json` renders each doc as
/// `id, body, category, _version_, _root_, score` — schema order, then the
/// internal fields Wayfinder lacks, then `score` last (finding 24: Solr
/// appends its pseudo-fields after the real ones). `assert_same_key_order`
/// already ignores `_version_`/`_root_` inside `response.docs[*]`/`match.docs[*]`,
/// so the expectation reduces to "every stored field in schema order, then
/// `score`".
#[tokio::test]
async fn mlt_fl_wildcard_plus_score_matches_the_fixture_doc_key_order() {
    let expected = common::key_order::fixture_key_order("mlt_fl_wildcard_score")
        .keys_at("response.docs[0]", "mlt_fl_wildcard_score doc keys");
    assert!(
        !common::key_order::is_alphabetical(&expected),
        "vacuity guard: the fixture's doc key order ({expected:?}) must not be alphabetical, or \
         `actual == fixture` would prove nothing about ordering"
    );

    let (app, _dir) = mlt_app().await;
    let (status, text) = get_text(
        &app,
        CORE,
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&fl=*,score&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {text}");
    assert_same_key_order(&text, "mlt_fl_wildcard_score");
}

// --- issue #189: mlt.maxntp limits parsed seed tokens -----------------------

#[tokio::test]
async fn mlt_maxntp_low_narrows_and_high_preserves_the_fixture_result() {
    // Strict params makes acceptance part of the contract: a handler that
    // simply ignores an unregistered `mlt.maxntp` must not pass these fixtures.
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

    let (status, low) = get(
        &app,
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.maxdf=10&mlt.maxntp=1&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "low mlt.maxntp must be accepted, got: {low}"
    );
    assert_eq!(
        low.pointer("/response/numFound").and_then(Value::as_u64),
        Some(0),
        "mlt.maxntp=1 must narrow mlt11's similar docs to zero, got: {low}"
    );
    assert_matches_mlt_fixture(low, "mlt_maxntp_low");

    let (status, high) = get(
        &app,
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.maxdf=10&mlt.maxntp=5000&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "high mlt.maxntp must be accepted, got: {high}"
    );
    assert_matches_mlt_fixture(high, "mlt_maxntp_noop");
}

#[tokio::test]
async fn mlt_maxntp_counts_before_noise_and_resets_for_each_stored_value() {
    // Lucene 9.12.3's addTermFrequencies starts tokenCount at zero for every
    // stored IndexableField value and increments it before isNoiseWord. With
    // maxntp=1, the first value spends its allowance on the MLT stop word
    // "the", while the reset lets the second value contribute "beta". A
    // global cap would return no docs; counting after noise would also return
    // the alpha doc.
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), MLT_MAXNTP_SCHEMA_TOML)
        .expect("custom MLT app must build");
    let docs = serde_json::json!([
        {"id":"seed", "body":["the alpha", "beta gamma"]},
        {"id":"alpha", "body":["alpha"]},
        {"id":"beta", "body":["beta"]}
    ]);
    let (status, body) = common::post_docs(&app, &docs).await;
    assert_eq!(status, StatusCode::OK, "indexing must succeed: {body}");

    let (status, body) = get(
        &app,
        "mlt?q=id:seed&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.maxntp=1&fl=id&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "maxntp request must succeed: {body}"
    );
    assert_eq!(
        body.pointer("/response/numFound"),
        Some(&serde_json::json!(1))
    );
    assert_eq!(
        body.pointer("/response/docs/0/id"),
        Some(&serde_json::json!("beta"))
    );
}

#[tokio::test]
async fn mlt_maxntp_matches_solrs_signed_java_int_edges() {
    // Captured live against solr:9 on 2026-08-01: zero and negatives are
    // accepted and mine no terms; malformed and out-of-i32 values are 400s.
    let (app, _dir) = mlt_app().await;
    for value in ["0", "-1"] {
        let (status, body) = get(
            &app,
            &format!(
                "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.maxntp={value}&wt=json"
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "maxntp={value}: {body}");
        assert_eq!(
            body.pointer("/response/numFound"),
            Some(&serde_json::json!(0)),
            "maxntp={value} must mine no terms: {body}"
        );
    }

    for (value, fixture_name) in [
        ("abc", "mlt_maxntp_invalid"),
        ("2147483648", "mlt_maxntp_overflow"),
    ] {
        let (status, body) = get(
            &app,
            &format!("mlt?q=id:mlt11&mlt.fl=body&mlt.maxntp={value}&wt=json"),
        )
        .await;
        let expected = fixture(fixture_name);
        assert_eq!(
            status.as_u16() as i64,
            expected["error"]["code"].as_i64().unwrap(),
            "maxntp={value} HTTP status must match Solr: {body}"
        );
        assert_eq!(body["responseHeader"], expected["responseHeader"]);
        assert_eq!(body["error"]["code"], expected["error"]["code"]);
        let metadata = body["error"]["metadata"].as_array().unwrap();
        assert!(metadata.iter().any(|value| value == "error-class"));
        assert!(metadata.iter().any(|value| value == "root-error-class"));
        assert!(
            body["error"]["msg"]
                .as_str()
                .is_some_and(|msg| !msg.is_empty())
        );
    }

    // Live Solr parses `q` before `mlt.maxntp`: with both malformed, its
    // parser error names the query and never reaches the Java-int error.
    let (status, body) = get(&app, "mlt?q=body:%5B&mlt.fl=body&mlt.maxntp=abc&wt=json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]["msg"]
            .as_str()
            .is_some_and(|msg| msg.contains("body:[") && !msg.contains("mlt.maxntp")),
        "malformed q must win parse precedence: {body}"
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

// --- fixture-backed coverage over the dedicated MLT requests ----------------

/// Every dedicated-corpus request whose response fixture this suite verifies.
const MLT_FIXTURE_REQUESTS: &[(&str, &str)] = &[
    ("mlt_baseline", "mlt?q=id:mlt1&mlt.fl=body,category&wt=json"),
    ("mlt_fl_restricted", "mlt?q=id:mlt1&mlt.fl=body&wt=json"),
    (
        "mlt_mintf_mindf_maxdf",
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.maxdf=10&wt=json",
    ),
    (
        "mlt_minwl_maxwl",
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.minwl=6&mlt.maxwl=10&wt=json",
    ),
    (
        "mlt_maxqt",
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.maxqt=2&wt=json",
    ),
    (
        "mlt_boost",
        "mlt?q=id:mlt1&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.boost=true&wt=json",
    ),
    (
        "mlt_fl_rows_start",
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&fl=id,score&rows=2&start=1&wt=json",
    ),
    (
        "mlt_interesting_terms_details",
        "mlt?q=id:mlt1&mlt.fl=body&mlt.interestingTerms=details&wt=json",
    ),
    (
        "mlt_no_interesting_terms",
        "mlt?q=id:mlt20&mlt.fl=body&wt=json",
    ),
    (
        "mlt_nonexistent_doc",
        "mlt?q=id:nosuchdoc&mlt.fl=body&wt=json",
    ),
    (
        "mlt_fq_scope",
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&fq=category:astronomy&wt=json",
    ),
    (
        "mlt_fq_seed_not_filtered",
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&fq=category:cooking&wt=json",
    ),
    (
        "mlt_fq_multiple_and",
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&fq=category:astronomy&fq=category:outdoors&wt=json",
    ),
    (
        "mlt_match_include_false",
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.match.include=false&wt=json",
    ),
    (
        "mlt_match_offset",
        "mlt?q=category:astronomy&mlt.fl=body&mlt.match.offset=1&wt=json",
    ),
    (
        "mlt_json_nl_map_empty_terms",
        "mlt?q=id:mlt1&mlt.fl=body&mlt.interestingTerms=details&json.nl=map&wt=json",
    ),
    (
        "mlt_fl_wildcard_score",
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&fl=*,score&wt=json",
    ),
    (
        "mlt_maxntp_noop",
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.maxdf=10&mlt.maxntp=5000&wt=json",
    ),
    (
        "mlt_maxntp_low",
        "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.maxdf=10&mlt.maxntp=1&wt=json",
    ),
];

#[tokio::test]
async fn hermetic_mlt_fixture_requests_match_committed_fixtures() {
    let (app, _dir) = mlt_app().await;
    let mut failures = Vec::new();
    for &(name, path) in MLT_FIXTURE_REQUESTS {
        let (status, actual) = get(&app, path).await;
        if status != StatusCode::OK {
            failures.push(format!("{name}: HTTP status {status} vs expected 200"));
            continue;
        }

        const SCORE_MAGNITUDE_EXEMPT: &[&str] = &["mlt_fl_rows_start", "mlt_fl_wildcard_score"];
        let (expected, actual) = if SCORE_MAGNITUDE_EXEMPT.contains(&name) {
            let normalized_actual = normalize_mlt(actual);
            if name == "mlt_fl_rows_start" {
                assert_mlt_fl_rows_start_maxscore_semantics(&normalized_actual);
            }
            (
                blank_bm25_score_magnitudes(normalize_mlt(fixture(name))),
                blank_bm25_score_magnitudes(normalized_actual),
            )
        } else {
            (normalize_mlt(fixture(name)), normalize_mlt(actual))
        };
        let report = diff(&expected, &actual);
        if !report.diffs.is_empty() {
            failures.push(format!("{name}: {:?}", report.diffs));
        }
    }

    assert!(
        failures.is_empty(),
        "hermetic MLT fixture failures:\n{}",
        failures.join("\n")
    );
}
