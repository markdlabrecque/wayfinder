//! Issue #251: `bench/src/corpus.rs::query_terms()` exposes the corpus's
//! query vocabulary so the cold pass (`bench/run.sh`) can query terms that
//! provably exist in the corpus without a second hardcoded word list in
//! shell.
//!
//! Premise check (spec's "Premises to verify", item 1): the spec guesses
//! "~7 collisions" between `TITLE_WORDS` (16 words) and `BODY_WORDS` (40
//! words). Checked directly against the word lists themselves, there are
//! zero overlapping words -- but that isn't the property that matters.
//! Solr's `queryResultCache` keys on the *parsed* query, not the `q`
//! string, and verified live against a real Solr 9 with
//! `defType=edismax&qf=title body&debugQuery=true`: 8 of the 56 raw terms
//! (`the`, `a`, `in`, `and`, `on`, `to`, `of`, `for`) are `text_en`
//! stopwords that all analyse to the identical empty parsed query `+()`
//! (`numFound` 0). Those 8 collapse onto one cache key -- the first insert,
//! the other seven cache hits -- which is exactly the ~8 hits per pass an
//! earlier scratch harness saw and misattributed to duplicate list entries.
//! They are also useless as benchmark queries: they match nothing.
//!
//! `query_terms()` must therefore exclude those 8 stopwords, yielding a
//! true distinct-query count of **48**, confirmed live: a cold pass over
//! the 48 terms reports `lookups=49 hits=0` against Solr's
//! `admin/mbeans?cat=CACHE&stats=true&wt=json`.
//!
//! `query_terms()` does not exist yet, so every test in this file is
//! currently a **compile-time** red, not a runtime assertion failure (the
//! one exception the test-writer's brief allows: a test that cannot compile
//! without the function existing is left failing at compile time rather
//! than stubbed).

use wayfinder_bench::corpus::{generate, query_terms};

const TRUE_DISTINCT_TERM_COUNT: usize = 48;

// ponytail: this exclusion list is a property of Solr's `text_en` analyser
// (its English stopword set), which `corpus.rs` cannot introspect from
// Rust -- it was derived empirically against a live Solr 9 with
// `debugQuery=true` (see this file's module doc comment), not computed.
// If `text_en`'s stopword set ever changes, or either word list gains a
// new stopword, this hardcoded list silently drifts from reality; the
// `query_terms_excludes_the_known_stopword_collisions` test below is the
// guard against that drift being dropped quietly, not a guarantee it can't
// happen -- it only catches the *removal* of the exclusion, not a stopword
// set change on Solr's side.
const KNOWN_STOPWORD_COLLISIONS: &[&str] = &["the", "a", "in", "and", "on", "to", "of", "for"];

// Literal copy of `TITLE_WORDS` then `BODY_WORDS`, minus
// `KNOWN_STOPWORD_COLLISIONS`, as they exist today in `bench/src/corpus.rs`
// (both source consts are private, so a test outside the crate can't
// reference them directly). This is the order-stable dedup's expected
// output: the two source lists don't overlap with each other, but 8 of
// their words collide on Solr's parsed-empty-query cache key and so must
// be excluded.
const EXPECTED_ORDER_STABLE_TERMS: &[&str] = &[
    // TITLE_WORDS (no stopword collisions in this list)
    "rocket",
    "launch",
    "mission",
    "control",
    "orbit",
    "satellite",
    "gravity",
    "engine",
    "capsule",
    "station",
    "voyage",
    "signal",
    "descent",
    "ascent",
    "thruster",
    "payload",
    // BODY_WORDS, with "the", "a", "in", "and", "on", "to", "of", "for"
    // excluded (KNOWN_STOPWORD_COLLISIONS)
    "quick",
    "brown",
    "fox",
    "jumps",
    "over",
    "lazy",
    "dog",
    "system",
    "returns",
    "result",
    "after",
    "processing",
    "every",
    "record",
    "sequence",
    "verifying",
    "each",
    "field",
    "against",
    "expected",
    "output",
    "before",
    "moving",
    "next",
    "batch",
    "work",
    "items",
    "queued",
    "execution",
    "today",
    "yesterday",
    "tomorrow",
];

#[test]
fn query_terms_is_non_empty() {
    assert!(!query_terms().is_empty());
}

#[test]
fn query_terms_has_no_duplicates() {
    let terms = query_terms();
    let unique: std::collections::HashSet<&str> = terms.iter().copied().collect();
    assert_eq!(
        unique.len(),
        terms.len(),
        "query_terms() must be deduplicated; got duplicates in {terms:?}"
    );
}

#[test]
fn query_terms_true_distinct_count_is_48() {
    // Premise 1 correction (revised): zero literal overlap between
    // TITLE_WORDS and BODY_WORDS, but 8 raw terms are text_en stopwords
    // that all parse to the same empty edismax query and so collide on one
    // queryResultCache key -- see this file's module doc comment. Excluding
    // those 8 brings the true distinct-*query* count to 48, verified live
    // against Solr 9 (cold pass: lookups=49 hits=0).
    let terms = query_terms();
    assert_eq!(
        terms.len(),
        TRUE_DISTINCT_TERM_COUNT,
        "expected 48 terms (56 raw, minus 8 stopwords that collapse onto Solr's parsed-empty-\
         query cache key); got {} terms: {terms:?}",
        terms.len()
    );
}

#[test]
fn query_terms_excludes_the_known_stopword_collisions() {
    let terms = query_terms();
    for stopword in KNOWN_STOPWORD_COLLISIONS {
        assert!(
            !terms.contains(stopword),
            "query_terms() must exclude {stopword:?}: it's a text_en stopword that parses to \
             the same empty query as the other stopwords in {KNOWN_STOPWORD_COLLISIONS:?}, so \
             including it would make the cold pass not cold (it would cache-hit on whichever \
             stopword was queried first); got terms: {terms:?}"
        );
    }
}

#[test]
fn query_terms_is_title_words_then_body_words_order_stable() {
    let terms = query_terms();
    assert_eq!(
        terms, EXPECTED_ORDER_STABLE_TERMS,
        "query_terms() must yield TITLE_WORDS then BODY_WORDS, in that order, deduplicated, \
         with the known stopword collisions excluded"
    );
}

#[test]
fn every_query_term_appears_in_a_generated_corpus() {
    // Hermetic: no Docker, no network. Seed 42 at even a small size covers
    // every term in both word lists (confirmed empirically against the
    // current generator before writing this test -- see the test-writer
    // report); if that ever regresses, this test should fail loudly rather
    // than pass on a partial coverage fluke.
    let docs = generate(42, 50);
    let mut seen_words: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for doc in &docs {
        seen_words.extend(doc.title.split_whitespace());
        seen_words.extend(doc.body.split_whitespace());
    }

    let terms = query_terms();
    let missing: Vec<&&str> = terms.iter().filter(|t| !seen_words.contains(**t)).collect();
    assert!(
        missing.is_empty(),
        "every term query_terms() returns must provably appear in the corpus; missing: \
         {missing:?}"
    );
}
