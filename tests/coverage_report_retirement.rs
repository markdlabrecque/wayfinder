//! Retirement contract for the removed Search API coverage report (#392).

use std::path::Path;
use std::process::Command;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn search_api_coverage_report_surface_remains_retired() {
    let mut remaining = Vec::new();

    for path in [
        "src/coverage.rs",
        "coverage/search_api_coverage_contract.json",
        "tests/search_api_coverage.rs",
    ] {
        if root().join(path).exists() {
            remaining.push(path.to_owned());
        }
    }

    let lib = std::fs::read_to_string(root().join("src/lib.rs")).expect("read library source");
    if lib.contains("mod coverage;") || lib.contains("coverage_report") {
        remaining.push("public coverage_report API".to_owned());
    }

    let coverage = Command::new(env!("CARGO_BIN_EXE_wayfinder"))
        .args(["coverage", "--format", "json"])
        .output()
        .expect("run wayfinder coverage command");
    if coverage.status.success() {
        remaining.push("wayfinder coverage --format json CLI".to_owned());
    }

    assert!(
        remaining.is_empty(),
        "Search API coverage reporting must remain retired; still present: {}",
        remaining.join(", ")
    );
}
