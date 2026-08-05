//! Executable documentation contract for issue #392.
//!
//! Assertions are limited to the governing policy sections so ordinary
//! historical prose cannot accidentally satisfy or violate them.

const CLAUDE: &str = include_str!("../CLAUDE.md");
const PRD: &str = include_str!("../docs/PRD.md");
const FINDINGS: &str = include_str!("../docs/solr-ref-findings.md");
const SPECS_README: &str = include_str!("../docs/specs/README.md");
const BACKEND_PLAN: &str = include_str!("../docs/plans/57-search-api-wayfinder-backend.md");
const HISTORICAL_SPECS: &[(&str, &str)] = &[
    (
        "PREP-1-vendor-source.md",
        include_str!("../docs/specs/PREP-1-vendor-source.md"),
    ),
    (
        "350-form-encoded-post.md",
        include_str!("../docs/specs/350-form-encoded-post.md"),
    ),
    (
        "351-autocomplete-endpoint.md",
        include_str!("../docs/specs/351-autocomplete-endpoint.md"),
    ),
    (
        "352-suggest-buildall.md",
        include_str!("../docs/specs/352-suggest-buildall.md"),
    ),
    (
        "353-highlight-params.md",
        include_str!("../docs/specs/353-highlight-params.md"),
    ),
    (
        "354-admin-endpoints.md",
        include_str!("../docs/specs/354-admin-endpoints.md"),
    ),
    (
        "355-finding-132-amendment.md",
        include_str!("../docs/specs/355-finding-132-amendment.md"),
    ),
    (
        "357-online-snapshot-flake.md",
        include_str!("../docs/specs/357-online-snapshot-flake.md"),
    ),
    (
        "358-string-sort-copy.md",
        include_str!("../docs/specs/358-string-sort-copy.md"),
    ),
    (
        "359-spellcheck-multi-dictionary.md",
        include_str!("../docs/specs/359-spellcheck-multi-dictionary.md"),
    ),
    (
        "360-extended-results-shape.md",
        include_str!("../docs/specs/360-extended-results-shape.md"),
    ),
    (
        "361-querybuilder-fl.md",
        include_str!("../docs/specs/361-querybuilder-fl.md"),
    ),
    (
        "362-sort-copy-fanout.md",
        include_str!("../docs/specs/362-sort-copy-fanout.md"),
    ),
];

fn between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let start_index = text
        .find(start)
        .unwrap_or_else(|| panic!("missing section start `{start}`"));
    let section = &text[start_index..];
    let end_index = section
        .find(end)
        .unwrap_or_else(|| panic!("section `{start}` must end before `{end}`"));
    &section[..end_index]
}

fn normalized(text: &str) -> String {
    text.replace("**", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn contains_all(section: &str, terms: &[&str], contract: &str) {
    let section = normalized(section);
    let missing: Vec<_> = terms
        .iter()
        .filter(|term| !section.contains(&normalized(term)))
        .collect();
    assert!(missing.is_empty(), "{contract}; missing terms: {missing:?}");
}

fn paragraph_with_all<'a>(section: &'a str, terms: &[&str], contract: &str) -> &'a str {
    section
        .split("\n\n")
        .find(|paragraph| {
            let paragraph = normalized(paragraph);
            terms
                .iter()
                .all(|term| paragraph.contains(&normalized(term)))
        })
        .unwrap_or_else(|| panic!("{contract}; no paragraph contains {terms:?}"))
}

fn assert_flat_permanent_boundary(boundary: &str, contract: &str) {
    contains_all(boundary, &["permanent", "unsupported"], contract);
    let boundary = normalized(boundary);
    for forbidden in ["revisit", "guard", "roadmap"] {
        assert!(
            !boundary.contains(forbidden),
            "{contract}; permanent boundaries must not retain `{forbidden}` choreography"
        );
    }
}

#[test]
fn compatibility_contract_freezes_the_existing_fixture_baseline() {
    let compatibility = between(CLAUDE, "## Compatibility contract", "## Testing");

    contains_all(
        compatibility,
        &[
            "solr-ref/responses",
            "frozen regression baseline",
            "expected values come from fixtures",
            "never from implementation output",
            "no new solr-parity work or captures are planned",
        ],
        "CLAUDE.md's compatibility contract must freeze fixtures without planning new parity work",
    );
    assert!(
        !normalized(compatibility).contains("differential harness"),
        "CLAUDE.md's compatibility contract must not retain a differential harness"
    );
}

#[test]
fn prd_problem_statement_makes_the_wire_historical_not_a_parity_goal() {
    let problem = between(
        PRD,
        "## 1. Problem & motivation",
        "## 2. Compatibility contract",
    );

    contains_all(
        problem,
        &[
            "solr-compatible wire",
            "how wayfinder was built",
            "existing clients",
            "not an ongoing parity goal",
        ],
        "PRD §1 must describe the Solr wire as Wayfinder's historical client entry point, not a goal",
    );
}

