//! Differential harness (issue #1, PRD §8): runs the query set in
//! `solr-ref/manifest.tsv` and diffs the response against a known-good
//! side, failing on any difference outside the explicit, logged normaliser
//! in `tests/common/diff.rs`.
//!
//! Two modes:
//! - **Hermetic (default, plain `cargo test`):** every manifest entry
//!   against an in-process Wayfinder (`common::indexed_app`), diffed against
//!   the committed fixture in `solr-ref/responses/`. No network, no Docker.
//! - **Live (`WAYFINDER_DIFF_SOLR=1 cargo test --test differential`):** same
//!   query set, same differ, expected side comes from a live Solr over HTTP.
//!   Requires `solr-ref/capture.sh` to have been run first (leaves the
//!   container up with schema + corpus already loaded) — this harness does
//!   not reimplement docker orchestration. Base URL from
//!   `WAYFINDER_DIFF_SOLR_URL`, default `http://localhost:8983/solr/content`.
//!   Gated by the env var alone (not `#[ignore]` as well), so it stays a
//!   plain `#[test]` that no-ops under default `cargo test`.

mod common;

use std::path::{Path, PathBuf};

use axum::Router;
use axum::http::StatusCode;
use common::diff::{
    ManifestEntry, ManifestErrorEntry, RankedDoc, diff, diff_ranked_ids, fetch_live_full,
    fetch_live_status, live_reachable, load_manifest, load_manifest_errors, normalize, ranked_docs,
    score_tolerance,
};
use common::key_order::fixture_text;
use common::{fixture, get, indexed_app, post_docs, request_full};
use serde_json::{Value, json};
use tempfile::TempDir;

fn manifest_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("solr-ref/manifest.tsv")
}

fn manifest_errors_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("solr-ref/manifest-errors.tsv")
}

// --- duplicated schema/corpus for manifest-errors.tsv's `facets`/`keyorder`
// rows (same precedent tests/json_key_order.rs documents for its own copies:
// tests/common/ is compiled once per integration-test binary, so sharing
// these across binaries is not straightforward, and this file needs its own
// in-process apps to run those rows hermetically). Every Wayfinder test app
// names its core `content` — the Solr-side core name (`facets`/`keyorder`)
// only ever appears in the manifest-errors row's URL, which the runner below
// rewrites before issuing the request.

const FACETS_SCHEMA_TOML: &str = r#"
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
name = "views"
type = "int"
stored = true
fast = true

[[fields]]
name = "created"
type = "date"
stored = true
fast = true

[[fields]]
name = "note"
type = "string"
stored = true
"#;

fn facets_corpus() -> Value {
    json!([
        {"id":"r1","views":5, "created":"2020-01-02T00:00:00Z","note":"alpha"},
        {"id":"r2","views":15,"created":"2020-01-03T00:00:00Z","note":"beta"},
        {"id":"r3","views":25,"created":"2020-01-03T00:00:00Z","note":"alpha"},
        {"id":"r4","views":35,"created":"2020-01-05T00:00:00Z"}
    ])
}

async fn facets_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), FACETS_SCHEMA_TOML).expect("app must build");
    let (status, body) = post_docs(&app, &facets_corpus()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the facets corpus must succeed, got {body}"
    );
    (app, dir)
}

const KEYORDER_SCHEMA_TOML: &str = r#"
[core]
name = "content"
unique_key = "id"
default_field = "id"

[[fields]]
name = "id"
type = "string"
stored = true
required = true
fast = true

[[fields]]
name = "views"
type = "int"
stored = true
fast = true

[[fields]]
name = "tag"
type = "string"
stored = true
fast = true
multi_valued = true
"#;

fn keyorder_corpus() -> Value {
    json!([
        {"id":"k1","views":5,  "tag":["zebra","apple"]},
        {"id":"k2","views":15, "tag":["zebra","apple"]},
        {"id":"k3","views":45, "tag":["zebra","mango"]},
        {"id":"k4","views":95, "tag":["zebra","apple"]},
        {"id":"k5","views":105,"tag":["mango","banana"]},
        {"id":"k6","views":155,"tag":["apple"]},
        {"id":"k7","views":195,"tag":["apple"]},
        {"id":"k8","views":125,"tag":["zebra"]}
    ])
}

async fn keyorder_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), KEYORDER_SCHEMA_TOML).expect("app must build");
    let (status, body) = post_docs(&app, &keyorder_corpus()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the key-order corpus must succeed, got {body}"
    );
    (app, dir)
}

/// Ratified, **permanent** divergences from captured Solr behaviour — the
/// opposite of `EXPECTED_DIVERGENCES` below, which is a self-expiring to-do
/// list for unbuilt features. Every entry here cites the PRD/findings
/// section that ratifies it (findings doc's "Expected-divergence list"
/// section explains the distinction). Printed during the manifest-errors
/// run so the accepted set is visible, not silent.
const ACCEPTED_DIVERGENCES: &[(&str, &str)] = &[
    (
        "err_missing_core",
        "finding 15 / PRD ratified-divergence 1: an unknown core is Solr's 404 HTML easter \
         egg, Wayfinder's normal 404 JSON error envelope — status only, body deliberately \
         not JSON on the fixture side",
    ),
    (
        "update_unknown_field_schemaless",
        "PRD ratified-divergence 3: Wayfinder has no schemaless mode and no \
         schemaless_probe core; hermetically this 404s where the fixture is 200",
    ),
    (
        "facet_non_docvalues_text",
        "finding 16 / PRD ratified-divergence 2: Wayfinder 400s a facet on an unfacetable \
         (non-docValues) field where Solr 200s with empty counts",
    ),
    (
        "facet_non_docvalues_text_enum",
        "finding 16 / PRD ratified-divergence 2, facet.method=enum variant of the same field",
    ),
    (
        "facet_stored_only_field",
        "finding 16 / PRD ratified-divergence 2, stored-only (non-indexed) field variant",
    ),
];

fn accepted_divergence_reason(name: &str) -> Option<&'static str> {
    ACCEPTED_DIVERGENCES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, reason)| *reason)
}

