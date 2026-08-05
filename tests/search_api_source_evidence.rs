//! Integrity audit for retained historical Search API Solr client-source evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

const SOURCE_EVIDENCE: &str =
    include_str!("../coverage/search_api_solr_4.4.0_source_evidence.json");

#[derive(Debug, Deserialize)]
struct SourceEvidence {
    upstream: Upstream,
    snapshot_root: String,
    files: Vec<SourceFile>,
    citations: Vec<Citation>,
    exclusions: Vec<Exclusion>,
}

#[derive(Debug, Deserialize)]
struct Upstream {
    project: String,
    tag: String,
    archive_sha256: String,
}

#[derive(Debug, Deserialize)]
struct SourceFile {
    path: String,
    sha256: String,
}

#[derive(Debug, Deserialize)]
struct Citation {
    id: String,
    source_path: String,
    line_start: usize,
    line_end: usize,
    source_sha256: String,
    excerpt_sha256: String,
    excerpt: String,
}

#[derive(Debug, Deserialize)]
struct Exclusion {
    id: String,
    reason: String,
    evidence: String,
    required_expressions: Vec<String>,
    forbidden_expressions: Vec<String>,
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn source_file_paths(root: &Path) -> BTreeSet<String> {
    fn collect(root: &Path, directory: &Path, files: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(directory)
            .unwrap_or_else(|e| panic!("read source snapshot {}: {e}", directory.display()))
        {
            let entry = entry.expect("read source snapshot entry");
            let path = entry.path();
            let file_type = entry
                .file_type()
                .unwrap_or_else(|e| panic!("read source snapshot type {}: {e}", path.display()));
            if file_type.is_dir() {
                collect(root, &path, files);
            } else if file_type.is_file() {
                files.insert(
                    path.strip_prefix(root)
                        .expect("source snapshot entry under snapshot root")
                        .to_str()
                        .expect("source snapshot path is UTF-8")
                        .replace('\\', "/"),
                );
            } else {
                panic!(
                    "source snapshot must contain only regular files and directories: {}",
                    path.display()
                );
            }
        }
    }

    let mut files = BTreeSet::new();
    collect(root, root, &mut files);
    files
}

fn source_excerpt(source: &str, line_start: usize, line_end: usize) -> String {
    assert!(line_start > 0 && line_start <= line_end);
    let excerpt = source
        .split_inclusive('\n')
        .skip(line_start - 1)
        .take(line_end - line_start + 1)
        .collect::<String>();
    assert_eq!(
        excerpt.lines().count(),
        line_end - line_start + 1,
        "source range must be present in the immutable snapshot"
    );
    excerpt
}

#[test]
fn source_evidence_is_hash_pinned_complete_and_auditable() {
    let evidence: SourceEvidence =
        serde_json::from_str(SOURCE_EVIDENCE).expect("valid Search API Solr source evidence");
    assert_eq!(
        evidence.upstream.project,
        "https://git.drupalcode.org/project/search_api_solr"
    );
    assert_eq!(evidence.upstream.tag, "4.4.0");
    assert_eq!(
        evidence.upstream.archive_sha256,
        "5cfcb17d7a325a01eb04f09ca12b6f0d3012ebe0fcfea431ee04a592507c0bce"
    );

    let is_sha256 =
        |digest: &str| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit());
    let sha256 = |text: &str| format!("{:x}", Sha256::digest(text));
    assert!(is_sha256(&evidence.upstream.archive_sha256));

    let snapshot_relative = Path::new(&evidence.snapshot_root);
    assert!(
        snapshot_relative
            .components()
            .all(|component| matches!(component, Component::Normal(_))),
        "source snapshot root must be a relative normal path"
    );
    let snapshot_root = root().join(snapshot_relative);