#[test]
fn prd_compatibility_section_describes_current_behavior_without_future_divergence_process() {
    let compatibility = between(PRD, "## 2. Compatibility contract", "## 3. Configuration");
    let ratified = between(
        compatibility,
        "#### Ratified divergences from captured Solr behaviour",
        "\n1.",
    );

    contains_all(
        compatibility,
        &[
            "current supported behavior",
            "what must match exactly",
            "what deliberately differs",
            "ratified divergences from captured solr behaviour",
        ],
        "PRD §2 must retain its descriptive record of current supported behavior and divergences",
    );
    let ratified = normalized(ratified);
    for forbidden in [
        "nothing may be added",
        "must be added",
        "future divergences",
    ] {
        assert!(
            !ratified.contains(forbidden),
            "PRD §2's ratified-divergence preamble must not prescribe `{forbidden}`"
        );
    }
}

#[test]
fn prd_scope_lists_permanent_unsupported_boundaries_without_parity_choreography() {
    let scope = between(PRD, "## 5. Feature scope", "## 6. Tuning knobs");

    assert_flat_permanent_boundary(
        paragraph_with_all(
            scope,
            &["q.op", "qt"],
            "PRD §5 must retain the q.op/qt boundary",
        ),
        "q.op/qt must be a flat permanent boundary",
    );
    assert_flat_permanent_boundary(
        paragraph_with_all(
            scope,
            &["search_api_solr_admin"],
            "PRD §5 must retain the search_api_solr_admin boundary",
        ),
        "search_api_solr_admin must be a flat permanent boundary",
    );
    assert_flat_permanent_boundary(
        paragraph_with_all(
            scope,
            &["pf2", "pf3", "ps", "stopwords", "lowercaseoperators"],
            "PRD §5 must retain all five unsupported edismax parameters",
        ),
        "the five edismax parameters must be a flat permanent boundary",
    );
    assert_flat_permanent_boundary(
        paragraph_with_all(
            scope,
            &["atomic updates", "optimistic concurrency"],
            "PRD §5 must retain the atomic-update/concurrency boundary",
        ),
        "atomic updates and optimistic concurrency must be a flat permanent boundary",
    );
}

#[test]
fn findings_introduction_preserves_evidence_but_removes_capture_and_harness_sections() {
    let introduction = between(
        FINDINGS,
        "# Solr reference capture — findings",
        "## Findings from the issue #11 error-shape capture",
    );

    contains_all(
        introduction,
        &[
            "factual evidence",
            "numbering gaps are intentional",
            "#392 removed only unnumbered planning and comparison-runner sections",
        ],
        "the findings introduction must explain its retained factual evidence and intentional gaps",
    );
    for obsolete_heading in ["## Not yet captured", "## Differential harness"] {
        assert!(
            !introduction.contains(obsolete_heading),
            "the findings introduction must not retain `{obsolete_heading}`"
        );
    }

    let numbers: Vec<u16> = FINDINGS
        .lines()
        .filter_map(|line| {
            line.split_once('.')
                .and_then(|(number, _)| number.parse().ok())
        })
        .collect();
    let actual: std::collections::BTreeSet<_> = numbers.iter().copied().collect();
    let expected: std::collections::BTreeSet<u16> = (1..=196)
        .filter(|number| ![32, 33, 43, 44, 45, 85, 86, 104].contains(number))
        .collect();
    assert_eq!(
        actual, expected,
        "all numbered factual findings must remain"
    );
    assert_eq!(
        numbers.len(),
        actual.len(),
        "numbered factual findings must not be duplicated"
    );

    let mut in_finding = false;
    let mut hash = 0xcbf29ce484222325_u64;
    for line in FINDINGS.lines() {
        if line.starts_with("## ") {
            in_finding = false;
        }
        if line
            .split_once('.')
            .is_some_and(|(number, _)| number.parse::<u16>().is_ok())
        {
            in_finding = true;
        }
        if in_finding {
            for byte in line.bytes().chain(std::iter::once(b'\n')) {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x100000001b3);
            }
        }
    }
    assert_eq!(
        hash, 0xcc31601a94cd57a2,
        "numbered factual finding bodies must remain historical evidence"
    );
}

#[test]
fn parity_plans_are_historical_records_not_active_instructions() {
    contains_all(
        SPECS_README,
        &["historical", "solr-parity items are no longer planned"],
        "docs/specs/README.md must identify specs as historical rather than planned parity work",
    );
    for (name, contents) in HISTORICAL_SPECS {
        assert!(
            contents.starts_with("> **Historical implementation record.**"),
            "docs/specs/{name} must be marked historical when opened directly"
        );
    }

    assert!(
        BACKEND_PLAN.starts_with("> **Historical implementation record.**"),
        "the completed Search API backend plan must be neutralized when opened directly"
    );

    let obsolete_plan = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("docs/plans/289-302-search-api-parity-batch.md");
    assert!(
        !obsolete_plan.exists(),
        "the prospective Search API parity sequencing plan must remain removed"
    );
}