/// `manifest-errors.tsv`'s own self-expiring to-do list — the counterpart of
/// `EXPECTED_DIVERGENCES` below for `manifest.tsv`, but scoped to this file's
/// runner since the two loops have different app-selection and check logic.
/// Found while wiring the manifest-errors runner into the real differ for the
/// first time (issue #31): `facet_unknown_field` is a 400 on both sides (same
/// `error.code`, same class) but Wayfinder's actual response has no
/// `response` key at all, while Solr's fixture carries both `response` (the
/// base query, computed before the facet field is parsed) and `error` — a
/// genuine gap in `src/lib.rs::select`'s error path (it `?`-propagates the
/// facet error before the `response` json! literal is built), confirmed
/// against the live canonical container, not a harness bug and not a ratified
/// permanent divergence like `ACCEPTED_DIVERGENCES` above. Filed as issue #35;
/// this entry expires (and the guard below fails, naming it for deletion) the
/// moment that lands.
const EXPECTED_DIVERGENCES_MANIFEST_ERRORS: &[(&str, &str)] = &[(
    "facet_unknown_field",
    "Wayfinder's facet-field error propagates before the response block is built, so the \
     `response` key Solr's fixture carries alongside `error` is absent here — see issue #35",
)];

fn expected_divergence_manifest_errors_reason(name: &str) -> Option<&'static str> {
    EXPECTED_DIVERGENCES_MANIFEST_ERRORS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, reason)| *reason)
}

// --- normaliser: dropped fields pass and are logged -----------------------

#[test]
fn normalize_drops_qtime_and_logs_touched_path() {
    let v = json!({"responseHeader": {"status": 0, "QTime": 42}});
    let n = normalize(v);

    assert!(
        n.value["responseHeader"].get("QTime").is_none(),
        "QTime must be dropped"
    );
    assert!(
        n.touched.contains(&"responseHeader.QTime".to_string()),
        "dropping QTime must be recorded in touched paths, got {:?}",
        n.touched
    );
}

#[test]
fn differing_qtime_does_not_appear_as_a_diff() {
    let expected = normalize(json!({
        "responseHeader": {"QTime": 1},
        "response": {"numFound": 0, "docs": []}
    }));
    let actual = normalize(json!({
        "responseHeader": {"QTime": 99},
        "response": {"numFound": 0, "docs": []}
    }));

    let report = diff(&expected.value, &actual.value);

    assert!(
        report.diffs.is_empty(),
        "differing QTime must not be a diff after normalisation, got {:?}",
        report.diffs
    );
}

#[test]
fn normalize_drops_error_msg_and_metadata_but_keeps_code() {
    let v = json!({"error": {"code": 400, "msg": "undefined field x", "metadata": ["a", "b"]}});
    let n = normalize(v);

    assert!(
        n.value["error"].get("msg").is_none(),
        "error.msg must be dropped"
    );
    assert!(
        n.value["error"].get("metadata").is_none(),
        "error.metadata must be dropped"
    );
    assert_eq!(n.value["error"]["code"], 400, "error.code must be kept");
    assert!(
        n.touched.contains(&"error.msg".to_string()),
        "dropping error.msg must be recorded, got {:?}",
        n.touched
    );
    assert!(
        n.touched.contains(&"error.metadata".to_string()),
        "dropping error.metadata must be recorded, got {:?}",
        n.touched
    );
}

#[test]
fn differing_error_msg_and_metadata_do_not_appear_as_a_diff() {
    let expected = normalize(json!({
        "error": {"code": 400, "msg": "undefined field x", "metadata": ["error-class", "A"]}
    }));
    let actual = normalize(json!({
        "error": {"code": 400, "msg": "a totally different message", "metadata": ["error-class", "B", "root-error-class", "C"]}
    }));

    let report = diff(&expected.value, &actual.value);

    assert!(
        report.diffs.is_empty(),
        "differing free-text error.msg/error.metadata must not be a diff, got {:?}",
        report.diffs
    );
}

#[test]
fn differing_error_code_is_still_a_diff() {
    let expected = normalize(json!({"error": {"code": 400, "msg": "x", "metadata": []}}));
    let actual = normalize(json!({"error": {"code": 500, "msg": "y", "metadata": []}}));

    let report = diff(&expected.value, &actual.value);

    assert!(
        !report.diffs.is_empty(),
        "error.code must still be compared and a mismatch must be a diff"
    );
}

// --- score tolerance --------------------------------------------------------

#[test]
fn score_within_tolerance_passes_and_is_logged() {
    let tol = score_tolerance();
    let expected = json!({"response": {"docs": [{"id": "doc1", "score": 1.2345}]}});
    let actual = json!({"response": {"docs": [{"id": "doc1", "score": 1.2345 + tol / 2.0}]}});

    let report = diff(&expected, &actual);

    assert!(
        report.diffs.is_empty(),
        "score within tolerance must pass, got diffs {:?}",
        report.diffs
    );
    assert!(
        report.touched.iter().any(|p| p.contains("score")),
        "score comparison must be logged in touched even when it passes, got {:?}",
        report.touched
    );
}

#[test]
fn score_outside_tolerance_fails() {
    let tol = score_tolerance();
    let expected = json!({"response": {"docs": [{"id": "doc1", "score": 1.0}]}});
    let actual = json!({"response": {"docs": [{"id": "doc1", "score": 1.0 + tol * 10.0}]}});

    let report = diff(&expected, &actual);

    assert!(
        !report.diffs.is_empty(),
        "score outside tolerance must be reported as a diff"
    );
}

// --- real diffs must fail ---------------------------------------------------

#[test]
fn diff_fails_on_numfound_off_by_one() {
    let expected = json!({"response": {"numFound": 5, "start": 0, "docs": []}});
    let actual = json!({"response": {"numFound": 6, "start": 0, "docs": []}});

    let report = diff(&expected, &actual);

    assert!(
        !report.diffs.is_empty(),
        "numFound off by one must be reported as a diff"
    );
    assert!(
        report.diffs.iter().any(|d| d.path.contains("numFound")),
        "diff must name the numFound path, got {:?}",
        report.diffs
    );
}

#[test]
fn diff_fails_on_doc_reordered() {
    let expected = json!({"response": {"docs": [{"id": "doc1"}, {"id": "doc2"}]}});
    let actual = json!({"response": {"docs": [{"id": "doc2"}, {"id": "doc1"}]}});

    let report = diff(&expected, &actual);

    assert!(
        !report.diffs.is_empty(),
        "a reordered doc list must be reported as a diff by the generic differ"
    );
}

