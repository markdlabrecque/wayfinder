//! Drift checks for issue #423's shared manual reference inventories.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn source(root: &Path, relative: &str) -> String {
    std::fs::read_to_string(root.join(relative))
        .unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

fn occurrences(text: &str, needle: &str) -> usize {
    text.match_indices(needle).count()
}

fn quoted_entries(source: &str, constant: &str) -> Vec<String> {
    let start = source
        .find(&format!("const {constant}:"))
        .unwrap_or_else(|| panic!("missing {constant}"));
    let end = source[start..]
        .find("];\n")
        .unwrap_or_else(|| panic!("unterminated {constant}"));
    source[start..start + end]
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
}

fn route_methods(lib: &str) -> Vec<(String, String)> {
    let macro_start = lib
        .find("macro_rules! search_api_routes")
        .expect("search_api_routes declaration");
    let macro_end = lib[macro_start..]
        .find("macro_rules! wire_routes")
        .expect("search_api_routes end")
        + macro_start;
    let mut routes = lib[macro_start..macro_end]
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            line.strip_prefix("(\"").and_then(|line| {
                let (route, _) = line.split_once("\",")?;
                Some((route.to_owned(), "Any".to_owned()))
            })
        })
        .collect::<Vec<_>>();

    for line in lib.lines().map(str::trim) {
        let Some(line) = line.strip_prefix(".route(\"") else {
            continue;
        };
        let Some((route, handler)) = line.split_once("\",") else {
            continue;
        };
        if !route.starts_with("/ui") {
            continue;
        }
        let methods = match (handler.contains("get("), handler.contains(".post(")) {
            (true, true) => "GET; POST",
            (true, false) => "GET",
            (false, true) => "POST",
            (false, false) => panic!("no documented method for UI route {route}"),
        };
        routes.push((route.to_owned(), methods.to_owned()));
    }
    routes
}

#[test]
fn router_and_allowlist_declarations_have_exactly_one_reference_row() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let lib = source(root, "src/lib.rs");
    let routes = source(root, "manual/reference/wire-routes.md");
    let parameters = source(root, "manual/reference/parameters.md");

    for (route, method) in route_methods(&lib) {
        assert_eq!(
            occurrences(&routes, &format!("| `{route}` | {method} |")),
            1,
            "route and method need one inventory row: {route} {method}"
        );
    }

    for name in [
        "SELECT_PARAMS",
        "PER_FIELD_PARAMS",
        "UPDATE_PARAMS",
        "EXTRACT_PARAMS",
        "PING_PARAMS",
        "ADMIN_INFO_PARAMS",
        "SCHEMA_FIELDTYPES_PARAMS",
        "ADMIN_LUKE_PARAMS",
        "MBEANS_PARAMS",
        "MLT_PARAMS",
        "TERMS_PARAMS",
        "SUGGEST_PARAMS",
    ] {
        for parameter in quoted_entries(&lib, name) {
            let prefix = format!("| `{name}` | `{parameter}` | ");
            assert_eq!(
                occurrences(&parameters, &prefix),
                1,
                "{name} entry `{parameter}` needs exactly one inventory row"
            );
            let row = parameters
                .lines()
                .find(|line| line.starts_with(&prefix))
                .expect("row counted above");
            let status = row.split('|').nth(3).map(str::trim).unwrap_or_default();
            assert!(
                matches!(
                    status,
                    "implemented" | "constrained" | "inert" | "warning-only" | "prefix-family"
                ),
                "{name} entry `{parameter}` has invalid status `{status}`"
            );
        }
    }
}

fn public_fields(config: &str, struct_name: &str) -> Vec<String> {
    let start = config
        .find(&format!("pub struct {struct_name} {{"))
        .unwrap_or_else(|| panic!("missing config struct {struct_name}"));
    let body = &config[start..config[start..].find("\n}").expect("struct end") + start];
    body.lines()
        .filter_map(|line| line.trim().strip_prefix("pub "))
        .filter_map(|line| line.split_once(':').map(|(name, _)| name.trim().to_owned()))
        .collect()
}

fn inventory_row<'a>(inventory: &'a str, key: &str) -> &'a str {
    let prefix = format!("| `{key}` |");
    assert_eq!(
        occurrences(inventory, &prefix),
        1,
        "reference item needs exactly one row: {key}"
    );
    inventory
        .lines()
        .find(|line| line.starts_with(&prefix))
        .expect("row counted above")
}

