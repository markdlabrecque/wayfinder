//! Issue #137: local-params prefix parsing in `q`, e.g.
//! `{!edismax qf='fieldA^1 fieldB^1'}...`.
//!
//! ## Premises verified against fixtures before writing these tests
//!
//! 1. `/select`'s captured handler defaults
//!    (`solr-ref/search-api/configset/solrconfig_extra.xml:110-118`) are
//!    `defType=lucene`, `df=id`, `omitHeader=true` — confirmed by reading that
//!    file. The outer parser is lucene, so `{!edismax ...}` in Shape B is an
//!    **inline nested query**, not a position-0 local-params block. The
//!    leading `(` around the whole `q` is irrelevant to that fact.
//! 2. The claimed binding rule — Solr's inline nested query binds only the
//!    next whitespace-delimited token, and everything after that token is
//!    parsed by the outer lucene parser against `df=id`, which matches
//!    nothing — was checked against every one of the seven Shape-B traces in
//!    `solr-ref/search-api/trace/` (00003-00008, 00021). All seven fit, and
//!    only this model was needed to explain all seven:
//!
//!    | trace | text after `}`          | numFound | model                                    |
//!    |-------|--------------------------|----------|-------------------------------------------|
//!    | 00006 | `+"quick"`               | 2        | edismax(`+"quick"`) -> docs 1,3            |
//!    | 00005 | `"quick" "rocket"`       | 2        | edismax(`"quick"`) OR id:"rocket" (no hit) |
//!    | 00007 | `"quick" "rocket"`       | 2        | same as 00005 (duplicate capture)          |
//!    | 00003 | `+"quick" +"rocket"`     | 0        | edismax(`+"quick"`) AND id:"rocket" -> 0    |
//!    | 00004 | `+"quick" +"fox"`        | 0        | same shape as 00003                        |
//!    | 00008 | `+"quick" +"fox"`        | 0        | duplicate capture of 00004                 |
//!    | 00021 | `+"qwick"`               | 0        | typo, edismax(`+"qwick"`) -> no hit         |
//!
//!    No trace contradicted the model, so no premise correction is needed.
//! 3. Consequence of (2): for Shape B, correct-per-Solr behaviour is mostly
//!    **low recall**. `local_params_edismax_two_mandatory_terms_returns_zero`
//!    and `local_params_edismax_mandatory_terms_quick_fox_returns_zero` below
//!    pin `numFound == 0` for exactly the two traces (00003, 00004/00008)
//!    where a "corrected", whole-remainder edismax would return 1 (document
//!    `entity:node/1` contains both "quick" and "fox"). A test asserting
//!    `numFound == 1` for those two would be asserting the *documented bug*,
//!    not the captured behaviour, and would poison this ticket exactly the
//!    way the task brief warned against. Per `CLAUDE.md`'s compatibility
//!    contract, fixtures are ground truth, so bug-compatible (option (a) in
//!    the issue) is what these tests assert; if a human later ratifies
//!    option (b) ("deliberately correct", a documented divergence), those
//!    two tests must be revised explicitly, not silently relaxed.
//! 4. Shape A (traces 00002, 00009 — expanded per-language fields, plain
//!    lucene, no local params) already returns `numFound == 2` today
//!    (verified by running these tests before any implementation): see
//!    `shape_a_*` below, which are regression pins, not new-behaviour tests.
//!
//! ## Corpus
//!
//! `search_api_docs()` transcribes the six documents from
//! `solr-ref/search-api/trace/00001.json`'s add-batch (the same corpus the
//! Shape A/B traces above were captured against) verbatim: `id`,
//! `tm_X3b_en_title`, `tm_X3b_en_body` only — the fields the `qf` in every
//! Shape B trace actually names. Indexed against `presets/search-api.toml`
//! (issue #58), so the same dynamic-field resolution applies as in
//! production. `tm_X3b_und_*` is deliberately absent from every doc, exactly
//! as captured — the `und` half of every `qf` matches nothing by corpus
//! design, not by omission here.
//!
//! ## What's genuinely red today, confirmed by running this file
//!
//! With zero local-params support, Wayfinder does not treat the whole Shape
//! B `q` string as harmless opaque text: `{`, `!`, and the single-quoted
//! `qf` value are not valid lucene query syntax to Wayfinder's parser, so
//! every Shape-B query below currently returns **HTTP 400** (`SyntaxError:
//! could not parse query ...`), not `200` with the wrong `numFound`. All six
//! `local_params_edismax_*` tests are genuinely red today for that reason
//! (confirmed by running `cargo test --test local_params` before writing
//! any implementation). The two that assert `numFound == 0`
//! (`two_mandatory_terms`, `mandatory_terms_quick_fox`) still matter as the
//! bug-compatibility guard from premise 3 even though they are not
//! "currently green by coincidence" as originally guessed: once a parser
//! exists at all, it becomes possible to implement the tempting-but-wrong
//! "apply edismax to the whole remainder" fix, and those two are exactly the
//! traces that fix would get wrong (it would return 1, not the real Solr /
//! fixture value of 0).

