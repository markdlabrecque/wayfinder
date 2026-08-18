//! Executable guards for the consolidated documentation contract.

const CLAUDE: &str = include_str!("../CLAUDE.md");
const COMPATIBILITY: &str = include_str!("../docs/COMPATIBILITY.md");
const CONFIGURATION: &str = include_str!("../docs/CONFIGURATION.md");
const FINDINGS: &str = include_str!("../solr-ref/FINDINGS.md");

fn normalized(text: &str) -> String {
    text.replace("**", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn contains_all(text: &str, terms: &[&str], contract: &str) {
    let text = normalized(text);
    let missing: Vec<_> = terms
        .iter()
        .filter(|term| !text.contains(&normalized(term)))
        .collect();
    assert!(missing.is_empty(), "{contract}; missing terms: {missing:?}");
}

#[test]
fn docs_directory_has_only_the_four_primary_buckets() {
    let docs = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs");
    let mut names: Vec<_> = std::fs::read_dir(docs)
        .expect("read docs directory")
        .map(|entry| {
            entry
                .expect("read docs entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "COMPATIBILITY.md",
            "CONFIGURATION.md",
            "DEPLOYMENT.md",
            "DEVELOPMENT.md",
        ]
    );
}

#[test]
fn compatibility_freezes_the_existing_wire_without_planning_parity() {
    contains_all(
        COMPATIBILITY,
        &[
            "bounded compatibility contract",
            "not an ongoing Solr parity project",
            "solr-ref/responses/",
            "frozen regression baseline",
            "expected values come from fixtures",
            "never from implementation output",
            "no new Solr-parity captures are planned",
        ],
        "COMPATIBILITY.md must define the retained wire without creating a parity roadmap",
    );
    contains_all(
        CLAUDE,
        &[
            "frozen regression baseline",
            "expected values come from fixtures",
            "never from implementation output",
            "No new Solr-parity work or captures are planned",
        ],
        "CLAUDE.md must preserve the fixture-authority workflow",
    );
}

#[test]
fn compatibility_keeps_permanent_product_boundaries() {
    contains_all(
        COMPATIBILITY,
        &[
            "Permanent unsupported boundaries",
            "q.op",
            "qt",
            "search_api_solr_admin",
            "pf2",
            "pf3",
            "ps",
            "lowercaseOperators",
            "Atomic field modifiers",
            "optimistic concurrency",
        ],
        "COMPATIBILITY.md must retain the established unsupported surface",
    );
}

#[test]
fn configuration_classifies_every_server_knob() {
    contains_all(
        CONFIGURATION,
        &[
            "Live knobs",
            "query.time_allowed",
            "resources.searcher_pool_size",
            "intentionally inert",
            "extraction.max_body_bytes",
            "max_inflight_uploads",
            "commit.autocommit_max_docs",
            "admin.reported_server_version",
        ],
        "CONFIGURATION.md must distinguish working settings from accepted no-ops",
    );
}

#[test]
fn historical_findings_remain_immutable_evidence() {
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
    assert_eq!(actual, expected, "all numbered findings must remain");
    assert_eq!(
        numbers.len(),
        actual.len(),
        "findings must not be duplicated"
    );
}
