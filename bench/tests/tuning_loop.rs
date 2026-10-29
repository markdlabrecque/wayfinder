//! Issue #456: written instructions for the automated tuning loop.
//!
//! Location decision: the ticket says "a skill or prompt file in the repo"
//! without a path, so these tests read `bench/TUNING.md`, next to the README
//! that describes the benchmark the loop drives.
//!
//! The tests check that the file names each step an agent needs, using the
//! real commands, flags and files `wf-bench` has. They do not judge prose.

use std::fs;
use std::path::{Path, PathBuf};

fn bench_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn tuning() -> String {
    let path = bench_dir().join("TUNING.md");
    fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("expected loop instructions at {}", path.display()))
}

fn assert_mentions(doc: &str, needles: &[&str], why: &str) {
    for n in needles {
        assert!(doc.contains(n), "TUNING.md should mention `{n}`: {why}");
    }
}

#[test]
fn instructions_tell_the_agent_how_to_run_and_read_the_benchmark() {
    let doc = tuning();
    assert_mentions(
        &doc,
        &[
            "cargo build --release",
            "wf-bench",
            "bench/out/",
            "weights.tsv",
        ],
        "the agent must rebuild, run the benchmark, read the JSON detail, and pick a target by metric weight",
    );
    assert!(
        doc.contains("score") && doc.contains("ratios"),
        "TUNING.md should say to read the one-line score and the per-metric `ratios` in the JSON"
    );
}

#[test]
fn instructions_list_every_gate_a_change_must_pass() {
    let doc = tuning();
    assert_mentions(
        &doc,
        &[
            "cargo fmt --check",
            "cargo clippy --all-targets -- -D warnings",
            "cargo test",
        ],
        "a change is only kept when the repo's gates are green",
    );
    assert!(
        doc.contains("correct"),
        "TUNING.md should name the correctness check (answers still match Solr)"
    );
}

#[test]
fn instructions_say_how_to_revert_a_failed_try() {
    let doc = tuning();
    assert!(
        ["git checkout", "git restore", "git reset"]
            .iter()
            .any(|c| doc.contains(c)),
        "TUNING.md should give the exact git command that throws a failed try away"
    );
}

#[test]
fn instructions_use_the_noise_verdict_to_decide_keep_or_revert() {
    let doc = tuning();
    assert_mentions(
        &doc,
        &["win", "loss", "noise", "--noise", "--accept"],
        "keep only a `win`, revert `loss` and `noise`, and move the accepted baseline with --accept after a keep",
    );
}

#[test]
fn instructions_give_a_commit_message_format_carrying_the_score_change() {
    let doc = tuning();
    let in_fence = doc.split("```").skip(1).step_by(2).any(|block| {
        block.contains("->") && block.to_ascii_lowercase().contains("score")
    });
    assert!(
        in_fence,
        "TUNING.md should show a commit message in a fenced block with the score change as `old -> new`"
    );
    assert!(
        doc.contains("perf("),
        "TUNING.md should use a conventional-commit type for a kept win (CLAUDE.md: Conventions), e.g. `perf(scope):`"
    );
    assert!(
        doc.contains("ASCII"),
        "TUNING.md should remind the agent that commit messages are ASCII-only"
    );
}

#[test]
fn instructions_state_the_stop_rule() {
    let doc = tuning();
    let lower = doc.to_ascii_lowercase();
    assert!(
        lower.contains("in a row") || lower.contains("consecutive"),
        "TUNING.md should stop after N failed tries in a row"
    );
    assert!(
        doc.contains("N ") || doc.contains("`N`"),
        "TUNING.md should name N so the operator knows what to set"
    );
    assert!(
        lower.contains("target"),
        "TUNING.md should stop when the score reaches the target"
    );
    assert!(
        lower.contains("stop"),
        "TUNING.md should say plainly when to stop"
    );
}

#[test]
fn the_bench_readme_points_at_the_instructions() {
    let readme = fs::read_to_string(bench_dir().join("README.md")).unwrap();
    assert!(
        readme.contains("TUNING.md"),
        "bench/README.md should link to TUNING.md so the loop instructions are findable"
    );
}
