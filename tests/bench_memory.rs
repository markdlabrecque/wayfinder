//! Issue #448: `bench/` is a standalone crate, so a bare `cargo test` at the
//! repo root never reaches its tests. This runs the memory suite
//! (`bench/tests/memory.rs`) through the bench manifest so the root test
//! command covers it, the same way `bench_footprint.rs` does for #451.

use std::path::Path;
use std::process::Command;

#[test]
fn bench_memory_suite_passes() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("bench/Cargo.toml");
    let out = Command::new(env!("CARGO"))
        .args(["test", "--offline", "--manifest-path"])
        .arg(&manifest)
        .args(["--test", "memory"])
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .expect("spawn cargo");
    assert!(
        out.status.success(),
        "bench memory tests failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
