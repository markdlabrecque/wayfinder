//! Unit coverage for the `mm` (minimum-should-match) grammar (issue #7, PRD
//! §5 v1 exception): "a small self-contained parser... implement it fully."
//!
//! Per the issue spec this is a pure function — `(grammar string, clause
//! count) -> required count` — with no HTTP surface and no Solr fixture of
//! its own (a pure grammar-to-integer function needs no live-Solr ground
//! truth). The *values* in the table below are not taken from memory of
//! Solr's reference-guide prose, though: they were verified against a real,
//! disposable Solr 9 (finding 68, `docs/solr-ref-findings.md`) because two
//! independently-plausible readings of the grammar disagree on at least one
//! entry (`-25%` at `clause_count=3` — floor gives 3, a ceiling-based
//! "N% may be missing" reading gives 2; real Solr returns 3). Every row
//! below is the real, captured answer.
//!
//! ## The contract this test assumes
//!
//! `wayfinder::edismax::min_should_match(spec: &str, clause_count: usize) ->
//! usize`. No such module exists yet (`grep -rn "mod edismax" src/` is
//! empty) — this file is expected to fail to compile until the implementor
//! adds `pub mod edismax;` with that function. That is the correct red
//! state per this task's own boundary: a missing module/function, not a
//! stub written here to paper over it.
//!
//! ## The algorithm, spelled out (finding 68)
//!
//! - No spec, or a spec with no `<` in it applied as the *only* token:
//!   still starts from a default of `clause_count` (all required) before
//!   any override is applied.
//! - A `X<Y` pair: if `clause_count > X`, `result` becomes `apply(Y,
//!   clause_count)` and parsing continues to the next pair (a later pair
//!   can override again); if `clause_count <= X`, the pair is skipped
//!   (`result` unchanged) and parsing continues — this is the
//!   easy-to-get-backwards part: `clause_count <= X` means *do not apply*
//!   this pair's value, not "apply it because we're under the threshold".
//! - `apply(spec, n)` for a bare (non-conditional) token:
//!   - positive integer `Y`: `min(Y, n)`.
//!   - negative integer `-Y`: `max(0, n - Y)`.
//!   - positive percentage `P%`: `floor(P * n / 100)`.
//!   - negative percentage `-P%`: `n - floor(P * n / 100)` (floor on both
//!     signs — not ceiling for the negative side).

use wayfinder::edismax::min_should_match;

#[track_caller]
fn assert_mm(spec: &str, clause_count: usize, expected: usize) {
    assert_eq!(
        min_should_match(spec, clause_count),
        expected,
        "min_should_match({spec:?}, {clause_count}) should be {expected}"
    );
}

// --- bare absolute counts ----------------------------------------------------

#[test]
fn positive_absolute_is_a_fixed_required_count() {
    assert_mm("1", 3, 1);
}

#[test]
fn positive_absolute_larger_than_clause_count_clamps_to_clause_count() {
    assert_mm("5", 3, 3);
}

#[test]
fn negative_absolute_subtracts_from_clause_count() {
    assert_mm("-1", 3, 2);
}

#[test]
fn negative_absolute_more_negative_than_clause_count_clamps_to_zero() {
    assert_mm("-5", 3, 0);
}

#[test]
fn zero_requires_no_clause() {
    assert_mm("0", 3, 0);
}

// --- bare percentages ---------------------------------------------------------

#[test]
fn positive_percentage_floors() {
    // floor(0.75 * 3) = floor(2.25) = 2, not round-to-nearest (which would
    // also give 2 here, but `50%`/3 below disambiguates from rounding).
    assert_mm("75%", 3, 2);
}

#[test]
fn positive_percentage_of_three_at_fifty_percent_floors_not_rounds() {
    // floor(0.5 * 3) = floor(1.5) = 1. A round-to-nearest reading would give
    // 2 (or 1 with round-half-down) -- this disambiguates floor from
    // round-half-up specifically.
    assert_mm("50%", 3, 1);
}

#[test]
fn positive_percentage_of_four_at_fifty_percent() {
    assert_mm("50%", 4, 2);
}

#[test]
fn positive_percentage_of_five_at_thirty_three_percent_floors() {
    // floor(0.33 * 5) = floor(1.65) = 1.
    assert_mm("33%", 5, 1);
}

#[test]
fn one_hundred_percent_requires_every_clause() {
    assert_mm("100%", 3, 3);
}

#[test]
fn negative_percentage_uses_floor_on_the_missing_count_not_ceiling() {
    // This is the case memory gets wrong (finding 68): floor(0.25 * 3) = 0
    // clauses may be missing, so all 3 are required. A ceiling-based "1
    // clause may be missing" reading would give 2 -- that is NOT what real
    // Solr does.
    assert_mm("-25%", 3, 3);
}

#[test]
fn negative_percentage_of_eight_at_twenty_five_percent() {
    // n - floor(0.25 * 8) = 8 - 2 = 6.
    assert_mm("-25%", 8, 6);
}

#[test]
fn negative_one_hundred_percent_clamps_to_zero() {
    // n - floor(1.0 * 3) = 3 - 3 = 0.
    assert_mm("-100%", 3, 0);
}

// --- conditional lists (`X<Y X<Y ...`) ---------------------------------------

#[test]
fn conditional_list_at_the_first_threshold_boundary() {
    // "3<-1 10<-2" at clause_count=3: first pair's threshold is 3, and
    // clause_count(3) is NOT > 3, so the pair is skipped and the default
    // (clause_count = all required) stands. Second pair's threshold is 10,
    // also not exceeded, also skipped. Result stays at the default: 3.
    assert_mm("3<-1 10<-2", 3, 3);
}

#[test]
fn conditional_list_past_the_first_threshold() {
    // clause_count=10: first pair (threshold 3) IS exceeded -> apply -1:
    // 10 - 1 = 9. Second pair's threshold is 10, not exceeded (10 is not >
    // 10) -> skipped, keep 9.
    assert_mm("3<-1 10<-2", 10, 9);
}

#[test]
fn conditional_list_past_every_threshold_uses_the_last_pairs_value() {
    // clause_count=15: both thresholds (3, 10) exceeded. First pair applies
    // -1 (result=14), second pair then applies -2 against the actual clause
    // count (15 - 2 = 13), overriding the first.
    assert_mm("3<-1 10<-2", 15, 13);
}

#[test]
fn conditional_list_from_the_issues_own_example_at_the_lower_boundary() {
    // "2<-1 5<80%" at clause_count=2: first pair's threshold is 2, 2 is not
    // > 2, so skipped -- default (all required) stands: 2.
    assert_mm("2<-1 5<80%", 2, 2);
}

#[test]
fn conditional_list_from_the_issues_own_example_between_thresholds() {
    // clause_count=3: first pair's threshold (2) IS exceeded -> apply -1:
    // 3 - 1 = 2. Second pair's threshold (5) not exceeded -> skipped, keep 2.
    assert_mm("2<-1 5<80%", 3, 2);
}

#[test]
fn conditional_list_from_the_issues_own_example_past_every_threshold() {
    // clause_count=6: both thresholds exceeded. First pair applies -1
    // (result=5), second pair applies 80% against the actual clause count:
    // floor(0.8 * 6) = floor(4.8) = 4, overriding the first.
    assert_mm("2<-1 5<80%", 6, 4);
}

// --- degenerate single-clause case -------------------------------------------

#[test]
fn single_clause_positive_absolute_one_requires_the_one_clause() {
    assert_mm("1", 1, 1);
}
