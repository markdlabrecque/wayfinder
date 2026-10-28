//! The standalone `bench` crate is not part of this workspace, so a bare
//! `cargo test` never runs its tests. This runs the steady-machine tests for
//! wf-bench (issue #455, `bench/tests/machine.rs`) so they gate the root run too.

use std::process::Command;

#[test]
fn bench_machine_tests_pass() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args([
            "test",
            "--manifest-path",
            "bench/Cargo.toml",
            "--test",
            "machine",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run cargo");
    assert!(
        out.status.success(),
        "bench machine tests failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
