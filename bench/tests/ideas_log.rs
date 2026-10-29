//! Issue #458: a log of the tuning ideas that were tried.
//!
//! Location decision: the ticket suggests `bench/ideas.tsv`, an append-only
//! file the loop reads before picking a target and appends to after each try.
//! These tests check that the file exists with the agreed columns and that
//! `bench/TUNING.md` tells the agent to read it and to log every try, kept or
//! dropped. They do not judge prose.

use std::fs;
use std::path::{Path, PathBuf};

fn bench_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn tuning() -> String {
    fs::read_to_string(bench_dir().join("TUNING.md")).expect("bench/TUNING.md should exist")
}

/// Body of the `###` section whose heading contains `heading`, up to the next
/// `##` or `###` heading.
fn section(doc: &str, heading: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for line in doc.lines() {
        if line.starts_with("## ") || line.starts_with("### ") {
            if inside {
                break;
            }
            inside = line.contains(heading);
            continue;
        }
        if inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    assert!(!out.is_empty(), "TUNING.md should have a `{heading}` section");
    out
}

#[test]
fn the_ideas_file_exists_with_a_header_naming_each_column() {
    let path = bench_dir().join("ideas.tsv");
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("expected the ideas log at {}", path.display()));
    let header = text
        .lines()
        .find(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .expect("ideas.tsv should have a header row");
    let cols: Vec<String> = header.split('\t').map(|c| c.trim().to_lowercase()).collect();
    for want in ["date", "idea", "metric", "delta", "outcome", "commit"] {
        assert!(
            cols.iter().any(|c| c == want),
            "ideas.tsv header should have a `{want}` column, got {cols:?}"
        );
    }
}

#[test]
fn the_loop_reads_the_log_before_choosing_a_target() {
    let doc = tuning();
    let pick = section(&doc, "Pick a target");
    assert!(
        pick.contains("ideas.tsv"),
        "the `Pick a target` step should tell the agent to read bench/ideas.tsv first"
    );
}

#[test]
fn a_dropped_idea_is_logged_and_survives_the_revert() {
    let doc = tuning();
    let keep = section(&doc, "Keep or revert");
    let reset = keep
        .find("git reset")
        .expect("the revert step should give the git reset command");
    let log = keep
        .rfind("ideas.tsv")
        .expect("the `Keep or revert` step should say to log the dropped idea in ideas.tsv");
    assert!(
        log > reset,
        "the log row must be written after the revert: `git reset --hard` would discard an earlier append to the tracked ideas.tsv"
    );
}

#[test]
fn a_kept_idea_is_logged_with_its_commit() {
    let doc = tuning();
    let commit = section(&doc, "Commit a win");
    assert!(
        commit.contains("ideas.tsv"),
        "the `Commit a win` step should say to log the kept idea, with its commit, in ideas.tsv"
    );
}

#[test]
fn the_instructions_say_what_each_row_records_and_that_the_file_is_append_only() {
    let doc = tuning().to_lowercase();
    for needle in ["append", "delta", "outcome", "kept", "dropped"] {
        assert!(
            doc.contains(needle),
            "TUNING.md should explain the log: missing `{needle}`"
        );
    }
}

#[test]
fn the_final_report_points_at_the_log() {
    let doc = tuning();
    let report = section(&doc, "When the loop stops");
    assert!(
        report.contains("ideas.tsv"),
        "the stop report lists tried ideas; it should say they come from ideas.tsv"
    );
}
