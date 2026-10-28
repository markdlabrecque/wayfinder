//! The bench crate is standalone, so the root `cargo test` never builds it.
//! This runs its add-to-searchable tests (issue #450, `bench/tests/freshness.rs`)
//! so they gate the same command as everything else.

use std::process::Command;

#[test]
fn bench_add_to_searchable_tests_pass() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args(["test", "--manifest-path", "bench/Cargo.toml", "--test", "freshness"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run cargo");
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
