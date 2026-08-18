//! Filesystem contract for the retained tracer-bullet user manual (#423).

use std::path::Path;

fn assert_file(path: &Path) {
    assert!(
        path.is_file(),
        "required manual file is missing: {}",
        path.display()
    );
}

#[test]
fn manual_has_a_quickstart_lifecycle_and_executable_examples() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    let docs = root.join("docs");
    let mut docs_entries: Vec<_> = std::fs::read_dir(&docs)
        .expect("read docs directory")
        .map(|entry| entry.expect("read docs entry").file_name())
        .collect();
    docs_entries.sort();
    assert_eq!(
        docs_entries,
        [
            "COMPATIBILITY.md",
            "CONFIGURATION.md",
            "DEPLOYMENT.md",
            "DEVELOPMENT.md",
        ],
        "docs/ must remain exactly the four canonical documents"
    );

    for path in [
        "manual/README.md",
        "manual/getting-started/quickstart.md",
        "manual/getting-started/schema.toml",
        "manual/getting-started/corpus.json",
        "tests/manual_examples.rs",
    ] {
        assert_file(&root.join(path));
    }

    let quickstart = std::fs::read_to_string(root.join("manual/getting-started/quickstart.md"))
        .expect("read quickstart");
    let quickstart = quickstart.to_ascii_lowercase();
    for marker in [
        "start",
        "/update",
        "commit",
        "/select",
        "/ui",
        "stop",
        "restart",
        "mktemp -d",
        "unset wayfinder_config before running this default quickstart",
        "assert_listener_free",
        "wait_for_wayfinder",
        "emitted by this child only after its bind succeeds",
        "grep -fq \"wayfinder listening\"",
        "did not become ready within 30 seconds",
    ] {
        assert!(
            quickstart.contains(marker),
            "quickstart must cover the `{marker}` lifecycle step"
        );
    }

    let readme = std::fs::read_to_string(root.join("README.md")).expect("read README");
    assert!(
        readme.contains("manual/README.md"),
        "the repository documentation index must link the user manual"
    );
}

#[test]
fn custom_analyzer_example_is_valid_toml_with_query_settings_on_the_field_type() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let reference = std::fs::read_to_string(root.join("manual/reference/schema.md"))
        .expect("read schema reference");
    let start = reference
        .find("```toml\n[[field_types]]")
        .expect("custom analyzer TOML block")
        + "```toml\n".len();
    let end = reference[start..]
        .find("\n```")
        .map(|offset| start + offset)
        .expect("end custom analyzer TOML block");
    let value: toml::Value = toml::from_str(&reference[start..end])
        .expect("custom analyzer example must be syntactically valid TOML");
    let field_type = value["field_types"]
        .as_array()
        .and_then(|entries| entries.first())
        .and_then(toml::Value::as_table)
        .expect("custom analyzer example must define one field type");
    assert_eq!(
        field_type
            .get("query_tokenizer")
            .and_then(toml::Value::as_str),
        Some("simple"),
        "query_tokenizer must belong to the field type, not a filter table"
    );
    assert!(
        field_type
            .get("query_filters")
            .and_then(toml::Value::as_array)
            .is_some(),
        "query_filters must belong to the field type"
    );
}
