//! Issue #62 (defect 1): `bench/run.sh`'s `solr_mem_mb()` parses
//! `docker stats --format '{{.MemUsage}}'` output like `2.10GiB`. Today it
//! only recognizes `GiB` and `KiB` explicitly and silently treats anything
//! else -- including plain bytes (`B`) and `TiB` -- as if it were `MiB`,
//! which corrupts the reported max by ~1000x (B) or ~1,000,000x (TiB). See
//! docs/../ (issue #62) for the worked examples this test reproduces.
//!
//! These tests extract the real `solr_mem_mb` function out of `run.sh`
//! (see `tests/support/mod.rs`) and run it against a stubbed `docker` on
//! `PATH`, so no container is ever started -- hermetic, no Docker daemon
//! needed to run this suite.
//!
//! Expected fixed behavior (per the issue): `B`, `KiB`, `MiB`, `GiB`, `TiB`
//! all parse to the correct MB-equivalent value, and any *other* unit
//! causes a loud failure (non-zero exit), not a silently-wrong number.

mod support;

use support::{
    extract_bash_function, fresh_scratch_dir, run_bash, run_sh_source, write_executable,
};

const DOCKER_STUB: &str = r#"#!/usr/bin/env bash
set -euo pipefail
if [ "$1" = "stats" ]; then
  echo "$MOCK_MEM_USAGE"
  exit 0
fi
echo "mem_units test docker stub: unexpected invocation: $*" >&2
exit 1
"#;

/// Runs the real `solr_mem_mb` function extracted from `run.sh` against a
/// stubbed `docker stats` reporting `mem_usage_field` (e.g. `"2.10GiB / 4GiB"`,
/// matching docker's real no-space-before-unit formatting).
fn run_solr_mem_mb(mem_usage_field: &str) -> std::process::Output {
    let source = run_sh_source();
    let func = extract_bash_function(&source, "solr_mem_mb").expect(
        "run.sh should define a `solr_mem_mb` function (defect 1's parsing lives here); \
         if it's been renamed, update this test's extraction target",
    );

    let dir = fresh_scratch_dir("mem-units");
    write_executable(&dir, "docker", DOCKER_STUB);

    let script = format!("SOLR_CONTAINER=test-container\n{func}\nsolr_mem_mb\n");
    run_bash(&script, &dir, &[("MOCK_MEM_USAGE", mem_usage_field)])
}

fn stdout_trimmed(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

// --- already-recognized units: regression guard, currently green -------

#[test]
fn gib_parses_correctly() {
    let out = run_solr_mem_mb("2.10GiB / 4GiB");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout_trimmed(&out), "2150.40");
}

#[test]
fn kib_parses_correctly() {
    let out = run_solr_mem_mb("512.00KiB / 4GiB");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout_trimmed(&out), "0.50");
}

#[test]
fn mib_parses_correctly() {
    let out = run_solr_mem_mb("512.00MiB / 4GiB");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout_trimmed(&out), "512.00");
}

// --- defect 1: currently-mishandled units, red until fixed --------------

#[test]
fn plain_bytes_parse_as_bytes_not_as_mib() {
    // 2097152 B == exactly 2 MiB. Today this falls through to the `else`
    // branch (no GiB/KiB match), which strips nothing and hands the raw
    // "2097152.00B" token to awk's numeric context -- printing "2097152.00"
    // instead of "2.00", a ~1,000,000x over-report.
    let out = run_solr_mem_mb("2097152.00B / 4GiB");
    assert!(
        out.status.success(),
        "expected `B` to be a recognized unit, not a failure; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        stdout_trimmed(&out),
        "2.00",
        "2097152.00B is exactly 2 MiB; solr_mem_mb must scale bytes down, \
         not silently pass the raw number through as if it were already MiB"
    );
}

#[test]
fn tib_parses_correctly_not_as_mib() {
    // 1.50 TiB == 1.50 * 1024 * 1024 MiB == 1572864.00. Today this falls
    // through to the `else` branch and prints "1.50" (as if TiB == MiB),
    // a ~1,000,000x under-report -- the exact case named in issue #62.
    let out = run_solr_mem_mb("1.50TiB / 4TiB");
    assert!(
        out.status.success(),
        "expected `TiB` to be a recognized unit, not a failure; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        stdout_trimmed(&out),
        "1572864.00",
        "1.50TiB must scale up to its MiB-equivalent, not be treated as 1.50 MiB"
    );
}

#[test]
fn unrecognized_unit_fails_loudly_instead_of_defaulting_to_mib() {
    // No real Solr JVM reports memory in PiB, but the point of this test is
    // exactly that: an unrecognized unit must not silently default to MiB
    // scaling and corrupt the reported max -- it must fail loudly.
    let out = run_solr_mem_mb("3.00PiB / 4PiB");
    assert!(
        !out.status.success(),
        "an unrecognized memory unit (PiB) must cause solr_mem_mb to fail \
         loudly (non-zero exit), not silently print a wrong value; got \
         stdout={:?} status={:?}",
        stdout_trimmed(&out),
        out.status
    );
}
