//! Coverage contract for the task-oriented chapters added by issue #423.
//!
//! This deliberately checks chapter coverage and safety claims, not route,
//! parameter, inventory-row, or general Markdown-link validity. Those are the
//! separate responsibilities of `manual_reference_contract`.

use std::path::Path;

fn text(root: &Path, path: &str) -> String {
    std::fs::read_to_string(root.join(path)).unwrap_or_else(|error| panic!("read {path}: {error}"))
}

fn normalized(text: &str) -> String {
    text.replace(['`', '*'], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn require_all(text: &str, path: &str, markers: &[&str]) {
    let lower = normalized(text);
    for marker in markers {
        assert!(
            lower.contains(&normalized(marker)),
            "{path} must cover {marker:?}"
        );
    }
}

#[test]
fn issue_423_chapters_are_present_and_task_oriented() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let chapters: &[(&str, &[&str])] = &[
        (
            "manual/getting-started/orientation.md",
            &[
                "orientation and request path",
                "not solr",
                "concepts",
                "client -> reverse proxy",
                "supported means",
                "constrained",
                "inert",
                "warning-only",
                "unsupported",
            ],
        ),
        (
            "manual/schema-and-indexing/schema-design.md",
            &[
                "schema design and analyzers",
                "input types",
                "custom index and query analyzer",
                "dynamic fields, copy fields, and synonyms",
            ],
        ),
        (
            "manual/schema-and-indexing/index-lifecycle.md",
            &[
                "replace and delete documents",
                "partial valid prefixes",
                "commit and partial valid prefixes",
                "blue-green reindex",
            ],
        ),
        (
            "manual/schema-and-indexing/extraction.md",
            &[
                "detection, mapping, and limits",
                "generic xml dispatch is unsupported",
                "safe upload lifecycle",
            ],
        ),
        (
            "manual/search/query-and-results.md",
            &[
                "query syntax, filters, paging, sorting, and fields",
                "edismax, local parameters, functions, and payload scores",
                "strict limits",
                "envelope",
            ],
        ),
        (
            "manual/search/aggregations-and-presentation.md",
            &[
                "classic and json facets, and stats",
                "highlighting and grouping",
                "spatial, heatmap, and date-range",
            ],
        ),
        (
            "manual/search/discovery-helpers.md",
            &[
                "mlt, terms, spellcheck, and suggest",
                "choose the right helper",
            ],
        ),
        (
            "manual/integrations/drupal.md",
            &[
                "drupal search api integration",
                "prefix model",
                "admin handshake",
                "stock search_api_solr autocomplete is unsupported",
                "no connector/adapter is shipped",
                "reindex after a migration",
            ],
        ),
        (
            "manual/operations/deploy-and-recover.md",
            &[
                "server tuning and lifecycle",
                "ui, authentication, and observability",
                "same-origin",
                "systemd",
                "ghcr",
                "multi-architecture",
                "scratch",
                "backup, restore, upgrade, rollback, and disaster recovery",
                "omits",
                "one process, one port, one schema, and one data directory",
            ],
        ),
        (
            "manual/reference/cli-and-environment.md",
            &[
                "cli and environment reference",
                "`wayfinder`",
                "`wayfinder snapshot`",
                "`wayfinder_config`",
                "`rust_log`",
            ],
        ),
    ];

    for (path, markers) in chapters {
        let chapter = text(root, path);
        require_all(&chapter, path, markers);
    }
}

#[test]
fn stateful_and_destructive_workflows_explain_safe_lifecycle() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for path in [
        "manual/schema-and-indexing/schema-design.md",
        "manual/schema-and-indexing/index-lifecycle.md",
        "manual/schema-and-indexing/extraction.md",
        "manual/integrations/drupal.md",
        "manual/operations/deploy-and-recover.md",
        "manual/reference/cli-and-environment.md",
    ] {
        require_all(
            &text(root, path),
            path,
            &[
                "prerequisites",
                "visibility",
                "retry",
                "validation",
                "rollback",
            ],
        );
    }
}

