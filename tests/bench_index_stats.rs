//! Issue #449: indexing throughput (docs/sec, bytes/sec) is recorded for the
//! full corpus. The bench crate is not part of the root package, so a plain
//! `cargo test` never runs its tests. This runs the bench crate's
//! `index_stats` tests so the root suite goes red until the feature exists.

use std::process::Command;

#[test]
fn bench_index_stats_tests_pass() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/bench/Cargo.toml");
    let output = Command::new(cargo)
        .args(["test", "--manifest-path", manifest, "--test", "index_stats"])
        .output()
        .expect("failed to run cargo for the bench crate");

    assert!(
        output.status.success(),
        "bench index_stats tests failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
