//! Issue #340 — `{!payload_score}` + the `boost_term_payload` field type.
//!
//! Unit-level coverage of the parser and the error mapping (spec section C),
//! the schema field type round-tripping a multi-valued `boost_term` through
//! the update pipeline, the position-0 vs inline asymmetry (finding 168), and
//! `v` normalization (quotes, case). The wire-compatibility evidence for
//! every score value below is the same committed fixture
//! (`solr-ref/responses/pls_*.json`) that backs the `pls_*` rows replayed by
//! `tests/differential.rs`'s dedicated `pls_app` — this file duplicates a
//! handful of those numbers deliberately, as direct assertions rather than a
//! fixture diff, because a differential-only red gives no useful signal about
//! *why* a query 400s before the feature exists (see the module doc's
//! `PLS_SCHEMA_TOML` comment there).
//!
//! Premises (from the task spec, already verified against a real `solr:9`
//! before this file was written — do not re-litigate):
//! - `includeSpanScore` defaults to `false`; a `{!payload_score}` score is the
//!   raw payload value with no BM25 factor, so scores are exactly comparable.
//! - Index-time `boost_term` values are always single `<term>|<boost>`
//!   tokens; multi-term `v` is a named descope.
//! - The client emits `{!payload_score}` inline, never at position 0 — a
//!   position-0 block swallows the rest of `q` and discards it (finding 168).

mod common;

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;

use common::{app_with_schema, get, post_docs};

/// A `content` core with a `boost_term_payload` `boost_term` field, mirroring
/// the module's own field type (`solr-conf-templates/9.x/schema.xml:387-406`):
/// whitespace tokenizer, LengthFilter min=2/max=100, lowercase,
/// RemoveDuplicates, then a delimited payload split on the last `|` with a
/// float payload. `boost_term` is `multiValued` per the module's own schema.
const PLS_SCHEMA_TOML: &str = r#"
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
type = "text_general"
stored = true

[[fields]]
name = "boost_document"
type = "float"
stored = true
fast = true

[[fields]]
name = "boost_term"
type = "boost_term_payload"
stored = false
multi_valued = true
"#;

/// The same 5-doc corpus `solr-ref/capture.sh`'s pls block indexes (and
/// `tests/differential.rs::pls_corpus` mirrors byte-for-byte): d3 carries
/// `dog` twice with different payloads (1.5, 4.5) -- the only way
/// min/max/average/sum are distinguishable from each other -- and d4 has no
/// `boost_term` at all.
fn pls_corpus() -> Value {
    json!([
        {"id":"d1","body":"quick brown fox","boost_document":1.0,"boost_term":["fox|2.0","brown|1.5"]},
        {"id":"d2","body":"lazy dog","boost_document":1.0,"boost_term":["dog|3.0"]},
        {"id":"d3","body":"quick dog","boost_document":2.0,"boost_term":["dog|1.5","dog|4.5","quick|2.5"]},
        {"id":"d4","body":"quick fox","boost_document":0.5},
        {"id":"d5","body":"lazy brown","boost_document":1.0,"boost_term":["brown|4.0","lazy|0.5"]}
    ])
}

async fn pls_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = app_with_schema(dir.path(), PLS_SCHEMA_TOML).expect("pls app must build");
    let (status, body) = post_docs(&app, &pls_corpus()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the pls corpus must succeed, got {body}"
    );
    (app, dir)
}

fn doc_scores(body: &Value) -> Vec<(String, f64)> {
    body["response"]["docs"]
        .as_array()
        .unwrap_or_else(|| panic!("no response.docs array: {body}"))
        .iter()
        .map(|d| {
            (
                d["id"].as_str().expect("id").to_string(),
                d["score"].as_f64().expect("score"),
            )
        })
        .collect()
}

fn assert_scores_close(actual: &[(String, f64)], expected: &[(&str, f64)], ctx: &str) {
    let actual_ids: Vec<&str> = actual.iter().map(|(id, _)| id.as_str()).collect();
    let expected_ids: Vec<&str> = expected.iter().map(|(id, _)| *id).collect();
    assert_eq!(actual_ids, expected_ids, "{ctx}: doc id order mismatch");
    for ((_, got), (id, want)) in actual.iter().zip(expected.iter()) {
        assert!(
            (got - want).abs() < 1e-3,
            "{ctx}: {id} score {got} not within 1e-3 of expected {want}"
        );
    }
}

// --- A. schema: boost_term_payload round-trips a multi-valued field --------

