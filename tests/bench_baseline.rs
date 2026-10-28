//! Issue #454: the bench crate is standalone, so the root `cargo test` does not
//! run its tests. This runs the baseline tests in `bench/tests/baseline.rs` so
//! the normal suite fails until the saved baselines exist.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn bench_baseline_tests_pass() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(env!("CARGO"))
        .args(["test", "--test", "baseline", "--manifest-path"])
        .arg(root.join("bench/Cargo.toml"))
        .arg("--target-dir")
        .arg(root.join("target/bench-baseline"))
        .output()
        .expect("run bench baseline tests");
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