mod common;

use axum::http::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;

const PRESET_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/presets/search-api.toml");

fn preset_toml() -> String {
    std::fs::read_to_string(PRESET_PATH)
        .unwrap_or_else(|e| panic!("presets/search-api.toml must exist and be readable: {e}"))
}

/// Transcribed verbatim from `solr-ref/search-api/trace/00001.json`'s six
/// `add` documents (`entity:node/1` .. `entity:node/6`), keeping only the
/// fields the Shape A/B `qf`/query text below actually reference. Comments
/// note each doc's real `nid` for traceability back to the fixture.
fn search_api_docs() -> Value {
    json!([
        // entity:node/1 -- the decisive doc for premise 3: contains both
        // "quick" and "fox".
        {
            "id": "doc1",
            "tm_X3b_en_title": "The quick brown fox jumps over the lazy dog",
            "tm_X3b_en_body": "A classic pangram used to test typefaces and search relevance. The fox is quick and clever."
        },
        // entity:node/2
        {
            "id": "doc2",
            "tm_X3b_en_title": "A lazy afternoon in the garden",
            "tm_X3b_en_body": "Spent the afternoon reading in the garden. Bees buzzed lazily among the flowers."
        },
        // entity:node/3 -- contains "quick" and "rocket".
        {
            "id": "doc3",
            "tm_X3b_en_title": "Quick thinking saves the day at the rocket launch",
            "tm_X3b_en_body": "Engineers had to think quickly when the rocket launch sequence hit an anomaly."
        },
        // entity:node/4
        {
            "id": "doc4",
            "tm_X3b_en_title": "Dogs and cats living together",
            "tm_X3b_en_body": "A humorous look at household pets learning to coexist peacefully."
        },
        // entity:node/5
        {
            "id": "doc5",
            "tm_X3b_en_title": "About our mission",
            "tm_X3b_en_body": "We build search infrastructure and believe in open standards and interoperability."
        },
        // entity:node/6
        {
            "id": "doc6",
            "tm_X3b_en_title": "Archived legacy documentation",
            "tm_X3b_en_body": "This page documents a legacy system that has since been retired and archived."
        }
    ])
}

async fn search_api_app() -> (axum::Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), &preset_toml())
        .expect("presets/search-api.toml must build a working app");
    let (status, body) = common::post_docs(&app, &search_api_docs()).await;
    assert_eq!(status, StatusCode::OK, "index search-api corpus: {body}");
    (app, dir)
}

/// As `search_api_app`, but with `strict_params = true` -- open question 6:
/// local params live inside the `q` *value*, not a new param name, so the
/// `SELECT_PARAMS` allowlist should already cover this with no changes.
async fn search_api_app_strict() -> (axum::Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, preset_toml()).expect("write schema.toml");
    let config_path = dir.path().join("wayfinder.toml");
    std::fs::write(&config_path, "strict_params = true\n").expect("write config");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let app = wayfinder::app_with_config(&schema_path, &data_dir, &config_path)
        .expect("build strict app");
    let (status, body) = common::post_docs(&app, &search_api_docs()).await;
    assert_eq!(status, StatusCode::OK, "index search-api corpus: {body}");
    (app, dir)
}

fn num_found(body: &Value) -> Option<u64> {
    body.pointer("/response/numFound").and_then(Value::as_u64)
}

// -- Shape A: expanded per-language fields, plain lucene, no local params ----
// Regression pins -- these already pass today and must keep passing once a
// local-params parser lands; they exercise no local-params code at all.