#[test]
fn diff_fails_on_facet_count_changed() {
    let expected =
        json!({"facet_counts": {"facet_fields": {"category": ["animals", 2, "classic", 2]}}});
    let actual =
        json!({"facet_counts": {"facet_fields": {"category": ["animals", 3, "classic", 2]}}});

    let report = diff(&expected, &actual);

    assert!(
        !report.diffs.is_empty(),
        "a changed facet count must be reported as a diff"
    );
}

// --- ranked-ID-list mode -----------------------------------------------------

fn ranked(id: &str, score: Option<f64>) -> RankedDoc {
    RankedDoc {
        id: id.to_string(),
        score,
    }
}

#[test]
fn ranked_id_order_difference_fails_even_with_identical_membership() {
    let expected = vec![ranked("doc2", None), ranked("doc1", None)];
    let actual = vec![ranked("doc1", None), ranked("doc2", None)];

    let report = diff_ranked_ids(&expected, &actual);

    assert!(
        !report.diffs.is_empty(),
        "identical membership in a different order must fail ranked-ID comparison"
    );
}

#[test]
fn ranked_id_order_matching_passes() {
    let docs = vec![
        ranked("doc2", None),
        ranked("doc1", None),
        ranked("doc3", None),
    ];

    let report = diff_ranked_ids(&docs, &docs.clone());

    assert!(
        report.diffs.is_empty(),
        "identical order must pass, got {:?}",
        report.diffs
    );
}

#[test]
fn ranked_docs_extracts_ordered_id_score_pairs_from_an_envelope() {
    let envelope = json!({
        "response": {"docs": [{"id": "doc2", "score": 1.0}, {"id": "doc1", "score": 0.5}]}
    });

    assert_eq!(
        ranked_docs(&envelope),
        vec![ranked("doc2", Some(1.0)), ranked("doc1", Some(0.5))]
    );
}

#[test]
fn ranked_docs_extracts_ids_with_no_score_when_the_envelope_has_none() {
    let envelope = json!({
        "response": {"docs": [{"id": "doc2"}, {"id": "doc1"}]}
    });

    assert_eq!(
        ranked_docs(&envelope),
        vec![ranked("doc2", None), ranked("doc1", None)]
    );
}

// --- ranked-ID score tolerance (issue #31 follow-up 1-2) ---------------------
//
// `diff_ranked_ids` used to compare id order only. Score comparison was the
// spec's original intent (PRD §8: "ranked-ID-list mode with score
// tolerance") but no fixture's `fl` included `score`, so this path was dead
// code exercised only by synthetic tests. `select_term_scored`/
// `select_quick_scored` (issue #31) close that gap — see the
// fixture-derived tests below.

#[test]
fn ranked_id_score_within_tolerance_passes_and_is_logged() {
    let tol = score_tolerance();
    let expected = vec![ranked("doc1", Some(1.2345))];
    let actual = vec![ranked("doc1", Some(1.2345 + tol / 2.0))];

    let report = diff_ranked_ids(&expected, &actual);

    assert!(
        report.diffs.is_empty(),
        "score within tolerance must pass, got {:?}",
        report.diffs
    );
    assert!(
        report.touched.iter().any(|p| p.contains("score")),
        "the score comparison must be logged in touched even when it passes, got {:?}",
        report.touched
    );
}

#[test]
fn ranked_id_score_outside_tolerance_fails_naming_the_score_path() {
    let tol = score_tolerance();
    let expected = vec![ranked("doc1", Some(1.0))];
    let actual = vec![ranked("doc1", Some(1.0 + tol * 10.0))];

    let report = diff_ranked_ids(&expected, &actual);

    assert!(
        !report.diffs.is_empty(),
        "score outside tolerance must be reported as a diff"
    );
    assert!(
        report.diffs.iter().any(|d| d.path.contains("score")),
        "the diff must name the score path, got {:?}",
        report.diffs
    );
}

#[test]
fn ranked_id_score_present_vs_missing_is_a_diff() {
    let expected = vec![ranked("doc1", Some(1.0))];
    let actual = vec![ranked("doc1", None)];

    let report = diff_ranked_ids(&expected, &actual);

    assert!(
        !report.diffs.is_empty(),
        "a score present on one side and missing on the other must be a diff, not silently \
         skipped"
    );
}

/// The tolerance path exercised against **real** fixture data, not just
/// synthetic id/score pairs (issue #31 follow-up 2): loads the actual
/// `select_term_scored` fixture, perturbs its real BM25 scores by known
/// amounts relative to `score_tolerance()`, and diffs the fixture against
/// itself. `score_tolerance()/2` must pass (and log the score path in
/// `touched`); `10 * score_tolerance()` must fail, naming the score path.
#[test]
fn ranked_id_score_tolerance_exercised_against_real_select_term_scored_fixture() {
    let expected = fixture("select_term_scored");
    let expected_docs = ranked_docs(&expected);
    assert!(
        !expected_docs.is_empty(),
        "select_term_scored fixture must have ranked docs to exercise this path"
    );
    assert!(
        expected_docs.iter().all(|d| d.score.is_some()),
        "select_term_scored fixture's docs must all carry a real score, got {:?}",
        expected_docs
    );
    let tol = score_tolerance();

    let within: Vec<RankedDoc> = expected_docs
        .iter()
        .map(|d| ranked(&d.id, d.score.map(|s| s + tol / 2.0)))
        .collect();
    let within_report = diff_ranked_ids(&expected_docs, &within);
    assert!(
        within_report.diffs.is_empty(),
        "a perturbation of score_tolerance()/2 against the real fixture must pass, got {:?}",
        within_report.diffs
    );
    assert!(
        within_report.touched.iter().any(|p| p.contains("score")),
        "the real-fixture perturbation within tolerance must still be logged in touched, \
         got {:?}",
        within_report.touched
    );

    let outside: Vec<RankedDoc> = expected_docs
        .iter()
        .map(|d| ranked(&d.id, d.score.map(|s| s + tol * 10.0)))
        .collect();
    let outside_report = diff_ranked_ids(&expected_docs, &outside);
    assert!(
        !outside_report.diffs.is_empty(),
        "a perturbation of 10 * score_tolerance() against the real fixture must fail"
    );
    assert!(
        outside_report
            .diffs
            .iter()
            .any(|d| d.path.contains("score")),
        "the failure must name the score path, got {:?}",
        outside_report.diffs
    );
}

// --- params key order (documents existing serde_json behaviour) ------------

