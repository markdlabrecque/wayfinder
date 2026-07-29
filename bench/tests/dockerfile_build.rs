//! Live counterpart of `dockerfile_lint.rs`, gated by `WAYFINDER_BENCH_DOCKER=1`
//! exactly like `tests/differential.rs`'s `WAYFINDER_DIFF_SOLR=1` live mode in
//! the main `wayfinder` crate: a plain `#[test]` that no-ops under default
//! `cargo test` (gated by the env var alone, not `#[ignore]` as well), so it
//! never needs Docker or network to keep the suite hermetic.
//!
//! Actually builds the image and checks it lands under the PRD §8 target of
//! < 30 MB.

use std::path::Path;
use std::process::Command;

#[test]
fn built_image_is_under_the_30mb_target() {
    if std::env::var("WAYFINDER_BENCH_DOCKER").ok().as_deref() != Some("1") {
        eprintln!(
            "skipping live docker build of the benchmark image: set \
             WAYFINDER_BENCH_DOCKER=1 to enable (WAYFINDER_BENCH_DOCKER=1 \
             cargo test --manifest-path bench/Cargo.toml --test dockerfile_build)"
        );
        return;
    }

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .parent()
        .expect("bench/ has a parent directory");
    let dockerfile = repo_root.join("Dockerfile");
    let tag = "wayfinder-bench-image:test";

    let status = Command::new("docker")
        .arg("build")
        .arg("-f")
        .arg(&dockerfile)
        .arg("-t")
        .arg(tag)
        .arg(repo_root)
        .status()
        .expect("docker build should run (is docker installed and on PATH?)");
    assert!(
        status.success(),
        "docker build failed for {}",
        dockerfile.display()
    );

    let output = Command::new("docker")
        .arg("inspect")
        .arg("--format={{.Size}}")
        .arg(tag)
        .output()
        .expect("docker inspect should run");
    assert!(
        output.status.success(),
        "docker inspect failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let size_bytes: u64 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .expect("docker inspect --format={{.Size}} should print an integer byte count");

    const THIRTY_MB: u64 = 30 * 1024 * 1024;
    assert!(
        size_bytes < THIRTY_MB,
        "benchmark image is {size_bytes} bytes, target is < {THIRTY_MB} bytes (30 MB, PRD §8)"
    );
}