#[test]
fn configuration_and_schema_declarations_have_complete_reference_rows() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let config = source(root, "src/config.rs");
    let inventory = source(root, "manual/reference/configuration.md");
    let server_fields = public_fields(&config, "ServerConfig");
    assert!(server_fields.iter().any(|field| field == "strict_params"));
    let strict_params = inventory_row(&inventory, "strict_params");
    assert_eq!(
        strict_params.split('|').count(),
        7,
        "configuration row: {strict_params}"
    );
    for (struct_name, prefix) in [
        ("Indexing", "indexing."),
        ("Query", "query."),
        ("Resources", "resources."),
        ("Commit", "commit."),
        ("Admin", "admin."),
        ("Extraction", "extraction."),
    ] {
        for field in public_fields(&config, struct_name) {
            let row = inventory_row(&inventory, &format!("{prefix}{field}"));
            assert_eq!(row.split('|').count(), 7, "configuration row: {row}");
            assert!(
                row.split('|')
                    .skip(2)
                    .take(4)
                    .all(|cell| !cell.trim().is_empty()),
                "default, unit, validation, and lifecycle/effect required: {row}"
            );
        }
    }
    for key in ["auth.username", "auth.password"] {
        let row = inventory_row(&inventory, key);
        assert_eq!(row.split('|').count(), 7, "configuration row: {row}");
    }

    let schema = source(root, "src/schema.rs");
    let schema_inventory = source(root, "manual/reference/schema.md");
    for name in quoted_entries(&schema, "NON_LANGUAGE_BUILTIN_TYPES") {
        inventory_row(&schema_inventory, &name);
    }
    let languages = quoted_entries(&schema, "LANGUAGES");
    for code in languages.iter().filter(|code| code.as_str() != "en") {
        inventory_row(&schema_inventory, &format!("text_{code}"));
    }
    let analyzer_inventory = source(root, "manual/reference/analyzers.md");
    for field in public_fields(&schema, "FieldTypeConfig") {
        inventory_row(&analyzer_inventory, &format!("field_types.{field}"));
    }
    for field in public_fields(&schema, "FilterConfig") {
        inventory_row(&analyzer_inventory, &format!("field_types.filters.{field}"));
    }
}

fn markdown_files(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).expect("read manual directory") {
        let path = entry.expect("read manual entry").path();
        if path.is_dir() {
            markdown_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "md") {
            files.push(path);
        }
    }
}

fn anchor(text: &str) -> String {
    text.trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == ' ' || *c == '-')
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
}

#[test]
fn canonical_docs_remain_exactly_four_and_manual_links_resolve() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let docs = root.join("docs");
    let entries = std::fs::read_dir(&docs)
        .expect("read docs")
        .map(|entry| entry.expect("read docs entry").path())
        .collect::<Vec<_>>();
    assert!(
        entries.iter().all(|path| path.is_file()),
        "docs has no subdirectories"
    );
    assert_eq!(
        entries
            .iter()
            .filter_map(|path| path.file_name().and_then(|name| name.to_str()))
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "COMPATIBILITY.md",
            "CONFIGURATION.md",
            "DEPLOYMENT.md",
            "DEVELOPMENT.md",
        ]),
        "docs contains exactly the four canonical documents"
    );

    let mut files = Vec::new();
    markdown_files(&root.join("manual"), &mut files);
    for file in files {
        let text = std::fs::read_to_string(&file).expect("read manual Markdown");
        for (offset, _) in text.match_indices("](") {
            let target = &text[offset + 2..];
            let Some(end) = target.find(')') else {
                panic!("unterminated link in {}", file.display())
            };
            let target = &target[..end];
            if target.starts_with("http://")
                || target.starts_with("https://")
                || target.starts_with("mailto:")
            {
                continue;
            }
            let (path, fragment) = target.split_once('#').unwrap_or((target, ""));
            let destination = if path.is_empty() {
                file.clone()
            } else {
                file.parent().expect("manual file parent").join(path)
            };
            assert!(
                destination.is_file(),
                "link target missing: {} -> {target}",
                file.display()
            );
            if !fragment.is_empty() {
                let destination_text =
                    std::fs::read_to_string(&destination).expect("read anchor target");
                let anchors: BTreeSet<_> = destination_text
                    .lines()
                    .filter_map(|line| line.strip_prefix('#').map(anchor))
                    .collect();
                assert!(
                    anchors.contains(fragment),
                    "anchor missing: {} -> {target}",
                    file.display()
                );
            }
        }
    }
}
