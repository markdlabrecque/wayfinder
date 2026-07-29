//! Issue #13: structural checks on the Wayfinder image's Dockerfile,
//! hermetic (no `docker build`, no network) -- these run under plain
//! `cargo test`. The live build-and-measure counterpart is
//! `dockerfile_build.rs`, gated behind `WAYFINDER_BENCH_DOCKER=1`.
//!
//! Location decision (ambiguity flagged, see handoff): the issue doesn't say
//! whether the Dockerfile lives at the repo root or under `bench/`. This
//! repo's canonical container image doubles as the benchmark image, so
//! these tests look for it at the repo root (`../Dockerfile` relative to
//! this crate).

use std::fs;
use std::path::{Path, PathBuf};

fn dockerfile_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("bench/ has a parent directory")
        .join("Dockerfile")
}

#[test]
fn dockerfile_exists_at_the_repo_root() {
    assert!(
        dockerfile_path().exists(),
        "expected a multi-stage Dockerfile at {} for the Wayfinder benchmark/release image",
        dockerfile_path().display()
    );
}

#[test]
fn dockerfile_is_multi_stage_with_a_minimal_final_stage() {
    let contents = fs::read_to_string(dockerfile_path())
        .expect("bench/../Dockerfile should exist and be readable");

    let from_lines: Vec<&str> = contents
        .lines()
        .filter(|l| l.trim_start().to_ascii_uppercase().starts_with("FROM "))
        .collect();
    assert!(
        from_lines.len() >= 2,
        "expected a multi-stage build (>= 2 FROM lines) so build tooling doesn't \
         end up in the shipped image, got: {from_lines:?}"
    );

    let final_from = from_lines
        .last()
        .expect("checked len() >= 2 above")
        .to_ascii_lowercase();
    let minimal_bases = ["scratch", "alpine", "distroless"];
    assert!(
        minimal_bases.iter().any(|b| final_from.contains(b)),
        "final stage should use a minimal base image (scratch/alpine/distroless) \
         to hit the < 30 MB target (PRD §8), got: {final_from}"
    );
}

#[test]
fn dockerfile_copies_a_statically_linked_binary_from_the_builder_stage() {
    let contents = fs::read_to_string(dockerfile_path())
        .expect("bench/../Dockerfile should exist and be readable");
    let lower = contents.to_ascii_lowercase();

    assert!(
        lower.contains("copy --from="),
        "expected a `COPY --from=<builder-stage>` instruction carrying the \
         compiled binary into the final stage"
    );
    assert!(
        lower.contains("musl"),
        "expected a static-linking target (musl) so the final binary has no \
         dynamic libc dependency -- required for a `scratch`/`distroless` final stage"
    );
}