#[tokio::test]
async fn shape_a_expanded_fields_with_plus_operator_returns_two_hits() {
    let (app, _dir) = search_api_app().await;
    // Trace 00002's exact q, percent-encoded verbatim from the captured
    // request path.
    let (status, body) = common::get(
        &app,
        "select?q=%28tm_X3b_en_body%3A%28%2B%22quick%22%29%5E1%20tm_X3b_und_body%3A%28%2B%22quick%22%29%5E1%20tm_X3b_en_title%3A%28%2B%22quick%22%29%5E1%20tm_X3b_und_title%3A%28%2B%22quick%22%29%5E1%29&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        num_found(&body),
        Some(2),
        "Shape A (trace 00002) must keep matching docs 1 and 3: {body}"
    );
}

#[tokio::test]
async fn shape_a_expanded_fields_without_plus_operator_returns_two_hits() {
    let (app, _dir) = search_api_app().await;
    // Trace 00009's exact q, percent-encoded verbatim from the captured
    // request path.
    let (status, body) = common::get(
        &app,
        "select?q=%28tm_X3b_en_title%3A%28%28quick%29%29%5E1%20tm_X3b_und_title%3A%28%28quick%29%29%5E1%20tm_X3b_en_body%3A%28%28quick%29%29%5E1%20tm_X3b_und_body%3A%28%28quick%29%29%5E1%29&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        num_found(&body),
        Some(2),
        "Shape A (trace 00009) must keep matching docs 1 and 3: {body}"
    );
}

// -- Shape B: inline `{!edismax qf='...'}` nested query ----------------------

/// Trace 00006: `q=({!edismax qf='...'}+"quick")`. Genuinely red today: the
/// whole string is currently opaque lucene text against `df`/`default_field`
/// `id`, giving `numFound == 0`. Once the local-params prefix is recognised
/// and bound to only the next whitespace-delimited token (`+"quick"`), the
/// inner edismax search over `qf` must return docs 1 and 3.
#[tokio::test]
async fn local_params_edismax_single_mandatory_term_matches_two_docs() {
    let (app, _dir) = search_api_app().await;
    let (status, body) = common::get(
        &app,
        "select?q=%28%7B%21edismax%20qf%3D%27tm_X3b_en_title%5E1%20tm_X3b_und_title%5E1%20tm_X3b_en_body%5E1%20tm_X3b_und_body%5E1%27%7D%2B%22quick%22%29&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        num_found(&body),
        Some(2),
        "trace 00006 must match docs 1 and 3 via the inline edismax clause: {body}"
    );
}

/// Trace 00005/00007: `q=({!edismax qf='...'}"quick" "rocket")`. Genuinely
/// red today (currently 0). The bound token is `"quick"` (no `+`, so a
/// SHOULD clause); the trailing `"rocket"` is parsed by the outer lucene
/// parser against `df`/`default_field` `id` and matches nothing, but being a
/// bare (non-`+`) clause it does not exclude the edismax hits either --
/// `numFound` stays 2, driven entirely by the edismax match on "quick".
#[tokio::test]
async fn local_params_edismax_bound_term_plus_unbound_remainder_matches_two_docs() {
    let (app, _dir) = search_api_app().await;
    let (status, body) = common::get(
        &app,
        "select?q=%28%7B%21edismax%20qf%3D%27tm_X3b_en_title%5E1%20tm_X3b_und_title%5E1%20tm_X3b_en_body%5E1%20tm_X3b_und_body%5E1%27%7D%22quick%22%20%22rocket%22%29&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        num_found(&body),
        Some(2),
        "trace 00005/00007 must match docs 1 and 3 via edismax(\"quick\"), \
         ignoring the trailing unbound \"rocket\" clause: {body}"
    );
}

