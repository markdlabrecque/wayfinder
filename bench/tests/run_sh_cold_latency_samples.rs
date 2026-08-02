//! Issue #254: a cold p95 must be based on the complete distinct-term pass.
//!
//! `run.sh` is side-effectful, so these hermetic tests execute the required
//! validation seam and source-guard both engine call sites. The established
//! corpus contract is 48 distinct terms (see `query_terms.rs`): equality with
//! a terms file alone is insufficient, because a truncated two-line terms
//! file and a matching two-line latency file would otherwise pass.

mod support;

use std::fs;

use support::{
    extract_bash_function, fresh_scratch_dir, run_bash, run_sh_source, strip_function_bodies,
};

const DISTINCT_COLD_TERM_COUNT: usize = 48;

fn lines(count: usize, prefix: &str) -> String {
    (0..count)
        .map(|n| format!("{prefix}{n}\n"))
        .collect::<String>()
}

fn assert_sample_count(terms: usize, latencies: usize) -> std::process::Output {
    let source = run_sh_source();
    let function = extract_bash_function(&source, "assert_cold_latency_sample_count").expect(
        "run.sh must define assert_cold_latency_sample_count terms_file latency_file so a cold \
         p95 cannot be rendered from an incomplete sample",
    );
    let dir = fresh_scratch_dir("cold-latency-sample-count");
    let terms_path = dir.join("terms.txt");
    let latency_path = dir.join("latencies.txt");
    fs::write(&terms_path, lines(terms, "term-")).expect("write terms file");
    fs::write(&latency_path, lines(latencies, "1.")).expect("write latency file");

    let script = format!(
        "{function}\nassert_cold_latency_sample_count '{}' '{}'\n",
        terms_path.display(),
        latency_path.display()
    );
    run_bash(&script, &dir, &[])
}

#[test]
fn rejects_two_cold_samples_even_when_terms_txt_is_equally_truncated() {
    let out = assert_sample_count(2, 2);
    assert!(
        !out.status.success(),
        "two cold samples must fail even when terms.txt also has two lines: a 1-2 sample p95 \
         is not a benchmark measurement; stdout: {:?}; stderr: {:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn rejects_a_latency_file_whose_line_count_differs_from_the_48_term_corpus() {
    let out = assert_sample_count(DISTINCT_COLD_TERM_COUNT, DISTINCT_COLD_TERM_COUNT - 1);
    assert!(
        !out.status.success(),
        "the cold latency file must have one line per terms.txt entry; stdout: {:?}; stderr: {:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn accepts_a_complete_48_term_cold_sample() {
    let out = assert_sample_count(DISTINCT_COLD_TERM_COUNT, DISTINCT_COLD_TERM_COUNT);
    assert!(
        out.status.success(),
        "the complete established 48-term cold sample must remain valid; stderr: {}",
        String::from_utf8_lossy(&out.stderr),
    );
}

fn call_lines<'a>(source: &'a str, function: &str) -> Vec<(usize, &'a str)> {
    source
        .lines()
        .enumerate()
        .filter_map(|(line_no, line)| {
            let trimmed = line.trim_start();
            (trimmed.contains(function)
                && !trimmed.starts_with('#')
                && !trimmed.starts_with(&format!("{function}()")))
            .then_some((line_no, line))
        })
        .collect()
}

#[test]
fn both_engine_cold_passes_validate_their_latency_file_against_terms_txt() {
    let source = run_sh_source();
    let stripped = strip_function_bodies(&source);
    let cold_passes = call_lines(&stripped, "run_cold_query_pass");
    let validations = call_lines(&stripped, "assert_cold_latency_sample_count");

    assert_eq!(
        cold_passes.len(),
        2,
        "expected exactly the Wayfinder and Solr cold-pass call sites, found {cold_passes:?}"
    );
    assert_eq!(
        validations.len(),
        2,
        "expected one cold sample-count validation after each engine's cold pass, found \
         {validations:?}"
    );
    for ((cold_line, _), (validation_line, validation)) in cold_passes.iter().zip(&validations) {
        assert!(
            cold_line < validation_line,
            "the cold sample-count validation must run after its cold pass; cold at line \
             {cold_line}, validation at line {validation_line}: {validation}"
        );
        assert!(
            validation.contains("TERMS_FILE") && validation.contains("LATENCIES_COLD"),
            "each validation must compare terms.txt with that engine's cold latency file, got: \
             {validation}"
        );
    }
}