/// The field type must exist and accept `multiValued` values through the
/// normal update pipeline: d1/d3/d5 each carry 2-3 `boost_term` values, d4
/// carries none at all. If indexing 400s or 500s here, the field type is not
/// registered (fixture: none needed, this is a wiring precondition every
/// other test in this file relies on).
#[tokio::test]
async fn boost_term_payload_field_indexes_a_multivalued_field() {
    let (_app, _dir) = pls_app().await;
}

// --- B/C combined: the four payload functions match the captured fixtures -

/// `(fixture_name, func, expected_id_score_pairs)`.
type FuncCase<'a> = (&'a str, &'a str, &'a [(&'a str, f64)]);

/// `pls_max`/`pls_min`/`pls_average`/`pls_sum`: only d3 (payloads 1.5, 4.5)
/// distinguishes the four functions; d2's single 3.0 payload is the control
/// that scores 3.0 under all of them. Values from `solr-ref/responses/pls_
/// {max,min,average,sum}.json`.
#[tokio::test]
async fn payload_score_max_min_average_sum_match_captured_fixtures() {
    let (app, _dir) = pls_app().await;
    let cases: &[FuncCase] = &[
        ("pls_max", "max", &[("d3", 4.5), ("d2", 3.0)]),
        ("pls_min", "min", &[("d2", 3.0), ("d3", 1.5)]),
        ("pls_average", "average", &[("d2", 3.0), ("d3", 3.0)]),
        ("pls_sum", "sum", &[("d3", 6.0), ("d2", 3.0)]),
    ];
    for (name, func, expected) in cases {
        let qs = format!(
            "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22dog%22%20func%3D{func}%7D&fl=id,score&sort=score%20desc,id%20asc&wt=json"
        );
        let (status, body) = get(&app, &qs).await;
        assert_eq!(status, StatusCode::OK, "{name}: {body}");
        assert_scores_close(&doc_scores(&body), expected, name);
    }
}

/// `pls_span_false`: `includeSpanScore=false` is explicit rather than
/// defaulted, and must score identically to `pls_max`'s bare form (finding
/// 165 — no BM25 factor either way).
#[tokio::test]
async fn payload_score_explicit_span_score_false_matches_the_default() {
    let (app, _dir) = pls_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22dog%22%20func%3Dmax%20includeSpanScore%3Dfalse%7D&fl=id,score&sort=score%20desc,id%20asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_scores_close(
        &doc_scores(&body),
        &[("d3", 4.5), ("d2", 3.0)],
        "pls_span_false",
    );
}

/// `pls_unmatched`: a `v` with no matching payload term returns zero hits,
/// not an error.
#[tokio::test]
async fn payload_score_unmatched_v_returns_zero_hits() {
    let (app, _dir) = pls_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22nosuch%22%20func%3Dmax%7D&fl=id,score&sort=score%20desc,id%20asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["response"]["numFound"].as_i64(),
        Some(0),
        "pls_unmatched: {body}"
    );
}

// --- `v` normalization: quotes and case (finding 167) ----------------------

/// `v="dog"`, bare `v=dog`, and `v="DOG"` are all the same query — `v` is
/// analyzed by the field type, so the surrounding quotes `escapePhrase()`
/// adds are stripped and the value is lowercased. Fixtures: `pls_v_unquoted`,
/// `pls_v_upper` (both identical to `pls_max`'s d3 4.5 / d2 3.0).
#[tokio::test]
async fn payload_score_v_normalizes_quotes_and_case() {
    let (app, _dir) = pls_app().await;
    let variants = [
        ("pls_v_unquoted", "v%3Ddog"),
        ("pls_v_upper", "v%3D%22DOG%22"),
        ("pls_max_quoted_lower", "v%3D%22dog%22"),
    ];
    for (name, v_param) in variants {
        let qs = format!(
            "select?q=%7B%21payload_score%20f%3Dboost_term%20{v_param}%20func%3Dmax%7D&fl=id,score&sort=score%20desc,id%20asc&wt=json"
        );
        let (status, body) = get(&app, &qs).await;
        assert_eq!(status, StatusCode::OK, "{name}: {body}");
        assert_scores_close(&doc_scores(&body), &[("d3", 4.5), ("d2", 3.0)], name);
    }
}

// --- position-0 vs inline asymmetry (finding 168) --------------------------

/// `pls_two_terms`: two `{!payload_score}` blocks with nothing in front of
/// them. The first holds position 0 in `parse_function_query_q`, so it sets
/// the parser for the whole `q` and the second block is discarded verbatim,
/// not parsed as a child query — the result is identical to querying the
/// first block alone (`pls_max`'s d3 4.5 / d2 3.0), not d3's summed 4.5+2.5.
#[tokio::test]
async fn payload_score_position_zero_discards_a_second_trailing_block() {
    let (app, _dir) = pls_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22dog%22%20func%3Dmax%7D%20%7B%21payload_score%20f%3Dboost_term%20v%3D%22quick%22%20func%3Dmax%7D&fl=id,score&sort=score%20desc,id%20asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_scores_close(
        &doc_scores(&body),
        &[("d3", 4.5), ("d2", 3.0)],
        "pls_two_terms",
    );
}

/// `pls_client_shape`: the exact string `SearchApiSolrBackend::preQuery`
/// assembles — `{!boost b=boost_document}` holds position 0, so the two
/// trailing `{!payload_score}` blocks are inline SHOULD clauses of the
/// `{!boost}` child that the lucene parser sums, not discarded. Per the
/// spec's arithmetic: d3 = (4.5+2.5+1.0)*2.0 = 16.0 (the +1.0 is the
/// default-scored `*:*` clause); d2 = (3.0+1.0)*1.0 = 4.0; d1/d5 = 0+1.0 =
/// 1.0; d4 = 1.0*0.5 = 0.5 (no boost_term at all, so both payload_score
/// clauses contribute 0).
#[tokio::test]
async fn payload_score_inline_after_boost_sums_the_clauses() {
    let (app, _dir) = pls_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21boost%20b%3Dboost_document%7D%20%7B%21payload_score%20f%3Dboost_term%20v%3D%22dog%22%20func%3Dmax%7D%20%7B%21payload_score%20f%3Dboost_term%20v%3D%22quick%22%20func%3Dmax%7D%20*%3A*&fl=id,score&sort=score%20desc,id%20asc&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["response"]["numFound"].as_i64(), Some(5), "{body}");
    assert_scores_close(
        &doc_scores(&body),
        &[
            ("d3", 16.0),
            ("d2", 4.0),
            ("d1", 1.0),
            ("d5", 1.0),
            ("d4", 0.5),
        ],
        "pls_client_shape",
    );
}

