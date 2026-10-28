//! Issue #459: the old benchmark script (`bench/run.sh`) and results page
//! (`bench/RESULTS.md`) are replaced by `wf-bench`, and `bench/README.md`
//! explains how to run the new tool.

use std::path::{Path, PathBuf};

fn bench(file: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("bench")
        .join(file)
}

fn readme() -> String {
    std::fs::read_to_string(bench("README.md")).expect("bench/README.md exists")
}

#[test]
fn old_script_is_removed() {
    assert!(!bench("run.sh").exists(), "bench/run.sh should be removed");
}

#[test]
fn old_results_page_is_removed() {
    assert!(
        !bench("RESULTS.md").exists(),
        "bench/RESULTS.md should be removed"
    );
}

#[test]
fn readme_no_longer_points_at_the_old_tool() {
    let text = readme();
    for old in ["run.sh", "RESULTS.md"] {
        assert!(!text.contains(old), "bench/README.md still mentions {old}");
    }
}

#[test]
fn readme_explains_how_to_run_wf_bench() {
    let text = readme();
    for needed in [
        "wf-bench",
        "fetch-wiki.sh",
        "cargo run --release",
        "--docs",
        "--iters",
        "--cold",
        "--force",
        "bench/out",
        "weights.tsv",
        "docker",
    ] {
        assert!(
            text.contains(needed),
            "bench/README.md should explain the new tool; missing `{needed}`"
        );
    }
}
