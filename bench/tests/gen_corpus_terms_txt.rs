//! Issue #251: `gen_corpus` additionally writes `<out_dir>/terms.txt`, one
//! term per line, derived from `wayfinder_bench::corpus::query_terms()` --
//! not a second hardcoded word list. Runs the real `gen_corpus` binary
//! (small size, tmp dir) rather than the library directly, since the
//! requirement is specifically about what the *binary* writes to disk.
//!
//! `query_terms()` does not exist yet (see `tests/query_terms.rs`), so this
//! test deliberately does not import it -- it exercises the built
//! `gen_corpus` binary as a subprocess and compares its output against a
//! literal copy of the expected order-stable term list (kept in sync by
//! hand with `tests/query_terms.rs`'s `EXPECTED_ORDER_STABLE_TERMS`), so
//! this file's red state is a genuine runtime failure (missing
//! `terms.txt`), not a compile failure -- it compiles fine against today's
//! `gen_corpus` and just finds `terms.txt` absent.

use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

// Kept in sync by hand with `tests/query_terms.rs`'s
// `EXPECTED_ORDER_STABLE_TERMS` -- see this file's module doc comment for
// why this test does not import `query_terms()` directly. 48 terms: the 56
// raw TITLE_WORDS+BODY_WORDS entries minus "the", "a", "in", "and", "on",
// "to", "of", "for" -- those 8 are text_en stopwords that all analyse to
// the identical empty edismax query and would collide on one
// queryResultCache key (see `query_terms.rs`'s module doc comment).
const EXPECTED_ORDER_STABLE_TERMS: &[&str] = &[
    "rocket",
    "launch",
    "mission",
    "control",
    "orbit",
    "satellite",
    "gravity",
    "engine",
    "capsule",
    "station",
    "voyage",
    "signal",
    "descent",
    "ascent",
    "thruster",
    "payload",
    "quick",
    "brown",
    "fox",
    "jumps",
    "over",
    "lazy",
    "dog",
    "system",
    "returns",
    "result",
    "after",
    "processing",
    "every",
    "record",
    "sequence",
    "verifying",
    "each",
    "field",
    "against",
    "expected",
    "output",
    "before",
    "moving",
    "next",
    "batch",
    "work",
    "items",
    "queued",
    "execution",
    "today",
    "yesterday",
    "tomorrow",
];

fn fresh_out_dir() -> std::path::PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after the epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "wayfinder-bench-gen-corpus-terms-{}-{nanos}-{n}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create temp out dir");
    dir
}

#[test]
fn gen_corpus_writes_terms_txt_matching_query_terms() {
    let out_dir = fresh_out_dir();

    let output = Command::new(env!("CARGO_BIN_EXE_gen_corpus"))
        .args(["42", "50", out_dir.to_str().expect("out_dir is valid utf8")])
        .output()
        .expect("run gen_corpus");
    assert!(
        output.status.success(),
        "gen_corpus failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let terms_path = out_dir.join("terms.txt");
    let terms_contents = fs::read_to_string(&terms_path).unwrap_or_else(|e| {
        panic!(
            "expected gen_corpus to write {} (issue #251); it did not: {e}",
            terms_path.display()
        )
    });

    let written_terms: Vec<&str> = terms_contents
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();

    assert_eq!(
        written_terms, EXPECTED_ORDER_STABLE_TERMS,
        "terms.txt must contain exactly query_terms(), one term per line, in order"
    );

    fs::remove_dir_all(&out_dir).ok();
}