    let pinned_citation_hashes = BTreeMap::from([
        (
            "src/Plugin/search_api/backend/SearchApiSolrBackend.php",
            "587ccd8f3fadb606b6968bc589fd6312e02c4a95e2ee502b380ca6a7241cd21d",
        ),
        (
            "src/SolrConnector/SolrConnectorPluginBase.php",
            "b55ec67468adda7f70061aa8151861c7f9a7c63e680b6c48c6a7379aa9617df0",
        ),
        (
            "src/SolrSpellcheckBackendTrait.php",
            "0238f9e32ecfbe3da160e1a58ad56adade38f3ed8cd27adfc1268cd6c5e53771",
        ),
    ]);
    let manifest_hashes = evidence
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.sha256.as_str()))
        .collect::<BTreeMap<_, _>>();
    for (path, hash) in &pinned_citation_hashes {
        assert_eq!(
            manifest_hashes.get(path),
            Some(hash),
            "citation-bearing source file must remain pinned to Search API Solr 4.4.0: {path}"
        );
    }

    let expected_files = evidence
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(expected_files.len(), evidence.files.len());
    assert_eq!(source_file_paths(&snapshot_root), expected_files);

    let mut source_text = BTreeMap::new();
    for file in &evidence.files {
        assert!(is_sha256(&file.sha256));
        assert!(
            Path::new(&file.path)
                .components()
                .all(|component| matches!(component, Component::Normal(_))),
            "source file path must be a relative normal path"
        );
        let source_path = snapshot_root.join(&file.path);
        let source = std::fs::read_to_string(&source_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", source_path.display()));
        assert_eq!(sha256(&source), file.sha256, "hash for {}", file.path);
        source_text.insert(file.path.clone(), source);
    }

    let citation_sources = evidence
        .citations
        .iter()
        .map(|citation| citation.source_path.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        citation_sources,
        pinned_citation_hashes.keys().copied().collect(),
        "every citation must remain in a hash-pinned source file"
    );
    assert_eq!(
        evidence
            .citations
            .iter()
            .map(|citation| citation.id.as_str())
            .collect::<BTreeSet<_>>()
            .len(),
        evidence.citations.len(),
        "citation IDs must be unique"
    );
    for citation in &evidence.citations {
        assert!(citation.source_path.starts_with("src/"));
        assert!(citation.line_start <= citation.line_end);
        assert!(is_sha256(&citation.source_sha256));
        assert!(is_sha256(&citation.excerpt_sha256));
        let source = source_text
            .get(&citation.source_path)
            .unwrap_or_else(|| panic!("missing snapshot source {}", citation.source_path));
        assert_eq!(
            manifest_hashes.get(citation.source_path.as_str()),
            Some(&citation.source_sha256.as_str()),
            "citation source hash must match its manifest entry: {}",
            citation.id
        );
        assert_eq!(sha256(source), citation.source_sha256);
        assert_eq!(
            citation.excerpt,
            source_excerpt(source, citation.line_start, citation.line_end),
            "{} must be an exact source range",
            citation.id
        );
        assert_eq!(sha256(&citation.excerpt), citation.excerpt_sha256);
        assert!(!citation.excerpt.is_empty());
    }

    for (citation, needle) in [
        ("backend.extract-results", "getNumFound"),
        ("backend.highlighting", "['highlighting']"),
        ("backend.extract-facets", "getFacetSet"),
        ("spellcheck.suggestions", "COMPONENT_SPELLCHECK"),
        ("backend.search-spellcheck-collation", "getCollation"),
        ("connector.solr-version", "solr-spec-version"),
        ("connector.schema-version", "['core']['schema']"),
        ("backend.schema-field-types", "schema/"),
        ("backend.view-settings-luke", "['index']['numDocs']"),
        ("connector.stats-summary", "['solr-mbeans']"),
        ("backend.autocomplete-terms", "COMPONENT_TERMS"),
    ] {
        assert!(
            evidence
                .citations
                .iter()
                .find(|entry| entry.id == citation)
                .expect("required source excerpt")
                .excerpt
                .contains(needle),
            "{citation} must retain the client-consumption expression"
        );
    }

    let expected_exclusions = BTreeSet::from([
        "update.responseHeader.status",
        "select.response.start",
        "select.response.maxScore",
        "select.response.numFoundExact",
    ]);
    assert_eq!(
        evidence
            .exclusions
            .iter()
            .map(|exclusion| exclusion.id.as_str())
            .collect::<BTreeSet<_>>(),
        expected_exclusions,
        "every emitted-only exclusion needs source-audited evidence"
    );
    for exclusion in &evidence.exclusions {
        assert!(!exclusion.reason.is_empty());
        assert!(!exclusion.required_expressions.is_empty());
        assert!(!exclusion.forbidden_expressions.is_empty());
        let citation = evidence
            .citations
            .iter()
            .find(|citation| citation.id == exclusion.evidence)
            .unwrap_or_else(|| panic!("missing exclusion evidence {}", exclusion.evidence));
        for expression in &exclusion.required_expressions {
            assert!(
                citation.excerpt.contains(expression),
                "{} must retain required expression {expression:?}",
                exclusion.id
            );
        }
        for expression in &exclusion.forbidden_expressions {
            assert!(
                !citation.excerpt.contains(expression),
                "{} must not consume excluded expression {expression:?}",
                exclusion.id
            );
        }
    }
}