#[test]
fn params_object_equality_is_key_order_insensitive_by_construction() {
    // PRD §8 / findings fact 6: `responseHeader.params` key order is not
    // request order in Solr. No normaliser code is needed for this —
    // `serde_json::Value::Object` already compares as an order-independent
    // map. This test pins that fact rather than exercising our own code
    // (spec: "assert it rather than writing code for it").
    let a: Value = serde_json::from_str(r#"{"q":"*:*","wt":"json","rows":"10"}"#).unwrap();
    let b: Value = serde_json::from_str(r#"{"rows":"10","wt":"json","q":"*:*"}"#).unwrap();
    assert_eq!(a, b, "JSON object equality must not depend on key order");
}

// --- manifest loader ---------------------------------------------------------

#[test]
fn load_manifest_parses_every_line_of_the_real_manifest() {
    let path = manifest_path();
    let raw = std::fs::read_to_string(&path).expect("read solr-ref/manifest.tsv");
    let expected_count = raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .count();

    let entries = load_manifest(&path);

    assert_eq!(
        entries.len(),
        expected_count,
        "loader must parse every non-blank, non-comment line"
    );
    assert!(
        entries.contains(&ManifestEntry {
            name: "ping".to_string(),
            status: 200,
            path: "admin/ping?wt=json".to_string(),
        }),
        "loader must parse the ping entry, got {:?}",
        entries
    );
    assert!(
        entries
            .iter()
            .any(|e| e.name == "err_bad_sort" && e.status == 400),
        "loader must parse error entries with their non-200 status"
    );
}

#[test]
fn load_manifest_skips_blanks_and_comments_and_tolerates_trailing_columns() {
    let dir = tempfile::TempDir::new().expect("create temp dir");
    let path = dir.path().join("manifest.tsv");
    std::fs::write(
        &path,
        "\n# a leading comment\nfoo\t200\tselect?q=*:*&wt=json\n\n# another comment\nbar\t400\tselect?q=bad&wt=json\textra\tcolumns\n",
    )
    .expect("write temp manifest");

    let entries = load_manifest(&path);

    assert_eq!(entries.len(), 2, "blank lines and comments must be skipped");
    assert_eq!(
        entries[0],
        ManifestEntry {
            name: "foo".to_string(),
            status: 200,
            path: "select?q=*:*&wt=json".to_string(),
        }
    );
    assert_eq!(entries[1].name, "bar");
    assert_eq!(entries[1].status, 400);
    assert_eq!(
        entries[1].path, "select?q=bad&wt=json",
        "extra trailing columns beyond path must be tolerated (ignored), not error"
    );
}

// --- manifest-errors loader (issue #31, item 3) ------------------------------

#[test]
fn load_manifest_errors_parses_every_line_of_the_real_manifest_errors() {
    let path = manifest_errors_path();
    let raw = std::fs::read_to_string(&path).expect("read solr-ref/manifest-errors.tsv");
    let expected_count = raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .count();

    let entries = load_manifest_errors(&path);

    assert_eq!(
        entries.len(),
        expected_count,
        "loader must parse every non-blank, non-comment line of manifest-errors.tsv"
    );
    assert!(
        entries.contains(&ManifestErrorEntry {
            name: "err_missing_core".to_string(),
            status: 404,
            method: "GET".to_string(),
            url: "nosuchcore/select?q=*:*&wt=json".to_string(),
            body: None,
            base_url: None,
        }),
        "loader must parse err_missing_core with no body/base-url columns, got {:?}",
        entries
    );
    assert!(
        entries.contains(&ManifestErrorEntry {
            name: "err_update_bad_json".to_string(),
            status: 400,
            method: "POST".to_string(),
            url: "content/update?commit=true&wt=json".to_string(),
            body: Some("{not json".to_string()),
            base_url: None,
        }),
        "loader must parse err_update_bad_json's body column, got {:?}",
        entries
    );
    assert!(
        entries
            .iter()
            .any(|e| e.name == "update_unknown_field_schemaless"
                && e.base_url.as_deref() == Some("http://localhost:8983/solr")),
        "loader must parse the 6th (base-url) column when present, got {:?}",
        entries
    );
}

#[test]
fn load_manifest_errors_skips_blanks_and_comments_and_tolerates_missing_columns() {
    let dir = tempfile::TempDir::new().expect("create temp dir");
    let path = dir.path().join("manifest-errors.tsv");
    std::fs::write(
        &path,
        "\n# a leading comment\n\
         foo\t404\tGET\tnosuchcore/select?q=*:*&wt=json\t\t\n\
         \n# another comment\n\
         bar\t400\tPOST\tcontent/update?wt=json\t[]\thttp://localhost:8984/solr\n",
    )
    .expect("write temp manifest-errors");

    let entries = load_manifest_errors(&path);

    assert_eq!(entries.len(), 2, "blank lines and comments must be skipped");
    assert_eq!(
        entries[0],
        ManifestErrorEntry {
            name: "foo".to_string(),
            status: 404,
            method: "GET".to_string(),
            url: "nosuchcore/select?q=*:*&wt=json".to_string(),
            body: None,
            base_url: None,
        },
        "empty body/base-url columns must parse as None, not Some(\"\")"
    );
    assert_eq!(
        entries[1],
        ManifestErrorEntry {
            name: "bar".to_string(),
            status: 400,
            method: "POST".to_string(),
            url: "content/update?wt=json".to_string(),
            body: Some("[]".to_string()),
            base_url: Some("http://localhost:8984/solr".to_string()),
        }
    );
}

// --- hermetic whole-query-set run -------------------------------------------

/// The subset of manifest entries that are free-text relevance queries (PRD
/// §8: "compare ranked ID lists, not just result sets"). `select_term` is
/// the current free-text `q=` entry; `select_fq_multi` is a filter query, not
/// relevance, so it is diffed generically like everything else.
/// `select_term_scored`/`select_quick_scored` (issue #31) add score-bearing
/// entries so the ranked+score path runs against real data, not just
/// synthetic tests — both are also in `EXPECTED_DIVERGENCES` below, since
/// Wayfinder does not implement `fl=score` yet (issue #34).
const RANKED_RELEVANCE_ENTRIES: &[&str] =
    &["select_term", "select_term_scored", "select_quick_scored"];

/// Manifest entries with a *known, currently real* Wayfinder-vs-Solr
/// divergence, each caused by an unbuilt feature rather than a harness bug
/// (escalated and accepted by the orchestrator — see this issue's handoff).
/// Excluded from the pass/fail loop below, but only ever as a documented,
/// self-expiring to-do: every reason names the issue that owns the fix, and
/// the guard at the end of the test loop below FAILS the moment any of these
/// entries stops diverging — that means the feature landed and the entry
/// must be deleted from this list, not that the harness can go quiet about
/// it. `ping` gets no normaliser carve-out for its unreproducible `rid`
/// value; encoding that in the normaliser would risk hiding a real
/// `params` diff on every other entry, so it lives here instead, alongside
/// the rest.
// Sort (issue #2) used to be listed here: #11 landed sort validation, which made
// `err_bad_sort` match, and #2 landed the ordering itself, which made
// `select_sort` — plus the sixteen `select_sort_*` / `err_sort_*` entries added
// with it — match too. Both are gone from this list, as designed.
// Faceting (issue #3) used to hold seven entries here — `facet_mincount`,
// `facet_limit`, `facet_missing`, `facet_query`, `facet_json_nl_map`,
// `facet_zero`, `facet_all_filtered`. Real fast-field aggregation over the whole
// term dictionary made all seven match, so they are gone too.
// `select_term_scored`/`select_quick_scored` (issue #31) are new: Wayfinder
// silently drops `fl=score` (grep of src/ confirms no doc-score rendering,
// the same `select_fl_missing` precedent for an unknown-but-harmless `fl`
// entry), so the actual score is always absent on the Wayfinder side —
// tracked as issue #34. Both entries expire the moment #34 lands: the guard
// below fails and names them for deletion.
const EXPECTED_DIVERGENCES: &[(&str, &str)] = &[
    (
        "ping",
        "`responseHeader.params` carries Solr ping-handler artifacts incl. a per-run `rid` counter no implementation can reproduce; see the same carve-out in `tracer_bullet.rs::ping_reports_ok`",
    ),
    (
        "select_term_scored",
        "Wayfinder does not implement fl=score (silently dropped, same as an unknown fl \
         field) — see issue #34",
    ),
    (
        "select_quick_scored",
        "Wayfinder does not implement fl=score (silently dropped, same as an unknown fl \
         field) — see issue #34",
    ),
];

/// The `EXPECTED_DIVERGENCES` reason for `name`, or `None` if `name` is not
/// in the list. Every entry has a mandatory reason by construction (the list
/// is `&[(&str, &str)]`) — this just looks one up by name.
fn expected_divergence_reason(name: &str) -> Option<&'static str> {
    EXPECTED_DIVERGENCES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, reason)| *reason)
}

