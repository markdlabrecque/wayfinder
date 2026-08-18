//! Detail and safety contract for the follow-up chapters in issue #423.
//!
//! This owns only the new task chapters and their source-derived boundaries;
//! route inventories, parameter rows, and Markdown links are covered elsewhere.

use std::path::Path;

fn chapter(root: &Path, path: &str) -> String {
    std::fs::read_to_string(root.join(path)).unwrap_or_else(|error| panic!("read {path}: {error}"))
}

fn normalized(value: &str) -> String {
    value
        .replace(['`', '*', '_'], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn require_markers(root: &Path, path: &str, markers: &[&str]) {
    let text = normalized(&chapter(root, path));
    for marker in markers {
        assert!(
            text.contains(&normalized(marker)),
            "{path} must retain source-relevant detail or boundary: {marker:?}"
        );
    }
}

#[test]
fn issue_423_detail_chapters_exist_with_safety_boundaries() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let chapters: &[(&str, &[&str])] = &[
        (
            "manual/getting-started/concepts.md",
            &[
                "request architecture",
                "one configured core per process",
                "bounded compatibility contract",
                "generic xml unsupported",
            ],
        ),
        (
            "manual/schema-and-indexing/field-and-analyzer-reference.md",
            &[
                "json null is rejected",
                "static field always wins",
                "longest matching pattern wins",
                "copy fields apply at index time",
                "synonyms.txt",
                "no solr xml analyzer imports",
            ],
        ),
        (
            "manual/schema-and-indexing/updates-and-commits.md",
            &[
                "partial valid prefix",
                "pending",
                "searchable",
                "durable",
                "atomic field modifiers",
                "optimistic concurrency",
                "stale-write conflicts",
            ],
        ),
        (
            "manual/schema-and-indexing/file-extraction-reference.md",
            &[
                "generic xml dispatch is unsupported",
                "does not fetch linked resources",
                "unknown declaration falls through to detection",
                "empty output can be a successful 200 result",
                "encrypted pdf",
                "deadline",
            ],
        ),
        (
            "manual/search/query-cookbook.md",
            &[
                "payload-bearing term",
                "includespanscore=false",
                "pf2",
                "q.op",
                "wt=json",
            ],
        ),
        (
            "manual/search/search-components.md",
            &[
                "facet.method=enum",
                "facet_fields",
                "json facets",
                "date_range",
                "heatmap",
                "choose",
                "spellcheck",
                "suggest",
            ],
        ),
        (
            "manual/search/response-contract.md",
            &[
                "numfoundexact",
                "omitheader",
                "strict_params",
                "responseheader.warnings",
                "inert",
                "clamp",
            ],
        ),
        (
            "manual/integrations/drupal-reference.md",
            &[
                "search api",
                "prefix",
                "admin handshake",
                "no connector/adapter is shipped",
                "stock search_api_solr autocomplete is unsupported",
            ],
        ),
        (
            "manual/operations/server-and-deployment.md",
            &[
                "sigterm",
                "systemd",
                "ghcr",
                "digest",
                "scratch",
                "reverse proxy isolation",
            ],
        ),
        (
            "manual/operations/security-and-observability.md",
            &[
                "basic",
                "public ping",
                "full uri",
                "no prometheus",
                "no opentelemetry",
                "mbeans reset",
            ],
        ),
        (
            "manual/operations/backup-and-migrations.md",
            &[
                "snapshot omission",
                "checksum",
                "retention",
                "encryption",
                "off-host",
                "drill",
            ],
        ),
    ];

    for (path, markers) in chapters {
        assert!(
            root.join(path).is_file(),
            "required detailed chapter is missing: {path}"
        );
        require_markers(root, path, markers);
    }
}

#[test]
fn detailed_state_changes_are_operable_and_reversible() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for path in [
        "manual/schema-and-indexing/field-and-analyzer-reference.md",
        "manual/schema-and-indexing/updates-and-commits.md",
        "manual/schema-and-indexing/file-extraction-reference.md",
        "manual/integrations/drupal-reference.md",
        "manual/operations/server-and-deployment.md",
        "manual/operations/security-and-observability.md",
        "manual/operations/backup-and-migrations.md",
    ] {
        require_markers(
            root,
            path,
            &[
                "prerequisites",
                "visibility",
                "durability",
                "retry",
                "failure",
                "validation",
                "rollback",
            ],
        );
    }
}
