//! Issue #147: the two edismax facts that must trace to a capture, not to
//! documentation or to a speculatively authored expectation.
//!
//! CLAUDE.md's compatibility contract: "Fixtures in `solr-ref/responses/` are
//! ground truth. Expected values in tests come from them, never from what the
//! implementation happens to produce." Two things in the edismax
//! implementation violate that today, both knowingly, both with this issue
//! attached:
//!
//! 1. **Phrase vs OR for an unquoted multi-token clause.**
//!    `build_field_disjunction` (`src/core_index.rs`, #137) builds a boolean OR.
//!    The justification is Solr's *documented* `autoGeneratePhraseQueries`
//!    default for schema `version >= 1.4` (off), read off
//!    `solr-ref/search-api/configset/schema.xml:52`'s `version="1.6"` plus the
//!    absence of the attribute anywhere in the configset. (`schema.xml:63` is
//!    inside an XML comment and establishes nothing on its own -- it is not
//!    cited here as if it were a setting.) No committed fixture distinguishes
//!    the two readings; the only assertion of the OR behaviour is the
//!    `select.q.local-params-edismax.and` coverage probe, whose expected
//!    `Some(2)` was authored **speculatively** in `bb44cc4` (#105) for an entry
//!    that could not pass then, and never validated against real Solr.
//! 2. **The Shape-B binding rule.** `local_params::bound_token_len` binds an
//!    inline `{!edismax ...}` to the next run, terminating on whitespace at
//!    quote depth 0 or on a `)` at run-local paren depth 0. That is consistent
//!    with all seven captured Shape-B traces' `numFound` (00003=0, 00004=0,
//!    00005=2, 00006=2, 00007=2, 00008=0, 00021=0) and is what findings 90/91
//!    record -- but consistency with seven outcomes is not Solr telling us its
//!    parse tree, and no capture ever asked it with `debugQuery=true`.
//!
//! This file is the provenance side of closing that gap: it fails while either
//! capture is missing, and once a capture exists it holds the *citations*
//! honest -- the comment in `src/core_index.rs`, the coverage probe's
//! expectation, `src/local_params.rs`'s binding rule, and findings 90/91/92 must
//! all point at the fixture rather than at an inference.
//!
//! It also expires the two existing "still unsettled" guards in
//! `tests/local_params.rs`. Those exist to fail the day the captures land (per
//! CLAUDE.md's "deliberate skips must expire"), so once a fixture is here they
//! are stale by construction and must be deleted rather than edited into
//! permanent greenness. The assertions below are what make that deletion
//! non-optional instead of a nice intention.
//!
//! The behavioural assertions derived from the same two fixtures live in
//! `tests/edismax.rs`
//! (`unquoted_multitoken_clause_matches_committed_capture`,
//! `shape_b_debug_parsedquery_shows_the_plus_binding_only_the_next_run`).

use std::path::{Path, PathBuf};

/// Answers phrase-vs-OR. See `tests/edismax.rs` for the request it is captured
/// from and why that request separates the two readings.
const UNQUOTED_MULTITOKEN_FIXTURE: &str = "edismax_unquoted_multitoken";

/// The two `debugQuery=true` Shape-B captures, one per terminator in the rule
/// `local_params::bound_token_len` implements: trace 00003's shape ends the
/// bound run on **whitespace**, trace 00006's on a **`)` at run-local paren
/// depth 0**. Both are needed or half the rule stays inferred from `numFound`
/// consistency, which is the gap issue #147 exists to close. See
/// `tests/edismax.rs` for each one's request and what a wrong terminator would
/// have produced instead.
const SHAPE_B_DEBUG_FIXTURES: [&str; 2] = [
    "edismax_shape_b_debug_parsedquery",
    "edismax_shape_b_debug_parsedquery_paren_terminated",
];