// --- C. error mapping: all 400, `error.code: 400` --------------------------

/// One assertion per named error condition in the spec's table. `func`'s
/// exact wording is asserted (not just status) because the differential
/// harness drops `error.msg` — this file is the only place those exact wire
/// messages are pinned. `f` naming a real, non-payload field is intentionally
/// excluded here: it is a *permanent* documented divergence from Solr's own
/// 500 (spec section D), asserted in `tests/differential.rs`'s manifest-errors
/// run via `ACCEPTED_DIVERGENCES`, not here.
#[tokio::test]
async fn payload_score_error_messages_match_the_spec_table() {
    let (app, _dir) = pls_app().await;
    let cases: &[(&str, &str)] = &[
        (
            "select?q=%7B%21payload_score%20v%3D%22dog%22%20func%3Dmax%7D&fl=id,score&wt=json",
            "'f' not specified",
        ),
        (
            "select?q=%7B%21payload_score%20f%3Dboost_term%20func%3Dmax%7D&fl=id,score&wt=json",
            "SpanQuery is null",
        ),
        (
            "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22a%22%20func%3Dmax%7D&fl=id,score&wt=json",
            "SpanQuery is null",
        ),
        (
            "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22dog%22%7D&fl=id,score&wt=json",
            "Unknown payload function: null",
        ),
        (
            "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22dog%22%20func%3Dbogus%7D&fl=id,score&wt=json",
            "Unknown payload function: bogus",
        ),
        (
            "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22dog%22%20func%3DMAX%7D&fl=id,score&wt=json",
            "Unknown payload function: MAX",
        ),
        (
            "select?q=%7B%21payload_score%20f%3Dnosuchfield%20v%3D%22dog%22%20func%3Dmax%7D&fl=id,score&wt=json",
            "undefined field nosuchfield",
        ),
    ];
    for (qs, expected_msg) in cases {
        let (status, body) = get(&app, qs).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{qs}: expected 400, got {status}: {body}"
        );
        assert_eq!(
            body["error"]["code"].as_i64(),
            Some(400),
            "{qs}: error.code must be 400: {body}"
        );
        // Solr's own message for `func=null`/`func=bogus`/`func=MAX` carries an
        // `org.apache.solr.search.SyntaxError: ` prefix ahead of the sentence
        // asserted here (see `pls_err_no_func.json` etc.); this checks the
        // sentence is present as a substring rather than pinning the prefix,
        // since the prefix is a Java exception class name, not part of the
        // wire contract the differential harness itself compares (it drops
        // `error.msg` entirely).
        let got_msg = body["error"]["msg"].as_str().unwrap_or_default();
        assert!(
            got_msg.contains(expected_msg),
            "{qs}: expected error.msg to contain {expected_msg:?}, got {got_msg:?}"
        );
    }
}

