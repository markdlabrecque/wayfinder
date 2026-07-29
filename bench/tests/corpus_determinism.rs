//! Issue #13: the corpus generator must be deterministic -- same seed, same
//! doc count, byte-identical output -- so a benchmark run is reproducible
//! and two runs of the same command diff to nothing but timing.
//!
//! Exercises the 50k-doc case's *logic* directly (no Tantivy, no disk, no
//! Docker); the 2M corpus is out of scope for a unit test per the task spec.

use wayfinder_bench::corpus::{content_hash, generate};

const FIFTY_K: usize = 50_000;

#[test]
fn same_seed_and_size_produce_byte_identical_corpora() {
    let a = generate(42, FIFTY_K);
    let b = generate(42, FIFTY_K);
    assert_eq!(
        a, b,
        "same seed + size must reproduce the exact same corpus"
    );
}

#[test]
fn same_seed_and_size_produce_identical_content_hash() {
    let a = generate(42, FIFTY_K);
    let b = generate(42, FIFTY_K);
    assert_eq!(
        content_hash(&a),
        content_hash(&b),
        "content_hash must be a pure function of the generated docs"
    );
}

#[test]
fn generated_doc_count_matches_the_requested_size() {
    let docs = generate(42, FIFTY_K);
    assert_eq!(docs.len(), FIFTY_K);
}

#[test]
fn different_seeds_produce_different_corpora() {
    let a = generate(1, 1_000);
    let b = generate(2, 1_000);
    assert_ne!(a, b, "different seeds must not collide on the same corpus");
    assert_ne!(content_hash(&a), content_hash(&b));
}

#[test]
fn different_sizes_produce_different_content_hash() {
    let a = generate(42, 1_000);
    let b = generate(42, 2_000);
    assert_ne!(
        content_hash(&a),
        content_hash(&b),
        "doc count is part of what content_hash must capture"
    );
}

#[test]
fn ids_are_unique_across_the_generated_corpus() {
    let docs = generate(42, FIFTY_K);
    let unique_ids: std::collections::HashSet<&str> = docs.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(
        unique_ids.len(),
        docs.len(),
        "every generated doc must have a unique id"
    );
}