#[test]
fn chapters_point_to_normative_and_reference_authorities() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let all = [
        "manual/getting-started/orientation.md",
        "manual/schema-and-indexing/schema-design.md",
        "manual/schema-and-indexing/index-lifecycle.md",
        "manual/schema-and-indexing/extraction.md",
        "manual/search/query-and-results.md",
        "manual/search/aggregations-and-presentation.md",
        "manual/search/discovery-helpers.md",
        "manual/integrations/drupal.md",
        "manual/operations/deploy-and-recover.md",
        "manual/reference/cli-and-environment.md",
    ]
    .into_iter()
    .map(|path| text(root, path))
    .collect::<Vec<_>>()
    .join("\n");

    // These assertions establish the manual's authority boundary; they do not
    // re-check inventory rows or generic link resolution.
    for authority in [
        "../../docs/compatibility.md",
        "../../docs/configuration.md",
        "../../docs/deployment.md",
        "../reference/wire-routes.md",
        "../reference/parameters.md",
        "../reference/schema.md",
        "../reference/analyzers.md",
        "../reference/configuration.md",
        "../reference/extraction.md",
        "../reference/response-errors.md",
        "../reference/drupal.md",
    ] {
        assert!(
            all.to_ascii_lowercase().contains(authority),
            "new chapters must direct readers to {authority}"
        );
    }
}

#[test]
fn compatibility_capabilities_and_permanent_boundaries_have_manual_owners() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let compatibility = text(root, "docs/COMPATIBILITY.md");
    let owners: &[(&str, &str)] = &[
        ("Query syntax", "manual/search/query-and-results.md"),
        ("Retrieval", "manual/search/query-and-results.md"),
        (
            "Classic facets",
            "manual/search/aggregations-and-presentation.md",
        ),
        (
            "JSON facets",
            "manual/search/aggregations-and-presentation.md",
        ),
        ("Relevance", "manual/search/query-and-results.md"),
        (
            "Presentation",
            "manual/search/aggregations-and-presentation.md",
        ),
        ("Updates", "manual/schema-and-indexing/index-lifecycle.md"),
        ("Helpers", "manual/search/discovery-helpers.md"),
        ("Extraction", "manual/schema-and-indexing/extraction.md"),
        ("q.op", "manual/search/query-and-results.md"),
        (
            "search_api_solr_admin core reload, field-analysis, and configset-file routes",
            "manual/integrations/drupal.md",
        ),
        (
            "Open-ended solr_text_custom analyzer families and Solr XML analyzer imports",
            "manual/schema-and-indexing/schema-design.md",
        ),
        (
            "Atomic field modifiers, optimistic concurrency, versions=true, and stale-write conflicts",
            "manual/schema-and-indexing/index-lifecycle.md",
        ),
        (
            "Classic facet.method=enum",
            "manual/search/aggregations-and-presentation.md",
        ),
        (
            "SolrCloud, ZooKeeper, distributed/sharded search, streaming expressions, and SQL",
            "manual/getting-started/orientation.md",
        ),
        (
            "XML, javabin, PHP, and other non-JSON response writers",
            "manual/search/query-and-results.md",
        ),
        (
            "OCR and external extraction services",
            "manual/schema-and-indexing/extraction.md",
        ),
        (
            "Wayfinder serves one configured core per process.",
            "manual/operations/deploy-and-recover.md",
        ),
    ];

    for (capability_or_boundary, owner) in owners {
        assert!(
            normalized(&compatibility).contains(&normalized(capability_or_boundary)),
            "owner map must track a current Compatibility capability/boundary: {capability_or_boundary}"
        );
        assert!(
            root.join(owner).is_file(),
            "{capability_or_boundary} must have manual owner {owner}"
        );
    }
    assert_eq!(
        owners.len(),
        18,
        "map every current capability and permanent boundary"
    );
}
