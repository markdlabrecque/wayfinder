//! Issue #457: the tuning loop must not change the rules it is judged by.
//!
//! The accepted baseline records a hash of every protected file (Solr's saved
//! answers, the allowed-diffs list, the query list, the weights and the scoring
//! code). A run compares the files on disk with those hashes and refuses to
//! score when any differ. These tests build a small fake repo in a scratch dir
//! and drive the library seam the driver uses; no Solr or Wayfinder is run.

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use support::fresh_scratch_dir;
use wayfinder_bench::protected::{
    PROTECTED, check_run, load_hashes, report, save_hashes, snapshot,
};

/// A repo root holding every protected path, with known content.
fn fake_repo(label: &str) -> PathBuf {
    let root = fresh_scratch_dir(label);
    for p in PROTECTED {
        let path = root.join(p.trim_end_matches('/'));
        if p.ends_with('/') {
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join("a.json"), "{\"a\":1}").unwrap();
            fs::write(path.join("b.json"), "{\"b\":2}").unwrap();
        } else {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, format!("original {p}\n")).unwrap();
        }
    }
    root
}

/// Accepts the repo as it stands, returning where the hashes were saved.
fn accept(root: &Path) -> PathBuf {
    let saved = root.join("wf-protected.json");
    save_hashes(&saved, &snapshot(root).unwrap()).unwrap();
    saved
}

#[test]
fn the_protected_list_names_every_file_the_loop_is_judged_by() {
    for needle in [
        "solr-ref/responses/",
        "bench/allowed-diffs.tsv",
        "bench/queries.tsv",
        "bench/weights.tsv",
        "bench/src/score.rs",
    ] {
        assert!(
            PROTECTED.contains(&needle),
            "{needle} should be protected, list was {PROTECTED:?}"
        );
    }
}

#[test]
fn an_untouched_tree_is_allowed_to_score() {
    let root = fake_repo("prot-clean");
    let saved = accept(&root);
    assert_eq!(check_run(&root, &saved), Ok(()));
}

#[test]
fn hashes_round_trip_through_the_saved_file() {
    let root = fake_repo("prot-roundtrip");
    let saved = accept(&root);
    assert_eq!(load_hashes(&saved).unwrap(), Some(snapshot(&root).unwrap()));
    assert_eq!(load_hashes(&root.join("missing.json")).unwrap(), None);
}

#[test]
fn editing_any_protected_file_makes_the_run_refuse_and_names_the_file() {
    for target in [
        "bench/allowed-diffs.tsv",
        "bench/queries.tsv",
        "bench/weights.tsv",
        "bench/src/score.rs",
        "solr-ref/responses/a.json",
    ] {
        let root = fake_repo("prot-edit");
        let saved = accept(&root);
        fs::write(root.join(target), "cheated\n").unwrap();
        let err = check_run(&root, &saved).expect_err(target);
        assert!(err.contains(target), "{target} not named in: {err}");
    }
}

#[test]
fn adding_or_deleting_a_protected_file_is_also_refused() {
    let root = fake_repo("prot-add");
    let saved = accept(&root);
    fs::write(root.join("solr-ref/responses/extra.json"), "{}").unwrap();
    let err = check_run(&root, &saved).unwrap_err();
    assert!(err.contains("solr-ref/responses/extra.json"), "{err}");

    let root = fake_repo("prot-del");
    let saved = accept(&root);
    fs::remove_file(root.join("solr-ref/responses/b.json")).unwrap();
    let err = check_run(&root, &saved).unwrap_err();
    assert!(err.contains("solr-ref/responses/b.json"), "{err}");

    let root = fake_repo("prot-del-file");
    let saved = accept(&root);
    fs::remove_file(root.join("bench/queries.tsv")).unwrap();
    let err = check_run(&root, &saved).unwrap_err();
    assert!(err.contains("bench/queries.tsv"), "{err}");
}

#[test]
fn restoring_the_file_lets_the_run_score_again() {
    let root = fake_repo("prot-restore");
    let saved = accept(&root);
    let q = root.join("bench/queries.tsv");
    let original = fs::read_to_string(&q).unwrap();
    fs::write(&q, "cheated\n").unwrap();
    assert!(check_run(&root, &saved).is_err());
    fs::write(&q, original).unwrap();
    assert_eq!(check_run(&root, &saved), Ok(()));
}

#[test]
fn a_run_with_no_accepted_hashes_is_not_blocked() {
    let root = fake_repo("prot-none");
    assert_eq!(check_run(&root, &root.join("never-accepted.json")), Ok(()));
}

#[test]
fn the_report_names_each_changed_file_and_says_a_person_must_sign_off() {
    let root = fake_repo("prot-report");
    let saved = accept(&root);
    fs::write(root.join("bench/weights.tsv"), "cheated\n").unwrap();
    fs::write(root.join("bench/queries.tsv"), "cheated\n").unwrap();
    let err = check_run(&root, &saved).unwrap_err();
    for f in ["bench/weights.tsv", "bench/queries.tsv"] {
        assert!(err.contains(f), "{f} missing from: {err}");
    }
    assert!(!err.contains("bench/allowed-diffs.tsv"), "{err}");
    assert!(
        err.contains("--accept"),
        "should say how a person re-accepts: {err}"
    );

    let line = report(&["bench/weights.tsv".to_string()]);
    assert!(line.contains("bench/weights.tsv"), "{line}");
    assert!(line.to_ascii_lowercase().contains("protected"), "{line}");
}

#[test]
fn the_tuning_instructions_name_the_protected_files_and_the_refusal() {
    let doc = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("TUNING.md")).unwrap();
    for f in [
        "solr-ref/responses/",
        "allowed-diffs.tsv",
        "queries.tsv",
        "weights.tsv",
        "score.rs",
    ] {
        assert!(doc.contains(f), "TUNING.md should name protected `{f}`");
    }
    assert!(
        doc.to_ascii_lowercase().contains("protected"),
        "TUNING.md should call these files protected"
    );
    assert!(
        doc.contains("refuse"),
        "TUNING.md should say the run refuses to score when they change, and the loop must stop and report it"
    );
}