/// Trace 00003: `q=({!edismax qf='...'}+"quick" +"rocket")`. Decisive
/// bug-compatibility case (premise 3): `numFound` must be **0**, matching
/// real Solr's inline-nested-query binding, not the 1 a whole-remainder
/// "corrected" edismax would return (doc 1 alone has "quick" but not
/// "rocket"; doc 3 has "rocket" but this doc doesn't match on quick+rocket
/// together in this corpus either way -- the point is that the *mandatory*
/// `id:"rocket"` clause outside the nested query matches nothing at all, so
/// the AND collapses the whole result to empty).
#[tokio::test]
async fn local_params_edismax_two_mandatory_terms_returns_zero() {
    let (app, _dir) = search_api_app().await;
    let (status, body) = common::get(
        &app,
        "select?q=%28%7B%21edismax%20qf%3D%27tm_X3b_en_title%5E1%20tm_X3b_und_title%5E1%20tm_X3b_en_body%5E1%20tm_X3b_und_body%5E1%27%7D%2B%22quick%22%20%2B%22rocket%22%29&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        num_found(&body),
        Some(0),
        "trace 00003 must return 0 hits, bug-compatible with real Solr's \
         inline-nested-query binding -- NOT the higher-recall count a \
         whole-remainder edismax would produce: {body}"
    );
}

/// Trace 00004/00008: `q=({!edismax qf='...'}+"quick" +"fox")`. THE decisive
/// case from the issue body: document `entity:node/1` (doc1 here) is titled
/// "The quick brown fox..." and its body also says "...quick and clever" --
/// a correctly-applied edismax over the whole remainder *would* match it
/// (numFound 1). Real Solr returns 0, because `+"fox"` never reaches
/// edismax at all -- it is parsed by the outer lucene parser against `id`
/// and excludes everything via the mandatory `+`. This is the single
/// clearest test in this file that would falsely go green under the
/// "obviously more correct" implementation the task brief warned against.
#[tokio::test]
async fn local_params_edismax_mandatory_terms_quick_fox_returns_zero() {
    let (app, _dir) = search_api_app().await;
    let (status, body) = common::get(
        &app,
        "select?q=%28%7B%21edismax%20qf%3D%27tm_X3b_en_title%5E1%20tm_X3b_und_title%5E1%20tm_X3b_en_body%5E1%20tm_X3b_und_body%5E1%27%7D%2B%22quick%22%20%2B%22fox%22%29&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        num_found(&body),
        Some(0),
        "trace 00004/00008 must return 0 hits even though doc1 contains \
         both \"quick\" and \"fox\" -- a numFound of 1 here means the \
         parser applied edismax to the whole remainder instead of \
         reproducing Solr's single-token binding: {body}"
    );
}