/// `func` is matched literally: `func=MAX` is an error (`Unknown payload
/// function: MAX`), not silently normalized to `max`. This is the guard
/// spec section C names for mutation testing: flip the comparison to
/// case-insensitive and this test must catch it.
#[tokio::test]
async fn payload_score_func_case_is_literal_not_normalized() {
    let (app, _dir) = pls_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22dog%22%20func%3DMAX%7D&fl=id,score&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "func=MAX must 400, not silently match func=max: {body}"
    );
    let got_msg = body["error"]["msg"].as_str().unwrap_or_default();
    assert!(
        got_msg.contains("Unknown payload function: MAX"),
        "expected the literal-case rejection message, got {got_msg:?}"
    );
}

/// `v` analyzing to empty (below the field type's `min=2` LengthFilter) is
/// the same `SpanQuery is null` 400 as `v` being entirely absent — not a
/// silent no-op query that matches everything, and not a distinct message.
/// This is the second spec-C mutation-testing guard: drop the length check
/// and `v="a"` would either match nothing distinctly (wrong message) or match
/// everything (far worse).
#[tokio::test]
async fn payload_score_v_below_min_length_is_span_query_is_null() {
    let (app, _dir) = pls_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22a%22%20func%3Dmax%7D&fl=id,score&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let got_msg = body["error"]["msg"].as_str().unwrap_or_default();
    assert!(
        got_msg.contains("SpanQuery is null"),
        "expected the analyzes-to-empty rejection message, got {got_msg:?}"
    );
}

/// A real, non-payload field (`f=body`, a plain analyzed text field) is a
/// declared, permanent divergence from Solr's upstream 500/NPE (spec section
/// D): Wayfinder answers 400. Only status is pinned here — the wording is
/// Wayfinder's own ("your wording" per the spec table) — and it must not be
/// the same generic "unsupported local-params query parser" 400 every
/// unimplemented parser currently produces, which would mean the parser was
/// never really implemented for the payload_score name at all.
#[tokio::test]
async fn payload_score_on_a_non_payload_field_is_400_not_500() {
    let (app, _dir) = pls_app().await;
    let (status, body) = get(
        &app,
        "select?q=%7B%21payload_score%20f%3Dbody%20v%3D%22dog%22%20func%3Dmax%7D&fl=id,score&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let got_msg = body["error"]["msg"].as_str().unwrap_or_default();
    assert!(
        !got_msg.contains("unsupported local-params query parser"),
        "a real payload_score implementation must not fall through to the generic \
         unsupported-parser message for a non-payload field: {got_msg:?}"
    );
}

// --- payload-free occurrences and the block boost ---------------------------

/// A three-doc corpus over the same schema, mirroring `capture.sh`'s `plsz`
/// core (finding 172): z1's only `dog` occurrence is a *bare* token, z2 is the
/// payloaded control, and z3 carries both forms of `cat`.
/// Fixture name, `v`, `func`, expected `(id, score)` order — the same shape as
/// [`FuncCase`], plus the `v` these rows vary.
type PayloadFreeCase<'a> = (&'a str, &'a str, &'a str, &'a [(&'a str, f64)]);

async fn plsz_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = app_with_schema(dir.path(), PLS_SCHEMA_TOML).expect("plsz app must build");
    let corpus = json!([
        {"id":"z1","boost_term":["dog"]},
        {"id":"z2","boost_term":["dog|3.0"]},
        {"id":"z3","boost_term":["cat","cat|2.0"]}
    ]);
    let (status, body) = post_docs(&app, &corpus).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the plsz corpus must succeed, got {body}"
    );
    (app, dir)
}