const CORE_INDEX: &str = include_str!("../src/core_index.rs");
const COVERAGE: &str = include_str!("../src/coverage.rs");
const LOCAL_PARAMS_SRC: &str = include_str!("../src/local_params.rs");
const LOCAL_PARAMS_TESTS: &str = include_str!("local_params.rs");
const CAPTURE_SH: &str = include_str!("../solr-ref/capture.sh");
const MANIFEST: &str = include_str!("../solr-ref/manifest.tsv");
const FINDINGS: &str = include_str!("../docs/solr-ref-findings.md");

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn fixture_exists(name: &str) -> bool {
    root()
        .join("solr-ref/responses")
        .join(format!("{name}.json"))
        .is_file()
}

/// One numbered finding from `docs/solr-ref-findings.md`: the line starting
/// `<n>. ` plus every following line up to the next numbered finding or the
/// next `##` section heading.
fn finding(number: u32) -> String {
    let start_marker = format!("{number}. ");
    let mut lines = FINDINGS
        .lines()
        .skip_while(|l| !l.starts_with(&start_marker));
    let first = lines
        .next()
        .unwrap_or_else(|| panic!("docs/solr-ref-findings.md has no finding {number}"));
    let rest = lines.take_while(|line| {
        let numbered = line
            .split_once(". ")
            .is_some_and(|(head, _)| !head.is_empty() && head.chars().all(|c| c.is_ascii_digit()));
        !numbered && !line.starts_with("##")
    });
    std::iter::once(first)
        .chain(rest)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The 30 source lines immediately preceding a coverage probe's match arm --
/// where `src/coverage.rs` puts every probe's provenance comment.
fn coverage_arm_preamble(item_id: &str) -> String {
    let arm = format!("\"{item_id}\" =>");
    let head = COVERAGE
        .split_once(&arm)
        .unwrap_or_else(|| panic!("src/coverage.rs has no `{arm}` probe arm"))
        .0;
    let lines: Vec<&str> = head.lines().collect();
    lines[lines.len().saturating_sub(30)..].join("\n")
}

/// Blindness control. Every citation assertion in this file sits *after* a
/// fixture-existence assertion, so none of them runs until issue #147's
/// captures land -- which means a broken scanner would go unnoticed until then
/// and then fire with a nonsense message, or (worse, if it silently matched
/// nothing) never fire at all. This test exercises `finding` and
/// `coverage_arm_preamble` against text that is present today.
#[test]
fn the_provenance_scanners_can_see_what_they_scan() {
    let f90 = finding(90);
    assert!(
        f90.starts_with("90. ") && f90.contains("inline nested query") && f90.lines().count() > 5,
        "finding 90 should be the multi-line Shape-B binding finding, got:\n{f90}"
    );
    let f91 = finding(91);
    assert!(
        f91.starts_with("91. ") && f91.contains("bound_token_len"),
        "finding 91 should be the bound-run terminators finding, got:\n{f91}"
    );
    let f92 = finding(92);
    assert!(
        f92.starts_with("92. ") && f92.contains("autoGeneratePhraseQueries"),
        "finding 92 should be the autoGeneratePhraseQueries finding, got:\n{f92}"
    );
    // The three must be distinct blocks, not one run-on slice: `finding` stops
    // at the next numbered entry, and a regex-free scanner that failed to would
    // make every "does finding N cite X" assertion below trivially satisfiable
    // by a citation in a neighbouring finding.
    assert!(
        !f90.contains("91. ") && !f91.contains("92. ") && !f92.contains("93. "),
        "`finding` must stop at the next numbered entry"
    );
    // Two assertions used to live here and just below: that finding 92 still
    // said "not captured", and that `src/core_index.rs` still admitted
    // "documentation-derived, not fixture-derived". They were *pre-capture
    // staleness checks* -- each the exact negation of an expiry assertion in the
    // two tests below -- so they could only hold while issue #147's captures
    // were missing. The captures landed, and both were deleted rather than
    // softened; keeping either would have made this file unsatisfiable, and
    // editing the expiry assertions instead would have been the papering-over
    // they exist to prevent.
    //
    // What survives is this test's actual job: proving the scanners are not
    // blind. Every expiry assertion below is a *positive* "must contain the
    // fixture name" check, so a scanner that silently returned an empty string
    // would fail them rather than pass vacuously -- but only the assertions here
    // pin that `finding` slices the right block and stops at the next numbered
    // entry, and that `coverage_arm_preamble`'s window is wide enough to reach a
    // provenance comment. Those cannot be inferred from the citations alone.

    let preamble = coverage_arm_preamble("select.q.local-params-edismax.and");
    assert!(
        preamble.contains("select.q.plain-query"),
        "the 30 lines before the `select.q.local-params-edismax.and` arm should reach its \
         neighbouring probe, so a provenance comment added above the arm lands inside the window:\n\
         {preamble}"
    );

    // `LOCAL_PARAMS_TESTS` is the one scanned constant whose only expiry
    // assertions are *negative* ("the guard must be gone"), which an empty
    // `include_str!` would satisfy vacuously. Positive control, so the include
    // is proven to be reading the real file: the two `numFound == 0` Shape-B
    // tests findings 90/91 name as their guard are in there.
    for present in [
        "local_params_edismax_two_mandatory_terms_returns_zero",
        "local_params_edismax_mandatory_terms_quick_fox_returns_zero",
    ] {
        assert!(
            LOCAL_PARAMS_TESTS.contains(present),
            "tests/local_params.rs scanned empty or lost `{present}`, one of the two \
             `numFound == 0` Shape-B tests findings 90/91 name as their guard. That would make \
             this file's \"the expiring guard is deleted\" assertions pass without reading \
             anything."
        );
    }
}

/// The phrase-vs-OR capture must exist, and everything that currently rests on
/// the documented `autoGeneratePhraseQueries` default must cite it instead.
///
/// Red today on the first assertion: the capture does not exist, so the
/// speculative `Some(2)` in `select.q.local-params-edismax.and` is still the
/// only thing asserting the OR behaviour.
#[test]
fn unquoted_multitoken_capture_backs_the_speculative_coverage_expectation() {
    assert!(
        fixture_exists(UNQUOTED_MULTITOKEN_FIXTURE),
        "solr-ref/responses/{UNQUOTED_MULTITOKEN_FIXTURE}.json does not exist, so nothing captured \
         says whether an unquoted multi-token edismax clause is a phrase or an OR. \
         `select.q.local-params-edismax.and`'s expected `Some(2)` is still the speculative value \
         authored in `bb44cc4` (#105), which is the inverse of CLAUDE.md's \"fixtures are ground \
         truth\" contract. Capture it (command in \
         `tests/edismax.rs::unquoted_multitoken_clause_matches_committed_capture`) and derive the \
         expectation from it."
    );

    assert!(
        CAPTURE_SH.contains(UNQUOTED_MULTITOKEN_FIXTURE),
        "solr-ref/capture.sh does not mention `{UNQUOTED_MULTITOKEN_FIXTURE}`, so the fixture is \
         not reproducible. Issue #147 owns capture.sh: append the block at the END of the file per \
         CLAUDE.md."
    );

    let preamble = coverage_arm_preamble("select.q.local-params-edismax.and");
    assert!(
        preamble.contains(UNQUOTED_MULTITOKEN_FIXTURE),
        "the `select.q.local-params-edismax.and` probe in src/coverage.rs must cite \
         `{UNQUOTED_MULTITOKEN_FIXTURE}` as the provenance of its expected `numFound`. Every other \
         probe in that file carries its provenance in the comment above the arm; this one carried \
         a speculative placeholder instead. Preceding lines were:\n{preamble}"
    );

    assert!(
        CORE_INDEX.contains(UNQUOTED_MULTITOKEN_FIXTURE),
        "src/core_index.rs must cite `{UNQUOTED_MULTITOKEN_FIXTURE}` where \
         `build_field_disjunction` explains the quoted/unquoted split -- the capture is now the \
         authority, not Solr's documented `autoGeneratePhraseQueries` default"
    );
    assert!(
        !CORE_INDEX.contains("documentation-derived, not fixture-derived"),
        "`build_field_disjunction`'s comment still says its quoted/unquoted split is \
         \"documentation-derived, not fixture-derived\". A fixture now exists \
         (`{UNQUOTED_MULTITOKEN_FIXTURE}`), so that statement is false -- rewrite the comment to \
         cite the capture (issue #147's acceptance criterion)."
    );

    let f92 = finding(92);
    assert!(
        f92.contains(UNQUOTED_MULTITOKEN_FIXTURE),
        "finding 92 must cite `{UNQUOTED_MULTITOKEN_FIXTURE}`, the capture that settles \
         phrase-vs-OR, instead of resting on the documented default:\n{f92}"
    );
    assert!(
        !f92.contains("not captured"),
        "finding 92 still flags itself \"not captured\" while the capture that settles it exists:\n\
         {f92}"
    );

    assert!(
        !LOCAL_PARAMS_TESTS.contains("phrase_vs_or_is_still_unsettled_by_capture"),
        "`tests/local_params.rs::phrase_vs_or_is_still_unsettled_by_capture` is the expiring guard \
         for exactly this gap: it exists to fail the day the capture lands. The capture has landed, \
         so delete the guard -- do not edit it back to green, which would leave a permanently \
         green claim that the question is still open."
    );
}

/// Both `debugQuery=true` Shape-B captures must exist — one per terminator —
/// and the binding rule must cite Solr's own parse tree rather than seven
/// `numFound` values.
///
/// Red today on the first assertion. Deliberately requires *both*: a single
/// capture of trace 00003's shape leaves finding 91's `)` terminator resting on
/// exactly the inference issue #147 exists to replace, so "one debug capture
/// landed" must not be enough to make this green.
#[test]
fn shape_b_debugquery_captures_back_the_binding_rule_in_findings_90_and_91() {
    for fixture in SHAPE_B_DEBUG_FIXTURES {
        assert!(
            fixture_exists(fixture),
            "solr-ref/responses/{fixture}.json does not exist, so the Shape-B binding rule in \
             `local_params::bound_token_len` and findings 90/91 still rests on being consistent \
             with seven captured `numFound` values rather than on Solr's own parsed query. Both \
             terminators need their own capture -- whitespace (trace 00003's shape) and a `)` at \
             run-local paren depth 0 (trace 00006's). Commands are in `tests/edismax.rs`'s \
             `shape_b_debug_parsedquery_*` tests."
        );

        assert!(
            CAPTURE_SH.contains(fixture),
            "solr-ref/capture.sh does not record how `{fixture}` was captured. It is deliberately \
             not a `cape`/manifest.tsv row (see below), so a commented command at the END of \
             capture.sh is what keeps it reproducible."
        );

        assert!(
            LOCAL_PARAMS_SRC.contains(fixture),
            "src/local_params.rs must cite `{fixture}` where it documents the bound-run rule -- \
             Solr's captured `parsedquery` is now the authority for which clause the `+` binds and \
             where the run ends, and each terminator has its own capture to cite"
        );
    }

    assert!(
        !MANIFEST.to_ascii_lowercase().contains("debugquery"),
        "neither `debugQuery` capture may be a solr-ref/manifest.tsv row: \
         `hermetic_edismax_manifest_entries_match_committed_fixtures` and \
         `tests/differential.rs` compare whole response bodies, and Wayfinder implements no \
         `debug` section at all, so such a row could only pass by widening a normaliser over a real \
         capability gap. Same deliberate exclusion as `edismax_qf_partial_invalid` (#111)."
    );

    let shape_b_findings = format!("{}\n{}", finding(90), finding(91));
    for fixture in SHAPE_B_DEBUG_FIXTURES {
        assert!(
            shape_b_findings.contains(fixture),
            "findings 90/91 must cite `{fixture}`: they currently justify the binding rule by \
             fitting seven traces' `numFound`, and the captures replace that inference with Solr's \
             own parse tree -- both terminators, not just the whitespace one:\n{shape_b_findings}"
        );
    }

    assert!(
        !LOCAL_PARAMS_TESTS.contains("shape_b_binding_rule_is_still_unconfirmed_by_debugquery"),
        "`tests/local_params.rs::shape_b_binding_rule_is_still_unconfirmed_by_debugquery` is the \
         expiring guard for this gap and only scans the manifests, so it cannot see a capture that \
         is deliberately not a manifest row -- it would stay green forever. The capture exists: \
         delete the guard."
    );
}
