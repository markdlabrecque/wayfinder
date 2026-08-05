//! Executable documentation contract for issue #399.
//!
//! Assertions are scoped to the governing sections so unrelated historical
//! prose cannot accidentally satisfy them.

const CLAUDE: &str = include_str!("../CLAUDE.md");
const PRD: &str = include_str!("../docs/PRD.md");
const FINDINGS: &str = include_str!("../docs/solr-ref-findings.md");
const DIFFERENTIAL: &str = include_str!("differential.rs");

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

#[test]
fn fixtures_are_evidence_not_automatic_scope() {
    let claude = between(CLAUDE, "## Compatibility contract", "## Testing");
    let prd = between(PRD, "## 2. Compatibility contract", "## 3. Configuration");
    let findings = between(FINDINGS, "## Evidence boundary", "## Numbering");

    contains_all(
        claude,
        &[
            "immutable factual evidence",
            "expected values",
            "never from what the implementation happens to produce",
            "not automatic product scope",
            "captured but unexercised path is optional",
            "not automatically a bug or an implementation obligation",
        ],
        "CLAUDE.md must distinguish immutable fixture evidence from product scope",
    );
    assert!(
        !claude.contains("Divergence from captured Solr behaviour is a bug"),
        "CLAUDE.md must not restore the absolute all-divergence-is-a-bug rule"
    );

    contains_all(
        prd,
        &[
            "do not automatically define product scope",
            "supports and a real client exercises is held exactly",
            "captured path no client reaches is optional",
        ],
        "PRD §2 must weight exact fidelity by supported client-exercised paths",
    );
    contains_all(
        findings,
        &[
            "factual solr capture and client-source evidence",
            "do not automatically create product scope",
            "captured but unexercised behaviour is evidence, not an implementation obligation",
        ],
        "the findings evidence boundary must remain factual rather than normative",
    );
}

#[test]
fn supported_departures_and_unsupported_scope_have_different_ledgers() {
    let claude = between(CLAUDE, "## Compatibility contract", "## Testing");
    let prd_two = between(PRD, "## 2. Compatibility contract", "## 3. Configuration");
    let prd_five = between(PRD, "## 5. Feature scope", "## 6. Tuning knobs");
    let findings = between(FINDINGS, "## Evidence boundary", "## Numbering");
    let ratified = between(
        prd_two,
        "#### Ratified divergences from captured Solr behaviour",
        "### How this is verified",
    );

    contains_all(
        claude,
        &[
            "departure on any supported path",
            "prd §2's ratified-divergence list",
            "unsupported or out-of-scope behaviour in prd §5",
            "never in the ratified-divergence list",
        ],
        "CLAUDE.md must name one unambiguous decision-record rule",
    );
    contains_all(
        prd_two,
        &[
            "mismatches on supported paths, whether client-exercised or not",
            "unsupported or out-of-scope path belongs in §5",
            "fixture or client evidence",
            "reason",
            "issue or report",
        ],
        "PRD §2 must reserve ratification for departures on any supported path",
    );
    assert!(
        !ratified.contains("{!lucene}"),
        "unsupported parser types must not remain in PRD §2's ratified list"
    );
    contains_all(
        prd_five,
        &[
            "unsupported local-params parser types",
            "{!lucene}",
            "syntaxerror",
            "issue #137",
            "not §2's ratified divergence list",
        ],
        "PRD §5 must retain the shipped unsupported-parser boundary and its decision record",
    );
    contains_all(
        prd_five,
        &[
            "unsupported classic-facet methods",
            "facet.method=enum",
            "out of scope",
            "not a prd §2-ratified supported-path divergence",
            "finding 106",
            "#399",
        ],
        "PRD §5 must classify the captured enum method as unsupported inventory",
    );
    contains_all(
        findings,
        &[
            "departure on any supported path belongs in prd §2",
            "unsupported or out-of-scope behaviour belongs in prd §5",
        ],
        "the findings evidence boundary must point decisions to the correct ledgers",
    );

    let accepted = between(
        DIFFERENTIAL,
        "const ACCEPTED_DIVERGENCES: ",
        "fn accepted_divergence_reason",
    );
    let expected = between(
        DIFFERENTIAL,
        "const EXPECTED_DIVERGENCES_MANIFEST_ERRORS: ",
        "fn expected_divergence_manifest_errors_reason",
    );
    assert!(
        !accepted.contains("facet_non_docvalues_text_enum"),
        "unsupported facet.method=enum must not be ratified as a supported-path divergence"
    );
    assert!(
        expected.contains("facet_non_docvalues_text_enum"),
        "the captured unsupported facet.method=enum mismatch must remain visible as inventory"
    );
}

#[test]
fn differential_inventory_is_not_scope_authority() {
    let claude = between(CLAUDE, "## Compatibility contract", "## Testing");
    let prd_eight = between(PRD, "## 8. Conformance & benchmarking", "## 9. Risks");
    let findings_harness = between(
        FINDINGS,
        "## Differential harness (issue #1)",
        "## Findings from the issue #2 `sort` capture",
    );

    contains_all(
        claude,
        &[
            "regression evidence and inventory evidence",
            "not scope authority",
            "does not create a product commitment",
            "mechanically self-expiring",
        ],
        "CLAUDE.md must state the harness's temporary non-authoritative meaning",
    );
    contains_all(
        prd_eight,
        &[
            "regression and inventory evidence",
            "not scope authority or a product commitment",
            "cannot itself require implementation",
            "does not decide which paths wayfinder supports",
        ],
        "PRD §8 must not turn a captured difference into implementation scope",
    );
    contains_all(
        findings_harness,
        &[
            "not product scope, an implementation queue, or ratification",
            "separate prd and issue/pr scope decision is required",
            "inventory, not a permanent skip or implementation queue",
            "entry itself never scopes that work",
        ],
        "the findings harness description must classify differences without assigning work",
    );

    for forbidden in [
        "self-expiring to-do list",
        "this list is a to-do",
        "when you fix the owning feature",
        "mandatory reason naming the owning issue",
    ] {
        assert!(
            !normalized(findings_harness).contains(forbidden),
            "findings must not restore EXPECTED_DIVERGENCES as an implementation mandate: `{forbidden}`"
        );
    }
}
