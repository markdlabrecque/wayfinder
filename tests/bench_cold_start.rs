//! Issue #446: `bench/` is a standalone crate, so a bare `cargo test` at the
//! repo root never reaches its tests. This runs the cold-start suite
//! (`bench/tests/cold_start.rs`) through the bench manifest so the root test
//! command covers it, as CI's `cargo test --manifest-path bench/Cargo.toml` does.

use std::path::Path;
use std::process::Command;

#[test]
fn bench_cold_start_suite_passes() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("bench/Cargo.toml");
    let out = Command::new(env!("CARGO"))
        .args(["test", "--offline", "--manifest-path"])
        .arg(&manifest)
        .args(["--test", "cold_start"])
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .expect("spawn cargo");
    assert!(
        out.status.success(),
        "bench cold_start tests failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
