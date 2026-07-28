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

use std::path::Path;

use common::diff::{
    ManifestEntry, diff, diff_ranked_ids, doc_ids, load_manifest, normalize, score_tolerance,
};
use common::{fixture, get, indexed_app};
use serde_json::{Value, json};

fn manifest_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("solr-ref/manifest.tsv")
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

#[test]
fn ranked_id_order_difference_fails_even_with_identical_membership() {
    let expected_ids = vec!["doc2".to_string(), "doc1".to_string()];
    let actual_ids = vec!["doc1".to_string(), "doc2".to_string()];

    let diffs = diff_ranked_ids(&expected_ids, &actual_ids);

    assert!(
        !diffs.is_empty(),
        "identical membership in a different order must fail ranked-ID comparison"
    );
}

#[test]
fn ranked_id_order_matching_passes() {
    let ids = vec!["doc2".to_string(), "doc1".to_string(), "doc3".to_string()];

    let diffs = diff_ranked_ids(&ids, &ids.clone());

    assert!(
        diffs.is_empty(),
        "identical order must pass, got {:?}",
        diffs
    );
}

#[test]
fn doc_ids_extracts_ordered_id_list_from_an_envelope() {
    let envelope = json!({
        "response": {"docs": [{"id": "doc2", "score": 1.0}, {"id": "doc1", "score": 0.5}]}
    });

    assert_eq!(
        doc_ids(&envelope),
        vec!["doc2".to_string(), "doc1".to_string()]
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

// --- hermetic whole-query-set run -------------------------------------------

/// The subset of manifest entries that are free-text relevance queries (PRD
/// §8: "compare ranked ID lists, not just result sets"). `select_term` is
/// the current free-text `q=` entry; `select_fq_multi` is a filter query, not
/// relevance, so it is diffed generically like everything else.
const RANKED_RELEVANCE_ENTRIES: &[&str] = &["select_term"];

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
// term dictionary made all seven match, so they are gone too. `ping` is the only
// entry left, and it is the one that can never expire.
const EXPECTED_DIVERGENCES: &[(&str, &str)] = &[(
    "ping",
    "`responseHeader.params` carries Solr ping-handler artifacts incl. a per-run `rid` counter no implementation can reproduce; see the same carve-out in `tracer_bullet.rs::ping_reports_ok`",
)];

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
            let expected_ids = doc_ids(&expected_n.value);
            let actual_ids = doc_ids(&actual_n.value);
            let id_diffs = diff_ranked_ids(&expected_ids, &actual_ids);
            eprintln!("{}: ranked-id diffs: {:?}", entry.name, id_diffs);

            match divergence_reason {
                Some(reason) if id_diffs.is_empty() => failures.push(format!(
                    "{}: EXPECTED_DIVERGENCES says this should still diverge ({reason}), \
                     but it now matches — the underlying feature has landed, so remove this \
                     entry from EXPECTED_DIVERGENCES in tests/differential.rs",
                    entry.name
                )),
                Some(reason) => eprintln!("  (expected divergence: {reason})"),
                None if !id_diffs.is_empty() => {
                    failures.push(format!("{}: ranked-id diffs: {:?}", entry.name, id_diffs))
                }
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
        let report = diff(&expected_n.value, &actual_n.value);
        match (report.diffs.is_empty(), divergence_reason) {
            // Self-expiring in this mode too: an entry that stops diverging
            // must be removed, or the list quietly becomes a lie here while
            // the hermetic run still polices it.
            (true, Some(reason)) => failures.push(format!(
                "{}: EXPECTED_DIVERGENCES says this should still diverge ({reason}), but it \
                 matches live Solr — remove this entry from EXPECTED_DIVERGENCES in \
                 tests/differential.rs",
                entry.name
            )),
            (false, Some(reason)) => eprintln!(
                "{}: {:?}\n  (expected divergence: {reason})",
                entry.name, report.diffs
            ),
            (false, None) => failures.push(format!("{}: {:?}", entry.name, report.diffs)),
            (true, None) => {}
        }
    }

    assert!(
        failures.is_empty(),
        "live Solr differential failures:\n{}",
        failures.join("\n")
    );
}