#[tokio::test]
async fn hermetic_whole_query_set_matches_committed_fixtures() {
    let (app, _dir) = indexed_app().await;
    let entries = load_manifest(&manifest_path());
    assert!(!entries.is_empty(), "manifest must not be empty");

    let manifest_names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    for (name, reason) in EXPECTED_DIVERGENCES {
        assert!(
            manifest_names.contains(name),
            "EXPECTED_DIVERGENCES entry `{name}` (reason: {reason}) does not match any \
             manifest entry — fix the name or remove the stale entry"
        );
    }

    let mut failures = Vec::new();
    eprintln!("--- differential run: every manifest entry ---");
    for entry in &entries {
        let (status, actual) = get(&app, &entry.path).await;
        let divergence_reason = expected_divergence_reason(&entry.name);

        if status.as_u16() != entry.status {
            let msg = format!(
                "{}: HTTP status {} vs expected {}",
                entry.name, status, entry.status
            );
            eprintln!("{msg}");
            match divergence_reason {
                Some(reason) => eprintln!("  (expected divergence: {reason})"),
                None => failures.push(msg),
            }
            continue;
        }

        let expected = fixture(&entry.name);
        let expected_n = normalize(expected);
        let actual_n = normalize(actual);
        eprintln!(
            "{}: normaliser touched {:?}",
            entry.name, expected_n.touched
        );

        if RANKED_RELEVANCE_ENTRIES.contains(&entry.name.as_str()) {
            let expected_docs = ranked_docs(&expected_n.value);
            let actual_docs = ranked_docs(&actual_n.value);
            let ranked_report = diff_ranked_ids(&expected_docs, &actual_docs);
            eprintln!(
                "{}: ranked-id diffs: {:?}, touched {:?}",
                entry.name, ranked_report.diffs, ranked_report.touched
            );

            match divergence_reason {
                Some(reason) if ranked_report.diffs.is_empty() => failures.push(format!(
                    "{}: EXPECTED_DIVERGENCES says this should still diverge ({reason}), \
                     but it now matches — the underlying feature has landed, so remove this \
                     entry from EXPECTED_DIVERGENCES in tests/differential.rs",
                    entry.name
                )),
                Some(reason) => eprintln!("  (expected divergence: {reason})"),
                None if !ranked_report.diffs.is_empty() => failures.push(format!(
                    "{}: ranked-id diffs: {:?}",
                    entry.name, ranked_report.diffs
                )),
                None => {}
            }
        } else {
            let report = diff(&expected_n.value, &actual_n.value);
            eprintln!(
                "{}: {} diffs, touched (tolerance-applied) {:?}",
                entry.name,
                report.diffs.len(),
                report.touched
            );
            if !report.diffs.is_empty() {
                eprintln!("  diffs: {:?}", report.diffs);
            }

            match divergence_reason {
                Some(reason) if report.diffs.is_empty() => failures.push(format!(
                    "{}: EXPECTED_DIVERGENCES says this should still diverge ({reason}), \
                     but it now matches — the underlying feature has landed, so remove this \
                     entry from EXPECTED_DIVERGENCES in tests/differential.rs",
                    entry.name
                )),
                Some(reason) => eprintln!("  (expected divergence: {reason})"),
                None if !report.diffs.is_empty() => {
                    failures.push(format!("{}: {:?}", entry.name, report.diffs))
                }
                None => {}
            }
        }
    }

    eprintln!(
        "--- expected-divergence list (excluded from pass/fail above, each self-expiring) ---"
    );
    for (name, reason) in EXPECTED_DIVERGENCES {
        eprintln!("  {name}: {reason}");
    }

    assert!(
        failures.is_empty(),
        "hermetic differential failures against solr-ref fixtures:\n{}",
        failures.join("\n")
    );
}

// --- live Solr round trip (gated) -------------------------------------------

