//! Issue #448: runs the anon-peak suite (`bench/tests/memory_peak.rs`) through
//! the bench manifest so the root test command covers it, the same way
//! `bench_memory.rs` does for the rest of the memory suite.

use std::path::Path;
use std::process::Command;

#[test]
fn bench_memory_peak_suite_passes() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("bench/Cargo.toml");
    let out = Command::new(env!("CARGO"))
        .args(["test", "--offline", "--manifest-path"])
        .arg(&manifest)
        .args(["--test", "memory_peak"])
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .expect("spawn cargo");
    assert!(
        out.status.success(),
        "bench memory peak tests failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
