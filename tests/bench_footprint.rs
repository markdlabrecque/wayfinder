//! Issue #451: `bench/` is a standalone crate, so a bare `cargo test` at the
//! repo root never reaches its tests. This runs the footprint suite
//! (`bench/tests/footprint.rs`) through the bench manifest so the root test
//! command covers it, the same way `bench_cold_start.rs` does for #446.

use std::path::Path;
use std::process::Command;

#[test]
fn bench_footprint_suite_passes() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("bench/Cargo.toml");
    let out = Command::new(env!("CARGO"))
        .args(["test", "--offline", "--manifest-path"])
        .arg(&manifest)
        .args(["--test", "footprint"])
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .expect("spawn cargo");
    assert!(
        out.status.success(),
        "bench footprint tests failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