/// Live counterpart of the hermetic run, gated by `WAYFINDER_DIFF_SOLR=1` so
/// plain `cargo test` never touches the network or requires Docker. Run
/// `solr-ref/capture.sh` first — it leaves the container up with the schema
/// and corpus already loaded; this test does not orchestrate Docker itself.
///
/// `#[ignore]` is deliberately *not* also used here — the spec calls for one
/// gating mechanism, not both, so this stays a plain `#[test]` that no-ops
/// (and passes) when the env var is unset.
#[test]
fn live_solr_matches_committed_query_set() {
    if std::env::var("WAYFINDER_DIFF_SOLR").ok().as_deref() != Some("1") {
        eprintln!(
            "skipping live Solr differential: run solr-ref/capture.sh, then set \
             WAYFINDER_DIFF_SOLR=1 to enable (WAYFINDER_DIFF_SOLR=1 cargo test --test differential)"
        );
        return;
    }

    let base_url = std::env::var("WAYFINDER_DIFF_SOLR_URL")
        .unwrap_or_else(|_| "http://localhost:8983/solr/content".to_string());

    let entries = load_manifest(&manifest_path());
    let mut failures = Vec::new();
    for entry in &entries {
        // `EXPECTED_DIVERGENCES` applies here exactly as it does hermetically,
        // and for a sharper reason: this mode compares live Solr against
        // *captured Solr*, so `ping`'s per-run `rid` counter differs from one
        // Solr run to the next. A listed entry failing here is the list being
        // right, not the harness finding a bug.
        let divergence_reason = expected_divergence_reason(&entry.name);

        let (status, actual) = common::diff::fetch_live(&base_url, &entry.path);
        if status != entry.status {
            let msg = format!(
                "{}: HTTP status {} vs expected {}",
                entry.name, status, entry.status
            );
            match divergence_reason {
                Some(reason) => eprintln!("{msg}\n  (expected divergence: {reason})"),
                None => failures.push(msg),
            }
            continue;
        }

        let expected = fixture(&entry.name);
        let expected_n = normalize(expected);
        let actual_n = normalize(actual);

        // Extended (issue #31) so RANKED_RELEVANCE_ENTRIES rows use the
        // ranked+score path here too — previously this loop generic-diffed
        // everything, unlike the hermetic run above.
        let diffs_empty = if RANKED_RELEVANCE_ENTRIES.contains(&entry.name.as_str()) {
            let expected_docs = ranked_docs(&expected_n.value);
            let actual_docs = ranked_docs(&actual_n.value);
            let ranked_report = diff_ranked_ids(&expected_docs, &actual_docs);
            let empty = ranked_report.diffs.is_empty();
            if !empty {
                eprintln!("{}: ranked-id diffs: {:?}", entry.name, ranked_report.diffs);
            }
            empty
        } else {
            let report = diff(&expected_n.value, &actual_n.value);
            let empty = report.diffs.is_empty();
            if !empty {
                eprintln!("{}: {:?}", entry.name, report.diffs);
            }
            empty
        };

        match (diffs_empty, divergence_reason) {
            // Self-expiring in this mode too — for divergences inherent to
            // *Solr itself* (`ping`'s per-run `rid`): an entry that stops
            // diverging must be removed, or the list quietly becomes a lie
            // here while the hermetic run still polices it.
            //
            // This does NOT hold for `select_term_scored`/`select_quick_scored`
            // (issue #31/#34): this loop's "actual" side is a *live re-fetch of
            // Solr itself*, not Wayfinder (`fetch_live` always hits
            // `WAYFINDER_DIFF_SOLR_URL`, Solr's own port) — see the comment
            // above. A Wayfinder-only feature gap like missing `fl=score`
            // support trivially "matches" here on every run, since real Solr's
            // BM25 is deterministic against its own historical capture,
            // regardless of whether #34 has landed. Self-expiry here would
            // misfire unconditionally, so it is skipped for those two — the
            // hermetic loop is the one that actually exercises Wayfinder and
            // owns the self-expiry signal for this class of entry.
            (true, Some(reason)) if RANKED_RELEVANCE_ENTRIES.contains(&entry.name.as_str()) => {
                eprintln!(
                    "{}: matches live Solr (expected — this loop compares Solr against its own \
                     capture, not Wayfinder; self-expiry for {reason} is decided by the \
                     hermetic run)",
                    entry.name
                );
            }
            (true, Some(reason)) => failures.push(format!(
                "{}: EXPECTED_DIVERGENCES says this should still diverge ({reason}), but it \
                 matches live Solr — remove this entry from EXPECTED_DIVERGENCES in \
                 tests/differential.rs",
                entry.name
            )),
            (false, Some(reason)) => eprintln!("{}: (expected divergence: {reason})", entry.name),
            (false, None) => failures.push(format!("{}: differs from live Solr", entry.name)),
            (true, None) => {}
        }
    }

    assert!(
        failures.is_empty(),
        "live Solr differential failures:\n{}",
        failures.join("\n")
    );
}

// --- manifest-errors.tsv wired into the harness (issue #31, item 3) --------
//
// `manifest-errors.tsv` (added by issue #11 for the non-core-relative-GET
// error fixtures) was covered only by `tests/error_shapes.rs`'s unit tests
// until now. This runs EVERY row against an in-process Wayfinder, per-row
// app selection by the URL's leading core segment: `content/...` ->
// `common::indexed_app()`; `facets/...` -> `facets_app()`; `keyorder/...` ->
// `keyorder_app()`. All three name their core `content`, so the leading
// segment is rewritten to `content` before the request is issued.
//
// A row whose leading segment names none of the three (`nosuchcore/...`,
// `schemaless_probe/...`) is not rewritten at all — that mismatch (a core
// Wayfinder genuinely does not have) is exactly the shape of the
// `err_missing_core`/`update_unknown_field_schemaless` `ACCEPTED_DIVERGENCES`
// rows, so it is issued against the default content app with its literal,
// unrewritten URL, and checked by the narrower rule those two entries define
// rather than the full differ.

/// Selects the app for `entry` by its URL's leading core segment and
/// returns `(app, request_url)`, where `request_url` has that segment
/// rewritten to `content` for the three known cores. An unrecognised
/// segment is returned unrewritten against `content_app` — see the module
/// comment above.
fn app_and_request_url<'a>(
    entry: &ManifestErrorEntry,
    content_app: &'a Router,
    facets_app: &'a Router,
    keyorder_app: &'a Router,
) -> (&'a Router, String) {
    match entry.url.split_once('/') {
        Some(("content", rest)) => (content_app, format!("content/{rest}")),
        Some(("facets", rest)) => (facets_app, format!("content/{rest}")),
        Some(("keyorder", rest)) => (keyorder_app, format!("content/{rest}")),
        _ => (content_app, entry.url.clone()),
    }
}