/// Trace 00021: `q=({!edismax qf='...'}+"qwick")` -- a typo with no match in
/// the corpus either way. Kept as a low-value but free additional guard.
#[tokio::test]
async fn local_params_edismax_typo_term_returns_zero() {
    let (app, _dir) = search_api_app().await;
    let (status, body) = common::get(
        &app,
        "select?q=%28%7B%21edismax%20qf%3D%27tm_X3b_en_title%5E1%20tm_X3b_und_title%5E1%20tm_X3b_en_body%5E1%20tm_X3b_und_body%5E1%27%7D%2B%22qwick%22%29&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(num_found(&body), Some(0), "trace 00021: {body}");
}

/// Round-2 review must-fix 1: the implementation rewrites each inline nested
/// query to an internal sentinel literal (`__wf_nested_query_N__`). A `q` that
/// contains that literal *itself* must not have the user's own text resolved to
/// a nested query. Real Solr has no such token: it parses
/// `+__wf_nested_query_0__` as an ordinary mandatory term against `df=id`,
/// which matches no document in this corpus, so the whole query returns **0**.
/// Before the fix this returned `numFound == 2` -- user-controlled input
/// changing which query ran.
///
/// The query is otherwise trace 00006's shape with a second, mandatory clause
/// appended, so a 2 here means the sentinel collided and the extra mandatory
/// clause was replaced by a copy of the edismax nested query instead of being
/// searched as a term.
#[tokio::test]
async fn local_params_sentinel_literal_in_user_query_is_not_resolved() {
    let (app, _dir) = search_api_app().await;
    let (status, body) = common::get(
        &app,
        "select?q=%28%7B%21edismax%20qf%3D%27tm_X3b_en_title%5E1%20tm_X3b_und_title%5E1%20tm_X3b_en_body%5E1%20tm_X3b_und_body%5E1%27%7D%2B%22quick%22%20%2B__wf_nested_query_0__%29&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        num_found(&body),
        Some(0),
        "the literal `__wf_nested_query_0__` must stay an ordinary outer-parser \
         term against `id` (no match), not resolve to the inline edismax \
         clause: {body}"
    );
}

/// Open question 6: local params live inside the `q` *value*; `strict_params
/// = true` must not 400 a request whose `q` happens to contain `{!edismax
/// ...}` syntax, because `q` is already an allowed param name -- nothing
/// about local-params parsing should touch `SELECT_PARAMS`. Also exercises
/// the single-quoted `qf` value with embedded spaces end-to-end under the
/// strict configuration.
#[tokio::test]
async fn local_params_in_q_value_does_not_400_under_strict_params() {
    let (app, _dir) = search_api_app_strict().await;
    let (status, body) = common::get(
        &app,
        "select?q=%28%7B%21edismax%20qf%3D%27tm_X3b_en_title%5E1%20tm_X3b_und_title%5E1%20tm_X3b_en_body%5E1%20tm_X3b_und_body%5E1%27%7D%2B%22quick%22%29&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "strict_params=true must not 400 on local-params syntax living \
         inside the q value: {body}"
    );
    assert_eq!(
        num_found(&body),
        Some(2),
        "and the parse must still produce the correct result under strict \
         params, same as the non-strict case (trace 00006): {body}"
    );
}

/// PRD §5's unsupported local-params boundary, end to end. The prose records a
/// *specific* shipped answer for a block naming any parser other than
/// `edismax`, `func`, or `boost`: HTTP 400 with a `wayfinder::SyntaxError` in
/// the Solr error envelope. Until this test the only coverage was
/// `src/local_params.rs`'s unit test on the message string, which says nothing
/// about status or envelope, so the documented boundary could have drifted to
/// a 500 or a silently empty 200 with every test green.
///
/// Per `tests/error_shapes.rs`'s narrow contract (and `tests/edismax.rs`'s
/// `mm_present_but_empty_400s_like_a_malformed_spec`, the closest precedent),
/// `error.msg` is free text and is never compared verbatim — only its presence.
/// The `root-error-class` *is* compared, because "a `SyntaxError` 400" is the
/// literal content of the documented unsupported boundary.
#[tokio::test]
async fn unrecognised_local_params_type_400s_with_a_syntax_error_envelope() {
    let (app, _dir) = search_api_app().await;
    // The types PRD §5's unsupported boundary names by hand, plus the type-less
    // block `src/local_params.rs`'s ceiling note rejects alongside them.
    // (`{!func}` and `{!boost}` are now implemented (#289), so they are not
    // in this unrecognised set.)
    for (encoded, shape) in [
        ("%7B%21lucene%7Dquick", "{!lucene}quick"),
        ("%7B%21term%20f%3Did%7Ddoc1", "{!term f=id}doc1"),
        (
            "%7B%21qf%3Dtm_X3b_en_title%7Dquick",
            "{!qf=tm_X3b_en_title}quick",
        ),
    ] {
        let (status, body) = common::get(&app, &format!("select?q={encoded}&wt=json")).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "PRD §5 records a hard 400 for unsupported `{shape}`, not a 500 and not a \
             silently-empty 200: {body}"
        );
        assert_eq!(
            body["error"]["code"].as_i64(),
            Some(400),
            "`{shape}` must carry error.code 400 in the body too, matching Solr's envelope: \
             {body}"
        );
        assert!(
            body["error"]["msg"].as_str().is_some_and(|s| !s.is_empty()),
            "`{shape}` must carry a non-empty error.msg (never compared verbatim): {body}"
        );
        let metadata = body["error"]["metadata"]
            .as_array()
            .unwrap_or_else(|| panic!("`{shape}`: error.metadata must be a flat array: {body}"));
        let pairs: Vec<&str> = metadata.iter().filter_map(Value::as_str).collect();
        assert!(
            pairs.contains(&"error-class") && pairs.contains(&"root-error-class"),
            "`{shape}`: error.metadata must carry the same key shape as Solr's: {body}"
        );
        let root = pairs
            .iter()
            .position(|k| *k == "root-error-class")
            .and_then(|i| pairs.get(i + 1));
        assert_eq!(
            root,
            Some(&"wayfinder::SyntaxError"),
            "`{shape}`: PRD §5 records a *SyntaxError* 400 specifically -- a different error \
             class means the unsupported-boundary statement no longer describes what ships: {body}"
        );
    }
}

