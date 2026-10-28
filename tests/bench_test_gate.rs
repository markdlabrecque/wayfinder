//! Issue #444: `wf-bench` refuses to measure a build that fails `cargo test`.
//!
//! The bench crate is standalone (not a workspace member), so the root
//! `cargo test` does not run its tests. This file builds the `wf-bench` driver
//! and runs it as a real process with fake `cargo` and `docker` first on PATH.
//! The fake `cargo` reports a pass or a failure; both fakes log their calls, so
//! the tests can tell whether the run stopped before starting any container.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Builds the driver once, with the real toolchain, and returns its path.
fn driver() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let target = root().join("target/wf-bench-gate");
        let status = Command::new(env!("CARGO"))
            .args(["build", "--bin", "wf-bench", "--manifest-path"])
            .arg(root().join("bench/Cargo.toml"))
            .arg("--target-dir")
            .arg(&target)
            .status()
            .expect("build wf-bench");
        assert!(status.success(), "wf-bench did not build");
        target.join("debug/wf-bench")
    })
}

fn fake(dir: &Path, name: &str, script: &str) {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

struct Run {
    out: Output,
    dir: PathBuf,
}

impl Run {
    fn log(&self, name: &str) -> String {
        fs::read_to_string(self.dir.join(name)).unwrap_or_default()
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn run(tag: &str, cargo_exit: i32) -> Run {
    let bin = driver();
    let dir = std::env::temp_dir().join(format!("wf-bench-gate-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fake(
        &dir,
        "cargo",
        &format!(
            "echo \"$@\" >> \"{d}/cargo.log\"\necho 'test result: FAILED. 1 failed' >&2\nexit {cargo_exit}",
            d = dir.display()
        ),
    );
    fake(
        &dir,
        "docker",
        &format!(
            "echo \"$@\" >> \"{d}/docker.log\"\nexit 1",
            d = dir.display()
        ),
    );
    let path = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new(bin)
        .env("PATH", path)
        .output()
        .expect("run wf-bench");
    Run { out, dir }
}

#[test]
fn failing_tests_stop_the_run_before_measuring_and_say_why() {
    let r = run("fail", 101);
    let stdout = String::from_utf8_lossy(&r.out.stdout);
    assert!(!r.out.status.success(), "expected non-zero exit");
    assert!(stdout.contains("FAIL tests"), "stdout was: {stdout}");
    assert!(
        r.log("cargo.log").contains("test"),
        "cargo test was not run"
    );
    assert_eq!(
        r.log("docker.log"),
        "",
        "docker was used despite failing tests"
    );
    assert!(!stdout.contains("score"), "a score was printed: {stdout}");
}

#[test]
fn passing_tests_let_the_run_continue() {
    let r = run("pass", 0);
    let stdout = String::from_utf8_lossy(&r.out.stdout);
    assert!(
        r.log("cargo.log").contains("test"),
        "cargo test was not run"
    );
    assert!(!stdout.contains("FAIL tests"), "stdout was: {stdout}");
}