#[tokio::test]
async fn manifest_errors_every_row_runs_against_the_matching_hermetic_app() {
    let entries = load_manifest_errors(&manifest_errors_path());
    let raw = std::fs::read_to_string(manifest_errors_path()).expect("read manifest-errors.tsv");
    let expected_count = raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .count();
    assert_eq!(
        entries.len(),
        expected_count,
        "loader must parse every row of manifest-errors.tsv"
    );
    assert!(!entries.is_empty(), "manifest-errors.tsv must not be empty");

    for (name, reason) in ACCEPTED_DIVERGENCES {
        assert!(
            entries.iter().any(|e| e.name == *name),
            "ACCEPTED_DIVERGENCES entry `{name}` (reason: {reason}) does not match any \
             manifest-errors row — fix the name or remove the stale entry"
        );
    }

    let (content_app, _content_dir) = indexed_app().await;
    let (facets_app, _facets_dir) = facets_app().await;
    let (keyorder_app, _keyorder_dir) = keyorder_app().await;

    let mut ran = 0usize;
    let mut diffed = 0usize;
    let mut failures = Vec::new();
    eprintln!("--- manifest-errors differential run ---");
    eprintln!("accepted (permanent, ratified) divergences:");
    for (name, reason) in ACCEPTED_DIVERGENCES {
        eprintln!("  {name}: {reason}");
    }

    for entry in &entries {
        let (app, url) = app_and_request_url(entry, &content_app, &facets_app, &keyorder_app);
        let (status, actual) = request_full(app, &entry.method, &url, entry.body.as_deref()).await;

        if let Some(reason) = accepted_divergence_reason(&entry.name) {
            match entry.name.as_str() {
                "err_missing_core" => {
                    // Keep the check honest: the fixture must genuinely be
                    // non-JSON (Solr's 404 HTML easter egg), or this
                    // "accepted" entry could rot into a false excuse.
                    let text = fixture_text(&entry.name);
                    assert!(
                        serde_json::from_str::<Value>(&text).is_err(),
                        "{}: fixture must be non-JSON for this accepted divergence to still \
                         apply ({reason})",
                        entry.name
                    );
                    if status.as_u16() != entry.status {
                        failures.push(format!(
                            "{}: HTTP status {} vs fixture status {} ({reason})",
                            entry.name, status, entry.status
                        ));
                    }
                }
                "update_unknown_field_schemaless" => {
                    assert_eq!(
                        entry.status, 200,
                        "{}: fixture must be 200 for this accepted divergence to still name \
                         the gap ({reason})",
                        entry.name
                    );
                    if status.as_u16() == 200 {
                        failures.push(format!(
                            "{}: Wayfinder answered 200 — the documented schemaless \
                             divergence no longer holds, remove this ACCEPTED_DIVERGENCES \
                             entry and update the PRD ({reason})",
                            entry.name
                        ));
                    }
                }
                "facet_non_docvalues_text"
                | "facet_non_docvalues_text_enum"
                | "facet_stored_only_field" => {
                    if status.as_u16() == entry.status {
                        failures.push(format!(
                            "{}: Wayfinder matched the fixture's status — the documented \
                             unfacetable-field divergence no longer holds, remove this \
                             ACCEPTED_DIVERGENCES entry and update finding 16 ({reason})",
                            entry.name
                        ));
                    }
                }
                other => unreachable!(
                    "ACCEPTED_DIVERGENCES entry `{other}` has no matching check arm in this test"
                ),
            }
            // Counted here, past the actual accepted-divergence check above,
            // not as the loop's first statement — see the tautology this
            // guards against in the comment on the final assertion below.
            ran += 1;
            continue;
        }

        if status.as_u16() != entry.status {
            failures.push(format!(
                "{}: HTTP status {} vs expected {}",
                entry.name, status, entry.status
            ));
            ran += 1;
            continue;
        }

        let expected = fixture(&entry.name);
        let expected_n = normalize(expected);
        let actual_n = normalize(actual);
        let report = diff(&expected_n.value, &actual_n.value);
        // The differ-bound counter: only rows that actually reach `diff()`
        // count here, so a bug that hollowed out this branch (while leaving
        // `ran` incrementing elsewhere) would still be caught below.
        diffed += 1;
        eprintln!(
            "{}: {} diffs, touched (tolerance-applied) {:?}",
            entry.name,
            report.diffs.len(),
            report.touched
        );
        if !report.diffs.is_empty() {
            eprintln!("  diffs: {:?}", report.diffs);
        }

        match expected_divergence_manifest_errors_reason(&entry.name) {
            Some(reason) if report.diffs.is_empty() => failures.push(format!(
                "{}: EXPECTED_DIVERGENCES_MANIFEST_ERRORS says this should still diverge \
                 ({reason}), but it now matches — the underlying fix has landed, so remove \
                 this entry from EXPECTED_DIVERGENCES_MANIFEST_ERRORS in tests/differential.rs",
                entry.name
            )),
            Some(reason) => eprintln!("  (expected divergence: {reason})"),
            None if !report.diffs.is_empty() => {
                failures.push(format!("{}: {:?}", entry.name, report.diffs))
            }
            None => {}
        }
        ran += 1;
    }

    for (name, reason) in EXPECTED_DIVERGENCES_MANIFEST_ERRORS {
        assert!(
            entries.iter().any(|e| e.name == *name),
            "EXPECTED_DIVERGENCES_MANIFEST_ERRORS entry `{name}` (reason: {reason}) does not \
             match any manifest-errors row — fix the name or remove the stale entry"
        );
    }

    // The weakness to guard against (issue #31): a loader that parses rows
    // but a loop that never executes them would be green and worthless.
    // `ran` is incremented only after each branch's real check has run (not
    // as the loop's first statement), and `diffed` is incremented only where
    // `diff()` is actually called — every non-accepted-divergence row, since
    // `EXPECTED_DIVERGENCES_MANIFEST_ERRORS` entries still go through the
    // real differ, just without failing the suite on a match. A loop
    // hollowed out to just count rows, without doing the checks, would leave
    // `diffed` short of this target even if `ran` alone looked fine.
    assert_eq!(
        ran,
        entries.len(),
        "every manifest-errors row must be exercised, not just loaded"
    );
    assert_eq!(
        diffed,
        entries.len() - ACCEPTED_DIVERGENCES.len(),
        "every non-accepted-divergence manifest-errors row must go through the real differ"
    );
    assert!(
        failures.is_empty(),
        "manifest-errors differential failures:\n{}",
        failures.join("\n")
    );
}