// --- Documentation guards for issue #137's recorded decisions --------------
//
// Issue #137's acceptance criteria require two decisions to be *recorded*, not
// merely implemented: the Shape-B bug-compatible reading and the unsupported
// parser-type hard 400 (both PRD §5, with only the former client-exercised).
// These are tripwires against that prose being dropped or reverted.
//
// ponytail: keyword co-occurrence scoped to one blank-line-separated block,
// exactly like `tests/edismax_descope_guard.rs`. Scoping to a block rules out
// an unrelated passage elsewhere supplying a token, but cannot distinguish a
// claim from its negation. Correctness of the wording is a review
// responsibility; these assert the claim is still on the page.

const PRD: &str = include_str!("../docs/PRD.md");

/// The blank-line-separated blocks of the PRD section starting at `heading`,
/// up to (but not including) the next heading of the same level.
fn prd_blocks(heading: &str) -> Vec<String> {
    let start = PRD
        .find(heading)
        .unwrap_or_else(|| panic!("docs/PRD.md must still contain the `{heading}` section"));
    let rest = &PRD[start..];
    let level: String = format!("\n{} ", heading.split_whitespace().next().unwrap());
    let end = rest[1..].find(&level).map(|i| i + 1).unwrap_or(rest.len());
    rest[..end]
        .split("\n\n")
        .map(|b| b.trim().to_lowercase())
        .filter(|b| !b.is_empty())
        .collect()
}

#[test]
fn prd_records_the_shape_b_bug_compatible_decision() {
    let blocks = prd_blocks("### v1 exception — edismax");
    let binding = blocks
        .iter()
        .find(|b| b.contains("#137") && b.contains("numfound: 0"))
        .unwrap_or_else(|| {
            panic!(
                "issue #137's acceptance criterion 1 requires PRD §5 to record the Shape-B \
                 decision: no single block in the v1 edismax section ties issue #137 to the \
                 reproduced `numFound: 0` outcome"
            )
        });
    assert!(
        binding.contains("deftype=lucene") && binding.contains("df=id"),
        "the Shape-B block must name why the outer parser is lucene (the captured handler \
         defaults `defType=lucene`, `df=id`), otherwise the `numFound: 0` outcome reads as a \
         Wayfinder bug rather than reproduced Solr behaviour"
    );
    assert!(
        binding.contains("00004") || binding.contains("00008"),
        "the Shape-B block must cite the decisive traces (00004/00008), whose document contains \
         both terms yet returns 0"
    );
    assert!(
        binding.contains("finding 90") || binding.contains("findings 90"),
        "the Shape-B block must cite the findings carrying the evidence (90, 91, 92) rather than \
         restating it"
    );
    let stance = blocks
        .iter()
        .find(|b| b.contains("divergence") && b.contains("fidelity"))
        .unwrap_or_else(|| {
            panic!(
                "PRD §5 must state plainly that reproducing Shape B's low recall is deliberate \
                 fidelity and that the high-recall reading would be a divergence needing \
                 ratification — no block ties those two together"
            )
        });
    assert!(
        stance.contains("wrong-premised") || stance.contains("wrong premised"),
        "PRD §5 must record that issue #137's own title (\"so keyword search works\") is \
         wrong-premised: search does not start working, it fails the way real Solr fails"
    );
}

#[test]
fn prd_records_the_hard_400_on_unsupported_local_params_types() {
    let blocks = prd_blocks("### v1 exception — edismax");
    let entry = blocks
        .iter()
        .find(|b| b.contains("{!lucene}") && b.contains("400"))
        .unwrap_or_else(|| {
            panic!(
                "issue #137's open question 5 is an unsupported boundary: PRD §5 must record \
                 Wayfinder 400ing local-params blocks naming an unsupported parser, although \
                 real Solr parses registered types such as lucene"
            )
        });
    assert!(
        entry.contains("not a regression"),
        "the scope statement must say accurately that this is **not a regression**: origin/main already \
         400s these because Tantivy's grammar rejects the raw `{{!` string, and issue #137 changed \
         only the error message"
    );
    assert!(
        entry.contains("{!func}") && entry.contains("#289"),
        "the scope statement must reflect that `{{!func}}`/`{{!boost}}` landed in #289 (a real evaluator) \
         and no longer silently half-work or belong to v4"
    );
    assert!(
        entry.contains("#137"),
        "the entry must cite issue #137 so a future reader can find the decision behind it"
    );
}
