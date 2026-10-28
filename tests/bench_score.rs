//! Issue #452: the bench crate is standalone, so the root `cargo test` does not
//! run its tests. This runs the weighted-score tests in `bench/tests/score.rs`
//! so the normal suite fails until the score and `bench/weights.tsv` exist.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn bench_score_tests_pass() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(env!("CARGO"))
        .args(["test", "--test", "score", "--manifest-path"])
        .arg(root.join("bench/Cargo.toml"))
        .arg("--target-dir")
        .arg(root.join("target/bench-score"))
        .output()
        .expect("run bench score tests");
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