/// Live counterpart, gated by `WAYFINDER_DIFF_SOLR=1` exactly like
/// `live_solr_matches_committed_query_set`. Each row uses its own effective
/// base URL (column 6, defaulting to the canonical `http://localhost:8983/solr`)
/// and method/body. A row whose base URL does not answer a quick
/// reachability probe is a PRINTED, named skip — the per-issue containers on
/// 8984/8985/8986 are not guaranteed to be up — but a row on the default
/// 8983 base must actually run; that base is the canonical container this
/// whole harness depends on.
#[test]
fn live_solr_matches_committed_manifest_errors() {
    if std::env::var("WAYFINDER_DIFF_SOLR").ok().as_deref() != Some("1") {
        eprintln!(
            "skipping live Solr manifest-errors differential: set WAYFINDER_DIFF_SOLR=1 to \
             enable (WAYFINDER_DIFF_SOLR=1 cargo test --test differential)"
        );
        return;
    }

    const DEFAULT_BASE: &str = "http://localhost:8983/solr";

    let entries = load_manifest_errors(&manifest_errors_path());
    let mut failures = Vec::new();
    // Mirrors the hermetic run's anti-vacuity counters (issue #31 review
    // follow-up): `ran`/`diffed` are only incremented past each branch's
    // real check, never as the loop's first statement, so a hollowed-out
    // loop body cannot satisfy the assertions below by construction.
    // `skipped` tracks legitimate, printed skips for an absent per-issue
    // container — those rows are neither `ran` nor a failure.
    let mut ran = 0usize;
    let mut diffed = 0usize;
    let mut skipped = 0usize;
    for entry in &entries {
        let base_url = entry.base_url.as_deref().unwrap_or(DEFAULT_BASE);
        let divergence_reason = accepted_divergence_reason(&entry.name);

        if !live_reachable(base_url) {
            if base_url == DEFAULT_BASE {
                failures.push(format!(
                    "{}: default base {base_url} did not answer a reachability probe — the \
                     canonical container must be up for this row to run",
                    entry.name
                ));
            } else {
                eprintln!(
                    "{}: skipping — {base_url} did not answer a reachability probe (per-issue \
                     container may be absent)",
                    entry.name
                );
                skipped += 1;
            }
            continue;
        }

        if let Some(reason) = divergence_reason {
            // Accepted divergences are checked hermetically above (including
            // `err_missing_core`'s honesty check that the fixture is
            // genuinely non-JSON); live mode here only re-confirms the status
            // code matched. Deliberately a status-only fetch, not
            // `fetch_live_full`: `err_missing_core`'s body is Solr's 404 HTML
            // easter egg, which `fetch_live_full`'s JSON parse would panic on.
            let status =
                fetch_live_status(base_url, &entry.method, &entry.url, entry.body.as_deref());
            if status != entry.status {
                failures.push(format!(
                    "{}: HTTP status {} vs expected {} (accepted divergence: {reason})",
                    entry.name, status, entry.status
                ));
            } else {
                eprintln!("{}: (accepted divergence: {reason})", entry.name);
            }
            ran += 1;
            continue;
        }

        let (status, actual) =
            fetch_live_full(base_url, &entry.method, &entry.url, entry.body.as_deref());

        if status != entry.status {
            failures.push(format!(
                "{}: HTTP status {} vs expected {}",
                entry.name, status, entry.status
            ));
            ran += 1;
            continue;
        }

        // `EXPECTED_DIVERGENCES_MANIFEST_ERRORS` (issue #35's
        // `facet_unknown_field`) is not self-expired here for the same reason
        // `live_solr_matches_committed_query_set` skips it for
        // `RANKED_RELEVANCE_ENTRIES`: this loop's "actual" side is a live
        // re-fetch of Solr itself, not Wayfinder, so a Wayfinder-only gap
        // trivially "matches" on every run regardless of whether the fix has
        // landed. The hermetic run owns that signal.
        let expected_divergence_reason = expected_divergence_manifest_errors_reason(&entry.name);

        let expected = fixture(&entry.name);
        let expected_n = normalize(expected);
        let actual_n = normalize(actual);
        let report = diff(&expected_n.value, &actual_n.value);
        diffed += 1;
        match (report.diffs.is_empty(), expected_divergence_reason) {
            (true, Some(reason)) => eprintln!(
                "{}: matches live Solr (expected — this loop compares Solr against its own \
                 capture, not Wayfinder; self-expiry for {reason} is decided by the hermetic \
                 run)",
                entry.name
            ),
            (false, Some(reason)) => eprintln!("{}: (expected divergence: {reason})", entry.name),
            (false, None) => failures.push(format!("{}: {:?}", entry.name, report.diffs)),
            (true, None) => {}
        }
        ran += 1;
    }

    // Same weakness as the hermetic run: a loader that parses rows but a
    // loop that never executes them (or that stops short after a container
    // outage) would be green and worthless. Every row is either `ran`,
    // legitimately `skipped` (a named, printed per-issue-container skip), or
    // a counted failure above — accounted for exactly, not just non-zero.
    // `diffed` covers every `ran` row except the accepted-divergence ones,
    // which deliberately skip the real differ (checked by status only,
    // hermetically checked in full above) — all of which live on the
    // always-reachable default base, so none of them can end up in
    // `skipped`.
    assert_eq!(
        ran + skipped,
        entries.len(),
        "every manifest-errors row must be run or legitimately skipped, not silently dropped"
    );
    assert_eq!(
        diffed,
        ran - ACCEPTED_DIVERGENCES.len(),
        "every non-accepted-divergence row that ran must go through the real differ"
    );
    assert!(
        diffed > 0,
        "the live manifest-errors run must exercise the real differ against at least one row \
         when the canonical 8983 container (required to be reachable) is up"
    );

    assert!(
        failures.is_empty(),
        "live Solr manifest-errors differential failures:\n{}",
        failures.join("\n")
    );
}