/// `plsz_bare_*` / `plsz_mixed_*`: a payload-free occurrence contributes the
/// factor `1.0` — Solr's `PayloadDecoder` decodes a null payload to `1f`
/// rather than skipping the position (finding 172).
///
/// The `mixed_min` case is the one that actually discriminates: z3 aggregates
/// `[1.0, 2.0]`, so skipping the bare occurrence would give `2.0` and
/// contributing gives `1.0`. Scoring the bare occurrence `0.0` instead — the
/// natural but wrong reading — would drop `bare_*` z1 to 0.0 and `mixed_min`
/// to 0.0 as well.
#[tokio::test]
async fn payload_free_occurrence_contributes_one() {
    let (app, _dir) = plsz_app().await;
    let cases: &[PayloadFreeCase] = &[
        ("plsz_bare_max", "dog", "max", &[("z2", 3.0), ("z1", 1.0)]),
        ("plsz_bare_min", "dog", "min", &[("z2", 3.0), ("z1", 1.0)]),
        (
            "plsz_bare_average",
            "dog",
            "average",
            &[("z2", 3.0), ("z1", 1.0)],
        ),
        ("plsz_bare_sum", "dog", "sum", &[("z2", 3.0), ("z1", 1.0)]),
        ("plsz_mixed_max", "cat", "max", &[("z3", 2.0)]),
        ("plsz_mixed_min", "cat", "min", &[("z3", 1.0)]),
        ("plsz_mixed_average", "cat", "average", &[("z3", 1.5)]),
        ("plsz_mixed_sum", "cat", "sum", &[("z3", 3.0)]),
    ];
    for (name, v, func, expected) in cases {
        let qs = format!(
            "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22{v}%22%20func%3D{func}%7D&fl=id,score&sort=score%20desc,id%20asc&wt=json"
        );
        let (status, body) = get(&app, &qs).await;
        assert_eq!(status, StatusCode::OK, "{name}: {body}");
        assert_scores_close(&doc_scores(&body), expected, name);
    }
}

/// A `^n` on an **inline** `{!payload_score}` clause must multiply the payload
/// aggregate.
///
/// The clause must be **parenthesised** for the boost to reach the query, and
/// that is a property of the local-params extractor, not of this feature:
/// `local_params::bound_token_len` ends a block's bound token only at
/// whitespace or `)`, so in `{!payload_score ...}^2` the `^2` is swallowed into
/// the bound token and then discarded with it, while in
/// `({!payload_score ...})^2` it lands outside and the grammar parses it as a
/// boost. (At position 0 the `^2` is likewise swallowed, there as part of the
/// remainder a position-0 block discards — finding 168.) Only the
/// parenthesised shape is asserted here; the bare-`^2` shape is an
/// extractor-wide ceiling shared with `{!edismax}` and has no fixture either
/// way, so this test pins the path that does reach the scorer rather than
/// blessing the one that does not.
///
/// No fixture: the module never boosts a payload_score clause, so this pins an
/// internal invariant rather than a captured wire value. It is a real
/// regression guard all the same — `PayloadScoreScorer` deliberately never
/// reads its child's score, so forwarding the Tantivy weight boost into the
/// child (the obvious wiring, and what `BoostQuery` does for every other
/// query) silently drops it.
///
/// Asserted as a ratio against the unboosted form rather than as absolute
/// numbers, so the guard is exactly "the boost is applied" and does not also
/// encode the `*:*`-plus-clause sum that `pls_client_shape` already covers.
#[tokio::test]
async fn inline_payload_score_block_boost_multiplies_the_aggregate() {
    let (app, _dir) = pls_app().await;
    let tail = "&fl=id,score&sort=score%20desc,id%20asc&wt=json";
    let clause = "%7B%21payload_score%20f%3Dboost_term%20v%3D%22dog%22%20func%3Dmax%7D";
    let mut scores = Vec::new();
    for suffix in ["", "%5E2"] {
        let qs = format!("select?q=*%3A*%20%28{clause}%29{suffix}{tail}");
        let (status, body) = get(&app, &qs).await;
        assert_eq!(status, StatusCode::OK, "inline boost {suffix:?}: {body}");
        scores.push(doc_scores(&body));
    }
    // `*:*` contributes a constant 1.0 to every document, so the payload part
    // of a score is `score - 1.0`. d3's max payload is 4.5 and d2's is 3.0.
    for (id, payload) in [("d3", 4.5f64), ("d2", 3.0)] {
        let plain = scores[0]
            .iter()
            .find(|(d, _)| d == id)
            .unwrap_or_else(|| panic!("{id} missing from unboosted run: {:?}", scores[0]))
            .1;
        let boosted = scores[1]
            .iter()
            .find(|(d, _)| d == id)
            .unwrap_or_else(|| panic!("{id} missing from boosted run: {:?}", scores[1]))
            .1;
        assert!(
            (plain - (1.0 + payload)).abs() < 1e-3,
            "{id}: unboosted inline score {plain} should be 1.0 + {payload}"
        );
        assert!(
            (boosted - (1.0 + 2.0 * payload)).abs() < 1e-3,
            "{id}: ^2 inline score {boosted} should be 1.0 + 2*{payload}, so the \
             block boost reaches the payload aggregate"
        );
    }
}
